//! responder — 自建应答器（R6 6c）。
//!
//! Rust 生态无服务端终端应答实现 ⇒ 按 libghostty-vt 语义自建。**行为真源 =
//! `fixtures/vectors/term_responder.json` / `term_palette.json`**（克隆 harness 实测采的
//! ghostty 默认应答，43+4 案逐字节对拍），识别条件对齐 libghostty-vt `stream.zig`：
//!
//! - DECRQM 只认 `?$` 双 intermediate（ANSI 面 `CSI Pm $ p` 结构性不答）；
//! - XTVERSION（`CSI ? 65 c`，param≠0）vte 状态机天然不分发 ⇒ 不答；
//! - OSC 10/11/12 未设主题（SetDefaultColors 未调）不答；12（光标色）答前景值；
//! - 应答终止符跟随查询（BEL/ST）；
//! - CSI 14t/16t/18t/21t、XTQMODKEYS、ENQ 不答（alacritty 侧由 VoidListener 吞事件）。
//!
//! 模式位表（`DecModes`）：alacritty TermMode 只覆盖 DEC 模式子集，DECRQM 需要完整的
//! ghostty 模式面（modes.zig entries 的 DEC 侧）⇒ 自管 38 位表 + 默认值；特判 117 =
//! permanently_reset(4)、5522 = not_recognized(0)（Go 侧恒不装 clipboard_read effect）。
//!
//! 并发：全部状态挂在 [`crate::term::vt::SessionVt`] 上，由会话锁串行（同 vt 纪律）。

use std::sync::OnceLock;

/// ghostty DECRQM 模式号 → 表内序号/默认值（modes.zig entries 的 DEC 侧，顺序照抄）。
/// `(模式号, 默认值)`；默认值与 ghostty `ModeEntry.default` 一致。
const DEC_MODES: &[(u16, bool)] = &[
    (1, false),     // cursor_keys（DECCKM）
    (3, false),     // 132_column
    (4, false),     // slow_scroll
    (5, false),     // reverse_colors
    (6, false),     // origin（DECOM）
    (7, true),      // wraparound
    (8, false),     // autorepeat
    (9, false),     // mouse_event_x10（D-18：alacritty 无位，自管记账）
    (12, false),    // cursor_blinking
    (25, true),     // cursor_visible
    (40, false),    // enable_mode_3
    (45, false),    // reverse_wrap
    (47, false),    // alt_screen_legacy
    (66, false),    // keypad_keys
    (67, false),    // backarrow_key_mode（DECBKM）
    (69, false),    // enable_left_and_right_margin（DECLRMM）
    (1000, false),  // mouse_event_normal
    (1002, false),  // mouse_event_button
    (1003, false),  // mouse_event_any
    (1004, false),  // focus_event
    (1005, false),  // mouse_format_utf8
    (1006, false),  // mouse_format_sgr
    (1007, true),   // mouse_alternate_scroll
    (1015, false),  // mouse_format_urxvt（D-18）
    (1016, false),  // mouse_format_sgr_pixels（D-18）
    (1035, true),   // ignore_keypad_with_numlock
    (1036, true),   // alt_esc_prefix
    (1039, false),  // alt_sends_escape
    (1045, false),  // reverse_wrap_extended
    (1047, false),  // alt_screen
    (1048, false),  // save_cursor
    (1049, false),  // alt_screen_save_cursor_clear_enter
    (2004, false),  // bracketed_paste
    (2026, false),  // synchronized_output
    (2027, false),  // grapheme_cluster
    (2031, false),  // report_color_scheme
    (2033, false),  // report_visibility
    (2048, false),  // in_band_size_reports
];

/// ghostty 全 DEC 模式自管表（DECRQM 应答的唯一依据；屏态变化仍由 alacritty TermMode
/// 承担——本表只回答「终端认不认这个模式/现在是开是关」）。
#[derive(Debug, Clone)]
pub struct DecModes {
    values: [bool; DEC_MODES.len()],
}

impl Default for DecModes {
    fn default() -> Self {
        DecModes { values: std::array::from_fn(|i| DEC_MODES[i].1) }
    }
}

impl DecModes {
    /// 模式号 → 表内序号。
    fn index_of(mode: u16) -> Option<usize> {
        DEC_MODES.iter().position(|&(m, _)| m == mode)
    }

    pub fn set(&mut self, mode: u16, value: bool) {
        if let Some(i) = Self::index_of(mode) {
            self.values[i] = value;
        }
    }

    /// 模式号 → 当前值（表外模式恒 false）。
    pub fn get(&self, mode: u16) -> bool {
        Self::index_of(mode).is_some_and(|i| self.values[i])
    }

    /// DEC 1049/1047/1048 的互斥联动（alt_screen 族）：1049/1047 置位 ⇒ 各自位记录；
    /// 具体屏态由 alacritty 处理，这里只管 DECRQM 报告面。
    pub fn set_alt_screen(&mut self, on: bool) {
        self.set(1047, on);
        self.set(1049, on);
    }

    /// DECRQM 状态码：0=未识别 / 1=set / 2=reset / 4=永久 reset。
    pub fn decrqm_state(&self, mode: u16) -> u8 {
        if mode == 117 {
            return 4; // DECECM：行为固定等价 reset（modes.zig getReport 特判）
        }
        if mode == 5522 {
            return 0; // kitty_paste_events：无 clipboard_read ⇒ not_recognized（requestMode 特判）
        }
        match Self::index_of(mode) {
            Some(i) if self.values[i] => 1,
            Some(_) => 2,
            None => 0,
        }
    }
}

/// OSC 4 未 set 索引的应答基表：ghostty 内置 256 色（`term_palette.json` 单真源，
/// include 直嵌——升级基线重跑向量即同步）。解析一次，形如 `rgb:rrrr/gggg/bbbb`。
pub fn ghostty_palette() -> &'static [[u8; 3]; 256] {
    static PALETTE: OnceLock<[[u8; 3]; 256]> = OnceLock::new();
    PALETTE.get_or_init(|| {
        let raw = include_str!("../../../../fixtures/vectors/term_palette.json");
        let v: serde_json::Value = serde_json::from_str(raw).expect("term_palette.json 合法");
        let mut out = [[0u8; 3]; 256];
        let entries = v.get("osc4_unset").and_then(|x| x.as_array()).expect("osc4_unset 数组");
        assert_eq!(entries.len(), 256, "调色板须 256 项");
        for (i, e) in entries.iter().enumerate() {
            let s = e.as_str().expect("调色板项为串");
            let hex = s.strip_prefix("rgb:").expect("rgb: 前缀");
            // rrrr/gggg/bbbb：16bit 通道取高 8 位（8bit×257 编码的可逆截断）
            for (c, part) in out[i].iter_mut().zip(hex.split('/')) {
                *c = u8::from_str_radix(&part[..2], 16).expect("通道 hex");
            }
        }
        out
    })
}

/// 动态色三件（OSC 10/11/12 面）：`None` = 未设置 ⇒ 查询不答（V-2 定案）。
/// cursor（12）查询回落前景值（实测 osc12_theme 答 fg）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DynamicColors {
    pub fg: Option<[u8; 3]>,
    pub bg: Option<[u8; 3]>,
    pub cursor: Option<[u8; 3]>,
}

impl DynamicColors {
    /// OSC 码（10/11/12）→ 应答色；12 无独立值时回落 fg。
    pub fn answer(&self, code: u16) -> Option<[u8; 3]> {
        match code {
            10 => self.fg,
            11 => self.bg,
            12 => self.cursor.or(self.fg),
            _ => None,
        }
    }

    /// OSC 码 set 分量。
    pub fn set(&mut self, code: u16, rgb: [u8; 3]) {
        match code {
            10 => self.fg = Some(rgb),
            11 => self.bg = Some(rgb),
            12 => self.cursor = Some(rgb),
            _ => {}
        }
    }
}

/// 颜色应答体：`rgb:rrrr/gggg/bbbb`（16bit/通道 = 8bit×257）。
pub fn color_report(rgb: [u8; 3]) -> String {
    let [r, g, b] = rgb;
    format!("rgb:{r:02x}{r:02x}/{g:02x}{g:02x}/{b:02x}{b:02x}")
}

/// DA 族应答（`identify_terminal` 拦截面）。intermediate 来源 = vte csi_dispatch 的
/// intermediates.first()（`>`/`=`）；None = DA1。
pub fn device_attributes(intermediate: Option<char>) -> Vec<u8> {
    match intermediate {
        None => b"\x1b[?62;22c".to_vec(),
        Some('>') => b"\x1b[>1;0;0c".to_vec(),
        Some('=') => b"\x1bP!|00000000\x1b\\".to_vec(),
        _ => Vec::new(), // vte 只传 '>'/'='/None；空 = 不答（与 XTVERSION 同路）
    }
}

/// DSR 应答（`device_status` 拦截面）。CPR 按 DECOM 折算（ghostty：origin 态报滚动区
/// 相对坐标；实测 dsr_cpr_origin_scrolled）。`cursor` = (y0, x0)（0-based 视口坐标），
/// `scroll_top` = 滚动区顶（0-based 视口行）。
pub fn device_status(arg: usize, cursor: (i32, usize), origin: bool, scroll_top: i32) -> Vec<u8> {
    match arg {
        5 => b"\x1b[0n".to_vec(),
        6 => {
            let (y, x) = cursor;
            let y = if origin { y - scroll_top } else { y };
            format!("\x1b[{};{}R", y + 1, x + 1).into_bytes()
        }
        _ => Vec::new(),
    }
}

/// DECRQM 应答体（`report_private_mode` 拦截面）。
pub fn decrqm(mode: u16, state: u8) -> Vec<u8> {
    format!("\x1b[?{mode};{state}$y").into_bytes()
}

/// kitty 键盘查询应答（`report_keyboard_mode` 拦截面）：`\x1b[?<flags>u`（0 也显式）。
pub fn kitty_query(flags: u8) -> Vec<u8> {
    format!("\x1b[?{flags}u").into_bytes()
}

/// DECRQSS 应答所需的当前态快照（由 [`crate::term::vt::SessionVt`] 组装——
/// alacritty 类型不透出本模块）。
pub struct DecrqssView {
    /// SGR 笔态串（ghostty `printAttributes` 形态，恒 "0" 起头；不含 'm'）。
    pub sgr: String,
    /// DECSCUSR 数值（1..6）。
    pub decscusr: u8,
    /// DECSTBM（1-based top/bottom）。
    pub scroll_region: (u32, u32),
}

/// DECRQSS 应答（旁路扫描器面，门一评审 F3；载荷 0..2 字节，ghostty `dcs.zig`）：
/// - `m`（SGR）→ `DCS 1 $r {sgr}m ST`；` q`（DECSCUSR）→ `1 $r {n} q`；
///   `r`（DECSTBM）→ `1 $r {top};{bottom}r`；
/// - `s`（DECSLRM）恒答 **0**（invalid）——alacritty 不跟踪左右边距（DECLRMM
///   面 69 有记账但边距无从取；真应用不开 69，登记差异）；
/// - 其余/空载荷 → `DCS 0 $r ST`。
///
/// 3+ 字节载荷在扫描器层就不构成完整模式（ghostty 第 3 个 put 即丢弃且不应答）。
pub fn decrqss(payload: &[u8], view: &DecrqssView) -> Vec<u8> {
    let body: String = match payload {
        b"m" => format!("{}m", view.sgr),
        b" q" => format!("{} q", view.decscusr),
        b"r" => format!("{};{}r", view.scroll_region.0, view.scroll_region.1),
        b"s" => String::new(), // DECSLRM：恒 invalid（见上）
        _ => String::new(),
    };
    if body.is_empty() {
        b"\x1bP0$r\x1b\\".to_vec()
    } else {
        format!("\x1bP1$r{body}\x1b\\").into_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::term::vt::SessionVt;

    /// 单案（含分片序列）：name / setup / query / themed / 期望应答 hex / chunks。
    type ResponderCaseFull = (String, Vec<u8>, Vec<u8>, bool, String, Vec<Vec<u8>>);

    /// 读应答向量：name / setup / query / themed / 期望应答 hex / 期望分片序列。
    /// `chunks` 是「每个应答一片、顺序与流中触发一致」的判据（r1-低11：只比拼接
    /// 字节抓不到分片边界错）。
    fn responder_cases() -> Vec<ResponderCaseFull> {
        let raw = include_str!("../../../../fixtures/vectors/term_responder.json");
        let v: serde_json::Value = serde_json::from_str(raw).expect("term_responder.json invalid");
        v.get("cases")
            .and_then(|c| c.as_array())
            .expect("cases array")
            .iter()
            .map(|c| {
                let hex = |f: &str| -> Vec<u8> {
                    let h = c.get(f).and_then(|x| x.as_str()).unwrap_or("");
                    (0..h.len())
                        .step_by(2)
                        .map(|i| u8::from_str_radix(&h[i..i + 2], 16).expect("hex"))
                        .collect()
                };
                (
                    c.get("name").and_then(|x| x.as_str()).expect("name").to_string(),
                    hex("setup_hex"),
                    hex("query_hex"),
                    c.get("themed").and_then(|x| x.as_bool()).unwrap_or(false),
                    c.get("response_hex").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                    c.get("chunks")
                        .and_then(|x| x.as_array())
                        .map(|a| a.iter().map(|x| hex_of(x.as_str().unwrap_or(""))).collect())
                        .unwrap_or_default(),
                )
            })
            .collect()
    }

    fn hex_of(h: &str) -> Vec<u8> {
        (0..h.len()).step_by(2).map(|i| u8::from_str_radix(&h[i..i + 2], 16).expect("hex")).collect()
    }

    /// 6c 判据：全部应答向量逐案对拍（喂 SessionVt → write_collecting → 比对字节
    /// **与分片序列**）+ 按 chunks 切点拆投喂的跨块续接轮。F2/F3（?998n 与 DECRQSS
    /// 三态）已由旁路扫描器实装，四案入对拍。
    #[test]
    fn responder_parity_with_go_vectors() {
        let cases = responder_cases();
        assert!(cases.len() >= 57, "vector cases {}", cases.len());
        let mut failures = Vec::new();
        for (name, setup, query, themed, expect, chunks) in &cases {
            let mut vt = SessionVt::new(100, 32, 1000).expect("vt");
            if *themed {
                vt.set_default_colors([0x11, 0x22, 0x33], [0xaa, 0xbb, 0xcc]);
            }
            if !setup.is_empty() {
                vt.write(setup);
            }
            // 逐片收集：分片边界/片数也是契约（r1-低11）
            let mut pieces: Vec<Vec<u8>> = Vec::new();
            vt.write_collecting(query, &mut |p| pieces.push(p.to_vec()));
            let joined = pieces.concat();
            let got = joined.iter().map(|b| format!("{b:02x}")).collect::<String>();
            if got != expect.as_str() {
                failures.push(format!("{name}: expect {expect} got {got}"));
            }
            // 分片契约：piece 边界只允许比 chunks 更细（vte 对多索引 OSC 4 逐参分发，
            // ghostty 合成一条应答——拼接字节才是 wire 契约，r1-低11 的放宽形态）：
            // 每片必须完整落在某个期望 chunk 内（顺序 + 不跨界）。
            if !chunks.is_empty() && !pieces_cover_chunks(&pieces, chunks) {
                failures.push(format!("{name}: 分片序列不符（片序/跨界，chunks 契约）"));
            }
            // 跨块轮：按每个 chunks 边界与固定切点 1/2/3 拆投喂，拼接字节必须同
            if query.len() >= 2 {
                for split in [1usize, 2, 3, query.len() / 2].into_iter().filter(|s| *s < query.len()) {
                    let mut vt2 = SessionVt::new(100, 32, 1000).expect("vt");
                    if *themed {
                        vt2.set_default_colors([0x11, 0x22, 0x33], [0xaa, 0xbb, 0xcc]);
                    }
                    if !setup.is_empty() {
                        vt2.write(setup);
                    }
                    let mut got2: Vec<u8> = Vec::new();
                    vt2.write_collecting(&query[..split], &mut |p| got2.extend_from_slice(p));
                    vt2.write_collecting(&query[split..], &mut |p| got2.extend_from_slice(p));
                    if got2 != joined {
                        failures.push(format!("{name}: 跨块切点 {split} 应答漂移"));
                    }
                }
            }
        }
        assert!(failures.is_empty(), "parity failed {}:\n{}", failures.len(), failures.join("\n"));
    }

    /// pieces 逐片完整落在 chunks 的分段内（允许一片 chunk 被切成多 piece，
    /// 不允许一片 piece 横跨两个 chunk 或乱序）。
    fn pieces_cover_chunks(pieces: &[Vec<u8>], chunks: &[Vec<u8>]) -> bool {
        let (mut ci, mut off) = (0usize, 0usize);
        for p in pieces {
            let cur = chunks.get(ci).map(|c| &c[off.min(c.len())..]).unwrap_or(&[]);
            if cur.len() < p.len() || cur[..p.len()] != p[..] {
                return false;
            }
            off += p.len();
            if let Some(c) = chunks.get(ci) {
                if off >= c.len() {
                    ci += 1;
                    off = 0;
                }
            }
        }
        ci == chunks.len() && off == 0
    }

    #[test]
    fn da_family_bytes() {
        assert_eq!(device_attributes(None), b"\x1b[?62;22c");
        assert_eq!(device_attributes(Some('>')), b"\x1b[>1;0;0c");
        assert_eq!(device_attributes(Some('=')), b"\x1bP!|00000000\x1b\\");
        assert!(device_attributes(Some('?')).is_empty());
    }

    #[test]
    fn dsr_cpr_origin_folds_scroll_top() {
        // 向量 dsr_cpr：setup 3;7H（无 origin）→ [3;7R
        assert_eq!(device_status(6, (2, 6), false, 0), b"\x1b[3;7R");
        // 向量 dsr_cpr_origin：origin + 5;10H → [5;10R（滚动区默认顶 0）
        assert_eq!(device_status(6, (4, 9), true, 0), b"\x1b[5;10R");
        // 向量 dsr_cpr_origin_scrolled：滚动区 5..20（0-based top=4）+ origin + 3;4H
        // ⇒ 光标绝对行 = 4+2 = 6，应答 6-4+1 = 3
        assert_eq!(device_status(6, (6, 3), true, 4), b"\x1b[3;4R");
    }

    #[test]
    fn decrqm_table_semantics() {
        let mut m = DecModes::default();
        assert_eq!(m.decrqm_state(1000), 2, "默认 reset");
        m.set(1000, true);
        assert_eq!(m.decrqm_state(1000), 1);
        assert_eq!(m.decrqm_state(9999), 0, "表外未识别");
        assert_eq!(m.decrqm_state(117), 4, "DECECM 永久 reset");
        assert_eq!(m.decrqm_state(5522), 0, "kitty_paste 未装 read");
        assert_eq!(m.decrqm_state(7), 1, "wraparound 默认 set");
        assert_eq!(decrqm(1000, 1), b"\x1b[?1000;1$y");
    }

    #[test]
    fn palette_loads_and_matches_samples() {
        let p = ghostty_palette();
        // 向量样本：idx0=1d1d1f21 21（≈ #1d1f21）、idx1=cccc66（#cc6666）、idx5=b2b29494bbbb
        assert_eq!(p[0], [0x1d, 0x1f, 0x21]);
        assert_eq!(p[1], [0xcc, 0x66, 0x66]);
        assert_eq!(p[5], [0xb2, 0x94, 0xbb]);
        assert_eq!(p[255], [0xee, 0xee, 0xee]);
    }

    #[test]
    fn color_report_doubles_channels() {
        assert_eq!(color_report([0x11, 0x22, 0x33]), "rgb:1111/2222/3333");
    }

    #[test]
    fn dynamic_colors_fallbacks() {
        let mut c = DynamicColors { fg: Some([1, 2, 3]), bg: None, cursor: None };
        assert_eq!(c.answer(10), Some([1, 2, 3]));
        assert_eq!(c.answer(11), None, "未设 bg 不答");
        assert_eq!(c.answer(12), Some([1, 2, 3]), "cursor 回落 fg");
        c.set(12, [9, 9, 9]);
        assert_eq!(c.answer(12), Some([9, 9, 9]));
    }
}
