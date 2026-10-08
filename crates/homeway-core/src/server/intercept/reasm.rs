//! 出口侧**有界 IPv4 分片重组**（Q-K F1；语义真源 = gVisor `internal/fragmentation`
//! 与 Linux `ipfrag`，取值/差异逐条登记在 `docs/INTEROP-CRITERIA.md` 与
//! `docs/reviews/QK-design.md` §3/§4）。
//!
//! 结构约束（**不得绕过**）：
//! - 重组发生在**建会话之前**——`push` 只碰本模块；建流唯一入口仍是
//!   `Interceptor::route_plain`（Q-B F7 的性质：分片自身**不得**建会话/污染载荷）；
//! - 重组成功的整包走**与正常包完全相同的** `route_plain` 路径（结构上不存在
//!   「分片旁路」）；
//! - 本模块**不碰 L4**：L4 校验和随后由既有 `rewrite_dst` 按与正常包相同的规则处理。
//!
//! 有界性（全部编译期常量；A2–A5 的处置见设计文档 §3）：
//! - 并发上下文 ≤ [`REASM_MAX_CTX`]，超限**淘汰最老**（gVisor `fragmentation.go:215-225`
//!   同形：不拒新）；
//! - 每源上下文 ≤ [`REASM_MAX_PER_SRC`]（**公平性闸**：合法单设备实际恒 ≤1——smoltcp
//!   的 `Fragmenter` 是单缓冲；抗多源伪造靠全局闸）；
//! - 每上下文片数 ≤ [`REASM_MAX_FRAGS`]（前提假设：隧道契约 MTU=1280 ⇒ 合法最坏
//!   `ceil(65515 / 1256) = 53` 片；异种 MTU 对端会被 `fragLimit` 拒——**有意更严**）；
//! - 全局字节 ≤ [`REASM_MAX_BYTES`]（= gVisor `HighFragThreshold` / Linux
//!   `ipfrag_high_thresh`；**冗余第二道闸**，见编译期断言）；
//! - 单上下文 ≤ 65535 字节（IPv4 `total_len` 是 u16——结构性）；
//! - 超时 [`REASM_TIMEOUT`]（= gVisor `ReassembleTimeout` = Linux `ipfrag_time`），
//!   **按 `created` 计、每片到达不续期**（gVisor `createdAt` 语义，
//!   `fragmentation.go:271-289`）——**实现者不得「顺手续期」**。

use std::collections::HashMap;
use std::net::Ipv4Addr;
use std::time::{Duration, Instant};

use super::nat::{fix_ip_checksum, FragSlice};

/// 全局并发上下文上限（≥ 2× `DEFAULT_MAX_DEVICES = 32`；64 × 64 KiB = 4 MiB 与
/// gVisor/Linux 的 `ipfrag_high_thresh` 同量级——两套口径同时对齐）。
pub(crate) const REASM_MAX_CTX: usize = 64;
/// 每源上下文上限（公平性闸——**不承担**抗多源伪造职责，那是全局闸）。
pub(crate) const REASM_MAX_PER_SRC: usize = 4;
/// 单上下文片数上限（前提假设 MTU=1280 ⇒ 合法最坏 53 片，留 ~20% 裕度）。
pub(crate) const REASM_MAX_FRAGS: usize = 64;
/// 全局重组字节上限（= gVisor `HighFragThreshold` / Linux `ipfrag_high_thresh`）。
pub(crate) const REASM_MAX_BYTES: usize = 4 * 1024 * 1024;
/// 重组超时（= gVisor `ReassembleTimeout` = Linux `ipfrag_time` = 30s；不续期）。
pub(crate) const REASM_TIMEOUT: Duration = Duration::from_secs(30);

// 编译期断言（对齐既有 `const _: () = assert!(UDP_OUT_GATE < WATERMARK);` 形态）：
// 每个上下文 ≤65535 字节（u16 结构性）⇒ `REASM_MAX_CTX` 个上下文也进不了
// `REASM_MAX_BYTES` ⇒ 字节闸是**冗余第二道闸**，只在「63 条近满上下文 + 新片」
// 这一窄窗口独立触发（设计 §3.2 的量化）。
const _: () = assert!(REASM_MAX_BYTES >= REASM_MAX_CTX * 65535);

/// 重组键（newtype 化；字段集对齐 gVisor `FragmentID{Source,Destination,ID,Protocol}`）。
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) struct FragKey {
    pub src: Ipv4Addr,
    pub dst: Ipv4Addr,
    pub ident: u16,
    pub proto: u8,
}

/// 整条上下文被丢弃的原因——与 `fragDrop` 恒等式的四项**一一对应**
/// （`fragDrop == fragBad + fragLimit + fragTimeout + fragOverlap`）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum DropReason {
    /// 非法片（偏移越界 / 非末片非 8 倍 / 空片）⇒ 整条丢弃。
    Bad,
    /// 重叠（部分重叠 / 内含重叠 / 冲突字节 / 越过末片边界）⇒ 整条丢弃。
    Overlap,
    /// 上限（上下文数 / 每源 / 片数 / 字节）触发的拒绝或淘汰。
    Limit,
    /// 30s 超时（不续期）。
    Timeout,
}

/// 一次丢弃的完整描述（`push` / `sweep` **统一**的原因通道）。
pub(crate) struct Dropped {
    pub reason: DropReason,
    /// 连带丢弃的分片包数（含触发本次丢弃的那一片）。
    pub packets: u64,
    /// 该上下文所属源（**日志行文用**——不进限流键，见 `Interceptor` 的两字段表）。
    pub src: Ipv4Addr,
    /// ICMP 载荷源（首片前缀 = IP 头 + 前 8 字节）——**仅超时淘汰且首片在位**时 `Some`
    /// （对齐 gVisor `OnReassemblyTimeout` 的 `if pkt != nil`）；其余原因恒 `None`。
    pub icmp_orig: Option<Vec<u8>>,
}

/// 一次 `push` 的结果。
///
/// 形态说明（设计 §2-F1.1 的 `PushOutcome` 枚举 → 本二字段结构的理由）：一次 push 可
/// **同时**产生两类事件——「为腾额度淘汰了旧上下文」（Dropped ≤ 若干条）与「本片被
/// 收下 / 完成 / 被拒」。单值枚举表达不了这种并发，故统一为「`dropped` 记录列表 +
/// `done` 整包」；丢弃原因仍是**单一通道**（`Dropped`），调用方一个循环记账。
/// **注意**：`done` 与 `dropped` **可以同时非空**（字节闸淘汰旧上下文 + 本条恰好完成）。
pub(crate) struct PushResult {
    /// 完成时的整包（改头后）。
    pub done: Option<Vec<u8>>,
    /// 本次调用连带丢弃的分片包（按原因分条）。
    pub dropped: Vec<Dropped>,
}

/// 一个已收下的片（按 `off` 升序、互不重叠——重叠在 `push` 的入闸处被整体拒绝）。
struct Piece {
    off: u32,
    data: Vec<u8>,
}

/// 一条重组上下文。
struct Ctx {
    /// 首片 IP 头（含选项，≤60B）——`off == 0` 的片到达时记录。
    hdr: Option<Vec<u8>>,
    /// 已收片（按 `off` 升序）。
    pieces: Vec<Piece>,
    /// 报文的**载荷总长**（由末片 `off*8 + len` 定下）。
    total_len: Option<usize>,
    /// Σ片长。
    bytes: usize,
    /// 上下文创建时刻——超时按本值计，**每片到达不续期**。
    created: Instant,
    /// 到达序（淘汰最老的判据；`Instant` 分辨率不足以排序同拍多上下文）。
    seq: u64,
    /// 本上下文承载的分片包数（丢弃连带计数的基数）。
    packets: u64,
}

/// 有界重组器（驱动线程独占，无锁）。
pub(crate) struct Reassembler {
    ctx: HashMap<FragKey, Ctx>,
    /// Σ(全部上下文 bytes)——全局字节闸的维护量（O(1)）。
    total_bytes: usize,
    /// 上下文到达序发号器。
    seq: u64,
}

impl Default for Reassembler {
    fn default() -> Self {
        Self::new()
    }
}

impl Reassembler {
    pub fn new() -> Self {
        Self { ctx: HashMap::new(), total_bytes: 0, seq: 0 }
    }

    /// 在册上下文数（观测/单测面）。
    #[allow(clippy::len_without_is_empty)] // 语义：`len()==0` 即空（无「容量」概念）
    pub fn len(&self) -> usize {
        self.ctx.len()
    }

    /// 某源的在册上下文数（日志行文面——**不进限流键**）。
    pub fn count_src(&self, src: Ipv4Addr) -> usize {
        self.ctx.keys().filter(|k| k.src == src).count()
    }

    /// 收工清空（`Interceptor::close` 用）：**静默**——收工面不发 ICMP、不记账。
    pub fn clear(&mut self) {
        self.ctx.clear();
        self.total_bytes = 0;
    }

    /// 收下一片。返回（是否完成 / 连带丢弃清单）。
    ///
    /// 偏移合法性（§4.2，任一违反 ⇒ 丢弃该片**并清掉整条上下文**——gVisor
    /// `ErrInvalidArgs ⇒ release(r)` 同形）：`more && len % 8 != 0`；`off*8 + len > 65535`；
    /// `len == 0`（`more` 真**与**假两态——**不得**把空末片当「末片定位器」）。
    ///
    /// 重叠（§4.3，**一律整条丢弃**——比 gVisor 严、与 Linux 一致；合法发送方永不产生）：
    /// - 同区间 **同字节** ⇒ 静默忽略（不计数，与 Go 一致）；
    /// - 同区间 **不同字节** ⇒ 冲突 ⇒ 整条丢弃；
    /// - **内含但区间不同**（落在已填充区间内）⇒ 整条丢弃。
    ///
    /// 末片边界（§4.2 第 5 条）：`total_len` 一旦由末片定下就**不可变**——任何
    /// **越过末尾**的片、以及**落在空洞里但范围不同**的另一片末片（后者 = gVisor
    /// `ErrFragmentConflict`，`reassembler.go:96-101`）都 ⇒ 整条丢弃。
    pub(crate) fn push(&mut self, frag: FragSlice<'_>, now: Instant) -> PushResult {
        let hdr = frag.hdr;
        let key = FragKey {
            src: hdr.src,
            dst: hdr.dst,
            ident: hdr.ident,
            proto: hdr.proto,
        };
        let off = hdr.frag_off as usize * 8;
        let len = frag.payload.len();
        // ---- ① 偏移合法性 ----
        if len == 0 || off + len > 65535 || (hdr.mf && !len.is_multiple_of(8)) {
            return self.drop_whole(key, DropReason::Bad, 1);
        }
        let end = off + len;
        let mut dropped: Vec<Dropped> = Vec::new();
        // ---- ② 上下文准入（新键才过闸；已存在的键不重复过每源闸） ----
        if !self.ctx.contains_key(&key) {
            if self.count_src(key.src) >= REASM_MAX_PER_SRC {
                // 每源闸：**拒新**（不淘汰他人——公平性闸，不是淘汰闸）
                dropped.push(Dropped { reason: DropReason::Limit, packets: 1, src: key.src, icmp_orig: None });
                return PushResult { done: None, dropped };
            }
            // 全局闸（上下文数 / 字节）：淘汰最老（不拒新，gVisor 同形）
            self.make_room(len, true, Some(key), &mut dropped);
            self.seq += 1;
            let seq = self.seq;
            self.ctx.insert(
                key,
                Ctx { hdr: None, pieces: Vec::new(), total_len: None, bytes: 0, created: now, seq, packets: 0 },
            );
        }
        // ---- ③ 邻域判定（有序二分 + 只与相邻片比较） ----
        enum Verdict {
            /// 可插入（位置）。
            Insert(usize),
            /// 同区间同字节 ⇒ 静默忽略（不计数）。
            Ignore,
            /// 重叠/冲突/越过末尾/末片边界不一致 ⇒ 整条丢弃。
            Overlap,
        }
        let verdict = {
            let ctx = self.ctx.get(&key).expect("刚插入/已存在");
            // 末片边界一致性（**先于**邻域判定）：`total_len` 一旦定下不可变——
            // 越过末尾、或另一片范围不同的末片（含落在空洞里的「更短末片」）都是冲突。
            let boundary_conflict = match ctx.total_len {
                Some(total) if end > total => true,
                Some(total) => !hdr.mf && total != end,
                None => false,
            };
            if boundary_conflict {
                Verdict::Overlap
            } else {
                let i = ctx.pieces.partition_point(|p| (p.off as usize) < off);
                let prev_hits = i > 0
                    && ctx.pieces[i - 1].off as usize + ctx.pieces[i - 1].data.len() > off;
                let next = ctx.pieces.get(i);
                let next_hits = next.is_some_and(|p| (p.off as usize) < end);
                if !prev_hits && !next_hits {
                    Verdict::Insert(i)
                } else if prev_hits {
                    // 前邻起点**严格小于**本片（二分定位性质）⇒ 区间必不同（部分/内含
                    // 重叠）⇒ 整条丢弃（比 gVisor 的「内含静默忽略」严、与 Linux 一致）
                    Verdict::Overlap
                } else {
                    let p = next.expect("next_hits ⇒ Some");
                    if p.off as usize == off && p.data.len() == len && p.data == frag.payload {
                        Verdict::Ignore
                    } else {
                        Verdict::Overlap
                    }
                }
            }
        };
        let insert_at = match verdict {
            Verdict::Ignore => return PushResult { done: None, dropped },
            Verdict::Overlap => {
                self.drop_whole_into(key, DropReason::Overlap, 1, &mut dropped);
                return PushResult { done: None, dropped };
            }
            Verdict::Insert(i) => i,
        };
        // ---- ④ 片数闸（只对**真插入**判——重复片不占片数） ----
        {
            let ctx = self.ctx.get(&key).expect("刚判存在");
            if ctx.pieces.len() >= REASM_MAX_FRAGS {
                self.drop_whole_into(key, DropReason::Limit, 1, &mut dropped);
                return PushResult { done: None, dropped };
            }
        }
        // ---- ⑤ 存储（含全局字节闸的冗余复查——上下文数不变，不再过计数闸） ----
        self.make_room(len, false, Some(key), &mut dropped);
        {
            let ctx = self.ctx.get_mut(&key).expect("刚判存在");
            if !hdr.mf {
                debug_assert!(ctx.total_len.is_none_or(|t| t == end), "末片边界不可变");
                ctx.total_len = Some(end);
            }
            if off == 0 {
                ctx.hdr = Some(frag.head.to_vec()); // 首片 IP 头（含选项）
            }
            ctx.pieces.insert(insert_at, Piece { off: off as u32, data: frag.payload.to_vec() });
            ctx.bytes += len;
            ctx.packets += 1;
        }
        self.total_bytes += len;
        // ---- ⑥ 完成判定 ----
        match self.try_finish(&key) {
            Finish::Done(full) => PushResult { done: Some(full), dropped },
            Finish::Pending => PushResult { done: None, dropped },
            Finish::Oversize => {
                // 输出长度超 IPv4 的 u16 total_len（首片带 IP 选项 ⇒ ihl+payload > 65535）
                // ⇒ 整条丢弃 + 计 Bad（**不得**静默回绕后当成「成功交付」——修前会
                // `as u16` 截断，产物被 `route_plain` 的 parse 拒而静默丢，却已计
                // `fragReasm`）。
                self.drop_whole_into(key, DropReason::Bad, 0, &mut dropped);
                PushResult { done: None, dropped }
            }
        }
    }

    /// 超时清扫（`pump` / `pump_hold` **每拍开头**调用；上下文 ≤64 ⇒ 线性扫可忽略）。
    /// 超时且首片在位 ⇒ `icmp_orig = Some(首片前缀)`（调用方发 ICMP 11/1）。
    pub(crate) fn sweep(&mut self, now: Instant) -> Vec<Dropped> {
        let mut out = Vec::new();
        let expired: Vec<FragKey> = self
            .ctx
            .iter()
            // 按 `created` 计、**不续期**（gVisor `createdAt` 语义）——不得改成
            // 「距最近一片」的时间。
            .filter(|(_, c)| now.duration_since(c.created) >= REASM_TIMEOUT)
            .map(|(k, _)| *k)
            .collect();
        for key in expired {
            if let Some(c) = self.ctx.remove(&key) {
                self.total_bytes -= c.bytes;
                out.push(Dropped {
                    reason: DropReason::Timeout,
                    packets: c.packets,
                    src: key.src,
                    icmp_orig: icmp_prefix(&c),
                });
            }
        }
        out
    }

    /// 全局闸：淘汰最老的上下文直到留出入额。`incoming` = 待入片长；`ctx_gate` = 是否
    /// 同时受「上下文数」闸约束（**新键准入为 true**；已存在的键只受字节闸——
    /// 上下文数不因它增长）；`skip` = 不得淘汰的键（本次要写入的那条）。
    ///
    /// **字节闸的分支在既有不变量下不可达**（纯防御第二道闸）：每上下文 ≤65535 字节
    /// （u16 `total_len` 结构性、且 `off + len > 65535` 已被 ① 拒）、上下文 ≤
    /// [`REASM_MAX_CTX`] ⇒ 任何合法写入后 `Σbytes ≤ 64 × 65535 = 4,194,240 <
    /// REASM_MAX_BYTES = 4,194,304`（编译期断言 `REASM_MAX_BYTES >= MAX_CTX * 65535`
    /// 即此式）。保留它 = 防「将来放宽单上下文上限或上下文数上限」时失去闸门
    /// （以及 `total_bytes` 记账被改坏时的兜底）。
    fn make_room(
        &mut self,
        incoming: usize,
        ctx_gate: bool,
        skip: Option<FragKey>,
        out: &mut Vec<Dropped>,
    ) {
        while (ctx_gate && self.ctx.len() >= REASM_MAX_CTX)
            || self.total_bytes + incoming > REASM_MAX_BYTES
        {
            let victim = self
                .ctx
                .iter()
                .filter(|(k, _)| Some(**k) != skip)
                .min_by_key(|(_, c)| c.seq)
                .map(|(k, _)| *k);
            let Some(victim) = victim else { return }; // 无可淘汰（防御：配置为 0 时不死循环）
            let c = self.ctx.remove(&victim).expect("刚判存在");
            self.total_bytes -= c.bytes;
            out.push(Dropped {
                reason: DropReason::Limit,
                packets: c.packets,
                src: victim.src,
                icmp_orig: None,
            });
            if self.ctx.is_empty() && self.total_bytes + incoming <= REASM_MAX_BYTES {
                break;
            }
        }
    }

    /// 本键的上下文数按源统计（O(64)）——`count_src` 的私有别名点已删（评审低-6）。
    /// 丢弃整条上下文（连带 `extra` 片——触发本次丢弃的那片本身）并**追加**到调用方的
    /// 丢弃清单（评审低-8：早退路径不得丢掉已累积的 `dropped`）。
    fn drop_whole_into(
        &mut self,
        key: FragKey,
        reason: DropReason,
        extra: u64,
        out: &mut Vec<Dropped>,
    ) {
        let carried = match self.ctx.remove(&key) {
            Some(c) => {
                self.total_bytes -= c.bytes;
                c.packets
            }
            None => 0,
        };
        out.push(Dropped { reason, packets: carried + extra, src: key.src, icmp_orig: None });
    }

    /// 丢弃整条上下文（单条结果形态——供 `push` 的纯早退点使用）。
    fn drop_whole(&mut self, key: FragKey, reason: DropReason, extra: u64) -> PushResult {
        let mut dropped = Vec::new();
        self.drop_whole_into(key, reason, extra, &mut dropped);
        PushResult { done: None, dropped }
    }

    /// 完成判定 + 组装（逐条串行；无重叠 ⇒ 片区间互不相交）。
    fn try_finish(&mut self, key: &FragKey) -> Finish {
        let Some(ctx) = self.ctx.get(key) else {
            return Finish::Pending;
        };
        let Some(total) = ctx.total_len else {
            return Finish::Pending;
        };
        let Some(hdr) = ctx.hdr.as_ref() else {
            return Finish::Pending; // 覆盖起点只有 off==0 的片能做到 ⇒ 首片必在位
        };
        // 连续覆盖检查（[0, total) 无空洞）
        let mut cursor = 0usize;
        for p in &ctx.pieces {
            if p.off as usize != cursor {
                return Finish::Pending;
            }
            cursor += p.data.len();
        }
        if cursor != total {
            return Finish::Pending;
        }
        // 输出长度必须装得进 IPv4 的 u16 `total_len`（首片带 IP 选项时 `ihl > 20`，
        // `ihl + total` 可超 65535——**不得**静默回绕成小值当成功交付）。
        let ihl = hdr.len();
        if ihl + total > 65535 {
            return Finish::Oversize;
        }
        // 组装：头 = 首片 IP 头（含选项）；体 = 逐片按 off 落位
        let mut buf = vec![0u8; ihl + total];
        buf[..ihl].copy_from_slice(hdr);
        for p in &ctx.pieces {
            buf[ihl + p.off as usize..][..p.data.len()].copy_from_slice(&p.data);
        }
        // 改头：total_len = ihl + payload；flags/frag_off 清零（MF/DF/off 全清）；
        // IP 校验和重算。**不碰 L4**（Q-B F7 的载荷污染面保持关闭；L4 校验和随后由
        // `rewrite_dst` 按与正常包相同的规则处理——勿在此加分片特判）。
        let tl = (ihl + total) as u16;
        buf[2..4].copy_from_slice(&tl.to_be_bytes());
        buf[6] = 0;
        buf[7] = 0;
        fix_ip_checksum(&mut buf);
        let c = self.ctx.remove(key).expect("刚判存在");
        self.total_bytes -= c.bytes;
        debug_assert_eq!(c.bytes, total, "无重叠 ⇒ Σ片长 == 覆盖字节");
        Finish::Done(buf)
    }
}

/// 完成判定的三态（`try_finish` 的返回形态）。
enum Finish {
    /// 完成——组装好的整包。
    Done(Vec<u8>),
    /// 尚未完整（等后续片 / 等首片 / 有空洞）。
    Pending,
    /// 已完整但 `ihl + payload > 65535`（首片带 IP 选项）——输出装不进 u16 total_len，
    /// 由调用方按 `Bad` 整条丢弃。
    Oversize,
}

/// 超时 ICMP 的载荷源：首片 IP 头 + 前 8 字节载荷（首片不在位 ⇒ None——不发 ICMP）。
/// 首片（`off == 0 && mf`）载荷恒是 8 的倍数且非空 ⇒ ≥8 字节，`min(8)` 只是防御。
fn icmp_prefix(c: &Ctx) -> Option<Vec<u8>> {
    let hdr = c.hdr.as_ref()?;
    let first = c.pieces.first().filter(|p| p.off == 0)?;
    let n = 8.min(first.data.len());
    let mut v = Vec::with_capacity(hdr.len() + n);
    v.extend_from_slice(hdr);
    v.extend_from_slice(&first.data[..n]);
    Some(v)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::intercept::nat::{build_udp, Ipv4FragHdr};

    /// 造一个「全长 `payload` 的 UDP 报文被切成 `frag_len` 字节/片」的分片序列——
    /// 走真实 `nat::build_udp` 造的整包再手工切（首片带完整 IP+UDP 头；后续片
    /// 复用同一 IP 头但 total_len/flags/off 改写）。
    fn fragments(payload: &[u8], frag_len: usize) -> Vec<Vec<u8>> {
        let full = build_udp(
            Ipv4Addr::new(100, 64, 10, 1),
            50000,
            Ipv4Addr::new(8, 8, 8, 8),
            9999,
            payload,
        );
        fragments_of(&full, frag_len)
    }

    /// 把整包（任意 `ihl`，含 IP 选项）按 `frag_len` 切成片序列。
    fn fragments_of(full: &[u8], frag_len: usize) -> Vec<Vec<u8>> {
        let ihl = (full[0] & 0x0f) as usize * 4;
        let total = u16::from_be_bytes([full[2], full[3]]) as usize;
        let body = &full[ihl..total];
        let ident = u16::from_be_bytes([full[4], full[5]]);
        let mut out = Vec::new();
        let mut off = 0usize;
        while off < body.len() {
            let n = frag_len.min(body.len() - off);
            let mf = off + n < body.len();
            let mut p = full[..ihl].to_vec();
            p[4..6].copy_from_slice(&ident.to_be_bytes());
            p[2..4].copy_from_slice(&((ihl + n) as u16).to_be_bytes());
            let flags = if mf { 0x2000u16 } else { 0 } | ((off / 8) as u16);
            p[6..8].copy_from_slice(&flags.to_be_bytes());
            p.extend_from_slice(&body[off..off + n]);
            crate::server::intercept::nat::fix_ip_checksum(&mut p);
            out.push(p);
            off += n;
        }
        out
    }

    /// 给整包加 4 字节 IP 选项（ihl 20 → 24；同步 total_len 与 IP 校验和）。
    /// **L4 校验和不受影响**（伪头与 L4 段字节都没变——这也是「选项腿」能过
    /// smoltcp 校验的原因）。
    fn add_ip_options(pkt: &[u8]) -> Vec<u8> {
        let total = u16::from_be_bytes([pkt[2], pkt[3]]) as usize;
        let mut out = Vec::with_capacity(pkt.len() + 4);
        out.extend_from_slice(&pkt[..20]);
        out.extend_from_slice(&[0x01, 0x01, 0x01, 0x01]); // NOP ×4（合法填充）
        out.extend_from_slice(&pkt[20..total]);
        out[0] = 0x46; // version 4 + ihl 6（24B 头）
        out[2..4].copy_from_slice(&((total + 4) as u16).to_be_bytes());
        crate::server::intercept::nat::fix_ip_checksum(&mut out);
        out
    }

    /// 用 `template` 的 IP 头（含选项）造一片：`off_bytes`（8 的倍数）、载荷 `len`。
    fn piece_of(template: &[u8], off_bytes: usize, len: usize, mf: bool, fill: u8) -> Vec<u8> {
        let ihl = (template[0] & 0x0f) as usize * 4;
        let ident = u16::from_be_bytes([template[4], template[5]]);
        let mut p = template[..ihl].to_vec();
        p[4..6].copy_from_slice(&ident.to_be_bytes());
        p[2..4].copy_from_slice(&((ihl + len) as u16).to_be_bytes());
        let flags = if mf { 0x2000u16 } else { 0 } | ((off_bytes / 8) as u16);
        p[6..8].copy_from_slice(&flags.to_be_bytes());
        p.extend(std::iter::repeat_n(fill, len));
        crate::server::intercept::nat::fix_ip_checksum(&mut p);
        p
    }

    fn push_pkt(r: &mut Reassembler, pkt: &[u8], now: Instant) -> PushResult {
        let f = Ipv4FragHdr::parse(pkt).expect("测试包可解析");
        r.push(f, now)
    }

    /// 首片带 IP 选项（ihl=24）：选项随首片保留、输出 `total_len` 自洽；
    /// **`ihl + payload > 65535` 时整条丢弃（Bad）**——`u16` 不得静默回绕
    /// （评审中-1：修前产物被 `route_plain` 的 parse 拒而静默丢，却已计成功）。
    #[test]
    fn ip_options_preserved_and_oversize_dropped() {
        let now = Instant::now();
        // ① 正常：ihl=24 + 208 字节体（UDP 头 8 + 载荷 200）⇒ 2 片
        let payload: Vec<u8> = (0..200u32).map(|i| (i % 61) as u8).collect();
        let full = add_ip_options(&build_udp(
            Ipv4Addr::new(100, 64, 10, 1),
            50000,
            Ipv4Addr::new(8, 8, 8, 8),
            9999,
            &payload,
        ));
        let frs = fragments_of(&full, 104); // 104 = 8 × 13
        assert_eq!(frs.len(), 2, "208 字节体切 104 ⇒ 2 片");
        let mut r = Reassembler::new();
        let mut done = push_pkt(&mut r, &frs[0], now).done;
        if done.is_none() {
            done = push_pkt(&mut r, &frs[1], now).done;
        }
        let done = done.expect("两片齐 ⇒ 完成");
        assert_eq!(done[0] & 0x0f, 6, "ihl 保留（24B 头）");
        assert_eq!(&done[20..24], &[0x01; 4], "IP 选项随首片保留");
        assert_eq!(
            u16::from_be_bytes([done[2], done[3]]) as usize,
            done.len(),
            "total_len = ihl + payload"
        );
        assert_eq!(&done[24..], &full[24..], "载荷逐字节（选项段之后全同）");
        // ② 越界：ihl=24 + 覆盖 65535 ⇒ ihl+total = 65559 > 65535 ⇒ 装不进 u16 ⇒ Bad
        let tpl = add_ip_options(&build_udp(
            Ipv4Addr::new(100, 64, 10, 1),
            50000,
            Ipv4Addr::new(8, 8, 8, 8),
            9999,
            &[0u8; 8],
        ));
        let f0 = piece_of(&tpl, 0, 120, true, 0x11); // [0,120)
        let f1 = piece_of(&tpl, 120, 65415, false, 0x22); // [120, 65535) ⇒ total = 65535
        let mut r = Reassembler::new();
        assert!(push_pkt(&mut r, &f0, now).done.is_none());
        let res = push_pkt(&mut r, &f1, now);
        assert!(res.done.is_none(), "u16 装不下 ⇒ 不得交付（不得回绕成小值）");
        assert_eq!(res.dropped.len(), 1);
        assert_eq!(res.dropped[0].reason, DropReason::Bad);
        assert_eq!(res.dropped[0].packets, 2, "整条丢弃（连带已收首片）");
        assert_eq!(r.len(), 0);
        assert_eq!(r.total_bytes, 0);
    }

    /// 两个范围不同的末片（含「更短、落在空洞里」那片）⇒ 冲突整条丢弃
    /// （gVisor `ErrFragmentConflict` 同形）。修前后者会把 `total_len` **改小** ⇒
    /// 覆盖已完整却永不完成、白占 30s（评审低-1）。
    #[test]
    fn shorter_final_fragment_conflicts() {
        let now = Instant::now();
        let tpl = build_udp(
            Ipv4Addr::new(100, 64, 10, 1),
            50000,
            Ipv4Addr::new(8, 8, 8, 8),
            9999,
            &[0u8; 8],
        );
        let a = piece_of(&tpl, 0, 96, true, 1); // [0,96) 非末片
        let b = piece_of(&tpl, 200, 100, false, 2); // 末片 [200,300) ⇒ total = 300（留洞）
        let c = piece_of(&tpl, 96, 104, false, 3); // 另一末片 [96,200)（更短、填洞）
        let mut r = Reassembler::new();
        push_pkt(&mut r, &a, now);
        push_pkt(&mut r, &b, now);
        let res = push_pkt(&mut r, &c, now);
        assert_eq!(res.dropped.len(), 1);
        assert_eq!(res.dropped[0].reason, DropReason::Overlap, "末片边界不一致 = 冲突");
        assert_eq!(res.dropped[0].packets, 3, "整条丢弃（连带已收 2 片）");
        assert_eq!(r.len(), 0);
    }

    /// 造一片：字节偏移 `off_bytes`（必须是 8 的倍数）、载荷 `payload`、`mf` 自定。
    fn piece_at(off_bytes: usize, payload: &[u8], mf: bool, ident: u16) -> Vec<u8> {
        let mut p = build_udp(
            Ipv4Addr::new(100, 64, 10, 1),
            50000,
            Ipv4Addr::new(8, 8, 8, 8),
            9999,
            payload,
        );
        let ihl = 20;
        p[4..6].copy_from_slice(&ident.to_be_bytes());
        p[2..4].copy_from_slice(&((ihl + payload.len()) as u16).to_be_bytes());
        let flags = if mf { 0x2000u16 } else { 0 } | ((off_bytes / 8) as u16);
        p[6..8].copy_from_slice(&flags.to_be_bytes());
        crate::server::intercept::nat::fix_ip_checksum(&mut p);
        p
    }

    /// 基础多片重组 + 乱序 + 交付字节逐一对齐。
    #[test]
    fn two_fragments_and_out_of_order() {
        let payload: Vec<u8> = (0..3000u32).map(|i| (i % 251) as u8).collect();
        let frs = fragments(&payload, 1256);
        assert_eq!(frs.len(), 3, "3016 字节体 ⇒ 3 片");
        let now = Instant::now();
        // 乱序：末片先到、首片最后
        let mut r = Reassembler::new();
        assert!(push_pkt(&mut r, &frs[2], now).done.is_none());
        assert!(push_pkt(&mut r, &frs[1], now).done.is_none());
        let done = push_pkt(&mut r, &frs[0], now).done.expect("三片齐 ⇒ 完成");
        // 整包 = IP 头 + UDP 头 + payload；改头后 total/flags 正确
        let v = Ipv4FragHdr::parse(&done).unwrap();
        assert_eq!(v.hdr.frag_off, 0);
        assert!(!v.hdr.mf, "MF 清零");
        assert_eq!(v.hdr.total_len, done.len());
        assert_eq!(&done[28..], &payload[..], "载荷逐字节一致（含 UDP 头）");
        assert_eq!(r.len(), 0, "完成后上下文摘除");
        assert_eq!(r.total_bytes, 0);
    }

    /// 同区间同字节重复 ⇒ 静默忽略（不计数）；上下文仍完成。
    #[test]
    fn duplicate_identical_ignored() {
        let payload: Vec<u8> = (0..3000u32).map(|i| (i % 13) as u8).collect();
        let frs = fragments(&payload, 1256);
        assert_eq!(frs.len(), 3);
        let now = Instant::now();
        let mut r = Reassembler::new();
        push_pkt(&mut r, &frs[0], now);
        let dup = push_pkt(&mut r, &frs[0], now);
        assert!(dup.dropped.is_empty(), "同区间同字节 ⇒ 不计数");
        push_pkt(&mut r, &frs[1], now);
        let dup = push_pkt(&mut r, &frs[1], now);
        assert!(dup.dropped.is_empty(), "重复中间片同样忽略");
        let done = push_pkt(&mut r, &frs[2], now).done;
        assert!(done.is_some(), "三片齐（含重复片不占片数）⇒ 完成");
    }

    /// 同区间不同字节 ⇒ 冲突 ⇒ 整条丢弃（连带包数 = 已收片 + 本片）。
    #[test]
    fn conflicting_duplicate_drops_whole() {
        let payload: Vec<u8> = (0..2500u32).map(|i| (i % 7) as u8).collect();
        let frs = fragments(&payload, 1256);
        let now = Instant::now();
        let mut r = Reassembler::new();
        push_pkt(&mut r, &frs[0], now);
        let mut evil = frs[0].clone();
        let n = evil.len();
        evil[n - 1] ^= 0xFF; // 同区间、改一字节
        crate::server::intercept::nat::fix_ip_checksum(&mut evil);
        let res = push_pkt(&mut r, &evil, now);
        assert_eq!(res.dropped.len(), 1);
        assert_eq!(res.dropped[0].reason, DropReason::Overlap);
        assert_eq!(res.dropped[0].packets, 2, "已收 1 片 + 冲突片 1");
        assert_eq!(r.len(), 0, "整条丢弃");
    }

    /// 重叠三子形态（**全部整条丢弃**——比 gVisor 严、与 Linux 一致）：
    /// ① 逆出边界（起点更早、尾越过前邻起点）；② 内含（严格落在已收区间内）；
    /// ③ 同起点但更短。
    #[test]
    fn overlap_subforms_drop_whole() {
        let now = Instant::now();
        let cases: [(&str, usize, usize); 3] = [
            ("逆出边界", 1248, 16),
            ("内含", 1264, 8),
            ("同起点更短", 1256, 8),
        ];
        for (name, off, len) in cases {
            let payload: Vec<u8> = (0..3000u32).map(|i| (i % 17) as u8).collect();
            let frs = fragments(&payload, 1256);
            let ident = u16::from_be_bytes([frs[1][4], frs[1][5]]); // 同 ident = 同上下文
            let mut r = Reassembler::new();
            push_pkt(&mut r, &frs[1], now); // 已收 [1256, 2512)
            let p = piece_at(off, &payload[..len], true, ident);
            let res = push_pkt(&mut r, &p, now);
            assert_eq!(res.dropped.len(), 1, "{name}：应整条丢弃");
            assert_eq!(res.dropped[0].reason, DropReason::Overlap, "{name}");
            assert_eq!(res.dropped[0].packets, 2, "{name}：连带 2 片");
            assert_eq!(r.len(), 0, "{name}：上下文清空");
        }
    }

    /// 偏移非法三态（非末片非 8 倍 / 越界 / 空片）⇒ fragBad 类丢弃。
    #[test]
    fn bad_offsets_drop_whole() {
        let now = Instant::now();
        // 非末片 9 字节（MF=1）
        let mut p = build_udp(Ipv4Addr::new(1, 1, 1, 1), 1, Ipv4Addr::new(2, 2, 2, 2), 2, &[0u8; 9]);
        p[6..8].copy_from_slice(&0x2000u16.to_be_bytes());
        p[2..4].copy_from_slice(&29u16.to_be_bytes());
        crate::server::intercept::nat::fix_ip_checksum(&mut p);
        let mut r = Reassembler::new();
        let res = push_pkt(&mut r, &p, now);
        assert_eq!(res.dropped[0].reason, DropReason::Bad);
        assert_eq!(res.dropped[0].packets, 1);
        // 越界：off = 65528（8191×8） + 16 字节 > 65535
        let mut q = build_udp(Ipv4Addr::new(1, 1, 1, 1), 1, Ipv4Addr::new(2, 2, 2, 2), 2, &[0u8; 16]);
        q[6..8].copy_from_slice(&(0x2000u16 | 8191).to_be_bytes());
        crate::server::intercept::nat::fix_ip_checksum(&mut q);
        let res = push_pkt(&mut r, &q, now);
        assert_eq!(res.dropped[0].reason, DropReason::Bad);
        // 空片（MF 真/假两态）——total_len = ihl ⇒ 片载荷空
        for flags in [0x2000u16, 0x0000] {
            let mut z = build_udp(Ipv4Addr::new(1, 1, 1, 1), 1, Ipv4Addr::new(2, 2, 2, 2), 2, &[]);
            z[6..8].copy_from_slice(&flags.to_be_bytes());
            z[2..4].copy_from_slice(&20u16.to_be_bytes());
            crate::server::intercept::nat::fix_ip_checksum(&mut z);
            let mut r2 = Reassembler::new();
            let res = push_pkt(&mut r2, &z, now);
            assert_eq!(res.dropped[0].reason, DropReason::Bad, "空片（flags={flags:#x}）");
            assert_eq!(r2.len(), 0, "空片不建上下文");
        }
    }

    /// 两个不同范围的末片 ⇒ 冲突 ⇒ 整条丢弃。
    #[test]
    fn conflicting_final_drops_whole() {
        let now = Instant::now();
        let payload: Vec<u8> = (0..3000u32).map(|i| (i % 5) as u8).collect();
        let frs = fragments(&payload, 1256);
        assert_eq!(frs.len(), 3);
        let mut r = Reassembler::new();
        push_pkt(&mut r, &frs[2], now); // 末片先到 ⇒ total = 3008
        // 另一片越过末尾：off = 3000、末片、1256 字节 ⇒ end = 4256 > 3008 ⇒ 冲突
        let mut p = frs[1].clone();
        p[6..8].copy_from_slice(&(3000u16 / 8).to_be_bytes()); // off = 3000（mf=0）
        crate::server::intercept::nat::fix_ip_checksum(&mut p);
        let res = push_pkt(&mut r, &p, now);
        assert_eq!(res.dropped.len(), 1);
        assert_eq!(res.dropped[0].reason, DropReason::Overlap);
        assert_eq!(r.len(), 0);
    }

    /// 超时淘汰（按 created 计、不续期）：注入 now+31s ⇒ 清空 + `icmp_orig` 在首片在位时 Some。
    #[test]
    fn timeout_evicts_and_not_renewed() {
        let payload: Vec<u8> = (0..3000u32).map(|i| (i % 3) as u8).collect();
        let frs = fragments(&payload, 1256);
        let t0 = Instant::now();
        let mut r = Reassembler::new();
        push_pkt(&mut r, &frs[0], t0);
        // 多片到达**不**延长寿命（不续期）
        push_pkt(&mut r, &frs[1], t0 + Duration::from_secs(29));
        let out = r.sweep(t0 + Duration::from_secs(31));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].reason, DropReason::Timeout);
        assert_eq!(out[0].packets, 2);
        let icmp = out[0].icmp_orig.as_ref().expect("首片在位 ⇒ 有 ICMP 载荷源");
        assert_eq!(icmp.len(), 20 + 8, "IP 头 + 前 8 字节");
        assert_eq!(r.len(), 0, "槽位回收");
        assert_eq!(r.total_bytes, 0);
        // 槽位回收后合法重组可进
        for p in &frs {
            push_pkt(&mut r, p, t0 + Duration::from_secs(31));
        }
        assert_eq!(r.len(), 0, "重放三片应完成");
        // 无首片 ⇒ 无 ICMP（对齐 gVisor `if pkt != nil`）
        let mut r2 = Reassembler::new();
        push_pkt(&mut r2, &frs[1], t0);
        let out = r2.sweep(t0 + Duration::from_secs(31));
        assert!(out[0].icmp_orig.is_none(), "无首片不发 ICMP");
    }

    /// 全局上下文闸：造 MAX_CTX+1 条（每源 ≤ REASM_MAX_PER_SRC ⇒ 用不同源地址）
    /// ⇒ 淘汰最老、新报文能进。
    #[test]
    fn ctx_limit_evicts_oldest() {
        let now = Instant::now();
        let mut r = Reassembler::new();
        let mut dropped = Vec::new();
        let src_of = |i: u16| Ipv4Addr::new(10, (i >> 8) as u8, (i & 0xff) as u8, 1);
        for i in 0..REASM_MAX_CTX as u16 {
            let mut p =
                build_udp(src_of(i), 1, Ipv4Addr::new(2, 2, 2, 2), 2, &[0u8; 8]);
            p[4..6].copy_from_slice(&(1000 + i).to_be_bytes()); // ident 区分
            p[6..8].copy_from_slice(&0x2000u16.to_be_bytes()); // MF 首片
            crate::server::intercept::nat::fix_ip_checksum(&mut p);
            let res = push_pkt(&mut r, &p, now);
            dropped.extend(res.dropped);
        }
        assert_eq!(r.len(), REASM_MAX_CTX);
        assert!(dropped.is_empty(), "未超限不淘汰");
        // 第 65 条（新 ident + 新源）
        let mut p = build_udp(Ipv4Addr::new(10, 255, 255, 1), 1, Ipv4Addr::new(2, 2, 2, 2), 2, &[0u8; 8]);
        p[4..6].copy_from_slice(&9999u16.to_be_bytes());
        p[6..8].copy_from_slice(&0x2000u16.to_be_bytes());
        crate::server::intercept::nat::fix_ip_checksum(&mut p);
        let res = push_pkt(&mut r, &p, now);
        assert_eq!(res.dropped.len(), 1, "淘汰最老一条");
        assert_eq!(res.dropped[0].reason, DropReason::Limit);
        assert_eq!(r.len(), REASM_MAX_CTX, "新报文进（不拒新）");
    }

    /// 每源闸：单源第 5 条 ⇒ 拒新（上下文数不变）。
    #[test]
    fn per_src_limit_rejects_new() {
        let now = Instant::now();
        let mut r = Reassembler::new();
        for i in 0..REASM_MAX_PER_SRC as u16 {
            let mut p = build_udp(Ipv4Addr::new(9, 9, 9, 9), 1, Ipv4Addr::new(2, 2, 2, 2), 2, &[0u8; 8]);
            p[4..6].copy_from_slice(&(i + 1).to_be_bytes());
            p[6..8].copy_from_slice(&0x2000u16.to_be_bytes());
            crate::server::intercept::nat::fix_ip_checksum(&mut p);
            push_pkt(&mut r, &p, now);
        }
        assert_eq!(r.len(), REASM_MAX_PER_SRC);
        let mut p = build_udp(Ipv4Addr::new(9, 9, 9, 9), 1, Ipv4Addr::new(2, 2, 2, 2), 2, &[0u8; 8]);
        p[4..6].copy_from_slice(&77u16.to_be_bytes());
        p[6..8].copy_from_slice(&0x2000u16.to_be_bytes());
        crate::server::intercept::nat::fix_ip_checksum(&mut p);
        let res = push_pkt(&mut r, &p, now);
        assert_eq!(res.dropped.len(), 1);
        assert_eq!(res.dropped[0].reason, DropReason::Limit);
        assert_eq!(res.dropped[0].packets, 1, "只有被拒的新片");
        assert_eq!(r.len(), REASM_MAX_PER_SRC, "不建上下文");
    }

    /// 片数闸：单上下文第 65 片 ⇒ 整条丢弃。
    #[test]
    fn frag_count_limit_drops_whole() {
        let now = Instant::now();
        let mut r = Reassembler::new();
        // 造 64 片互不相邻的片（off 依次 +8、片长恒 8——`&[]` ⇒ body 恰 8B 的 UDP 头）
        let mut last = None;
        for i in 0..REASM_MAX_FRAGS as u16 {
            let mut p = build_udp(Ipv4Addr::new(3, 3, 3, 3), 1, Ipv4Addr::new(4, 4, 4, 4), 2, &[]);
            p[6..8].copy_from_slice(&(0x2000u16 | i).to_be_bytes()); // off = i*8
            crate::server::intercept::nat::fix_ip_checksum(&mut p);
            last = Some(push_pkt(&mut r, &p, now));
        }
        assert!(last.unwrap().dropped.is_empty());
        assert_eq!(r.len(), 1);
        // 第 65 片
        let mut p = build_udp(Ipv4Addr::new(3, 3, 3, 3), 1, Ipv4Addr::new(4, 4, 4, 4), 2, &[]);
        p[6..8].copy_from_slice(&(0x2000u16 | (REASM_MAX_FRAGS as u16 + 5)).to_be_bytes());
        crate::server::intercept::nat::fix_ip_checksum(&mut p);
        let res = push_pkt(&mut r, &p, now);
        assert_eq!(res.dropped.len(), 1);
        assert_eq!(res.dropped[0].reason, DropReason::Limit);
        assert_eq!(res.dropped[0].packets, (REASM_MAX_FRAGS + 1) as u64);
        assert_eq!(r.len(), 0, "整条丢弃");
    }

    /// 片载荷取段钉死（T12）：`total_len` 之后的尾随字节不进重组结果。
    #[test]
    fn trailing_bytes_excluded() {
        let payload: Vec<u8> = (0..3000u32).map(|i| (i % 11) as u8).collect();
        let frs = fragments(&payload, 1256);
        let now = Instant::now();
        let mut r = Reassembler::new();
        for (i, p) in frs.iter().enumerate() {
            let mut p = p.clone();
            // 尾部追加垃圾（不改 total_len —— 模拟链路填充）
            p.extend_from_slice(&[0xEE; 16]);
            let res = push_pkt(&mut r, &p, now);
            if i + 1 == frs.len() {
                let done = res.done.expect("完成");
                assert!(!done.windows(4).any(|w| w == [0xEE; 4]), "尾随字节不得夹带");
                assert_eq!(&done[28..], &payload[..]);
            }
        }
    }
}
