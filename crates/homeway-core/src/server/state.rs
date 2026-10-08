//! 出口 state（R3；语义真源 `internal/server/state.go`）。
//!
//! `<dir>/key.bin` 身份私钥（0600，存在即复用——**升级不轮换**，token 里烤的是公钥，
//! 换钥 = 所有 token 作废）；`<dir>/tokens.jsonl` 台账（append-only；每行
//! `{id,secret,endpoints,issued}`，**末行 = 最近在用 token** 的不变量由写入纪律保证：
//! 每次铸出与末行不同即追加）；`<dir>/revoked.jsonl` 吊销表（append-only；**不改台账**）。
//!
//! 台账 JSON 与 Go 字节对齐（字段名 PascalCase / 声明序 / id omitempty / RFC3339 时间）。

use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use x25519_dalek::PublicKey;
use x25519_dalek::StaticSecret;

use crate::token::{Endpoint, EndpointKind, Secret, Token};

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum StateError {
    #[error("state: key.bin 长度 {0} 非法")]
    BadKeyLen(usize),
    #[error("state: tokens.jsonl 坏行: {0}")]
    BadTokenLine(String),
    #[error("state: revoked.jsonl 坏行: {0}")]
    BadRevokedLine(String),
    #[error("state: 台账 secret 非法")]
    BadSecret,
    #[error("该凭证已被吊销")]
    SecretRevoked,
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// 出口运行态（state 目录句柄）。
pub struct State {
    dir: PathBuf,
}

fn now_utc() -> String {
    // RFC3339（UTC，秒粒度——Go time.RFC3339 对 UTC 时区输出 Z 后缀）
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = secs / 86400;
    let rem = secs % 86400;
    let (h, mi, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    // civil_from_days（Howard Hinnant 算法）
    let z = days as i64 + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z")
}

/// 凭证短指纹（8 hex）= sha256(secret)[:4]（Go credID——确定派生，不落额外状态）。
fn cred_id(secret: &[u8; 32]) -> String {
    let sum = Sha256::digest(secret);
    sum[..4].iter().map(|b| format!("{b:02x}")).collect()
}

/// 台账行的 endpoints 段（与 Go `proto.Endpoint` 的 JSON 形态字节对齐）。
///
/// **M1 S1c 的 additive 字段（`"Quic"`）**：QUIC 类端点（设计 §1.1/§3.6）需要与
/// WG/中继区分——`"Relay"` 保持「是否中继」原义（QUIC 端点恒 `false`），另加 `"Quic": true`
/// **仅在为真时出现**（`skip_serializing_if`）⇒ 既有行**逐字节不变**；读侧缺键 = 非 QUIC。
/// 登记：S4 的判据登记条「token 端点表新增 QUIC 类 + RPK 字段」须点名本字段（台账格式面）。
fn endpoint_json(eps: &[Endpoint]) -> serde_json::Value {
    serde_json::Value::Array(
        eps.iter()
            .map(|e| {
                let mut obj = serde_json::json!({
                    "Addr": e.addr,
                    "Relay": matches!(e.kind, EndpointKind::Relay),
                });
                if e.kind == EndpointKind::Quic {
                    obj["Quic"] = serde_json::Value::Bool(true);
                }
                obj
            })
            .collect(),
    )
}

fn endpoint_from_json(v: &serde_json::Value) -> Option<Endpoint> {
    let addr = v.get("Addr")?.as_str()?.to_string();
    let relay = v.get("Relay").and_then(|b| b.as_bool()).unwrap_or(false);
    let quic = v.get("Quic").and_then(|b| b.as_bool()).unwrap_or(false);
    Some(Endpoint {
        addr,
        kind: match (relay, quic) {
            (_, true) => EndpointKind::Quic,
            (true, false) => EndpointKind::Relay,
            (false, false) => EndpointKind::Direct,
        },
    })
}

#[derive(Serialize, Deserialize)]
struct TokenRecord {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    id: String,
    secret: String,
    endpoints: serde_json::Value,
    issued: String,
    /// M1 S1c：出口 RPK 裸公钥（base64url-nopad 32B）——**追加在末位**（既有四个字段的
    /// 位置/形态零漂移）。`None`（无 QUIC 面/未铸出）= 该键**不出现** ⇒ 旧行逐字节不变。
    /// 登记同 `endpoint_json` 的 `Quic` 条（S4）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    rpk: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct RevokeRecord {
    id: String,
    secret: String,
    #[serde(rename = "revoked")]
    at: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    reason: String,
}

/// 台账只读呈现（`serve token list` 面）。
pub struct LedgerEntry {
    pub id: String,
    pub secret: String,
    pub issued: String,
    pub endpoints: Vec<Endpoint>,
    pub revoked: bool,
    pub reason: String,
}

impl State {
    /// 打开（目录 **0700**：`DirBuilder::mode` 创建即收紧 + create 后无条件 chmod
    /// 归一——`mkdir` 同样受 umask 掩码，只靠 `.mode` 不够；chmod 失败**告警**，
    /// 对齐 `nodestate.rs` 的口径。Q-G F4/A3）。
    pub fn open(dir: &Path) -> std::io::Result<Self> {
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
        if let Err(e) = fs::set_permissions(dir, fs::Permissions::from_mode(0o700)) {
            eprintln!(
                "homeway: ⚠️ state 目录 {} 收紧 0700 失败（{e}）——建议手工 chmod",
                dir.display()
            );
        }
        Ok(Self { dir: dir.to_path_buf() })
    }

    fn key_path(&self) -> PathBuf {
        self.dir.join("key.bin")
    }

    fn tokens_path(&self) -> PathBuf {
        self.dir.join("tokens.jsonl")
    }

    fn revoked_path(&self) -> PathBuf {
        self.dir.join("revoked.jsonl")
    }

    /// 身份私钥（不存在则生成；重启不变是 token 稳定性的前提）。
    ///
    /// **Q-G F4/A1（高危面）**：Go 是原子 `os.WriteFile(...,0o600)`（`state.go:70`）；
    /// 旧形态「默认权限建 → **静默** chmod」在 chmod 失败时会让**身份私钥永久 0644**
    /// 且无任何告警 ⇒ 本处改「创建即 0600（`.mode`）+ 拿到 handle 后 fchmod 归一
    /// （防 umask 掩码）+ 失败告警」。
    ///
    /// **代码门① 补强**：**读路径**（既有 key.bin）也做一次 best-effort 归一——
    /// 否则「已命中旧 chmod 失败、磁盘上就是 0644」的存量私钥**永远不会**被修复
    /// （本批只覆盖未来）；读语义不变（失败只告警）。
    pub fn private_key(&self) -> Result<StaticSecret, StateError> {
        if let Ok(b) = fs::read(self.key_path()) {
            let n = b.len();
            let arr: [u8; 32] = b.try_into().map_err(|_| StateError::BadKeyLen(n))?;
            if let Ok(f) = OpenOptions::new().read(true).open(self.key_path()) {
                if let Err(e) = f.set_permissions(fs::Permissions::from_mode(0o600)) {
                    eprintln!(
                        "homeway: ⚠️ 存量身份私钥 {} 收紧 0600 失败（{e}）——建议手工 chmod",
                        self.key_path().display()
                    );
                }
            }
            return Ok(StaticSecret::from(arr));
        }
        let mut seed = [0u8; 32];
        getrandom::getrandom(&mut seed).expect("系统随机源不可用");
        let mut f = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(self.key_path())?;
        if let Err(e) = f.set_permissions(fs::Permissions::from_mode(0o600)) {
            eprintln!(
                "homeway: ⚠️ 身份私钥 {} 收紧 0600 失败（{e}）——文件权限未归一，建议手工 chmod",
                self.key_path().display()
            );
        }
        f.write_all(&seed)?;
        Ok(StaticSecret::from(seed))
    }

    /// 生成新 token：随机 secret、追加台账（IssueToken 同义——**不复用于 AppendToken**：
    /// 这里每次新签发 secret）。
    ///
    /// `rpk` = 出口 RPK 裸公钥（M1 S1c：有 QUIC 面才给；`None` = 台账不打该字段）。
    pub fn issue_token(
        &self,
        eps: Vec<Endpoint>,
        rpk: Option<[u8; 32]>,
    ) -> Result<Token, StateError> {
        let privkey = self.private_key()?;
        let mut secret = [0u8; 32];
        getrandom::getrandom(&mut secret).expect("系统随机源不可用");
        let tok = Token {
            peer_id: crate::token::PeerId::from(PublicKey::from(&privkey).to_bytes()),
            secret: Secret::from(secret),
            endpoints: eps,
            rpk: rpk.map(crate::token::RpkPubKey::from),
        };
        self.append_record(secret, &tok.endpoints, rpk)?;
        Ok(tok)
    }

    /// 台账写入纪律的追加入口（AppendToken 同义）：与末行**不同**才追加（secret 与
    /// endpoints 序都相同 = 无变化）；**已吊销的 secret 一律拒写**（否则吊销后端点变化轮
    /// 会把死凭证的新一轮写进台账，末行不再代表有效 token）。
    pub fn append_token(
        &self,
        secret: &[u8; 32],
        eps: &[Endpoint],
        rpk: Option<[u8; 32]>,
    ) -> Result<(), StateError> {
        let revoked = self.revoked_secrets()?;
        if revoked.contains(secret) {
            return Err(StateError::SecretRevoked);
        }
        if let Some(last) = self.last_record()? {
            let same = last.secret == *secret
                && last.endpoints.len() == eps.len()
                && last.endpoints.iter().zip(eps.iter()).all(|(a, b)| a == b)
                && last.rpk == rpk;
            if same {
                return Ok(()); // 无变化不追加
            }
        }
        self.append_record(*secret, eps, rpk)
    }

    fn append_record(
        &self,
        secret: [u8; 32],
        eps: &[Endpoint],
        rpk: Option<[u8; 32]>,
    ) -> Result<(), StateError> {
        let rec = TokenRecord {
            id: cred_id(&secret),
            secret: B64.encode(secret),
            endpoints: endpoint_json(eps),
            issued: now_utc(),
            rpk: rpk.map(|k| B64.encode(k)),
        };
        let mut line = serde_json::to_string(&rec).expect("台账行恒可序列化");
        line.push('\n');
        append_file(&self.tokens_path(), line.as_bytes())
    }

    /// 吊销一枚凭证（append-only；幂等：已在表内返回 true 不重复追加）。**不改台账**。
    pub fn revoke(&self, secret: &[u8; 32], reason: &str) -> Result<bool, StateError> {
        let set = match self.revoked_secrets() {
            Ok(s) => s,
            Err(StateError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => Default::default(),
            Err(e) => return Err(e),
        };
        if set.contains(secret) {
            return Ok(true);
        }
        let rec = RevokeRecord {
            id: cred_id(secret),
            secret: B64.encode(secret),
            at: now_utc(),
            reason: reason.to_string(),
        };
        let mut line = serde_json::to_string(&rec).expect("吊销行恒可序列化");
        line.push('\n');
        append_file(&self.revoked_path(), line.as_bytes())?;
        Ok(false)
    }

    /// 吊销表全量（list 面；文件不存在 = 空）。
    pub fn revocations(&self) -> Result<Vec<(String, String, String, String)>, StateError> {
        // (id, secret_b64, at, reason)
        let b = match fs::read(self.revoked_path()) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        let mut out = Vec::new();
        for line in split_lines(&b) {
            if line.is_empty() {
                continue;
            }
            let rec: RevokeRecord = serde_json::from_slice(line)
                .map_err(|e| StateError::BadRevokedLine(e.to_string()))?;
            out.push((rec.id, rec.secret, rec.at, rec.reason));
        }
        Ok(out)
    }

    /// 吊销集合（secret 集；坏行按错误上报——吊销是安全面，不能静默当「没吊销」放行）。
    pub fn revoked_secrets(&self) -> Result<std::collections::HashSet<[u8; 32]>, StateError> {
        let b = match fs::read(self.revoked_path()) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Default::default())
            }
            Err(e) => return Err(e.into()),
        };
        let mut out = std::collections::HashSet::new();
        for line in split_lines(&b) {
            if line.is_empty() {
                continue;
            }
            let rec: RevokeRecord = serde_json::from_slice(line)
                .map_err(|e| StateError::BadRevokedLine(e.to_string()))?;
            let raw = B64
                .decode(rec.secret.as_bytes())
                .map_err(|_| StateError::BadSecret)?;
            let s: [u8; 32] = raw.try_into().map_err(|_| StateError::BadSecret)?;
            out.insert(s);
        }
        Ok(out)
    }

    /// 台账只读全量（`serve token list` 面；含吊销状态与原因）。
    pub fn ledger(&self) -> Result<Vec<LedgerEntry>, StateError> {
        let b = match fs::read(self.tokens_path()) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        let revoked = self.revoked_secrets()?;
        let mut reason_of: std::collections::HashMap<[u8; 32], String> = std::collections::HashMap::new();
        for (id, secret, _at, reason) in self.revocations()? {
            let _ = id;
            if let Ok(raw) = B64.decode(secret.clone().into_bytes()) {
                if let Ok(s) = <[u8; 32]>::try_from(raw) {
                    reason_of.insert(s, reason);
                }
            }
        }
        let mut out = Vec::new();
        for line in split_lines(&b) {
            if line.is_empty() {
                continue;
            }
            let rec: TokenRecord = serde_json::from_slice(line)
                .map_err(|e| StateError::BadTokenLine(e.to_string()))?;
            let raw = B64
                .decode(rec.secret.as_bytes())
                .map_err(|_| StateError::BadSecret)?;
            let s: [u8; 32] = raw.try_into().map_err(|_| StateError::BadSecret)?;
            let id = if rec.id.is_empty() { cred_id(&s) } else { rec.id }; // 老行按 secret 现算
            let eps = rec
                .endpoints
                .as_array()
                .map(|a| a.iter().filter_map(endpoint_from_json).collect())
                .unwrap_or_default();
            out.push(LedgerEntry {
                id,
                secret: rec.secret,
                issued: rec.issued,
                endpoints: eps,
                revoked: revoked.contains(&s),
                reason: reason_of.get(&s).cloned().unwrap_or_default(),
            });
        }
        Ok(out)
    }

    /// 台账末行（「末行 = 最近在用 token」的读半边；空台账 = None）。
    pub fn last_token(&self) -> Result<Option<Token>, StateError> {
        let Some(rec) = self.last_record()? else {
            return Ok(None);
        };
        Ok(Some(Token {
            // 末行重铸的 peer_id 用当前 key 的公钥（台账不存公钥——Go LastToken 同义：
            // token 的 PeerID 恒 = 本出口身份公钥）
            peer_id: crate::token::PeerId::from(PublicKey::from(&self.private_key()?).to_bytes()),
            secret: Secret::from(rec.secret),
            endpoints: rec.endpoints,
            // M1 S1c：台账行带 RPK ⇒ 末行重铸的 token 也带它（否则 `serve token` 会打印
            // 一枚与运行中出口不一致的串——客户端钉定会用错身份）
            rpk: rec.rpk.map(crate::token::RpkPubKey::from),
        }))
    }

    fn last_record(&self) -> Result<Option<TokenRecordOwned>, StateError> {
        Ok(self.last_record_raw()?.map(|(r, _)| r))
    }

    fn last_record_raw(&self) -> Result<Option<(TokenRecordOwned, usize)>, StateError> {
        let b = match fs::read(self.tokens_path()) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let lines = split_lines(&b);
        for (i, line) in lines.iter().enumerate().rev() {
            if line.is_empty() {
                continue;
            }
            let rec: TokenRecord = serde_json::from_slice(line)
                .map_err(|e| StateError::BadTokenLine(e.to_string()))?;
            let endpoints = rec
                .endpoints
                .as_array()
                .map(|a| a.iter().filter_map(endpoint_from_json).collect())
                .unwrap_or_default();
            return Ok(Some((
                TokenRecordOwned {
                    secret: decode_secret_b64(&rec.secret)?,
                    endpoints,
                    rpk: rec.rpk.as_deref().map(decode_secret_b64).transpose()?,
                },
                i,
            )));
        }
        Ok(None)
    }

    /// 全部有效 secret（reg 验证逐一试 HMAC；**已吊销的被滤除**——全被吊销 = 空集，
    /// 启动路径据此铸新）。
    pub fn secrets(&self) -> Result<Vec<[u8; 32]>, StateError> {
        let revoked = self.revoked_secrets()?;
        let b = match fs::read(self.tokens_path()) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        let mut out = Vec::new();
        for line in split_lines(&b) {
            if line.is_empty() {
                continue;
            }
            let rec: TokenRecord = serde_json::from_slice(line)
                .map_err(|e| StateError::BadTokenLine(e.to_string()))?;
            let s = decode_secret_b64(&rec.secret)?;
            if revoked.contains(&s) {
                continue;
            }
            out.push(s);
        }
        Ok(out)
    }
}

/// append_token 的去重比较面（secret + endpoints）。
struct TokenRecordOwned {
    secret: [u8; 32],
    endpoints: Vec<Endpoint>,
    /// 台账行里的 RPK（M1 S1c；旧行无该键 = `None`）。
    rpk: Option<[u8; 32]>,
}

fn decode_secret_b64(s: &str) -> Result<[u8; 32], StateError> {
    let raw = B64.decode(s.as_bytes()).map_err(|_| StateError::BadSecret)?;
    raw.try_into().map_err(|_| StateError::BadSecret)
}

/// 台账追加入口（Q-G F4/A2）：`create|append` **创建即 0600**（`.mode`）+ handle 后
/// fchmod 归一 + 失败告警——旧形态的双层静默 `let _`（连 `metadata()` 错误都吞）
/// 会让 token/吊销台账在 chmod 失败时永久 0644。Go 侧为原子
/// `os.OpenFile(..., O_CREATE|O_APPEND, 0o600)`（`state.go:181/221`）。
fn append_file(path: &Path, bytes: &[u8]) -> Result<(), StateError> {
    let mut f = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(path)?;
    if let Err(e) = f.set_permissions(fs::Permissions::from_mode(0o600)) {
        eprintln!(
            "homeway: ⚠️ 台账 {} 收紧 0600 失败（{e}）——文件权限未归一，建议手工 chmod",
            path.display()
        );
    }
    f.write_all(bytes)?;
    Ok(())
}

fn split_lines(b: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut start = 0;
    for (i, c) in b.iter().enumerate() {
        if *c == b'\n' {
            out.push(&b[start..i]);
            start = i + 1;
        }
    }
    if start < b.len() {
        out.push(&b[start..]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "homeway-rs-state-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
        ));
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn eps() -> Vec<Endpoint> {
        vec![
            Endpoint { addr: "127.0.0.1:42641".into(), kind: EndpointKind::Direct },
        ]
    }

    /// Q-G F4（A1/A2/A3，高危面回归守卫）：**全新** state 目录上——
    /// `State::open` 目录 = 0700；`key.bin` = 0600；`tokens.jsonl`/`revoked.jsonl`
    /// = 0600。**注**：正常 umask 下修前最终 mode 也是 0600（旧后置 chmod 会执行），
    /// 故这是**回归守卫 + 代码面复核**，不是修前红（设计 §4.2 已降级登记）。
    #[test]
    fn permissions_are_atomic_0600_and_dir_0700() {
        use std::os::unix::fs::PermissionsExt as _;
        // 目录刻意先建成 0755（模拟 umask 宽松的 state 根）
        let dir = tmpdir("perm");
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
        let st = State::open(&dir).unwrap();
        assert_eq!(
            fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
            0o700,
            "State::open 后目录必须 0700"
        );
        let _k = st.private_key().unwrap();
        assert_eq!(
            fs::metadata(st.key_path()).unwrap().permissions().mode() & 0o777,
            0o600,
            "key.bin（身份私钥）必须 0600"
        );
        st.issue_token(eps(), None).unwrap();
        assert_eq!(
            fs::metadata(st.tokens_path()).unwrap().permissions().mode() & 0o777,
            0o600,
            "tokens.jsonl（token 台账）必须 0600"
        );
        st.revoke(&st.secrets().unwrap()[0], "perm").unwrap();
        assert_eq!(
            fs::metadata(st.revoked_path()).unwrap().permissions().mode() & 0o777,
            0o600,
            "revoked.jsonl（吊销台账）必须 0600"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    /// 台账一行制：issue 追加；append_token 同值不追加、变化才追加；末行 = 在用。
    #[test]
    fn ledger_append_discipline() {
        let dir = tmpdir("append");
        let st = State::open(&dir).unwrap();
        assert!(st.secrets().unwrap().is_empty(), "空台账");
        let _tok = st.issue_token(eps(), None).unwrap();
        let secrets = st.secrets().unwrap();
        assert_eq!(secrets.len(), 1);
        let s0 = secrets[0];
        // 同值不追加
        st.append_token(&s0, &eps(), None).unwrap();
        assert_eq!(st.secrets().unwrap().len(), 1);
        // 变化才追加（端点变）
        let eps2 = vec![
            Endpoint { addr: "127.0.0.1:42641".into(), kind: EndpointKind::Direct },
            Endpoint { addr: "192.168.3.12:42641".into(), kind: EndpointKind::Direct },
        ];
        st.append_token(&s0, &eps2, None).unwrap();
        let ledger = st.ledger().unwrap();
        assert_eq!(ledger.len(), 2, "变化应追加");
        assert_eq!(ledger[1].endpoints.len(), 2);
        // 末行在用
        let last = st.last_token().unwrap().unwrap();
        assert_eq!(last.endpoints.len(), 2);
        let _ = fs::remove_dir_all(&dir);
    }

    /// 吊销：secrets 滤除、append 拒写、幂等、last_token 仍可读（reveal 用）。
    #[test]
    fn revoke_filters_and_blocks_append() {
        let dir = tmpdir("revoke");
        let st = State::open(&dir).unwrap();
        st.issue_token(eps(), None).unwrap();
        let s0 = st.secrets().unwrap()[0];
        assert!(!st.revoke(&s0, "泄漏测试").unwrap(), "首次吊销非幂等命中");
        assert!(st.revoke(&s0, "再吊").unwrap(), "重复吊销幂等");
        assert!(st.secrets().unwrap().is_empty(), "吊销后无有效凭证");
        assert!(matches!(
            st.append_token(&s0, &eps(), None),
            Err(StateError::SecretRevoked)
        ));
        let _ = fs::remove_dir_all(&dir);
    }

    /// key.bin 复用（重启不变）；私钥长度校验。
    #[test]
    fn private_key_stable() {
        let dir = tmpdir("key");
        let st = State::open(&dir).unwrap();
        let k1 = st.private_key().unwrap();
        let k2 = st.private_key().unwrap();
        assert_eq!(k1.to_bytes(), k2.to_bytes());
        fs::write(dir.join("key.bin"), [0u8; 31]).unwrap();
        assert!(matches!(
            st.private_key(),
            Err(StateError::BadKeyLen(31))
        ));
        let _ = fs::remove_dir_all(&dir);
    }

    /// 台账行 JSON 形态与 Go 字节对齐（PascalCase/声明序/id omitempty/RFC3339 形状）。
    #[test]
    fn ledger_line_shape_matches_go() {
        let dir = tmpdir("shape");
        let st = State::open(&dir).unwrap();
        st.issue_token(eps(), None).unwrap();
        let raw = fs::read_to_string(dir.join("tokens.jsonl")).unwrap();
        let line = raw.lines().next().unwrap();
        // 键序：id → secret → endpoints → issued（Go 结构体字段序）
        let id_pos = line.find("\"id\":").unwrap();
        let sec_pos = line.find("\"secret\":").unwrap();
        let eps_pos = line.find("\"endpoints\":").unwrap();
        let iss_pos = line.find("\"issued\":").unwrap();
        assert!(id_pos < sec_pos && sec_pos < eps_pos && eps_pos < iss_pos);
        assert!(line.contains("\"Addr\":\"127.0.0.1:42641\",\"Relay\":false"), "端点字段形态");
        assert!(line.contains("\"issued\":\"2"), "RFC3339 年头");
        // M1 S1c additive 字段：无 QUIC 面时**两个新键都不出现**（旧行逐字节不变）
        assert!(!line.contains("\"Quic\""), "非 QUIC 端点不得出现 Quic 键");
        assert!(!line.contains("\"rpk\""), "无 RPK 时不得出现 rpk 键");
        let _ = fs::remove_dir_all(&dir);
    }

    /// **判据（M1 S1c 的台账 additive 字段）**：QUIC 端点 ⇒ `"Quic": true`（端点对象内）；
    /// 带 RPK ⇒ 记录末位 `"rpk"`（记录级键名风格 = 既有四键的小写）；读回后
    /// `last_token()` 的端点类别与 rpk 全还原；**既有四键的位置与形态不变**。
    ///
    /// 加字段的显式决定（任务书要求登记）：新增 `Quic`（端点级，仅真时出现）与 `Rpk`
    /// （记录级，仅带时出现）两个 additive 键——旧行不出现它们 ⇒ 逐字节不变；
    /// S4 的登记条须点名（token 端点表 + RPK 字段条）。
    #[test]
    fn ledger_quic_and_rpk_fields_are_additive() {
        let dir = tmpdir("quic-rpk");
        let st = State::open(&dir).unwrap();
        let eps = vec![
            Endpoint { addr: "127.0.0.1:42641".into(), kind: EndpointKind::Direct },
            Endpoint { addr: "10.0.0.5:42642".into(), kind: EndpointKind::Quic },
            Endpoint { addr: "198.51.100.212:41741".into(), kind: EndpointKind::Relay },
        ];
        let rpk = [0xA7u8; 32];
        st.issue_token(eps.clone(), Some(rpk)).unwrap();
        let raw = fs::read_to_string(dir.join("tokens.jsonl")).unwrap();
        let line = raw.lines().next().unwrap();
        assert!(
            line.contains("\"Addr\":\"10.0.0.5:42642\",\"Quic\":true,\"Relay\":false"),
            "QUIC 端点形态（端点对象的键序 = serde_json::Map 字典序，既有行本就是这个口径）：{line}"
        );
        assert!(line.contains("\"Addr\":\"127.0.0.1:42641\",\"Relay\":false}"), "WG 端点形态不变：{line}");
        // 键序：id → secret → endpoints → issued → rpk（Rpk 追加在末位）
        let iss_pos = line.find("\"issued\":").unwrap();
        let rpk_pos = line.find("\"rpk\":").unwrap();
        assert!(iss_pos < rpk_pos, "rpk 必须在既有四键之后（位置零漂移）");

        // 读回：端点类别与 rpk 全还原
        let tok = st.last_token().unwrap().unwrap();
        assert_eq!(tok.endpoints.len(), 3);
        assert_eq!(tok.endpoints[1].kind, EndpointKind::Quic);
        assert_eq!(tok.endpoints[2].kind, EndpointKind::Relay);
        assert_eq!(tok.rpk.map(|k| *k.as_bytes()), Some(rpk), "末行重铸必须带上 rpk");
        // 追加纪律把 rpk 也算进「同值」判据（否则同端点同 rpk 会重复追加）
        let sec = *tok.secret.as_bytes();
        st.append_token(&sec, &eps, Some(rpk)).unwrap();
        assert_eq!(st.ledger().unwrap().len(), 1, "同 secret/端点/rpk ⇒ 不追加");
        st.append_token(&sec, &eps, None).unwrap();
        assert_eq!(st.ledger().unwrap().len(), 2, "rpk 变了（撤下）⇒ 追加");
        let _ = fs::remove_dir_all(&dir);
    }
}
