//! 出口侧 **dial 腿**（M4 S1；设计 `docs/reviews/M4-design.md` §1/§3）。
//!
//! 干什么：`STREAM[tag=dial]`（tag=4）的**出口面**——读 6B 目标帧 ⇒ 地址类判定
//! （[`DialAddrClass`]，A8 定稿 §1.4）⇒ `TcpStream::connect`（预算 [`DIAL_BUDGET`]）
//! ⇒ **写 1B 拨号回执**（[`crate::stream::DIAL_OK`]，§1.2）⇒ 复用 [`super::pump`] 的
//! 泛型泵做双向搬运。
//!
//! 协议面（写死，逐条对着设计表）：
//!
//! ```text
//! 客户端→出口：tag(0x04) ‖ [4B IPv4][2B BE port] ‖ 目标侧原始字节流（帧后即裸字节）
//! 出口→客户端：DIAL_OK(0x01) ‖ 目标侧原始字节流        ← M4 的唯一 wire 增量
//!              失败路径**不写回执**，改为 reset(0x25 拒 / 0x26 超期)
//! ```
//!
//! **为什么直接拨 OS（不走 intercept，§3.1）**：`STREAM[dial]` 的语义 = 「**出口**去连这个
//! 地址」，出口的 OS 网络栈就是「出口可达」的权威定义；intercept 是**客户端 TCP 的用户态
//! 终结/NAT**（它要伪造客户端源地址 + 建 NAT 表项），把 dial 塞进去是纯增复杂度。
//! 附带好处：M5 收窄 intercept（只留 DATAGRAM 过境 + DNS）时**本文件零改动**。
//!
//! **同源不变量（§3.1，r19 M-[中]）**：dial 腿与 intercept 的 transit 腿**当前都是
//! 「无目的地策略的直拨」**——这是两者等价的前提。**若将来（含 M5）给 transit 腿引入目的地
//! 策略（拦 metadata / 私网 / ACL），dial 腿必须同批同步**（策略单源或同批登记），
//! **不得**让 dial 腿静默成为该策略的旁路。M4 只写这条不变量，不实现策略。
//!
//! **A8 判定表（§1.4）的三类**：允许（回环/私网/CGNAT/出口自己的 LAN IP/公网单播/预留段/
//! fake-IP 段，一律透传 OS）/ 拒（未指定 `0.0.0.0`、本网络 `0/8`、受限广播、组播、端口 0）
//! / 不单独判（子网广播、`240/4`、`198.18/15`——归因交 OS）。**出口侧不得拒环回**
//! （§1.3-A1②：拒了会打死「出口本机」这一合法目标）。
//!
//! **与 `probe_addr_acceptable` 的分面关系（§1.4-2，禁止混用）**：那个卫兵面对的是
//! 「客户端要连的**出口/中继端点**」（准入前的信任面，拒 private/回环/link-local/fake-IP/
//! CGNAT）；本表面对的是「**已准入设备**的出口出站目标」。两者取值集**必然不同**
//! （本表接受私网/回环），复用任一方的判定即打死 spec 场景②。**不得互相引用**。
//!
//! **本文件属异步面**（隔离门 ② 条的 `ASYNC_FILES` 显式清单；照 `exit/serve.rs` 先例）。

use std::net::{Ipv4Addr, SocketAddrV4};
use std::sync::Arc;
use std::time::{Duration, Instant};

use quinn::{RecvStream, ReadExactError, SendStream};
use tokio::net::TcpStream;

use crate::stream::{dial_parse, reset, DIAL_OK, StreamTag, TAG_READ_BUDGET};

use super::serve::{dev_of, refuse, varint};
use super::{log_due, FaceCtx};

/// 出口侧**拨号预算**（§3.2）：与 WG 档出口侧 `intercept` 的 `dialTimeout`（10s）**同值**
/// ——不是新发明。缺省客户端预算 15s > 10s ⇒ 正常形态下 `0x26` 由**出口先给**（精确归因）。
///
/// 客户端预算更短（脏配置 `dialMs < 10000`）时客户端先收线，出口的在途 dial 最多再活到
/// 自身期限（设计 §10-W3；实测锚点 §12-C2：客户端 600ms 放弃、出口 10.0017s 才收）。
pub(crate) const DIAL_BUDGET: Duration = Duration::from_secs(10);

/// 地址接受集（A8 定稿 §1.4）的**拒入类**。
///
/// 落 typed enum + [`DialAddrClass::text`]（照 `TableErr`/`StreamErr` 的**单源**先例）：
/// 拒行的 `%s` 由 `text()` 产出，**不得**散在 `format!` 里（防「行文与码各写一份」的漂移）。
///
/// 取值集只收「**平台语义歧义或归因不可读**」的类（§1.4-4：这不是「缩小 SSRF 面」的机制，
/// 而是消灭歧义；能发的对端**早已**拥有经出口的 L3 全局代理 ⇒ 收紧不增加安全性）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum DialAddrClass {
    /// `0.0.0.0`：**平台陷阱**——macOS 实测 `connect(0.0.0.0:port)` = **连本机**（§12-P1），
    /// 静默错位；客户端已归一为出口回环（§1.3）⇒ 出口侧拒 = 防御面（防手写/异常对端把它
    /// 当「随机本机端口」用）。
    Unspecified,
    /// `0.0.0.0/8` 的其余地址（`0.1.2.3` 等）：RFC 1122 保留，平台归一不一（实测
    /// `HostUnreachable`）⇒ 归因不可靠，直拒更诚实。
    ThisNetwork,
    /// `255.255.255.255`：TCP connect 无意义（实测 `EAFNOSUPPORT(47)`）⇒ 拒使归因可读。
    Broadcast,
    /// `224.0.0.0/4`：同受限广播（`224.0.0.1`/`239.1.1.1` 实测同码）。
    Multicast,
    /// **端口 0**：没有「连接」语义（`EINVAL`/`EADDRNOTAVAIL` 平台不一）；旁路配置形态
    /// （`targetPort=0`）在客户端经 `dial_target()` 折叠后线上就是 `127.0.0.1:0`（§1.1）。
    PortZero,
}

impl DialAddrClass {
    /// 判据行 `%s`（§1.4 的 why 串，单源；`0x25` 拒行里出现的就是它）。
    pub(crate) const fn text(self) -> &'static str {
        match self {
            DialAddrClass::Unspecified => "未指定 0.0.0.0",
            DialAddrClass::ThisNetwork => "本网络 0/8",
            DialAddrClass::Broadcast => "受限广播 255.255.255.255",
            DialAddrClass::Multicast => "组播 224/4",
            DialAddrClass::PortZero => "端口 0 不是可拨端口",
        }
    }

    /// 判定（§1.4 表的**拒入行**）；`None` = 允许 / 不单独判（两者都透传 OS，出口不再分）。
    ///
    /// 判定顺序（写死；写成一条语句是为了让「哪一类优先」成为可读事实）：**地址类先于
    /// 端口 0**——`0.0.0.0:0` 报地址类（信息量更大），`127.0.0.1:0` 报端口 0（§1.4 行 11）。
    pub(crate) fn classify(dst: SocketAddrV4) -> Option<DialAddrClass> {
        let ip = *dst.ip();
        if ip.is_unspecified() {
            return Some(DialAddrClass::Unspecified);
        }
        if ip == Ipv4Addr::BROADCAST {
            return Some(DialAddrClass::Broadcast);
        }
        if ip.is_multicast() {
            return Some(DialAddrClass::Multicast);
        }
        if ip.octets()[0] == 0 {
            return Some(DialAddrClass::ThisNetwork);
        }
        if dst.port() == 0 {
            return Some(DialAddrClass::PortZero);
        }
        None
    }
}

/// `STREAM[tag=dial]` 的出口面（§3.2 的七步；tag 已由 [`super::serve::handle_stream`] 读出）。
pub(crate) async fn dial_serve(
    mut send: SendStream,
    mut recv: RecvStream,
    conn_id: u64,
    ctx: &Arc<FaceCtx>,
) {
    // ---- ② 读 6B 目标帧（预算 = TAG_READ_BUDGET，M3 现值；读不出/畸形 ⇒ 0x25 + 拒行）----
    let mut frame = [0u8; 6];
    let dst = match tokio::time::timeout(TAG_READ_BUDGET, recv.read_exact(&mut frame)).await {
        Ok(Ok(())) => match dial_parse(&frame) {
            Some(d) => d,
            // `read_exact(6)` 成功后长度必为 6 ⇒ 本臂不可达（留作帧格式变更的防线）
            None => {
                let _ = send.reset(varint(reset::DIAL_REFUSED));
                refuse(
                    ctx,
                    conn_id,
                    Some(StreamTag::Dial),
                    reset::DIAL_REFUSED,
                    "目标帧畸形（长度非 6B）",
                );
                return;
            }
        },
        Ok(Err(ReadExactError::FinishedEarly(n))) => {
            // 对端只发 tag 就收线（0B/短帧）⇒ 没有目标可拨
            let _ = send.reset(varint(reset::DIAL_REFUSED));
            refuse(
                ctx,
                conn_id,
                Some(StreamTag::Dial),
                reset::DIAL_REFUSED,
                &format!("目标帧未读出（对端提前收线，已得 {n}B）"),
            );
            return;
        }
        Ok(Err(e)) => {
            let _ = send.reset(varint(reset::DIAL_REFUSED));
            refuse(
                ctx,
                conn_id,
                Some(StreamTag::Dial),
                reset::DIAL_REFUSED,
                &format!("目标帧未读出（{e}）"),
            );
            return;
        }
        Err(_elapsed) => {
            let _ = send.reset(varint(reset::DIAL_REFUSED));
            refuse(
                ctx,
                conn_id,
                Some(StreamTag::Dial),
                reset::DIAL_REFUSED,
                &format!("目标帧未读出（预算 {TAG_READ_BUDGET:?} 到点）"),
            );
            return;
        }
    };

    // ---- ③ 地址类判定（必须在 `connect` 之前；§1.4）----
    if let Some(class) = DialAddrClass::classify(dst) {
        let _ = send.reset(varint(reset::DIAL_REFUSED));
        refuse(
            ctx,
            conn_id,
            Some(StreamTag::Dial),
            reset::DIAL_REFUSED,
            &format!("目标地址类不可拨（{dst}：{}）", class.text()),
        );
        return;
    }

    // ---- ④ 拨号（预算 10s；超期 ⇒ 0x26，其余失败 ⇒ 0x25）----
    let tcp = match connect_bounded(DIAL_BUDGET, TcpStream::connect(dst)).await {
        Ok(tcp) => tcp,
        Err(fail) => {
            let code = fail.reset_code();
            let _ = send.reset(varint(code));
            refuse(ctx, conn_id, Some(StreamTag::Dial), code, &fail.why(dst));
            return;
        }
    };

    // ---- ④b. `TCP_NODELAY`（**M4 实施期对齐**；S1–S3 交下的待确认项，S5 实测给结论）----
    //
    // 依据 = **出口侧同源先例**：WG 档的 transit 腿在拨号完成后设 `TCP_NODELAY`（Go
    // `SetDelayOption(false)` 同口径，`server/intercept/mod.rs:2233-2246`）——dial 腿与它同属
    // 「无目的地策略的直拨」，**同一映射在两条承载下的小包时延不得有差异**（Nagle 会把
    // 「请求—响应」型小包多压一个 ACK 往返）。失败不致命（最佳努力，与 intercept 同形）。
    let _ = tcp.set_nodelay(true);

    // ---- ⑤ 回执（**先于任何目标字节**，§1.2）----
    //
    // 写失败（对端已 reset / 连接死）⇒ **立即返回**（`tcp` 随作用域 drop ⇒ 目标连接不成为
    // 活僵尸）；**不得**继续起泵。
    if send.write_all(&[DIAL_OK]).await.is_err() {
        return;
    }

    // ---- ⑥ 受理计数 + 行（E-q5 族；**语义 = 拨号成功**——其余 tag 是「入队即受理」）----
    let n = ctx
        .stats
        .streams_open
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        + 1;
    if log_due(n) {
        (*ctx.logf)(&format!(
            "quic: 服务流已受理（tag={} dev={} 第 {n} 次）",
            StreamTag::Dial.text(),
            dev_of(ctx, conn_id)
        ));
    }

    // ---- ⑦ 泵（复用泛型化后的同一份实现；半关逐向传播）----
    pump_dial(send, recv, tcp, ctx, n).await;
}

/// 拨号失败的**两形态**（§3.2 的精确归因：超期 ⇒ `0x26`；其余 ⇒ `0x25`）。
///
/// 复位码与 why 串都在本件里**单源**产出（[`DialFail::reset_code`]/[`DialFail::why`]）——
/// 拒行文字与码不得各写一份（照 `StreamErr`/`TableErr` 的先例）。
#[derive(Debug)]
enum DialFail {
    /// `connect` 返回错误（含 errno 原文 ⇒ 排障可读）。
    Refused(std::io::Error),
    /// `connect` 未在 [`DIAL_BUDGET`] 内落定（黑洞目标 / SYN 无应答）。
    Timeout,
}

impl DialFail {
    /// 复位码（§3.2；码表零改动——M4 只是让 M3 备好的两码**可达**）。
    const fn reset_code(&self) -> u64 {
        match self {
            DialFail::Refused(_) => reset::DIAL_REFUSED,
            DialFail::Timeout => reset::DIAL_TIMEOUT,
        }
    }

    /// 拒行 why 串（E-q5 族的 `%s`；`%v` = 目标、`%e` = errno 原文）。
    fn why(&self, dst: SocketAddrV4) -> String {
        match self {
            DialFail::Refused(e) => format!("目标拨号失败（{dst}：{e}）"),
            DialFail::Timeout => format!("目标拨号超时（{dst}；预算 {DIAL_BUDGET:?}）"),
        }
    }
}

/// 拨号步骤（有界）：**唯一**把「`connect` 的两种失败」分成两形态的地方（§3.2）。
///
/// `connect` 注入成 future（而不是直接写 `TcpStream::connect(dst)`）的唯一理由 =
/// **可测**：黑洞目标（「SYN 无应答」）在本机是**机器相关**的（§12.1 实测：`169.254.169.254`
/// 在本机 1.5s 给 `TimedOut`，别的机器可能立刻 `EHOSTUNREACH`），拿它当 CI 判据就是把
/// 环境事实伪装成产品事实。生产路径恒传 `TcpStream::connect(dst)`（见 [`dial_serve`]）。
async fn connect_bounded<F>(budget: Duration, connect: F) -> Result<TcpStream, DialFail>
where
    F: std::future::Future<Output = std::io::Result<TcpStream>>,
{
    match tokio::time::timeout(budget, connect).await {
        Ok(Ok(tcp)) => Ok(tcp),
        Ok(Err(e)) => Err(DialFail::Refused(e)),
        Err(_elapsed) => Err(DialFail::Timeout),
    }
}

/// dial 腿的泵段：**复用** [`super::pump`] 的 `upstream`/`downstream`（§3.3：不写第四份泵）。
///
/// 方向口径（与 `pump::run` 一致）：`up` = 客户端 → 目标（= `stream_bytes_in`），
/// `down` = 目标 → 客户端（= `stream_bytes_out`）。**1B 回执不计入字节账**（它写在泵之前；
/// 否则 `↑%dB ↓%dB` 与目标真实字节差 1——§3.3 的口径写死）。
///
/// 目标侧异常结束（§3.3，r19 L5）：目标 RST / 读错误都以 `finish()` 收口（与 M3 现装一致），
/// 而客户端对「白名单外复位码 / FIN」都归 **EOF** ⇒ 客户端侧的 `port-forward[down] 读错误`
/// 行在该形态下会消失 ⇒ **出口侧补一行**（additive）把可观测面回到出口。
async fn pump_dial(
    mut send: SendStream,
    mut recv: RecvStream,
    tcp: TcpStream,
    ctx: &Arc<FaceCtx>,
    n: u64,
) {
    let t0 = Instant::now();
    let (mut tr, mut tw) = tokio::io::split(tcp);
    // **逐方向落行**（不是 `join!` 之后统一落）：两个方向各自独立收摊——某一方向的中断
    // 不能等另一方向收线才可见（客户端可能长时间不再发数据 ⇒ 等它 = 行迟迟不出）。
    let up_f = async {
        let r = super::pump::upstream(&mut recv, &mut tw).await;
        note_dir(ctx, "up", &r);
        r
    };
    let down_f = async {
        let r = super::pump::downstream(&mut tr, &mut send).await;
        note_dir(ctx, "down", &r);
        r
    };
    let (up, down) = tokio::join!(up_f, down_f);
    ctx.stats
        .stream_bytes_in
        .fetch_add(up.bytes, std::sync::atomic::Ordering::SeqCst);
    ctx.stats
        .stream_bytes_out
        .fetch_add(down.bytes, std::sync::atomic::Ordering::SeqCst);
    ctx.stats
        .streams_closed
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    if log_due(n) {
        (*ctx.logf)(&format!(
            "quic: 服务流结束（tag={}，↑{}B ↓{}B，耗时 {:?}）",
            StreamTag::Dial.text(),
            up.bytes,
            down.bytes,
            t0.elapsed()
        ));
    }
}

/// 目标侧中断行（§3.3 的 additive 可观测面）：**只在异常结束时产行**，且**不节流**
/// （节流会把「目标反复自杀」的证据抹掉；每流至多两行，方向各一）。
fn note_dir(ctx: &Arc<FaceCtx>, dir: &str, r: &super::pump::CopyEnd) {
    if let Some(e) = &r.err {
        (*ctx.logf)(&format!(
            "quic: 服务流目标侧中断（tag={}，{dir} 方向：{e}）",
            StreamTag::Dial.text()
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v4(s: &str) -> SocketAddrV4 {
        s.parse().expect("测试地址可解")
    }

    /// **判据（A8 判定表逐行，§1.4）**：拒入类**逐行**（第 5–8、11 类）＋允许/不单独判面
    /// （第 1–4、9、10、12、13 类）逐行留证。
    ///
    /// 这张表是**行为契约**：数值与 `.text()` 都对着设计 §1.4 的原文；改表 = 改协议面。
    #[test]
    fn a8_address_table_row_by_row() {
        // 允许（1–4）与不单独判（9、12、13）：全部 ⇒ `None`（透传 OS）
        for ok in [
            "127.0.0.1:8080",       // 1 回环 = 出口本机（**必须 accept**，§1.3-A1②）
            "127.9.8.7:1",          // 1 环回任意地址
            "192.168.3.12:443",     // 2 出口自己的 LAN IP
            "10.255.255.1:9",       // 3 私网
            "172.16.5.5:80",        // 3 私网
            "100.64.7.9:7802",      // 3 CGNAT（客户端侧哨兵常量；§1.3 裁决 D-1 登记行）
            "169.254.169.254:80",   // 3 链路本地（黑洞也照拨——归因交 OS）
            "8.8.8.8:53",           // 4 公网单播
            "192.168.1.255:80",     // 9 子网广播（需掩码才判得出 ⇒ 不单独判）
            "240.0.0.1:80",         // 12 保留/未来用
            "198.18.0.1:80",        // 13 基准测试段（fake-IP 代理段）
        ] {
            assert_eq!(
                DialAddrClass::classify(v4(ok)),
                None,
                "{ok} 属允许/不单独判类（透传 OS）"
            );
        }
        // 拒入（5–8、11）：逐行给出类与 why 串
        let rows = [
            ("0.0.0.0:8080", DialAddrClass::Unspecified, "未指定 0.0.0.0"),
            ("0.1.2.3:8080", DialAddrClass::ThisNetwork, "本网络 0/8"),
            ("0.255.255.255:1", DialAddrClass::ThisNetwork, "本网络 0/8"),
            (
                "255.255.255.255:80",
                DialAddrClass::Broadcast,
                "受限广播 255.255.255.255",
            ),
            ("224.0.0.1:80", DialAddrClass::Multicast, "组播 224/4"),
            ("239.1.1.1:80", DialAddrClass::Multicast, "组播 224/4"),
            (
                "127.0.0.1:0",
                DialAddrClass::PortZero,
                "端口 0 不是可拨端口",
            ),
            ("8.8.8.8:0", DialAddrClass::PortZero, "端口 0 不是可拨端口"),
        ];
        for (addr, class, text) in rows {
            assert_eq!(DialAddrClass::classify(v4(addr)), Some(class), "{addr} 的类");
            assert_eq!(class.text(), text, "{addr} 的 why 串");
        }
        // 判定顺序（写死的一条事实）：`0.0.0.0:0` 报**地址类**（信息量更大）而非端口 0
        assert_eq!(
            DialAddrClass::classify(v4("0.0.0.0:0")),
            Some(DialAddrClass::Unspecified),
            "地址类先于端口 0"
        );
        // 环回 + 端口 0（= 客户端 `targetPort=0` 旁路形态的线上形态，§1.1）：端口 0 拒
        assert_eq!(
            DialAddrClass::classify(v4("127.0.0.1:0")),
            Some(DialAddrClass::PortZero)
        );
    }

    /// 预算是**设计值**（与 WG 档出口 `intercept` 的 `dialTimeout` 同值 10s；改动即偏离）。
    #[test]
    fn dial_budget_is_the_designed_value() {
        assert_eq!(DIAL_BUDGET, Duration::from_secs(10));
        // 出口预算 < 客户端缺省预算（15s）⇒ 正常形态 0x26 由出口先给（精确归因，§3.2）
        assert!(DIAL_BUDGET < Duration::from_secs(15));
    }

    /// **判据（0x25/0x26 双码 + why 串单源，§3.2 第 ④ 步）**：`connect_bounded` 的两个失败
    /// 形态各归其码；why 串带目标与 errno 原文（`%v`/`%e`）。
    ///
    /// **注入 future 的理由（不是「为了好测」）**：黑洞目标（SYN 无应答）在本机是**机器相关**
    /// 的——设计 §12.1 实测 `169.254.169.254` 在本机 1.5s 给 `TimedOut`，别的机器可能立刻
    /// `EHOSTUNREACH`（那就成了 `0x25`）。把环境事实当 CI 判据 = 假绿/假红两头都占。
    #[tokio::test]
    async fn dial_failures_map_to_the_two_codes_and_why_texts() {
        let dst = v4("127.0.0.1:1");
        // ① `connect` 返回错误 ⇒ 0x25 + errno 原文
        let e = std::io::Error::from_raw_os_error(61); // ECONNREFUSED（本机 errno 值见 §12.1）
        let fail = connect_bounded(Duration::from_secs(1), async { Err(e) })
            .await
            .expect_err("必须归失败");
        assert!(matches!(fail, DialFail::Refused(_)), "{fail:?}");
        assert_eq!(fail.reset_code(), reset::DIAL_REFUSED);
        let why = fail.why(dst);
        assert!(why.starts_with(&format!("目标拨号失败（{dst}：")), "{why}");
        assert!(why.contains("Connection refused") || why.contains("61"), "{why}");
        // ② `connect` 未在预算内落定 ⇒ 0x26 + 预算文案
        let fail = connect_bounded(
            Duration::from_millis(20),
            std::future::pending::<std::io::Result<TcpStream>>(),
        )
        .await
        .expect_err("必须归超期");
        assert!(matches!(fail, DialFail::Timeout), "{fail:?}");
        assert_eq!(fail.reset_code(), reset::DIAL_TIMEOUT);
        assert_eq!(
            fail.why(dst),
            format!("目标拨号超时（{dst}；预算 {DIAL_BUDGET:?}）")
        );
        // 两码各自落在白名单里（对端读得成 typed 错误）
        for code in [fail.reset_code(), reset::DIAL_REFUSED] {
            assert!(crate::stream::StreamErr::from_reset_code(code).is_some());
        }
    }

    /// 拨号成功路径：`connect_bounded` 原样返回 socket（不吞错、不重试——**单次尝试**语义）。
    #[tokio::test]
    async fn connect_bounded_passes_through_the_connected_socket() {
        let ln = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("目标可绑");
        let addr = match ln.local_addr().expect("地址") {
            std::net::SocketAddr::V4(v) => v,
            std::net::SocketAddr::V6(_) => unreachable!(),
        };
        let tcp = connect_bounded(DIAL_BUDGET, TcpStream::connect(addr))
            .await
            .expect("回环上的活目标必拨通");
        let peer = tcp.peer_addr().expect("对端地址");
        assert_eq!(peer.ip(), std::net::IpAddr::V4(*addr.ip()));
        assert_eq!(peer.port(), addr.port());
    }
}
