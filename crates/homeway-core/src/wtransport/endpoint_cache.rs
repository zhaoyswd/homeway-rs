//! 端点学习缓存（**内存版**——R1 范围：任务指令明确要求内存实现；落盘/巡检接线属 R2）。
//!
//! 语义真源 `baseline:clientcore/internal/wtransport/endpointcache.go`（本期裁剪面：
//! Observe/MarkVerified/Entries/Merge 的 TTL 与排序语义逐条对齐；Save/mergeDiskLocked
//! 落盘族不实现）。四种来源 provenance：
//! - `Token`：静态候选（不进缓存，Merge 时由调用方传入）；
//! - `Inband`：后端隧道内自报（认证、最强；生产者未实现，来源保留）；
//! - `Hint`：中继观察（不可信）；
//! - `Probe`：探测应答端点列表（非认证，与 hint 同级信任）。
//!
//! 记录带 `learned_at`/`verified_at`（Unix 毫秒）；**verified 只在「该地址真正跑通过一次
//! 认证握手」后打**；新鲜度 = max(learned, verified)，TTL 7 天。

use std::collections::HashMap;
use std::net::SocketAddr;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// 学习地址的有效期（Go `LearnedEndpointTTL`）。
pub const LEARNED_ENDPOINT_TTL: Duration = Duration::from_secs(7 * 24 * 3600);

/// 学习来源（provenance；强度 Inband > Hint = Probe > Token）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[non_exhaustive]
pub enum EndpointSource {
    Token,
    Hint,
    Probe,
    Inband,
}

impl EndpointSource {
    fn strength(self) -> u8 {
        match self {
            EndpointSource::Token => 1,
            EndpointSource::Hint | EndpointSource::Probe => 2,
            EndpointSource::Inband => 3,
        }
    }
}

/// 一条学习记录（时间戳 Unix 毫秒——同秒内仍能定序，Go 同义）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LearnedEndpoint {
    pub addr: SocketAddr,
    pub source: EndpointSource,
    pub learned_at: i64,
    pub verified_at: i64,
}

impl LearnedEndpoint {
    /// 是否已验证过（一次成功的认证会话）。
    pub fn verified(&self) -> bool {
        self.verified_at > 0
    }
    /// 新鲜度基准 = max(learned, verified)（持续被验证的长连端点不得被 TTL 静默删）。
    fn freshness(&self) -> i64 {
        self.verified_at.max(self.learned_at)
    }
    fn valid(&self, now_ms: i64, ttl: Duration) -> bool {
        self.learned_at > 0 && now_ms.saturating_sub(self.freshness()) <= ttl.as_millis() as i64
    }
}

/// 一个后端一份的端点缓存。`dir` 非空 = 落盘形态（`<dir>/<peerID hex>.json`，
/// 原子写 + 内容未变不写 + 写前重读合并——Go endpointcache.go:256-322）。
pub struct EndpointCache {
    entries: HashMap<SocketAddr, LearnedEndpoint>,
    ttl: Option<Duration>,
    dir: Option<std::path::PathBuf>,
    peer_hex: String,
    /// 上次写盘内容（节流：内容未变不写）。
    last_raw: String,
    logf: Option<crate::Logf>,
}

fn now_ms(t: SystemTime) -> i64 {
    t.duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

impl Default for EndpointCache {
    fn default() -> Self {
        Self::new()
    }
}

impl EndpointCache {
    /// 内存形态（无落盘）。
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
            ttl: None,
            dir: None,
            peer_hex: String::new(),
            last_raw: String::new(),
            logf: None,
        }
    }

    /// 打开（不存在则空）某后端的端点缓存；读盘失败安全降级为空（缓存坏了不该
    /// 影响建连——Go OpenEndpointCache 同义）。
    pub fn open(dir: &std::path::Path, peer_id: crate::token::PeerId) -> Self {
        let mut c = Self::new();
        c.dir = Some(dir.to_path_buf());
        c.peer_hex = hex_encode(peer_id.as_bytes());
        c.load();
        c
    }

    pub fn set_logger(&mut self, logf: crate::Logf) {
        self.logf = Some(logf);
    }

    fn path(&self) -> Option<std::path::PathBuf> {
        self.dir.as_ref().map(|d| d.join(format!("{}.json", self.peer_hex)))
    }

    fn load(&mut self) {
        let Some(p) = self.path() else { return };
        let raw = match std::fs::read(&p) {
            Ok(r) => r,
            Err(_) => return, // 文件缺失/不可读：空缓存
        };
        let f: CacheFile = match serde_json::from_slice(&raw) {
            Ok(f) => f,
            Err(e) => {
                if let Some(l) = &self.logf {
                    l(&format!("ENDPOINTCACHE 读盘失败（忽略，用空缓存）：{e}"));
                }
                return;
            }
        };
        for e in f.entries {
            if let Ok(ap) = e.endpoint.parse::<SocketAddr>() {
                self.entries.insert(
                    ap,
                    LearnedEndpoint {
                        addr: ap,
                        source: e.source.into(),
                        learned_at: e.learned_at,
                        verified_at: e.verified_at.unwrap_or(0),
                    },
                );
            }
        }
    }

    /// 落盘（内容未变不写；原子写 tmp+rename；**写前重读合并**——同一 peerID 的两份
    /// 缓存实例各持内存表，整文件覆盖会抹掉对方学到的条目：磁盘上本实例没有的条目
    /// 先并入，同址以内存为准）。
    pub fn save(&mut self, now: SystemTime) -> std::io::Result<()> {
        let Some(_dir) = &self.dir else { return Ok(()) };
        self.merge_disk();
        let file = CacheFile {
            peer: if self.peer_hex.is_empty() { None } else { Some(self.peer_hex.clone()) },
            entries: self
                .entries(now)
                .into_iter()
                .map(|e| CacheEntry {
                    endpoint: e.addr.to_string(),
                    source: e.source.into(),
                    learned_at: e.learned_at,
                    verified_at: (e.verified_at > 0).then_some(e.verified_at),
                })
                .collect(),
        };
        // 键序 = Go 结构体声明序（endpoint/source/learnedAt/verifiedAt omitempty）——
        // 同数据与 Go 字节相同（serde 无 map 参与键排序，struct 字段序即声明序）。
        let raw = serde_json::to_vec(&file).map_err(std::io::Error::other)?;
        if raw == self.last_raw.as_bytes() {
            return Ok(());
        }
        let p = self.path().expect("dir 已判在");
        std::fs::create_dir_all(p.parent().expect("路径必有父"))?;
        let tmp = p.with_file_name(format!(
            "{}.tmp.{}.{}",
            p.file_name().expect("有名").to_string_lossy(),
            std::process::id(),
            now.duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0),
        ));
        std::fs::write(&tmp, &raw)?;
        match std::fs::rename(&tmp, &p) {
            Ok(()) => {
                self.last_raw = String::from_utf8_lossy(&raw).into_owned();
                Ok(())
            }
            Err(e) => {
                let _ = std::fs::remove_file(&tmp); // 只删自己的 tmp（名字唯一）
                Err(e)
            }
        }
    }

    /// 写前重读合并：磁盘上本实例没有的条目并入内存（读盘失败静默跳过——合并是
    /// 保护性的）。
    fn merge_disk(&mut self) {
        let Some(p) = self.path() else { return };
        let raw = match std::fs::read(&p) {
            Ok(r) => r,
            Err(_) => return,
        };
        let Ok(f) = serde_json::from_slice::<CacheFile>(&raw) else { return };
        for e in f.entries {
            if let Ok(ap) = e.endpoint.parse::<SocketAddr>() {
                self.entries.entry(ap).or_insert(LearnedEndpoint {
                    addr: ap,
                    source: e.source.into(),
                    learned_at: e.learned_at,
                    verified_at: e.verified_at.unwrap_or(0),
                });
            }
        }
    }

    /// 记录一条学习到的地址（hint/inband/token）：刷新时间；来源按强度升级。
    pub fn observe(&mut self, addr: SocketAddr, source: EndpointSource, now: SystemTime) {
        let ts = now_ms(now);
        let e = self.entries.entry(addr).or_insert(LearnedEndpoint {
            addr,
            source,
            learned_at: ts,
            verified_at: 0,
        });
        e.learned_at = ts;
        if source.strength() > e.source.strength() {
            e.source = source;
        }
    }

    /// 标记「该地址上完成过一次成功认证会话」（缺记录时补建）。
    pub fn mark_verified(&mut self, addr: SocketAddr, source: EndpointSource, now: SystemTime) {
        let ts = now_ms(now);
        let e = self.entries.entry(addr).or_insert(LearnedEndpoint {
            addr,
            source,
            learned_at: ts,
            verified_at: 0,
        });
        e.verified_at = ts;
        if source.strength() > e.source.strength() {
            e.source = source;
        }
    }

    /// TTL 内、按「已验证优先 → 最近验证 → 最近学习 → 地址定序」排序的记录。
    pub fn entries(&self, now: SystemTime) -> Vec<LearnedEndpoint> {
        let now = now_ms(now);
        let mut out: Vec<_> = self
            .entries
            .values()
            .copied()
            .filter(|e| e.valid(now, self.ttl.unwrap_or(LEARNED_ENDPOINT_TTL)))
            .collect();
        out.sort_by(|a, b| {
            b.verified().cmp(&a.verified())
                .then(b.verified_at.cmp(&a.verified_at))
                .then(b.learned_at.cmp(&a.learned_at))
                .then(a.addr.to_string().cmp(&b.addr.to_string()))
        });
        out
    }

    /// 组装建连候选：学习到的在前，静态 token 候选去重接上；学习地址一律按 direct
    /// 处理（中继腿由 token 给——R1 无中继腿，`relay` 恒 false，签名保留 bool 对齐
    /// Go 的 Merge 形状、R2 落盘接线时扩展）。
    pub fn merge(&self, static_cands: &[SocketAddr], now: SystemTime) -> Vec<SocketAddr> {
        let mut out = Vec::with_capacity(self.entries.len() + static_cands.len());
        let mut seen = std::collections::HashSet::new();
        for e in self.entries(now) {
            if seen.insert(e.addr) {
                out.push(e.addr);
            }
        }
        for s in static_cands {
            if seen.insert(*s) {
                out.push(*s);
            }
        }
        out
    }
}

// ---------- 落盘形态（Go endpointFile：Peer/Entries 均 omitempty；键序 = 声明序） ----------

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct CacheEntry {
    endpoint: String,
    source: EndpointSourceSerde,
    learned_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    verified_at: Option<i64>,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct CacheFile {
    #[serde(skip_serializing_if = "Option::is_none")]
    peer: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    entries: Vec<CacheEntry>,
}

/// wire 形态的来源串（Go EndpointSource 字符串族；Rust enum 的 serde 面）。
#[derive(Debug, serde::Serialize, serde::Deserialize, Clone, Copy, PartialEq)]
enum EndpointSourceSerde {
    #[serde(rename = "token")]
    Token,
    #[serde(rename = "inband")]
    Inband,
    #[serde(rename = "hint")]
    Hint,
    #[serde(rename = "probe")]
    Probe,
}

impl From<EndpointSourceSerde> for EndpointSource {
    fn from(s: EndpointSourceSerde) -> Self {
        match s {
            EndpointSourceSerde::Token => EndpointSource::Token,
            EndpointSourceSerde::Inband => EndpointSource::Inband,
            EndpointSourceSerde::Hint => EndpointSource::Hint,
            EndpointSourceSerde::Probe => EndpointSource::Probe,
        }
    }
}

impl From<EndpointSource> for EndpointSourceSerde {
    fn from(s: EndpointSource) -> Self {
        match s {
            EndpointSource::Token => EndpointSourceSerde::Token,
            EndpointSource::Inband => EndpointSourceSerde::Inband,
            EndpointSource::Hint => EndpointSourceSerde::Hint,
            EndpointSource::Probe => EndpointSourceSerde::Probe,
        }
    }
}

fn hex_encode(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(port: u16) -> SocketAddr {
        format!("127.0.0.1:{port}").parse().unwrap()
    }

    #[test]
    fn ttl_freshness_and_ordering() {
        let mut c = EndpointCache::new();
        let t0 = UNIX_EPOCH + Duration::from_secs(1_000_000);
        c.observe(addr(1), EndpointSource::Hint, t0);
        c.observe(addr(2), EndpointSource::Probe, t0);
        c.mark_verified(addr(2), EndpointSource::Probe, t0);

        let list = c.entries(t0);
        assert_eq!(list.len(), 2);
        assert!(list[0].verified(), "已验证优先");
        assert_eq!(list[0].addr, addr(2));

        // verified 刷新新鲜度：learned 满 TTL 但 verified 在 TTL 内 ⇒ 仍有效
        let later = t0 + Duration::from_secs(7 * 24 * 3600 - 10);
        c.mark_verified(addr(2), EndpointSource::Probe, later);
        let much = later + Duration::from_secs(7 * 24 * 3600 - 10);
        let list = c.entries(much);
        assert_eq!(list.len(), 1, "只有持续验证的活下来");
        assert_eq!(list[0].addr, addr(2));

        // 未验证的满 TTL 即失效
        let stale = t0 + Duration::from_secs(7 * 24 * 3600 + 1);
        assert!(c.entries(stale).iter().all(|e| e.addr != addr(1)));
    }

    #[test]
    fn source_upgrade_and_merge_dedup() {
        let mut c = EndpointCache::new();
        let t0 = UNIX_EPOCH + Duration::from_secs(1_000_000);
        c.observe(addr(1), EndpointSource::Hint, t0);
        c.observe(addr(1), EndpointSource::Inband, t0); // 升级
        assert_eq!(c.entries(t0)[0].source, EndpointSource::Inband);
        c.observe(addr(1), EndpointSource::Token, t0); // 弱源不降级
        assert_eq!(c.entries(t0)[0].source, EndpointSource::Inband);

        // Merge：学习在前、静态去重接上
        let merged = c.merge(&[addr(1), addr(2)], t0);
        assert_eq!(merged, vec![addr(1), addr(2)]);
    }
}
