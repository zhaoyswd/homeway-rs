//! DNS 代答核心（R3；语义真源 `pkg/dns/{server,message,resolv}.go`——openspec
//! dns-host-resolver）。
//!
//! 出口把代答挂在隧道栈内（隧道 IP:53 UDP/TCP = 手机声明的 DNS；隧道 IP:<解析腿端口>
//! TCP = 客户端远程解析腿），由拦截层（`intercept` 的 DNS 面监听）喂进来；非隧道 IP 的
//! :53（应用写死公共 DNS）由拦截层走进程内入口（submit）。上游 = 主机系统解析
//! （resolv.conf 跟随，1s 节流 + last-good），按序尝试 + 末位公共 DNS 兜底（仅连接层
//! 失败触发，否定应答绝不兜底）；单查询总预算 2.5s / 单次尝试 800ms 上限（末次不吃）；
//! v6 类 qtype 回空应答；应答 TTL 钳制 ≤60s；UDP 超限截断置 TC。
//!
//! **线程模型（设计 §1.1 / 评审 H3 整改——DNS 专用线程落位）**：上游查询是阻塞 IO
//! （最长 2.5s/查询），不得跑在驱动线程——本模块自带固定 worker 池 + 在途上限
//! （MaxInFlight=256，超限丢弃计数——客户端按超时重试，不排长队放大延迟）；驱动线程
//! 只做「读查询 → 投队列」与「收应答 → 写回栈内 socket」（应答经 `DnsReply` 回投通道
//! 异步到达，tag = 提交方的路由键）。

use std::net::{SocketAddr, TcpStream, ToSocketAddrs, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use crate::Logf;

/// 全部 nameserver 连接层失败时的末位兜底（Go defaultFallback）。
const DEFAULT_FALLBACK: &str = "223.5.5.5";
/// 单查询总预算（主/兜共享）。
const DEFAULT_BUDGET: Duration = Duration::from_millis(2500);
/// 在途查询上限（超限丢弃计数；`dnsface` 回投容量与之对齐——见 F4d）。
pub(crate) const MAX_IN_FLIGHT: usize = 256;
/// 应答 TTL 钳制上限（秒）。
const MAX_TTL: u32 = 60;
/// 上游列表跟随检查节流。
const CHECK_INTERVAL: Duration = Duration::from_secs(1);
/// 单次上游尝试的预算上限（防静默黑洞上游吃干总预算——review F1；末次尝试不吃）。
const MAX_PER_TRY: Duration = Duration::from_millis(800);
/// UDP 缓冲上界（Go udpBufSize）。
const UDP_BUF: usize = 64 << 10;
/// DNS worker 池线程数缺省（F4c：2 → 64）。依据 = 到达率 × 上游时延（手机整页解析
/// 突发 ~50 查询/100ms、慢上游 800ms/次 ⇒ 数十并发才不排队）；资源上界 = 64 × 512KB
/// 栈 = 32MB 虚存；与 DNS 面既有容量同阶（`dnsface::MAX_TCP_CONNS = 64`）。
/// 修掉 F4a（per-attempt 绝对期限）后 worker 不再会被**永久**占死，worker 数只决定
/// 稳态吞吐。残余：全死上游下稳态 ≈25.6 qps（Go goroutine-per-query ~102 qps）⇒ §6。
const DEFAULT_WORKERS: usize = 64;

/// DNS UDP 应答在隧道内可承载的报文上限（proto.MaxDNSPayload53）。
pub const MAX_DNS_PAYLOAD_53: usize = 1232;

// ---------- 消息纯函数（message.go 平移；解析失败一律 None，绝不 panic） ----------

/// 过滤的 qtype 集合（隧道仅承载 IPv4 的协议事实）：
/// AAAA(28)/HTTPS(65)/SVCB(64) 引导 v6 直连绕过隧道；ANY(255) 应答内容不可控。
const TYPE_AAAA: u16 = 28;
const TYPE_HTTPS: u16 = 65;
const TYPE_SVCB: u16 = 64;
const TYPE_ANY: u16 = 255;
/// EDNS 伪记录：TTL 位是 extended-RCODE/flags，不可钳制。
const TYPE_OPT: u16 = 41;
/// TSIG：TTL 必须为 0，不可钳制。
const TYPE_TSIG: u16 = 250;

/// 该 qtype 是否被代答过滤（回空应答）。
pub fn filtered_qtype(qt: u16) -> bool {
    qt == TYPE_AAAA || qt == TYPE_HTTPS || qt == TYPE_SVCB || qt == TYPE_ANY
}

/// 提取查询 question 段的 qtype（解析失败 None）。
pub fn qtype(query: &[u8]) -> Option<u16> {
    if query.len() < 12 {
        return None;
    }
    let off = skip_name(query, 12)?;
    if off + 4 > query.len() {
        return None;
    }
    Some(u16::from_be_bytes([query[off], query[off + 1]]))
}

/// 为被过滤的查询构造 NOERROR 空应答：回显 question 段、置 QR/RD/RA、QDCOUNT=1。
/// 查询带 OPT 时应答不含 OPT（EDNS 客户端按无 EDNS 退化处理）。畸形查询返回 None。
pub fn empty_response(query: &[u8]) -> Option<Vec<u8>> {
    if query.len() < 12 {
        return None;
    }
    let qend = skip_name(query, 12)?;
    if qend + 4 > query.len() {
        return None;
    }
    let mut resp = vec![0u8; 12];
    resp[..2].copy_from_slice(&query[..2]); // ID 原样
    resp[2] = 0x80 | (query[2] & 0x01); // QR=1，保留 RD
    resp[3] = 0x80; // RA=1，RCODE=0
    resp[4..6].copy_from_slice(&1u16.to_be_bytes()); // QDCOUNT=1
    resp.truncate(12);
    resp.extend_from_slice(&query[12..qend + 4]); // question 原样回显
    Some(resp)
}

/// 把应答里普通 RR 的 TTL 压到 ≤max（OPT/TSIG 跳过；只动 RR 头，不碰 RDATA）。
/// 解析失败原样不动。
pub fn clamp_ttl(resp: &mut [u8], max: u32) {
    if resp.len() < 12 {
        return;
    }
    let Some(mut off) = skip_name(resp, 12) else { return };
    off += 4; // question 的 QTYPE+QCLASS
    for sec in 0..3 {
        // 三段 RR 的计数在 header 的 AN/NS/AR（[6:8]/[8:10]/[10:12]）——QD 已随 question 跳过
        let count = u16::from_be_bytes([resp[6 + 2 * sec], resp[7 + 2 * sec]]) as usize;
        for _ in 0..count {
            let Some(p) = skip_name(resp, off) else { return };
            if p + 10 > resp.len() {
                return;
            }
            let rdlen = u16::from_be_bytes([resp[p + 8], resp[p + 9]]) as usize;
            if p + 10 + rdlen > resp.len() {
                return;
            }
            let typ = u16::from_be_bytes([resp[p], resp[p + 1]]);
            if typ != TYPE_OPT && typ != TYPE_TSIG {
                let ttl = u32::from_be_bytes(resp[p + 4..p + 8].try_into().expect("定长"));
                if ttl > max {
                    resp[p + 4..p + 8].copy_from_slice(&max.to_be_bytes());
                }
            }
            off = p + 10 + rdlen;
        }
    }
}

/// 统计应答答案段的 AAAA 记录数（过滤面收缩的观测：只计数不剥离——已知限制）。
pub fn count_aaaa(resp: &[u8]) -> usize {
    if resp.len() < 12 {
        return 0;
    }
    let Some(mut off) = skip_name(resp, 12) else { return 0 };
    off += 4;
    let mut n = 0;
    let count = u16::from_be_bytes([resp[6], resp[7]]) as usize; // ANCOUNT
    for _ in 0..count {
        let Some(p) = skip_name(resp, off) else { return n };
        if p + 10 > resp.len() {
            return n;
        }
        let rdlen = u16::from_be_bytes([resp[p + 8], resp[p + 9]]) as usize;
        if p + 10 + rdlen > resp.len() {
            return n;
        }
        if u16::from_be_bytes([resp[p], resp[p + 1]]) == TYPE_AAAA {
            n += 1;
        }
        off = p + 10 + rdlen;
    }
    n
}

/// 把应答截到 ≤max 字节：保留 header + question + 尽可能多条完整 answer（不留半条），
/// 按实存记录修三计数、置 TC。authority/additional（含 OPT）整体丢弃——客户端按 TC
/// 语义走 TCP 取全量。已 ≤max 或无法解析时原样返回。
pub fn truncate(resp: &[u8], max: usize) -> Vec<u8> {
    if resp.len() <= max || resp.len() < 12 {
        return resp.to_vec();
    }
    let Some(qend) = skip_name(resp, 12) else { return resp.to_vec() };
    if qend + 4 > resp.len() {
        return resp.to_vec();
    }
    let mut out = resp[..12].to_vec();
    out[2] |= 0x02; // TC=1
    out.extend_from_slice(&resp[12..qend + 4]);
    out[4..6].copy_from_slice(&1u16.to_be_bytes()); // QDCOUNT
    for i in 0..3 {
        out[6 + 2 * i..8 + 2 * i].copy_from_slice(&0u16.to_be_bytes());
    }
    let mut kept = 0usize;
    let mut off = qend + 4;
    let count = u16::from_be_bytes([resp[6], resp[7]]) as usize;
    for _ in 0..count {
        let Some(p) = skip_name(resp, off) else { break };
        if p + 10 > resp.len() {
            break;
        }
        let rdlen = u16::from_be_bytes([resp[p + 8], resp[p + 9]]) as usize;
        if p + 10 + rdlen > resp.len() {
            break;
        }
        let end = p + 10 + rdlen;
        if out.len() + (end - off) > max {
            break;
        }
        out.extend_from_slice(&resp[off..end]);
        kept += 1;
        off = end;
    }
    if kept > 0 {
        out[6..8].copy_from_slice(&(kept as u16).to_be_bytes());
    }
    out
}

/// 跳过（不解析）一个 DNS 名字：label 序列或压缩指针（就地结束，不跟随——只 skip）。
fn skip_name(msg: &[u8], mut off: usize) -> Option<usize> {
    loop {
        if off >= msg.len() {
            return None;
        }
        let l = msg[off] as usize;
        if l == 0 {
            return Some(off + 1);
        }
        if l & 0xC0 == 0xC0 {
            if off + 2 > msg.len() {
                return None;
            }
            return Some(off + 2);
        }
        if l & 0xC0 != 0 {
            return None;
        }
        off += 1 + l;
        if off > msg.len() {
            return None;
        }
    }
}

// ---------- 上游跟随（resolv.go 平移） ----------

/// /etc/resolv.conf nameserver 列表的跟随器：mtime 检查 1s 节流；变了重解析替换；
/// 读取/解析失败保留 last-good；空表每节流窗口强制重读（启动竞态里文件可能原地出现）。
pub struct Upstreams {
    path: String,
    state: Mutex<UpstreamsState>,
}

struct UpstreamsState {
    last_check: Instant,
    mtime: Option<SystemTime>,
    /// 上游列表**快照**（Q-I F5：`Arc` 换新不原地改——`list()` 的 clone 退化为引用
    /// 计数，且在途查询恒读到一致快照，不会「读一半被换」）。
    list: Arc<Vec<String>>,
}

impl Upstreams {
    pub fn new(path: &str) -> Self {
        let mut st = UpstreamsState {
            last_check: Instant::now(),
            mtime: None,
            list: Arc::new(Vec::new()),
        };
        if let Ok(md) = std::fs::metadata(path) {
            if let Some(list) = parse_resolv_nameservers(path) {
                if !list.is_empty() {
                    st.mtime = md.modified().ok();
                    st.list = Arc::new(list);
                }
            }
        }
        Self { path: path.to_owned(), state: Mutex::new(st) }
    }

    /// 当前 nameserver 列表**快照**（host 形式——resolv.conf 的 nameserver 都是 IP）。
    /// 返回 `Arc`：clone = 引用计数（F5 起，此前是 `Vec<String>` 深拷贝/查询）。
    pub fn list(&self) -> Arc<Vec<String>> {
        let mut st = self.state.lock().expect("上游表锁中毒");
        if st.last_check.elapsed() < CHECK_INTERVAL {
            return Arc::clone(&st.list);
        }
        st.last_check = Instant::now();
        if !st.list.is_empty() {
            if let Ok(md) = std::fs::metadata(&self.path) {
                if md.modified().ok() == st.mtime {
                    return Arc::clone(&st.list); // 未变：保留 last-good
                }
            }
        }
        if let Some(list) = parse_resolv_nameservers(&self.path) {
            if !list.is_empty() {
                st.mtime = std::fs::metadata(&self.path).and_then(|m| m.modified()).ok();
                st.list = Arc::new(list); // 换新 Arc（在途查询持旧快照）
            }
        }
        Arc::clone(&st.list)
    }

    /// 上游列表的逗连摘要（E4 判据行用）。**行文/语义不变**（F5 只换持有形态）。
    pub fn text(&self) -> String {
        self.list().join(", ")
    }
}

/// 逐行取 nameserver 项（忽略注释/其它指令）；空列表 = 文件无有效上游。
fn parse_resolv_nameservers(path: &str) -> Option<Vec<String>> {
    let body = std::fs::read_to_string(path).ok()?;
    let mut out = Vec::new();
    for line in body.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        let mut fields = line.split_whitespace();
        if let (Some(k), Some(v)) = (fields.next(), fields.next()) {
            if k == "nameserver" && !v.is_empty() {
                out.push(v.to_owned());
            }
        }
    }
    Some(out)
}

// ---------- 计数器族（StatsLine 的数据源；跨线程原子——worker 池并发应答） ----------

#[derive(Default)]
pub struct DnsStats {
    q: AtomicU64,
    qtcp: AtomicU64,
    resp: AtomicU64,
    filtered: AtomicU64,
    trunc: AtomicU64,
    fallback: AtomicU64,
    fail: AtomicU64,
    dropped: AtomicU64,
    malformed: AtomicU64,
    aaaa_mixed: AtomicU64,
}

impl DnsStats {
    /// E22 判据行（debug 级周期输出）。
    pub fn line(&self) -> String {
        format!(
            "dns: q={} qtcp={} resp={} filter={} trunc={} fallback={} fail={} drop={} malformed={} aaaa-mixed={}",
            self.q.load(Ordering::Relaxed),
            self.qtcp.load(Ordering::Relaxed),
            self.resp.load(Ordering::Relaxed),
            self.filtered.load(Ordering::Relaxed),
            self.trunc.load(Ordering::Relaxed),
            self.fallback.load(Ordering::Relaxed),
            self.fail.load(Ordering::Relaxed),
            self.dropped.load(Ordering::Relaxed),
            self.malformed.load(Ordering::Relaxed),
            self.aaaa_mixed.load(Ordering::Relaxed),
        )
    }
}

/// 应答回投事件（驱动线程 drain 后路由回栈内 socket；tag 由提交方定义）。
pub struct DnsReply {
    pub tag: u64,
    pub resp: Option<Vec<u8>>,
}

/// DNS 代答配置（零值走默认；测试用 resolv_path + fallback_dns 指向本地 fake 上游）。
#[derive(Clone)]
pub struct DnsConfig {
    pub resolv_path: String,
    pub fallback_dns: String,
    pub budget: Duration,
    pub max_in_flight: usize,
    /// worker 池线程数（0 = `DEFAULT_WORKERS`；测试注入小值以钉并发语义）。
    pub workers: usize,
}

impl Default for DnsConfig {
    fn default() -> Self {
        Self {
            resolv_path: "/etc/resolv.conf".to_owned(),
            fallback_dns: DEFAULT_FALLBACK.to_owned(),
            budget: DEFAULT_BUDGET,
            max_in_flight: MAX_IN_FLIGHT,
            workers: DEFAULT_WORKERS,
        }
    }
}

impl DnsConfig {
    fn filled(self) -> Self {
        Self {
            budget: if self.budget.is_zero() { DEFAULT_BUDGET } else { self.budget },
            fallback_dns: if self.fallback_dns.is_empty() {
                DEFAULT_FALLBACK.to_owned()
            } else {
                self.fallback_dns
            },
            max_in_flight: if self.max_in_flight == 0 { MAX_IN_FLIGHT } else { self.max_in_flight },
            workers: if self.workers == 0 { DEFAULT_WORKERS } else { self.workers },
            ..self
        }
    }
}

/// 一条待处理的查询（tag = 提交方的路由键；reply_to = 应答投递面——异步提交用共享
/// 事件通道，同步面用一次性通道）。
struct DnsJob {
    tag: u64,
    query: Vec<u8>,
    is_tcp: bool,
    reply_to: mpsc::Sender<DnsReply>,
}

/// 提交回执（F2）：`Dropped` = 在途超限或 `try_send` 失败，**未进 worker 队列、
/// 不会有应答回投**——调用方须回收已登记的 route tag（否则 `pending` 项永不回收）。
/// `#[must_use]` 借 `clippy -D warnings` 兜住漏检。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[must_use]
pub enum SubmitOutcome {
    Accepted,
    Dropped,
}

/// DNS 代答句柄（驱动线程/拦截层持有；submit 非阻塞，应答经回投通道异步到达）。
pub struct DnsProxy {
    core: Arc<ResponderCore>,
    job_tx: mpsc::SyncSender<DnsJob>,
    reply_tx: mpsc::Sender<DnsReply>,
    in_flight: Arc<AtomicUsize>,
}

impl DnsProxy {
    /// 起代答核心 + DNS 专用 worker 池（H3 整改：阻塞面全长 2.5s/查询不占驱动线程）。
    /// 返回（句柄, 应答回投通道——驱动线程在拍内 drain）。
    pub fn spawn(cfg: DnsConfig, logf: Logf, dlogf: Logf) -> (Arc<Self>, mpsc::Receiver<DnsReply>) {
        let cfg = cfg.filled();
        let (job_tx, job_rx) = mpsc::sync_channel::<DnsJob>(cfg.max_in_flight);
        let (reply_tx, reply_rx) = mpsc::channel::<DnsReply>();
        let core = Arc::new(ResponderCore {
            ups: Upstreams::new(&cfg.resolv_path),
            stats: Arc::new(DnsStats::default()),
            fb_once: AtomicBool::new(false),
            budget: cfg.budget,
            fallback_dns: cfg.fallback_dns.clone(),
            max_in_flight: cfg.max_in_flight,
            logf,
            dlogf,
        });
        let in_flight = Arc::new(AtomicUsize::new(0));
        let job_rx = Arc::new(Mutex::new(job_rx));
        let mut started = 0usize;
        for _ in 0..cfg.workers {
            let c2 = Arc::clone(&core);
            let job_rx2 = Arc::clone(&job_rx);
            let in_flight2 = Arc::clone(&in_flight);
            match std::thread::Builder::new()
                .name("homeway-dns".into())
                .stack_size(512 * 1024)
                .spawn(move || {
                    // Q-I F5：worker 自持读缓冲（懒分配——首次 `exchange` 用到才
                    // `vec![0u8; UDP_BUF]`；常态空闲 worker 零占用）。
                    let mut scratch: Vec<u8> = Vec::new();
                    loop {
                        let job = { job_rx2.lock().expect("作业队列锁中毒").recv() };
                        let Ok(job) = job else { return };
                        let resp = c2.respond(&job.query, job.is_tcp, &mut scratch);
                        let _ = job.reply_to.send(DnsReply { tag: job.tag, resp });
                        in_flight2.fetch_sub(1, Ordering::Relaxed);
                    }
                }) {
                Ok(_) => started += 1,
                Err(e) => {
                    // worker 数从 2→64 后 spawn 失败概率上升（EMFILE 等）：记行并按已起数继续
                    (core.logf)(&format!("⚠️ dns: worker 线程起不来（{e}）——已起 {started} 条"));
                    break;
                }
            }
        }
        if started == 0 {
            (core.logf)("⚠️ dns: 没有可用 worker——代答不可用（上游查询不会被处理）");
        }
        (
            Arc::new(Self { core, job_tx, reply_tx, in_flight }),
            reply_rx,
        )
    }

    /// 当前上游列表的逗连摘要（E4 判据行用）。
    pub fn upstreams_text(&self) -> String {
        self.core.ups.text()
    }

    /// E22 计数行。
    pub fn stats_line(&self) -> String {
        self.core.stats.line()
    }

    /// 提交一条 UDP 查询（非阻塞；在途超限按 Go drop 计数丢弃）。
    pub fn submit_udp(&self, tag: u64, query: Vec<u8>) -> SubmitOutcome {
        self.submit(tag, query, false)
    }

    /// 拦截层进程内腿（非隧道 IP :53 的兜底）：**不计 q**（Go `Answer()` 直调 respond
    /// 同口径——q 只在隧道栈 UDP listener 面计数）。
    pub fn submit_leg(&self, tag: u64, query: Vec<u8>) -> SubmitOutcome {
        self.submit_leg_impl(tag, query)
    }

    /// 提交一条 TCP 查询（qtcp 单列——「出口 5300 收到 TCP 查询」的判据面）。
    pub fn submit_tcp(&self, tag: u64, query: Vec<u8>) -> SubmitOutcome {
        self.submit(tag, query, true)
    }

    fn submit_leg_impl(&self, tag: u64, query: Vec<u8>) -> SubmitOutcome {
        if self.in_flight.fetch_add(1, Ordering::Relaxed) + 1 > self.core.max_in_flight {
            self.in_flight.fetch_sub(1, Ordering::Relaxed);
            self.core.stats.dropped.fetch_add(1, Ordering::Relaxed);
            return SubmitOutcome::Dropped;
        }
        if self
            .job_tx
            .try_send(DnsJob { tag, query, is_tcp: false, reply_to: self.reply_tx.clone() })
            .is_err()
        {
            self.in_flight.fetch_sub(1, Ordering::Relaxed);
            self.core.stats.dropped.fetch_add(1, Ordering::Relaxed);
            return SubmitOutcome::Dropped;
        }
        SubmitOutcome::Accepted
    }

    fn submit(&self, tag: u64, query: Vec<u8>, is_tcp: bool) -> SubmitOutcome {
        if is_tcp {
            self.core.stats.qtcp.fetch_add(1, Ordering::Relaxed);
        } else {
            self.core.stats.q.fetch_add(1, Ordering::Relaxed);
        }
        if self.in_flight.fetch_add(1, Ordering::Relaxed) + 1 > self.core.max_in_flight {
            self.in_flight.fetch_sub(1, Ordering::Relaxed);
            self.core.stats.dropped.fetch_add(1, Ordering::Relaxed);
            return SubmitOutcome::Dropped; // 超限丢弃：客户端按超时重试，不排长队放大延迟
        }
        if self
            .job_tx
            .try_send(DnsJob { tag, query, is_tcp, reply_to: self.reply_tx.clone() })
            .is_err()
        {
            self.in_flight.fetch_sub(1, Ordering::Relaxed);
            self.core.stats.dropped.fetch_add(1, Ordering::Relaxed);
            return SubmitOutcome::Dropped;
        }
        SubmitOutcome::Accepted
    }

    /// 同步应答一条查询（自检/测试面；经同一 worker 管线——与异步路径同一条应答逻辑）。
    pub fn answer_sync(&self, query: &[u8]) -> Option<Vec<u8>> {
        let (tx, rx) = mpsc::channel();
        if self
            .job_tx
            .send(DnsJob { tag: 0, query: query.to_vec(), is_tcp: false, reply_to: tx })
            .is_err()
        {
            return None;
        }
        rx.recv_timeout(self.core.budget + Duration::from_secs(1)).ok().and_then(|r| r.resp)
    }

    /// 启动自验证：真跑一遍「查询 → 上游」链路（SERVFAIL = 上游全挂且兜底也失败；
    /// 失败不禁用代答——上游会跟随主机恢复，翻转会让 DNS 在两种模式间抖动）。
    pub fn self_check(&self) -> Result<(), String> {
        match self.answer_sync(&self_check_query()) {
            None => Err("自验证无应答（查询处理失败）".to_owned()),
            Some(resp) => {
                if resp.len() >= 4 && resp[3] & 0x0F == 2 {
                    Err("自验证拿到 SERVFAIL（上游全挂且兜底失败）".to_owned())
                } else {
                    Ok(())
                }
            }
        }
    }
}

/// 应答管线本体（worker 池共享；respond 自身无共享可变状态）。
struct ResponderCore {
    ups: Upstreams,
    stats: Arc<DnsStats>,
    /// 兜底首次触发的摘要位（每进程一次）。
    fb_once: AtomicBool,
    budget: Duration,
    fallback_dns: String,
    max_in_flight: usize,
    logf: Logf,
    dlogf: Logf,
}

impl ResponderCore {
    /// 单查询处理。None = 不回包（畸形）。
    /// is_tcp：TCP 客户端正是为「取全量」而来（UDP 截断后的重试），不受 1232 的
    /// TUN 单包约束（无条件截断 = 永远拿不到全量的死循环）。
    /// `scratch`：worker 自持上游读缓冲（F5：每查询每腿省一次 64KB alloc+零初始化；
    /// 懒分配在 `exchange` 内，交付仍 `to_vec()` 保 owned 语义）。
    fn respond(&self, query: &[u8], is_tcp: bool, scratch: &mut Vec<u8>) -> Option<Vec<u8>> {
        let Some(qt) = qtype(query) else {
            self.stats.malformed.fetch_add(1, Ordering::Relaxed);
            return None;
        };
        if filtered_qtype(qt) {
            self.stats.filtered.fetch_add(1, Ordering::Relaxed);
            return empty_response(query);
        }
        let mut resp = match self.forward(query, scratch) {
            Some(r) => r,
            None => {
                self.stats.fail.fetch_add(1, Ordering::Relaxed);
                let mut out = empty_response(query)?;
                out[3] |= 0x02; // RCODE=2（SERVFAIL）——客户端按 DNS 失败重试
                return Some(out);
            }
        };
        let n = count_aaaa(&resp);
        if n > 0 {
            self.stats.aaaa_mixed.fetch_add(n as u64, Ordering::Relaxed); // 只计数不剥离（已知限制）
        }
        clamp_ttl(&mut resp, MAX_TTL);
        if !is_tcp && resp.len() > MAX_DNS_PAYLOAD_53 {
            resp = truncate(&resp, MAX_DNS_PAYLOAD_53);
            self.stats.trunc.fetch_add(1, Ordering::Relaxed);
        }
        self.stats.resp.fetch_add(1, Ordering::Relaxed);
        Some(resp)
    }

    /// 按序尝试 nameserver 列表 + 末位兜底，共享单查询总预算。
    /// 每次尝试的预算 = min(剩余, 800ms)；末次尝试不吃上限（「慢但活着」的唯一上游
    /// 能用满剩余预算，而不是提前 SERVFAIL 白扔）。
    fn forward(&self, query: &[u8], scratch: &mut Vec<u8>) -> Option<Vec<u8>> {
        let deadline = Instant::now() + self.budget;
        let ups = self.ups.list();
        let attempts = ups.len() + 1; // nameservers + 兜底
        let mut try_one = |addr: &str, i: usize| -> Option<Vec<u8>> {
            let remain = deadline.saturating_duration_since(Instant::now());
            if remain.is_zero() {
                return None;
            }
            let mut per_try = remain / ((attempts - i) as u32);
            if i < attempts - 1 && per_try > MAX_PER_TRY {
                per_try = MAX_PER_TRY;
            }
            exchange(addr, query, per_try, deadline, scratch)
        };
        for (i, up) in ups.iter().enumerate() {
            if let Some(resp) = try_one(&dial_addr(up), i) {
                return Some(resp);
            }
        }
        if let Some(resp) = try_one(&dial_addr(&self.fallback_dns), ups.len()) {
            self.stats.fallback.fetch_add(1, Ordering::Relaxed);
            (self.dlogf)(&format!("dns: 兜底触发（nameserver 全部连接层失败）→ {}", self.fallback_dns));
            if self
                .fb_once
                .compare_exchange(false, true, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
            {
                (self.logf)(&format!(
                    "⚠️ dns 上游全部不可达，已触发公共 DNS 兜底（{}）——fake-ip 主机上该应答为真实 IP，域名规则对这些流量失效",
                    self.fallback_dns
                ));
            }
            return Some(resp);
        }
        None
    }
}

/// 向单个上游转发一条查询（UDP，TC 切 TCP）。应答回填原始 ID；只接受来自该 socket、
/// ID 与 question 段匹配的应答（防投毒/串答）。连接层失败 = None；否定应答原样透传。
///
/// **本腿期限是绝对期限**（F4a，对齐 Go `conn.SetDeadline(now+budget)`，`server.go:415`）：
/// `budget` = 调用方给的**本腿上试预算**（`per_try`，末次腿不吃上限），循环里每次
/// `recv` 前按剩余重设读超时——`SO_RCVTIMEO` 是 per-syscall，灌不匹配包的坏上游
/// 此前可让该 worker **永不返回**（配合 worker 数少 = 全通道瘫痪）。
/// 全局 `deadline` **仍只喂 TC→TCP 分支**（不改 `MAX_PER_TRY` 与上游回退序语义）。
/// `scratch`：调用方（worker）自持的读缓冲（F5 复用；懒分配 = 首次用到才 64KB 零
/// 初始化。`UDP_BUF`(64KiB) ≥ UDP 最大载荷 ⇒ 无短读风险）。
fn exchange(
    addr: &str,
    query: &[u8],
    budget: Duration,
    deadline: Instant,
    scratch: &mut Vec<u8>,
) -> Option<Vec<u8>> {
    if budget.is_zero() {
        return None;
    }
    let attempt_deadline = Instant::now() + budget; // 本腿绝对期限（per-attempt）
    let orig_id = u16::from_be_bytes([query[0], query[1]]);
    let mut fwd = [0u8; 2];
    getrandom::getrandom(&mut fwd).ok()?;
    let new_id = u16::from_be_bytes(fwd);
    let mut q = query.to_vec();
    q[0..2].copy_from_slice(&new_id.to_be_bytes()); // 事务 ID 重写为密码学随机，防可预测 ID 投毒

    let raddr: SocketAddr = resolve_udp_addr(addr)?;
    let conn = UdpSocket::bind(if raddr.is_ipv4() { "0.0.0.0:0" } else { "[::]:0" }).ok()?;
    conn.connect(raddr).ok()?; // 每查询新 socket = 随机源端口
    conn.set_read_timeout(Some(budget)).ok()?; // 初值（循环顶按本腿剩余重设）
    conn.send(&q).ok()?;
    let qend = skip_name(&q, 12)?;
    // F5：复用调用方缓冲（懒分配一次；`recv` 只写 `0..n`，下游一律 `&buf[..n]`）。
    if scratch.len() < UDP_BUF {
        scratch.resize(UDP_BUF, 0);
    }
    loop {
        // 每轮按本腿剩余重设（绝对期限收敛——continue 不续命）
        let remain = attempt_deadline.saturating_duration_since(Instant::now());
        if remain.is_zero() {
            return None;
        }
        conn.set_read_timeout(Some(remain)).ok()?;
        let n = conn.recv(&mut scratch[..]).ok()?;
        if n < 12 {
            continue;
        }
        let resp = &scratch[..n];
        if u16::from_be_bytes([resp[0], resp[1]]) != new_id {
            continue; // 事务 ID 不匹配：丢弃（伪造/迟到应答）
        }
        let Some(rend) = skip_name(resp, 12) else { continue };
        if rend + 4 > resp.len() || resp[12..rend + 4] != q[12..qend + 4] {
            continue; // question 段不一致：丢弃
        }
        let mut out = resp.to_vec();
        out[0..2].copy_from_slice(&orig_id.to_be_bytes());
        if out[2] & 0x02 != 0 {
            // 上游置 TC → 切 TCP 取全量（预算 = 查询剩余时间，非本腿新起算）
            let remain = deadline.saturating_duration_since(Instant::now());
            if !remain.is_zero() {
                if let Some(tcp_resp) = exchange_tcp(addr, query, &out, remain) {
                    return Some(tcp_resp);
                }
            }
            // TCP 腿失败：回 UDP 截断应答（TC 保持）——UDP 腿已证上游活着，按连接层
            // 失败试下一个反而丢掉已拿到的（截断）应答。
        }
        return Some(out);
    }
}

/// 经 TCP 向上游重查：发**原始查询报文**（重新随机事务 ID），读单条应答并校验
/// (ID, question)。失败回退 = 入参的 UDP 截断应答（TC 保持，客户端可自行重试）。
/// 拨号带期限（F4b：`to_socket_addrs` + 逐地址 `connect_timeout`，对齐 Go
/// `net.Dialer{Timeout: budget}`，`server.go:460`——修前是 OS 默认，macOS 可达 ~75s）。
fn exchange_tcp(addr: &str, query: &[u8], udp_resp: &[u8], budget: Duration) -> Option<Vec<u8>> {
    let deadline = Instant::now() + budget;
    let conn = connect_within(addr, deadline)?;
    // 拨号 + 读写**共用同一绝对期限**（tarpit 上游不能把单查询推到 ~2×perTry）：
    // `SO_RCVTIMEO` 是 per-syscall，只设一次会被「每 <budget 送 1 字节」的滴流无限续命
    // （`read_exact` 内部循环同理）⇒ 走逐 syscall 收敛的 `DeadlineIo`。
    let mut io = DeadlineIo { sock: &conn, deadline };
    let orig_id = u16::from_be_bytes([query[0], query[1]]);
    let mut q = query.to_vec();
    let mut id = [0u8; 2];
    getrandom::getrandom(&mut id).ok()?;
    let new_id = u16::from_be_bytes(id);
    q[0..2].copy_from_slice(&new_id.to_be_bytes());
    write_tcp_message(&mut io, &q).ok()?;
    let resp = read_tcp_message(&mut io).ok()?;
    if resp.len() < 12 {
        return Some(udp_resp.to_vec());
    }
    if u16::from_be_bytes([resp[0], resp[1]]) != new_id {
        return Some(udp_resp.to_vec()); // ID 不匹配：不认（垃圾回包/错位应答）
    }
    let rend = skip_name(&resp, 12)?;
    let qend = skip_name(&q, 12)?;
    if rend + 4 > resp.len() || qend + 4 > q.len() || resp[12..rend + 4] != q[12..qend + 4] {
        return Some(udp_resp.to_vec()); // question 段不一致
    }
    let mut out = resp;
    out[0..2].copy_from_slice(&orig_id.to_be_bytes());
    Some(out)
}

/// RFC 1035 TCP 分帧：2 字节长度前缀。
fn read_tcp_message(r: &mut dyn std::io::Read) -> std::io::Result<Vec<u8>> {
    let mut lb = [0u8; 2];
    r.read_exact(&mut lb)?;
    let n = u16::from_be_bytes(lb) as usize;
    if n == 0 {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "dns: 空 TCP 消息"));
    }
    let mut msg = vec![0u8; n];
    r.read_exact(&mut msg)?;
    Ok(msg)
}

fn write_tcp_message(w: &mut impl std::io::Write, msg: &[u8]) -> std::io::Result<()> {
    if msg.len() > 0xFFFF {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "dns: TCP 消息超长"));
    }
    w.write_all(&(msg.len() as u16).to_be_bytes())?;
    w.write_all(msg)
}

/// nameserver 形态转可拨地址：纯 IPv4 拼 :53；裸 IPv6 加方括号；已带端口原样
/// （测试注入用，标准文件不会出现）。
fn dial_addr(up: &str) -> String {
    if up.starts_with('[') && up.contains("]:") {
        return up.to_owned(); // [v6]:port
    }
    if up.matches(':').count() == 1 {
        if let Some((_, p)) = up.rsplit_once(':') {
            if p.parse::<u16>().is_ok() {
                return up.to_owned(); // v4:port
            }
        }
    }
    if up.contains(':') {
        return format!("[{up}]:53"); // 裸 IPv6
    }
    format!("{up}:53")
}

fn resolve_udp_addr(addr: &str) -> Option<SocketAddr> {
    addr.to_socket_addrs().ok()?.next()
}

/// 期限感知读（F4b 收口）：每次 syscall 前按**绝对期限**剩余重设 `SO_RCVTIMEO`——零即
/// 立即报超时（`read_exact` 的内部循环同样受界；逐字节滴流无法靠「每次成功读续命」）。
struct DeadlineIo<'a> {
    sock: &'a TcpStream,
    deadline: Instant,
}

impl std::io::Read for DeadlineIo<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let remain = self.deadline.saturating_duration_since(Instant::now());
        if remain.is_zero() {
            return Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "dns: TCP 腿预算耗尽"));
        }
        self.sock.set_read_timeout(Some(remain))?;
        self.sock.read(buf)
    }
}

impl std::io::Write for DeadlineIo<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let remain = self.deadline.saturating_duration_since(Instant::now());
        if remain.is_zero() {
            return Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "dns: TCP 腿预算耗尽"));
        }
        self.sock.set_write_timeout(Some(remain))?;
        self.sock.write(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.sock.flush()
    }
}

/// 带期限拨号（F4b/F7 共用形态）：`to_socket_addrs` + 逐地址 `connect_timeout(剩余)`；
/// 剩余为零即放弃。
fn connect_within(addr: &str, deadline: Instant) -> Option<TcpStream> {
    let addrs = addr.to_socket_addrs().ok()?;
    for a in addrs {
        let remain = deadline.saturating_duration_since(Instant::now());
        if remain.is_zero() {
            return None;
        }
        if let Ok(c) = TcpStream::connect_timeout(&a, remain) {
            return Some(c);
        }
    }
    None
}

/// 构造自验证查询（随机 ID + 一次性域名，避免命中任何缓存）。
/// 头部**完整 12 字节**（ID/flags/QD + 置零的 AN/NS/AR——Go selfCheckQuery 的
/// `make([]byte, 12)` 同形态）：R6.5 E2E 抓出过 5 字节残头的直译 bug——名字错位到
/// offset 12 之后，自检恒 malformed=1 且「自验证无应答」误报（真代答不受影响）。
fn self_check_query() -> Vec<u8> {
    let mut id = [0u8; 2];
    getrandom::getrandom(&mut id).ok();
    let mut q = Vec::with_capacity(47);
    q.extend_from_slice(&id);
    q.extend_from_slice(&[0x01, 0x00]); // flags：RD
    q.extend_from_slice(&1u16.to_be_bytes()); // QDCOUNT
    q.extend_from_slice(&0u16.to_be_bytes()); // ANCOUNT
    q.extend_from_slice(&0u16.to_be_bytes()); // NSCOUNT
    q.extend_from_slice(&0u16.to_be_bytes()); // ARCOUNT
    for label in ["selfcheck", "dns", "homeway", "invalid"] {
        q.push(label.len() as u8);
        q.extend_from_slice(label.as_bytes());
    }
    q.extend_from_slice(&[0, 0x00, 0x01, 0x00, 0x01]); // 根 + A/IN
    q
}

#[cfg(test)]
mod tests {
    use super::*;

    fn noop() -> (Logf, Logf) {
        (Arc::new(|_| {}), Arc::new(|_| {}))
    }

    fn query_for(name: &str, id: u16, qtype_be: [u8; 2]) -> Vec<u8> {
        let mut q = Vec::new();
        q.extend_from_slice(&id.to_be_bytes());
        q.extend_from_slice(&[0x01, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0]);
        for label in name.split('.') {
            q.push(label.len() as u8);
            q.extend_from_slice(label.as_bytes());
        }
        q.push(0);
        q.extend_from_slice(&qtype_be); // QTYPE
        q.extend_from_slice(&[0x00, 0x01]); // IN
        q
    }

    fn a_query(name: &str, id: u16) -> Vec<u8> {
        query_for(name, id, [0x00, 0x01])
    }

    fn aaaa_query(name: &str) -> Vec<u8> {
        query_for(name, 7, 28u16.to_be_bytes())
    }

    /// 构造一条带 TTL 的 A 应答（answer 段用压缩指针回指 question）。
    fn a_response(name: &str, id: u16, ttl: u32, addrs: &[u32]) -> Vec<u8> {
        let mut r = Vec::new();
        r.extend_from_slice(&id.to_be_bytes());
        r.extend_from_slice(&[0x80 | 0x01, 0x80, 0x00, 0x01]); // QR/RD/RA + QD=1
        r.extend_from_slice(&(addrs.len() as u16).to_be_bytes()); // AN
        r.extend_from_slice(&0u16.to_be_bytes()); // NS
        r.extend_from_slice(&0u16.to_be_bytes()); // AR
        // question（偏移 12 起——压缩指针 0xC00C 指这里）
        let q = a_query(name, id);
        r.extend_from_slice(&q[12..]);
        for a in addrs {
            r.extend_from_slice(&[0xC0, 0x0C]);
            r.extend_from_slice(&1u16.to_be_bytes()); // A
            r.extend_from_slice(&1u16.to_be_bytes()); // IN
            r.extend_from_slice(&ttl.to_be_bytes());
            r.extend_from_slice(&4u16.to_be_bytes()); // rdlen
            r.extend_from_slice(&a.to_be_bytes());
        }
        r
    }

    /// qtype/过滤/空应答（message.go 对拍）。
    #[test]
    fn qtype_filter_and_empty_response() {
        let q = a_query("example.com", 0x1234);
        assert_eq!(qtype(&q), Some(1));
        assert!(!filtered_qtype(1));
        for qt in [28u16, 64, 65, 255] {
            assert!(filtered_qtype(qt));
        }
        assert_eq!(qtype(&aaaa_query("example.com")), Some(28));
        let resp = empty_response(&q).unwrap();
        assert_eq!(&resp[..2], &0x1234u16.to_be_bytes());
        assert_eq!(resp[2], 0x81, "QR=1 | RD=1");
        assert_eq!(resp[3], 0x80, "RA=1");
        assert_eq!(u16::from_be_bytes([resp[4], resp[5]]), 1);
        assert_eq!(&resp[12..], &q[12..]);
        assert!(empty_response(b"short").is_none());
        assert!(qtype(b"short").is_none());
    }

    /// TTL 钳制 + AAAA 计数 + 截断（message.go 对拍）。
    #[test]
    fn clamp_count_truncate() {
        let resp = a_response("fake.test", 1, 3600, &[0x0102_0304, 0x0506_0708]);
        assert_eq!(count_aaaa(&resp), 0);
        let mut r = resp.clone();
        clamp_ttl(&mut r, MAX_TTL);
        // 两条 answer 的 TTL 都应钳到 60（最后一条 RR 尾布局 type2+class2+ttl4+rdlen2+rdata4）
        let off = r.len() - 10;
        assert_eq!(u32::from_be_bytes(r[off..off + 4].try_into().unwrap()), MAX_TTL);
        // AAAA 计数：构造一条 AAAA RR（把 A RR 的 type 位改 28）
        let mut r6 = resp.clone();
        let n = r6.len();
        r6[n - 14..n - 12].copy_from_slice(&28u16.to_be_bytes()); // 尾 RR 的 type 字段
        assert_eq!(count_aaaa(&r6), 1);
        // 截断：小 max 保留 header+question+TC
        let cut = truncate(&resp, 40);
        assert!(cut.len() <= 40);
        assert_eq!(cut[2] & 0x02, 0x02, "TC 应置位");
        assert_eq!(u16::from_be_bytes([cut[6], cut[7]]), 0, "一条都放不下时 AN=0");
        // 已 ≤max 原样
        let small = a_response("fake.test", 2, 5, &[1]);
        assert_eq!(truncate(&small, 512), small);
    }

    /// 本地 fake 上游：forward/exchange 全链（ID 重写回填 / TTL 钳制 / 过滤面）。
    #[test]
    fn forward_against_local_fake_upstream() {
        let up = UdpSocket::bind(("127.0.0.1", 0)).unwrap();
        let up_addr = format!("127.0.0.1:{}", up.local_addr().unwrap().port());
        std::thread::spawn(move || {
            let mut buf = [0u8; 1500];
            loop {
                let Ok((_n, from)) = up.recv_from(&mut buf) else { return };
                let mut r = a_response("fake.test", 0, 300, &[0x7f00_0001]);
                r[0..2].copy_from_slice(&buf[..2]);
                let _ = up.send_to(&r, from);
            }
        });
        let dir = std::env::temp_dir().join(format!("homeway-rs-dns-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("resolv.conf"), format!("nameserver {up_addr}\n")).unwrap();
        let (logf, dlogf) = noop();
        let core = ResponderCore {
            ups: Upstreams::new(&dir.join("resolv.conf").to_string_lossy()),
            stats: Arc::new(DnsStats::default()),
            fb_once: AtomicBool::new(false),
            budget: DEFAULT_BUDGET,
            fallback_dns: DEFAULT_FALLBACK.to_owned(),
            max_in_flight: MAX_IN_FLIGHT,
            logf,
            dlogf,
        };
        let q = a_query("fake.test", 0xABCD);
        let resp = core.respond(&q, false, &mut Vec::new()).unwrap();
        assert_eq!(&resp[..2], &0xABCDu16.to_be_bytes(), "原始 ID 应回填");
        assert_eq!(core.stats.resp.load(Ordering::Relaxed), 1);
        let rdoff = resp.len() - 10;
        assert_eq!(u32::from_be_bytes(resp[rdoff..rdoff + 4].try_into().unwrap()), MAX_TTL);
        // 过滤面：AAAA 查询回空应答（不经上游——QD=1/无 answer）
        let r2 = core.respond(&aaaa_query("fake.test"), false, &mut Vec::new()).unwrap();
        assert_eq!(u16::from_be_bytes([r2[4], r2[5]]), 1);
        assert_eq!(u16::from_be_bytes([r2[6], r2[7]]), 0, "空应答无 answer");
        assert_eq!(core.stats.filtered.load(Ordering::Relaxed), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 全上游死 → 兜底（fallback 指向另一 fake）→ fallback 计数；全死 → SERVFAIL。
    #[test]
    fn fallback_and_servfail() {
        let fb = UdpSocket::bind(("127.0.0.1", 0)).unwrap();
        let fb_port = fb.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let mut buf = [0u8; 1500];
            loop {
                let Ok((_n, from)) = fb.recv_from(&mut buf) else { return };
                let mut r = a_response("fb.test", 0, 10, &[0x7f00_0001]);
                r[0..2].copy_from_slice(&buf[..2]);
                let _ = fb.send_to(&r, from);
            }
        });
        let dir = std::env::temp_dir().join(format!("homeway-rs-dnsfb-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("resolv.conf"), "nameserver 127.0.0.1:1\n").unwrap();
        let (logf, dlogf) = noop();
        let core = ResponderCore {
            ups: Upstreams::new(&dir.join("resolv.conf").to_string_lossy()),
            stats: Arc::new(DnsStats::default()),
            fb_once: AtomicBool::new(false),
            budget: Duration::from_secs(1),
            fallback_dns: format!("127.0.0.1:{fb_port}"),
            max_in_flight: MAX_IN_FLIGHT,
            logf,
            dlogf,
        };
        let resp = core.respond(&a_query("fb.test", 0x4242), false, &mut Vec::new()).unwrap();
        assert_eq!(&resp[..2], &0x4242u16.to_be_bytes());
        assert_eq!(core.stats.fallback.load(Ordering::Relaxed), 1);
        // 全死（兜底也指死端口）→ SERVFAIL
        let core2 = ResponderCore {
            ups: Upstreams::new(&dir.join("resolv.conf").to_string_lossy()),
            stats: Arc::new(DnsStats::default()),
            fb_once: AtomicBool::new(false),
            budget: Duration::from_millis(300),
            fallback_dns: "127.0.0.1:1".to_owned(),
            max_in_flight: MAX_IN_FLIGHT,
            logf: Arc::new(|_| {}),
            dlogf: Arc::new(|_| {}),
        };
        let resp = core2.respond(&a_query("dead.test", 1), false, &mut Vec::new()).unwrap();
        assert_eq!(resp[3] & 0x0F, 2, "SERVFAIL");
        assert_eq!(core2.stats.fail.load(Ordering::Relaxed), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 专用 worker 池：submit 异步应答经回投通道到达（H3 形态验收）+ qtcp 单列。
    #[test]
    fn proxy_worker_roundtrip() {
        let up = UdpSocket::bind(("127.0.0.1", 0)).unwrap();
        let up_addr = format!("127.0.0.1:{}", up.local_addr().unwrap().port());
        std::thread::spawn(move || {
            let mut buf = [0u8; 1500];
            loop {
                let Ok((_n, from)) = up.recv_from(&mut buf) else { return };
                let mut r = a_response("wr.test", 0, 5, &[0x7f00_0001]);
                r[0..2].copy_from_slice(&buf[..2]);
                let _ = up.send_to(&r, from);
            }
        });
        let dir = std::env::temp_dir().join(format!("homeway-rs-dnswr-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("resolv.conf"), format!("nameserver {up_addr}\n")).unwrap();
        let (logf, dlogf) = noop();
        let (proxy, events) = DnsProxy::spawn(
            DnsConfig {
                resolv_path: dir.join("resolv.conf").to_string_lossy().into_owned(),
                ..Default::default()
            },
            logf,
            dlogf,
        );
        assert_eq!(
            proxy.submit_udp(42, a_query("wr.test", 0x7777)),
            SubmitOutcome::Accepted,
            "正常提交应进 worker 队列"
        );
        let reply = events.recv_timeout(Duration::from_secs(3)).expect("应答应到达");
        assert_eq!(reply.tag, 42);
        assert_eq!(&reply.resp.unwrap()[..2], &0x7777u16.to_be_bytes());
        // 同步面
        let resp = proxy.answer_sync(&a_query("wr.test", 0x8888)).unwrap();
        assert_eq!(&resp[..2], &0x8888u16.to_be_bytes());
        // 计数：q=1（只有 submit_udp 计；answer_sync = Go Answer() 直调口径不计 q）
        assert!(proxy.stats_line().contains("q=1"), "stats: {}", proxy.stats_line());
        assert!(proxy.stats_line().contains("resp=2"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// F2：在途超限 ⇒ `submit_*` 返 `Dropped`（调用方据此回收 route tag）+ drop 计数。
    #[test]
    fn submit_over_limit_returns_dropped() {
        // 上游不回（worker 阻塞到 budget）——max_in_flight=1 下第二条必被丢
        let up = UdpSocket::bind(("127.0.0.1", 0)).unwrap(); // 只绑不收
        let up_addr = format!("127.0.0.1:{}", up.local_addr().unwrap().port());
        let dir = std::env::temp_dir().join(format!("homeway-rs-dnsdrop-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("resolv.conf"), format!("nameserver {up_addr}\n")).unwrap();
        let (logf, dlogf) = noop();
        let (proxy, _events) = DnsProxy::spawn(
            DnsConfig {
                resolv_path: dir.join("resolv.conf").to_string_lossy().into_owned(),
                budget: Duration::from_secs(5),
                max_in_flight: 1,
                ..Default::default()
            },
            logf,
            dlogf,
        );
        assert_eq!(proxy.submit_udp(1, a_query("a.test", 0x1111)), SubmitOutcome::Accepted);
        assert_eq!(
            proxy.submit_udp(2, a_query("b.test", 0x2222)),
            SubmitOutcome::Dropped,
            "在途超限 = 丢弃（无应答回投 ⇒ 调用方须回收 tag）"
        );
        assert!(proxy.stats_line().contains("drop=1"), "丢弃计数：{}", proxy.stats_line());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 自检查询构造（R6.6 P1-① 回归钉）：完整 12 字节头 + qtype 可解析——
    /// 残头形态（5 字节）会让 qtype() 把名字当头解析 → 恒 malformed、自检恒误报。
    #[test]
    fn self_check_query_shape() {
        let q = self_check_query();
        assert_eq!(q.len(), 47, "12 头 + 30 名字 + 5 尾（根 + QTYPE/CLASS）");
        assert_eq!(&q[2..4], &[0x01, 0x00], "flags = RD（评审 r1：丢 RD 也绿的缺口钉死）");
        assert_eq!(qtype(&q), Some(1), "A 查询应可解析出 qtype");
        assert_eq!(u16::from_be_bytes([q[4], q[5]]), 1, "QDCOUNT=1");
        assert_eq!(&q[6..12], &[0u8; 6], "AN/NS/AR 全零");
        assert_eq!(empty_response(&q).map(|r| r.len()), Some(47), "可回显 question");
    }

    /// 自检链路真跑（R6.6 P1-①）：fake 上游 → self_check 过 + 不产 malformed；
    /// 上游全死 → SERVFAIL 报错（Go TestSelfCheckFailsWhenAllDead 对齐）。
    #[test]
    fn self_check_passes_with_upstream_and_fails_when_dead() {
        let up = UdpSocket::bind(("127.0.0.1", 0)).unwrap();
        let up_addr = format!("127.0.0.1:{}", up.local_addr().unwrap().port());
        std::thread::spawn(move || {
            let mut buf = [0u8; 1500];
            loop {
                let Ok((n, from)) = up.recv_from(&mut buf) else { return };
                let mut r = a_response("selfcheck.dns.homeway.invalid.", 0, 10, &[0x7f00_0001]);
                let id = u16::from_be_bytes([buf[0], buf[1]]);
                r[0..2].copy_from_slice(&id.to_be_bytes());
                r[12..12 + (n - 12)].copy_from_slice(&buf[12..n]);
                let _ = up.send_to(&r, from);
            }
        });
        let dir = std::env::temp_dir().join(format!("homeway-rs-dnssc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("resolv.conf"), format!("nameserver {up_addr}\n")).unwrap();
        let (logf, dlogf) = noop();
        let (proxy, _events) = DnsProxy::spawn(
            DnsConfig {
                resolv_path: dir.join("resolv.conf").to_string_lossy().into_owned(),
                fallback_dns: up_addr.clone(), // 兜底也指 fake（防真实外联）
                ..Default::default()
            },
            logf,
            dlogf,
        );
        assert!(proxy.self_check().is_ok(), "上游活着时自检应通过");
        assert!(
            !proxy.stats_line().contains("malformed=1"),
            "自检不应产 malformed：{}",
            proxy.stats_line()
        );

        // 上游全死（含兜底）：自检必须报错（老 bug 形态 = 恒「无应答」误报，
        // 修复后死上游报 SERVFAIL、活上游报 Ok——两种结局都真实反映链路）
        let dead_dir = dir.join("dead");
        std::fs::create_dir_all(&dead_dir).unwrap();
        std::fs::write(dead_dir.join("resolv.conf"), "nameserver 127.0.0.1:1\n").unwrap();
        let (proxy2, _events2) = DnsProxy::spawn(
            DnsConfig {
                resolv_path: dead_dir.join("resolv.conf").to_string_lossy().into_owned(),
                fallback_dns: "127.0.0.1:1".to_owned(), // 兜底也死（防真实外联 223.5.5.5）
                budget: Duration::from_millis(300),
                ..Default::default()
            },
            Arc::new(|_| {}),
            Arc::new(|_| {}),
        );
        let err = proxy2.self_check().expect_err("全死上游应报错");
        assert!(err.contains("SERVFAIL"), "死上游报 SERVFAIL：{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// dialAddr 形态（resolv.conf nameserver → 可拨地址）。
    #[test]
    fn dial_addr_shapes() {
        assert_eq!(dial_addr("192.168.1.1"), "192.168.1.1:53");
        assert_eq!(dial_addr("127.0.0.1:15353"), "127.0.0.1:15353");
        assert_eq!(dial_addr("fd00::1"), "[fd00::1]:53");
        assert_eq!(dial_addr("[fd00::1]:53"), "[fd00::1]:53");
    }

    // ---------- F4a 本腿绝对期限（per-attempt） ----------

    /// **核心回归（修前红/挂死）**：坏上游持续灌「错误 ID + 短包」，按**生产形态**
    /// 构造（`budget = 200ms`、全局 `deadline = now + 2500ms`）⇒ `exchange` 必须在
    /// 本腿期限（~200ms）内判负返回 `None`。若误把全局 deadline 当本腿期限重设，
    /// 用例会跑满 2.5s（> 1s 阈值 ⇒ 红）；修前（per-syscall 续命）则永不返回。
    #[test]
    fn exchange_absolute_deadline_under_poison_upstream() {
        let up = UdpSocket::bind(("127.0.0.1", 0)).unwrap();
        let up_addr = format!("127.0.0.1:{}", up.local_addr().unwrap().port());
        std::thread::spawn(move || {
            let mut buf = [0u8; 1500];
            let Ok((_n, from)) = up.recv_from(&mut buf) else { return };
            // 灌包 2s（跨过 2×budget）：错误 ID 的应答 + 短包交替。poison 的 **question
            // 段用别的域名**（即便 0xBEEF 与随机 new_id 撞车，question 比对也会拒）——
            // 修前 1/65536 概率的假绿/假红已消除。
            let poison = a_response("poison.test", 0xBEEF, 5, &[0x0102_0304]);
            let t0 = Instant::now();
            while t0.elapsed() < Duration::from_secs(2) {
                let _ = up.send_to(&poison, from);
                let _ = up.send_to(&[0u8; 8], from);
            }
        });
        let q = a_query("fake.test", 0x1234);
        let t0 = Instant::now();
        let r = exchange(
            &up_addr,
            &q,
            Duration::from_millis(200),
            Instant::now() + Duration::from_millis(2500),
            &mut Vec::new(),
        );
        let dt = t0.elapsed();
        assert!(r.is_none(), "灌包下必须按本腿期限判负（得 {r:?}）");
        assert!(
            dt < Duration::from_millis(1000),
            "本腿绝对期限（per-attempt ≈200ms；误用全局 deadline ⇒ ~2.5s）应生效（实 {dt:?}）"
        );
    }

    // ---------- F4b TCP 腿拨号期限 ----------

    /// 期限已过 ⇒ 立即放弃（不进入拨号）；黑洞形态（TEST-NET，无对端应答）也须
    /// 在 `budget + 余量` 内返回。注：拨号失败返 `None`（由 `exchange` 调用方回退到
    /// UDP 截断应答，既有语义）；本用例只钉**受界性**（修前无期限 = OS 默认可达 ~75s）。
    #[test]
    fn tcp_leg_connect_bounded() {
        let t0 = Instant::now();
        assert!(connect_within("192.0.2.1:53", Instant::now()).is_none(), "期限已过应放弃");
        assert!(t0.elapsed() < Duration::from_millis(100), "零剩余不得阻塞：{:?}", t0.elapsed());
        let q = a_query("blackhole.test", 0x2222);
        let udp_resp = a_response("blackhole.test", 0x2222, 5, &[1]);
        let t0 = Instant::now();
        let _ = exchange_tcp("192.0.2.1:53", &q, &udp_resp, Duration::from_millis(300));
        let dt = t0.elapsed();
        assert!(dt < Duration::from_millis(1000), "拨号须带期限（budget=300ms；实 {dt:?}）");
    }

    /// **M1 回归**：TCP 腿滴流上游（长度前缀声明 0xFFFF，1 字节/100ms 持续 5s）⇒
    /// `exchange_tcp` 按本腿绝对期限（300ms）收口返回（修前 `read_exact` 每字节续命，
    /// 最坏 ≈(2+65535)×budget）。用例在两种形态下都有界（修前 ~5s+ ⇒ 阈值 1s 判红）。
    #[test]
    fn tcp_leg_read_bounded_under_dribble() {
        use std::io::{Read as _, Write as _};
        let ln = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let addr = format!("127.0.0.1:{}", ln.local_addr().unwrap().port());
        std::thread::spawn(move || {
            for conn in ln.incoming().flatten() {
                let mut c = conn;
                // 读掉 2B 长度前缀 + 正文（尽力而为），随后滴流一段「大声明」响应
                let mut req = [0u8; 512];
                let _ = c.read(&mut req);
                let mut out: Vec<u8> = vec![0xFFu8, 0xFF]; // 声明 65535 字节
                let t0 = Instant::now();
                while t0.elapsed() < Duration::from_secs(5) {
                    out.push(0x41);
                    if c.write_all(&out).is_err() {
                        return;
                    }
                    out.clear();
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
        });
        let q = a_query("dribble.test", 0x3333);
        let udp_resp = a_response("dribble.test", 0x3333, 5, &[1]);
        let t0 = Instant::now();
        let _ = exchange_tcp(&addr, &q, &udp_resp, Duration::from_millis(300));
        let dt = t0.elapsed();
        assert!(dt < Duration::from_secs(1), "TCP 腿读须按绝对期限收口（实 {dt:?}）");
    }

    // ---------- F4c worker 池并发 ----------

    /// 并发 fake 上游（按请求起线程应答，避免 responder 串行化污染测量）：
    /// workers=8 + 8 条查询 ⇒ 总耗时远小于串行理论值（8×300ms）；workers=1 的控制臂
    /// 反向钉死（3 条查询 ≥ 2× 单条时延）。
    #[test]
    fn worker_pool_concurrency() {
        fn fake_upstream(delay: Duration) -> String {
            let up = UdpSocket::bind(("127.0.0.1", 0)).unwrap();
            let addr = format!("127.0.0.1:{}", up.local_addr().unwrap().port());
            std::thread::spawn(move || {
                let up = std::sync::Arc::new(up);
                let mut buf = [0u8; 1500];
                loop {
                    let Ok((n, from)) = up.recv_from(&mut buf) else { return };
                    let req = buf[..n].to_vec();
                    let up2 = std::sync::Arc::clone(&up);
                    std::thread::spawn(move || {
                        std::thread::sleep(delay);
                        let mut r = a_response("cc.test", 0, 5, &[0x7f00_0001]);
                        r[0..2].copy_from_slice(&req[..2]);
                        let _ = up2.send_to(&r, from);
                    });
                }
            });
            addr
        }
        fn run(workers: usize, n: usize, up_addr: &str) -> Duration {
            let dir = std::env::temp_dir().join(format!(
                "homeway-rs-dnswk-{}-{}-{}",
                std::process::id(),
                workers,
                std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().subsec_nanos()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("resolv.conf"), format!("nameserver {up_addr}\n")).unwrap();
            let (logf, dlogf) = noop();
            let (proxy, events) = DnsProxy::spawn(
                DnsConfig {
                    resolv_path: dir.join("resolv.conf").to_string_lossy().into_owned(),
                    // 兜底也指 fake（防真实外联；也避免"真兜底快速应答"造成的假绿）
                    fallback_dns: up_addr.to_owned(),
                    workers,
                    budget: Duration::from_secs(5),
                    ..Default::default()
                },
                logf,
                dlogf,
            );
            let t0 = Instant::now();
            for i in 0..n {
                assert_eq!(proxy.submit_udp(i as u64 + 1, a_query("cc.test", i as u16 + 1)), SubmitOutcome::Accepted);
            }
            for _ in 0..n {
                events.recv_timeout(Duration::from_secs(5)).expect("应答应到达");
            }
            let dt = t0.elapsed();
            let _ = std::fs::remove_dir_all(&dir);
            dt
        }
        // 生产缺省 = 64（F4c 的注入面之外，钉住默认值本身）
        assert_eq!(DnsConfig::default().filled().workers, DEFAULT_WORKERS);
        assert_eq!(DEFAULT_WORKERS, 64);
        let up = fake_upstream(Duration::from_millis(300));
        let parallel = run(8, 8, &up);
        // 串行理论值 = 8 × 300ms = 2.4s；并行 ≈300ms。阈值 1.2s = 串行值的 1/2（两侧 ≥2× 余量）
        assert!(parallel < Duration::from_millis(1200), "8 workers × 8 查询应并行（实 {parallel:?}）");
        let serial = run(1, 3, &up);
        // 单 worker：3 条 × 300ms = 900ms 理论；断言 ≥ 600ms（≥2× 余量，防串行臂被误判并行）
        assert!(serial >= Duration::from_millis(600), "单 worker 形态应串行（实 {serial:?}）");
    }

    /// 上游跟随：文件变更后 1s 节流窗内取 last-good、窗口后跟随。
    #[test]
    fn upstream_follow_on_change() {
        let dir = std::env::temp_dir().join(format!("homeway-rs-dnsup-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("resolv.conf");
        std::fs::write(&f, "nameserver 10.0.0.1\n").unwrap();
        let ups = Upstreams::new(f.to_string_lossy().as_ref());
        assert_eq!(ups.list().join(","), "10.0.0.1");
        std::fs::write(&f, "nameserver 10.0.0.2\n").unwrap();
        assert_eq!(ups.list().join(","), "10.0.0.1", "节流窗内保持 last-good");
        std::thread::sleep(CHECK_INTERVAL + Duration::from_millis(50));
        assert_eq!(ups.list().join(","), "10.0.0.2", "窗口后跟随变更");
        // 解析失败（写垃圾）保留 last-good
        std::thread::sleep(CHECK_INTERVAL + Duration::from_millis(50));
        std::fs::write(&f, "not a nameserver line\n").unwrap();
        std::thread::sleep(CHECK_INTERVAL + Duration::from_millis(50));
        assert_eq!(ups.list().join(","), "10.0.0.2", "坏文件保留 last-good");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// F5：上游列表 **Arc 快照**语义——未变窗口内 `Arc::ptr_eq`（clone = 引用计数，
    /// 不再每查询深拷贝）；变更时**换新 Arc**（在途查询持旧快照读到一致内容，
    /// 不被「读一半被换」）。
    #[test]
    fn upstream_list_is_arc_snapshot() {
        let dir = std::env::temp_dir().join(format!("homeway-rs-dnsarc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("resolv.conf");
        std::fs::write(&f, "nameserver 10.1.1.1\n").unwrap();
        let ups = Upstreams::new(f.to_string_lossy().as_ref());
        let a1 = ups.list();
        let a2 = ups.list();
        assert!(Arc::ptr_eq(&a1, &a2), "节流窗内 clone = 同一快照（引用计数）");
        assert_eq!(a1.join(","), "10.1.1.1");
        // 变更（跨节流窗）⇒ 换新 Arc；旧快照内容不变（在途查询一致性）
        std::thread::sleep(CHECK_INTERVAL + Duration::from_millis(50));
        std::fs::write(&f, "nameserver 10.1.1.2\n").unwrap();
        let b = ups.list();
        assert!(!Arc::ptr_eq(&a1, &b), "变更后必须换新 Arc（不原地改）");
        assert_eq!(a1.join(","), "10.1.1.1", "旧快照内容逐字不变");
        assert_eq!(b.join(","), "10.1.1.2");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
