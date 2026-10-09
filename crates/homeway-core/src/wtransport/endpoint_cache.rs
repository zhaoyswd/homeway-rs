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

use super::bind::Candidate;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// 学习地址的有效期（Go `LearnedEndpointTTL`）。
pub const LEARNED_ENDPOINT_TTL: Duration = Duration::from_secs(7 * 24 * 3600);
/// 单后端学习缓存条数上限（F4；Go 无此上限——本批为**有意分歧**，已登记）。
pub const MAX_ENTRIES: usize = 64;
/// 投喂配额窗口（F4）：每窗口内**新增未验证地址**条数上限（hint/probe 共用计数器）。
pub const FEED_WINDOW: Duration = Duration::from_secs(60);
/// 投喂配额上限（F4）：一次探测应答可注入 ≤8 条永久候选，多个应答/多次 hint 可无限
/// 灌入；只计**新增**（刷新已有条目不计），rearm 归零。
pub const FEED_MAX_NEW: usize = 24;

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
    /// 条数上限（F4：cap 是唯一必须的硬界；落 observe/mark_verified/load/merge_disk/
    /// entries 出口——只落写入侧的话 `merge_disk`/`load` 仍能绕过）。
    max_entries: usize,
    /// 投喂配额窗口起点与窗口内新增未验证地址数（F4；hint/probe 共用）。
    feed_win: Option<SystemTime>,
    feed_new: usize,
}

fn now_ms(t: SystemTime) -> i64 {
    t.duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// 缓存键归一（M2 代码门 G3，同类 D1）：v4-mapped 先归纯 v4。
///
/// 缓存键 = `HashMap<SocketAddr, _>` 的地址，且 `merge` 的 relay 位反查、verified
/// 保护档、条数上限/投喂配额都按它比对：不归一时同一地址的 mapped/纯 v4 两形态各占
/// 一条（双计配额、双倍条数，中继位反查失配 ⇒ 候选按直连裸发），`verified` 只打在
/// 其中一种形态上。四个入口（`load` / `merge_disk` / `observe` / `mark_verified`）
/// 统一走本函数 ⇒ **「缓存键恒为规范纯 v4/v6」是类型不变量**（Go 侧无此归一，
/// 属有意收口：规范输入逐字同行为）。
fn canon(addr: SocketAddr) -> SocketAddr {
    crate::udpbatch::unmap_v4_in6(addr)
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
            max_entries: MAX_ENTRIES,
            feed_win: None,
            feed_new: 0,
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
                let ap = canon(ap);
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
        self.trim();
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
        // Q-G F4（B 组新增）：**创建即 0600** + fchmod 归一——旧形态是裸
        // `fs::write`（完全无权限收紧）；Go = `os.WriteFile(tmp, raw, 0o600)`
        // （`endpointcache.go:290`）⇒ 移植回退面收口（内容 = 学到的候选端点）。
        // 失败**告警不阻断**（代码门②：与 state.rs 的告警形态一致）。
        {
            use std::io::Write as _;
            use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&tmp)?;
            if let Err(e) = f.set_permissions(std::fs::Permissions::from_mode(0o600)) {
                eprintln!("homeway: ⚠️ 端点缓存 {} 收紧 0600 失败（{e}）——建议手工 chmod", tmp.display());
            }
            f.write_all(&raw)?;
        }
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
                let ap = canon(ap);
                self.entries.entry(ap).or_insert(LearnedEndpoint {
                    addr: ap,
                    source: e.source.into(),
                    learned_at: e.learned_at,
                    verified_at: e.verified_at.unwrap_or(0),
                });
            }
        }
        self.trim();
    }

    /// 记录一条学习到的地址（hint/inband/token）：刷新时间；来源按强度升级。
    /// 返回 false = 被**投喂配额**拒绝（F4：本窗口新增未验证地址超限；刷新已有条目
    /// 不计配额、永不被拒）。
    pub fn observe(&mut self, addr: SocketAddr, source: EndpointSource, now: SystemTime) -> bool {
        let addr = canon(addr);
        if !self.feed_allows(addr, source, now) {
            return false;
        }
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
        self.trim_except(Some(addr));
        true
    }

    /// 标记「该地址上完成过一次成功认证会话」（缺记录时补建）。verified 是保护档，
    /// 不受投喂配额约束。
    pub fn mark_verified(&mut self, addr: SocketAddr, source: EndpointSource, now: SystemTime) {
        let addr = canon(addr);
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
        self.trim_except(Some(addr));
    }

    /// rearm 归零投喂配额（F4：软/硬赛跑是「重新开始」的语义边界）。
    pub fn note_rearm(&mut self) {
        self.feed_win = None;
        self.feed_new = 0;
    }

    /// 投喂配额判定：只对**新增**的不可信来源（hint/probe）计数；刷新已有条目、
    /// 以及可信来源（inband/token）不受限。
    fn feed_allows(&mut self, addr: SocketAddr, source: EndpointSource, now: SystemTime) -> bool {
        if self.entries.contains_key(&addr) {
            return true;
        }
        if matches!(source, EndpointSource::Inband | EndpointSource::Token) {
            return true;
        }
        let t = now_ms(now);
        let expired = self
            .feed_win
            .is_none_or(|w| t.saturating_sub(now_ms(w)) >= FEED_WINDOW.as_millis() as i64);
        if expired {
            self.feed_win = Some(now);
            self.feed_new = 0;
        }
        if self.feed_new >= FEED_MAX_NEW {
            return false;
        }
        self.feed_new += 1;
        true
    }

    /// 超 cap 时从**排序尾部**淘汰（保护 verified：未验证优先、其中最旧 learned 先出；
    /// 全 verified 时淘汰最久未验证的）。
    fn trim(&mut self) {
        self.trim_except(None);
    }

    /// `protect` = 本次刚接纳的新条目——**新条目永不被拒**（设计 F4 的核心：全 verified
    /// 且表满时若把新 hint 自己淘汰掉，就等于「存不进」，漫游/换网新线索永久丢失）。
    /// 故淘汰只在**既有条目**里挑排序尾部。
    fn trim_except(&mut self, protect: Option<SocketAddr>) {
        while self.entries.len() > self.max_entries {
            let ranked: Vec<LearnedEndpoint> = self
                .sorted_all()
                .into_iter()
                .filter(|e| Some(e.addr) != protect)
                .collect();
            let Some(victim) = ranked.last() else { break };
            self.entries.remove(&victim.addr);
        }
    }

    /// 全量排序（不按 TTL 过滤——淘汰必须能清掉过期条目，否则过期条目永不退场）。
    fn sorted_all(&self) -> Vec<LearnedEndpoint> {
        let mut out: Vec<_> = self.entries.values().copied().collect();
        out.sort_by(cmp_rank);
        out
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
        out.sort_by(cmp_rank);
        out.truncate(self.max_entries); // 出口截断（F4：C13 条数上界）
        out
    }

    /// 组装建连候选：学习到的在前（按已验证/新鲜排序），静态 token 候选去重接上。
    /// 学习地址一律按 direct 处理；**但**同一地址在 static 里是中继条目时沿用 Relay
    /// 标记（按地址去重把类型位吃掉正是 token relay 腿失效那类事故的同源形态——
    /// Go endpointcache.go:236-256 逐条）。
    pub fn merge(&self, static_cands: &[Candidate], now: SystemTime) -> Vec<Candidate> {
        let relay_addrs: std::collections::HashSet<SocketAddr> =
            static_cands.iter().filter(|c| c.relay).map(|c| c.addr).collect();
        let mut out: Vec<Candidate> = Vec::with_capacity(self.entries.len() + static_cands.len());
        let mut seen = std::collections::HashSet::new();
        for e in self.entries(now) {
            if seen.insert(e.addr) {
                out.push(Candidate { addr: e.addr, relay: relay_addrs.contains(&e.addr) });
            }
        }
        for s in static_cands {
            if seen.insert(s.addr) {
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
    #[serde(rename = "learnedAt")]
    learned_at: i64,
    #[serde(rename = "verifiedAt", skip_serializing_if = "Option::is_none")]
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

/// 测试面：按 Go endpointFile 序列化形状拼 JSON（与 save 的 CacheFile 同形；
/// entries 按 Go validLocked 排序——已验证优先、新鲜优先）。
pub fn debug_wire_json(c: &EndpointCache, peer_hex: &str, now: SystemTime) -> String {
    let entries = c.entries(now);
    let file = CacheFile {
        peer: Some(peer_hex.to_owned()),
        entries: entries
            .iter()
            .map(|e| CacheEntry {
                endpoint: e.addr.to_string(),
                source: e.source.into(),
                learned_at: e.learned_at,
                verified_at: (e.verified_at > 0).then_some(e.verified_at),
            })
            .collect(),
    };
    serde_json::to_string(&file).unwrap()
}

fn hex_encode(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// 排序键（`entries()` 与淘汰共用）：已验证优先 → 最近验证 → 最近学习 → 地址定序。
fn cmp_rank(a: &LearnedEndpoint, b: &LearnedEndpoint) -> std::cmp::Ordering {
    b.verified()
        .cmp(&a.verified())
        .then(b.verified_at.cmp(&a.verified_at))
        .then(b.learned_at.cmp(&a.learned_at))
        .then(a.addr.to_string().cmp(&b.addr.to_string()))
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

    /// M2 代码门 G3（D1 同类）：缓存键归一——同一地址的 mapped / 纯 v4 两形态算
    /// **一条**（键恒为规范纯 v4；不归一时双计投喂配额、双倍条数、verified 只打在
    /// 其中一种形态上）。
    #[test]
    fn keys_normalize_v4_mapped() {
        let mut c = EndpointCache::new();
        let t0 = UNIX_EPOCH + Duration::from_secs(1_000_000);
        let pure: SocketAddr = "198.51.100.7:41641".parse().unwrap();
        let mapped: SocketAddr = "[::ffff:198.51.100.7]:41641".parse().unwrap();
        assert!(c.observe(mapped, EndpointSource::Hint, t0));
        assert!(c.observe(pure, EndpointSource::Probe, t0), "同址刷新（非新增）永不被配额拒");
        let list = c.entries(t0);
        assert_eq!(list.len(), 1, "mapped 与纯 v4 同址必须一条");
        assert_eq!(list[0].addr, pure, "键 = 规范纯 v4");
        assert_eq!(list[0].source, EndpointSource::Hint, "Hint/Probe 同强度：来源不改写（原语义）");
        // 强来源升级仍生效（同一条记录）
        assert!(c.observe(mapped, EndpointSource::Inband, t0));
        assert_eq!(c.entries(t0)[0].source, EndpointSource::Inband, "来源按强度升级");
        // mark_verified 以 mapped 形态入参 ⇒ 打在同一条上
        c.mark_verified(mapped, EndpointSource::Probe, t0 + Duration::from_secs(1));
        let list = c.entries(t0 + Duration::from_secs(1));
        assert_eq!(list.len(), 1);
        assert!(list[0].verified(), "verified 打在规范键那一条上");
        // 真 v6 原样（不归一）
        let v6: SocketAddr = "[2001:db8::2]:41641".parse().unwrap();
        assert!(c.observe(v6, EndpointSource::Hint, t0));
        assert!(c.entries(t0).iter().any(|e| e.addr == v6));
    }

    /// G3 同类：读盘也归一（旧文件里的 mapped 条目归到规范键，与纯 v4 条目去重）。
    #[test]
    fn load_normalizes_v4_mapped_keys() {
        let dir = std::env::temp_dir().join(format!("hw-epc-norm-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);
        let peer = crate::token::PeerId::from([0x22u8; 32]);
        let hex = hex_encode(peer.as_bytes());
        let t0 = UNIX_EPOCH + Duration::from_secs(1_000_000);
        let raw = format!(
            "{{\"peer\":\"{hex}\",\"entries\":[\
             {{\"endpoint\":\"[::ffff:198.51.100.7]:41641\",\"source\":\"hint\",\"learnedAt\":{}}},\
             {{\"endpoint\":\"198.51.100.7:41641\",\"source\":\"probe\",\"learnedAt\":{}}}]}}",
            now_ms(t0),
            now_ms(t0) + 1
        );
        std::fs::write(dir.join(format!("{hex}.json")), raw).unwrap();
        let c = EndpointCache::open(&dir, peer);
        let list = c.entries(t0 + Duration::from_secs(10));
        assert_eq!(list.len(), 1, "mapped 与纯 v4 同址读盘后一条");
        assert_eq!(list[0].addr, "198.51.100.7:41641".parse::<SocketAddr>().unwrap());
        let _ = std::fs::remove_dir_all(&dir);
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

        // Merge：学习在前、静态去重接上（relay 位沿用）
        let merged = c.merge(
            &[Candidate { addr: addr(1), relay: false }, Candidate { addr: addr(2), relay: true }],
            t0,
        );
        assert_eq!(
            merged,
            vec![
                Candidate { addr: addr(1), relay: false },
                Candidate { addr: addr(2), relay: true },
            ]
        );
        // 学习地址撞上 static 的中继条目：沿用 Relay 标记
        let mut c2 = EndpointCache::new();
        c2.observe(addr(5), EndpointSource::Probe, t0);
        let m2 = c2.merge(&[Candidate { addr: addr(5), relay: true }], t0);
        assert_eq!(m2, vec![Candidate { addr: addr(5), relay: true }]);
    }

    /// F4：超 cap 插入 ⇒ 条数受限、未验证/最旧先出、**已验证保留**；全 verified + 表满
    /// ⇒ 新条目仍被接纳（淘汰尾部，不得因满而丢新 hint）。
    #[test]
    fn cap_evicts_tail_and_protects_verified() {
        let mut c = EndpointCache::new();
        c.max_entries = 3;
        let t0 = UNIX_EPOCH + Duration::from_secs(1_000_000);
        // 一条已验证（长连活口）——必须活到最后
        c.mark_verified(addr(1), EndpointSource::Probe, t0);
        // 灌入未验证条目：最旧 learned 先出
        for p in 2..=5u16 {
            c.observe(addr(p), EndpointSource::Hint, t0 + Duration::from_secs(p as u64));
        }
        let list = c.entries(t0 + Duration::from_secs(100));
        assert_eq!(list.len(), 3, "条数受限");
        assert!(list.iter().any(|e| e.addr == addr(1)), "已验证保留");
        assert!(!list.iter().any(|e| e.addr == addr(2)), "最旧未验证先出");
        assert!(list.iter().any(|e| e.addr == addr(5)), "最新未验证在");

        // 全 verified + 表满：新条目仍被接纳（淘汰尾部）
        let mut c2 = EndpointCache::new();
        c2.max_entries = 2;
        c2.mark_verified(addr(1), EndpointSource::Probe, t0);
        c2.mark_verified(addr(2), EndpointSource::Probe, t0 + Duration::from_secs(1));
        assert!(c2.observe(addr(3), EndpointSource::Hint, t0 + Duration::from_secs(2)), "新条目永不被拒");
        let list = c2.entries(t0 + Duration::from_secs(10));
        assert_eq!(list.len(), 2);
        assert!(list.iter().any(|e| e.addr == addr(3)), "新条目在表");
    }

    /// F4：`load`/`merge_disk` 超 cap 也受限（写盘路径不得绕过 cap）。
    /// **真断言**（代码门 M2）：直接看内部表 `entries.len()`——用 `entries()` 出口
    /// 断言会被出口 `truncate` 兜住，测不出 load/merge 自己是否 trim。
    #[test]
    fn load_and_merge_respect_cap() {
        let dir = std::env::temp_dir().join(format!("hw-epc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);
        let t0 = UNIX_EPOCH + Duration::from_secs(1_000_000);
        let peer = crate::token::PeerId::from([0x11u8; 32]);
        {
            let mut c = EndpointCache::open(&dir, peer);
            c.max_entries = 64;
            for p in 1..=6u16 {
                c.observe(addr(p), EndpointSource::Hint, t0 + Duration::from_secs(p as u64));
            }
            c.save(t0 + Duration::from_secs(10)).unwrap();
        }
        // load 真断言：默认 cap 下 open 已装载 6 条；调小 cap 后再 load 一次同盘文件，
        // 内部表必须被 trim 到 2（不是靠 entries() 出口截断）。
        let mut c = EndpointCache::open(&dir, peer);
        assert_eq!(c.entries.len(), 6, "默认 cap 下 load 全量装载");
        c.max_entries = 2;
        c.load();
        assert_eq!(c.entries.len(), 2, "load 自身必须 trim（真断言）");
        // merge_disk 真断言：满表内存 + 磁盘 6 条合并后仍 ≤ cap
        let mut c2 = EndpointCache::open(&dir, peer);
        c2.max_entries = 1;
        c2.merge_disk();
        assert_eq!(c2.entries.len(), 1, "merge_disk 自身必须 trim（真断言）");
        assert!(c2.entries(t0 + Duration::from_secs(31)).len() <= 1, "出口截断兜底");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Q-G F4（B 组新增）：端点缓存落盘文件 = **0600**（旧形态裸 `fs::write` 无权限
    /// 收紧；Go = `os.WriteFile(tmp, raw, 0o600)`）。
    #[test]
    fn save_artifact_is_0600() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = std::env::temp_dir().join(format!("hw-epc-perm-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);
        let t0 = UNIX_EPOCH + Duration::from_secs(1_000_000);
        let peer = crate::token::PeerId::from([0x22u8; 32]);
        let mut c = EndpointCache::open(&dir, peer);
        c.observe(addr(7), EndpointSource::Probe, t0);
        c.save(t0 + Duration::from_secs(1)).unwrap();
        let p = c.path().expect("有 dir");
        assert!(p.exists(), "缓存文件应在 {}", p.display());
        assert_eq!(
            std::fs::metadata(&p).unwrap().permissions().mode() & 0o777,
            0o600,
            "端点缓存产物必须 0600"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// F4：投喂配额——每窗口新增未验证地址 ≤ FEED_MAX_NEW（hint/probe 共用）；刷新
    /// 已有条目不计；新窗口重置；rearm 归零。
    #[test]
    fn feed_quota_shared_and_reset() {
        let mut c = EndpointCache::new();
        let t0 = UNIX_EPOCH + Duration::from_secs(1_000_000);
        let mut accepted = 0usize;
        for p in 0..(FEED_MAX_NEW as u16 + 10) {
            if c.observe(addr(1000 + p), EndpointSource::Hint, t0) {
                accepted += 1;
            }
        }
        assert_eq!(accepted, FEED_MAX_NEW, "hint 新增受配额");
        // probe 与 hint 共用同一计数器
        assert!(!c.observe(addr(9999), EndpointSource::Probe, t0), "probe 共享同一配额");
        // 刷新已有条目不被拒
        assert!(c.observe(addr(1000), EndpointSource::Hint, t0), "刷新已有条目不计配额");
        // 下一窗口重置（满额可用）
        let t1 = t0 + FEED_WINDOW + Duration::from_secs(1);
        let mut again = 0usize;
        for p in 0..(FEED_MAX_NEW as u16 + 5) {
            if c.observe(addr(2000 + p), EndpointSource::Hint, t1) {
                again += 1;
            }
        }
        assert_eq!(again, FEED_MAX_NEW, "新窗口重置配额");
        // rearm 归零（同窗口内）
        assert!(!c.observe(addr(3000), EndpointSource::Hint, t1), "同窗口已用尽");
        c.note_rearm();
        assert!(c.observe(addr(3000), EndpointSource::Hint, t1), "rearm 归零配额");
    }
}
