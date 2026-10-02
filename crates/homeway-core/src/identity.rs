//! 设备身份（WG 私钥 + devTag）与身份存储（master.key 持久化派生）。
//!
//! 语义真源 `baseline:clientcore/internal/wtransport/identity{,_store}.go`
//! （对照向量 `fixtures/vectors/identity.json`，经 LoadOrCreate 全路径交叉验证）：
//!
//! - `master` = 32B 随机（`<dir>/master.key`，0600；目录 0700；永不外传）；
//! - `identity(backend) = HKDF-SHA256(master, salt=nil, info="tier/dev-id/v1"‖peerID, 32B)`
//!   ——私钥**未钳位**（钳位在标量乘内部；dalek 同义），同设备同后端恒定；
//! - `devTag` = 8B 随机（`<dir>/devtag`，0600）——**独立于主密钥**持久化：身份轮换
//!   （删 master.key 重建）时 devTag 不变，出口走「同 devTag 换公钥」原子替换；
//!   标签文件不可用时退化为 `HKDF(master, "tier/dev-tag/v1", 8B)`（TagDerived）。
//!
//! 持久化健壮性（Go 同款）：`O_EXCL` 抢占最终路径；并发输家（EEXIST）等赢家写完并
//! **用赢家的钥匙**（收敛）；残缺文件有界重试（10×20ms）后归档 `*.bad-<ts>` 重建。
//!
//! **已登记差异（R1）**：Go 侧另有跨进程 flock（identity.lock）串行化「隧道会话 vs
//! App 服务会话」的并发首启——R1 客户端是单进程单会话，无此并发面；flock 留 R7
//! （APP 双进程形态）补齐（设计文档 §5 1a 登记）。

use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::thread::sleep;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use x25519_dalek::StaticSecret;

use crate::psk::hkdf_expand_32;
use crate::token::PeerId;

const MASTER_KEY_FILE: &str = "master.key";
const DEV_TAG_FILE: &str = "devtag";
const MASTER_KEY_LEN: usize = 32;
const DEV_TAG_LEN: usize = 8;
const DEV_ID_LABEL: &[u8] = b"tier/dev-id/v1";
const DEV_TAG_LABEL: &[u8] = b"tier/dev-tag/v1";
/// 残缺文件的有界等待（Go：10×20ms）。
const WAIT_ROUNDS: usize = 10;
const WAIT_INTERVAL: Duration = Duration::from_millis(20);

/// 身份来源（Go `IdentitySource` 的 enum 形态；日志/判据区分「新建/复用/重建/降级」）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum IdentitySource {
    /// 首次生成并落盘。
    Created,
    /// 复用既有主密钥。
    Reused,
    /// 既有文件损坏 → 归档后重建。
    Rebuilt,
    /// 设备标签文件不可用 → 从主密钥派生（重置身份会换标签，打警告）。
    TagDerived,
    /// 目录不可用 → 本次临时身份（降级，绝不因存储失败拒绝建连）。
    Ephemeral,
}

impl IdentitySource {
    /// 判据行 C1 的形态词（Go session.go:197/199 的「新建/复用」）。
    pub fn zh(self) -> &'static str {
        match self {
            IdentitySource::Created => "新建",
            IdentitySource::Reused => "复用",
            IdentitySource::Rebuilt => "重建",
            IdentitySource::TagDerived => "标签派生",
            IdentitySource::Ephemeral => "临时",
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum IdentityError {
    #[error("identity: 存储不可用（{0}）")]
    StoreUnavailable(String),
    #[error("identity: 随机材料生成失败：{0}")]
    Random(String),
    #[error("identity: {0}")]
    Io(#[from] std::io::Error),
}

/// 设备标签（出口设备表的键）：8B，Debug 只出 4B hex 短指纹（日志纪律）。
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct DevTag(pub(crate) [u8; DEV_TAG_LEN]);

impl DevTag {
    pub fn as_bytes(&self) -> &[u8; 8] {
        &self.0
    }
    /// 判据行 `dev=` 形态（shortTag：首 4B hex）。
    pub fn short(&self) -> String {
        hex_encode(&self.0[..4])
    }
}

impl fmt::Debug for DevTag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "DevTag({}…)", self.short())
    }
}

/// 设备 WG 身份：私钥不外泄（Debug 脱敏）、公钥进 reg 报文/日志短指纹。
/// Clone = 密钥复制（会话重建要同身份再装配一代——R2 rebuild；dalek StaticSecret
/// 本身可克隆）。
#[derive(Clone)]
pub struct Identity {
    key: StaticSecret,
    dev_tag: DevTag,
}

impl fmt::Debug for Identity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Identity")
            .field("pub", &self.short_pub())
            .field("dev", &self.dev_tag.short())
            .finish()
    }
}

impl Identity {
    /// 临时身份（随机钥 + 随机标签）：测试与「身份目录不可写」降级路径
    /// （Go `NewIdentity`；生产路径走 [`load_or_create`]）。
    pub fn ephemeral() -> Result<Self, IdentityError> {
        let key: [u8; 32] = random_bytes(MASTER_KEY_LEN)?.try_into().expect("请求 32B");
        let tag: [u8; DEV_TAG_LEN] = random_bytes(DEV_TAG_LEN)?.try_into().expect("请求 8B");
        Ok(Self {
            key: StaticSecret::from(key),
            dev_tag: DevTag(tag),
        })
    }

    /// 公钥（reg 报文载荷 / 后端设备登记 / 隧道地址派生输入）。
    pub fn public_key(&self) -> [u8; 32] {
        x25519_dalek::PublicKey::from(&self.key).to_bytes()
    }

    /// 私钥（boringtun `Tunn::new` 装配面）。
    pub fn private_key(&self) -> &StaticSecret {
        &self.key
    }

    /// 私钥字节视图（向量对账/装配用；敏感材料，不进日志）。
    pub fn private_key_bytes(&self) -> [u8; 32] {
        self.key.to_bytes()
    }

    pub fn dev_tag(&self) -> DevTag {
        self.dev_tag
    }

    /// 日志短指纹（4B hex，同 Go ShortPub/ShortDev）。
    pub fn short_pub(&self) -> String {
        hex_encode(&self.public_key()[..4])
    }

    pub fn short_dev(&self) -> String {
        self.dev_tag.short()
    }
}

/// 取本设备对 peerID（后端静态公钥）的稳定身份。
///
/// 返回 [`IdentitySource`] 供判据日志区分「新建/复用/重建/降级」；`warning` 非空 =
/// 存储不可用走了临时身份（Go：`err != nil && src == SourceEphemeral`，调用方打警告后
/// 照常建连）。`Err` 仅剩「存储不可用且临时身份生成失败」的双重失败（实践不可达）。
pub fn load_or_create(
    dir: Option<&Path>,
    peer_id: &PeerId,
) -> Result<(Identity, IdentitySource, Option<String>), IdentityError> {
    let Some(dir) = dir else {
        return ephemeral_fallback("identity: 未配置身份目录".to_owned());
    };
    if dir.as_os_str().is_empty() {
        // 空串与未配置同义（Go 对 dir=="" 显式报错走临时身份；create_dir_all("") 是
        // Ok(()) 且 join 得相对路径——身份会随 CWD 漂移，评审中-6）
        return ephemeral_fallback("identity: 未配置身份目录".to_owned());
    }
    let (master, src) = match load_or_create_master(dir) {
        Ok(v) => v,
        Err(err) => return ephemeral_fallback(err.to_string()),
    };
    let (tag, src) = match load_or_create_dev_tag(dir) {
        Ok(tag) => (tag, src),
        Err(_) => {
            // 标签文件不可用（目录只读且缺失等）：退化为从主密钥派生——同设备仍稳定，
            // 但重置身份会换标签（出口会多一条记录）。
            (derive_dev_tag(&master), IdentitySource::TagDerived)
        }
    };
    let key = derive_key(&master, peer_id);
    Ok((Identity { key, dev_tag: tag }, src, None))
}

fn ephemeral_fallback(
    reason: String,
) -> Result<(Identity, IdentitySource, Option<String>), IdentityError> {
    let id = Identity::ephemeral()?;
    Ok((id, IdentitySource::Ephemeral, Some(reason)))
}

/// master + 后端公钥 → 该后端的 WG 私钥（确定性、跨进程一致；未钳位）。
fn derive_key(master: &[u8; 32], peer_id: &PeerId) -> StaticSecret {
    let mut info = Vec::with_capacity(DEV_ID_LABEL.len() + 32);
    info.extend_from_slice(DEV_ID_LABEL);
    info.extend_from_slice(peer_id.as_bytes());
    StaticSecret::from(hkdf_expand_32(master, &info))
}

/// master → 设备标签（TagDerived 降级路径；8B）。
fn derive_dev_tag(master: &[u8; 32]) -> DevTag {
    let out = hkdf_expand_32(master, DEV_TAG_LABEL);
    DevTag(out[..DEV_TAG_LEN].try_into().expect("长度 8 恒成立"))
}

/// 读主密钥；缺失则创建、损坏则归档重建（Go `loadOrCreateMaster`）。
fn load_or_create_master(dir: &Path) -> Result<([u8; 32], IdentitySource), IdentityError> {
    // 目录 0700（Go MkdirAll(dir, 0o700)；create_dir_all 默认 0777&~umask——评审中-5）
    let mut db = std::fs::DirBuilder::new();
    db.recursive(true);
    use std::os::unix::fs::DirBuilderExt;
    db.mode(0o700)
        .create(dir)
        .map_err(|e| IdentityError::StoreUnavailable(format!("建目录 {dir:?}: {e}")))?;
    let path = dir.join(MASTER_KEY_FILE);
    match fs::read(&path) {
        Ok(b) => {
            if b.len() == MASTER_KEY_LEN && !b.iter().all(|&v| v == 0) {
                return Ok((b.try_into().expect("长度已判 32"), IdentitySource::Reused));
            }
            // 已存在但残缺/全零：有界等「写入未完成」的赢家，等不到才算真损坏。
            if let Some(b2) = wait_for_complete(&path, MASTER_KEY_LEN) {
                return Ok((b2.try_into().expect("wait_for_complete 保证长度"), IdentitySource::Reused));
            }
            archive(&path)?;
            let nb = create_random_file_excl(&path, MASTER_KEY_LEN).map_err(io_of)?;
            Ok((nb.try_into().expect("生成长度 32"), IdentitySource::Rebuilt))
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            match create_random_file_excl(&path, MASTER_KEY_LEN) {
                Ok(nb) => Ok((nb.try_into().expect("生成长度 32"), IdentitySource::Created)),
                // 输家（EEXIST，本实现无 flock 时的兜底）：等赢家写完并用赢家的钥匙（收敛）。
                Err(CreateExclError::Exists) => match wait_for_complete(&path, MASTER_KEY_LEN) {
                    Some(b2) => Ok((b2.try_into().expect("wait_for_complete 保证长度"), IdentitySource::Reused)),
                    None => Err(IdentityError::StoreUnavailable(format!("并发创建未收敛：{path:?}"))),
                },
                Err(CreateExclError::Io(e)) => Err(e.into()),
            }
        }
        Err(e) => Err(IdentityError::StoreUnavailable(format!("读 {path:?}: {e}"))),
    }
}

/// 读设备标签（8B）；缺失则创建、损坏则归档重建（与主密钥同目录、同权限）。
fn load_or_create_dev_tag(dir: &Path) -> Result<DevTag, IdentityError> {
    let path = dir.join(DEV_TAG_FILE);
    match fs::read(&path) {
        Ok(b) => {
            if b.len() == DEV_TAG_LEN && !b.iter().all(|&v| v == 0) {
                return Ok(DevTag(b.try_into().expect("长度已判 8")));
            }
            archive(&path)?;
            let nb = create_random_file_excl(&path, DEV_TAG_LEN).map_err(io_of)?;
            Ok(DevTag(nb.try_into().expect("生成长度 8")))
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            match create_random_file_excl(&path, DEV_TAG_LEN) {
                Ok(nb) => Ok(DevTag(nb.try_into().expect("生成长度 8"))),
                Err(CreateExclError::Exists) => match wait_for_complete(&path, DEV_TAG_LEN) {
                    Some(b2) => Ok(DevTag(b2.try_into().expect("wait_for_complete 保证长度"))),
                    None => Err(IdentityError::StoreUnavailable(format!("并发创建未收敛：{path:?}"))),
                },
                Err(CreateExclError::Io(e)) => Err(e.into()),
            }
        }
        Err(e) => Err(IdentityError::StoreUnavailable(format!("读 {path:?}: {e}"))),
    }
}

/// 损坏文件归档：`<path>.bad-<unix秒>`（失败上抛——归档不动原文件即为只读故障）。
fn archive(path: &Path) -> Result<PathBuf, IdentityError> {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let arch = PathBuf::from(format!("{}.bad-{ts}", path.display()));
    fs::rename(path, &arch).map_err(|e| IdentityError::StoreUnavailable(format!("归档损坏文件 {path:?}: {e}")))?;
    Ok(arch)
}

#[derive(Debug)]
enum CreateExclError {
    /// EEXIST：并发创建输家（或旧版残留）——调用方走等待-复用路径。
    Exists,
    Io(std::io::Error),
}

/// `O_CREATE|O_EXCL` 抢占最终路径并写随机材料 + fsync（Go `createRandomFileUnderLock`；
/// 写失败自清理，不点半截文件）。
fn create_random_file_excl(path: &Path, n: usize) -> Result<Vec<u8>, CreateExclError> {
    let buf = random_bytes(n).map_err(|e| CreateExclError::Io(std::io::Error::other(e)))?;
    let mut f = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::AlreadyExists {
                CreateExclError::Exists
            } else {
                CreateExclError::Io(e)
            }
        })?;
    let cleanup = |e: std::io::Error| -> CreateExclError {
        let _ = fs::remove_file(path);
        CreateExclError::Io(e)
    };
    f.write_all(&buf).map_err(cleanup)?;
    f.sync_all().map_err(cleanup)?;
    drop(f);
    Ok(buf)
}

/// `CreateExclError::Io` 的 `?` 转换辅助（Exists 分支由调用方显式处理）。
fn io_of(e: CreateExclError) -> IdentityError {
    match e {
        CreateExclError::Io(io) => io.into(),
        CreateExclError::Exists => IdentityError::StoreUnavailable("EEXIST 意外出现在重建路径".into()),
    }
}

/// 等「看起来还在写入」的文件变完整（10×20ms 有界；返回 None = 超时仍残缺）。
fn wait_for_complete(path: &Path, want_len: usize) -> Option<Vec<u8>> {
    for _ in 0..WAIT_ROUNDS {
        sleep(WAIT_INTERVAL);
        if let Ok(b) = fs::read(path) {
            if b.len() == want_len && !b.iter().all(|&v| v == 0) {
                return Some(b);
            }
        }
    }
    None
}

fn random_bytes(n: usize) -> Result<Vec<u8>, IdentityError> {
    let mut buf = vec![0u8; n];
    getrandom::getrandom(&mut buf).map_err(|e| IdentityError::Random(e.to_string()))?;
    Ok(buf)
}

fn hex_encode(b: &[u8]) -> String {
    b.iter().map(|v| format!("{v:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h32(s: &str) -> [u8; 32] {
        let mut v = [0u8; 32];
        hex::decode_to_slice(s, &mut v).unwrap();
        v
    }

    /// 吃 Go 真源向量（fixtures/vectors/identity.json）：master+peerID → 私钥/公钥；
    /// master → devTag（TagDerived 派生路径）。
    #[test]
    fn derivation_matches_go_vectors() {
        for (master, peer, priv_want, pub_want) in [
            (
                "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f",
                "1111111111111111111111111111111111111111111111111111111111111111",
                "1ebf5701e608af93bc66b1c50c26b3683e8fc120251fef02bce7b52c07c3ae34",
                "3c98c9c07bf119c8ea650053f532639010563aa1993e85f12073ba1422296f17",
            ),
            (
                "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f",
                "2222222222222222222222222222222222222222222222222222222222222222",
                "db4a1b4800130b2fd2ca8ff5c3a1ae8d9994b88280aeaef14017c80e168583ac",
                "ab9bca523d26e4dd94783c5627e651cdd61f7e6d2d1688f48666091cac60e911",
            ),
            (
                "f1e2d3c4b5a6978869584a3b2c1d0e0f00112233445566778899aabbccddeeff",
                "1111111111111111111111111111111111111111111111111111111111111111",
                "48c0537f9a0d82f6f5c7a83eae3bd41bda48482eca1dc7b4ef3e0123d2018366",
                "aeb760ae073a3f07229a23ac7c257cc273c9974703358fa7e5bda859c8efcc52",
            ),
        ] {
            let m = h32(master);
            let key = derive_key(&m, &PeerId::from(h32(peer)));
            assert_eq!(hex::encode(key.to_bytes()), priv_want, "master={master}");
            let id = Identity { key, dev_tag: DevTag([0; 8]) };
            assert_eq!(hex::encode(id.public_key()), pub_want);
        }
        // devTag 派生（TagDerived 降级路径）
        for (master, want_tag, want_short) in [
            (
                "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f",
                "1273adaf45f0a411",
                "1273adaf",
            ),
            (
                "f1e2d3c4b5a6978869584a3b2c1d0e0f00112233445566778899aabbccddeeff",
                "1f33e66a917ee2ad",
                "1f33e66a",
            ),
        ] {
            let tag = derive_dev_tag(&h32(master));
            assert_eq!(hex::encode(tag.as_bytes()), want_tag);
            assert_eq!(tag.short(), want_short);
        }
    }

    /// 存储生命周期：新建 → 复用（同钥同标签）→ 损坏归档重建（换钥）。
    #[test]
    fn store_lifecycle_created_reused_rebuilt() {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .subsec_nanos();
        let dir = std::env::temp_dir().join(format!("homeway-rs-identity-{}-{nanos}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let peer = PeerId::from(h32("1111111111111111111111111111111111111111111111111111111111111111"));

        let (id1, src1, warn1) = load_or_create(Some(&dir), &peer).unwrap();
        assert_eq!(src1, IdentitySource::Created);
        assert!(warn1.is_none());

        let (id2, src2, warn2) = load_or_create(Some(&dir), &peer).unwrap();
        assert_eq!(src2, IdentitySource::Reused);
        assert!(warn2.is_none());
        assert_eq!(id2.public_key(), id1.public_key(), "复用必须同钥");
        assert_eq!(id2.dev_tag(), id1.dev_tag());

        // 损坏 master.key → 归档重建（换钥、devTag 不变——标签独立持久化）
        let bad = dir.join(MASTER_KEY_FILE);
        fs::write(&bad, [0u8; 8]).unwrap();
        let (id3, src3, _) = load_or_create(Some(&dir), &peer).unwrap();
        assert_eq!(src3, IdentitySource::Rebuilt);
        assert_ne!(id3.public_key(), id1.public_key(), "重建必须换钥");
        assert_eq!(id3.dev_tag(), id1.dev_tag(), "devTag 独立持久化不动");
        // 归档名含秒级时间戳（*.bad-<ts>），存在性匹配即可
        assert!(fs::read_dir(&dir)
            .unwrap()
            .any(|e| e.map(|e| e.file_name().to_string_lossy().contains(".bad-")).unwrap_or(false)));

        let _ = fs::remove_dir_all(&dir);
    }

    /// 目录不可用（空）→ 临时身份降级 + 警告（绝不拒绝建连）。
    #[test]
    fn missing_dir_falls_back_to_ephemeral() {
        let peer = PeerId::from([1u8; 32]);
        let (id, src, warn) = load_or_create(None, &peer).unwrap();
        assert_eq!(src, IdentitySource::Ephemeral);
        assert!(warn.is_some());
        assert_eq!(id.short_dev().len(), 8);
        // 两次临时身份不同钥（非稳定路径）
        let (id_b, _, _) = load_or_create(None, &peer).unwrap();
        assert_ne!(id.public_key(), id_b.public_key());
    }
}
