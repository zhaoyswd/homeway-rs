//! vt — 会话屏态的仿真底座（alacritty_terminal 0.26 适配层，R6 6b）。
//!
//! 对齐 Go `pkg/term/vt`（libghostty-vt cgo 绑定）的**读面**：视口网格、脏行、光标、
//! 模式位、回滚条、镜像窗口、绝对行号拉取。差异与归一化口径全部收在本文件
//! （设计 D-1..D-7，`docs/reviews/R6-design.md` §一）：
//!
//! - **坐标系**：alacritty `Line(0)` = 视口顶（负数进回滚）——与 Go render state 同向，
//!   换算只发生在「绝对行号 ⇔ Line」一处（`line_of_abs`）。
//! - **cell 归一化**：alacritty 空白格 = `' '` + Named 前景/背景 + 无样式位 ⇒ 归一化为
//!   Go 的「无字素空白格」（symbol 空、颜色 None）；zerowidth 组合字符并进 symbol。
//! - **模式位缺口**：X10 鼠标（DEC 9）/URXVT（1015）/光标闪烁独立位无对应 ⇒ 恒 false
//!   （D-4/D-7 登记）；kitty 键盘协议五位从 TermMode 拼装。
//! - **damage**：`Full`（滚动常态）⇒ 全视口行；`Partial` ⇒ 标脏行（含光标行 = 安全超集，
//!   D-6）。`reset_damage` 由调用方在载荷成功下发后调（乐观消费 + 全量兜底，同 Go）。
//!
//! 并发：本类型不带锁——由会话层的会话锁串行（同 Go 的 sessionVT 纪律）。

use alacritty_terminal::event::{EventListener, VoidListener};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line, Point};
use alacritty_terminal::term::cell::{Cell as AlacCell, Flags as AlacFlags};
use alacritty_terminal::term::{Config, Term, TermDamage, TermMode};
use alacritty_terminal::vte::ansi::{
    self as alac_ansi, Color as AlacColor, CursorShape as AlacCursorShape, NamedColor,
};

use super::keyenc;
use super::responder;

/// 回滚行数上限默认值（Go `vt.DefaultScrollbackLines` 同值；env
/// `HOMEWAY_TERM_SCROLLBACK_LINES` 由会话层注入）。
pub const DEFAULT_SCROLLBACK_LINES: usize = 10_000;

/// 光标形状（wire 值 = Go `vt.CursorShape`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CursorShape {
    #[default]
    Bar = 0,
    Block = 1,
    Underline = 2,
    BlockHollow = 3,
}

/// 光标快照（视口坐标，回滚偏移已折算——本层 display_offset 恒 0）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Cursor {
    pub x: u16,
    pub y: u16,
    pub visible: bool,
    pub blinking: bool,
    /// ghostty 的密码输入推断位：alacritty 无对应面 ⇒ 恒 false（D-7）。
    pub password: bool,
    pub wide_tail: bool,
    pub shape: CursorShape,
}

/// 回滚条（行号空间 = 绝对行 [0, total)，视口占 [offset, offset+len)）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Scrollbar {
    pub total: u64,
    pub offset: u64,
    pub len: u64,
}

impl Scrollbar {
    /// 视口是否贴底（= 跟随输出）。
    pub fn at_bottom(&self) -> bool {
        self.offset + self.len >= self.total
    }
}

/// 当前活动屏。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Screen {
    #[default]
    Primary = 0,
    Alternate = 1,
}

/// 鼠标上报事件面（门一评审 B5：**编码面是 last-set 单值语义**，与 wire 位的独立记账
/// 并存——ghostty `flags.mouse_event` 只记最后一次 set/reset 的模式；Go 实测
/// `?1000h ?1002h` 与 `?1002h ?1000h` 的 wire 位相同但 motion 上报行为不同）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MouseTracking {
    #[default]
    None,
    X10,        // DEC 9（ghostty 有独立位；alacritty 无 ⇒ 自管，D-18）
    Clicks,     // DEC 1000
    CellMotion, // DEC 1002
    AllMotion,  // DEC 1003
}

/// 鼠标上报字节格式（同 B5：last-set 单值；unset 回默认 X10 三字节）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MouseFormat {
    #[default]
    Default, // X10 三字节
    Utf8,    // DEC 1005
    Sgr,     // DEC 1006
    Urxvt,   // DEC 1015（D-18：alacritty 无位 ⇒ 自管）
}

/// 模式位快照（字段面对齐 Go `vt.Modes`；注释给 DEC/ANSI 模式号）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Modes {
    pub screen: Screen,

    pub cursor_keys_app: bool,  // DEC 1
    pub keypad_app: bool,       // DEC 66
    pub bracketed_paste: bool,  // DEC 2004
    pub focus_events: bool,     // DEC 1004

    pub mouse_x10: bool,   // DEC 9（alacritty 无位 ⇒ 恒 false，D-4）
    pub mouse_normal: bool, // DEC 1000
    pub mouse_button: bool, // DEC 1002
    pub mouse_any: bool,    // DEC 1003
    pub mouse_sgr: bool,    // DEC 1006
    pub mouse_utf8: bool,   // DEC 1005
    pub mouse_urxvt: bool,  // DEC 1015（alacritty 无位 ⇒ 恒 false，D-4）
    pub alt_scroll: bool,   // DEC 1007

    pub cursor_visible: bool, // DEC 25
    pub cursor_blink: bool,   // DEC 12（按光标样式带不带 blink 位近似）

    pub insert: bool,     // ANSI 4
    pub origin: bool,     // DEC 6
    pub wraparound: bool, // DEC 7

    /// kitty 键盘协议标志（位值 = 协议位：1/2/4/8/16）。
    pub kitty_flags: u8,
    /// xterm modifyOtherKeys mode 2（vendor 补丁 0002 暴露的查询面）。
    /// alacritty 不跟踪 ⇒ 由应答器侧记账后经 [`SessionVt::set_modify_other_keys`] 注入。
    pub modify_other_keys: bool,
}

impl Modes {
    /// 是否有任何鼠标上报模式激活（含 X10）。
    pub fn mouse_tracking(&self) -> bool {
        self.mouse_x10 || self.mouse_normal || self.mouse_button || self.mouse_any
    }
}

/// 颜色来源（cell 契约：调色板索引或 RGB，由客户端按主题解析）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Color {
    #[default]
    None,
    Palette(u8),
    Rgb(u8, u8, u8),
}

/// 属性位（u16 wire 掩码；下划线样式占 bit 8..11）。
pub mod attr {
    pub const BOLD: u16 = 1 << 0;
    pub const ITALIC: u16 = 1 << 1;
    pub const FAINT: u16 = 1 << 2;
    pub const BLINK: u16 = 1 << 3;
    pub const INVERSE: u16 = 1 << 4;
    pub const INVISIBLE: u16 = 1 << 5;
    pub const STRIKETHROUGH: u16 = 1 << 6;
    pub const OVERLINE: u16 = 1 << 7; // alacritty 无位 ⇒ 不可达（D-5）
    pub const UNDERLINE_SHIFT: u16 = 8;
    pub const UNDERLINE_MASK: u16 = 0xf << UNDERLINE_SHIFT;
}

/// 一个网格单元（wire cell 契约；symbol = 字素簇 UTF-8，空格/无文本为空串）。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Cell {
    pub symbol: String,
    /// 显示宽度：1 窄、2 宽、0 = 占位格（不渲染）。编码侧由「symbol 空」派生，此处保留
    /// 语义字段供适配层与测试直读。
    pub width: u8,
    /// 差分跳过位（宽字符尾/软折行占位）。
    pub skip: bool,
    pub fg: Color,
    pub bg: Color,
    pub attr: u16,
}

/// 一行（视口坐标 Y，顶起 0）。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Row {
    pub y: u16,
    pub dirty: bool,
    pub cells: Vec<Cell>,
}

/// 脏度。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dirty {
    None,
    Partial,
    Full,
}

/// 一个会话的服务端仿真器。
pub struct SessionVt {
    term: Term<VoidListener>,
    processor: alac_ansi::Processor,
    cols: usize,
    rows: usize,
    modify_other_keys: bool,
    /// 鼠标上报模式独立记账（wire 位语义：ghostty 各自独立置/复位；alacritty 把三者做成
    /// 互斥单值——对齐 golden 模式位列在此自管）。
    mouse_flags: u8,
    /// 强制全量脏（门一评审 B1）：拦截 ED2 等替代 `mark_fully_damaged`（alacritty 私有
    /// API）的自管位——置位后 `update()=Full`，`clean()` 清零。
    force_full: bool,
    /// 每视口行「上拍已下发内容」的 FNV 指纹（门一评审 B3）：damage_cursor 无条件标脏
    /// 光标行 ⇒ 脏集恒非空；本表过滤「内容未变」的行，使空闲/纯光标移动的差分行为与
    /// Go（空闲 count=0）一致。消费即更新（乐观，同 Go SurfaceClean；背压兜底归 6e 的
    /// needSnapshot）。
    flushed: Vec<u64>,
    /// resize 后行数变化 ⇒ 指纹表整体失效，强制全量重建。
    flushed_cols: usize,
    /// ghostty 全 DEC 模式自管表（DECRQM 应答面；应答器 6c）。
    dec_modes: responder::DecModes,
    /// 动态色（OSC 10/11/12 set 与 SetDefaultColors 的合并存储；查询应答面）。
    colors: responder::DynamicColors,
    /// OSC 4 set 镜像（索引 → 最近 set 值；查询回显，未 set 回 ghostty 基表）。
    palette_mirror: [Option<[u8; 3]>; 256],
    /// B5：编码面 last-set 单值（与 wire 独立位并存）。
    mouse_tracking: MouseTracking,
    mouse_format: MouseFormat,
    /// DECSTBM 滚动区（0-based 视口行；Term.scroll_region 私有 ⇒ 拦截
    /// set_scrolling_region 自管记账——CPR 的 DECOM 折算与 DECRQSS DECSTBM 应答用）。
    scroll_top: i32,
    scroll_bottom: i32,
    /// 旁路扫描器的跨块续接尾（vte 0.15 的语义层不转发 `CSI ? 998 n` 与 DCS
    /// hook/put/unhook ⇒ 这两族在解析器里被静默丢弃；扫描器在喂入前按字节流识别，
    /// 尾巴 = 上一批结尾处「可能是模式前缀」的未决字节）。
    scan_tail: Vec<u8>,
}

impl Default for SessionVt {
    fn default() -> Self {
        SessionVt::new(80, 24, DEFAULT_SCROLLBACK_LINES).expect("80x24 合法")
    }
}

/// 会话级事件接收方（EventListener 面）。term 服务的剪贴板/写回应答事件由 6c/6f 接线；
/// 底座阶段用空实现（VoidListener 同义，但保留类型以便后续替换）。
#[derive(Default)]
pub struct Sink;

impl EventListener for Sink {
    fn send_event(&self, _event: alacritty_terminal::event::Event) {}
}

impl SessionVt {
    /// 建 cols×rows 终端，回滚行数上限 scrollback（0 ⇒ 默认 10000）。
    pub fn new(cols: u16, rows: u16, scrollback: usize) -> Result<Self, String> {
        if cols == 0 || rows == 0 {
            return Err(format!("vt: 尺寸非法 {cols}x{rows}"));
        }
        let scrollback = if scrollback == 0 { DEFAULT_SCROLLBACK_LINES } else { scrollback };
        let config = Config {
            scrolling_history: scrollback,
            // kitty 协议的模式栈跟踪总开关：不开则 set/push/pop/query 全被忽略。
            kitty_keyboard: true,
            // OSC 52 双向（写=CLIPBOARD 帧转发、读=缓存应答；OnlyCopy 会拒掉读方向）。
            osc52: alacritty_terminal::term::Osc52::CopyPaste,
            ..Config::default()
        };
        let (cols, rows) = (cols as usize, rows as usize);
        let dims = SimpleDims { cols, rows };
        Ok(SessionVt {
            term: Term::new(config, &dims, VoidListener),
            processor: alac_ansi::Processor::new(),
            cols,
            rows,
            modify_other_keys: false,
            mouse_flags: 0,
            force_full: false,
            flushed: vec![0; rows],
            flushed_cols: cols,
            dec_modes: responder::DecModes::default(),
            colors: responder::DynamicColors::default(),
            palette_mirror: [None; 256],
            mouse_tracking: MouseTracking::None,
            mouse_format: MouseFormat::Default,
            scroll_top: 0,
            scroll_bottom: rows as i32 - 1,
            scan_tail: Vec::new(),
        })
    }

    /// 设置客户端上报的主题色（Go `SetDefaultColors` 面；THEME 帧后由会话层调）。
    /// 设置后 OSC 10/11 查询按上报值应答（未设置 ⇒ 不答，V-2 定案）。
    pub fn set_default_colors(&mut self, fg: [u8; 3], bg: [u8; 3]) {
        self.colors.fg = Some(fg);
        self.colors.bg = Some(bg);
    }

    /// 喂入并收集应答分片（`write_pty` 面）：每个应答一片、顺序与流中触发一致。
    /// 对齐 Go `SetResponseSink` 的流中同步回调语义（同一次 write 内顺序不乱）。
    ///
    /// **旁路扫描器**（vte 0.15 表外形态）：`CSI ? 998 n`（可见性查询，门一评审 F2）
    /// 与 `DCS $q … ST`（DECRQSS 三态，F3）在 vte 语义层没有分发臂 ⇒ 解析器静默丢弃。
    /// 扫描器按字节流在喂入前识别，命中段的应答按流内位置与解析器应答交错；
    /// 未决尾巴（跨块的模式前缀）存 `scan_tail` 续接到下一批。
    pub fn write_collecting(&mut self, p: &[u8], sink: &mut impl FnMut(&[u8])) {
        if p.is_empty() {
            return;
        }
        let mut responses: Vec<Vec<u8>> = Vec::new();
        let tail_len = self.scan_tail.len();
        let combined: Vec<u8> = if tail_len == 0 {
            p.to_vec()
        } else {
            let mut c = Vec::with_capacity(tail_len + p.len());
            c.extend_from_slice(&self.scan_tail);
            c.extend_from_slice(p);
            c
        };
        let mut fed = 0usize; // 本批 p 已喂入解析器的字节
        let mut i = 0usize; // combined 上的扫描游标（= 末次匹配结尾）
        while let Some((start, end, hit)) = scan_out_of_table(&combined, i) {
            // 模式终点前的字节先喂解析器（应答顺序 = 流内位置序）
            let feed_until = end.saturating_sub(tail_len).min(p.len());
            if feed_until > fed {
                self.feed_parser(&mut responses, &p[fed..feed_until]);
                fed = feed_until;
            }
            let resp = match hit {
                OutOfTableHit::Visibility => b"\x1b[?999;1n".to_vec(), // 服务端恒「潜在可见」
                OutOfTableHit::Decrqss { ps, pe } => {
                    let view = self.decrqss_view();
                    responder::decrqss(&combined[ps..pe], &view)
                }
            };
            responses.push(resp);
            i = end;
            let _ = start;
        }
        if fed < p.len() {
            self.feed_parser(&mut responses, &p[fed..]);
        }
        // 续接尾 = 未消费段结尾处最长的「模式前缀」（完整模式已被上面消费，不会重触）
        self.scan_tail = carry_prefix(&combined[i..]);
        for r in &responses {
            sink(r);
        }
    }

    fn feed_parser(&mut self, responses: &mut Vec<Vec<u8>>, bytes: &[u8]) {
        let mut probe = TermProbe {
            term: &mut self.term,
            modify_other_keys: &mut self.modify_other_keys,
            mouse_flags: &mut self.mouse_flags,
            force_full: &mut self.force_full,
            dec_modes: &mut self.dec_modes,
            colors: &mut self.colors,
            palette_mirror: &mut self.palette_mirror,
            mouse_tracking: &mut self.mouse_tracking,
            mouse_format: &mut self.mouse_format,
            scroll_top: &mut self.scroll_top,
            scroll_bottom: &mut self.scroll_bottom,
            responses,
        };
        self.processor.advance(&mut probe, bytes);
    }

    /// 把一批 PTY 输出喂进 vt（屏态唯一入口；应答丢弃——收集应答用
    /// [`SessionVt::write_collecting`]）。经 [`TermProbe`] 双面分发器：查询应答
    /// （DA/DSR/DECRQM/OSC 颜色）与 ghostty 兼容位在分发器上拦截，其余全量委托。
    pub fn write(&mut self, p: &[u8]) {
        self.write_collecting(p, &mut |_| {});
    }

    /// 改尺寸（含主屏回滚重排；备用屏不重排——alacritty 语义与 ghostty 一致）。
    /// 尺寸未变时空操作。行列变化 ⇒ 指纹表失效（下拍 `update()` 仍会 Full——resize 走
    /// 全量路径），此处重置。
    pub fn resize(&mut self, cols: u16, rows: u16) -> Result<(), String> {
        if cols == 0 || rows == 0 {
            return Err(format!("vt: 尺寸非法 {cols}x{rows}"));
        }
        let (cols, rows) = (cols as usize, rows as usize);
        if self.cols == cols && self.rows == rows {
            return Ok(());
        }
        self.term.resize(SimpleDims { cols, rows });
        self.cols = cols;
        self.rows = rows;
        self.flushed = vec![u64::MAX; rows];
        self.flushed_cols = cols;
        self.scroll_top = 0; // resize 清滚动区
        self.scroll_bottom = rows as i32 - 1;
        Ok(())
    }

    /// 当前网格尺寸。
    pub fn size(&self) -> (u16, u16) {
        (self.cols as u16, self.rows as u16)
    }

    /// 应答器侧的 modifyOtherKeys 记账回填（`CSI > 4;N m` 的解析在 6c 接线）。
    pub fn set_modify_other_keys(&mut self, on: bool) {
        self.modify_other_keys = on;
    }

    /// 键编码选项快照（`ghostty_key_encoder_setopt_from_terminal` 的等价面）：
    /// DECCKM/DECKPAM 取 TermMode（ESC 路径与 CSI 路径都会落到 alacritty 的模式位；
    /// 66 另见 dec_modes——CSI ?66h 不进 TermMode，与 ESC = 取或）；DECBKM/1035/1036
    /// 取自管 DECRQM 表（vte 表外模式）；mok2/kitty 取自管位/TermMode。
    pub fn key_options(&self) -> keyenc::KeyOptions {
        let mode = self.term.mode();
        keyenc::KeyOptions {
            cursor_key_application: mode.contains(TermMode::APP_CURSOR),
            keypad_key_application: self.dec_modes.get(66) || mode.contains(TermMode::APP_KEYPAD),
            backarrow_key_mode: self.dec_modes.get(67),
            ignore_keypad_with_numlock: self.dec_modes.get(1035),
            alt_esc_prefix: self.dec_modes.get(1036),
            modify_other_keys_state_2: self.modify_other_keys,
            kitty_flags: kitty_flags_of(*mode),
        }
    }

    /// 键编码（Go `Terminal.EncodeKey` 等价：选项按当前模式态现取现用）。
    pub fn encode_key(&self, ev: &keyenc::KeyEvent) -> Vec<u8> {
        keyenc::encode_key(ev, &self.key_options())
    }

    /// 鼠标编码（Go `Terminal.EncodeMouse` 等价：B5 单值 + 1×1 虚拟网格几何）。
    pub fn encode_mouse(&self, ev: &keyenc::MouseEvent) -> Vec<u8> {
        keyenc::encode_mouse(
            ev,
            &keyenc::MouseOptions {
                tracking: self.mouse_tracking,
                format: self.mouse_format,
                cols: self.cols as u16,
                rows: self.rows as u16,
            },
        )
    }

    /// DECRQSS 应答所需的当前态快照（SGR 笔态 / DECSCUSR / DECSTBM）。
    fn decrqss_view(&self) -> responder::DecrqssView {
        responder::DecrqssView {
            sgr: self.sgr_pen_string(),
            decscusr: self.decscusr_value(),
            scroll_region: (
                (self.scroll_top + 1).max(1) as u32,
                (self.scroll_bottom + 1).max(1) as u32,
            ),
        }
    }

    /// 当前 SGR 笔态串（ghostty `printAttributes`：恒 "0" 起头 + 属性段 + 前景/背景）。
    /// alacritty 笔态无 blink/overline 位 ⇒ SGR 5/53 不可达（与 D-5/B7 同族登记）。
    fn sgr_pen_string(&self) -> String {
        use std::fmt::Write as _;
        let cell = &self.term.grid().cursor.template;
        let mut s = String::from("0");
        if cell.flags.contains(AlacFlags::BOLD) {
            let _ = write!(s, ";1");
        }
        if cell.flags.contains(AlacFlags::DIM) {
            let _ = write!(s, ";2");
        }
        if cell.flags.contains(AlacFlags::ITALIC) {
            let _ = write!(s, ";3");
        }
        // 下划线：single 编 "4"，样式位编 "4:N"（ghostty 的 4 特例分支）
        if cell.flags.contains(AlacFlags::UNDERLINE) {
            s.push_str(";4");
        } else if cell.flags.contains(AlacFlags::DOUBLE_UNDERLINE) {
            s.push_str(";4:2");
        } else if cell.flags.contains(AlacFlags::UNDERCURL) {
            s.push_str(";4:3");
        } else if cell.flags.contains(AlacFlags::DOTTED_UNDERLINE) {
            s.push_str(";4:4");
        } else if cell.flags.contains(AlacFlags::DASHED_UNDERLINE) {
            s.push_str(";4:5");
        }
        // ;53（overline）/;5（blink）不可达：alacritty Flags 无对应位
        if cell.flags.contains(AlacFlags::INVERSE) {
            let _ = write!(s, ";7");
        }
        if cell.flags.contains(AlacFlags::HIDDEN) {
            let _ = write!(s, ";8");
        }
        if cell.flags.contains(AlacFlags::STRIKEOUT) {
            let _ = write!(s, ";9");
        }
        for (which, c) in [("38", &cell.fg), ("48", &cell.bg)] {
            match c {
                AlacColor::Named(NamedColor::Foreground | NamedColor::Background) => {}
                AlacColor::Named(named) => push_palette_sgr(&mut s, which, *named as u8),
                AlacColor::Indexed(n) => push_palette_sgr(&mut s, which, *n),
                AlacColor::Spec(rgb) => {
                    let _ = write!(s, ";{which}:2::{}:{}:{}", rgb.r, rgb.g, rgb.b);
                }
            }
        }
        s
    }

    /// DECSCUSR 数值（blink=1/3/5，steady=2/4/6；blink = 模式 12 或样式 blink 位）。
    fn decscusr_value(&self) -> u8 {
        let blink = self.dec_modes.get(12) || self.term.cursor_style().blinking;
        match self.term.cursor_style().shape {
            alacritty_terminal::vte::ansi::CursorShape::Underline if blink => 3,
            alacritty_terminal::vte::ansi::CursorShape::Underline => 4,
            alacritty_terminal::vte::ansi::CursorShape::Beam if blink => 5,
            alacritty_terminal::vte::ansi::CursorShape::Beam => 6,
            _ if blink => 1,
            _ => 2,
        }
    }

    /// 模式位快照。
    pub fn modes(&self) -> Modes {
        let mode = self.term.mode();
        let style = self.term.cursor_style();
        Modes {
            screen: if mode.contains(TermMode::ALT_SCREEN) {
                Screen::Alternate
            } else {
                Screen::Primary
            },
            cursor_keys_app: mode.contains(TermMode::APP_CURSOR),
            keypad_app: mode.contains(TermMode::APP_KEYPAD),
            bracketed_paste: mode.contains(TermMode::BRACKETED_PASTE),
            focus_events: mode.contains(TermMode::FOCUS_IN_OUT),
            mouse_x10: false,
            mouse_normal: self.mouse_flags & 1 != 0,
            mouse_button: self.mouse_flags & 2 != 0,
            mouse_any: self.mouse_flags & 4 != 0,
            mouse_sgr: mode.contains(TermMode::SGR_MOUSE),
            mouse_utf8: mode.contains(TermMode::UTF8_MOUSE),
            mouse_urxvt: false,
            alt_scroll: mode.contains(TermMode::ALTERNATE_SCROLL),
            cursor_visible: mode.contains(TermMode::SHOW_CURSOR),
            cursor_blink: style.blinking,
            insert: mode.contains(TermMode::INSERT),
            origin: mode.contains(TermMode::ORIGIN),
            wraparound: mode.contains(TermMode::LINE_WRAP),
            kitty_flags: kitty_flags_of(*mode),
            modify_other_keys: self.modify_other_keys,
        }
    }

    /// 消费脏状态并返回全局脏度（`Full` = 滚动/清屏/resize 等常态，视口全行脏）。
    /// force_full（B1 拦截位）等价 `mark_fully_damaged`（alacritty 私有 API 的自管替身）。
    pub fn update(&mut self) -> Dirty {
        if self.force_full {
            return Dirty::Full;
        }
        match self.term.damage() {
            TermDamage::Full => Dirty::Full,
            TermDamage::Partial(iter) => {
                if iter.into_iter().next().is_some() {
                    Dirty::Partial
                } else {
                    Dirty::None
                }
            }
        }
    }

    /// 取本拍需要重绘的视口行（`Full` 时 = 全视口行）。**必须先 [`SessionVt::update`]**。
    /// 只在「构建载荷」时调用；下发成功后 [`SessionVt::clean`]。
    /// Partial 路径按指纹过滤「内容未变」的行（B3：damage_cursor 会无条件标脏光标行，
    /// 空闲/纯光标移动的差分因此与 Go 的 count=0 对齐）。
    pub fn dirty_rows(&mut self) -> Vec<Row> {
        if self.force_full || matches!(self.term.damage(), TermDamage::Full) {
            return self.rows();
        }
        let damaged: std::collections::HashSet<usize> = match self.term.damage() {
            TermDamage::Partial(iter) => iter.into_iter().map(|l| l.line).collect(),
            _ => unreachable!("上分支已处理"),
        };
        let mut out = Vec::new();
        for y in (0..self.rows).filter(|y| damaged.contains(y)) {
            let row = self.row_at(y);
            let fp = row_fingerprint(&row);
            if self.flushed[y] != fp {
                self.flushed[y] = fp;
                out.push(row);
            }
        }
        out
    }

    /// 全部视口行（快照路径；隐含消费脏状态——调用方随后本就要发全量并 clean）。
    pub fn rows(&mut self) -> Vec<Row> {
        let _ = self.term.damage(); // 拉到最新（同 Go Rows 的隐含 Update）
        let mut out = Vec::with_capacity(self.rows);
        for y in 0..self.rows {
            let row = self.row_at(y);
            self.flushed[y] = row_fingerprint(&row);
            out.push(row);
        }
        out
    }

    /// 在载荷成功下发后消费脏标记。
    pub fn clean(&mut self) {
        self.force_full = false;
        self.term.reset_damage();
    }

    fn read_row(&self, grid: &alacritty_terminal::grid::Grid<AlacCell>, y: usize) -> Row {
        let line = Line(y as i32);
        let row = &grid[line];
        let mut cells = Vec::with_capacity(self.cols);
        for x in 0..self.cols {
            cells.push(cell_of(&row[Column(x)]));
        }
        Row { y: y as u16, dirty: false, cells }
    }

    fn row_at(&self, y: usize) -> Row {
        let grid = self.term.grid();
        self.read_row(grid, y)
    }

    /// 光标快照（视口坐标）。
    pub fn cursor(&mut self) -> Cursor {
        let grid = self.term.grid();
        let point: Point = grid.cursor.point; // 视口内（display_offset 恒 0）
        let style = self.term.cursor_style();
        let wide_tail = point.column.0 > 0
            && grid[point.line][Column(point.column.0 - 1)]
                .flags
                .contains(AlacFlags::WIDE_CHAR);
        let cur_cell = &grid[point.line][point.column];
        Cursor {
            x: point.column.0 as u16,
            y: point.line.0 as u16,
            visible: self.term.mode().contains(TermMode::SHOW_CURSOR),
            blinking: style.blinking,
            password: false,
            wide_tail: wide_tail && cur_cell.c == ' ',
            shape: match style.shape {
                alacritty_terminal::vte::ansi::CursorShape::Block => CursorShape::Block,
                alacritty_terminal::vte::ansi::CursorShape::Underline => CursorShape::Underline,
                alacritty_terminal::vte::ansi::CursorShape::Beam => CursorShape::Bar,
                _ => CursorShape::Block,
            },
        }
    }

    /// 回滚条状态（服务端不滚视口 ⇒ 恒贴底；offset/total 语义同 Go）。
    pub fn scrollbar(&self) -> Scrollbar {
        let total = self.term.total_lines() as u64;
        let len = self.term.screen_lines() as u64;
        Scrollbar { total, offset: total.saturating_sub(len), len }
    }

    /// 视口上方最多 `above` 行的回滚镜像窗口（最旧在前；备用屏/无回滚返回空）。
    pub fn mirror_rows(&self, above: usize) -> Vec<Row> {
        if above == 0 || self.modes_static().screen == Screen::Alternate {
            return Vec::new();
        }
        let sb = self.scrollbar();
        if sb.offset == 0 {
            return Vec::new();
        }
        let start = sb.offset.saturating_sub(above as u64);
        self.abs_rows(start as usize, (sb.offset - start) as usize)
    }

    /// 绝对行号区间 [from, from+count) 的行（FETCH-ROWS 应答；越界自动截断）。
    pub fn rows_at(&self, from: u64, count: usize) -> Vec<Row> {
        if count == 0 || self.modes_static().screen == Screen::Alternate {
            return Vec::new();
        }
        let total = self.scrollbar().total;
        if from >= total {
            return Vec::new();
        }
        let end = (from + count as u64).min(total);
        self.abs_rows(from as usize, (end - from) as usize)
    }

    fn modes_static(&self) -> Modes {
        // 供镜像/拉取路径的备用屏判定（不取 cursor_style，避免可变借用）。
        let mode = self.term.mode();
        Modes {
            screen: if mode.contains(TermMode::ALT_SCREEN) {
                Screen::Alternate
            } else {
                Screen::Primary
            },
            ..Modes::default()
        }
    }

    fn abs_rows(&self, from: usize, count: usize) -> Vec<Row> {
        let grid = self.term.grid();
        let total = self.term.total_lines();
        let cols = self.cols;
        let mut out = Vec::with_capacity(count);
        for i in 0..count {
            let abs = from + i;
            if abs >= total {
                break;
            }
            // 绝对行 a ⇔ Line(a - (total - rows))：Line(0)=视口顶、负数进回滚
            // （门一评审 B2：原 `a - total` 整体偏移 rows 行，debug panic / release 错行）。
            let top = total - self.rows;
            let line = Line(abs as i32 - top as i32);
            let row = &grid[line];
            let mut cells = Vec::with_capacity(cols);
            for x in 0..cols {
                cells.push(cell_of(&row[Column(x)]));
            }
            // 镜像/拉取行的 y 是**视口相对值**（服务端发的镜像 y 从视口顶起算——与 Go
            // 一致：客户端按返回顺序重排，不拿 y 当绝对行号）。
            out.push(Row { y: 0, dirty: false, cells });
        }
        out
    }

    /// 当前视口纯文本（检测引擎输入口径：跳占位格、空符号补空格、行尾裁空白）。
    pub fn screen_text(&mut self) -> String {
        let rows = self.rows();
        let lines: Vec<String> = rows
            .iter()
            .map(|r| {
                let mut s = String::with_capacity(r.cells.len());
                for c in &r.cells {
                    if c.skip {
                        continue;
                    }
                    if c.symbol.is_empty() {
                        s.push(' ');
                    } else {
                        s.push_str(&c.symbol);
                    }
                }
                s.trim_end_matches([' ', '\t', '\u{a0}']).to_string()
            })
            .collect();
        lines.join("\n")
    }
}

/// TermMode → kitty 协议五位（1/2/4/8/16；快照与查询应答共用一处拼装）。
fn kitty_flags_of(mode: TermMode) -> u8 {
    let mut kitty = 0u8;
    if mode.contains(TermMode::DISAMBIGUATE_ESC_CODES) {
        kitty |= 1;
    }
    if mode.contains(TermMode::REPORT_EVENT_TYPES) {
        kitty |= 2;
    }
    if mode.contains(TermMode::REPORT_ALTERNATE_KEYS) {
        kitty |= 4;
    }
    if mode.contains(TermMode::REPORT_ALL_KEYS_AS_ESC) {
        kitty |= 8;
    }
    if mode.contains(TermMode::REPORT_ASSOCIATED_TEXT) {
        kitty |= 16;
    }
    kitty
}

/// SGR 颜色段的调色板形态（ghostty printAttributes：0..7 → `;3N`/`;4N`、
/// 8..15 → `;9N`/`;10N`、≥16 → `{which}:5:N`）。
fn push_palette_sgr(s: &mut String, which: &str, idx: u8) {
    use std::fmt::Write as _;
    if idx < 8 {
        let c = if which == "38" { '3' } else { '4' };
        let _ = write!(s, ";{c}{idx}");
    } else if idx < 16 {
        let n = idx - 8;
        if which == "38" {
            let _ = write!(s, ";9{n}");
        } else {
            let _ = write!(s, ";10{n}");
        }
    } else {
        let _ = write!(s, ";{which}:5:{idx}");
    }
}

// ---------------------------------------------------------------------------
// 旁路扫描器（vte 表外形态：?998n 与 DCS $q；见 write_collecting 模块注）
// ---------------------------------------------------------------------------

/// 可见性查询的完整模式（`\x1b[?998n`）。
const VIS_QUERY: &[u8] = b"\x1b[?998n";
/// 续接尾的长度上限（`\x1bP` + 参数段 + `$q` + ≤2 载荷 + `\x1b\`；病态长参数放弃续接）。
const SCAN_CARRY_MAX: usize = 32;

/// 一次命中的形态（载荷区间指向 combined 里的 DECRQSS payload）。
enum OutOfTableHit {
    Visibility,
    Decrqss { ps: usize, pe: usize },
}

/// 在 `s[from..]` 找下一个完整命中（起始、结束、形态）。只认完整模式——
/// 结尾处的未决前缀由 [`carry_prefix`] 续接。
fn scan_out_of_table(s: &[u8], from: usize) -> Option<(usize, usize, OutOfTableHit)> {
    let mut i = from;
    while i < s.len() {
        if s[i] != 0x1b {
            i += 1;
            continue;
        }
        if s.len() - i >= VIS_QUERY.len() && &s[i..i + VIS_QUERY.len()] == VIS_QUERY {
            return Some((i, i + VIS_QUERY.len(), OutOfTableHit::Visibility));
        }
        // DCS $q：`\x1bP` + [0-9;:]* + `$q` + ≤2 非 ESC 载荷 + `\x1b\`
        // （ghostty dcs.zig：参数不设限；载荷第 3 字节起丢弃且**不**应答——扫描器
        // 以「载荷 ≤2」为完整模式的一部分，3+ 载荷自然不匹配）
        if s.len() - i >= 3 && s[i + 1] == b'P' {
            let mut j = i + 2;
            while j < s.len() && matches!(s[j], b'0'..=b'9' | b';' | b':') {
                j += 1;
            }
            if s.len() - j >= 2 && s[j] == b'$' && s[j + 1] == b'q' {
                let mut k = j + 2;
                while k < s.len() && s[k] != 0x1b && k < j + 4 {
                    k += 1; // 载荷至多 2 字节
                }
                if k + 1 < s.len() && s[k] == 0x1b && s[k + 1] == b'\\' {
                    return Some((i, k + 2, OutOfTableHit::Decrqss { ps: j + 2, pe: k }));
                }
            }
        }
        i += 1;
    }
    None
}

/// `s` 结尾处最长的「模式真前缀」（跨块续接依据）。
fn carry_prefix(s: &[u8]) -> Vec<u8> {
    for k in (1..=SCAN_CARRY_MAX.min(s.len())).rev() {
        let cand = &s[s.len() - k..];
        if (cand.len() < VIS_QUERY.len() && VIS_QUERY.starts_with(cand)) || is_dcs_prefix(cand) {
            return cand.to_vec();
        }
    }
    Vec::new()
}

/// DCS DECRQSS 模式的真前缀判定（参数段/载荷/半个 ST 都算未决）。
fn is_dcs_prefix(s: &[u8]) -> bool {
    if s.len() < 2 {
        return s == b"\x1b";
    }
    if s[0] != 0x1b || s[1] != b'P' {
        return false;
    }
    let mut j = 2;
    while j < s.len() && matches!(s[j], b'0'..=b'9' | b';' | b':') {
        j += 1;
    }
    if j == s.len() {
        return true; // 仍在参数段
    }
    if s[j] == b'$' {
        if j + 1 == s.len() {
            return true;
        }
        if s[j + 1] == b'q' {
            let payload = &s[j + 2..];
            let non_esc = payload.len() - usize::from(payload.last() == Some(&0x1b));
            return non_esc <= 2; // ≤2 载荷 + 可选半个 ST
        }
    }
    false
}

/// 行内容指纹（B3 的过滤依据）：对全格逐字段 FNV-1a——symbol/width/skip/颜色/属性任一
/// 变化即不同；同内容行（如 damage_cursor 误标的光标行）指纹稳定。
fn row_fingerprint(row: &Row) -> u64 {
    use std::hash::Hasher;
    let mut h = fnv::FnvHasher::default();
    for c in &row.cells {
        h.write(c.symbol.as_bytes());
        h.write_u8(c.width);
        h.write_u8(u8::from(c.skip));
        h.write_u16(match c.fg {
            Color::None => 0,
            Color::Palette(i) => 1 + i as u16,
            Color::Rgb(r, g, b) => 2 + (((r as u16) << 8) ^ ((g as u16) << 4) ^ (b as u16)),
        });
        h.write_u16(match c.bg {
            Color::None => 0,
            Color::Palette(i) => 1 + i as u16,
            Color::Rgb(r, g, b) => 2 + (((r as u16) << 8) ^ ((g as u16) << 4) ^ (b as u16)),
        });
        h.write_u16(c.attr);
    }
    h.finish()
}

/// alacritty cell → wire cell（归一化见模块头）。
fn cell_of(c: &AlacCell) -> Cell {
    let mut symbol = String::new();
    // width = **解码语义宽度**（Go goldenStyleText 的消费口径）：占位格/带样式空格 → 0
    // （客户端不画字形），其余 → 1（wire 不编宽度——宽字符的显示宽度由 skip 尾格表达）。
    let mut width = 1u8;
    let mut skip = false;
    if c.flags.contains(AlacFlags::WIDE_CHAR_SPACER)
        || c.flags.contains(AlacFlags::LEADING_WIDE_CHAR_SPACER)
    {
        width = 0;
        skip = true;
    }

    // 空白格归一化（V-1 实测定调，golden 逐格对拍裁决）：
    //   - 任何空格（含带样式）与占位格 ⇒ symbol 空（ghostty 的字素簇形态：未写格/
    //     带样式空格/占位格均无字素；「显式无样式空格」ghostty 保留 " "，但 alacritty
    //     无法区分显式空格与未写格——两者渲染/样式完全一致，编码侧统一走空白格，
    //     登记 D-13）；
    //   - 非空格字符 ⇒ symbol = 字符 + zerowidth 组合字符。
    // wire 的宽度不编（Go appendCell 同款），「解码语义宽度」由消费方从 symbol 推。
    let is_space = c.c == ' ' && c.zerowidth().is_none_or(|z| z.is_empty());
    if is_space {
        // 空格（显式或未写）一律无字素；**带样式的空格**宽度记 0（Go codecCell+decodeCell
        // 口径），无样式空格（= blankRun 族）宽度 1。
        let styled = !c.flags.is_empty()
            || c.fg != AlacColor::Named(NamedColor::Foreground)
            || c.bg != AlacColor::Named(NamedColor::Background);
        if styled && !skip {
            width = 0;
        }
    } else {
        symbol.push(c.c);
        if let Some(zw) = c.zerowidth() {
            for ch in zw {
                symbol.push(*ch);
            }
        }
    }

    let mut attr_bits = 0u16;
    if c.flags.contains(AlacFlags::BOLD) {
        attr_bits |= attr::BOLD;
    }
    if c.flags.contains(AlacFlags::ITALIC) {
        attr_bits |= attr::ITALIC;
    }
    if c.flags.contains(AlacFlags::DIM) {
        attr_bits |= attr::FAINT;
    }
    if c.flags.contains(AlacFlags::INVERSE) {
        attr_bits |= attr::INVERSE;
    }
    if c.flags.contains(AlacFlags::HIDDEN) {
        attr_bits |= attr::INVISIBLE;
    }
    if c.flags.contains(AlacFlags::STRIKEOUT) {
        attr_bits |= attr::STRIKETHROUGH;
    }
    // 下划线样式：1..5 = single/double/curly/dotted/dashed（ghostty SGR 下划线值同序）。
    let underline: u16 = if c.flags.contains(AlacFlags::UNDERLINE) {
        1
    } else if c.flags.contains(AlacFlags::DOUBLE_UNDERLINE) {
        2
    } else if c.flags.contains(AlacFlags::UNDERCURL) {
        3
    } else if c.flags.contains(AlacFlags::DOTTED_UNDERLINE) {
        4
    } else if c.flags.contains(AlacFlags::DASHED_UNDERLINE) {
        5
    } else {
        0
    };
    attr_bits |= underline << attr::UNDERLINE_SHIFT;

    Cell {
        symbol,
        width,
        skip,
        fg: color_of(&c.fg, false),
        bg: color_of(&c.bg, true),
        attr: attr_bits,
    }
}

/// alacritty 颜色 → wire 颜色。Named(Foreground/Background) 归 None（终端默认色）；
/// Named 调色板名/Indexed(n) → Palette；Spec → Rgb。
fn color_of(c: &AlacColor, _background: bool) -> Color {
    match c {
        AlacColor::Named(NamedColor::Foreground | NamedColor::Background) => Color::None,
        AlacColor::Named(named) => Color::Palette(*named as u8),
        AlacColor::Indexed(idx) => Color::Palette(*idx),
        AlacColor::Spec(rgb) => Color::Rgb(rgb.r, rgb.g, rgb.b),
    }
}

/// 最小 Dimensions（alacritty Term 构造/resize 用）。
struct SimpleDims {
    cols: usize,
    rows: usize,
}

impl alacritty_terminal::grid::Dimensions for SimpleDims {
    fn columns(&self) -> usize {
        self.cols
    }
    fn screen_lines(&self) -> usize {
        self.rows
    }
    fn total_lines(&self) -> usize {
        self.rows
    }
}


/// 双面分发器：vte Handler 的全量委托 + 拦截面。
///
/// 为什么必须有它：libghostty-vt 在流中同步应答查询/维护若干自管位，而 alacritty 的
/// `Term` 对同一批序列要么自答（DA/DSR/DECRQM——**答案与 ghostty 不同**，如 DA1 是
/// `?6c` vs ghostty `?62;22c`）、要么不落状态（modifyOtherKeys）。分发器在委托前拦下
/// 这些序列，按 ghostty 语义处置（查询应答通道 6c 接线；此处先落两个 ghostty 兼容位）。
struct TermProbe<'a, T> {
    term: &'a mut Term<T>,
    modify_other_keys: &'a mut bool,
    mouse_flags: &'a mut u8,
    force_full: &'a mut bool,
    dec_modes: &'a mut responder::DecModes,
    colors: &'a mut responder::DynamicColors,
    palette_mirror: &'a mut [Option<[u8; 3]>; 256],
    mouse_tracking: &'a mut MouseTracking,
    mouse_format: &'a mut MouseFormat,
    scroll_top: &'a mut i32,
    scroll_bottom: &'a mut i32,
    /// 本次 write 的应答分片（流中收集、批末由 [`SessionVt::write_collecting`] 投递）。
    responses: &'a mut Vec<Vec<u8>>,
}

impl<T: EventListener> alacritty_terminal::vte::ansi::Handler for TermProbe<'_, T> {
    #[inline]
    fn set_title(&mut self, _p0: Option<String>) { self.term.set_title(_p0) }
    #[inline]
    fn set_cursor_style(&mut self, _p0: Option<alac_ansi::CursorStyle>) { self.term.set_cursor_style(_p0) }
    #[inline]
    fn set_cursor_shape(&mut self, _shape: AlacCursorShape) { self.term.set_cursor_shape(_shape) }
    #[inline]
    fn input(&mut self, _c: char) { self.term.input(_c) }
    #[inline]
    fn goto(&mut self, _line: i32, _col: usize) { self.term.goto(_line, _col) }
    #[inline]
    fn goto_line(&mut self, _line: i32) { self.term.goto_line(_line) }
    #[inline]
    fn goto_col(&mut self, _col: usize) { self.term.goto_col(_col) }
    #[inline]
    fn insert_blank(&mut self, _p0: usize) { self.term.insert_blank(_p0) }
    #[inline]
    fn move_up(&mut self, _p0: usize) { self.term.move_up(_p0) }
    #[inline]
    fn move_down(&mut self, _p0: usize) { self.term.move_down(_p0) }
    /// 应答器（6c）：DA 族按 ghostty 应答（alacritty 自答 `\x1b[?6c` ≠ ghostty `?62;22c`，
    /// 拦截不委托；其应答走 PtyWrite 事件，VoidListener 丢弃——本层直接产字节进 responses）。
    fn identify_terminal(&mut self, _intermediate: Option<char>) {
        let r = responder::device_attributes(_intermediate);
        if !r.is_empty() {
            self.responses.push(r);
        }
    }
    /// 应答器（6c）：DSR 族。CPR 按 DECOM 折算（ghostty：origin 态报滚动区相对坐标）。
    fn device_status(&mut self, _p0: usize) {
        let point = self.term.grid().cursor.point;
        let origin = self.term.mode().contains(TermMode::ORIGIN);
        let top = *self.scroll_top;
        let r = responder::device_status(_p0, (point.line.0, point.column.0), origin, top);
        if !r.is_empty() {
            self.responses.push(r);
        }
    }
    #[inline]
    fn move_forward(&mut self, _col: usize) { self.term.move_forward(_col) }
    #[inline]
    fn move_backward(&mut self, _col: usize) { self.term.move_backward(_col) }
    #[inline]
    fn move_down_and_cr(&mut self, _row: usize) { self.term.move_down_and_cr(_row) }
    #[inline]
    fn move_up_and_cr(&mut self, _row: usize) { self.term.move_up_and_cr(_row) }
    #[inline]
    fn put_tab(&mut self, _count: u16) { self.term.put_tab(_count) }
    #[inline]
    fn backspace(&mut self) { self.term.backspace() }
    #[inline]
    fn carriage_return(&mut self) { self.term.carriage_return() }
    #[inline]
    fn linefeed(&mut self) { self.term.linefeed() }
    #[inline]
    fn bell(&mut self) { self.term.bell() }
    #[inline]
    fn substitute(&mut self) { self.term.substitute() }
    #[inline]
    fn newline(&mut self) { self.term.newline() }
    #[inline]
    fn set_horizontal_tabstop(&mut self) { self.term.set_horizontal_tabstop() }
    #[inline]
    fn scroll_up(&mut self, _p0: usize) { self.term.scroll_up(_p0) }
    #[inline]
    fn scroll_down(&mut self, _p0: usize) { self.term.scroll_down(_p0) }
    #[inline]
    fn insert_blank_lines(&mut self, _p0: usize) { self.term.insert_blank_lines(_p0) }
    #[inline]
    fn delete_lines(&mut self, _p0: usize) { self.term.delete_lines(_p0) }
    #[inline]
    fn erase_chars(&mut self, _p0: usize) { self.term.erase_chars(_p0) }
    #[inline]
    fn delete_chars(&mut self, _p0: usize) { self.term.delete_chars(_p0) }
    #[inline]
    fn move_backward_tabs(&mut self, _count: u16) { self.term.move_backward_tabs(_count) }
    #[inline]
    fn move_forward_tabs(&mut self, _count: u16) { self.term.move_forward_tabs(_count) }
    #[inline]
    fn save_cursor_position(&mut self) { self.term.save_cursor_position() }
    #[inline]
    fn restore_cursor_position(&mut self) { self.term.restore_cursor_position() }
    #[inline]
    fn clear_line(&mut self, _mode: alac_ansi::LineClearMode) { self.term.clear_line(_mode) }
    #[inline]
    fn clear_tabs(&mut self, _mode: alac_ansi::TabulationClearMode) { self.term.clear_tabs(_mode) }
    #[inline]
    fn set_tabs(&mut self, _interval: u16) { self.term.set_tabs(_interval) }
    /// RIS 全复位：自管位（mouse_flags/modifyOtherKeys，D-17/§二）随 ghostty 语义归零，
    /// DECRQM 模式表、调色板镜像与滚动区记账同样复位；theme（客户端上报）保留。
    /// 其余委托 alacritty。
    fn reset_state(&mut self) {
        *self.mouse_flags = 0;
        *self.modify_other_keys = false;
        *self.mouse_tracking = MouseTracking::None;
        *self.mouse_format = MouseFormat::Default;
        *self.dec_modes = responder::DecModes::default();
        *self.palette_mirror = [None; 256];
        *self.scroll_top = 0;
        *self.scroll_bottom = self.term.screen_lines() as i32 - 1;
        self.term.reset_state();
    }
    #[inline]
    fn reverse_index(&mut self) { self.term.reverse_index() }
    #[inline]
    fn terminal_attribute(&mut self, _attr: alac_ansi::Attr) { self.term.terminal_attribute(_attr) }
    #[inline]
    fn set_mode(&mut self, _mode: alac_ansi::Mode) { self.term.set_mode(_mode) }
    #[inline]
    fn unset_mode(&mut self, _mode: alac_ansi::Mode) { self.term.unset_mode(_mode) }
    #[inline]
    fn report_mode(&mut self, _mode: alac_ansi::Mode) { self.term.report_mode(_mode) }
    /// ghostty 兼容位（D-17 + B5）：wire 位独立记账（模式 1000/1002/1003 各自置/复位）；
    /// **编码面单值**（mouse_tracking/mouse_format 只记最后一次 set/reset——ghostty
    /// `flags.mouse_event/format` 语义，门一评审 B5）；DECRQM 面 = dec_modes 全表记账。
    /// X10（9）/URXVT（1015）/1016/47/1047 走 `PrivateMode::Unknown`（vte 表外，D-18）。
    fn set_private_mode(&mut self, mode: alac_ansi::PrivateMode) {
        use alac_ansi::{NamedPrivateMode, PrivateMode};
        if let PrivateMode::Named(m) = mode {
            match m {
                NamedPrivateMode::ReportMouseClicks => {
                    *self.mouse_flags |= 1;
                    *self.mouse_tracking = MouseTracking::Clicks;
                    self.dec_modes.set(1000, true);
                    return;
                }
                NamedPrivateMode::ReportCellMouseMotion => {
                    *self.mouse_flags |= 2;
                    *self.mouse_tracking = MouseTracking::CellMotion;
                    self.dec_modes.set(1002, true);
                    return;
                }
                NamedPrivateMode::ReportAllMouseMotion => {
                    *self.mouse_flags |= 4;
                    *self.mouse_tracking = MouseTracking::AllMotion;
                    self.dec_modes.set(1003, true);
                    return;
                }
                NamedPrivateMode::Utf8Mouse => {
                    *self.mouse_format = MouseFormat::Utf8;
                }
                NamedPrivateMode::SgrMouse => {
                    *self.mouse_format = MouseFormat::Sgr;
                }
                _ => {}
            }
            self.dec_modes.set(m as u16, true);
        } else if let PrivateMode::Unknown(n) = mode {
            match n {
                9 => *self.mouse_tracking = MouseTracking::X10,
                1015 => *self.mouse_format = MouseFormat::Urxvt,
                47 | 1047 => {
                    // B6 登记残余：alacritty 无公开 swap-alt API，备用屏内容面暂不可达
                    // （wire Alt 位与 DECRQM 报告面已记账；真 TUI 多用 1049 不受影响）。
                }
                _ => {}
            }
            self.dec_modes.set(n, true);
        }
        self.term.set_private_mode(mode);
    }

    /// ghostty 兼容位（D-15，门一评审 B4 补全）：未配对 `CSI ? 1049 l`（不在备用屏时收到
    /// 1049 退出）ghostty 按 DECRC 的「从未保存 ⇒ 恢复初始态」处理——复位 SGR 样式、清
    /// DECOM、光标归 (0,0)（golden session-styles 夹具的光标形态钉住）。alacritty 对此
    /// no-op；真实 TUI 的 1049h/l 成对，两者无差——本分支只钉未配对形态。
    /// charset G0/G1 与 protected аттр 的复位无公开 API，残余差异登记（门一评审 B4）。
    fn unset_private_mode(&mut self, mode: alac_ansi::PrivateMode) {
        use alac_ansi::{Attr, NamedPrivateMode, PrivateMode};
        if let PrivateMode::Named(m) = mode {
            match m {
                NamedPrivateMode::ReportMouseClicks => {
                    *self.mouse_flags &= !1;
                    if *self.mouse_tracking == MouseTracking::Clicks {
                        *self.mouse_tracking = MouseTracking::None; // B5 单值 reset
                    }
                    self.dec_modes.set(1000, false);
                    return;
                }
                NamedPrivateMode::ReportCellMouseMotion => {
                    *self.mouse_flags &= !2;
                    if *self.mouse_tracking == MouseTracking::CellMotion {
                        *self.mouse_tracking = MouseTracking::None;
                    }
                    self.dec_modes.set(1002, false);
                    return;
                }
                NamedPrivateMode::ReportAllMouseMotion => {
                    *self.mouse_flags &= !4;
                    if *self.mouse_tracking == MouseTracking::AllMotion {
                        *self.mouse_tracking = MouseTracking::None;
                    }
                    self.dec_modes.set(1003, false);
                    return;
                }
                NamedPrivateMode::Utf8Mouse => {
                    if *self.mouse_format == MouseFormat::Utf8 {
                        *self.mouse_format = MouseFormat::Default;
                    }
                }
                NamedPrivateMode::SgrMouse => {
                    if *self.mouse_format == MouseFormat::Sgr {
                        *self.mouse_format = MouseFormat::Default;
                    }
                }
                NamedPrivateMode::SwapScreenAndSetRestoreCursor
                    if !self.term.mode().contains(TermMode::ALT_SCREEN) =>
                {
                    self.term.terminal_attribute(Attr::Reset);
                    self.term.unset_private_mode(PrivateMode::Named(NamedPrivateMode::Origin));
                    self.term.goto(0, 0);
                    // 落到末尾继续 unset 1049（no-op，但保持序列语义完整）
                }
                _ => {}
            }
            self.dec_modes.set(m as u16, false);
        } else if let PrivateMode::Unknown(n) = mode {
            match n {
                9 => {
                    if *self.mouse_tracking == MouseTracking::X10 {
                        *self.mouse_tracking = MouseTracking::None;
                    }
                }
                1015 if *self.mouse_format == MouseFormat::Urxvt => {
                    *self.mouse_format = MouseFormat::Default;
                }
                _ => {}
            }
            self.dec_modes.set(n, false);
        }
        self.term.unset_private_mode(mode);
    }
    /// 应答器（6c）：DECRQM 按自管全模式表（alacritty 只认它有位的模式且自答格式不同）。
    fn report_private_mode(&mut self, _mode: alac_ansi::PrivateMode) {
        use alac_ansi::PrivateMode;
        let n = match _mode {
            PrivateMode::Named(m) => m as u16,
            PrivateMode::Unknown(n) => n,
        };
        let state = self.dec_modes.decrqm_state(n);
        self.responses.push(responder::decrqm(n, state));
    }
    fn set_scrolling_region(&mut self, _top: usize, _bottom: Option<usize>) {
        // 拦截记账（0-based 顶/底）：vte 传 1-based（`CSI 5;20r` → top=5，alacritty 内部
        // 减 1）；bottom None/0 = 全屏。CPR 的 DECOM 折算与 DECRQSS DECSTBM 应答用同基准。
        *self.scroll_top = _top as i32 - 1;
        *self.scroll_bottom = match _bottom {
            Some(b) => b as i32 - 1,
            None => self.term.screen_lines() as i32 - 1,
        };
        self.term.set_scrolling_region(_top, _bottom)
    }
    #[inline]
    fn set_keypad_application_mode(&mut self) { self.term.set_keypad_application_mode() }
    #[inline]
    fn unset_keypad_application_mode(&mut self) { self.term.unset_keypad_application_mode() }
    #[inline]
    fn set_active_charset(&mut self, _p0: alac_ansi::CharsetIndex) { self.term.set_active_charset(_p0) }
    #[inline]
    fn configure_charset(&mut self, _p0: alac_ansi::CharsetIndex, _p1: alac_ansi::StandardCharset) { self.term.configure_charset(_p0, _p1) }
    #[inline]
    /// 应答器（6c）：OSC 颜色查询（10/11/12 未设主题不答、12 回落 fg；OSC 4 = 镜像/基表；
    /// 终止符跟随查询）。委托版只会把应答发进 VoidListener ⇒ 拦截自行产字节。
    fn dynamic_color_sequence(&mut self, _p0: String, _p1: usize, _p2: &str) {
        let answer: Option<[u8; 3]> = if let Some(idx) = _p0.strip_prefix("4;") {
            // OSC 4：镜像值优先，未 set 回 ghostty 内置基表
            let idx: usize = idx.parse().unwrap_or(256);
            (idx < 256).then(|| {
                self.palette_mirror[idx]
                    .unwrap_or(responder::ghostty_palette()[idx])
            })
        } else {
            _p0.parse::<u16>().ok().and_then(|c| self.colors.answer(c))
        };
        if let Some(rgb) = answer {
            self.responses
                .push(format!("\x1b]{};{}{}", _p0, responder::color_report(rgb), _p2).into_bytes());
        }
    }
    /// OSC 颜色 set：委托（alacritty 自身色面）+ 记账（调色板镜像 / 动态色分量）。
    fn set_color(&mut self, _p0: usize, _p1: alac_ansi::Rgb) {
        let rgb = [_p1.r, _p1.g, _p1.b];
        if let Some(i) = _p0.checked_sub(256) {
            // NamedColor::Foreground=256 / Background=257 / Cursor=258
            self.colors.set(10 + i as u16, rgb);
        } else if _p0 < 256 {
            self.palette_mirror[_p0] = Some(rgb);
        }
        self.term.set_color(_p0, _p1);
    }
    #[inline]
    /// OSC 104/110-119 颜色复位：委托 + 清镜像/分量（theme 由 SetDefaultColors 设定的
    /// 分量随 RIS 保留——客户端上报主题是会话属性，非终端态；OSC 104 复位只清程序 set 面）。
    fn reset_color(&mut self, _p0: usize) {
        if let Some(i) = _p0.checked_sub(256) {
            match 10 + i as u16 {
                10 => self.colors.fg = None,
                11 => self.colors.bg = None,
                12 => self.colors.cursor = None,
                _ => {}
            }
        } else if _p0 < 256 {
            self.palette_mirror[_p0] = None;
        }
        self.term.reset_color(_p0);
    }
    #[inline]
    fn clipboard_store(&mut self, _p0: u8, _p1: &[u8]) { self.term.clipboard_store(_p0, _p1) }
    #[inline]
    fn clipboard_load(&mut self, _p0: u8, _p1: &str) { self.term.clipboard_load(_p0, _p1) }
    #[inline]
    fn decaln(&mut self) { self.term.decaln() }
    #[inline]
    fn push_title(&mut self) { self.term.push_title() }
    #[inline]
    fn pop_title(&mut self) { self.term.pop_title() }
    #[inline]
    fn text_area_size_pixels(&mut self) { self.term.text_area_size_pixels() }
    #[inline]
    fn text_area_size_chars(&mut self) { self.term.text_area_size_chars() }
    #[inline]
    fn set_hyperlink(&mut self, _p0: Option<alac_ansi::Hyperlink>) { self.term.set_hyperlink(_p0) }
    // CursorIcon 在 vte 0.15 是私有类型：本方法不委托（Term 的默认实现即 no-op，
    // 鼠标光标形状由客户端自理——服务端无 UI 面）。
    /// 应答器（6c）：kitty 键盘查询 `\x1b[?{flags}u`（flags 从 TermMode 拼装；0 也显式）。
    fn report_keyboard_mode(&mut self) {
        self.responses.push(responder::kitty_query(kitty_flags_of(*self.term.mode())));
    }
    #[inline]
    fn push_keyboard_mode(&mut self, _mode: alac_ansi::KeyboardModes) { self.term.push_keyboard_mode(_mode) }
    #[inline]
    fn pop_keyboard_modes(&mut self, _to_pop: u16) { self.term.pop_keyboard_modes(_to_pop) }
    #[inline]
    fn set_keyboard_mode(&mut self, _mode: alac_ansi::KeyboardModes, _behavior: alac_ansi::KeyboardModesApplyBehavior) { self.term.set_keyboard_mode(_mode, _behavior) }
    #[inline]
    /// 应答器（6c）：XTQMODKEYS 不答（实测向量 mok_query_*；拦截丢弃 alacritty 的自答事件）。
    fn report_modify_other_keys(&mut self) {}
    #[inline]
    fn set_scp(&mut self, _char_path: alac_ansi::ScpCharPath, _update_mode: alac_ansi::ScpUpdateMode) { self.term.set_scp(_char_path, _update_mode) }

    /// ghostty 兼容位（D-16）：`CSI 2J`（ED2）在主屏上 alacritty 走 `clear_viewport`
    /// ——把当前视口推入回滚（xterm 形态）；ghostty 只清视口、不增回滚（golden
    /// session-styles 的回滚 total=32 即此语义）。Go 出口的回滚条/镜像语义跟 ghostty，
    /// 此处对齐：主屏 ED2 = 重置视口区（不进历史）。**置 force_full**（门一评审 B1）：
    /// `reset_region` 不碰 damage，若不置位则 update()=Partial 且脏集只有光标行 ⇒
    /// 服务端屏已清空而差分只送一行。
    fn clear_screen(&mut self, mode: alac_ansi::ClearMode) {
        use alac_ansi::ClearMode;
        if matches!(mode, ClearMode::All) && !self.term.mode().contains(TermMode::ALT_SCREEN) {
            self.term.grid_mut().reset_region(..);
            *self.force_full = true;
            return;
        }
        self.term.clear_screen(mode);
    }

    /// modifyOtherKeys 记账（alacritty 的 Term 不落状态）：mode 2（EnableAll）置位、
    /// 其余（Reset/EnableExceptWellDefined）清零——ghostty 只认「other_keys_numeric」。
    fn set_modify_other_keys(&mut self, mode: alac_ansi::ModifyOtherKeys) {
        *self.modify_other_keys = matches!(mode, alac_ansi::ModifyOtherKeys::EnableAll);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(p: &str) -> Vec<u8> {
        let base = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures");
        std::fs::read(format!("{base}/{p}")).unwrap_or_else(|e| panic!("夹具 {p}: {e}"))
    }

    /// Go goldenDigest 同款：FNV-1a 64 → 16 位小写 hex。
    fn fnv_hex(data: &[u8]) -> String {
        let mut h = fnv::FnvHasher::default();
        use std::hash::Hasher;
        h.write(data);
        format!("{:016x}", h.finish())
    }

    /// Go gridTextLines 口径：占位格跳过、空符号补空格、行尾 TrimRight(" ")、\n 连接。
    fn text_of(rows: &[Row]) -> String {
        rows.iter()
            .map(|r| {
                let mut s = String::new();
                for c in &r.cells {
                    if c.skip {
                        continue;
                    }
                    if c.symbol.is_empty() {
                        s.push(' ');
                    } else {
                        s.push_str(&c.symbol);
                    }
                }
                s.trim_end_matches(' ').to_string()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Go goldenStyleText 口径：逐行逐格 26 个 hex（fg 5B · bg 5B · attr 2B · flags 1B）。
    /// 缺格按空白格（Width=1）；flags = skip(bit0) | width<<1。
    fn style_of(rows: &[Row], cols: usize) -> String {
        fn color_hex(c: Color) -> [u8; 5] {
            match c {
                Color::None => [0, 0, 0, 0, 0],
                Color::Palette(i) => [1, i, 0, 0, 0],
                Color::Rgb(r, g, b) => [2, 0, r, g, b],
            }
        }
        let mut out = String::new();
        for r in rows {
            for x in 0..cols {
                let blank = Cell { width: 1, ..Cell::default() };
                let c = r.cells.get(x).unwrap_or(&blank);
                let mut flags = 0u8;
                if c.skip {
                    flags |= 1;
                }
                flags |= c.width << 1;
                let fg = color_hex(c.fg);
                let bg = color_hex(c.bg);
                for b in fg.iter().chain(bg.iter()) {
                    out.push_str(&format!("{b:02x}"));
                }
                out.push_str(&format!("{:04x}{:02x}", c.attr, flags));
            }
            out.push('\n');
        }
        out
    }

    /// 读 surface-golden/manifest.tsv：name → 列向量。
    fn golden_manifest() -> Vec<Vec<String>> {
        let base = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures");
        let raw = std::fs::read_to_string(format!("{base}/surface-golden/manifest.tsv"))
            .expect("golden manifest");
        raw.lines()
            .filter(|l| !l.is_empty())
            .map(|l| l.split('\t').map(|s| s.to_string()).collect())
            .collect()
    }

    /// 6b 自检①：三个会话夹具喂底座——damage 有值、screen_text 非空且含语义锚点。
    #[test]
    fn vt_fixture_damage_and_text() {
        for (name, anchor) in [
            ("session-cjk.bin", "文件"),
            ("session-git-log.bin", "* a991c02"),
            ("session-hexdump.bin", "|"),
        ] {
            let data = fixture(&format!("term-vt/{name}"));
            let mut vt = SessionVt::new(100, 32, 5000).unwrap();
            vt.write(&data);
            let d = vt.update();
            assert!(d != Dirty::None, "{name}: damage 为空");
            let text = vt.screen_text();
            assert!(!text.is_empty(), "{name}: 屏幕文本为空");
            assert!(text.contains(anchor), "{name}: 文本缺锚点 {anchor}（截取：{}）", text[..text.len().min(200)].escape_default());
        }
    }

    /// 6b 自检②（V-1 裁决）：golden 文本/样式向量 digest 对拍 manifest.tsv。
    /// 仿真等价（alacritty vs ghostty）与 cell 归一化（空白格/带样式空格）在此定调。
    #[test]
    fn vt_golden_digest_parity() {
        let manifest = golden_manifest();
        assert!(manifest.len() >= 8, "golden manifest 行数 {}", manifest.len());
        let cases = [
            "session-cjk",
            "session-git-log",
            "session-hexdump",
            // 样式专项：内联序列（Go goldenStylesSession 同款）
            "session-styles",
        ];
        for key in cases {
            let row = manifest
                .iter()
                .find(|r| r[0] == key)
                .unwrap_or_else(|| panic!("manifest 缺 {key}"));
            let data = if key == "session-styles" {
                "\u{1b}[2J\u{1b}[H\u{1b}[1;3;4;7mBOLD\u{1b}[0m\u{1b}[2;9mDIM-STRIKE\u{1b}[0m\u{1b}[38;5;196mPAL256\u{1b}[0m \u{1b}[48;2;10;20;30mRGBBG\u{1b}[0m \u{1b}[31;44mRED-BLUE\u{1b}[0m \u{5bbd}\u{5b57}\r\n\u{1b}[?1000h\u{1b}[?1002h\u{1b}[?1006h\u{1b}[?2004h\u{1b}[?1049lSTYLES-OK".as_bytes().to_vec()
            } else {
                fixture(&format!("term-vt/{key}.bin"))
            };
            let mut vt = SessionVt::new(100, 32, 5000).unwrap();
            vt.write(&data);
            let rows = vt.rows();
            let text = fnv_hex(text_of(&rows).as_bytes());
            let style = fnv_hex(style_of(&rows, 100).as_bytes());
            assert_eq!(text, row[5], "{key}: 文本 digest 不一致（仿真等价/归一化漂移，见 V-1）");
            assert_eq!(style, row[15], "{key}: 样式向量 digest 不一致（V-1）");
        }
    }

    /// RIS 全复位清自管位（D-17 mouse_flags + modifyOtherKeys）——golden 模式位列之外
    /// 的行为钉子：`CSI ? 1000h` + `CSI > 4;2m` + RIS 后三位快照全归零。
    #[test]
    fn vt_ris_resets_self_managed_flags() {
        let mut vt = SessionVt::new(80, 24, 100).unwrap();
        vt.write(b"\x1b[?1000h\x1b[?1002h\x1b[?1006h\x1b[>4;2m");
        let m = vt.modes();
        assert!(m.mouse_normal && m.mouse_button && m.mouse_sgr);
        vt.set_modify_other_keys(true); // 记账路径（6c 接线前的直接面）
        vt.write(b"\x1bc"); // RIS
        let m = vt.modes();
        assert!(!m.mouse_normal && !m.mouse_button && !m.mouse_sgr, "RIS 须清 mouse_flags");
        assert!(!m.modify_other_keys, "RIS 须清 modifyOtherKeys");
        assert!(!m.bracketed_paste && m.cursor_visible, "RIS 须复位常规模式");
    }

    /// 门一评审 B2：绝对行号拉取的正确性（原实现整体偏移 rows 行）。
    /// 10×3 视口写 6 行：回滚 3 行（绝对 0-2）+ 视口 3 行（绝对 3-5）。
    #[test]
    fn vt_rows_at_absolute_lines() {
        fn text_of(row: &Row) -> String {
            row.cells.iter().map(|c| c.symbol.as_str()).collect::<String>()
        }
        let mut vt = SessionVt::new(10, 3, 100).unwrap();
        vt.write(b"line0\r\nline1\r\nline2\r\nline3\r\nline4\r\nline5");
        let sb = vt.scrollbar();
        assert_eq!((sb.total, sb.len, sb.offset), (6, 3, 3), "6 行进 3 视口 ⇒ 回滚 3");
        // rows_at(0,3) = 最旧三行（回滚区）
        let rows = vt.rows_at(0, 3);
        assert_eq!(rows.len(), 3);
        assert!(text_of(&rows[0]).starts_with("line0"), "绝对行 0 = 最旧回滚行");
        assert!(text_of(&rows[2]).starts_with("line2"));
        // rows_at(3,3) = 视口三行
        let rows = vt.rows_at(3, 3);
        assert!(text_of(&rows[0]).starts_with("line3"), "绝对行 3 = 视口顶");
        assert!(text_of(&rows[2]).starts_with("line5"), "绝对行 5 = 视口底");
        // 跨界 + 越界截断
        let rows = vt.rows_at(4, 99);
        assert_eq!(rows.len(), 2, "越界截断到 total");
        let rows = vt.rows_at(99, 2);
        assert!(rows.is_empty(), "from ≥ total 返回空");
        // 镜像窗口：offset 之上最多 above 行
        let mirror = vt.mirror_rows(2);
        assert_eq!(mirror.len(), 2);
        assert!(text_of(&mirror[0]).starts_with("line1"), "镜像最旧在前");
    }

    /// 门一评审 B1：主屏 ED2 后本拍必须全视口脏（reset_region 不碰 damage 的补偿）。
    #[test]
    fn vt_ed2_forces_full_damage() {
        let mut vt = SessionVt::new(20, 5, 100).unwrap();
        vt.write(b"full\r\nfull\r\nfull\r\nfull\r\nfull");
        let _ = vt.rows(); // 建立指纹基线并消费
        vt.clean();
        vt.write(b"\x1b[H\x1b[2J"); // 主屏 ED2
        assert_eq!(vt.update(), Dirty::Full, "ED2 后必须 Full（B1）");
        let rows = vt.dirty_rows();
        assert_eq!(rows.len(), 5, "Full = 全视口行");
        assert!(vt.screen_text().trim().is_empty(), "屏已清空");
        vt.clean();
        // clean 后 alacritty 侧仍恒 Partial（B3 机制），但差分集必须空
        let _ = vt.update();
        assert!(vt.dirty_rows().is_empty(), "clean 后差分空（update 的 Partial 由内容过滤兜住）");
    }

    /// 门一评审 B3：空闲拍差分为空（damage_cursor 误标光标行被指纹过滤）。
    #[test]
    fn vt_idle_dirty_rows_empty() {
        let mut vt = SessionVt::new(20, 5, 100).unwrap();
        vt.write(b"hello world");
        let _ = vt.update();
        let first = vt.dirty_rows();
        assert!(!first.is_empty(), "首拍有真脏行");
        vt.clean();
        // 空闲：无新字节 ⇒ damage_cursor 仍标脏光标行，但内容未变 ⇒ 差分空
        assert_eq!(vt.update(), Dirty::Partial, "alacritty 侧恒 Partial（damage_cursor）");
        assert!(vt.dirty_rows().is_empty(), "内容未变的行必须被过滤（B3）");
        // 纯光标移动：行内容不变 ⇒ 差分仍空（光标走 DIFF 的 cursor 字段，6e 编码）
        vt.write(b"\x1b[2;3H");
        let _ = vt.update();
        assert!(vt.dirty_rows().is_empty(), "纯光标移动无行差分（B3）");
        // 真实输出恢复差分
        vt.write(b"X");
        let _ = vt.update();
        assert_eq!(vt.dirty_rows().len(), 1, "新输出 ⇒ 对应行脏");
    }

    /// 门一评审 B4：未配对 1049l 的完整复位（样式/origin/光标）。
    #[test]
    fn vt_unpaired_1049l_resets_state() {
        let mut vt = SessionVt::new(20, 5, 100).unwrap();
        // 反色 + origin + 定位到滚动区内 ⇒ 未配对 1049l 后全复位
        vt.write(b"\x1b[7m\x1b[2;4r\x1b[?6h\x1b[2;3H");
        let cur = vt.cursor();
        assert_eq!((cur.x, cur.y), (2, 2), "origin 态光标（滚动区顶 1 + 行 2 - 1）");
        vt.write(b"\x1b[?1049l");
        let cur = vt.cursor();
        assert_eq!((cur.x, cur.y), (0, 0), "未配对 1049l ⇒ 光标归 (0,0)");
        assert!(!vt.modes().origin, "DECOM 一并复位（B4）");
        let rows = vt.rows();
        assert_eq!(rows[0].cells[0].attr & attr::INVERSE, 0, "SGR 反相须复位（B4）");
    }

    /// 门一评审 F2/F3 补充：旁路扫描器的跨块续接与流内应答顺序。
    #[test]
    fn vt_bypass_scanner_continuation_and_order() {
        // ① ?998n 跨三块拆分：应答 `\x1b[?999;1n` 且只应一次
        let mut vt = SessionVt::new(80, 24, 100).unwrap();
        let mut got: Vec<u8> = Vec::new();
        for chunk in [&b"\x1b["[..], &b"?99"[..], &b"8n"[..]] {
            vt.write_collecting(chunk, &mut |p| got.extend_from_slice(p));
        }
        assert_eq!(got, b"\x1b[?999;1n");
        // ② DA1 在前 + 998n 在后：应答顺序 = 流内顺序（解析器应答先出）
        let mut vt = SessionVt::new(80, 24, 100).unwrap();
        let mut got: Vec<u8> = Vec::new();
        vt.write_collecting(b"\x1b[c\x1b[?998n", &mut |p| got.extend_from_slice(p));
        assert_eq!(got, b"\x1b[?62;22c\x1b[?999;1n");
        // ③ DECRQSS 跨块（`\x1bP` 与 `$qm` 与 ST 各自一块）+ 应答反映查询时刻笔态
        let mut vt = SessionVt::new(80, 24, 100).unwrap();
        vt.write(b"\x1b[1;31;4m"); // BOLD + 红 fg + 单下划线
        let mut got: Vec<u8> = Vec::new();
        for chunk in [&b"\x1bP"[..], &b"$q"[..], &b"m"[..], &b"\x1b\\"[..]] {
            vt.write_collecting(chunk, &mut |p| got.extend_from_slice(p));
        }
        // "0;1;4;3;31m"（0 起头 + bold + 单下划线 4 + fg palette 1 → ";31"）
        assert_eq!(got, b"\x1bP1$r0;1;4;31m\x1b\\");
        // ④ 3+ 字节载荷不构成完整模式（ghostty 第 3 个 put 即丢弃、不应答）
        let mut vt = SessionVt::new(80, 24, 100).unwrap();
        let mut got: Vec<u8> = Vec::new();
        vt.write_collecting(b"\x1bP$qzzz\x1b\\", &mut |p| got.extend_from_slice(p));
        assert!(got.is_empty());
        // ⑤ 未决尾巴在后续不成立时自然消解（不误触）
        let mut vt = SessionVt::new(80, 24, 100).unwrap();
        let mut got: Vec<u8> = Vec::new();
        vt.write_collecting(b"\x1b[?9", &mut |p| got.extend_from_slice(p));
        vt.write_collecting(b"xyz", &mut |p| got.extend_from_slice(p));
        assert!(got.is_empty());
    }

    /// DECRQSS 的 DECSCUSR/DECSTBM 态（样式与滚动区折算）。
    #[test]
    fn vt_decrqss_decscusr_and_decstbm() {
        // 默认块形不闪 ⇒ "2 q"
        let mut vt = SessionVt::new(80, 24, 100).unwrap();
        let mut got: Vec<u8> = Vec::new();
        vt.write_collecting(b"\x1bP$q q\x1b\\", &mut |p| got.extend_from_slice(p));
        assert_eq!(got, b"\x1bP1$r2 q\x1b\\");
        // DECSCUSR 3（闪下划线）⇒ "3 q"
        let mut vt = SessionVt::new(80, 24, 100).unwrap();
        vt.write(b"\x1b[3 q");
        let mut got: Vec<u8> = Vec::new();
        vt.write_collecting(b"\x1bP$q q\x1b\\", &mut |p| got.extend_from_slice(p));
        assert_eq!(got, b"\x1bP1$r3 q\x1b\\");
        // DECSTBM 5..20 ⇒ "5;20r"
        let mut vt = SessionVt::new(80, 24, 100).unwrap();
        vt.write(b"\x1b[5;20r");
        let mut got: Vec<u8> = Vec::new();
        vt.write_collecting(b"\x1bP$qr\x1b\\", &mut |p| got.extend_from_slice(p));
        assert_eq!(got, b"\x1bP1$r5;20r\x1b\\");
        // DECSLRM 恒 invalid（alacritty 无边距面）
        let mut vt = SessionVt::new(80, 24, 100).unwrap();
        let mut got: Vec<u8> = Vec::new();
        vt.write_collecting(b"\x1bP$qs\x1b\\", &mut |p| got.extend_from_slice(p));
        assert_eq!(got, b"\x1bP0$r\x1b\\");
    }

    /// 光标/回滚条/模式位列对拍（快照语义的另一半）。
    #[test]
    fn vt_golden_cursor_scroll_modes_parity() {
        let manifest = golden_manifest();
        for key in ["session-cjk", "session-git-log", "session-hexdump", "session-styles"] {
            let row = manifest.iter().find(|r| r[0] == key).unwrap();
            let data = if key == "session-styles" {
                "\u{1b}[2J\u{1b}[H\u{1b}[1;3;4;7mBOLD\u{1b}[0m\u{1b}[2;9mDIM-STRIKE\u{1b}[0m\u{1b}[38;5;196mPAL256\u{1b}[0m \u{1b}[48;2;10;20;30mRGBBG\u{1b}[0m \u{1b}[31;44mRED-BLUE\u{1b}[0m \u{5bbd}\u{5b57}\r\n\u{1b}[?1000h\u{1b}[?1002h\u{1b}[?1006h\u{1b}[?2004h\u{1b}[?1049lSTYLES-OK".as_bytes().to_vec()
            } else {
                fixture(&format!("term-vt/{key}.bin"))
            };
            let mut vt = SessionVt::new(100, 32, 5000).unwrap();
            vt.write(&data);
            let cur = vt.cursor();
            let sb = vt.scrollbar();
            let m = vt.modes();
            assert_eq!(cur.x.to_string(), row[8], "{key}: 光标 X");
            assert_eq!(cur.y.to_string(), row[9], "{key}: 光标 Y");
            assert_eq!(sb.total.to_string(), row[12], "{key}: 回滚 total");
            assert_eq!(sb.offset.to_string(), row[13], "{key}: 回滚 offset");
            assert_eq!(sb.len.to_string(), row[14], "{key}: 回滚 len");
            // 模式位（wire u32：DECCKM=1|M1000=2|M1002=4|M1003=8|M1006=16|Focus=32|Bracketed=64|Alt=128）
            let mut bits = 0u32;
            let mm = vt.modes();
            if mm.cursor_keys_app { bits |= 1; }
            if mm.mouse_normal { bits |= 2; }
            if mm.mouse_button { bits |= 4; }
            if mm.mouse_any { bits |= 8; }
            if mm.mouse_sgr { bits |= 16; }
            if mm.focus_events { bits |= 32; }
            if mm.bracketed_paste { bits |= 64; }
            if mm.screen == Screen::Alternate { bits |= 128; }
            let _ = m;
            assert_eq!(bits.to_string(), row[16], "{key}: 模式位");
        }
    }
}
