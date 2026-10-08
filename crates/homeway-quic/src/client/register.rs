//! 客户端侧的登记面（设计 §1.3/§2.6）：`hr-reg3` 组帧 + 准入窗 + 刷新帧。
//!
//! 字节层真源 = [`crate::reg3`]（出口面的 MAC 校验与这里的组帧**共用同一段 HMAC 输入**
//! ——两侧各写一份标签顺序就会静默错位，故不重写）。
//!
//! 连接绑定（§1.3）：`exporter32 = Connection::export_keying_material(b"hw-quic-reg", b"")`
//! **在本连接上现算**并混进 MAC ⇒ 同帧换连接（重放）验不过。故 exporter 取一次、随连接
//! 存活复用（它是连接级不变量）。
//!
//! 准入窗（S1c 的实测锚点，`tools/quic-probe` 的 `--reg-wait` 缺省）：登记帧写出后出口
//! 还要走「引擎裁决 → 回执 → 绑定」三步，**立刻发数据报会被判「未登记连接的数据报」丢掉**
//! ⇒ 必须等 [`REG_SETTLE`] 再放行数据面（数据面接线 = S2-4；本模块只把窗定死）。

use std::net::SocketAddrV4;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::io::AsyncWriteExt;

use crate::cmd::{IslandErr, Logf};
use crate::config::IslandCredential;
use crate::reg3::{self, Reg3Frame};

/// 登记后的准入窗（= `tools/quic-probe --reg-wait` 的缺省 400ms，S1c 实测锚点）。
pub(crate) const REG_SETTLE: Duration = Duration::from_millis(400);

/// 秒级时间戳（出口按 ±90s 窗口校验；客户端只管打新鲜 ts——窗口语义在出口侧）。
pub(crate) fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 在本连接上取 TLS exporter（连接绑定值）。
pub(crate) fn exporter_of(conn: &quinn::Connection) -> Result<[u8; reg3::EXPORTER_LEN], IslandErr> {
    let mut exporter = [0u8; reg3::EXPORTER_LEN];
    conn.export_keying_material(&mut exporter, reg3::EXPORTER_LABEL, b"")
        .map_err(|_| IslandErr::ConnectionLost)?;
    Ok(exporter)
}

/// 组一帧（ts 现取）。
pub(crate) fn frame_now(
    cred: &IslandCredential,
    exporter: &[u8; reg3::EXPORTER_LEN],
) -> [u8; reg3::LEN] {
    let (secret, pubkey, dev_tag) = cred.parts();
    Reg3Frame::encode(secret, pubkey, dev_tag, now_unix(), exporter)
}

/// 刷新节拍（设计 §2.6：60s = 既有巡检节拍）。
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

/// 写一帧到控制流写半边 + 打 C15' 行（登记与刷新的**唯一**写入点：格式串只此一份）。
pub(crate) async fn write_frame<W: AsyncWriteExt + Unpin>(
    sink: &mut W,
    cred: &IslandCredential,
    exporter: &[u8; reg3::EXPORTER_LEN],
    logf: &Logf,
    ep: SocketAddrV4,
    relay: bool,
    first: bool,
) -> Result<(), IslandErr> {
    let frame = frame_now(cred, exporter);
    sink.write_all(&frame).await.map_err(|_| IslandErr::ConnectionLost)?;
    if first {
        (*logf)(&format!(
            "quic: 登记已发（dev={}，{}B；等准入窗 {}）",
            cred.dev_short(),
            reg3::LEN,
            super::fmt_dur(REG_SETTLE)
        ));
    } else {
        (*logf)(&format!(
            "quic: 注册刷新 → {}（dev={}，中继={relay}）",
            ep,
            cred.dev_short()
        ));
    }
    Ok(())
}

/// 首条 bidi 控制流登记（设计 §2.6）：开流 → 写首帧 → 等准入窗 → 交回**打开着的**
/// 两半边（发送半边供 60s 刷新；提前 drop 会给出口发 RESET_STREAM）。
///
/// 窗内连接被关（出口拒绝：坏 MAC/表满/吊销/帧非法）⇒ [`IslandErr::RegistrationFailed`]
/// ——客户端侧与「链路断」不可区分，出口侧归因行在出口日志。
pub(crate) async fn register_on_control_stream(
    conn: &quinn::Connection,
    cred: &IslandCredential,
    exporter: &[u8; reg3::EXPORTER_LEN],
    logf: &Logf,
    ep: SocketAddrV4,
    relay: bool,
) -> Result<(quinn::SendStream, quinn::RecvStream), IslandErr> {
    let (mut send, recv) = conn.open_bi().await.map_err(|_| IslandErr::ConnectionLost)?;
    write_frame(&mut send, cred, exporter, logf, ep, relay, true).await?;
    tokio::time::sleep(REG_SETTLE).await;
    if conn.close_reason().is_some() {
        return Err(IslandErr::RegistrationFailed);
    }
    Ok((send, recv))
}
