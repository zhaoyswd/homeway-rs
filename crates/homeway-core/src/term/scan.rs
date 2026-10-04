//! scan — 出口侧旁路扫描器（R6 6f；行为真源 = baseline 克隆 `pkg/term/modes.go`
//! 的 `termScan`，只读不消费字节）。
//!
//! 从 PTY 输出流维护：
//! ① 私有模式位掩码（legacy 客户端 attach 用；surface 路径读 vt 权威值）；
//! ② OSC 0/1/2 窗口标题（**标题的单一来源**——surface 快照标题与检测证据共用）；
//! ③ OSC 9 双语义：`9;4;<state>[;<pct>]` = progress（检测证据），裸 `9;<text>` = 通知；
//! ④ OSC 21337 `status=<value>` 状态直报（检测最高权威证据）。
//!
//! 关键约束：读缓冲边界不是协议边界 ⇒ 状态机能吃**跨 read 切断**的序列。

/// 模式位（与 App 侧 terminal 模块的 stream 层一一对应；= codec::mode_bits）。
use super::codec::mode_bits;

/// CSI 参数串上限（超长即放弃该序列，防内存放大）。
const MAX_CSI: usize = 64;
/// OSC 载荷上限。
const MAX_OSC: usize = 512;
/// 标题落库上限。
const TITLE_MAX: usize = 256;
/// progress / 直报状态这类短值上限。
const OSC_VALUE_MAX: usize = 64;

/// OSC 9;4 progress 的状态码（xterm 口径）。
pub const PROGRESS_CLEAR: i32 = 0;
pub const PROGRESS_NORMAL: i32 = 1;
pub const PROGRESS_ERROR: i32 = 2;
pub const PROGRESS_INDET: i32 = 3;
pub const PROGRESS_WARNING: i32 = 4;
/// progress 未给数值（0..100 之外的哨兵）。
pub const PROGRESS_VALUE_NONE: i32 = -1;

/// 最近一条 OSC 9;4 progress 载荷（检测证据通道）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Progress {
    pub state: i32,
    pub value: i32,
    pub ok: bool,
}

/// 一次扫描（每会话一个，由会话锁保护，与 pump 同线程喂）。
#[derive(Default)]
pub struct TermScan {
    st: ScanState,
    buf: Vec<u8>,
    too_long: bool,

    modes: u32,
    title: String,
    progress: Progress,
    osc_status: String,
    notify: String,
    /// OSC 7 上报的工作目录原始值（`file://<host><path>`；LIST 的 cwd 来源）。
    pwd: String,
    /// 当前标题**早于**本代前景 agent（切换 agent 时置位）：显示照旧用，
    /// 检测不得把上一进程的标题算进本进程判定。
    title_stale: bool,
    /// 本批字节里模式位/标题/证据有变化（调用方读后清零）。
    changed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum ScanState {
    #[default]
    None,
    Esc,
    Csi,
    Osc,
    OscEsc,
}

impl TermScan {
    pub fn new() -> Self {
        Self::default()
    }

    /// 喂一批 PTY 输出。
    pub fn write(&mut self, p: &[u8]) {
        for &b in p {
            self.byte_in(b);
        }
    }

    fn push(&mut self, b: u8, max: usize) {
        if self.buf.len() >= max {
            self.too_long = true;
            return;
        }
        self.buf.push(b);
    }

    fn byte_in(&mut self, b: u8) {
        match self.st {
            ScanState::None => {
                if b == 0x1b {
                    self.st = ScanState::Esc;
                }
            }
            ScanState::Esc => match b {
                b'[' => {
                    self.st = ScanState::Csi;
                    self.buf.clear();
                    self.too_long = false;
                }
                b']' => {
                    self.st = ScanState::Osc;
                    self.buf.clear();
                    self.too_long = false;
                }
                _ => self.st = ScanState::None,
            },
            ScanState::Csi => {
                if (0x40..=0x7e).contains(&b) {
                    // final byte
                    if !self.too_long {
                        self.finish_csi(b);
                    }
                    self.st = ScanState::None;
                    return;
                }
                self.push(b, MAX_CSI);
            }
            ScanState::Osc => match b {
                0x07 => {
                    // BEL
                    if !self.too_long {
                        self.finish_osc();
                    }
                    self.st = ScanState::None;
                }
                0x1b => self.st = ScanState::OscEsc,
                _ => self.push(b, MAX_OSC),
            },
            ScanState::OscEsc => {
                // ESC \ = ST；其它则把 ESC 当作序列结束（保守处理）。
                if b == b'\\' && !self.too_long {
                    self.finish_osc();
                }
                self.st = ScanState::None;
            }
        }
    }

    /// 处理 `CSI ? Pm h/l`（私有模式设置/复位）；其它 CSI 一律忽略。
    fn finish_csi(&mut self, final_byte: u8) {
        if final_byte != b'h' && final_byte != b'l' {
            return;
        }
        let params = self.buf.clone();
        if params.is_empty() || params[0] != b'?' {
            return;
        }
        let set = final_byte == b'h';
        let mut changed = false;
        for part in split_semi(&params[1..]) {
            let Ok(n) = parse_small_int(part) else { continue };
            let bit = match n {
                1 => mode_bits::DECCKM,
                47 | 1047 | 1049 => mode_bits::ALT_SCREEN,
                1000 => mode_bits::MOUSE_1000,
                1002 => mode_bits::MOUSE_1002,
                1003 => mode_bits::MOUSE_1003,
                1004 => mode_bits::FOCUS,
                1006 => mode_bits::MOUSE_1006,
                2004 => mode_bits::BRACKETED,
                _ => continue,
            };
            let before = self.modes;
            if set {
                self.modes |= bit;
            } else {
                self.modes &= !bit;
            }
            changed |= before != self.modes;
        }
        self.changed |= changed;
    }

    /// 按 OSC 命令号分派。OSC 9 双语义：只有 `9;4;…` 是 progress，其余 = 通知。
    fn finish_osc(&mut self) {
        let payload = self.buf.clone();
        if payload.is_empty() {
            return;
        }
        let (code, rest) = match payload.iter().position(|&b| b == b';') {
            Some(i) => (&payload[..i], &payload[i + 1..]),
            None => (&payload[..], &payload[payload.len()..]),
        };
        match code {
            b"0" | b"1" | b"2" => self.set_title(String::from_utf8_lossy(rest).as_ref()),
            b"7" => self.set_pwd(String::from_utf8_lossy(rest).as_ref()),
            b"9" => self.finish_osc9(&String::from_utf8_lossy(rest)),
            b"21337" => self.finish_osc21337(&String::from_utf8_lossy(rest)),
            _ => {}
        }
    }

    /// OSC 7：工作目录上报（原始值原样存；剥路径在 [`pwd_path`]——Go vtPwdPath 同款）。
    fn set_pwd(&mut self, raw: &str) {
        let v = sanitize_value(raw, 4096);
        if v != self.pwd {
            self.pwd = v;
        }
    }

    /// OSC 9：`4;<state>[;<pct>]` = progress；其余 = 通知（不进检测证据）。
    fn finish_osc9(&mut self, rest: &str) {
        if !rest.starts_with("4;") {
            let text = sanitize_value(rest, TITLE_MAX);
            if !text.is_empty() && text != self.notify {
                self.notify = text;
                self.changed = true;
            }
            return;
        }
        let parts = split_semi(&rest.as_bytes()[2..]);
        let Some(Ok(state)) = parts.first().map(|p| parse_small_int(p)) else {
            return;
        };
        if !(0..=4).contains(&state) {
            return;
        }
        let mut value = PROGRESS_VALUE_NONE;
        if let Some(p) = parts.get(1) {
            if !p.is_empty() {
                if let Ok(v) = parse_small_int(p) {
                    if (0..=100).contains(&v) {
                        value = v;
                    }
                }
            }
        }
        if self.progress.ok && self.progress.state == state && self.progress.value == value {
            return;
        }
        self.progress = Progress { state, value, ok: true };
        self.changed = true;
    }

    /// Fig/Amazon Q 状态直报：载荷形如 `status=<value>`（可带其它键，';' 分隔）。
    fn finish_osc21337(&mut self, rest: &str) {
        for kv in rest.split(';') {
            let Some((key, val)) = kv.split_once('=') else { continue };
            if key != "status" {
                continue;
            }
            let val = sanitize_value(val, OSC_VALUE_MAX);
            if val.is_empty() || val == self.osc_status {
                return;
            }
            self.osc_status = val;
            self.changed = true;
            return;
        }
    }

    /// 更新标题（OSC 0/1/2 共用；本代新标题自动让 stale 失效）。
    fn set_title(&mut self, raw: &str) {
        let title = sanitize_title(raw);
        if title == self.title && !self.title_stale {
            return;
        }
        self.title = title;
        self.title_stale = false;
        self.changed = true;
    }

    /// 清空 OSC 证据：前景 agent 变化时调用（旧 agent 的 progress/直报不得参与新 agent
    /// 判定）。标题保留显示但标 stale（检测侧读 [`Self::title_evidence`] 拿不到）。
    pub fn clear_osc_evidence(&mut self) {
        let had = self.progress.ok || !self.osc_status.is_empty() || !self.title_stale;
        self.progress = Progress::default();
        self.osc_status.clear();
        self.title_stale = true;
        if had {
            self.changed = true;
        }
    }

    /// 可作检测证据的标题（早于本代 agent 时返回空串）。
    pub fn title_evidence(&self) -> &str {
        if self.title_stale {
            ""
        } else {
            self.title.as_str()
        }
    }

    /// 显示用标题（不管 stale）。
    pub fn title(&self) -> &str {
        &self.title
    }

    /// 最近一条 OSC 9;4 progress（ok=false = 本代还没有过）。
    pub fn progress(&self) -> Progress {
        self.progress
    }

    /// 最近一条 OSC 21337 直报状态（空 = 无）。
    pub fn osc_status(&self) -> &str {
        &self.osc_status
    }

    /// 最近一条裸 OSC 9 通知文本（空 = 无）。
    pub fn notify(&self) -> &str {
        &self.notify
    }

    /// OSC 7 原始值（`file://…`；未上报 = 空）。
    pub fn pwd(&self) -> &str {
        &self.pwd
    }

    /// 剥成文件系统路径（Go `vtPwdPath` 同款）：`file://<host>/<path>` 取 host 后
    /// 第一个 '/' 起；百分号转义按 URI 解码；非 file:// / 无路径 = 空。
    pub fn pwd_path(&self) -> String {
        let Some(rest) = self.pwd.strip_prefix("file://") else {
            return String::new();
        };
        let Some(i) = rest.find('/') else {
            return String::new();
        };
        let path = &rest[i..];
        if path.is_empty() {
            return String::new();
        }
        if path.contains('%') {
            if let Some(d) = percent_decode(path) {
                return d;
            }
        }
        path.to_string()
    }

    /// legacy 模式位掩码。
    pub fn modes(&self) -> u32 {
        self.modes
    }

    /// 本批是否有变化；读后清零。
    pub fn take_changed(&mut self) -> bool {
        std::mem::take(&mut self.changed)
    }
}

/// 去控制字符并按上限截断（远端可随便发值，别让它撑爆元数据）。
/// 逐字节与 Go 同款（控制字节过滤不破坏 UTF-8——续字节恒 ≥ 0x80；截断也按字节对齐）。
fn sanitize_value(v: &str, max: usize) -> String {
    let mut out = Vec::with_capacity(v.len());
    for &b in v.as_bytes() {
        if b < 0x20 || b == 0x7f {
            continue;
        }
        out.push(b);
        if out.len() >= max {
            break;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// URI 百分号解码（Go `url.PathUnescape` 的路径面近似：`%HH` 两位十六进制；
/// '+' 不当空格——那是 query 面语义）。非法转义返回 None（调用方回落原串）。
fn percent_decode(v: &str) -> Option<String> {
    let bytes = v.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if i + 2 >= bytes.len() {
                return None; // 残缺转义
            }
            let hex = |b: u8| -> Option<u8> {
                match b {
                    b'0'..=b'9' => Some(b - b'0'),
                    b'a'..=b'f' => Some(b - b'a' + 10),
                    b'A'..=b'F' => Some(b - b'A' + 10),
                    _ => None,
                }
            };
            let h = hex(bytes[i + 1])?;
            let l = hex(bytes[i + 2])?;
            out.push(h << 4 | l);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    Some(String::from_utf8_lossy(&out).into_owned())
}

fn sanitize_title(t: &str) -> String {
    sanitize_value(t, TITLE_MAX)
}

fn split_semi(s: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut start = 0usize;
    for (i, &b) in s.iter().enumerate() {
        if b == b';' {
            out.push(&s[start..i]);
            start = i + 1;
        }
    }
    out.push(&s[start..]);
    out
}

/// 只接受 0..99999 的十进制（CSI/OSC 短参数），其余无效。
fn parse_small_int(s: &[u8]) -> Result<i32, ()> {
    if s.is_empty() || s.len() > 5 {
        return Err(());
    }
    let mut n: i32 = 0;
    for &b in s {
        if !b.is_ascii_digit() {
            return Err(());
        }
        n = n * 10 + i32::from(b - b'0');
    }
    Ok(n)
}

/// progress 还原成 OSC 9;4 载荷形态（region osc_progress 的判据是 `^4;0` 前缀）。
pub fn progress_payload(p: Progress) -> String {
    if !p.ok {
        return String::new();
    }
    if p.value < 0 {
        format!("4;{}", p.state)
    } else {
        format!("4;{};{}", p.state, p.value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn osc(seq: &str) -> String {
        format!("\x1b]{seq}\x07")
    }

    #[test]
    fn scan_title_and_stale() {
        let mut s = TermScan::new();
        s.write(osc("2;my-title").as_bytes());
        assert_eq!(s.title(), "my-title");
        assert_eq!(s.title_evidence(), "my-title");
        assert!(s.take_changed());
        // 同标题不置 changed
        s.write(osc("0;my-title").as_bytes());
        assert!(!s.take_changed());
        // agent 切换：证据清、标题保留显示但 stale
        s.clear_osc_evidence();
        assert_eq!(s.title(), "my-title", "标题保留显示");
        assert_eq!(s.title_evidence(), "", "证据侧拿不到");
        // 本代新标题让 stale 失效
        s.write(osc("2;new").as_bytes());
        assert_eq!(s.title_evidence(), "new");
    }

    #[test]
    fn scan_progress_and_notify_dual_semantics() {
        let mut s = TermScan::new();
        // progress：4;<state>[;<pct>]
        s.write(osc("9;4;1;50").as_bytes());
        assert_eq!(s.progress(), Progress { state: 1, value: 50, ok: true });
        assert_eq!(progress_payload(s.progress()), "4;1;50");
        // 同值不置 changed
        assert!(s.take_changed());
        s.write(osc("9;4;1;50").as_bytes());
        assert!(!s.take_changed());
        // 无百分比形态
        s.write(osc("9;4;3").as_bytes());
        assert_eq!(progress_payload(s.progress()), "4;3");
        // 非法 state（>4）忽略
        s.write(osc("9;4;9").as_bytes());
        assert_eq!(s.progress().state, 3, "非法载荷不覆盖");
        // 裸 9 = 通知
        s.write(osc("9;构建完成").as_bytes());
        assert_eq!(s.notify(), "构建完成");
        assert_eq!(s.progress().state, 3, "通知不进证据");
        // 切换 agent 清证据
        s.clear_osc_evidence();
        assert_eq!(s.progress(), Progress::default());
    }

    #[test]
    fn scan_osc21337_status_report() {
        let mut s = TermScan::new();
        s.write(osc("21337;other=k;status=working").as_bytes());
        assert_eq!(s.osc_status(), "working");
        assert!(s.take_changed(), "首条直报置位");
        // 同值不置 changed；其它键不算
        s.write(osc("21337;status=working").as_bytes());
        assert!(!s.take_changed());
        s.write(osc("21337;foo=bar").as_bytes());
        assert_eq!(s.osc_status(), "working", "无 status 键不动");
        // ST 终止形态 + 跨块切断
        let mut s2 = TermScan::new();
        s2.write(b"\x1b]21337;status=bl");
        assert_eq!(s2.osc_status(), "");
        s2.write(b"ocked\x1b\\");
        assert_eq!(s2.osc_status(), "blocked");
    }

    #[test]
    fn scan_private_modes() {
        let mut s = TermScan::new();
        s.write(b"\x1b[?1000;2004h\x1b[?1049h");
        assert_eq!(s.modes(), mode_bits::MOUSE_1000 | mode_bits::BRACKETED | mode_bits::ALT_SCREEN);
        s.write(b"\x1b[?1049l");
        assert_eq!(s.modes(), mode_bits::MOUSE_1000 | mode_bits::BRACKETED);
        // 非私有（无 ?）与未知模式号忽略
        s.write(b"\x1b[4h\x1b[?9999h");
        assert_eq!(s.modes(), mode_bits::MOUSE_1000 | mode_bits::BRACKETED);
        // 超长 CSI 放弃
        let mut s2 = TermScan::new();
        let long = format!("\x1b[?{}\x1b", "1".repeat(100));
        s2.write(long.as_bytes());
        // ESC 后的 [1h 是新序列：无 ? 前缀 → 不动模式位
        assert_eq!(s2.modes(), 0);
    }

    #[test]
    fn scan_csi_split_across_reads() {
        let mut s = TermScan::new();
        s.write(b"\x1b");
        s.write(b"[");
        s.write(b"?1");
        s.write(b"h");
        assert_eq!(s.modes(), mode_bits::DECCKM, "跨 read 切断仍命中");
    }

    #[test]
    fn scan_sanitize_control_chars() {
        let mut s = TermScan::new();
        s.write(osc("2;ab\x01cd\x7f").as_bytes());
        assert_eq!(s.title(), "abcd");
        // 长标题截断（256 上限）
        let mut s2 = TermScan::new();
        let long = "x".repeat(400);
        s2.write(osc(&format!("2;{long}")).as_bytes());
        assert_eq!(s2.title().len(), 256);
    }
}
