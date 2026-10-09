//! 出口 QUIC 面 ↔ 引擎（同步面）的**两向边界**（M1 设计 §1.3/§1.4/§1.5/§1.7/§6.4）：
//! 入站队列 + 唤醒 fd + 出站队列 + 绑定表 + 丢弃计数。
//!
//! 为什么必须有唤醒 fd（§1.4）：引擎主循环的命令面是 `try_recv` + `poll(…, 1|5ms)`
//! （`engine.rs` 的驱动循环），没有唤醒面时每个入站包最多等 5ms ⇒ TCP RTT/吞吐直接受损。
//! 唤醒形态 = **self-pipe**（`UnixStream::pair`，两端非阻塞）：出口面入队一包写 1B，
//! 引擎把该 fd 放进 poll 集，醒来后**先排空字节、再取队列**（顺序即防丢唤醒：字节已读
//! 而条目未取的竞态由「下次 poll 立即返回」兜住，不会永久空转）。
//!
//! 队列上限照 §6.4 的矩阵**绝对条数**（两侧各 8192）：满 ⇒ **丢 + 计数**，不许静默、
//! 不许把阻塞传染给引擎 poll。
//!
//! 本文件属异步面（`exit/**`）：出站队列用 tokio 通道（引擎侧 `try_send` 非阻塞）。

use std::collections::{HashMap, VecDeque};
use std::io::{Read, Write};
use std::net::Ipv4Addr;
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::net::UnixStream;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use quinn::{Connection, VarInt};
use tokio::sync::mpsc;
use tokio::sync::oneshot;

use crate::cmd::Logf;
use crate::reg4::{EXPORTER_LEN, Reg4Frame};
use crate::sync_util::lock_unpoison;

use super::{ExitStats, log_due};

/// 入站队列上限（§6.4：出口 · QUIC 面 → 引擎入站队列 = **8192 条**，多设备共享）。
pub(crate) const INBOUND_QUEUE_MAX: usize = 8192;
/// 出站队列上限（§6.4：出口 · 引擎 → QUIC 面出站队列 = **8192 条**）。
pub(crate) const OUTBOUND_QUEUE_MAX: usize = 8192;

/// 出口面 → 引擎的入站事件。
///
/// `#[non_exhaustive]`（跨 crate 消费面，M1 起会随观测面生长）：消费侧必须留通配臂。
#[non_exhaustive]
pub enum ExitInbound {
    /// 准入/刷新请求：首条 bidi 控制流上的 `hr-reg4` Proof 帧或 60s 刷新帧（`R4`）。
    /// **引擎必须回执**（[`Reg4Request::reply`]）——不回执 = 该连接卡在准入门外。
    Reg(Reg4Request),
    /// 某设备的内层明文 IPv4 包（**源校验已在出口面过**，§1.4）。
    Packet {
        /// 来源设备的 devTag（绑定表的键；观测/归因用）。
        dev: [u8; 8],
        /// 内层明文包（与今日 `device.rs` 的 `StepOut::PlainV4` 同形态）。
        pkt: Vec<u8>,
    },
}

/// 一条 `hr-reg4` 准入/刷新请求（帧 + 本连接的 TLS exporter + 回执口）。
pub struct Reg4Request {
    /// 已解出的帧（MAC **未**验——secret 在引擎侧）。
    pub frame: Reg4Frame,
    /// 本帧所属连接上现算的 TLS exporter（32B）——MAC 的连接绑定值。
    pub exporter: [u8; EXPORTER_LEN],
    /// 回执口（引擎 → 出口面；**消费 self** ⇒ 类型上保证一次裁决一次回执）。
    ///
    /// `oneshot::Sender::send` 是**非阻塞**的（可在同步线程调用，不需要 runtime 上下文）
    /// ——故引擎侧不必持异步栈类型（隔离门：同步面写不出 `tokio::`）。
    reply: oneshot::Sender<Reg4Verdict>,
}

impl Reg4Request {
    /// 内部构造（出口面专用；tests 走 `exit::tests` 的同 crate 面）。
    pub(crate) fn new(
        frame: Reg4Frame,
        exporter: [u8; EXPORTER_LEN],
        reply: oneshot::Sender<Reg4Verdict>,
    ) -> Self {
        Self { frame, exporter, reply }
    }

    /// 回执（非阻塞；接收端已放弃时静默——连接多半已死）。
    pub fn reply(self, verdict: Reg4Verdict) {
        let _ = self.reply.send(verdict);
    }
}

/// 引擎裁决拒绝的**类**（M3 §4：准入关闭码分桶用；**粗粒度两桶**——细粒度会给未认证
/// 对端更多 Oracle）。
///
/// 分桶依据（`homeway-core` 的 `table::RejectReason` 四值）：`no-token`/`revoked` ⇒
/// [`Self::Credential`]；`table-full`/`ip-conflict`（表压）⇒ [`Self::Resource`]。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[non_exhaustive]
pub enum EngineRejectClass {
    /// 凭证面（`no-token`/`revoked`）⇒ 关闭码 `0x11`。
    Credential,
    /// 资源面（`table-full`/`ip-conflict`）⇒ 关闭码 `0x12`。
    Resource,
}

/// 引擎裁决的**类型化拒绝原因**（设计门 r14 F7）：MAC 试秘在引擎侧，出口面拿不到原因
/// ⇒ 必须由 verdict 携带（否则出口面打不出 `hr-reg4 MAC 不符` 这条归因行）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[non_exhaustive]
pub enum RejectWhy {
    /// 逐 secret 试秘全不命中（含**换连接重放**：exporter 不同 ⇒ MAC 恒不匹配）。
    MacMismatch,
    /// 引擎裁决拒绝（时间窗 ±90s / 吊销 / 表满 / 地址冲突）——表内已打归因行 + 计数。
    /// **M3 §4**：带 [`EngineRejectClass`]（准入关闭码的分桶输入；行文与会话级拒绝面不变）。
    EngineRejected {
        /// 拒绝的类（凭证面 / 资源面）。
        class: EngineRejectClass,
    },
    /// **刷新帧的第三道前置**（设计 §1.4 步骤 6 ② / 设计门 r14 F25）：MAC 未验之前先看
    /// 「表内仍在册」——设备已被淘汰（TTL/表满/显式摘除）时刷新帧必须被拒，否则表里一有
    /// 空位它就会把设备重新 `Added`（**resurrect**：淘汰语义被一个刷新帧抹平）。
    /// 本道拒绝**不落表、不进表内拒绝计数**（设备的淘汰归因已由 TTL/表满路径打过）。
    RefreshNotRegistered,
}

impl RejectWhy {
    /// 出口面 `准入被拒` 行的 `why` 文案（设计 §1.4-4.3 的两类落点 + 刷新第三道前置）。
    /// **逐字不变**（M3 §4：出口侧详细归因行不改——只加关闭码）。
    pub fn text(self) -> &'static str {
        match self {
            Self::MacMismatch => "hr-reg4 MAC 不符——含换连接重放",
            Self::EngineRejected { .. } => "引擎裁决拒绝（见引擎侧归因行）",
            Self::RefreshNotRegistered => "刷新帧但设备不在册（已淘汰，不 resurrect）",
        }
    }
}

/// 准入裁决（引擎 → 出口面）。
///
/// `#[non_exhaustive]`：新增裁决形态不得硬断出口面（消费侧留通配臂）。
/// **没有 `Challenge` 变体**（设计门 r14 F7）：nonce/pending 全在出口面（本连接的任务内），
/// 引擎只回答「这条 Proof/刷新帧认不认」。
#[derive(Debug)]
#[non_exhaustive]
pub enum Reg4Verdict {
    /// 通过（`table.register` 的 Added/Rotated/Refreshed 任一）：附该设备的派生地址
    /// （出口面用于源校验 + 绑定）。
    Accepted {
        /// `hw-tun` 派生地址（客户端核栈地址）。
        tunnel_ip: Ipv4Addr,
        /// `hw-app` 派生地址（App 的 TUN 地址；出站分流的 QUIC 键，§1.5）。
        tun_ip: Ipv4Addr,
    },
    /// 拒绝（原因**类型化**——出口面按 `why` 打不同的归因行）。
    Rejected {
        /// 拒绝原因（`MacMismatch` / `EngineRejected`）。
        why: RejectWhy,
    },
}

/// 引擎 → 出口面的出站请求（目标设备公钥 + 内层明文包）。
pub(crate) struct Outbound {
    pub(crate) pubkey: [u8; 32],
    pub(crate) pkt: Vec<u8>,
}

/// [`crate::ExitQuic::send_to_pub`] 的调用面结果（M1 §1.5 的分流键）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ExitSend {
    /// 已由 QUIC 面承接：入队成功，**或**队列满 ⇒ 已**丢 + 计数**（§6.4）。
    /// 两种情形都**不得二次投递**（同一包两条路径各发一次 = 重复）。
    Handled,
    /// 该设备在 QUIC 面**无绑定**（含出口面已死）：调用方按「丢 + 计数 + 记行」处置
    /// （M5 §3.2-bis ②：原「按 WG `encapsulate` 原样走」的兜底面已随 WG 删除）。
    Unbound,
}

/// 丢弃归类（§6.4 的四类；文案 = 判据行 E-q3 的字段名）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum DropKind {
    /// 内层包 > `max_datagram_size()`（或 quinn 报 `TooLarge`）。
    TooLarge,
    /// 发送缓冲不足（出口侧含两条：引擎→面出站队列满、per-conn 发送缓冲预检不过）。
    SendBufferFull,
    /// 未登记（未登记连接的数据报、入境队列满、出站目标无绑定、连接未就绪）。
    Unregistered,
    /// 源校验拒（复刻 `device.rs` 的 `src_allowed`）。
    SrcRejected,
}

impl DropKind {
    pub(crate) fn text(self) -> &'static str {
        match self {
            DropKind::TooLarge => "超限",
            DropKind::SendBufferFull => "发送缓冲满",
            DropKind::Unregistered => "未登记",
            DropKind::SrcRejected => "源校验拒",
        }
    }

    fn bump(self, stats: &ExitStats) -> u64 {
        let c = match self {
            DropKind::TooLarge => &stats.drop_too_large,
            DropKind::SendBufferFull => &stats.drop_send_buffer_full,
            DropKind::Unregistered => &stats.drop_unregistered,
            DropKind::SrcRejected => &stats.drop_src_rejected,
        };
        c.fetch_add(1, Ordering::SeqCst) + 1
    }
}

/// 一条已建连绑定的设备（§1.3 的 `devKey ↔ ConnectionHandle`）。
struct Binding {
    dev: [u8; 8],
    pubkey: [u8; 32],
    conn_id: u64,
    conn: Connection,
    tunnel_ip: Ipv4Addr,
    tun_ip: Ipv4Addr,
}

/// 绑定的只读快照（数据报源校验面 / 刷新帧的身份比对面）。
///
/// 「连接 = 设备」不变的承载：一条 `conn_id` 在表内**只**对应一个 `dev`/`pubkey`
/// （设计门 r14 F15；`bind`/`unbind_*` 后由 [`ExitBridge::assert_bindings_consistent`] 校验）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Bound {
    pub(crate) dev: [u8; 8],
    pub(crate) pubkey: [u8; 32],
    pub(crate) tunnel_ip: Ipv4Addr,
    pub(crate) tun_ip: Ipv4Addr,
}

/// 绑定表三索引（devTag 主键 + 公钥 + 连接号回指）。
#[derive(Default)]
struct Bindings {
    by_dev: HashMap<[u8; 8], Binding>,
    by_pub: HashMap<[u8; 32], [u8; 8]>,
    by_conn: HashMap<u64, [u8; 8]>,
}

impl Bindings {
    /// 三索引一致性（**内部不变量**；`bind`/`unbind_*` 之后必须成立）。
    ///
    /// 逐条对应设计门 r14 F15 的破裂形态：`by_conn` 被覆盖而旧 dev 索引残留。
    fn is_consistent(&self) -> bool {
        if self.by_dev.len() != self.by_pub.len() || self.by_dev.len() != self.by_conn.len() {
            return false;
        }
        for (dev, b) in &self.by_dev {
            if self.by_pub.get(&b.pubkey) != Some(dev) {
                return false;
            }
            if self.by_conn.get(&b.conn_id) != Some(dev) {
                return false;
            }
        }
        true
    }
}

/// 两向边界（出口面线程与引擎线程共享；`Arc`）。
pub(crate) struct ExitBridge {
    stats: Arc<ExitStats>,
    logf: Logf,
    /// 入站队列（出口面 → 引擎；批次换出由引擎侧 drain）。
    inbound: Mutex<VecDeque<ExitInbound>>,
    /// 唤醒 fd（读端留引擎 poll；字节只作「有事件」语义）。
    wake_rx: UnixStream,
    wake_tx: UnixStream,
    /// 出站队列（引擎 → 出口面；`try_send` 非阻塞）。
    out_tx: mpsc::Sender<Outbound>,
    bindings: Mutex<Bindings>,
}

impl ExitBridge {
    pub(crate) fn new(
        stats: Arc<ExitStats>,
        logf: Logf,
        wake_tx: UnixStream,
        wake_rx: UnixStream,
        out_tx: mpsc::Sender<Outbound>,
    ) -> Self {
        Self {
            stats,
            logf,
            inbound: Mutex::new(VecDeque::new()),
            wake_rx,
            wake_tx,
            out_tx,
            bindings: Mutex::new(Bindings::default()),
        }
    }

    /// 唤醒 fd（引擎 poll 用；**引擎不得 close 它**——所有权在出口面句柄）。
    pub(crate) fn wake_fd(&self) -> RawFd {
        self.wake_rx.as_raw_fd()
    }

    /// 写唤醒字节（入队后调；非阻塞——管道满时读端必可读，唤醒不丢）。
    fn wake(&self) {
        match (&self.wake_tx).write(&[1u8]) {
            Ok(_) => {}
            // WouldBlock = 管道满 ⇒ 读端已有未消费字节 ⇒ 唤醒已有效
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            // 读端已关（面已收工）：静默（队列随收工丢弃）
            Err(_) => {}
        }
    }

    /// 排空唤醒字节（**必须在取队列之前**——顺序即防丢唤醒，见模块头）。
    pub(crate) fn drain_wake(&self) {
        let mut buf = [0u8; 64];
        loop {
            match (&self.wake_rx).read(&mut buf) {
                Ok(0) => break, // 写端关闭
                Ok(_) => continue,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(_) => break,
            }
        }
    }

    /// 批次换出入站队列（锁内只做搬运；回调在锁外跑，不把引擎工作攥在队列锁里）。
    pub(crate) fn take_inbound(&self) -> VecDeque<ExitInbound> {
        let mut q = lock_unpoison(&self.inbound);
        std::mem::take(&mut *q)
    }

    /// 入队一事件（满 ⇒ `false` + 调用方计 `未登记`，§6.4）。
    pub(crate) fn push_inbound(&self, item: ExitInbound) -> bool {
        {
            let mut q = lock_unpoison(&self.inbound);
            if q.len() >= INBOUND_QUEUE_MAX {
                return false;
            }
            q.push_back(item);
        }
        self.wake();
        true
    }

    /// 出站投递（引擎线程调用；**非阻塞**）。
    pub(crate) fn send_to_pub(&self, pubkey: &[u8; 32], pkt: &[u8]) -> ExitSend {
        if !self.is_bound(pubkey) {
            return ExitSend::Unbound;
        }
        match self.out_tx.try_send(Outbound { pubkey: *pubkey, pkt: pkt.to_vec() }) {
            Ok(()) => ExitSend::Handled,
            Err(mpsc::error::TrySendError::Full(_)) => {
                self.note_drop(DropKind::SendBufferFull, "出站队列满（8192 条）");
                ExitSend::Handled
            }
            // 出口面已收工：报 Unbound（无第二承载可落——调用方丢 + 计数）
            Err(mpsc::error::TrySendError::Closed(_)) => ExitSend::Unbound,
        }
    }

    /// 登记绑定（§1.3/§2.2）：**同 devTag 后到者替换并关闭旧连接**；同公钥换 devTag 亦替换。
    /// 成功即打 E-q2 采纳行（`quic: 连接采纳 dev=… tun=… ← …`）——**只在首次准入**打：
    /// 刷新成功不重绑、不打本行（设计门 r14 F11 ⇒ §1.8）。
    ///
    /// 「连接 = 设备」闭包（r14 F15）：同一 `conn_id` 上的旧绑定（若有）**先整条摘除**
    /// ——一条连接只许对应一个 dev；协议面已由「已绑定连接拒 `H4/P4`」挡住二次绑定，
    /// 这里是不变量的第二道（结构性）保证。
    pub(crate) fn bind(
        &self,
        dev: [u8; 8],
        pubkey: [u8; 32],
        conn_id: u64,
        conn: Connection,
        tunnel_ip: Ipv4Addr,
        tun_ip: Ipv4Addr,
    ) {
        let mut b = lock_unpoison(&self.bindings);
        let mut replaced = false;
        // ① 同连接号旧绑定：整条摘（防 by_conn 覆盖 + 旧 dev 索引残留）
        if let Some(prev_dev) = b.by_conn.remove(&conn_id) {
            if let Some(prev) = b.by_dev.remove(&prev_dev) {
                b.by_pub.remove(&prev.pubkey);
            }
        }
        // ② 同 devTag 后到者替换；同公钥换 devTag（克隆场景）也替换——防同键两连接并存
        let mut old = b.by_dev.remove(&dev);
        if old.is_none() {
            if let Some(d) = b.by_pub.get(&pubkey).copied() {
                old = b.by_dev.remove(&d);
            }
        }
        if let Some(old) = old {
            b.by_pub.remove(&old.pubkey);
            b.by_conn.remove(&old.conn_id);
            if old.conn_id != conn_id {
                // 现任裁决（专2-2）：旧连接**显式关闭**——回程不得再发往被丢弃的连接
                old.conn.close(VarInt::from_u32(0), b"replaced by newer registration");
                replaced = true;
            }
        }
        b.by_pub.insert(pubkey, dev);
        b.by_conn.insert(conn_id, dev);
        let remote = conn.remote_address();
        b.by_dev.insert(
            dev,
            Binding { dev, pubkey, conn_id, conn, tunnel_ip, tun_ip },
        );
        debug_assert!(b.is_consistent(), "绑定表三索引不一致（bind 后）");
        drop(b);
        if replaced {
            (self.logf)(&format!("quic: 替换旧连接（dev={}；旧连接已 CONNECTION_CLOSE）", dev_short(&dev)));
        }
        (self.logf)(&format!(
            "quic: 连接采纳 dev={} tun={} ← {}",
            dev_short(&dev),
            tun_ip,
            remote
        ));
    }

    /// 按连接号查绑定（数据报任务的源校验面 + 刷新帧的身份比对面）。已被替换的旧连接
    /// ⇒ `None`（其残余数据报按未登记丢）。
    pub(crate) fn binding_of_conn(&self, conn_id: u64) -> Option<Bound> {
        let b = lock_unpoison(&self.bindings);
        let dev = *b.by_conn.get(&conn_id)?;
        let binding = b.by_dev.get(&dev)?;
        if binding.conn_id != conn_id {
            return None;
        }
        Some(Bound {
            dev: binding.dev,
            pubkey: binding.pubkey,
            tunnel_ip: binding.tunnel_ip,
            tun_ip: binding.tun_ip,
        })
    }

    /// 已绑定设备数（`在途未认证 = 存活连接 − 已绑定` 的读面；设计 §1.6 的挑战行）。
    pub(crate) fn bound_count(&self) -> usize {
        lock_unpoison(&self.bindings).by_dev.len()
    }

    /// 三索引一致性断言（测试面；产品路径上的同一条不变量由 `bind`/`unbind_*` 内的
    /// `debug_assert!` 兜住）。
    #[cfg(test)]
    pub(crate) fn assert_bindings_consistent(&self) {
        assert!(
            lock_unpoison(&self.bindings).is_consistent(),
            "绑定表三索引不一致（by_dev/by_pub/by_conn 必须逐条互指）"
        );
    }

    /// 连接号 → devTag（路径变更行带 dev 用）。
    pub(crate) fn dev_of_conn(&self, conn_id: u64) -> Option<[u8; 8]> {
        lock_unpoison(&self.bindings).by_conn.get(&conn_id).copied()
    }

    /// 是否已有该公钥的绑定（出站分流键：无 ⇒ 调用方走 WG 原样）。
    pub(crate) fn is_bound(&self, pubkey: &[u8; 32]) -> bool {
        lock_unpoison(&self.bindings).by_pub.contains_key(pubkey)
    }

    /// 按公钥取连接句柄（出口面发 DATAGRAM 用）。
    pub(crate) fn conn_of_pub(&self, pubkey: &[u8; 32]) -> Option<Connection> {
        let b = lock_unpoison(&self.bindings);
        let dev = *b.by_pub.get(pubkey)?;
        b.by_dev.get(&dev).map(|x| x.conn.clone())
    }

    /// 摘一条连接号的绑定（连接死/被替换；只摘仍是「当前绑定」的那条）。
    pub(crate) fn unbind_conn(&self, conn_id: u64) {
        let mut b = lock_unpoison(&self.bindings);
        let Some(dev) = b.by_conn.remove(&conn_id) else { return };
        if b.by_dev.get(&dev).map(|x| x.conn_id) == Some(conn_id) {
            let Some(old) = b.by_dev.remove(&dev) else { return };
            b.by_pub.remove(&old.pubkey);
        }
        debug_assert!(b.is_consistent(), "绑定表三索引不一致（unbind_conn 后）");
    }

    /// 按公钥摘绑定并**关闭连接**（设备被摘除/身份轮换的 WG 侧路径，§1.3 撤销/轮换的
    /// 最小对齐）。
    pub(crate) fn unbind_pub(&self, pubkey: &[u8; 32]) {
        let mut b = lock_unpoison(&self.bindings);
        let Some(dev) = b.by_pub.remove(pubkey) else { return };
        if let Some(old) = b.by_dev.remove(&dev) {
            b.by_conn.remove(&old.conn_id);
            old.conn.close(VarInt::from_u32(0), b"device removed");
            debug_assert!(b.is_consistent(), "绑定表三索引不一致（unbind_pub 后）");
            drop(b);
            (self.logf)(&format!("quic: 拆连接（dev={} 已从设备表摘除/轮换）", dev_short(&dev)));
        }
    }

    /// 丢弃计数 + 节流记行（E-q3 形态：四字段序固定；节流照仓内「首 3 + 每 100」）。
    pub(crate) fn note_drop(&self, kind: DropKind, detail: &str) {
        let n = kind.bump(&self.stats);
        if log_due(n) {
            let s = self.stats.snapshot();
            (self.logf)(&format!(
                "quic: 丢弃 超限={} 发送缓冲满={} 未登记={} 源校验拒={}（本次：{} {}；计数行首 3 + 每 100）",
                s.drop_too_large,
                s.drop_send_buffer_full,
                s.drop_unregistered,
                s.drop_src_rejected,
                kind.text(),
                detail
            ));
        }
    }
}

/// devTag 短指纹（4B hex——与 `table.rs` 的 `dev_short` 同形）。
pub(crate) fn dev_short(d: &[u8; 8]) -> String {
    d.iter().take(4).map(|b| format!("{b:02x}")).collect()
}
