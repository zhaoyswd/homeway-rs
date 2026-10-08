//! frames — term 帧协议编解码（R6 6d；规格 = `docs/reviews/R6-design.md` §九 A5
//! 增量表，行为真源 = baseline 克隆 `pkg/term/frames.go`）。
//!
//! 帧 = `[op:1][len:2 LE][payload]`，`len ≤ 65535`（**超限截断不是拒帧**——Go
//! `encodeTermFrame` 直接切片）；DATA 由发送方按 ≤16KiB 分片。字节布局是两端契约
//! （App 侧 terminal 模块的 cpp/stream/term_frames.h 同形）。
//!
//! 词表面（ENDED code/reason、stateV2、agent、hello flags、caps 位、features 位）
//! 是**冻结枚举**（只增不改）；`[#[non_exhaustive]]` 的错误类型对齐仓内 thiserror
//! 纪律。判据 = `fixtures/term/frames.v1.jsonl`（13 案冻结契约）+ 本文件单测的
//! 往返/截断/尾随块形状族。

use std::io::Read;

// ---------------------------------------------------------------------------
// 常量与 op 表
// ---------------------------------------------------------------------------

/// 协议版本（GREETING ver 字节与服务端版本门的比对值）。
pub const PROTO_VER: u8 = 1;
/// 单帧载荷上限（超限截断，不是拒帧）。
pub const MAX_PAYLOAD: usize = 65535;
/// DATA 分片上限。
pub const DATA_CHUNK: usize = 16 << 10;
pub const MAX_NAME_LEN: usize = 64;
pub const MAX_REASON_LEN: usize = 200;
/// HELLO 尾随的实例标识字节上限。
pub const MAX_CLIENT_ID_LEN: usize = 64;

/// 帧类型（两方向共用一个字节空间，靠连接方向区分）。newtype 承载「字节空间只增
/// 不改、未知 op 由服务层拒绝」的现实；保留位 0x08 绝不复用。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Op(pub u8);

impl Op {
    pub const HELLO: Op = Op(0x00);
    pub const DATA: Op = Op(0x01);
    pub const RESIZE: Op = Op(0x02);
    pub const ENDED: Op = Op(0x03);
    /// C→S 请求 / S→C LIST-REPLY（payload = JSON）。
    pub const LIST: Op = Op(0x04);
    pub const KILL: Op = Op(0x05);
    pub const ERROR: Op = Op(0x06);
    pub const STATE: Op = Op(0x07);
    pub const ATTACHED: Op = Op(0x09);
    pub const REPLAY_DONE: Op = Op(0x0A);
    pub const OK: Op = Op(0x0B);
    pub const GREETING: Op = Op(0x0C);
    pub const SNAPSHOT: Op = Op(0x0D);
    pub const SNAPSHOT_DONE: Op = Op(0x0E);
    pub const SURFACE_DIFF: Op = Op(0x0F);
    pub const FETCH_ROWS: Op = Op(0x10);
    pub const INPUT: Op = Op(0x11);
    pub const THEME: Op = Op(0x12);
    pub const CLIPBOARD: Op = Op(0x13);
    pub const NOTIFY: Op = Op(0x14);
    pub const FETCH_SNAPSHOT: Op = Op(0x15);
    pub const EXPLAIN: Op = Op(0x16);
    pub const CREATE: Op = Op(0x17);
}

/// GREETING 的 features 位（客户端不认识的位忽略）。
pub mod features {
    pub const LIST: u32 = 1 << 0;
    pub const REPLAY: u32 = 1 << 1;
    pub const MODES: u32 = 1 << 2;
    pub const AGENT: u32 = 1 << 3;
    pub const TITLE: u32 = 1 << 4;
    /// surface 能力位：客户端见位才在 HELLO 尾随 capability 块。
    pub const SURFACE: u32 = 1 << 5;
    /// 「HELLO 版本声明可协商」位（FIX-29 服务端版本门的混合部署开关）。
    pub const PROTO_VER: u32 = 1 << 6;
    /// 本出口的全开位集（= 0x7F）。
    pub const ALL: u32 =
        LIST | REPLAY | MODES | AGENT | TITLE | SURFACE | PROTO_VER;
}

/// HELLO flags（term-host-cli 协议增量 D8）。
pub mod hello_flags {
    /// 创建语义（既有）。
    pub const CREATE: u8 = 1 << 0;
    /// 名字已存在则报 already_exists（`new` 不带 -A）。
    pub const ONLY_IF_ABSENT: u8 = 1 << 1;
    /// 显式接管：踢掉其它腿（`attach -d`；ENDED reason=replaced）。
    pub const TAKEOVER: u8 = 1 << 2;
}

/// HELLO 尾随 capability 块的 caps 位（只增不改；位 7 起刻意与被广泛解析的低位隔开）。
pub mod caps {
    pub const SURFACE: u8 = 1 << 0;
    pub const RAW_TERMINAL: u8 = 1 << 1;
    /// Q-J F1：客户端声明「本端 alt **不**产 ESC 前缀」= libghostty **darwin 编译分支**
    /// 语义（`option_as_alt ≡ .false`；附随：mok2 剥 alt 位、kitty 关联文本不因 alt 抑制、
    /// super 抑制文本）。与 [`KEY_ALT_ESC_PREFIX`] **互斥**。
    pub const KEY_ALT_NO_ESC_PREFIX: u8 = 1 << 2;
    /// Q-J F1：客户端声明「本端 alt 产 ESC 前缀」= libghostty **非 darwin 编译分支**语义
    /// （附随：mok2 保留 alt 位、kitty alt 阻文本）。与 [`KEY_ALT_NO_ESC_PREFIX`] **互斥**。
    /// **两位全不置 = 未声明 ⇒ 宿主推断**（缺省兼容，逐字节同今日）。
    /// **两位同置 = 歧义 ⇒ 按未声明处理（fail-soft）+ 计数 + 一次性告警，绝不拒腿**——
    /// 「裸 ID 尾随块」按 caps 解析出 `caps=0x7F`（恰含 bit2|bit3），拒绝面会把今天
    /// 可服务的腿变成 `bad_capability`（格式固有属性，见 `dec_hello_tail` 既有测试）。
    pub const KEY_ALT_ESC_PREFIX: u8 = 1 << 3;
    /// FIX-29：caps 带此位 ⇒ caps 块后跟 1 字节协议版本。
    pub const PROTO_VER: u8 = 1 << 7;
}

/// ENDED 的 code 词表（≥0 = 子进程退出码；负数 = 服务侧原因）。
pub mod ended_code {
    /// 同一会话被新的 attach 顶掉（reason 受控词表）。
    pub const REPLACED: i32 = -1;
    /// App 主动 kill。
    pub const KILLED: i32 = -2;
    /// 出口服务退出。
    pub const SERVICE_STOPPED: i32 = -3;
}

/// code=-1 的 reason 受控词表（冻结枚举，扩值先扩表）。
pub mod ended_reason {
    /// 被另一客户端显式接管（attach -d）。
    pub const REPLACED: &str = "replaced";
    /// 被同一实例标识的重连替换。
    pub const SELF_RECONNECT: &str = "self_reconnect";
}

/// code=-3 的 reason 词面（服务关停——CLI 按 reason 渲染文案，漏词落「未知」分支）。
pub const ENDED_REASON_SERVICE_STOPPED: &str = "service_stopped";

/// stateV2 字节枚举（STATE/ATTACHED/LIST JSON 共用）。
pub mod state_v2 {
    pub const UNKNOWN: u8 = 0;
    pub const WORKING: u8 = 1;
    pub const BLOCKED: u8 = 2;
    pub const IDLE: u8 = 3;

    /// 枚举 → 字符串词面（LIST JSON / App 词表分派用；只增不改）。
    pub fn name(s: u8) -> &'static str {
        match s {
            WORKING => "working",
            BLOCKED => "blocked",
            IDLE => "idle",
            _ => "unknown",
        }
    }
}

/// agent 字节枚举（与 App 侧一一对应）。
pub mod agent {
    pub const SHELL: u8 = 0;
    pub const CODEX: u8 = 1;
    pub const CLAUDE: u8 = 2;
    pub const OPENCODE: u8 = 3;
    pub const OPENCLAW: u8 = 4;
    pub const OTHER: u8 = 5;
    pub const UNKNOWN: u8 = 255;

    /// 枚举 → 可检索短名（日志/JSON；与字节枚举一一对应、只增不改）。
    pub fn name(a: u8) -> &'static str {
        match a {
            SHELL => "shell",
            CODEX => "codex",
            CLAUDE => "claude",
            OPENCODE => "opencode",
            OPENCLAW => "openclaw",
            OTHER => "other",
            _ => "unknown",
        }
    }
}

/// REPLAY-DONE 的 flags 位。
pub mod replay_flags {
    /// 头部被截断。
    pub const TRUNCATED: u8 = 1 << 0;
    /// 回放窗口跨过尺寸变化。
    pub const SIZE_CHANGE: u8 = 1 << 1;
}

/// CREATE 的 flags（⚠️ 极性与 HELLO bit1 相反：置位 = 存在则**复用**）。
pub mod create_flags {
    pub const REUSE_IF_EXISTS: u8 = 1 << 0;
}

/// INPUT 载荷的种类（首字节）。
pub mod input_kind {
    pub const KEY: u8 = 0;
    pub const TEXT: u8 = 1;
    pub const MOUSE: u8 = 2;
    pub const FOCUS: u8 = 3;
}

/// 文本事件（INPUT kind=1）的语义位（跨帧分片的粘贴）。
pub mod text_bits {
    /// 本片是粘贴内容（按括号粘贴模式包装）。
    pub const PASTE: u8 = 1 << 0;
    /// 后面还有同一段粘贴的后续片（闭合标记推迟到末片）。
    pub const MORE: u8 = 1 << 1;
    /// 本片是同一段粘贴的后续片（开标记已在首片发过）。
    pub const CONT: u8 = 1 << 2;
}

// ---------------------------------------------------------------------------
// 错误与帧读写
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum FrameError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("term: bad frame: greeting len {len}")]
    GreetingLen { len: usize },
    #[error("term: bad frame: hello len {len}")]
    HelloLen { len: usize },
    #[error("term: bad frame: hello name len {want} > {have}")]
    HelloNameLen { want: usize, have: usize },
    #[error("term: bad frame: capability 块声明 {want} 字节，实际只有 {have}")]
    CapsLen { want: usize, have: usize },
    #[error("term: bad frame: capability 声明了协议版本但版本字节缺失")]
    VerMissing,
    #[error("term: bad frame: 实例标识块声明 {want} 字节，实际只有 {have}")]
    ClientIdLen { want: usize, have: usize },
    #[error("term: bad frame: 实例标识 {len} 字节超过上限 {MAX_CLIENT_ID_LEN}")]
    ClientIdOver { len: usize },
    #[error("term: bad frame: 尾随块后还有 {len} 字节残留")]
    TailResidue { len: usize },
    #[error("term: bad frame: create len {len}")]
    CreateLen { len: usize },
    #[error("term: bad frame: create name len {want} > {have}")]
    CreateNameLen { want: usize, have: usize },
    #[error("term: bad frame: resize len {len}")]
    ResizeLen { len: usize },
    #[error("term: bad frame: replay-done len {len}")]
    ReplayDoneLen { len: usize },
    #[error("term: bad frame: attached head len {len}")]
    AttachedLen { len: usize },
    #[error("term: bad frame: name len {len}")]
    NameLen { len: usize },
    #[error("term: bad frame: name {want} > {have}")]
    NameOver { want: usize, have: usize },
    #[error("term: bad frame: error len {len}")]
    ErrorLen { len: usize },
    #[error("term: bad frame: error code len {want}")]
    ErrorCodeLen { want: usize },
    #[error("term: bad frame: error msg len {want} > {have}")]
    ErrorMsgLen { want: usize, have: usize },
    #[error("term: bad frame: input len {len}")]
    InputLen { len: usize },
    #[error("term: bad frame: input key utf8 len {want} > {have}")]
    InputKeyLen { want: usize, have: usize },
    #[error("term: bad frame: input text len {want} > {have}")]
    InputTextLen { want: usize, have: usize },
    #[error("term: bad frame: 未知 input kind {kind}")]
    InputKind { kind: u8 },
}

/// 一帧（op + 载荷；载荷归本帧所有）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub op: Op,
    pub payload: Vec<u8>,
}

/// 组帧（len 由这里算；载荷超 `MAX_PAYLOAD` 截断——同 Go，不是错误）。
pub fn encode_frame(op: Op, payload: &[u8]) -> Vec<u8> {
    let payload = &payload[..payload.len().min(MAX_PAYLOAD)];
    let mut out = Vec::with_capacity(3 + payload.len());
    out.push(op.0);
    out.extend_from_slice(&(payload.len() as u16).to_le_bytes());
    out.extend_from_slice(payload);
    out
}

/// 阻塞读一帧（短读/超长/未知长度一律报错，连接由调用方关掉）。
pub fn read_frame(r: &mut impl Read) -> Result<Frame, FrameError> {
    let mut hdr = [0u8; 3];
    r.read_exact(&mut hdr)?;
    let n = u16::from_le_bytes([hdr[1], hdr[2]]) as usize;
    let mut payload = vec![0u8; n];
    if n > 0 {
        r.read_exact(&mut payload)?;
    }
    Ok(Frame { op: Op(hdr[0]), payload })
}

// ---------------------------------------------------------------------------
// 载荷编解码
// ---------------------------------------------------------------------------

/// GREETING：`[ver:1][features:4LE]`。
pub fn enc_greeting() -> Vec<u8> {
    let mut p = Vec::with_capacity(5);
    p.push(PROTO_VER);
    p.extend_from_slice(&features::ALL.to_le_bytes());
    p
}

/// GREETING 解码（≥5B；多余字节忽略——同 Go 只读前 5 字节）。
pub fn dec_greeting(p: &[u8]) -> Result<(u8, u32), FrameError> {
    if p.len() < 5 {
        return Err(FrameError::GreetingLen { len: p.len() });
    }
    Ok((p[0], u32::from_le_bytes([p[1], p[2], p[3], p[4]])))
}

/// HELLO 载荷：`[cols:2LE][rows:2LE][flags:1][nameLen:1][name]` + 尾随块。
///
/// **nameLen 域（Q-L L5 判死，零行为加固注记）**：`name.len() as u8` 无钳制会静默截断/回绕，
/// 但**仓内不可达**——会话名双侧 ≤64（CLI `term_cli.rs` `validate_name` + 服务端
/// `service.rs` `valid_name`），且 Go 同形（`pkg/term/frames.go` `encHelloFlags` 的
/// `p[5] = byte(len(name))` 亦无钳制，`termMaxNameLen=64` 只作用于 `encName`）⇒ 判死不做，
/// 不加 `debug_assert`（收益为零、测试面误炸风险）。
pub fn enc_hello(cols: u16, rows: u16, flags: u8, name: &str, tail: &[u8]) -> Vec<u8> {
    let mut p = Vec::with_capacity(6 + name.len() + tail.len());
    p.extend_from_slice(&cols.to_le_bytes());
    p.extend_from_slice(&rows.to_le_bytes());
    p.push(flags);
    p.push(name.len() as u8);
    p.extend_from_slice(name.as_bytes());
    p.extend_from_slice(tail);
    p
}

/// HELLO 解码。返回 (cols, rows, flags, name, tail)——tail = name 之后的尾随字节
/// （旧客户端不发 ⇒ 空）。
pub fn dec_hello(p: &[u8]) -> Result<(u16, u16, u8, String, &[u8]), FrameError> {
    if p.len() < 6 {
        return Err(FrameError::HelloLen { len: p.len() });
    }
    let cols = u16::from_le_bytes([p[0], p[1]]);
    let rows = u16::from_le_bytes([p[2], p[3]]);
    let flags = p[4];
    let n = p[5] as usize;
    if p.len() < 6 + n {
        return Err(FrameError::HelloNameLen { want: n, have: p.len() - 6 });
    }
    let name = String::from_utf8_lossy(&p[6..6 + n]).into_owned();
    Ok((cols, rows, flags, name, &p[6 + n..]))
}

/// HELLO 尾随块（caps + 可选版本字节 + 可选实例标识）。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HelloTail {
    pub caps: u8,
    pub caps_present: bool,
    pub client_id: String,
    pub ver: u8,
    pub ver_present: bool,
}

/// 组尾随块。**编码侧约束（同形不可判别的配套）**：带 ID 必带 caps 块——
/// 未声明能力时静默丢弃 ID；caps 带 [`caps::PROTO_VER`] 位时插 1 字节版本。
pub fn enc_hello_tail(caps_bits: u8, caps_present: bool, id: &str) -> Vec<u8> {
    let mut out = Vec::new();
    if caps_present {
        // encCapability = [capLen:1][flags:capLen]（长度 + 位图，加能力只加位）
        out.push(1);
        out.push(caps_bits);
        if caps_bits & caps::PROTO_VER != 0 {
            out.push(PROTO_VER);
        }
        if !id.is_empty() {
            out.push(id.len() as u8);
            out.extend_from_slice(id.as_bytes());
        }
    }
    out
}

/// 解尾随块：形状校验（**恰好耗尽**；畸形一律拒收 = bad_capability 面）。
/// 空 caps 块（capLen=0）消费 1 字节且 caps_present=false（形状合法）；
/// 声明 protoVer 位而版本字节缺失 = 畸形。
pub fn dec_hello_tail(tail: &[u8]) -> Result<HelloTail, FrameError> {
    let mut t = HelloTail::default();
    let mut off = 0usize;
    if tail.len() > off {
        let n = tail[off] as usize;
        if n > 0 {
            if tail.len() < off + 1 + n {
                return Err(FrameError::CapsLen { want: n, have: tail.len() - off - 1 });
            }
            for i in 0..n {
                t.caps |= tail[off + 1 + i];
            }
            t.caps_present = true;
        }
        off += 1 + n;
    }
    if t.caps_present && t.caps & caps::PROTO_VER != 0 {
        if tail.len() <= off {
            return Err(FrameError::VerMissing);
        }
        t.ver = tail[off];
        t.ver_present = true;
        off += 1;
    }
    if tail.len() > off {
        let n = tail[off] as usize;
        if tail.len() < off + 1 + n {
            return Err(FrameError::ClientIdLen { want: n, have: tail.len() - off - 1 });
        }
        if n > MAX_CLIENT_ID_LEN {
            return Err(FrameError::ClientIdOver { len: n });
        }
        t.client_id = String::from_utf8_lossy(&tail[off + 1..off + 1 + n]).into_owned();
        off += 1 + n;
    }
    if tail.len() > off {
        return Err(FrameError::TailResidue { len: tail.len() - off });
    }
    Ok(t)
}

/// CREATE：`[flags:1][nameLen:1][name]`（bit0 = reuse-if-exists，极性与 HELLO bit1 相反）。
pub fn enc_create(flags: u8, name: &str) -> Vec<u8> {
    let mut p = Vec::with_capacity(2 + name.len());
    p.push(flags);
    p.push(name.len() as u8);
    p.extend_from_slice(name.as_bytes());
    p
}

pub fn dec_create(p: &[u8]) -> Result<(u8, String), FrameError> {
    if p.len() < 2 {
        return Err(FrameError::CreateLen { len: p.len() });
    }
    let n = p[1] as usize;
    if p.len() < 2 + n {
        return Err(FrameError::CreateNameLen { want: n, have: p.len() - 2 });
    }
    Ok((p[0], String::from_utf8_lossy(&p[2..2 + n]).into_owned()))
}

/// RESIZE：`[cols:2LE][rows:2LE]`。
pub fn enc_resize(cols: u16, rows: u16) -> Vec<u8> {
    let mut p = Vec::with_capacity(4);
    p.extend_from_slice(&cols.to_le_bytes());
    p.extend_from_slice(&rows.to_le_bytes());
    p
}

pub fn dec_resize(p: &[u8]) -> Result<(u16, u16), FrameError> {
    if p.len() < 4 {
        return Err(FrameError::ResizeLen { len: p.len() });
    }
    Ok((u16::from_le_bytes([p[0], p[1]]), u16::from_le_bytes([p[2], p[3]])))
}

/// ATTACHED：`[cols:2LE][rows:2LE][modes:4LE][agent:1][state:1][name]`
/// （name 无长度前缀、吃尽余量）。
pub fn enc_attached(cols: u16, rows: u16, modes: u32, agent: u8, state: u8, name: &str) -> Vec<u8> {
    let mut p = Vec::with_capacity(10 + name.len());
    p.extend_from_slice(&cols.to_le_bytes());
    p.extend_from_slice(&rows.to_le_bytes());
    p.extend_from_slice(&modes.to_le_bytes());
    p.push(agent);
    p.push(state);
    p.extend_from_slice(name.as_bytes());
    p
}

/// ATTACHED 解码（头部 ≥10B，短了报错——同 Go decAttachedHead 的 ok=false 面）。
pub fn dec_attached(p: &[u8]) -> Result<(u16, u16, u32, u8, u8, String), FrameError> {
    if p.len() < 10 {
        return Err(FrameError::AttachedLen { len: p.len() });
    }
    Ok((
        u16::from_le_bytes([p[0], p[1]]),
        u16::from_le_bytes([p[2], p[3]]),
        u32::from_le_bytes([p[4], p[5], p[6], p[7]]),
        p[8],
        p[9],
        String::from_utf8_lossy(&p[10..]).into_owned(),
    ))
}

/// REPLAY-DONE：`[replayed:4LE][flags:1]`。
pub fn enc_replay_done(replayed: u32, flags: u8) -> Vec<u8> {
    let mut p = Vec::with_capacity(5);
    p.extend_from_slice(&replayed.to_le_bytes());
    p.push(flags);
    p
}

pub fn dec_replay_done(p: &[u8]) -> Result<(u32, u8), FrameError> {
    if p.len() < 5 {
        return Err(FrameError::ReplayDoneLen { len: p.len() });
    }
    Ok((u32::from_le_bytes([p[0], p[1], p[2], p[3]]), p[4]))
}

/// ENDED：`[code:4LE][reasonLen:1][reason≤200]`（reason 超 200 截断）。
pub fn enc_ended(code: i32, reason: &str) -> Vec<u8> {
    let reason = truncate_bytes(reason, MAX_REASON_LEN);
    let mut p = Vec::with_capacity(5 + reason.len());
    p.extend_from_slice(&code.to_le_bytes());
    p.push(reason.len() as u8);
    p.extend_from_slice(reason);
    p
}

pub fn dec_ended(p: &[u8]) -> (i32, String) {
    if p.len() < 5 {
        return (0, String::new());
    }
    let code = i32::from_le_bytes([p[0], p[1], p[2], p[3]]);
    // 长度守卫（Go decEndedParts 同款：声明越界 ⇒ 空 reason，不 panic——评审整改 r1-中②）
    let n = (p[4] as usize).min(p.len() - 5);
    (code, String::from_utf8_lossy(&p[5..5 + n]).into_owned())
}

/// STATE：`[agent:1][state:1][titleLen:2LE][title≤512]`。
pub fn enc_state(agent: u8, state: u8, title: &str) -> Vec<u8> {
    let title = truncate_bytes(title, 512);
    let mut p = Vec::with_capacity(4 + title.len());
    p.push(agent);
    p.push(state);
    p.extend_from_slice(&(title.len() as u16).to_le_bytes());
    p.extend_from_slice(title);
    p
}

pub fn dec_state(p: &[u8]) -> (u8, u8, String) {
    if p.len() < 4 {
        return (agent::UNKNOWN, state_v2::UNKNOWN, String::new());
    }
    let n = u16::from_le_bytes([p[2], p[3]]) as usize;
    (
        p[0],
        p[1],
        String::from_utf8_lossy(&p[4..4 + n.min(p.len() - 4)]).into_owned(),
    )
}

/// ERROR：`[codeLen:1][code≤255][msgLen:2LE][msg≤4096]`。
pub fn enc_error(code: &str, msg: &str) -> Vec<u8> {
    let code = truncate_bytes(code, 255);
    let msg = truncate_bytes(msg, 4096);
    let mut p = Vec::with_capacity(3 + code.len() + msg.len());
    p.push(code.len() as u8);
    p.extend_from_slice(code);
    p.extend_from_slice(&(msg.len() as u16).to_le_bytes());
    p.extend_from_slice(msg);
    p
}

pub fn dec_error(p: &[u8]) -> Result<(String, String), FrameError> {
    if p.is_empty() {
        return Err(FrameError::ErrorLen { len: p.len() });
    }
    let n = p[0] as usize;
    if p.len() < 1 + n + 2 {
        return Err(FrameError::ErrorCodeLen { want: n });
    }
    let code = String::from_utf8_lossy(&p[1..1 + n]).into_owned();
    let ml = u16::from_le_bytes([p[1 + n], p[2 + n]]) as usize;
    if p.len() < 3 + n + ml {
        return Err(FrameError::ErrorMsgLen { want: ml, have: p.len() - 3 - n });
    }
    Ok((code, String::from_utf8_lossy(&p[3 + n..3 + n + ml]).into_owned()))
}

/// 字节级截断（Go 语义：`s[:N]` 按字节切、可切进多字节字符中间；载荷是字节串
/// 无 UTF-8 约束——绝不因截点非字符边界 panic）。评审整改 r1-高①。
fn truncate_bytes(s: &str, max: usize) -> &[u8] {
    &s.as_bytes()[..s.len().min(max)]
}

/// KILL 等的「nameLen + name」载荷（name > 64 截断——同 Go encName）。
pub fn enc_name(name: &str) -> Vec<u8> {
    let name = truncate_bytes(name, MAX_NAME_LEN);
    let mut out = Vec::with_capacity(1 + name.len());
    out.push(name.len() as u8);
    out.extend_from_slice(name);
    out
}

pub fn dec_name(p: &[u8]) -> Result<String, FrameError> {
    if p.is_empty() {
        return Err(FrameError::NameLen { len: p.len() });
    }
    let n = p[0] as usize;
    if p.len() < 1 + n {
        return Err(FrameError::NameOver { want: n, have: p.len() - 1 });
    }
    Ok(String::from_utf8_lossy(&p[1..1 + n]).into_owned())
}

// ---------------------------------------------------------------------------
// INPUT 上行（kind 0..3；服务端按 vt 真实模式编码——与 keyenc 消费面衔接在 6f）
// ---------------------------------------------------------------------------

/// 一次抽象输入的 wire 形态（owned；借给 [`crate::term::keyenc`] 编码由会话层组）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputEvent {
    /// 键：`[key:2LE][mods:2LE][action:1][utf8Len:1][utf8]`。
    Key { key: u16, mods: u16, action: u8, text: String },
    /// 文本（IME/粘贴）：`[flags:1][len:2LE][text]`。
    Text { flags: u8, text: String },
    /// 鼠标：`[action:1][button:1][mods:2LE][x:2LE][y:2LE]`（网格坐标）。
    Mouse { action: u8, button: u8, mods: u16, x: u16, y: u16 },
    /// 焦点：`[gained:1]`。
    Focus { gained: bool },
}

pub fn enc_input(ev: &InputEvent) -> Vec<u8> {
    match ev {
        InputEvent::Key { key, mods, action, text } => {
            let mut p = Vec::with_capacity(7 + text.len());
            p.push(input_kind::KEY);
            p.extend_from_slice(&key.to_le_bytes());
            p.extend_from_slice(&mods.to_le_bytes());
            p.push(*action);
            p.push(text.len() as u8);
            p.extend_from_slice(text.as_bytes());
            p
        }
        InputEvent::Text { flags, text } => {
            let mut p = Vec::with_capacity(4 + text.len());
            p.push(input_kind::TEXT);
            p.push(*flags);
            p.extend_from_slice(&(text.len() as u16).to_le_bytes());
            p.extend_from_slice(text.as_bytes());
            p
        }
        InputEvent::Mouse { action, button, mods, x, y } => {
            let mut p = Vec::with_capacity(10);
            p.push(input_kind::MOUSE);
            p.push(*action);
            p.push(*button);
            p.extend_from_slice(&mods.to_le_bytes());
            p.extend_from_slice(&x.to_le_bytes());
            p.extend_from_slice(&y.to_le_bytes());
            p
        }
        InputEvent::Focus { gained } => vec![input_kind::FOCUS, u8::from(*gained)],
    }
}

pub fn dec_input(p: &[u8]) -> Result<InputEvent, FrameError> {
    let Some(&kind) = p.first() else {
        return Err(FrameError::InputLen { len: p.len() });
    };
    match kind {
        input_kind::KEY => {
            if p.len() < 7 {
                return Err(FrameError::InputLen { len: p.len() });
            }
            let n = p[6] as usize;
            if p.len() < 7 + n {
                return Err(FrameError::InputKeyLen { want: n, have: p.len() - 7 });
            }
            Ok(InputEvent::Key {
                key: u16::from_le_bytes([p[1], p[2]]),
                mods: u16::from_le_bytes([p[3], p[4]]),
                action: p[5],
                text: String::from_utf8_lossy(&p[7..7 + n]).into_owned(),
            })
        }
        input_kind::TEXT => {
            if p.len() < 4 {
                return Err(FrameError::InputLen { len: p.len() });
            }
            let n = u16::from_le_bytes([p[2], p[3]]) as usize;
            if p.len() < 4 + n {
                return Err(FrameError::InputTextLen { want: n, have: p.len() - 4 });
            }
            Ok(InputEvent::Text {
                flags: p[1],
                text: String::from_utf8_lossy(&p[4..4 + n]).into_owned(),
            })
        }
        input_kind::MOUSE => {
            if p.len() < 9 {
                return Err(FrameError::InputLen { len: p.len() });
            }
            Ok(InputEvent::Mouse {
                action: p[1],
                button: p[2],
                mods: u16::from_le_bytes([p[3], p[4]]),
                x: u16::from_le_bytes([p[5], p[6]]),
                y: u16::from_le_bytes([p[7], p[8]]),
            })
        }
        input_kind::FOCUS => {
            if p.len() < 2 {
                return Err(FrameError::InputLen { len: p.len() });
            }
            Ok(InputEvent::Focus { gained: p[1] != 0 })
        }
        k => Err(FrameError::InputKind { kind: k }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unhex(s: &str) -> Vec<u8> {
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
    }

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    /// 6d 判据①：fixtures/term/frames.v1.jsonl 冻结契约逐案对拍（13 案）。
    /// （该 fixture 无 0x09 ATTACHED 案——门一评审 A5 登记，由下方 attached 测试补。）
    #[test]
    fn frames_fixture_parity() {
        let base = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/term");
        let raw = std::fs::read_to_string(format!("{base}/frames.v1.jsonl")).unwrap();
        let mut count = 0;
        let mut negatives = 0;
        for line in raw.lines().filter(|l| !l.trim().is_empty()) {
            let v: serde_json::Value = serde_json::from_str(line).unwrap();
            let name = v["name"].as_str().unwrap();
            let op = u8::from_str_radix(v["op"].as_str().unwrap().trim_start_matches("0x"), 16).unwrap();
            let payload = unhex(v["payloadHex"].as_str().unwrap_or(""));
            let expect = &v["expect"];
            if let Some(err) = expect.get("error").and_then(|e| e.as_str()) {
                // 负例（wire 字段 = `hex`）：流层错误走 read_frame、坏载荷走 dec_hello
                let wire = unhex(v["hex"].as_str().unwrap_or(""));
                match err {
                    "short_header" | "truncated" => {
                        let mut rd = wire.as_slice();
                        assert!(read_frame(&mut rd).is_err(), "{name}: 流层短读必须报错");
                    }
                    "bad_payload" => {
                        // 合法帧 + 坏载荷：解出帧后载荷层报 HelloLen
                        let mut rd = wire.as_slice();
                        let f = read_frame(&mut rd).unwrap();
                        assert!(matches!(dec_hello(&f.payload), Err(FrameError::HelloLen { .. })), "{name}");
                    }
                    other => panic!("{name}: 未知负例种类 {other}"),
                }
                negatives += 1;
                continue;
            }
            count += 1;
            let op_map: &[(&str, Op)] = &[
                ("greeting", Op::GREETING),
                ("hello", Op::HELLO),
                ("resize", Op::RESIZE),
                ("data", Op::DATA),
                ("replayDone", Op::REPLAY_DONE),
                ("ended", Op::ENDED),
                ("state", Op::STATE),
                ("error", Op::ERROR),
                ("ok", Op::OK),
                ("kill", Op::KILL),
            ];
            let kind = expect.as_object().unwrap().keys().next().unwrap();
            let want_op = op_map.iter().find(|(k, _)| *k == kind).unwrap().1;
            assert_eq!(op, want_op.0, "{name}: op 字节与 expect 种类不符");
            if let Some(g) = expect.get("greeting") {
                let (ver, features) = dec_greeting(&payload).unwrap();
                assert_eq!(ver as u64, g["ver"].as_u64().unwrap(), "{name}");
                assert_eq!(features as u64, g["features"].as_u64().unwrap(), "{name}");
                assert_eq!(hex(&enc_greeting()), hex(&payload), "{name}: greeting 往返");
            }
            if let Some(h) = expect.get("hello") {
                let (cols, rows, flags, name_out, tail) = dec_hello(&payload).unwrap();
                assert_eq!(cols as u64, h["cols"].as_u64().unwrap(), "{name}");
                assert_eq!(rows as u64, h["rows"].as_u64().unwrap(), "{name}");
                assert_eq!(flags as u64, h["flags"].as_u64().unwrap(), "{name}");
                assert_eq!(name_out, h["name"].as_str().unwrap(), "{name}");
                assert!(tail.is_empty(), "{name}: 本 fixture 无尾随");
            }
            if let Some(r) = expect.get("resize") {
                let (cols, rows) = dec_resize(&payload).unwrap();
                assert_eq!((cols, rows), (r["cols"].as_u64().unwrap() as u16, r["rows"].as_u64().unwrap() as u16), "{name}");
            }
            if let Some(d) = expect.get("data") {
                assert_eq!(hex(&payload), d["bytes"].as_str().unwrap(), "{name}");
            }
            if let Some(rd) = expect.get("replayDone") {
                let (rev, flags) = dec_replay_done(&payload).unwrap();
                assert_eq!(rev as u64, rd["rev"].as_u64().unwrap(), "{name}");
                assert_eq!(flags as u64, rd["flags"].as_u64().unwrap(), "{name}");
            }
            if let Some(e) = expect.get("ended") {
                let (code, reason) = dec_ended(&payload);
                assert_eq!(code, e["code"].as_i64().unwrap() as i32, "{name}");
                assert_eq!(reason, e["reason"].as_str().unwrap(), "{name}");
            }
            if let Some(s) = expect.get("state") {
                let (agent, state, title) = dec_state(&payload);
                assert_eq!(agent as u64, s["agent"].as_u64().unwrap(), "{name}");
                assert_eq!(state as u64, s["stateV2"].as_u64().unwrap(), "{name}");
                assert_eq!(title, s["title"].as_str().unwrap(), "{name}");
            }
            if let Some(e) = expect.get("error") {
                let (code, msg) = dec_error(&payload).unwrap();
                assert_eq!(code, e["code"].as_str().unwrap(), "{name}");
                assert_eq!(msg, e["msg"].as_str().unwrap(), "{name}");
            }
            if let Some(k) = expect.get("kill") {
                assert_eq!(dec_name(&payload).unwrap(), k["name"].as_str().unwrap(), "{name}");
            }
        }
        assert_eq!((count, negatives), (10, 3), "fixture 正例 10 + 负例 3 全数消费");
    }

    /// 帧读写：滴流读不丢帧不串帧（FrameReader 教训——read_exact 的续读语义）。
    struct Dribble<'a> {
        data: &'a [u8],
        pos: usize,
        chunk: usize,
    }
    impl Read for Dribble<'_> {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let n = buf.len().min(self.chunk).min(self.data.len() - self.pos);
            buf[..n].copy_from_slice(&self.data[self.pos..self.pos + n]);
            self.pos += n;
            Ok(n)
        }
    }

    #[test]
    fn frame_roundtrip_dribbled_reads() {
        let frames = [
            (Op::GREETING, enc_greeting()),
            (Op::DATA, b"hello \xe4\xb8\x96\xe7\x95\x8c".to_vec()),
            (Op::OK, Vec::new()),
            (Op::ENDED, enc_ended(ended_code::REPLACED, ended_reason::SELF_RECONNECT)),
            (Op::STATE, enc_state(agent::CODEX, state_v2::BLOCKED, "标题")),
        ];
        let mut wire = Vec::new();
        for (op, payload) in &frames {
            wire.extend(encode_frame(*op, payload));
        }
        for chunk in [1usize, 2, 3, 4] {
            let mut rd = Dribble { data: &wire, pos: 0, chunk };
            let mut got = Vec::new();
            while rd.pos < wire.len() {
                got.push(read_frame(&mut rd).unwrap());
            }
            assert_eq!(got.len(), frames.len(), "chunk={chunk}");
            for (f, (op, payload)) in got.iter().zip(&frames) {
                assert_eq!((f.op, f.payload.clone()), (*op, payload.clone()), "chunk={chunk}");
            }
        }
    }

    /// 载荷超限截断（65536 → 65535；同 Go 不是错误）。
    #[test]
    fn payload_over_limit_truncates() {
        let big = vec![7u8; MAX_PAYLOAD + 1];
        let f = encode_frame(Op::DATA, &big);
        assert_eq!(f.len(), 3 + MAX_PAYLOAD);
        assert_eq!(u16::from_le_bytes([f[1], f[2]]) as usize, MAX_PAYLOAD);
    }

    /// HELLO 尾随块形状族（FIX-29 版本门 + 同形不可判别的编码侧约束）。
    #[test]
    fn hello_tail_shapes() {
        // 全形态：caps(surface|protoVer) + ver + id
        let tail = enc_hello_tail(caps::SURFACE | caps::PROTO_VER, true, "host1");
        assert_eq!(hex(&tail), "01810105686f737431"); // [1][81][ver=01][5]"host1"
        let t = dec_hello_tail(&tail).unwrap();
        assert_eq!(t.caps, caps::SURFACE | caps::PROTO_VER);
        assert!(t.caps_present && t.ver_present);
        assert_eq!(t.ver, PROTO_VER);
        assert_eq!(t.client_id, "host1");
        // caps 无 protoVer 位 ⇒ 无版本字节
        let tail = enc_hello_tail(caps::RAW_TERMINAL, true, "h");
        let t = dec_hello_tail(&tail).unwrap();
        assert!(!t.ver_present);
        assert_eq!(t.client_id, "h");
        // 未声明能力 ⇒ ID 被静默丢弃（编码侧约束）
        let tail = enc_hello_tail(0, false, "dropped");
        assert!(tail.is_empty());
        // 空 caps 块（capLen=0）：形状合法、caps_present=false
        let t = dec_hello_tail(&[0]).unwrap();
        assert!(!t.caps_present);
        // 裸 ID 形状（无 caps 块头）按 caps 解析——同形不可判别，解码侧不特判
        let t = dec_hello_tail(&[4, b'h', b'o', b's', b't']).unwrap();
        assert!(t.caps_present && t.caps == (b'h' | b'o' | b's' | b't') && t.client_id.is_empty());
        // 残留字节拒绝
        assert!(matches!(dec_hello_tail(&[1, 2, 1, b'x', 9]), Err(FrameError::TailResidue { len: 1 })));
        // 声明 protoVer 位而版本字节缺失 = 畸形
        assert!(matches!(dec_hello_tail(&[1, caps::PROTO_VER]), Err(FrameError::VerMissing)));
        // caps 块声明超长拒绝
        assert!(matches!(dec_hello_tail(&[9, 1, 2]), Err(FrameError::CapsLen { .. })));
        // ID 超 64 字节拒绝
        let mut over = vec![1u8, caps::SURFACE];
        over.push(65);
        over.extend_from_slice(&[b'x'; 65]);
        assert!(matches!(dec_hello_tail(&over), Err(FrameError::ClientIdOver { len: 65 })));
    }

    /// HELLO 全载往返（含尾随）+ 畸形 name 长度。
    #[test]
    fn hello_payload_roundtrip_and_errors() {
        let tail = enc_hello_tail(caps::SURFACE | caps::PROTO_VER, true, "cid");
        let p = enc_hello(120, 40, hello_flags::CREATE | hello_flags::TAKEOVER, "dev-abc123", &tail);
        let (cols, rows, flags, name, t) = dec_hello(&p).unwrap();
        assert_eq!((cols, rows, flags, name.as_str()), (120, 40, hello_flags::CREATE | hello_flags::TAKEOVER, "dev-abc123"));
        let t = dec_hello_tail(t).unwrap();
        assert_eq!(t.client_id, "cid");
        // 短头 / name 越界
        assert!(matches!(dec_hello(&[1, 2, 3]), Err(FrameError::HelloLen { len: 3 })));
        assert!(matches!(dec_hello(&[0, 0, 0, 0, 0, 9, b'a']), Err(FrameError::HelloNameLen { .. })));
    }

    /// ATTACHED 头（fixture 缺 0x09 案的补钉：A5 登记项）。
    #[test]
    fn attached_roundtrip() {
        let p = enc_attached(100, 32, 0x7F, agent::CODEX, state_v2::WORKING, "dev-abc123");
        let (cols, rows, modes, a, s, name) = dec_attached(&p).unwrap();
        assert_eq!((cols, rows, modes, a, s, name.as_str()), (100, 32, 0x7F, agent::CODEX, state_v2::WORKING, "dev-abc123"));
        assert!(matches!(dec_attached(&[0; 9]), Err(FrameError::AttachedLen { len: 9 })));
    }

    /// CREATE 往返 + 极性常量（与 HELLO bit1 相反）。
    #[test]
    fn create_roundtrip() {
        let p = enc_create(create_flags::REUSE_IF_EXISTS, "dev-1");
        let (flags, name) = dec_create(&p).unwrap();
        assert_eq!((flags, name.as_str()), (create_flags::REUSE_IF_EXISTS, "dev-1"));
        assert_ne!(create_flags::REUSE_IF_EXISTS, hello_flags::ONLY_IF_ABSENT); // 极性相对、值域不同
        assert!(matches!(dec_create(&[0]), Err(FrameError::CreateLen { len: 1 })));
    }

    /// 截断上限族（name 64 / reason 200 / title 512 / error 255+4096）——含 CJK
    /// 截点（评审整改 r1-高①：字节级截断不 panic、长度精确到上限）。
    #[test]
    fn truncation_limits() {
        assert_eq!(enc_name(&"x".repeat(100)).len(), 1 + MAX_NAME_LEN);
        assert_eq!(enc_ended(1, &"r".repeat(300)).len(), 5 + MAX_REASON_LEN);
        assert_eq!(enc_state(agent::SHELL, state_v2::IDLE, &"t".repeat(600)).len(), 4 + 512);
        assert_eq!(enc_error(&"c".repeat(300), &"m".repeat(5000)).len(), 3 + 255 + 4096);
        // CJK：截点落进多字节字符（Go 按字节切不 panic；此处同长度、字节级）
        let cjk_title = "标".repeat(200); // 600B > 512
        let p = enc_state(agent::SHELL, state_v2::IDLE, &cjk_title);
        assert_eq!(p.len(), 4 + 512);
        let cjk_name = "会".repeat(40); // 120B > 64
        assert_eq!(enc_name(&cjk_name).len(), 1 + MAX_NAME_LEN);
        let cjk_reason = "由".repeat(100); // 300B > 200
        assert_eq!(enc_ended(1, &cjk_reason).len(), 5 + MAX_REASON_LEN);
        // dec_ended 声明越界 ⇒ 空 reason 不 panic（评审整改 r1-中②）
        let (code, reason) = dec_ended(&[0, 0, 0, 0, 200]);
        assert_eq!((code, reason.as_str()), (0, ""));
    }

    /// INPUT 上行四类往返 + 粘贴语义位。
    #[test]
    fn input_events_roundtrip() {
        for ev in [
            InputEvent::Key { key: 20, mods: 0b111, action: 1, text: "a".into() },
            InputEvent::Text { flags: text_bits::PASTE | text_bits::MORE, text: "pasted".into() },
            InputEvent::Mouse { action: 2, button: 4, mods: 3, x: 100, y: 50 },
            InputEvent::Focus { gained: true },
        ] {
            assert_eq!(dec_input(&enc_input(&ev)).unwrap(), ev);
        }
        assert!(matches!(dec_input(&[9]), Err(FrameError::InputKind { kind: 9 })));
        // 键载荷 utf8 越界
        assert!(matches!(dec_input(&[0, 20, 0, 0, 0, 1, 9, b'a']), Err(FrameError::InputKeyLen { .. })));
    }

    /// 词表面：stateV2/agent 名字映射 + ENDED 语义组。
    #[test]
    fn vocab_names_and_ended_semantics() {
        assert_eq!(state_v2::name(state_v2::WORKING), "working");
        assert_eq!(state_v2::name(state_v2::BLOCKED), "blocked");
        assert_eq!(state_v2::name(state_v2::IDLE), "idle");
        assert_eq!(state_v2::name(0), "unknown");
        assert_eq!(state_v2::name(9), "unknown");
        assert_eq!(agent::name(agent::CODEX), "codex");
        assert_eq!(agent::name(agent::UNKNOWN), "unknown");
        // ENDED 三负值 + 退出码正值 + service_stopped 词面
        let (c, r) = dec_ended(&enc_ended(ended_code::SERVICE_STOPPED, ENDED_REASON_SERVICE_STOPPED));
        assert_eq!((c, r.as_str()), (ended_code::SERVICE_STOPPED, ENDED_REASON_SERVICE_STOPPED));
        let (c, _) = dec_ended(&enc_ended(42, ""));
        assert_eq!(c, 42);
        let (c, r) = dec_ended(&enc_ended(ended_code::REPLACED, ended_reason::REPLACED));
        assert_eq!((c, r.as_str()), (ended_code::REPLACED, ended_reason::REPLACED));
    }
}
