//! 客户端侧的准入面（M2 设计 §1.2/§1.7）：`hr-reg4` **四帧**（Hello→Challenge→Proof→Accept）
//! 与 60s 刷新帧（`R4`）。
//!
//! 字节层真源 = [`crate::reg4`]（出口面的 MAC 校验与这里的组帧**共用同一段 HMAC 输入**
//! ——两侧各写一份标签顺序就会静默错位，故不重写）。
//!
//! **M1 的 400ms 登记经验窗已退役**（设计 §1.7 / 设计门 r14 F11）：它存在的原因是「写一帧后
//! 无法知道出口何时完成裁决」⇒ 只能睡一个经验窗；四帧流程给出**确定性回执**（`A4` 在
//! 出口 `bridge.bind` 之后立刻写出）⇒ 用「等 `A4`」替代经验窗。代价对比（算术）：LAN
//! RTT 1ms ⇒ 两往返 ≈ 2ms ≪ 400ms；LTE RTT 50ms ⇒ ≈100ms < 400ms ⇒ 常态更快。
//!
//! 连接绑定：`exporter32 = Connection::export_keying_material(b"hw-quic-reg", b"")`
//! **在本连接上现算**并混进 MAC ⇒ 同帧换连接（重放）验不过。故 exporter 取一次、随连接
//! 存活复用（它是连接级不变量）。
//!
//! **本切片的边界（S1）**：这里是「与新出口同协议」所需的最小实现——准入等待**没有**
//! 岛内期限（照 M1 现状：唯一外层界是宿主 RPC 超时）。准入预算（`max(剩余, ADMIT_MIN)`）
//! 与显式关连接 = S2-3 的范围。

use std::net::SocketAddrV4;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::io::AsyncWriteExt;

use crate::cmd::{IslandErr, Logf};
use crate::config::IslandCredential;
use crate::reg4::{self, AcceptFrame, ChallengeFrame, HelloFrame, ProofFrame, RefreshFrame};

/// 秒级时间戳（出口按 ±90s 窗口校验；客户端只管打新鲜 ts——窗口语义在出口侧）。
pub(crate) fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 在本连接上取 TLS exporter（连接绑定值）。
pub(crate) fn exporter_of(conn: &quinn::Connection) -> Result<[u8; reg4::EXPORTER_LEN], IslandErr> {
    let mut exporter = [0u8; reg4::EXPORTER_LEN];
    conn.export_keying_material(&mut exporter, reg4::EXPORTER_LABEL, b"")
        .map_err(|_| IslandErr::ConnectionLost)?;
    Ok(exporter)
}

/// 刷新节拍（设计 §1.8：60s = 既有巡检节拍）。
///
/// 纯定时语义（无 IO）⇒ 可用 `tokio::time` + `start_paused` 的虚拟时钟确定性单测
/// （设计 §10 S2-6 点名的形态；实现见 `client::tests`）。
#[derive(Debug)]
pub(crate) struct RefreshTimer {
    next: tokio::time::Instant,
    patrol: Duration,
}

impl RefreshTimer {
    pub(crate) fn new(patrol: Duration) -> Self {
        Self {
            next: tokio::time::Instant::now() + patrol,
            patrol,
        }
    }

    /// 到点则返 `true` 并推进下一拍（未到点不动）。
    pub(crate) fn due(&mut self, now: tokio::time::Instant) -> bool {
        if now < self.next {
            return false;
        }
        self.next = now + self.patrol;
        true
    }
}

/// 写一帧刷新（`R4`）+ 打 C15' 行（**行文不变**——M1 已登记；本函数是它的唯一写入点）。
pub(crate) async fn write_refresh<W: AsyncWriteExt + Unpin>(
    sink: &mut W,
    cred: &IslandCredential,
    exporter: &[u8; reg4::EXPORTER_LEN],
    logf: &Logf,
    ep: SocketAddrV4,
    relay: bool,
) -> Result<(), IslandErr> {
    let (secret, pubkey, dev_tag) = cred.parts();
    let frame = RefreshFrame::encode(secret, pubkey, dev_tag, now_unix(), exporter);
    sink.write_all(&frame).await.map_err(|_| IslandErr::ConnectionLost)?;
    (*logf)(&format!(
        "quic: 注册刷新 → {}（dev={}，中继={relay}）",
        ep,
        cred.dev_short()
    ));
    Ok(())
}

/// 首条 bidi 控制流准入（设计 §1.7）：开流 → 写 Hello → 等 Challenge → 写 Proof → 等 `A4`
/// → 交回**打开着的**两半边（发送半边供 60s 刷新；提前 drop 会给出口发 RESET_STREAM）。
///
/// 失败面（出口拒/帧不符）⇒ [`IslandErr::RegistrationFailed`]——客户端侧与「链路断」在
/// 传输层不可区分（出口侧归因行才是排障入口，设计 §1.6）。
pub(crate) async fn register_on_control_stream(
    conn: &quinn::Connection,
    cred: &IslandCredential,
    exporter: &[u8; reg4::EXPORTER_LEN],
    logf: &Logf,
) -> Result<(quinn::SendStream, quinn::RecvStream), IslandErr> {
    let t0 = std::time::Instant::now();
    let (mut send, mut recv) = conn.open_bi().await.map_err(|_| IslandErr::ConnectionLost)?;
    let (secret, pubkey, dev_tag) = cred.parts();
    let ts = now_unix();

    // ① Hello（50B）
    send.write_all(&HelloFrame::encode(pubkey, dev_tag, ts))
        .await
        .map_err(|_| IslandErr::ConnectionLost)?;
    (*logf)(&format!(
        "quic: 准入已发起（dev={}，Hello {}B；等挑战/回执）",
        cred.dev_short(),
        reg4::HELLO_LEN
    ));

    // ② Challenge（18B）——期间的失败 = 出口拒（关连接）或链路断
    let mut ch = [0u8; reg4::CHALLENGE_LEN];
    read_exact_or_registration_failed(conn, &mut recv, &mut ch).await?;
    let challenge = ChallengeFrame::parse(&ch).ok_or(IslandErr::RegistrationFailed)?;

    // ③ Proof（82B；回显 Hello 四字段 + nonce）
    send.write_all(&ProofFrame::encode(
        secret,
        pubkey,
        dev_tag,
        ts,
        &challenge.nonce,
        exporter,
    ))
    .await
    .map_err(|_| IslandErr::ConnectionLost)?;

    // ④ Accept（2B）——**绑定建立之后**由出口立刻写出（确定性回执）
    let mut acc = [0u8; reg4::ACCEPT_LEN];
    read_exact_or_registration_failed(conn, &mut recv, &mut acc).await?;
    if AcceptFrame::parse(&acc).is_none() {
        return Err(IslandErr::RegistrationFailed);
    }
    (*logf)(&format!(
        "quic: 准入完成（dev={}，耗时 {}）",
        cred.dev_short(),
        super::fmt_dur(t0.elapsed())
    ));
    Ok((send, recv))
}

/// 有界读一帧的回程字节：失败时按「连接是否已被对端关闭」分流归因
/// （被出口拒绝 ⇒ `RegistrationFailed`；否则链路断 ⇒ `ConnectionLost`）。
async fn read_exact_or_registration_failed(
    conn: &quinn::Connection,
    recv: &mut quinn::RecvStream,
    buf: &mut [u8],
) -> Result<(), IslandErr> {
    match recv.read_exact(buf).await {
        Ok(_) => Ok(()),
        Err(_) if conn.close_reason().is_some() => Err(IslandErr::RegistrationFailed),
        Err(_) => Err(IslandErr::ConnectionLost),
    }
}
