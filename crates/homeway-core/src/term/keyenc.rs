//! keyenc — 自建键/鼠标/焦点/粘贴编码器（R6 6c）。
//!
//! Rust 生态无服务端编码实现 ⇒ 按 libghostty-vt 语义自建。**行为真源 =
//! `fixtures/vectors/term_keyenc.json`（387 案）与 `term_mouseenc.json`（160 案）**
//! （克隆 harness 直调 Go `vt.Terminal.EncodeKey/EncodeMouse` 实测），决策树逐支对齐
//! libghostty-vt `src/input/{key_encode,mouse_encode,function_keys,kitty}.zig`。
//!
//! 输入面口径（与 Go `pkg/term/vt/input.go` 的调用形态一致）：
//! - **UnshiftedCodepoint 恒 0**（wire 的 key 载荷不带该字段，Go 组 KeyEvent 时不填）
//!   ⇒ kitty 表查不到的字符键**没有合成表项**，按纯文本事件处理；
//! - **composing 恒由 wire 载荷携带**（Go 同名 bool 直传）；
//! - **consumed_mods 不存在**（Go 不设 ⇒ effective mods ≡ 全量 mods）。
//!
//! 平台分支（D-10，ghostty 编译期分支的同形物）：macOS 上 super 抑制文本直发、
//! option-as-alt 恒 `.false`（wire 无该配置面 ⇒ alt 前缀/mok2 alt 位按 darwin 口径剥离）；
//! Linux 上 alt 前缀生效（1036 默认 on）。**向量由 darwin 宿主产出**——本机测试在
//! darwin 上逐字节对拍；linux 分支无向量判据（6g 补，登记 D-10）。
//!
//! 鼠标编码面消费 [`crate::term::vt`] 的 **B5 last-set 单值**
//! （`MouseTracking`/`MouseFormat`，ghostty `flags.mouse_event/format` 语义），
//! 与 wire 的独立模式位并存；几何口径 = Go `tier_vt_mouse_size_grid`（cell 1×1、
//! screen = cols×rows ⇒ 上行网格坐标即编码器像素坐标）；`any_button_pressed` 恒
//! false、同格 motion **不去重**（Go 实跑形态，D-11）。

// Vec<u8> 的格式化写入走 io::Write（本工具链 1.99 起 fmt::Write 不再有 Vec<u8> impl）
use std::io::Write as _;

use super::vt::{MouseFormat, MouseTracking};

/// 键码（W3C UI Events code 口径；数值 = ghostty `Key` 枚举序号，与
/// term_keyenc.json 的 `keys` 表一致）。newtype 防 u16 裸传。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Key(pub u16);


/// 按键动作（ghostty `input.Action` 同值：release=0/press=1/repeat=2）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum KeyAction {
    Release = 0,
    #[default]
    Press = 1,
    Repeat = 2,
}

impl KeyAction {
    pub const fn from_wire(v: u8) -> Option<Self> {
        match v {
            0 => Some(KeyAction::Release),
            1 => Some(KeyAction::Press),
            2 => Some(KeyAction::Repeat),
            _ => None,
        }
    }
}

/// 修饰符位掩码（wire 口径 = Go `Mods` = ghostty `Mods` 位序）：
/// shift 1 / ctrl 2 / alt 4 / super 8 / caps 16 / num 32 / 侧位 64..512。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Mods(pub u16);

impl Mods {
    pub const NONE: Mods = Mods(0);
    pub const fn shift(self) -> bool {
        self.0 & 1 != 0
    }
    pub const fn ctrl(self) -> bool {
        self.0 & 2 != 0
    }
    pub const fn alt(self) -> bool {
        self.0 & 4 != 0
    }
    pub const fn super_(self) -> bool {
        self.0 & 8 != 0
    }
    pub const fn caps_lock(self) -> bool {
        self.0 & 16 != 0
    }
    pub const fn num_lock(self) -> bool {
        self.0 & 32 != 0
    }

    /// 只保留可绑定修饰（shift/ctrl/alt/super；ghostty `Mods.binding()`——锁键与
    /// 侧位不参与表匹配）。
    pub const fn binding(self) -> Mods {
        Mods(self.0 & 0xF)
    }

    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }
}

/// 一次抽象按键上行（Go `vt.KeyEvent` 的 wire 子集：无 unshifted_codepoint/
/// consumed_mods——见模块头）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyEvent<'a> {
    pub key: Key,
    pub action: KeyAction,
    pub mods: Mods,
    /// 平台上屏文本（可为空）。恒为合法 UTF-8（wire 解码层保证；ghostty 的
    /// 「非法 UTF-8 视为无文本」分支在此不可达）。
    pub text: &'a str,
    pub composing: bool,
}

/// 编码选项（ghostty `key_encode.Options` 的服务端可观测面；由
/// [`crate::term::vt::SessionVt::key_options`] 从会话模式态快照）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyOptions {
    pub cursor_key_application: bool,      // DEC 1（DECCKM）
    pub keypad_key_application: bool,      // DEC 66（DECKPAM）
    pub backarrow_key_mode: bool,          // DEC 67（DECBKM）
    pub ignore_keypad_with_numlock: bool,  // DEC 1035（默认 on）
    pub alt_esc_prefix: bool,              // DEC 1036（默认 on）
    pub modify_other_keys_state_2: bool,   // xterm modifyOtherKeys mode 2
    /// kitty 键盘协议标志（位值 = 协议位 1/2/4/8/16）。
    pub kitty_flags: u8,
}

impl KeyOptions {
    /// ghostty 同款默认（1035/1036 默认 on；其余 off；kitty 关闭）。
    pub const DEFAULT: KeyOptions = KeyOptions {
        cursor_key_application: false,
        keypad_key_application: false,
        backarrow_key_mode: false,
        ignore_keypad_with_numlock: true,
        alt_esc_prefix: true,
        modify_other_keys_state_2: false,
        kitty_flags: 0,
    };
}

/// 服务端编码器固定按宿主平台走 ghostty 的编译期分支（D-10）。
const IS_DARWIN: bool = cfg!(target_os = "macos");

/// kitty 协议标志位（disambiguate 位不单独门任何编码分支——只贡献「kitty 开」
/// 的判定，见 encode_key 的 flags != 0 检查）。
const KF_REPORT_EVENTS: u8 = 2;
const KF_REPORT_ALTERNATES: u8 = 4;
const KF_REPORT_ALL: u8 = 8;
const KF_REPORT_ASSOCIATED: u8 = 16;

// ---------------------------------------------------------------------------
// kitty 功能键表（libghostty-vt `input/kitty.zig` raw_entries 全 81 条，值照抄）。
// ---------------------------------------------------------------------------

/// kitty 表项：`(key, code, final, modifier)`。
const KITTY_ENTRIES: &[(u16, u32, u8, bool)] = &[
    (120, 27, b'u', false),      // escape
    (58, 13, b'u', false),       // enter
    (64, 9, b'u', false),        // tab
    (53, 127, b'u', false),      // backspace
    (72, 2, b'~', false),        // insert
    (68, 3, b'~', false),        // delete
    (76, 1, b'D', false),        // arrow_left
    (77, 1, b'C', false),        // arrow_right
    (78, 1, b'A', false),        // arrow_up
    (75, 1, b'B', false),        // arrow_down
    (74, 5, b'~', false),        // page_up
    (73, 6, b'~', false),        // page_down
    (71, 1, b'H', false),        // home
    (69, 1, b'F', false),        // end
    (54, 57358, b'u', true),     // caps_lock
    (149, 57359, b'u', false),   // scroll_lock
    (79, 57360, b'u', true),     // num_lock
    (148, 57361, b'u', false),   // print_screen
    (150, 57362, b'u', false),   // pause
    (121, 1, b'P', false),       // f1
    (122, 1, b'Q', false),       // f2
    (123, 13, b'~', false),      // f3
    (124, 1, b'S', false),       // f4
    (125, 15, b'~', false),      // f5
    (126, 17, b'~', false),      // f6
    (127, 18, b'~', false),      // f7
    (128, 19, b'~', false),      // f8
    (129, 20, b'~', false),      // f9
    (130, 21, b'~', false),      // f10
    (131, 23, b'~', false),      // f11
    (132, 24, b'~', false),      // f12
    (133, 57376, b'u', false),   // f13
    (134, 57377, b'u', false),   // f14
    (135, 57378, b'u', false),   // f15
    (136, 57379, b'u', false),   // f16
    (137, 57380, b'u', false),   // f17
    (138, 57381, b'u', false),   // f18
    (139, 57382, b'u', false),   // f19
    (140, 57383, b'u', false),   // f20
    (141, 57384, b'u', false),   // f21
    (142, 57385, b'u', false),   // f22
    (143, 57386, b'u', false),   // f23
    (144, 57387, b'u', false),   // f24
    (145, 57388, b'u', false),   // f25
    (80, 57399, b'u', false),    // numpad_0
    (81, 57400, b'u', false),    // numpad_1
    (82, 57401, b'u', false),    // numpad_2
    (83, 57402, b'u', false),    // numpad_3
    (84, 57403, b'u', false),    // numpad_4
    (85, 57404, b'u', false),    // numpad_5
    (86, 57405, b'u', false),    // numpad_6
    (87, 57406, b'u', false),    // numpad_7
    (88, 57407, b'u', false),    // numpad_8
    (89, 57408, b'u', false),    // numpad_9
    (95, 57409, b'u', false),    // numpad_decimal
    (96, 57410, b'u', false),    // numpad_divide
    (104, 57411, b'u', false),   // numpad_multiply
    (107, 57412, b'u', false),   // numpad_subtract
    (90, 57413, b'u', false),    // numpad_add
    (97, 57414, b'u', false),    // numpad_enter
    (98, 57415, b'u', false),    // numpad_equal
    (108, 57416, b'u', false),   // numpad_separator
    (112, 57417, b'u', false),   // numpad_left
    (111, 57418, b'u', false),   // numpad_right
    (109, 57419, b'u', false),   // numpad_up
    (110, 57420, b'u', false),   // numpad_down
    (118, 57421, b'u', false),   // numpad_page_up
    (119, 57422, b'u', false),   // numpad_page_down
    (114, 57423, b'u', false),   // numpad_home
    (115, 57424, b'u', false),   // numpad_end
    (116, 57425, b'u', false),   // numpad_insert
    (117, 57426, b'u', false),   // numpad_delete
    (113, 57427, b'u', false),   // numpad_begin
    (61, 57441, b'u', true),     // shift_left
    (62, 57447, b'u', true),     // shift_right
    (56, 57442, b'u', true),     // control_left
    (57, 57448, b'u', true),     // control_right
    (59, 57444, b'u', true),     // meta_left（= super/command 左）
    (60, 57450, b'u', true),     // meta_right
    (51, 57443, b'u', true),     // alt_left
    (52, 57449, b'u', true),     // alt_right
];

/// 物理键 → 未修饰码点（ghostty `key.zig` codepoint_map；ctrl 回退与 kitty
/// alternates 基座码点用）。numpad 族在 ghostty 表里各自映射到同名字符。
fn key_codepoint(key: Key) -> Option<char> {
    Some(match key.0 {
        20..=45 => char::from_u32(b'a' as u32 + (key.0 as u32 - 20))?, // key_a..key_z
        6..=15 => char::from_u32(b'0' as u32 + (key.0 as u32 - 6))?,   // digit_0..9
        49 => ';',
        63 => ' ',
        48 => '\'',
        5 => ',',
        1 => '`',
        47 => '.',
        50 => '/',
        46 => '-',
        16 => '=',
        3 => '[',
        4 => ']',
        2 => '\\',
        64 => '\t',
        80..=89 => char::from_u32(b'0' as u32 + (key.0 as u32 - 80))?, // numpad_0..9
        95 => '.',
        96 => '/',
        104 => '*',
        107 => '-',
        90 => '+',
        98 => '=',
        _ => return None,
    })
}

// ---------------------------------------------------------------------------
// legacy PC 功能键表（function_keys.zig）。
// ---------------------------------------------------------------------------

/// 光标键门（DECCKM）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CursorMode {
    Any,
    Normal,
    Application,
}

/// 小键盘门（DECKPAM + 1035）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KeypadMode {
    Any,
    Normal,
    Application,
}

/// modifyOtherKeys 门（xterm `>4;2m` = set_other）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MokMode {
    Any,
    Set,
    SetOther,
}

/// 手写表用的修饰位名（wire 位序，见 [`Mods`]）。
const MB_S: u8 = 0b0001;
const MB_C: u8 = 0b0010;
const MB_A: u8 = 0b0100;
const MB_P: u8 = 0b1000; // super

/// modifyOtherKeys 的 15 修饰组合（function_keys.zig `modifiers` 声明序；矩阵码 = 序号 + 2）。
const MOK_MODIFIERS: [u8; 15] = [
    MB_S,                       // shift
    MB_A,                       // alt
    MB_S | MB_A,                // shift+alt
    MB_C,                       // ctrl
    MB_S | MB_C,                // shift+ctrl
    MB_A | MB_C,                // alt+ctrl
    MB_S | MB_A | MB_C,         // shift+alt+ctrl
    MB_P,                       // super
    MB_S | MB_P,                // shift+super
    MB_A | MB_P,                // alt+super
    MB_S | MB_A | MB_P,         // shift+alt+super
    MB_C | MB_P,                // ctrl+super
    MB_S | MB_C | MB_P,         // shift+ctrl+super
    MB_A | MB_C | MB_P,         // alt+ctrl+super
    MB_S | MB_A | MB_C | MB_P,  // shift+alt+ctrl+super
];

/// 单条功能键规则（表项按声明序匹配，先中先得）。
enum Rule {
    /// pcStyle 修饰矩阵：`prefix + 矩阵码 + fin`（15 条 mods 精确匹配项的生成形态；
    /// `keypad` 门给小键盘族用）。
    PcMods { prefix: &'static str, fin: &'static str, keypad: KeypadMode },
    /// 字面表项。
    Lit {
        mods: u8, // binding 位；0 = 空
        empty_any: bool,
        cursor: CursorMode,
        keypad: KeypadMode,
        mok: MokMode,
        seq: &'static str,
        decbkm: Option<&'static str>,
    },
}

use CursorMode as Cm;
use KeypadMode as Km;
use MokMode as Mk;
use Rule::{Lit, PcMods};

/// 光标键两态（normal/application）。
const fn cursor_key(normal: &'static str, application: &'static str) -> [Rule; 2] {
    [
        Lit { mods: 0, empty_any: true, cursor: Cm::Normal, keypad: Km::Any, mok: Mk::Any, seq: normal, decbkm: None },
        Lit { mods: 0, empty_any: true, cursor: Cm::Application, keypad: Km::Any, mok: Mk::Any, seq: application, decbkm: None },
    ]
}

/// mods 空且 empty_any 的普通字面项。
const fn lit(seq: &'static str) -> Rule {
    Lit { mods: 0, empty_any: true, cursor: Cm::Any, keypad: Km::Any, mok: Mk::Any, seq, decbkm: None }
}

/// 全表（function_keys.zig `keys`；键值 = Key 枚举序）。
fn function_keys() -> &'static [(u16, Vec<Rule>)] {
    use std::sync::OnceLock;
    static TABLE: OnceLock<Vec<(u16, Vec<Rule>)>> = OnceLock::new();
    TABLE.get_or_init(|| {
        fn kpd(suffix: &'static str) -> Rule {
            // kpDefault：mods 空、精确（empty_any=false）、keypad=application
            let seq: &'static str = leak_concat("\x1bO", suffix);
            Lit { mods: 0, empty_any: false, cursor: Cm::Any, keypad: Km::Application, mok: Mk::Any, seq, decbkm: None }
        }
        fn kp(suffix: &'static str, normal: &'static str) -> Vec<Rule> {
            vec![
                kpd(suffix),
                PcMods { prefix: "\x1bO", fin: suffix, keypad: Km::Application },
                Lit { mods: 0, empty_any: true, cursor: Cm::Any, keypad: Km::Normal, mok: Mk::Any, seq: normal, decbkm: None },
            ]
        }
        fn pc(prefix: &'static str, fin: &'static str) -> Vec<Rule> {
            vec![PcMods { prefix, fin, keypad: Km::Any }]
        }
        fn arrows(prefix: &'static str, fin: &'static str, normal: &'static str, application: &'static str) -> Vec<Rule> {
            let mut v = pc(prefix, fin);
            v.extend(cursor_key(normal, application));
            v
        }
        fn pc_lit(prefix: &'static str, fin: &'static str, plain: &'static str) -> Vec<Rule> {
            let mut v = pc(prefix, fin);
            v.push(lit(plain));
            v
        }
        vec![
            (78, arrows("\x1b[1;", "A", "\x1b[A", "\x1bOA")), // arrow_up
            (75, arrows("\x1b[1;", "B", "\x1b[B", "\x1bOB")), // arrow_down
            (77, arrows("\x1b[1;", "C", "\x1b[C", "\x1bOC")), // arrow_right
            (76, arrows("\x1b[1;", "D", "\x1b[D", "\x1bOD")), // arrow_left
            (71, arrows("\x1b[1;", "H", "\x1b[H", "\x1bOH")), // home
            (69, arrows("\x1b[1;", "F", "\x1b[F", "\x1bOF")), // end
            (72, pc_lit("\x1b[2;", "~", "\x1b[2~")),           // insert
            (68, pc_lit("\x1b[3;", "~", "\x1b[3~")),           // delete
            (74, pc_lit("\x1b[5;", "~", "\x1b[5~")),           // page_up
            (73, pc_lit("\x1b[6;", "~", "\x1b[6~")),           // page_down
            (70, pc_lit("\x1b[28;", "~", "\x1b[28~")),         // help
            (55, pc_lit("\x1b[29;", "~", "\x1b[29~")),         // context_menu
            (121, pc_lit("\x1b[1;", "P", "\x1bOP")),           // f1
            (122, pc_lit("\x1b[1;", "Q", "\x1bOQ")),           // f2
            (123, pc_lit("\x1b[13;", "~", "\x1bOR")),          // f3
            (124, pc_lit("\x1b[1;", "S", "\x1bOS")),           // f4
            (125, pc_lit("\x1b[15;", "~", "\x1b[15~")),        // f5
            (126, pc_lit("\x1b[17;", "~", "\x1b[17~")),        // f6
            (127, pc_lit("\x1b[18;", "~", "\x1b[18~")),        // f7
            (128, pc_lit("\x1b[19;", "~", "\x1b[19~")),        // f8
            (129, pc_lit("\x1b[20;", "~", "\x1b[20~")),        // f9
            (130, pc_lit("\x1b[21;", "~", "\x1b[21~")),        // f10
            (131, pc_lit("\x1b[23;", "~", "\x1b[23~")),        // f11
            (132, pc_lit("\x1b[24;", "~", "\x1b[24~")),        // f12
            (133, pc_lit("\x1b[25;", "~", "\x1b[25~")),        // f13
            (134, pc_lit("\x1b[26;", "~", "\x1b[26~")),        // f14
            (135, pc_lit("\x1b[28;", "~", "\x1b[28~")),        // f15
            (136, pc_lit("\x1b[29;", "~", "\x1b[29~")),        // f16
            (137, pc_lit("\x1b[31;", "~", "\x1b[31~")),        // f17
            (138, pc_lit("\x1b[32;", "~", "\x1b[32~")),        // f18
            (139, pc_lit("\x1b[33;", "~", "\x1b[33~")),        // f19
            (140, pc_lit("\x1b[34;", "~", "\x1b[34~")),        // f20
            (141, pc_lit("\x1b[42;", "~", "\x1b[42~")),        // f21
            (142, pc_lit("\x1b[43;", "~", "\x1b[43~")),        // f22
            (143, pc_lit("\x1b[44;", "~", "\x1b[44~")),        // f23
            (144, pc_lit("\x1b[45;", "~", "\x1b[45~")),        // f24
            (145, pc_lit("\x1b[46;", "~", "\x1b[46~")),        // f25
            (80, kp("p", "0")),   // numpad_0
            (81, kp("q", "1")),   // numpad_1
            (82, kp("r", "2")),   // numpad_2
            (83, kp("s", "3")),   // numpad_3
            (84, kp("t", "4")),   // numpad_4
            (85, kp("u", "5")),   // numpad_5
            (86, kp("v", "6")),   // numpad_6
            (87, kp("w", "7")),   // numpad_7
            (88, kp("x", "8")),   // numpad_8
            (89, kp("y", "9")),   // numpad_9
            (95, kp("n", ".")),   // numpad_decimal
            (96, kp("o", "/")),   // numpad_divide
            (104, kp("j", "*")),  // numpad_multiply
            (107, kp("m", "-")),  // numpad_subtract
            (90, kp("k", "+")),   // numpad_add
            (97, kp("M", "\r")),  // numpad_enter
            (109, arrows("\x1b[1;", "A", "\x1b[A", "\x1bOA")), // numpad_up
            (110, arrows("\x1b[1;", "B", "\x1b[B", "\x1bOB")), // numpad_down
            (111, arrows("\x1b[1;", "C", "\x1b[C", "\x1bOC")), // numpad_right
            (112, arrows("\x1b[1;", "D", "\x1b[D", "\x1bOD")), // numpad_left
            (113, arrows("\x1b[1;", "E", "\x1b[E", "\x1bOE")), // numpad_begin
            (114, arrows("\x1b[1;", "H", "\x1b[H", "\x1bOH")), // numpad_home
            (115, arrows("\x1b[1;", "F", "\x1b[F", "\x1bOF")), // numpad_end
            (116, pc_lit("\x1b[2;", "~", "\x1b[2~")),          // numpad_insert
            (117, pc_lit("\x1b[3;", "~", "\x1b[3~")),          // numpad_delete
            (118, pc_lit("\x1b[5;", "~", "\x1b[5~")),          // numpad_page_up
            (119, pc_lit("\x1b[6;", "~", "\x1b[6~")),          // numpad_page_down
            // backspace：mok 两族 + ctrl/decbkm 项（顺序照抄 function_keys.zig）
            (53, vec![
                // Modify Keys Normal
                Lit { mods: 0b0001, empty_any: false, cursor: Cm::Any, keypad: Km::Any, mok: Mk::Set, seq: "\x7f", decbkm: None },
                Lit { mods: 0b0100, empty_any: false, cursor: Cm::Any, keypad: Km::Any, mok: Mk::Set, seq: "\x1b\x7f", decbkm: None },
                Lit { mods: 0b0101, empty_any: false, cursor: Cm::Any, keypad: Km::Any, mok: Mk::Set, seq: "\x1b\x7f", decbkm: None },
                Lit { mods: 0b0011, empty_any: false, cursor: Cm::Any, keypad: Km::Any, mok: Mk::Set, seq: "\x08", decbkm: None },
                Lit { mods: 0b0110, empty_any: false, cursor: Cm::Any, keypad: Km::Any, mok: Mk::Set, seq: "\x1b\x08", decbkm: None },
                Lit { mods: 0b1000, empty_any: false, cursor: Cm::Any, keypad: Km::Any, mok: Mk::Set, seq: "\x7f", decbkm: None },
                Lit { mods: 0b1001, empty_any: false, cursor: Cm::Any, keypad: Km::Any, mok: Mk::Set, seq: "\x7f", decbkm: None },
                Lit { mods: 0b1100, empty_any: false, cursor: Cm::Any, keypad: Km::Any, mok: Mk::Set, seq: "\x1b\x7f", decbkm: None },
                Lit { mods: 0b1101, empty_any: false, cursor: Cm::Any, keypad: Km::Any, mok: Mk::Set, seq: "\x1b\x7f", decbkm: None },
                Lit { mods: 0b1010, empty_any: false, cursor: Cm::Any, keypad: Km::Any, mok: Mk::Set, seq: "\x08", decbkm: None },
                Lit { mods: 0b1011, empty_any: false, cursor: Cm::Any, keypad: Km::Any, mok: Mk::Set, seq: "\x08", decbkm: None },
                Lit { mods: 0b1110, empty_any: false, cursor: Cm::Any, keypad: Km::Any, mok: Mk::Set, seq: "\x1b\x08", decbkm: None },
                Lit { mods: 0b1111, empty_any: false, cursor: Cm::Any, keypad: Km::Any, mok: Mk::Set, seq: "\x1b\x08", decbkm: None },
                // Modify Keys Other（set_other）
                Lit { mods: 0b0001, empty_any: false, cursor: Cm::Any, keypad: Km::Any, mok: Mk::SetOther, seq: "\x1b[27;2;127~", decbkm: None },
                Lit { mods: 0b0100, empty_any: false, cursor: Cm::Any, keypad: Km::Any, mok: Mk::SetOther, seq: "\x1b[27;3;127~", decbkm: None },
                Lit { mods: 0b0101, empty_any: false, cursor: Cm::Any, keypad: Km::Any, mok: Mk::SetOther, seq: "\x1b[27;4;127~", decbkm: None },
                Lit { mods: 0b0011, empty_any: false, cursor: Cm::Any, keypad: Km::Any, mok: Mk::SetOther, seq: "\x1b[27;6;127~", decbkm: None },
                Lit { mods: 0b0110, empty_any: false, cursor: Cm::Any, keypad: Km::Any, mok: Mk::SetOther, seq: "\x1b[27;7;127~", decbkm: None },
                Lit { mods: 0b0111, empty_any: false, cursor: Cm::Any, keypad: Km::Any, mok: Mk::SetOther, seq: "\x1b[27;8;127~", decbkm: None },
                Lit { mods: 0b1000, empty_any: false, cursor: Cm::Any, keypad: Km::Any, mok: Mk::SetOther, seq: "\x1b[27;9;127~", decbkm: None },
                Lit { mods: 0b1001, empty_any: false, cursor: Cm::Any, keypad: Km::Any, mok: Mk::SetOther, seq: "\x1b[27;10;127~", decbkm: None },
                Lit { mods: 0b1100, empty_any: false, cursor: Cm::Any, keypad: Km::Any, mok: Mk::SetOther, seq: "\x1b[27;11;127~", decbkm: None },
                Lit { mods: 0b1101, empty_any: false, cursor: Cm::Any, keypad: Km::Any, mok: Mk::SetOther, seq: "\x1b[27;12;127~", decbkm: None },
                Lit { mods: 0b1010, empty_any: false, cursor: Cm::Any, keypad: Km::Any, mok: Mk::SetOther, seq: "\x1b[27;13;127~", decbkm: None },
                Lit { mods: 0b1011, empty_any: false, cursor: Cm::Any, keypad: Km::Any, mok: Mk::SetOther, seq: "\x1b[27;14;127~", decbkm: None },
                Lit { mods: 0b1110, empty_any: false, cursor: Cm::Any, keypad: Km::Any, mok: Mk::SetOther, seq: "\x1b[27;15;127~", decbkm: None },
                Lit { mods: 0b1111, empty_any: false, cursor: Cm::Any, keypad: Km::Any, mok: Mk::SetOther, seq: "\x1b[27;16;127~", decbkm: None },
                // ctrl（DECBKM 翻转）与裸键
                Lit { mods: 0b0010, empty_any: false, cursor: Cm::Any, keypad: Km::Any, mok: Mk::Any, seq: "\x08", decbkm: Some("\x7f") },
                Lit { mods: 0, empty_any: true, cursor: Cm::Any, keypad: Km::Any, mok: Mk::Any, seq: "\x7f", decbkm: Some("\x08") },
            ]),
            // tab
            (64, vec![
                Lit { mods: 0b0001, empty_any: false, cursor: Cm::Any, keypad: Km::Any, mok: Mk::Set, seq: "\x1b[Z", decbkm: None },
                Lit { mods: 0b0100, empty_any: false, cursor: Cm::Any, keypad: Km::Any, mok: Mk::Set, seq: "\x1b\t", decbkm: None },
                Lit { mods: 0b0001, empty_any: false, cursor: Cm::Any, keypad: Km::Any, mok: Mk::SetOther, seq: "\x1b[27;2;9~", decbkm: None },
                Lit { mods: 0b0100, empty_any: false, cursor: Cm::Any, keypad: Km::Any, mok: Mk::SetOther, seq: "\x1b[27;3;9~", decbkm: None },
                Lit { mods: 0b0101, empty_any: false, cursor: Cm::Any, keypad: Km::Any, mok: Mk::Any, seq: "\x1b[27;4;9~", decbkm: None },
                Lit { mods: 0b0010, empty_any: false, cursor: Cm::Any, keypad: Km::Any, mok: Mk::Any, seq: "\x1b[27;5;9~", decbkm: None },
                Lit { mods: 0b0011, empty_any: false, cursor: Cm::Any, keypad: Km::Any, mok: Mk::Any, seq: "\x1b[27;6;9~", decbkm: None },
                Lit { mods: 0b0110, empty_any: false, cursor: Cm::Any, keypad: Km::Any, mok: Mk::Any, seq: "\x1b[27;7;9~", decbkm: None },
                Lit { mods: 0b0111, empty_any: false, cursor: Cm::Any, keypad: Km::Any, mok: Mk::Any, seq: "\x1b[27;8;9~", decbkm: None },
                Lit { mods: 0b1000, empty_any: false, cursor: Cm::Any, keypad: Km::Any, mok: Mk::Any, seq: "\x1b[27;9;9~", decbkm: None },
                Lit { mods: 0b1001, empty_any: false, cursor: Cm::Any, keypad: Km::Any, mok: Mk::Any, seq: "\x1b[27;10;9~", decbkm: None },
                Lit { mods: 0b1100, empty_any: false, cursor: Cm::Any, keypad: Km::Any, mok: Mk::Any, seq: "\x1b[27;11;9~", decbkm: None },
                Lit { mods: 0b1101, empty_any: false, cursor: Cm::Any, keypad: Km::Any, mok: Mk::Any, seq: "\x1b[27;12;9~", decbkm: None },
                Lit { mods: 0b1010, empty_any: false, cursor: Cm::Any, keypad: Km::Any, mok: Mk::Any, seq: "\x1b[27;13;9~", decbkm: None },
                Lit { mods: 0b1011, empty_any: false, cursor: Cm::Any, keypad: Km::Any, mok: Mk::Any, seq: "\x1b[27;14;9~", decbkm: None },
                Lit { mods: 0b1110, empty_any: false, cursor: Cm::Any, keypad: Km::Any, mok: Mk::Any, seq: "\x1b[27;15;9~", decbkm: None },
                Lit { mods: 0b1111, empty_any: false, cursor: Cm::Any, keypad: Km::Any, mok: Mk::Any, seq: "\x1b[27;16;9~", decbkm: None },
                lit("\t"),
            ]),
            // enter
            (58, vec![
                Lit { mods: 0b0001, empty_any: false, cursor: Cm::Any, keypad: Km::Any, mok: Mk::Any, seq: "\x1b[27;2;13~", decbkm: None },
                Lit { mods: 0b0100, empty_any: false, cursor: Cm::Any, keypad: Km::Any, mok: Mk::Set, seq: "\x1b\r", decbkm: None },
                Lit { mods: 0b0100, empty_any: false, cursor: Cm::Any, keypad: Km::Any, mok: Mk::SetOther, seq: "\x1b[27;3;13~", decbkm: None },
                Lit { mods: 0b0101, empty_any: false, cursor: Cm::Any, keypad: Km::Any, mok: Mk::Any, seq: "\x1b[27;4;13~", decbkm: None },
                Lit { mods: 0b0010, empty_any: false, cursor: Cm::Any, keypad: Km::Any, mok: Mk::Any, seq: "\x1b[27;5;13~", decbkm: None },
                Lit { mods: 0b0011, empty_any: false, cursor: Cm::Any, keypad: Km::Any, mok: Mk::Any, seq: "\x1b[27;6;13~", decbkm: None },
                Lit { mods: 0b0110, empty_any: false, cursor: Cm::Any, keypad: Km::Any, mok: Mk::Any, seq: "\x1b[27;7;13~", decbkm: None },
                Lit { mods: 0b0111, empty_any: false, cursor: Cm::Any, keypad: Km::Any, mok: Mk::Any, seq: "\x1b[27;8;13~", decbkm: None },
                Lit { mods: 0b1000, empty_any: false, cursor: Cm::Any, keypad: Km::Any, mok: Mk::Any, seq: "\x1b[27;9;13~", decbkm: None },
                Lit { mods: 0b1001, empty_any: false, cursor: Cm::Any, keypad: Km::Any, mok: Mk::Any, seq: "\x1b[27;10;13~", decbkm: None },
                Lit { mods: 0b1100, empty_any: false, cursor: Cm::Any, keypad: Km::Any, mok: Mk::Any, seq: "\x1b[27;11;13~", decbkm: None },
                Lit { mods: 0b1101, empty_any: false, cursor: Cm::Any, keypad: Km::Any, mok: Mk::Any, seq: "\x1b[27;12;13~", decbkm: None },
                Lit { mods: 0b1010, empty_any: false, cursor: Cm::Any, keypad: Km::Any, mok: Mk::Any, seq: "\x1b[27;13;13~", decbkm: None },
                Lit { mods: 0b1011, empty_any: false, cursor: Cm::Any, keypad: Km::Any, mok: Mk::Any, seq: "\x1b[27;14;13~", decbkm: None },
                Lit { mods: 0b1110, empty_any: false, cursor: Cm::Any, keypad: Km::Any, mok: Mk::Any, seq: "\x1b[27;15;13~", decbkm: None },
                Lit { mods: 0b1111, empty_any: false, cursor: Cm::Any, keypad: Km::Any, mok: Mk::Any, seq: "\x1b[27;16;13~", decbkm: None },
                lit("\r"),
            ]),
            // escape
            (120, vec![
                Lit { mods: 0b0001, empty_any: false, cursor: Cm::Any, keypad: Km::Any, mok: Mk::Any, seq: "\x1b[27;2;27~", decbkm: None },
                Lit { mods: 0b0100, empty_any: false, cursor: Cm::Any, keypad: Km::Any, mok: Mk::Set, seq: "\x1b\x1b", decbkm: None },
                Lit { mods: 0b0100, empty_any: false, cursor: Cm::Any, keypad: Km::Any, mok: Mk::SetOther, seq: "\x1b[27;3;27~", decbkm: None },
                Lit { mods: 0b0101, empty_any: false, cursor: Cm::Any, keypad: Km::Any, mok: Mk::Any, seq: "\x1b[27;4;27~", decbkm: None },
                Lit { mods: 0b0010, empty_any: false, cursor: Cm::Any, keypad: Km::Any, mok: Mk::Any, seq: "\x1b[27;5;27~", decbkm: None },
                Lit { mods: 0b0011, empty_any: false, cursor: Cm::Any, keypad: Km::Any, mok: Mk::Any, seq: "\x1b[27;6;27~", decbkm: None },
                Lit { mods: 0b0110, empty_any: false, cursor: Cm::Any, keypad: Km::Any, mok: Mk::Any, seq: "\x1b[27;7;27~", decbkm: None },
                Lit { mods: 0b0111, empty_any: false, cursor: Cm::Any, keypad: Km::Any, mok: Mk::Any, seq: "\x1b[27;8;27~", decbkm: None },
                Lit { mods: 0b1000, empty_any: false, cursor: Cm::Any, keypad: Km::Any, mok: Mk::Any, seq: "\x1b[27;9;27~", decbkm: None },
                Lit { mods: 0b1001, empty_any: false, cursor: Cm::Any, keypad: Km::Any, mok: Mk::Any, seq: "\x1b[27;10;27~", decbkm: None },
                Lit { mods: 0b1100, empty_any: false, cursor: Cm::Any, keypad: Km::Any, mok: Mk::Any, seq: "\x1b[27;11;27~", decbkm: None },
                Lit { mods: 0b1101, empty_any: false, cursor: Cm::Any, keypad: Km::Any, mok: Mk::Any, seq: "\x1b[27;12;27~", decbkm: None },
                Lit { mods: 0b1010, empty_any: false, cursor: Cm::Any, keypad: Km::Any, mok: Mk::Any, seq: "\x1b[27;13;27~", decbkm: None },
                Lit { mods: 0b1011, empty_any: false, cursor: Cm::Any, keypad: Km::Any, mok: Mk::Any, seq: "\x1b[27;14;27~", decbkm: None },
                Lit { mods: 0b1110, empty_any: false, cursor: Cm::Any, keypad: Km::Any, mok: Mk::Any, seq: "\x1b[27;15;27~", decbkm: None },
                Lit { mods: 0b1111, empty_any: false, cursor: Cm::Any, keypad: Km::Any, mok: Mk::Any, seq: "\x1b[27;16;27~", decbkm: None },
                lit("\x1b"),
            ]),
        ]
    })
}

/// 一次性拼接泄漏（kpDefault 的 `\x1bO{sx}`；表只建一次，静态生命周期）。
fn leak_concat(a: &'static str, b: &'static str) -> &'static str {
    let mut s = String::with_capacity(a.len() + b.len());
    s.push_str(a);
    s.push_str(b);
    Box::leak(s.into_boxed_str())
}

/// PC 功能键匹配（key_encode.zig `pcStyleFunctionKey`）：按表序找首个通过
/// cursor/keypad/mok/mods 四道门的项。命中返回完整序列（PcMods 即时格式化）。
fn pc_style_function_key(
    key: Key,
    binding: Mods,
    opts: &KeyOptions,
) -> Option<Vec<u8>> {
    let mods_int = binding.0 as u8;
    // 1035（默认 on）⇒ 小键盘恒数值模式（ghostty 的 numlock 隐式判定）
    let keypad_app = !opts.ignore_keypad_with_numlock && opts.keypad_key_application;
    for (k, rules) in function_keys() {
        if *k != key.0 {
            continue;
        }
        for rule in rules {
            match rule {
                Rule::PcMods { prefix, fin, keypad } => {
                    if !keypad_gate(*keypad, keypad_app) {
                        continue;
                    }
                    let Some(i) = MOK_MODIFIERS.iter().position(|m| *m == mods_int) else {
                        continue;
                    };
                    let mut out = Vec::with_capacity(prefix.len() + 4);
                    out.extend_from_slice(prefix.as_bytes());
                    // 矩阵码 = 序号 + 2（前缀已含 num; 或 \x1bO）
                    let _ = write!(out, "{}{}", i + 2, fin);
                    return Some(out);
                }
                Rule::Lit { mods, empty_any, cursor, keypad, mok, seq, decbkm } => {
                    if !cursor_gate(*cursor, opts.cursor_key_application) {
                        continue;
                    }
                    if !keypad_gate(*keypad, keypad_app) {
                        continue;
                    }
                    if !mok_gate(*mok, opts.modify_other_keys_state_2) {
                        continue;
                    }
                    if *mods == 0 {
                        if mods_int != 0 && !*empty_any {
                            continue;
                        }
                    } else if *mods != mods_int {
                        continue;
                    }
                    if opts.backarrow_key_mode {
                        if let Some(d) = decbkm {
                            return Some(d.as_bytes().to_vec());
                        }
                    }
                    return Some(seq.as_bytes().to_vec());
                }
            }
        }
        return None; // 找到键组但无匹配项
    }
    None
}

fn cursor_gate(rule: CursorMode, app: bool) -> bool {
    match rule {
        CursorMode::Any => true,
        CursorMode::Normal => !app,
        CursorMode::Application => app,
    }
}

fn keypad_gate(rule: KeypadMode, app: bool) -> bool {
    match rule {
        KeypadMode::Any => true,
        KeypadMode::Normal => !app,
        KeypadMode::Application => app,
    }
}

fn mok_gate(rule: MokMode, mok2: bool) -> bool {
    match rule {
        MokMode::Any => true,
        MokMode::Set => !mok2,
        MokMode::SetOther => mok2,
    }
}

/// ctrl → C0 白名单（key_encode.zig `ctrlSeq` 的映射表；i/m/[/` 故意缺席 = fixterms
/// 归 CSI u）。返回 C0 字节。
fn ctrl_c0(char: u8) -> Option<u8> {
    Some(match char {
        b' ' => 0,
        b'/' => 31,
        b'0' => 48,
        b'1' => 49,
        b'2' => 0,
        b'3' => 27,
        b'4' => 28,
        b'5' => 29,
        b'6' => 30,
        b'7' => 31,
        b'8' => 127,
        b'9' => 57,
        b'?' => 127,
        b'@' => 0,
        b'\\' => 28,
        b']' => 29,
        b'^' => 30,
        b'_' => 31,
        b'a'..=b'h' => char - b'a' + 1,
        b'j' | b'k' | b'l' => char - b'j' + 10,
        b'n'..=b'z' => char - b'n' + 14,
        // 'i'（0x09）/ 'm'（0x0D）/ '['（0x1B）fixterms 特意排除 ⇒ CSI u
        b'~' => 30,
        _ => return None,
    })
}

/// ctrl → C0 判定（`ctrlSeq` 全逻辑；unshifted_codepoint ≡ 0 ⇒ 大写回退分支不可达）。
fn ctrl_seq(key: Key, text: &str, mods: Mods) -> Option<u8> {
    const CTRL_ONLY: u16 = 0b0010; // 仅 ctrl 的 binding 位（wire 位序）

    if !mods.ctrl() {
        return None;
    }
    // 只取可绑定修饰，剥 alt（ESC 前缀逻辑另行处理）
    let mut unset = mods.binding();
    unset = Mods(unset.0 & !0b0100); // 剥 alt（ESC 前缀逻辑另行处理）

    // 只取可绑定修饰，剥 alt（ESC 前缀逻辑另行处理）；unshifted ≡ 0 ⇒ 大写
    // 回退分支（caps-lock 场景）不可达，char 不再重赋值
    let char: u8 = if text.len() == 1 {
        text.as_bytes()[0]
    } else {
        // 布局回退：物理键自带可打印 ASCII 时仅在「恰好只按 ctrl」下采信
        let cp = key_codepoint(key)? as u32;
        let byte = u8::try_from(cp).ok()?;
        if unset.0 != CTRL_ONLY {
            return None;
        }
        byte
    };

    // 大写区外的 shift 剥除（fixterms：ctrl+shift+- 仍产 C0；'@' 特例保留）
    if unset.shift() && !(char.is_ascii_uppercase() || char == b'@') {
        unset = Mods(unset.0 & !0b0001);
    }

    if unset.0 != CTRL_ONLY {
        return None;
    }
    ctrl_c0(char)
}

/// alt 前缀（`legacyAltPrefix`；macos_option_as_alt ≡ false ⇒ darwin 恒无前缀，
/// unshifted ≡ 0 ⇒ 无「仅码点」分支——两条简化都有向量/源码依据）。
fn legacy_alt_prefix(out: &mut Vec<u8>, binding: Mods, text: &str, opts: &KeyOptions) -> bool {
    if !binding.alt() || !opts.alt_esc_prefix {
        return false;
    }
    if IS_DARWIN {
        return false; // option-as-alt = .false（wire 无该配置面）
    }
    if text.is_empty() {
        return false;
    }
    out.push(0x1b);
    out.extend_from_slice(text.as_bytes());
    true
}

fn is_control(cp: u32) -> bool {
    cp < 0x20 || cp == 0x7f
}

/// 恰一个码点的文本（多码点/空 ⇒ None）。
fn single_codepoint(text: &str) -> Option<char> {
    let mut it = text.chars();
    let c = it.next()?;
    it.next().is_none().then_some(c)
}

// ---------------------------------------------------------------------------
// 主入口
// ---------------------------------------------------------------------------

/// 键编码（`key_encode.encode`）：kitty 开 ⇒ kitty 路径，否则 legacy。
/// 无输出（模式抑制/纯修饰等）返回空 Vec。
pub fn encode_key(ev: &KeyEvent, opts: &KeyOptions) -> Vec<u8> {
    let mut out = Vec::with_capacity(16);
    if opts.kitty_flags != 0 {
        kitty_encode(&mut out, ev, opts);
    } else {
        legacy_encode(&mut out, ev, opts);
    }
    out
}

/// legacy 路径（传统终端 + xterm modifyOtherKeys + fixterms CSI u 的组合）。
fn legacy_encode(out: &mut Vec<u8>, ev: &KeyEvent, opts: &KeyOptions) {
    let all_mods = ev.mods;
    let binding = all_mods.binding();

    // legacy 只编 press/repeat；死键组段中不出序列
    if !matches!(ev.action, KeyAction::Press | KeyAction::Repeat) {
        return;
    }
    if ev.composing {
        return;
    }

    // ① PC 功能键表
    if let Some(seq) = pc_style_function_key(ev.key, binding, opts) {
        // 带文本时的 IME 特例：控制字符文本 → 照发序列；backspace → 无输出；
        // enter/escape → 跳过 PC 编码（后续按提交文本处理）
        let mut fallthrough = false;
        if !ev.text.is_empty()
            && matches!(ev.key.0, 53 | 58 | 120) // backspace | enter | escape
        {
            let control = ev.text.len() == 1 && is_control(ev.text.as_bytes()[0] as u32);
            if !control {
                if ev.key.0 == 53 {
                    return; // backspace：IME 修正 ⇒ 无输出
                }
                fallthrough = true; // enter/escape：走文本路径
            }
        }
        if !fallthrough {
            out.extend_from_slice(&seq);
            return;
        }
    }

    // ② modifyOtherKeys mode 2：CSI 27（在 ctrl→C0 之前——mode 2 也编码这些键）
    if opts.modify_other_keys_state_2 {
        if let Some(cp) = single_codepoint(ev.text) {
            // darwin + option-as-alt=false ⇒ alt 不进 modcode（向量 mok2 ctrl+alt 钉死）
            let mut m = binding.0 as u8;
            if IS_DARWIN {
                m &= !MB_A; // option-as-alt ≡ false ⇒ alt 不进 modcode
            }
            let cpn = cp as u32;
            let no_shift = m & !0b0001;
            let should_modify =
                (0x40..=0x7f).contains(&cpn) || no_shift != 0 || cp == ' ';
            if should_modify {
                if let Some(i) = MOK_MODIFIERS.iter().position(|x| *x == m) {
                    let _ = write!(out, "\x1b[27;{};{}~", i + 2, cpn);
                    return;
                }
            }
        }
    }

    // ③ ctrl → C0（全部 mods 判定：只认「恰好 ctrl」）
    if let Some(c0) = ctrl_seq(ev.key, ev.text, all_mods) {
        if binding.alt() {
            out.push(0x1b);
        }
        out.push(c0);
        return;
    }

    // ④ 无文本：只剩 alt 前缀可能
    if ev.text.is_empty() {
        legacy_alt_prefix(out, binding, ev.text, opts);
        return;
    }

    // ⑤ fixterms CSI u（ctrl 组合；Kitty 变体：A-Z + shift 先转小写；unshifted ≡ 0 ⇒
    // shift 恒被剥除——「shift 用于取得字符则不报」的 fixterms 规则）
    if all_mods.ctrl() {
        if let Some(ch) = single_codepoint(ev.text) {
            let ch = if ev.mods.shift() && ch.is_ascii_uppercase() {
                ch.to_ascii_lowercase()
            } else {
                ch
            };
            // CsiUMods 位序 = shift1|alt2|ctrl4（与 wire 输入的 ctrl2/alt4 不同，重排；
            // 不含 super；shift 恒剥——unshifted ≡ 0 ⇒ 「shift 用于取得字符」恒成立）
            let mut bits = 0u16;
            if all_mods.alt() {
                bits |= 0b010;
            }
            if all_mods.ctrl() {
                bits |= 0b100;
            }
            let _ = write!(out, "\x1b[{};{}u", ch as u32, bits + 1);
            return;
        }
    }

    // ⑥ alt 前缀（文本形态）
    if legacy_alt_prefix(out, binding, ev.text, opts) {
        return;
    }

    // ⑦ darwin 上 super+键不产文本；其余直发
    if IS_DARWIN && all_mods.super_() {
        return;
    }
    out.extend_from_slice(ev.text.as_bytes());
}

// ---------------------------------------------------------------------------
// kitty 路径
// ---------------------------------------------------------------------------

/// kitty 序列组装态（`KittySequence`）。
struct KittySeq {
    key: u32,
    final_: u8,
    /// shift1|alt2|ctrl4|super8|caps64|num128，应答值 = +1。
    mods: u8,
    /// 0 = 不编 / 1 press / 2 repeat / 3 release。
    event: u8,
    alt0: Option<u32>,
    alt1: Option<u32>,
    text: String,
}

impl KittySeq {
    fn encode(&self, out: &mut Vec<u8>) {
        if self.final_ == b'u' || self.final_ == b'~' {
            self.encode_full(out);
        } else {
            self.encode_special(out);
        }
    }

    /// `u`/`~` 终止符族：press 不带 `:1`（ghostty 注记「Kitty 省 :1，我们也不带——
    /// u/~ 族按 omit 实现」；实测 enter/`27u` 均无）。
    fn encode_full(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(b"\x1b[");
        let _ = write!(out, "{}", self.key);
        if let Some(a0) = self.alt0 {
            let _ = write!(out, ":{a0}");
        }
        if let Some(a1) = self.alt1 {
            if self.alt0.is_none() {
                let _ = write!(out, "::{a1}"); // 空 shifted 槽的 `::base` 形态
            } else {
                let _ = write!(out, ":{a1}");
            }
        }
        let mods = self.mods as u32 + 1;
        let mut emit_prior = false;
        if self.event != 0 && self.event != 1 {
            let _ = write!(out, ";{mods}:{}", self.event);
            emit_prior = true;
        } else if mods > 1 {
            let _ = write!(out, ";{mods}");
            emit_prior = true;
        }
        // 文本段：控制码剔除；首码点前补 `;;`（无修饰段时）或单个 `;`
        let mut count = 0;
        for cp in self.text.chars() {
            let cpn = cp as u32;
            if is_control(cpn) {
                continue;
            }
            if count == 0 {
                if !emit_prior {
                    out.push(b';');
                }
                out.push(b';');
            } else {
                out.push(b':');
            }
            let _ = write!(out, "{cpn}");
            count += 1;
        }
        out.push(self.final_);
    }

    /// 字母终止符族（A/B/C/D/F/H/P/Q/S…）：report_events 开 ⇒ press 也带 `:1`
    /// （实测 arrow 族 `1;5:1D`）。
    fn encode_special(&self, out: &mut Vec<u8>) {
        let mods = self.mods as u32 + 1;
        if self.event != 0 {
            let _ = write!(out, "\x1b[1;{mods}:{}{}", self.event, self.final_ as char);
        } else if mods > 1 {
            let _ = write!(out, "\x1b[1;{mods}{}", self.final_ as char);
        } else {
            let _ = write!(out, "\x1b[{}", self.final_ as char);
        }
    }
}

fn kitty_encode(out: &mut Vec<u8>, ev: &KeyEvent, opts: &KeyOptions) {
    let flags = opts.kitty_flags;
    let report_events = flags & KF_REPORT_EVENTS != 0;
    let report_all = flags & KF_REPORT_ALL != 0;
    let report_alternates = flags & KF_REPORT_ALTERNATES != 0;
    let report_associated = flags & KF_REPORT_ASSOCIATED != 0;

    // release 门：未开 EVENT_TYPES 不报；enter/backspace/tab 还要 REPORT_ALL
    if ev.action == KeyAction::Release {
        if !report_events {
            return;
        }
        if !report_all && matches!(ev.key.0, 58 | 53 | 64) {
            return;
        }
    }

    let all_mods = ev.mods;
    let binding = all_mods.binding();

    // 表项：仅功能/预定义键（unshifted ≡ 0 ⇒ 无合成表项）
    let entry = KITTY_ENTRIES.iter().find(|e| e.0 == ev.key.0);

    // 预处理块（ghostty `preprocessing:` 标签的直译；`break 'preprocessing` = 跳过
    // 块内余下步骤直接进序列编码）
    'preprocessing: {
        if ev.composing {
            if let Some(e) = entry {
                if e.3 {
                    break 'preprocessing; // 纯修饰键放行
                }
            }
            return;
        }

        // enter/backspace 带文本：控制字符 ⇒ 继续走序列；backspace 其余无输出；
        // enter 直发文本（IME 确认形态）
        if !ev.text.is_empty() && matches!(ev.key.0, 58 | 53) {
            let control = ev.text.len() == 1 && is_control(ev.text.as_bytes()[0] as u32);
            if !control {
                if ev.key.0 == 53 {
                    return;
                }
                out.extend_from_slice(ev.text.as_bytes());
                return;
            }
        }

        if !report_all {
            // enter/tab/backspace 无绑定修饰 ⇒ 裸字节（崩溃后的 `reset` 可用性）
            if binding.is_empty() {
                match ev.key.0 {
                    58 => {
                        out.push(b'\r');
                        return;
                    }
                    64 => {
                        out.push(b'\t');
                        return;
                    }
                    53 => {
                        out.push(0x7f);
                        return;
                    }
                    _ => {}
                }
            }
            // 无绑定修饰的可打印单段文本直发（release 除外——release 走专用编码）
            if !ev.text.is_empty()
                && binding.is_empty()
                && ev.action != KeyAction::Release
                && ev.text.chars().all(|c| !is_control(c as u32))
            {
                out.extend_from_slice(ev.text.as_bytes());
                return;
            }
        }
    }

    let Some(e) = entry else {
        // 纯文本事件：release 不插字、空文本不产出，否则原样直发
        if ev.action == KeyAction::Release {
            return;
        }
        if ev.text.is_empty() {
            return;
        }
        out.extend_from_slice(ev.text.as_bytes());
        return;
    };

    // 纯修饰键需要 REPORT_ALL
    if e.3 && !report_all {
        return;
    }

    let mut seq = KittySeq {
        key: e.1,
        final_: e.2,
        mods: kitty_mods(all_mods),
        event: 0,
        alt0: None,
        alt1: None,
        text: String::new(),
    };
    if report_events {
        seq.event = match ev.action {
            KeyAction::Press => 1,
            KeyAction::Repeat => 2,
            KeyAction::Release => 3,
        };
    }

    if report_alternates && !is_control(seq.key) {
        let mut it = ev.text.chars();
        match it.next() {
            Some(cp1) => {
                let cp1 = cp1 as u32;
                if cp1 != seq.key && seq.mods & 1 != 0 {
                    seq.alt0 = Some(cp1);
                }
                let has_cp2 = it.next().is_some();
                if let Some(base) = key_codepoint(ev.key) {
                    let base = base as u32;
                    if base != seq.key && cp1 != base && !has_cp2 {
                        seq.alt1 = Some(base);
                    }
                }
            }
            None => {
                if let Some(base) = key_codepoint(ev.key) {
                    let base = base as u32;
                    if base != seq.key {
                        seq.alt1 = Some(base);
                    }
                }
            }
        }
    }

    if report_associated && seq.event != 3 {
        // darwin + option-as-alt=false ⇒ alt 不阻文本（D-10 darwin 口径）
        let alt_prevents_text = !IS_DARWIN;
        let prevents = (seq.mods & 0b10 != 0 && alt_prevents_text)
            || seq.mods & 0b100 != 0
            || seq.mods & 0b1000 != 0;
        if !prevents {
            seq.text = ev.text.to_string();
        }
    }

    seq.encode(out);
}

/// wire mods → kitty 修饰位（shift1|alt2|ctrl4|super8|caps64|num128）。
fn kitty_mods(mods: Mods) -> u8 {
    let mut m = 0u8;
    if mods.shift() {
        m |= 1;
    }
    if mods.alt() {
        m |= 2;
    }
    if mods.ctrl() {
        m |= 4;
    }
    if mods.super_() {
        m |= 8;
    }
    if mods.caps_lock() {
        m |= 64;
    }
    if mods.num_lock() {
        m |= 128;
    }
    m
}

// ---------------------------------------------------------------------------
// 鼠标编码
// ---------------------------------------------------------------------------

/// 鼠标动作（ghostty `mouse.Action` 同值：press=0/release=1/motion=2）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MouseAction {
    #[default]
    Press = 0,
    Release = 1,
    Motion = 2,
}

impl MouseAction {
    pub const fn from_wire(v: u8) -> Option<Self> {
        match v {
            0 => Some(MouseAction::Press),
            1 => Some(MouseAction::Release),
            2 => Some(MouseAction::Motion),
            _ => None,
        }
    }
}

/// 鼠标按钮（Go `MouseButton`/ghostty `Button` 同值：0=未知〔无按钮〕，
/// 1/2/3 = 左/右/中，4/5 = 滚轮上/下）。six..eleven 走 wire 值但 Go 输入面
/// 不产（`buttonCode` 的 66/67/128/129 分支保留对齐、实测不可达）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MouseButton(pub u8);

impl MouseButton {
    pub const UNKNOWN: MouseButton = MouseButton(0);
    pub const LEFT: MouseButton = MouseButton(1);
    pub const RIGHT: MouseButton = MouseButton(2);
    pub const MIDDLE: MouseButton = MouseButton(3);
    pub const FOUR: MouseButton = MouseButton(4);
    pub const FIVE: MouseButton = MouseButton(5);
}

/// 鼠标事件（X/Y = 网格坐标，0 起）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MouseEvent {
    pub action: MouseAction,
    pub button: MouseButton,
    pub mods: Mods,
    pub x: u16,
    pub y: u16,
}

/// 鼠标编码选项：B5 单值对 + 视口尺寸（网格坐标即像素坐标的 1×1 虚拟网格）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MouseOptions {
    pub tracking: MouseTracking,
    pub format: MouseFormat,
    pub cols: u16,
    pub rows: u16,
}

/// 鼠标编码（`mouse_encode.encode`；any_button_pressed 恒 false、同格不去重 D-11）。
pub fn encode_mouse(ev: &MouseEvent, opts: &MouseOptions) -> Vec<u8> {
    let mut out = Vec::with_capacity(16);
    if !should_report(ev, opts) {
        return out;
    }
    // 视口外（release 恒报；其余需 motion 模式且 any_button_pressed——Go 从不设该态
    // ⇒ 恒 false，视口外非 release 一律不报，实测 press_left_edge 222 钉死）
    if ev.action != MouseAction::Release && (ev.x > opts.cols || ev.y > opts.rows) {
        return out;
    }
    let cell_x = (ev.x as u32).min(opts.cols.saturating_sub(1) as u32);
    let cell_y = (ev.y as u32).min(opts.rows.saturating_sub(1) as u32);
    let Some(btn) = button_code(ev, opts) else {
        return out;
    };
    match opts.format {
        MouseFormat::Default => {
            // X10 三字节只能编到 223（cell ≤ 222）
            if cell_x > 222 || cell_y > 222 {
                return out;
            }
            out.extend_from_slice(b"\x1b[M");
            out.push(32 + btn);
            out.push(32 + cell_x as u8 + 1);
            out.push(32 + cell_y as u8 + 1);
        }
        MouseFormat::Utf8 => {
            out.extend_from_slice(b"\x1b[M");
            out.push(32 + btn);
            push_utf8_codepoint(&mut out, cell_x + 33);
            push_utf8_codepoint(&mut out, cell_y + 33);
        }
        MouseFormat::Sgr => {
            let final_ = if ev.action == MouseAction::Release { 'm' } else { 'M' };
            let _ = write!(out, "\x1b[<{btn};{};{}{final_}", cell_x + 1, cell_y + 1);
        }
        MouseFormat::Urxvt => {
            let _ = write!(out, "\x1b[{};{};{}M", 32 + btn as u32, cell_x + 1, cell_y + 1);
        }
    }
    out
}

/// 上报范围（`shouldReport`）。
fn should_report(ev: &MouseEvent, opts: &MouseOptions) -> bool {
    match opts.tracking {
        MouseTracking::None => false,
        MouseTracking::X10 => {
            ev.action == MouseAction::Press
                && matches!(ev.button, MouseButton::LEFT | MouseButton::MIDDLE | MouseButton::RIGHT)
        }
        MouseTracking::Clicks => ev.action != MouseAction::Motion,
        MouseTracking::CellMotion => ev.button != MouseButton::UNKNOWN,
        MouseTracking::AllMotion => true,
    }
}

/// 按钮码（`buttonCode`；legacy release 恒 3、null 按钮恒 3、修饰位/motion 位叠加）。
fn button_code(ev: &MouseEvent, opts: &MouseOptions) -> Option<u8> {
    let legacy_release = ev.action == MouseAction::Release
        && !matches!(opts.format, MouseFormat::Sgr);
    let mut acc: u8 = match ev.button {
        MouseButton::UNKNOWN => 3,
        _ if legacy_release => 3,
        MouseButton::LEFT => 0,
        MouseButton::MIDDLE => 1,
        MouseButton::RIGHT => 2,
        MouseButton::FOUR => 64,
        MouseButton::FIVE => 65,
        MouseButton(6) => 66,
        MouseButton(7) => 67,
        MouseButton(8) => 128,
        MouseButton(9) => 129,
        _ => return None, // ten/eleven 与超界值：不产出
    };
    if opts.tracking != MouseTracking::X10 {
        if ev.mods.shift() {
            acc += 4;
        }
        if ev.mods.alt() {
            acc += 8;
        }
        if ev.mods.ctrl() {
            acc += 16;
        }
    }
    if ev.action == MouseAction::Motion {
        acc += 32;
    }
    Some(acc)
}

fn push_utf8_codepoint(out: &mut Vec<u8>, cp: u32) {
    if let Some(c) = char::from_u32(cp) {
        let mut buf = [0u8; 4];
        out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
    }
}

// ---------------------------------------------------------------------------
// 焦点与粘贴
// ---------------------------------------------------------------------------

/// 焦点事件编码（`CSI I`/`CSI O`）。⚠️ 调用方必须先确认 DEC 1004 开（ghostty
/// 上游只编码不看模式——写进 PTY 前的模式门在调用侧，同 Go `EncodeFocus` 注记）。
pub fn encode_focus(gained: bool) -> Vec<u8> {
    if gained {
        b"\x1b[I".to_vec()
    } else {
        b"\x1b[O".to_vec()
    }
}

/// 括号粘贴分帧编码（`EncodePastePart`）：open = 首片、close = 末片；
/// 非括号模式忽略 open/close 原样写。空文本且无开闭 ⇒ None（Go 返回 nil）。
pub fn encode_paste_part(text: &[u8], bracketed: bool, open: bool, close: bool) -> Option<Vec<u8>> {
    if text.is_empty() && !open && !close {
        return None;
    }
    if !bracketed {
        return Some(text.to_vec());
    }
    let mut out = Vec::with_capacity(text.len() + 12);
    if open {
        out.extend_from_slice(b"\x1b[200~");
    }
    out.extend_from_slice(text);
    if close {
        out.extend_from_slice(b"\x1b[201~");
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    const KF_DISAMBIGUATE: u8 = 1;

    fn hex(s: &str) -> String {
        s.bytes().map(|b| format!("{b:02x}")).collect()
    }

    fn hx(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    fn unhex(s: &str) -> Vec<u8> {
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
    }

    fn load(path: &str) -> serde_json::Value {
        let base = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/vectors");
        serde_json::from_str(&std::fs::read_to_string(format!("{base}/{path}")).unwrap())
            .expect("向量文件合法")
    }

    /// 6c 判据：键编码 387 案逐字节对拍（喂 SessionVt 模式序列 → encode_key）。
    #[test]
    fn keyenc_parity_with_go_vectors() {
        use crate::term::vt::SessionVt;
        let v = load("term_keyenc.json");
        let cases = v.get("cases").unwrap().as_array().unwrap();
        assert!(cases.len() >= 387, "向量案数 {}", cases.len());
        let mut failures = Vec::new();
        let mut by_mode: std::collections::HashMap<String, usize> = Default::default();
        for c in cases {
            let mode = c.get("mode").unwrap().as_str().unwrap();
            let setup_hex = v.get("modes").unwrap().get(mode).unwrap().as_str().unwrap();
            let mut vt = SessionVt::new(100, 32, 1000).unwrap();
            let setup = unhex(setup_hex);
            if !setup.is_empty() {
                vt.write(&setup);
            }
            let e = c.get("event").unwrap();
            let key_name = e.get("key").unwrap().as_str().unwrap();
            let key = Key(v.get("keys").unwrap().get(key_name).unwrap().as_u64().unwrap() as u16);
            let ev = KeyEvent {
                key,
                action: KeyAction::from_wire(e.get("action").and_then(|x| x.as_u64()).unwrap_or(1) as u8).unwrap(),
                mods: Mods(e.get("mods").and_then(|x| x.as_u64()).unwrap_or(0) as u16),
                text: e.get("text").and_then(|x| x.as_str()).unwrap_or(""),
                composing: e.get("composing").and_then(|x| x.as_bool()).unwrap_or(false),
            };
            let got = vt.encode_key(&ev);
            let got_hex = got.iter().map(|b| format!("{b:02x}")).collect::<String>();
            let want = c.get("out_hex").and_then(|x| x.as_str()).unwrap_or("");
            if got_hex != want {
                failures.push(format!(
                    "{}[{}]: key={} act={} mods={} text={:?} expect {want} got {got_hex}",
                    mode,
                    c.get("name").and_then(|x| x.as_str()).unwrap_or("?"),
                    ev.key.0,
                    ev.action as u8,
                    ev.mods.0,
                    ev.text
                ));
            }
            *by_mode.entry(mode.to_string()).or_default() += 1;
        }
        assert!(
            failures.is_empty(),
            "键编码 parity 失败 {} 案（共 {}）:\n{}",
            failures.len(),
            cases.len(),
            failures.join("\n")
        );
        // 全模式都被喂过（防 setup 串错导致的静默全对）
        assert_eq!(by_mode.len(), v.get("modes").unwrap().as_object().unwrap().len());
    }

    /// 6c 判据：鼠标编码 160 案逐字节对拍（B5 单值语义 + 视口边界）。
    #[test]
    fn mouseenc_parity_with_go_vectors() {
        use crate::term::vt::SessionVt;
        let v = load("term_mouseenc.json");
        let cases = v.get("cases").unwrap().as_array().unwrap();
        assert!(cases.len() >= 160, "向量案数 {}", cases.len());
        let mut failures = Vec::new();
        for c in cases {
            let mode = c.get("mode").unwrap().as_str().unwrap();
            let setup_hex = v.get("modes").unwrap().get(mode).unwrap().as_str().unwrap();
            let mut vt = SessionVt::new(100, 32, 1000).unwrap();
            let setup = unhex(setup_hex);
            if !setup.is_empty() {
                vt.write(&setup);
            }
            let e = c.get("event").unwrap();
            let ev = MouseEvent {
                action: MouseAction::from_wire(e.get("action").and_then(|x| x.as_u64()).unwrap_or(0) as u8).unwrap(),
                button: MouseButton(e.get("button").and_then(|x| x.as_u64()).unwrap_or(0) as u8),
                mods: Mods(e.get("mods").and_then(|x| x.as_u64()).unwrap_or(0) as u16),
                x: e.get("x").and_then(|x| x.as_u64()).unwrap_or(0) as u16,
                y: e.get("y").and_then(|x| x.as_u64()).unwrap_or(0) as u16,
            };
            let got = vt.encode_mouse(&ev);
            let got_hex = got.iter().map(|b| format!("{b:02x}")).collect::<String>();
            let want = c.get("out_hex").and_then(|x| x.as_str()).unwrap_or("");
            if got_hex != want {
                failures.push(format!("{}[{}]: {:?}", mode, c.get("name").and_then(|x| x.as_str()).unwrap_or("?"), ev));
            }
        }
        assert!(failures.is_empty(), "鼠标 parity 失败 {} 案:\n{}", failures.len(), failures.join("\n"));
    }

    /// mok2 矩阵码抽查（向量之外的组合；对照 zig `modifiers` 序）。
    #[test]
    fn mok2_modifier_matrix_codes() {
        let opts = KeyOptions { modify_other_keys_state_2: true, ..KeyOptions::DEFAULT };
        // ctrl（第 4 项 ⇒ 码 5）
        let ev = KeyEvent { key: Key(20), action: KeyAction::Press, mods: Mods(2), text: "a", composing: false };
        assert_eq!(hx(&encode_key(&ev, &opts)), hex("\x1b[27;5;97~"));
        // shift+alt+ctrl+super（darwin 剥 alt ⇒ shift+ctrl+super 第 13 项码 14；
        // 非 darwin 全四修饰 = 第 15 项码 16）
        let ev = KeyEvent { key: Key(20), action: KeyAction::Press, mods: Mods(0b1111), text: "a", composing: false };
        if IS_DARWIN {
            assert_eq!(hx(&encode_key(&ev, &opts)), hex("\x1b[27;14;97~"));
        } else {
            assert_eq!(hx(&encode_key(&ev, &opts)), hex("\x1b[27;16;97~"));
        }
        // darwin：alt 位不进 modcode（ctrl+alt ⇒ 只叠 ctrl）
        let ev = KeyEvent { key: Key(20), action: KeyAction::Press, mods: Mods(0b0110), text: "a", composing: false };
        if IS_DARWIN {
            assert_eq!(hx(&encode_key(&ev, &opts)), hex("\x1b[27;5;97~"));
        }
    }

    /// DECBKM 翻转（backspace 的 sequence_decbkm）。
    #[test]
    fn backarrow_mode_flips_backspace() {
        let off = KeyOptions::DEFAULT;
        let on = KeyOptions { backarrow_key_mode: true, ..KeyOptions::DEFAULT };
        let ev = KeyEvent { key: Key(53), action: KeyAction::Press, mods: Mods::NONE, text: "\x7f", composing: false };
        assert_eq!(encode_key(&ev, &off), b"\x7f");
        assert_eq!(encode_key(&ev, &on), b"\x08");
        let ctrl = KeyEvent { key: Key(53), action: KeyAction::Press, mods: Mods(2), text: "", composing: false };
        assert_eq!(encode_key(&ctrl, &off), b"\x08");
        assert_eq!(encode_key(&ctrl, &on), b"\x7f");
    }

    /// 小键盘应用模式（DECKPAM + 1035 复位）——ghostty 语义：1035 默认 on 时
    /// keypad 恒数值模式。
    #[test]
    fn keypad_1035_gates_application_mode() {
        let deckpam = KeyOptions { keypad_key_application: true, ..KeyOptions::DEFAULT };
        let deckpam_1035_off = KeyOptions {
            keypad_key_application: true,
            ignore_keypad_with_numlock: false,
            ..KeyOptions::DEFAULT
        };
        let ev = KeyEvent { key: Key(81), action: KeyAction::Press, mods: Mods::NONE, text: "1", composing: false };
        // 1035 on（默认）⇒ 数值模式
        assert_eq!(encode_key(&ev, &deckpam), b"1");
        // 1035 off + DECKPAM ⇒ application 模式 SS3
        assert_eq!(encode_key(&ev, &deckpam_1035_off), b"\x1bOq");
        // 无文本的 numpad_enter：表匹配 `\x1bOM`（kpDefault）
        let ev = KeyEvent { key: Key(97), action: KeyAction::Press, mods: Mods::NONE, text: "", composing: false };
        assert_eq!(encode_key(&ev, &deckpam_1035_off), b"\x1bOM");
        assert_eq!(encode_key(&ev, &deckpam), b"\r");
    }

    /// kitty 的 `:1` 规则：字母终止符族 press 带 `:1`、u/~ 族不带（实测形态）。
    #[test]
    fn kitty_press_event_colon_one_rules() {
        let opts = KeyOptions { kitty_flags: KF_DISAMBIGUATE | KF_REPORT_EVENTS, ..KeyOptions::DEFAULT };
        let arrow = KeyEvent { key: Key(76), action: KeyAction::Press, mods: Mods::NONE, text: "", composing: false };
        assert_eq!(encode_key(&arrow, &opts), b"\x1b[1;1:1D");
        let f5 = KeyEvent { key: Key(125), action: KeyAction::Press, mods: Mods(2), text: "", composing: false };
        assert_eq!(encode_key(&f5, &opts), b"\x1b[15;5~");
        let enter = KeyEvent { key: Key(58), action: KeyAction::Press, mods: Mods(2), text: "\r", composing: false };
        assert_eq!(encode_key(&enter, &opts), b"\x1b[13;5u");
    }

    /// 焦点/粘贴编码面。
    #[test]
    fn focus_and_paste_encoding() {
        assert_eq!(encode_focus(true), b"\x1b[I");
        assert_eq!(encode_focus(false), b"\x1b[O");
        assert_eq!(encode_paste_part(b"hi", true, true, true).unwrap(), b"\x1b[200~hi\x1b[201~");
        assert_eq!(encode_paste_part(b"mid", true, false, false).unwrap(), b"mid");
        assert_eq!(encode_paste_part(b"raw", false, true, true).unwrap(), b"raw");
        assert!(encode_paste_part(b"", false, false, false).is_none());
    }

    /// utf8 鼠标格式大坐标（>95 与 X10 区分；E1 登记的判别力补钉）。
    #[test]
    fn mouse_utf8_large_coordinates() {
        let opts = MouseOptions {
            tracking: MouseTracking::AllMotion,
            format: MouseFormat::Utf8,
            cols: 400,
            rows: 400,
        };
        let ev = MouseEvent { action: MouseAction::Press, button: MouseButton::LEFT, mods: Mods::NONE, x: 300, y: 400 - 1 };
        let out = encode_mouse(&ev, &opts);
        // 300+33=333、399+33=432 的 UTF-8 编码
        let mut want = b"\x1b[M ".to_vec();
        let push = |cp: u32, w: &mut Vec<u8>| {
            let c = char::from_u32(cp).unwrap();
            let mut buf = [0u8; 4];
            w.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
        };
        push(333, &mut want);
        push(432, &mut want);
        assert_eq!(out, want);
    }
}
