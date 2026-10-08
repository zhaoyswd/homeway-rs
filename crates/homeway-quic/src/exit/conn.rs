//! 每连接两枚任务（出口面 · `current_thread` runtime 内）：
//!
//! - [`control`]：首条 bidi 控制流 = `hr-reg3` 帧（准入 + 60s 刷新），走引擎裁决后登记绑定；
//! - [`datagrams`]：DATAGRAM 收包 → 绑定查表 → **源校验** → 入队 + 唤醒（§1.4）。
//!
//! 发送面（[`send_datagram_checked`]）是本 crate **唯一**的 `Connection::send_datagram`
//! 调用点：裸调用在缓冲满时会静默淘汰最旧且恒返 Ok（M1 设计 §0.3 P4 实测），故全部
//! 出口包走「预检 + 分类计数 + 才发」。
//!
//! 单线程前提（§6.4）：本 crate 的 runtime 是 `current_thread`，且预检与发送之间**无
//! `await`** ⇒ 同线程上不存在「预检通过后、发送前」被他处插入的空间竞争（这也是
//! `datagram_send_buffer_space()` 两次加锁读可用的前提）。

use std::net::Ipv4Addr;
use std::sync::Arc;

use bytes::Bytes;
use quinn::{Connection, SendDatagramError, VarInt};
use tokio::sync::oneshot;

use crate::reg3::{self, Reg3Frame};

use super::bridge::{DropKind, ExitInbound, Reg3Request, Reg3Verdict};
use super::{FaceCtx, log_due};

/// 发送结果（测试面：`Dropped` 与计数一一对应）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum SendOutcome {
    Sent,
    Dropped,
}

/// 入站内层包的源校验（**复刻** `homeway-core` 的 `server/device.rs` 的 `src_allowed`，
/// M1 设计 §1.3/§1.4）：非 IPv4（短包或版本 ≠ 4）或 `src ∉ {tunnel_ip, tun_ip}` ⇒ 拒。
///
/// 为什么是复刻而不是调用：QUIC 入站**不经过** `device.rs`（本 crate 是叶子，不得依赖
/// `homeway-core`）；行为必须逐字同（拒绝计数对应今日的 `src_rejects`）。
fn src_allowed(pkt: &[u8], tunnel_ip: Ipv4Addr, tun_ip: Ipv4Addr) -> bool {
    if pkt.len() < 20 || pkt[0] >> 4 != 4 {
        return false;
    }
    let src = Ipv4Addr::new(pkt[12], pkt[13], pkt[14], pkt[15]);
    src == tunnel_ip || src == tun_ip
}

/// 出站内层包 → DATAGRAM：预检（`max_datagram_size` / 发送缓冲空余）⇒ 分类计数 ⇒ 才发。
pub(crate) fn send_datagram_checked(conn: &Connection, pkt: Vec<u8>, ctx: &FaceCtx) -> SendOutcome {
    match conn.max_datagram_size() {
        // 连接未就绪（对端尚未确认 DATAGRAM 参数）⇒ 归 `未登记`（§6.4 的映射表）
        None => {
            ctx.bridge
                .note_drop(DropKind::Unregistered, "连接未就绪（max_datagram_size 未知）");
            return SendOutcome::Dropped;
        }
        Some(mds) if pkt.len() > mds => {
            ctx.bridge.note_drop(
                DropKind::TooLarge,
                &format!("包 {}B > max_datagram_size {mds}B", pkt.len()),
            );
            return SendOutcome::Dropped;
        }
        Some(_) => {}
    }
    let space = conn.datagram_send_buffer_space();
    if space < pkt.len() {
        // 裸 `send_datagram` 在这个点会静默淘汰最旧并返 Ok（P4）——这里丢 + 计数（不静默）
        ctx.bridge.note_drop(
            DropKind::SendBufferFull,
            &format!("缓冲空余 {space}B < 包 {}B（不静默：丢 + 计数）", pkt.len()),
        );
        return SendOutcome::Dropped;
    }
    match conn.send_datagram(Bytes::from(pkt)) {
        Ok(()) => SendOutcome::Sent,
        Err(SendDatagramError::TooLarge) => {
            ctx.bridge
                .note_drop(DropKind::TooLarge, "quinn TooLarge（本地单报文上限）");
            SendOutcome::Dropped
        }
        // UnsupportedByPeer / Disabled / ConnectionLost ⇒ `未登记`（§6.4 按变体映射）
        Err(e) => {
            ctx.bridge.note_drop(DropKind::Unregistered, &format!("发送失败（{e}）"));
            SendOutcome::Dropped
        }
    }
}

/// 控制流任务：首条 bidi 流 = `hr-reg3` 帧（一次准入 + 之后每帧刷新）。
///
/// 帧是**定长**的（[`reg3::LEN`]）：控制流上按定长切帧，逐帧走引擎裁决。任何一帧被拒
/// ⇒ 关连接（准入是连接级的，拒绝后不给「同连接再试」的口子）。
pub(crate) async fn control(conn: Connection, conn_id: u64, ctx: Arc<FaceCtx>) {
    let Ok((send, mut recv)) = conn.accept_bi().await else {
        return; // 连接在开流前就没了
    };
    // 发送半边保持打开（不写、不 reset）：S2 客户端的刷新帧只用发送方向；提前 drop
    // 会发 RESET_STREAM 给对端（无谓的噪声）。
    let _keepalive_send = send;
    let mut buf = [0u8; reg3::LEN];
    loop {
        if recv.read_exact(&mut buf).await.is_err() {
            return; // 对端关流/连接死
        }
        let Some(frame) = Reg3Frame::parse(&buf) else {
            // 不可解的帧没有可信的 devTag：记行用全零位（不假装知道设备身份）
            reject(&ctx, &conn, &[0u8; 8], "帧格式非法（魔数/长度）");
            return;
        };
        let mut exporter = [0u8; reg3::EXPORTER_LEN];
        if conn
            .export_keying_material(&mut exporter, reg3::EXPORTER_LABEL, b"")
            .is_err()
        {
            // TLS exporter 取不到 = 该连接不是可绑定形态（理论上 TLS1.3 后必可得）
            reject(&ctx, &conn, &frame.dev_tag, "TLS exporter 不可得");
            return;
        }
        let (reply_tx, reply_rx) = oneshot::channel();
        if !ctx.bridge.push_inbound(ExitInbound::Reg(Reg3Request::new(
            frame,
            exporter,
            reply_tx,
        ))) {
            ctx.bridge
                .note_drop(DropKind::Unregistered, "准入请求（入境队列满 8192 条）");
            conn.close(VarInt::from_u32(0), b"inbound queue full");
            return;
        }
        match reply_rx.await {
            Ok(Reg3Verdict::Accepted { tunnel_ip, tun_ip }) => {
                ctx.stats.regs_accepted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                ctx.bridge.bind(
                    frame.dev_tag,
                    frame.pubkey,
                    conn_id,
                    conn.clone(),
                    tunnel_ip,
                    tun_ip,
                );
            }
            Ok(Reg3Verdict::Rejected) => {
                reject(&ctx, &conn, &frame.dev_tag, "引擎裁决拒绝（见引擎侧归因行）");
                return;
            }
            Err(_) => return, // 引擎侧未回执（收工）：连接留给收工链
        }
    }
}

/// 关连接 + 计数 + 记行（准入拒绝的唯一出口）。
fn reject(ctx: &FaceCtx, conn: &Connection, dev: &[u8; 8], why: &str) {
    let n = ctx
        .stats
        .regs_rejected
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        + 1;
    if log_due(n) {
        (ctx.logf)(&format!(
            "quic: 准入被拒（dev={} ← {}；{why}；第 {n} 次）",
            super::bridge::dev_short(dev),
            conn.remote_address()
        ));
    }
    conn.close(VarInt::from_u32(0), b"registration rejected");
}

/// 数据报任务：`read_datagram` → 绑定查表 → 源校验 → 入队 + 唤醒。
pub(crate) async fn datagrams(conn: Connection, conn_id: u64, ctx: Arc<FaceCtx>) {
    loop {
        let dg = match conn.read_datagram().await {
            Ok(d) => d,
            Err(_) => return, // 连接死/被替换
        };
        // 绑定查表（§1.3）：未登记连接的数据报**直接丢 + 计数**（含被替换的旧连接）
        let Some((dev, tunnel_ip, tun_ip)) = ctx.bridge.binding_of_conn(conn_id) else {
            ctx.bridge
                .note_drop(DropKind::Unregistered, "未登记连接的数据报");
            continue;
        };
        // 源校验（复刻 `device.rs` 的 `src_allowed`；§1.3/§1.4）
        if !src_allowed(&dg, tunnel_ip, tun_ip) {
            ctx.bridge.note_drop(
                DropKind::SrcRejected,
                &format!("src ∉ {{tunnel_ip,tun_ip}}（dev={}）", super::bridge::dev_short(&dev)),
            );
            continue;
        }
        if !ctx
            .bridge
            .push_inbound(ExitInbound::Packet { dev, pkt: dg.to_vec() })
        {
            ctx.bridge.note_drop(DropKind::Unregistered, "入境队列满（8192 条）");
        }
    }
}
