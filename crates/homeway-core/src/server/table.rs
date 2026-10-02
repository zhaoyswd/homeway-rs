//! devTag 设备表（R3；语义真源 `pkg/servercore/peers.go`）。
//!
//! 键 = 注册报文里的设备标签 devTag（设备本地生成、跨连接稳定）：
//! - 同设备重复注册只刷新 lastReg；同 devTag 换公钥 = 身份轮换 ⇒ 原子替换 device 侧
//!   peer 配置（Remove 先于 Add，顺序固定）；
//! - 表满只淘汰**超过活跃宽限期未刷新**的设备中最旧的一条；全部活跃则拒绝新设备
//!   （绝不淘汰在线设备）；
//! - TTL 周期回收长期不活跃设备；
//! - 登记/刷新/轮换/淘汰/拒绝/回收各打一行日志（dev/pub 短指纹 + 隧道地址 + n/cap + 原因）。
//!
//! **与 Go 的结构差异（有意）**：Go 的 `Configurer`（IpcSet 落 wireguard-go device）在
//! Rust 侧消掉了——`register` 返回 `Vec<DevOp>`（Add/Remove 序列），调用方（驱动线程）
//! 按序应用到自建 device。数据与副作用分离，表逻辑零 mock 可测；顺序保证（rotate 的
//! Remove→Add）由返回序直接承载（Go 需要 opCh FIFO 队列保证的东西在这里是天然串行）。

use std::collections::HashMap;
use std::net::Ipv4Addr;
use std::time::{Duration, SystemTime};

use hmac::{Hmac, Mac};
use sha2::Sha256;

use crate::tunnel_addr::{derive_tunnel_ip, derive_tun_ip};
use crate::Logf;

/// 设备表容量缺省（Go defaultMaxDevices）。
pub const DEFAULT_MAX_DEVICES: usize = 32;
/// 失联设备回收 TTL 缺省（7 天；config/flag 层落值，**0 = 关闭**）。
pub const DEFAULT_TTL: Duration = Duration::from_secs(7 * 24 * 3600);
/// 表满淘汰门槛缺省（活跃宽限 10 分钟）。
pub const DEFAULT_GRACE: Duration = Duration::from_secs(600);
/// reg 报文时间窗（±90s）。
const REG_WINDOW: u64 = 90;
/// reg 报文定长。
const REG_LEN: usize = 66;

/// reg 验证失败的三类（Go ErrRegMalformed/ErrRegExpired/ErrRegBadMAC 对应）。
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RegVerifyError {
    #[error("homeway/reg: 报文格式非法")]
    Malformed,
    #[error("homeway/reg: 报文时间戳超出窗口: 偏差 {0:?}")]
    Expired(i64),
    #[error("homeway/reg: HMAC 校验失败")]
    BadMac,
}

/// 校验注册报文（Go proto.VerifyReg 同义）：返回 (公钥, 设备标签)。
/// window <= 0 用缺省 90s。
pub fn verify_reg(
    secret: &[u8; 32],
    pkt: &[u8],
    now: SystemTime,
    window: Duration,
) -> Result<([u8; 32], [u8; 8]), RegVerifyError> {
    if pkt.len() != REG_LEN || &pkt[..2] != b"H2" {
        return Err(RegVerifyError::Malformed);
    }
    let pubkey: [u8; 32] = pkt[2..34].try_into().expect("长度已判");
    let dev_tag: [u8; 8] = pkt[34..42].try_into().expect("长度已判");
    // mac = HMAC-SHA256(secret, "hr-reg2" ‖ pubkey ‖ devTag ‖ ts)[:16]
    let mut mac = Hmac::<Sha256>::new_from_slice(secret).expect("HMAC 任意长密钥");
    mac.update(b"hr-reg2");
    mac.update(&pkt[2..50]);
    let sum = mac.finalize().into_bytes();
    // 常量时间比较（Go hmac.Equal 同义——防时序侧信道）
    if !const_time_eq_16(&sum[..16], &pkt[50..66]) {
        return Err(RegVerifyError::BadMac);
    }
    let ts = i64::from_be_bytes(pkt[42..50].try_into().expect("长度已判"));
    let now_s = now
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let d = now_s - ts;
    let w = if window.as_secs() == 0 { REG_WINDOW as i64 } else { window.as_secs() as i64 };
    if d > w || d < -w {
        return Err(RegVerifyError::Expired(d));
    }
    Ok((pubkey, dev_tag))
}

/// 16 字节常量时间比较（Go hmac.Equal 同义——防时序侧信道）。
/// `pub(crate)`：relaywire 的 MAC 校验共用（R4-design §1.1）。
pub(crate) fn const_time_eq_16(a: &[u8], b: &[u8]) -> bool {
    if a.len() != 16 || b.len() != 16 {
        return false;
    }
    let mut diff = 0u8;
    for i in 0..16 {
        diff |= a[i] ^ b[i];
    }
    diff == 0
}

/// 注册拒绝归因（计数键；Go RejX）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RejectReason {
    NoToken,
    Revoked,
    TableFull,
    IpConflict,
}

impl RejectReason {
    fn as_str(self) -> &'static str {
        match self {
            RejectReason::NoToken => "no-token",
            RejectReason::Revoked => "revoked",
            RejectReason::TableFull => "table-full",
            RejectReason::IpConflict => "ip-conflict",
        }
    }

    /// 摘要行的可行动提示（Go rejectHint）。
    fn hint(self) -> &'static str {
        match self {
            RejectReason::Revoked => "该凭证已被吊销。向出口索取新 token（`homeway serve restart` 会铸出新凭证）后重新粘贴；旧 token 不再可用",
            RejectReason::NoToken => "token 抄错或来自别的出口？核对该 token 并在 App 里重新粘贴",
            RejectReason::TableFull => "设备表已满且在线设备不可淘汰；等待失联设备过期或调大 --max-peers",
            RejectReason::IpConflict => "派生地址与在表设备冲突；在该手机上「重置本机身份」后重连",
        }
    }
}

/// 设备表配置（零值走默认）。
pub struct TableConfig {
    pub max_devices: usize,
    /// **0 = 关闭 TTL 回收**（<0 视同关）。
    pub ttl: Duration,
    pub grace: Duration,
    /// 凭证吊销判定（每次验证复查——吊销秒级生效；None = 无吊销面）。
    pub revoked: Option<RevokedHook>,
}

impl Default for TableConfig {
    fn default() -> Self {
        Self {
            max_devices: DEFAULT_MAX_DEVICES,
            ttl: DEFAULT_TTL,
            grace: DEFAULT_GRACE,
            revoked: None,
        }
    }
}

/// 吊销判定钩子（Arc 共享给驱动线程的每次验证复查）。
pub type RevokedHook = std::sync::Arc<dyn Fn(&[u8; 32]) -> bool + Send + Sync>;

/// 注册对 device 的落位操作（按序应用；rotate 的 Remove→Add 顺序在此承载）。
#[derive(Debug, Clone)]
pub enum DevOp {
    Add { pubkey: [u8; 32], psk: [u8; 32], tunnel_ip: Ipv4Addr, tun_ip: Ipv4Addr },
    Remove { pubkey: [u8; 32] },
}

/// 一次注册/回收的快照（日志与测试断言面）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Added,
    Refreshed,
    Rotated,
    Expired,
}

struct Entry {
    #[allow(dead_code)]
    dev: [u8; 8],
    pub_key: [u8; 32],
    psk: [u8; 32],
    ip: Ipv4Addr,
    tun_ip: Ipv4Addr,
    last_reg: SystemTime,
}

/// devTag 键的动态设备表（**驱动线程独占**——Register 在收包路径、GC 在定时拍，
/// 都在同一线程；快照面经 Mutex 只读）。
pub struct DeviceTable {
    secrets: Vec<[u8; 32]>,
    cfg: TableConfig,
    logf: Logf,
    entries: HashMap<[u8; 8], Entry>,
    /// 按原因的累计拒绝计数 + 「首次大声」摘要位。
    rej: HashMap<RejectReason, (u64, bool)>,
}

struct VerifyOk {
    pubkey: [u8; 32],
    dev_tag: [u8; 8],
    secret: [u8; 32],
}

impl DeviceTable {
    pub fn new(secrets: Vec<[u8; 32]>, cfg: TableConfig, logf: Logf) -> Self {
        Self {
            secrets,
            cfg: TableConfig {
                max_devices: if cfg.max_devices == 0 { DEFAULT_MAX_DEVICES } else { cfg.max_devices },
                ttl: cfg.ttl,
                grace: if cfg.grace.is_zero() { DEFAULT_GRACE } else { cfg.grace },
                revoked: cfg.revoked,
            },
            logf,
            entries: HashMap::new(),
            rej: HashMap::new(),
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn limits(&self) -> (usize, Duration, Duration) {
        (self.cfg.max_devices, self.cfg.ttl, self.cfg.grace)
    }

    /// 拒绝计数快照（诊断/测试）。
    pub fn reject_counts(&self) -> HashMap<&'static str, u64> {
        self.rej
            .iter()
            .map(|(k, (n, _))| (k.as_str(), *n))
            .collect()
    }

    /// 逐一试 secrets 全集（个人规模 N 极小）——返回 (公钥, devTag, 命中 secret)。
    fn verify(&self, reg: &[u8], now: SystemTime) -> Result<VerifyOk, RejectReason> {
        for sec in &self.secrets {
            if let Ok((pk, dt)) = verify_reg(sec, reg, now, Duration::ZERO) {
                // 吊销复查（跟随读语义：吊销秒级生效、无需重启）
                if let Some(revoked) = self.cfg.revoked.as_ref() {
                    if revoked(sec) {
                        return Err(RejectReason::Revoked);
                    }
                }
                return Ok(VerifyOk { pubkey: pk, dev_tag: dt, secret: *sec });
            }
        }
        Err(RejectReason::NoToken)
    }

    /// 注册（Go Register 同义）：验证 → 登记/刷新/轮换/拒绝。返回动作快照与 device
    /// 落位操作序列（调用方按序应用）。
    pub fn register(&mut self, reg: &[u8], now: SystemTime) -> Result<(Action, Vec<DevOp>), RejectReason> {
        let VerifyOk { pubkey, dev_tag, secret } = match self.verify(reg, now) {
            Ok(v) => v,
            Err(r) => {
                self.note_reject(r, "");
                return Err(r);
            }
        };
        let psk = *crate::psk::Psk::from(crate::token::Secret::from(secret)).as_bytes();

        if let Some(e) = self.entries.get_mut(&dev_tag) {
            if e.pub_key == pubkey {
                let prev = e.last_reg;
                e.last_reg = now;
                let idle = now.duration_since(prev).unwrap_or_default();
                (self.logf)(&format!(
                    "peer: ~ dev={} refresh (idle={}) n={}/{}",
                    dev_short(&dev_tag),
                    fmt_idle(idle),
                    self.entries.len(),
                    self.cfg.max_devices
                ));
                return Ok((Action::Refreshed, Vec::new()));
            }
            // 身份轮换：先移除旧 peer 再写新的——顺序固定（返回序承载）。
            let old_pub = e.pub_key;
            let old_ip = e.ip;
            let ip = self.assign_ip(&secret, &pubkey, &dev_tag).inspect_err(|&r| {
                self.note_reject(r, "");
            })?;
            let tun_ip = self.assign_tun_ip(&secret, &pubkey, &dev_tag).inspect_err(|&r| {
                self.note_reject(r, "");
            })?;
            let e = self.entries.get_mut(&dev_tag).expect("刚判存在");
            e.pub_key = pubkey;
            e.psk = psk;
            e.ip = ip;
            e.tun_ip = tun_ip;
            e.last_reg = now;
            (self.logf)(&format!(
                "peer: ~ dev={} rotate pub={}→{} ip={}→{} n={}/{}",
                dev_short(&dev_tag),
                pub_short(&old_pub),
                pub_short(&pubkey),
                old_ip,
                ip,
                self.entries.len(),
                self.cfg.max_devices
            ));
            return Ok((
                Action::Rotated,
                vec![
                    DevOp::Remove { pubkey: old_pub },
                    DevOp::Add { pubkey, psk, tunnel_ip: ip, tun_ip },
                ],
            ));
        }

        if self.entries.len() >= self.cfg.max_devices && !self.evict_stale(now) {
            self.note_reject(
                RejectReason::TableFull,
                &format!(
                    "dev={} n={}/{}",
                    dev_short(&dev_tag),
                    self.entries.len(),
                    self.cfg.max_devices
                ),
            );
            return Err(RejectReason::TableFull);
        }
        let ip = self.assign_ip(&secret, &pubkey, &dev_tag).inspect_err(|&r| {
            self.note_reject(r, "");
        })?;
        let tun_ip = self.assign_tun_ip(&secret, &pubkey, &dev_tag).inspect_err(|&r| {
            self.note_reject(r, "");
        })?;
        self.entries.insert(
            dev_tag,
            Entry { dev: dev_tag, pub_key: pubkey, psk, ip, tun_ip, last_reg: now },
        );
        // 同公钥挂两个 devTag 的克隆检测（诊断行）
        if let Some(other) = self.find_by_pub(&pubkey, &dev_tag) {
            (self.logf)(&format!(
                "peer: ! pub={} 同时登记在 dev={} 与 dev={}（疑似同一身份被两台设备使用：克隆/迁移过应用数据？）",
                pub_short(&pubkey),
                dev_short(&other),
                dev_short(&dev_tag)
            ));
        }
        (self.logf)(&format!(
            "peer: + dev={} pub={} ip={} n={}/{}",
            dev_short(&dev_tag),
            pub_short(&pubkey),
            ip,
            self.entries.len(),
            self.cfg.max_devices
        ));
        Ok((Action::Added, vec![DevOp::Add { pubkey, psk, tunnel_ip: ip, tun_ip }]))
    }

    /// TTL 回收（GC 同义）：回收超过 TTL 未刷新的设备（每条打日志），返回快照与 ops。
    pub fn gc(&mut self, now: SystemTime) -> Vec<(Action, Vec<DevOp>)> {
        if self.cfg.ttl.is_zero() {
            return Vec::new();
        }
        let victims: Vec<[u8; 8]> = self
            .entries
            .iter()
            .filter(|(_, e)| {
                now.duration_since(e.last_reg).map(|d| d > self.cfg.ttl).unwrap_or(false)
            })
            .map(|(k, _)| *k)
            .collect();
        let mut out = Vec::new();
        for dev in victims {
            let Some(e) = self.entries.remove(&dev) else { continue };
            let idle = now.duration_since(e.last_reg).unwrap_or_default();
            (self.logf)(&format!(
                "peer: - dev={} reason=ttl (idle={}) n={}/{}",
                dev_short(&dev),
                fmt_idle(idle),
                self.entries.len(),
                self.cfg.max_devices
            ));
            out.push((
                Action::Expired,
                vec![DevOp::Remove { pubkey: e.pub_key }],
            ));
        }
        out
    }

    /// 表满时淘汰：只在「超过 grace 未刷新」的设备里挑最旧的一条（严格 > 比较）。
    /// false = 全部活跃（调用方拒绝新设备，绝不淘汰在线设备）。
    fn evict_stale(&mut self, now: SystemTime) -> bool {
        let mut victim: Option<([u8; 8], SystemTime)> = None;
        for (k, e) in &self.entries {
            let idle = now.duration_since(e.last_reg).unwrap_or_default();
            if idle <= self.cfg.grace {
                continue; // 活跃宽限期内：不碰
            }
            if victim.is_none() || e.last_reg < victim.as_ref().unwrap().1 {
                victim = Some((*k, e.last_reg));
            }
        }
        let Some((dev, _)) = victim else { return false };
        let e = self.entries.remove(&dev).expect("刚选出的 victim");
        let idle = now.duration_since(e.last_reg).unwrap_or_default();
        (self.logf)(&format!(
            "peer: - dev={} reason=stale (idle={}) n={}/{}",
            dev_short(&dev),
            fmt_idle(idle),
            self.entries.len(),
            self.cfg.max_devices
        ));
        true
    }

    /// 隧道地址派生 + 双地址并集占用检测（Go assignIPLocked 同义）。
    fn assign_ip(&mut self, secret: &[u8; 32], pub_key: &[u8; 32], dev: &[u8; 8]) -> Result<Ipv4Addr, RejectReason> {
        let ip = derive_tunnel_ip(&crate::token::Secret::from(*secret), pub_key);
        if !self.ip_taken(&ip) {
            return Ok(ip);
        }
        // 同公钥不同 devTag（克隆场景）：同钥派生地址本就相同，不算冲突——沿用
        if self.ip_held_by_pub(pub_key, &ip) {
            return Ok(ip);
        }
        self.note_reject(
            RejectReason::IpConflict,
            &format!("ip={ip}（派生地址与在表设备撞车；消解 = 手机上「重置本机身份」后重连）"),
        );
        let _ = dev;
        Err(RejectReason::IpConflict)
    }

    fn assign_tun_ip(&mut self, secret: &[u8; 32], pub_key: &[u8; 32], dev: &[u8; 8]) -> Result<Ipv4Addr, RejectReason> {
        let ip = derive_tun_ip(&crate::token::Secret::from(*secret), pub_key);
        if !self.ip_taken(&ip) {
            return Ok(ip);
        }
        if self.ip_held_by_pub(pub_key, &ip) {
            return Ok(ip);
        }
        self.note_reject(
            RejectReason::IpConflict,
            &format!("tunip={ip}（应用面派生地址与在表设备撞车）"),
        );
        let _ = dev;
        Err(RejectReason::IpConflict)
    }

    /// 双地址集合占用判定（任一类的 /32 撞车都让 allowedips 错路由——并集判定）。
    fn ip_taken(&self, ip: &Ipv4Addr) -> bool {
        self.entries.values().any(|e| &e.ip == ip || &e.tun_ip == ip)
    }

    fn ip_held_by_pub(&self, pub_key: &[u8; 32], ip: &Ipv4Addr) -> bool {
        self.entries
            .values()
            .any(|e| &e.pub_key == pub_key && (&e.ip == ip || &e.tun_ip == ip))
    }

    fn find_by_pub(&self, pub_key: &[u8; 32], except: &[u8; 8]) -> Option<[u8; 8]> {
        self.entries
            .iter()
            .find(|(k, e)| k != &except && &e.pub_key == pub_key)
            .map(|(k, _)| *k)
    }

    /// 拒绝计数 + 细节行 + 摘要首大声（每类原因每进程一次）。
    fn note_reject(&mut self, reason: RejectReason, extra: &str) {
        let entry = self.rej.entry(reason).or_insert((0, false));
        entry.0 += 1;
        let n = entry.0;
        let first = !entry.1;
        if first {
            entry.1 = true;
            (self.logf)(&format!(
                "⚠️ 注册被拒（原因={}，累计 {n}）——{}",
                reason.as_str(),
                reason.hint()
            ));
        }
        (self.logf)(&format!(
            "peer: ! {}reject reason={}{}（原因={}，累计 {n}）",
            if reason == RejectReason::TableFull { "dev=… ".to_string() } else { String::new() },
            reason.as_str(),
            if extra.is_empty() { String::new() } else { format!(" {extra}") },
            reason.as_str(),
        ));
    }
}

fn dev_short(d: &[u8; 8]) -> String {
    d.iter().take(4).map(|b| format!("{b:02x}")).collect()
}

fn pub_short(p: &[u8; 32]) -> String {
    p.iter().take(4).map(|b| format!("{b:02x}")).collect()
}

/// idle 的 Go Duration 圆整形态（roundDur = Round(1s)——判据行 `idle=7m47s` 形态）。
fn fmt_idle(d: Duration) -> String {
    let s = d.as_secs_f64().round() as u64;
    crate::go_fmt::fmt_duration_go_secs(Duration::from_secs(s))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wtransport::reg;
    use crate::identity::DevTag;
    use std::sync::{Arc, Mutex};

    fn reg_bytes(secret: &[u8; 32], pubkey: &[u8; 32], dev: &[u8; 8], ts: u64) -> Vec<u8> {
        let mut out = Vec::with_capacity(REG_LEN);
        out.extend_from_slice(b"H2");
        out.extend_from_slice(pubkey);
        out.extend_from_slice(dev);
        out.extend_from_slice(&ts.to_be_bytes());
        let mut mac = Hmac::<Sha256>::new_from_slice(secret).unwrap();
        mac.update(b"hr-reg2");
        mac.update(&out[2..50]);
        let sum = mac.finalize().into_bytes();
        out.extend_from_slice(&sum[..16]);
        out
    }

    struct LogSink(Arc<Mutex<Vec<String>>>);

    fn sink() -> (LogSink, Logf) {
        let lines: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let l2 = Arc::clone(&lines);
        let logf: Logf = Arc::new(move |s: &str| {
            l2.lock().unwrap().push(s.to_string());
        });
        (LogSink(lines), logf)
    }

    fn now() -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000)
    }

    fn now_unix() -> u64 {
        1_800_000_000
    }

    fn add_ops(ops: &[DevOp]) -> Vec<(Ipv4Addr, Ipv4Addr)> {
        ops.iter()
            .filter_map(|op| match op {
                DevOp::Add { tunnel_ip, tun_ip, .. } => Some((*tunnel_ip, *tun_ip)),
                _ => None,
            })
            .collect()
    }

    /// add → refresh → rotate → 拒绝全族 + 判据行文案。
    #[test]
    fn register_lifecycle_and_lines() {
        let secret = [0x42u8; 32];
        let (sink, logf) = sink();
        let mut t = DeviceTable::new(vec![secret], TableConfig::default(), logf);
        let (pk1, dt1, pk2) = ([1u8; 32], [0xAA; 8], [2u8; 32]);

        // add
        let (a, ops) = t.register(&reg_bytes(&secret, &pk1, &dt1, now_unix()), now()).unwrap();
        assert_eq!(a, Action::Added);
        assert_eq!(ops.len(), 1);
        let (ip1, _tun1) = add_ops(&ops)[0];
        assert_eq!(t.len(), 1);
        assert!(sink.0.lock().unwrap().iter().any(|l| l.contains(&format!("peer: + dev={} pub={} ip={}", dev_short(&dt1), pub_short(&pk1), ip1))));

        // refresh（同钥同 dev）
        let (a, ops) = t.register(&reg_bytes(&secret, &pk1, &dt1, now_unix() + 5), now() + Duration::from_secs(5)).unwrap();
        assert_eq!(a, Action::Refreshed);
        assert!(ops.is_empty());
        assert!(sink.0.lock().unwrap().iter().any(|l| l.starts_with(&format!("peer: ~ dev={} refresh", dev_short(&dt1)))));

        // rotate（同 dev 换钥）：Remove→Add 顺序
        let (a, ops) = t.register(&reg_bytes(&secret, &pk2, &dt1, now_unix() + 10), now() + Duration::from_secs(10)).unwrap();
        assert_eq!(a, Action::Rotated);
        assert!(matches!(ops[0], DevOp::Remove { pubkey } if pubkey == pk1));
        assert!(matches!(ops[1], DevOp::Add { pubkey, .. } if pubkey == pk2));
        assert!(sink.0.lock().unwrap().iter().any(|l| l.contains("rotate pub=")));

        // no-token 拒绝（错 secret）
        let bad = [0x99u8; 32];
        let r = t.register(&reg_bytes(&bad, &pk1, &dt1, now_unix()), now());
        assert_eq!(r.unwrap_err(), RejectReason::NoToken);
        assert_eq!(t.reject_counts().get("no-token"), Some(&1));
        // 过期窗口
        let r = t.register(&reg_bytes(&secret, &pk1, &dt1, now_unix() - 200), now());
        assert!(matches!(r, Err(RejectReason::NoToken)), "过期报文验不过（归因 no-token——Go reason=verify 同并入）");
        // 畸形
        let r = t.register(b"short", now());
        assert_eq!(r.unwrap_err(), RejectReason::NoToken);
    }

    /// 表满：活跃全在 ⇒ 拒绝；超宽限的最旧被淘汰（stale 行）。
    #[test]
    fn table_full_and_stale_eviction() {
        let secret = [7u8; 32];
        let (sink, logf) = sink();
        let mut t = DeviceTable::new(vec![secret], TableConfig { max_devices: 2, ..Default::default() }, logf);
        let base = now();
        // 两个设备，d1 已超宽限（11 分钟前注册）
        let old_ts = now_unix() - (DEFAULT_GRACE.as_secs() + 60);
        t.register(&reg_bytes(&secret, &[1; 32], &[1; 8], old_ts), base - DEFAULT_GRACE - Duration::from_secs(60)).unwrap();
        t.register(&reg_bytes(&secret, &[2; 32], &[2; 8], now_unix()), base).unwrap();
        assert_eq!(t.len(), 2);
        // 第三个：淘汰 d1（超宽限最旧）
        let (a, _) = t.register(&reg_bytes(&secret, &[3; 32], &[3; 8], now_unix()), base).unwrap();
        assert_eq!(a, Action::Added);
        assert_eq!(t.len(), 2, "淘汰一个加一个");
        assert!(sink.0.lock().unwrap().iter().any(|l| l.contains("reason=stale")));
        // 全活跃 ⇒ 拒绝
        let r = t.register(&reg_bytes(&secret, &[4; 32], &[4; 8], now_unix()), base);
        assert_eq!(r.unwrap_err(), RejectReason::TableFull);
        assert_eq!(t.reject_counts().get("table-full"), Some(&1));
    }

    /// TTL 回收（gc）。
    #[test]
    fn ttl_gc() {
        let secret = [9u8; 32];
        let (sink, logf) = sink();
        let mut t = DeviceTable::new(vec![secret], TableConfig { ttl: Duration::from_secs(3600), ..Default::default() }, logf);
        let base = now();
        t.register(&reg_bytes(&secret, &[1; 32], &[1; 8], now_unix() - 7200), base - Duration::from_secs(7200)).unwrap();
        t.register(&reg_bytes(&secret, &[2; 32], &[2; 8], now_unix()), base).unwrap();
        let out = t.gc(base);
        assert_eq!(out.len(), 1);
        assert!(matches!(out[0].1[0], DevOp::Remove { .. }));
        assert_eq!(t.len(), 1);
        assert!(sink.0.lock().unwrap().iter().any(|l| l.contains("reason=ttl (idle=2h0m0s)")));
    }

    /// 吊销跟随：revoked 钩子命中即拒（秒级语义——每次验证复查）。
    #[test]
    fn revoked_hook_rejects() {
        let secret = [0x55u8; 32];
        let (_sink, logf) = sink();
        let flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let f2 = Arc::clone(&flag);
        let revoked = move |s: &[u8; 32]| s == &secret && f2.load(std::sync::atomic::Ordering::SeqCst);
        let mut t = DeviceTable::new(vec![secret], TableConfig { revoked: Some(std::sync::Arc::new(revoked)), ..Default::default() }, logf);
        // 未吊销：正常
        t.register(&reg_bytes(&secret, &[1; 32], &[1; 8], now_unix()), now()).unwrap();
        // 吊销：同 secret 拒（归因 revoked）
        flag.store(true, std::sync::atomic::Ordering::SeqCst);
        let r = t.register(&reg_bytes(&secret, &[2; 32], &[2; 8], now_unix()), now());
        assert_eq!(r.unwrap_err(), RejectReason::Revoked);
    }

    /// reg 编码真源交叉验（wtransport::reg 的 encode → 本表 verify）。
    #[test]
    fn verify_accepts_client_encoding() {
        let secret_bytes = [0x42u8; 32];
        let secret = crate::token::Secret::from(secret_bytes);
        let pubkey = [3u8; 32];
        let dev = DevTag([0xBB; 8]);
        let ts = 1_800_000_000u64;
        let mut pkt2 = Vec::new();
        reg::encode_reg_parts(&secret, &pubkey, dev.as_bytes(), ts, &mut pkt2);
        let (pk, dt) = verify_reg(&secret_bytes, &pkt2, SystemTime::UNIX_EPOCH + Duration::from_secs(ts), Duration::ZERO).unwrap();
        assert_eq!(pk, pubkey);
        assert_eq!(dt, *dev.as_bytes());
    }
}
