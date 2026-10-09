//! STREAM 协议面（M3 设计 §1 的**单源**）：tag / 复位码表 / [`StreamErr`] / 写回执形态。
//!
//! **本文件纯 std**（隔离门 ② 条扫描面：不得出现 `tokio::|quinn|rustls|async fn|.await`）——
//! 出口侧分发（`exit/serve.rs`）与岛侧开流（`client/streams.rs`）都读同一份常量，
//! **任一侧另写一个字面量 tag = 静默错位**（照 `FRAME_KIND_QUIC` 的双侧断言先例）。
//!
//! 逐条真源（`docs/reviews/M3-design.md`）：
//! - §1.1 tag 的线格式：一条 bidi 流 = 一个服务会话；**首字节 = tag**（客户端→出口，
//!   紧随流开）；`1=files / 2=term / 3=speedtest / 4=dial / 5=probe`；其余取值 ⇒ 拒；
//! - §1.1 地址族：**本期限定 IPv4**——`dial` 目标 = `[4B IPv4][2B BE port]`（[`dial_target`]）；
//! - §1.6 错误面：流复位码表（`0x21`–`0x27`）+ **白名单规则**（其余含 `0x00`/未知/对端 FIN
//!   ⇒ 当 EOF，见 [`StreamErr::from_reset_code`]）；`StreamErr` 落 enum（thiserror，
//!   `#[non_exhaustive]`，**禁字符串错误**）；
//! - §1.7/§8.2-16：`TAG_READ_BUDGET = 5s`（出口读 tag 的预算；到点 ⇒ `reset(0x27)`）。

use std::fmt;
use std::net::SocketAddrV4;
use std::time::Duration;

/// 一条服务流的服务类别（**tag 的单源**）。
///
/// `#[non_exhaustive]`（AGENTS 工程原则 1：协议帧类型）——跨 crate 消费必须留通配臂；
/// **本 crate 内的匹配一律穷尽**（[`StreamTag::text`]/[`StreamTag::ALL`] 处加成员即编译红）。
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[non_exhaustive]
pub enum StreamTag {
    /// `1`：文件服务（应用层帧逐字节不变，§1.2）。
    Files,
    /// `2`：终端（HSP 帧；桥侧 48B 首包令牌不在本层，§1.2）。
    Term,
    /// `3`：测速（warmup/窗口/结算帧，§1.2）。
    Speedtest,
    /// `4`：拨号（**M3 只定协议**：出口一律 `reset(0x22)`；M4 换轨，§1.1/§12-4）。
    Dial,
    /// `5`：快探（客户端写 N(≥1)B ⇒ 出口原样回显直到客户端半关，§1.2/§3.2）。
    Probe,
}

impl StreamTag {
    /// 全部合法 tag（校验集与遍历面的单源；顺序 = 线值序）。
    pub const ALL: [StreamTag; 5] = [
        StreamTag::Files,
        StreamTag::Term,
        StreamTag::Speedtest,
        StreamTag::Dial,
        StreamTag::Probe,
    ];

    /// 线字节（§1.1 的单源；出口分发与岛侧开流都只经它）。
    pub const fn as_byte(self) -> u8 {
        match self {
            StreamTag::Files => 1,
            StreamTag::Term => 2,
            StreamTag::Speedtest => 3,
            StreamTag::Dial => 4,
            StreamTag::Probe => 5,
        }
    }

    /// 线字节 → tag（`None` = 非法 tag ⇒ 出口侧 `reset(0x21)`，§1.6）。
    pub const fn from_byte(b: u8) -> Option<StreamTag> {
        match b {
            1 => Some(StreamTag::Files),
            2 => Some(StreamTag::Term),
            3 => Some(StreamTag::Speedtest),
            4 => Some(StreamTag::Dial),
            5 => Some(StreamTag::Probe),
            _ => None,
        }
    }

    /// 判据行的 `%s`（E-q5 / C19 族；与 §8.2 的两族行文同词）。
    pub const fn text(self) -> &'static str {
        match self {
            StreamTag::Files => "files",
            StreamTag::Term => "term",
            StreamTag::Speedtest => "speedtest",
            StreamTag::Dial => "dial",
            StreamTag::Probe => "probe",
        }
    }
}

impl fmt::Display for StreamTag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.text())
    }
}

/// 流复位码表（§1.6；`VarInt` 取值；**出口写、客户端经 `ReadError::Reset(code)` 读**）。
///
/// 白名单语义（写死，别处不得再判）：`[TAG_UNKNOWN, TAG_READ_TIMEOUT]` 闭区间是 typed
/// 拒绝面；区间外（含 `0x00` 与对端自选码）**不是** typed 错误 ⇒ 客户端按 EOF 收
/// （[`StreamErr::from_reset_code`] 的 `None` 分支）——这样「半途复位 = 静默截断」的
/// 既有行为逐字保留（§1.6 设计门 3-2）。
pub mod reset {
    /// 未知 tag（tag ∉ {1..5}）。
    pub const TAG_UNKNOWN: u64 = 0x21;
    /// 服务不可用（`HOMEWAY_TERM=off` / 无 speedtest 监听 / files 监听失败 / **S2 接线前**
    /// 的 tag 1–4 暂态——设计门 N16 的「保守选择」注记见 §1.6）。
    pub const SERVICE_DISABLED: u64 = 0x22;
    /// 入口队列满（§1.7 的 per-tag 全局 intake；`在册上限 + K`）。
    pub const INTAKE_FULL: u64 = 0x23;
    /// 未绑定连接上的服务流（准入未完成）。
    pub const UNBOUND: u64 = 0x24;
    /// `dial` 目标拒（**M4**）。
    pub const DIAL_REFUSED: u64 = 0x25;
    /// `dial` 目标超时（**M4**）。
    pub const DIAL_TIMEOUT: u64 = 0x26;
    /// 开流后未在 [`crate::stream::TAG_READ_BUDGET`] 内读到 tag。
    pub const TAG_READ_TIMEOUT: u64 = 0x27;

    /// typed 白名单闭区间下界（= [`TAG_UNKNOWN`]）。
    pub const WHITELIST_MIN: u64 = TAG_UNKNOWN;
    /// typed 白名单闭区间上界（= [`TAG_READ_TIMEOUT`]）。
    pub const WHITELIST_MAX: u64 = TAG_READ_TIMEOUT;
}

/// tag 首帧的**读取预算**（§1.7 的队头阻塞面 / §8.2-16 的出口常量）：出口 accept 一条
/// 服务流后必须在 5s 内读到 tag，到点 ⇒ `reset(0x27)`（**reset 而非 drop**：quinn 的
/// 未 accept/未 reset 流不归还并发额度）。
pub const TAG_READ_BUDGET: Duration = Duration::from_secs(5);

/// `dial` 流的**拨号成功回执**（M4 §1.2；出口→客户端方向的**唯一 wire 增量**）。
///
/// 语义（写死，两侧共用这一枚常量；**不得**在任一侧另写字面量）：
/// - 出口在 `TcpStream::connect` **成功之后、泵启动之前**写下它，随后才是目标侧裸字节流
///   ⇒ 客户端读到的首块形如 `[0x01, payload…]`（**余量必须预置进读半缓冲**，§1.2 铁律）；
/// - 失败路径**不写回执**（改为 `reset(0x25/0x26)`）⇒ 客户端不会把失败读成「成功但无数据」；
/// - 必须存在的原因：拨号发生在**出口**，客户端 `open_bi + 写 6B` 之后无从知道拨号成败，
///   无回执则 Q-F-B 钉住的「拨号失败 ⇒ 对端 `read` = `ConnectionReset`」语义会退化成静默 EOF。
pub const DIAL_OK: u8 = 0x01;

/// 岛侧开流的兜底预算（对端 TP 异常时快速失败而不是挂死命令循环）。
///
/// **不是背压预算**：自记账（§1.4-N14 的容量）通过后 `open_bi` 只受对端
/// `max_concurrent_bidi_streams` 门控，两端同一份配置 ⇒ 实际不阻塞；本预算只兜
/// 「对端配置不同」的错配形态（到点归 [`StreamErr::Busy`]，与额度耗尽同面）。
pub const OPEN_BUDGET: Duration = TAG_READ_BUDGET;

/// 流的错误面（§1.6；**typed**，thiserror + `#[non_exhaustive]`，无字符串错误）。
///
/// 三个来源共用这一形态：①出口复位码（白名单映射）；②岛内本地判定（额度耗尽/连接断/
/// 未知流 id）；③对端 FIN（= [`StreamErr::Closed`]，即今天的 EOF 语义）。
#[derive(Clone, Copy, PartialEq, Eq, Debug, thiserror::Error)]
#[non_exhaustive]
pub enum StreamErr {
    /// 未绑定连接（`0x24`；准入未完成）。
    #[error("未绑定")]
    Unbound,
    /// 服务不可用（`0x22`；含 S2 接线前的 tag 1–4 暂态）。
    #[error("服务不可用")]
    NotSupported,
    /// 入口队列满（`0x23`）**或本端额度耗尽**（§1.6：两种都归这里，调用方按同一档处置）。
    #[error("入口队列满")]
    Busy,
    /// tag ∉ {1..5}（`0x21`）或 tag 读取超时（`0x27`）——客户端侧不可再分（§1.6 的映射表）。
    #[error("tag 读取超时")]
    BadTag,
    /// `dial` 目标拒（`0x25`；M4）。
    #[error("目标拒绝")]
    Refused,
    /// `dial` 目标超时（`0x26`；M4）。
    #[error("服务流超时")]
    Timeout,
    /// 流已关闭：对端 FIN（EOF 语义）、白名单外的复位码、或本端已 `close`。
    #[error("服务流已关闭")]
    Closed,
    /// 承载已断（连接死/被替换/岛未持有连接）。
    #[error("承载重连")]
    ConnectionLost,
}

impl StreamErr {
    /// 复位码 → typed 归因（§1.6 的**白名单规则**）：`0x21..=0x27` ⇒ `Some(..)`；
    /// 其余（`0x00`/未知/对端自选）⇒ `None` = 调用方按 **EOF**（[`StreamErr::Closed`]）收。
    pub const fn from_reset_code(code: u64) -> Option<StreamErr> {
        match code {
            reset::TAG_UNKNOWN | reset::TAG_READ_TIMEOUT => Some(StreamErr::BadTag),
            reset::SERVICE_DISABLED => Some(StreamErr::NotSupported),
            reset::INTAKE_FULL => Some(StreamErr::Busy),
            reset::UNBOUND => Some(StreamErr::Unbound),
            reset::DIAL_REFUSED => Some(StreamErr::Refused),
            reset::DIAL_TIMEOUT => Some(StreamErr::Timeout),
            _ => None,
        }
    }

    /// 判据行短文本（C19 族的 `%s`；与 §8.2-11 的取值集同词）。
    pub const fn text(self) -> &'static str {
        match self {
            StreamErr::Unbound => "未绑定",
            StreamErr::NotSupported => "服务不可用",
            StreamErr::Busy => "入口队列满",
            StreamErr::BadTag => "tag 读取超时",
            StreamErr::Refused => "目标拒绝",
            StreamErr::Timeout => "服务流超时",
            StreamErr::Closed => "服务流已关闭",
            StreamErr::ConnectionLost => "承载重连",
        }
    }

    /// 阶梯豁免集（§1.6 设计门 3-3）：**服务级拒绝不是连接故障** ⇒ 不触发恢复阶梯
    /// （S3 消费：`{NotSupported, Busy, Unbound, BadTag}`；只有 `{Closed, Timeout,
    /// ConnectionLost}` 触发恢复）。落在此处的理由：取值集与「什么算连接故障」是同一份
    /// 判断，分两处写必然漂移。
    pub const fn is_ladder_exempt(self) -> bool {
        matches!(
            self,
            StreamErr::NotSupported | StreamErr::Busy | StreamErr::Unbound | StreamErr::BadTag
        )
    }
}

/// 写回执（§1.4：**逐字段同形** `wgcore::WriteOut`）。
///
/// 语义（写死，防误读成 `io::Write` 契约）：
/// - `n` = 本次**接纳**的字节数（前缀；由**非阻塞**判定给出——有界待发队列余量）；
/// - `n == 0` ⇒ `back = Some(原 Vec)`：**背压**信号，调用方走仓内既有的 Ok(0) 分级退避环
///   （2ms→10ms→20ms 封顶 + 10s 无进展上界；`facade/tun_exec.rs` 的 `SessionWriteHalf`）。
///   **不得**按 `io::Write` 的「`Ok(0)` = 通道关」处理（那会让上行 bulk 一进慢链路就断流）；
/// - `0 < n < data.len()` ⇒ `back = None`：余量由调用方按 `io::Write` 契约以 `&data[n..]`
///   重试（**与 `wgcore` 同款**：部分接纳不回带，省一次整段拷贝）。
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct StreamWriteOut {
    /// 本次接纳的字节数（0 = 背压）。
    pub n: usize,
    /// 零接纳时**原样带回**的载荷（调用方重试直接复用，不重拷）。
    pub back: Option<Vec<u8>>,
}

/// 一条服务流的句柄（岛内分配；单调增、不复用）。
///
/// newtype（AGENTS 原则 1：裸 `u64` 在跨 crate 面会被误当别的 id——岛上另有连接
/// `stable_id`/赛跑 id 等裸 `ulong` 面）；`Display` 供 C19 行/排障。
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct StreamId(u64);

impl StreamId {
    /// 岛内构造（从 1 起；0 留作「未分配」哨兵）。
    pub(crate) const fn new(raw: u64) -> Self {
        Self(raw)
    }

    /// 原始值（排障/日志用；协议面不出现裸值）。
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl fmt::Display for StreamId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// `dial` tag 的目标载荷（§1.1：`[4B IPv4][2B BE port]` = 6B；**本期限定 IPv4**）。
///
/// M3 只定协议（出口一律 `reset(0x22)`）；M4 换轨时 `[`dial_parse`]` 反向解析同一份帧
/// ——本切片把两个方向一次钉死，M4 **不再改帧**（§12-4）。
pub fn dial_target(dst: SocketAddrV4) -> [u8; 6] {
    let mut out = [0u8; 6];
    out[..4].copy_from_slice(&dst.ip().octets());
    out[4..].copy_from_slice(&dst.port().to_be_bytes());
    out
}

/// `dial` 目标载荷的反解析（`None` = 长度不是 6B 的畸形帧）。
pub fn dial_parse(payload: &[u8]) -> Option<SocketAddrV4> {
    if payload.len() != 6 {
        return None;
    }
    let ip = std::net::Ipv4Addr::new(payload[0], payload[1], payload[2], payload[3]);
    let port = u16::from_be_bytes([payload[4], payload[5]]);
    Some(SocketAddrV4::new(ip, port))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **判据（tag 单源）**：线字节 = §1.1 的五值且双射；非法字节全部落 `None`。
    #[test]
    fn tag_bytes_are_the_designed_single_source() {
        let want = [
            (StreamTag::Files, 1u8, "files"),
            (StreamTag::Term, 2, "term"),
            (StreamTag::Speedtest, 3, "speedtest"),
            (StreamTag::Dial, 4, "dial"),
            (StreamTag::Probe, 5, "probe"),
        ];
        for (tag, byte, text) in want {
            assert_eq!(tag.as_byte(), byte, "tag {tag:?} 的线字节");
            assert_eq!(StreamTag::from_byte(byte), Some(tag), "反解析");
            assert_eq!(tag.text(), text, "判据行文本");
            assert_eq!(tag.to_string(), text, "Display = 判据行文本");
        }
        // `ALL` 与线值序一致（遍历面漂移 = 校验集漏项）
        let all: Vec<u8> = StreamTag::ALL.iter().map(|t| t.as_byte()).collect();
        assert_eq!(all, vec![1, 2, 3, 4, 5]);
        // 非法值（含 0 与 6..=255 的抽样）一律 `None`
        for b in [0u8, 6, 0x21, 0xFF] {
            assert_eq!(StreamTag::from_byte(b), None, "非法 tag 字节 {b:#04x}");
        }
    }

    /// **判据（复位码表 + 白名单规则）**：`0x21..=0x27` 逐码映射；区间外 ⇒ `None`（= EOF）。
    #[test]
    fn reset_code_whitelist_maps_to_typed_errors() {
        assert_eq!(
            StreamErr::from_reset_code(reset::TAG_UNKNOWN),
            Some(StreamErr::BadTag)
        );
        assert_eq!(
            StreamErr::from_reset_code(reset::TAG_READ_TIMEOUT),
            Some(StreamErr::BadTag)
        );
        assert_eq!(
            StreamErr::from_reset_code(reset::SERVICE_DISABLED),
            Some(StreamErr::NotSupported)
        );
        assert_eq!(
            StreamErr::from_reset_code(reset::INTAKE_FULL),
            Some(StreamErr::Busy)
        );
        assert_eq!(
            StreamErr::from_reset_code(reset::UNBOUND),
            Some(StreamErr::Unbound)
        );
        assert_eq!(
            StreamErr::from_reset_code(reset::DIAL_REFUSED),
            Some(StreamErr::Refused)
        );
        assert_eq!(
            StreamErr::from_reset_code(reset::DIAL_TIMEOUT),
            Some(StreamErr::Timeout)
        );
        // 区间外：0x00（今天出口的普通 close 码）/ 0x20 / 0x28 / 对端自选 7（§13 语义台
        // 实测里的 reset(7)）⇒ 全部当 EOF（`None`），**不得**误报成 typed 拒绝。
        for code in [0u64, 0x20, 0x28, 7, 0x100, u64::MAX] {
            assert_eq!(
                StreamErr::from_reset_code(code),
                None,
                "{code:#x} 不在白名单 ⇒ 按 EOF"
            );
        }
        // 白名单常量与映射表同界（改一处忘另一处 = 本断言红）
        assert_eq!(reset::WHITELIST_MIN, 0x21);
        assert_eq!(reset::WHITELIST_MAX, 0x27);
        for code in reset::WHITELIST_MIN..=reset::WHITELIST_MAX {
            assert!(
                StreamErr::from_reset_code(code).is_some(),
                "{code:#x} 在白名单区间内但无映射"
            );
        }
    }

    /// **判据（阶梯豁免集，§1.6-3-3）**：服务级拒绝不触发恢复；只有连接面错误触发。
    #[test]
    fn ladder_exempt_set_is_exactly_the_service_level_refusals() {
        for e in [
            StreamErr::NotSupported,
            StreamErr::Busy,
            StreamErr::Unbound,
            StreamErr::BadTag,
        ] {
            assert!(e.is_ladder_exempt(), "{e:?} 必须豁免");
        }
        for e in [
            StreamErr::Closed,
            StreamErr::Timeout,
            StreamErr::ConnectionLost,
            StreamErr::Refused,
        ] {
            assert!(!e.is_ladder_exempt(), "{e:?} 必须触发恢复阶梯");
        }
    }

    /// **判据（dial 帧逐字节，§1.1）**：`[4B IPv4][2B BE port]` 组帧/解帧互逆；畸形帧 ⇒ None。
    #[test]
    fn dial_target_frame_is_byte_exact_and_round_trips() {
        let dst: SocketAddrV4 = "100.64.7.9:7802".parse().unwrap();
        let frame = dial_target(dst);
        assert_eq!(frame, [100, 64, 7, 9, 0x1E, 0x7A], "4B IPv4 + 2B BE 端口");
        assert_eq!(dial_parse(&frame), Some(dst), "解帧互逆");
        // 畸形：长度不是 6B（含空/短/长）一律 None（M4 换轨时按「拒」处置）
        for bad in [vec![], vec![1, 2, 3, 4, 5], vec![0; 7]] {
            assert_eq!(dial_parse(&bad), None, "长度 {} 非 6B", bad.len());
        }
    }

    /// 预算常量与判据行文本（改动即协议偏离）。
    #[test]
    fn budgets_and_texts_are_pinned() {
        assert_eq!(TAG_READ_BUDGET, Duration::from_secs(5));
        assert_eq!(OPEN_BUDGET, TAG_READ_BUDGET);
        assert_eq!(StreamErr::Busy.text(), "入口队列满");
        assert_eq!(StreamErr::ConnectionLost.text(), "承载重连");
        // Display 与 text 同源（判据行与错误链不各写一份词表）
        assert_eq!(StreamErr::Unbound.to_string(), "未绑定");
    }

    /// **判据（dial 回执字节，M4 §1.2）**：`DIAL_OK` = `0x01` 且**不落在**复位码表区间里
    /// （否则「回执字节」与「复位码」会共用值域 ⇒ 两侧读法歧义）；也不等于任何 tag 字节
    /// （首字节含义在**方向**上区分：客户端→出口 = tag，出口→客户端 = 回执）。
    #[test]
    fn dial_ok_is_pinned_and_disjoint_from_other_byte_spaces() {
        assert_eq!(DIAL_OK, 0x01);
        assert!(
            !(reset::WHITELIST_MIN..=reset::WHITELIST_MAX).contains(&(DIAL_OK as u64)),
            "回执字节不得落在复位码白名单区间内"
        );
        assert_ne!(DIAL_OK, 0x00, "0x00 是传输层的「无应用错误码」默认值");
        assert_eq!(
            StreamTag::from_byte(DIAL_OK),
            Some(StreamTag::Files),
            "同一字节在「tag 面」有别的含义——两侧读数只按方向区分（本断言记录该事实）"
        );
    }

    /// 写回执形态（`n=0` 必带 `back` 的约定由生产方保证；此处钉「零接纳带回原 Vec」的等价性）。
    #[test]
    fn write_out_zero_accept_carries_the_original_buffer() {
        let data = vec![7u8; 4096];
        let out = StreamWriteOut {
            n: 0,
            back: Some(data.clone()),
        };
        assert_eq!(out.back.as_deref(), Some(data.as_slice()));
        assert_eq!(out.n, 0);
    }
}
