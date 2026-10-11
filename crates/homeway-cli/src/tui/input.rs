//! 输入通路（tasks 3.5）：crossterm 事件 → keyenc 编码 → DATA 字节帧。
//!
//! - 键：crossterm `KeyCode` → ghostty 键值表（真源 = `key_codepoint` 的翻译表
//!   及 fixtures/vectors/term_keyenc.json 的 keys 表——Key 是**物理键码**不是
//!   码点，'a'=20 而非 97）；表外可打印字符按码点直给（kitty CSI u 对未映射
//!   字符的协议语义即码点；legacy 路径有 text 走 text，等价正确）。

//! - 修饰：crossterm KeyModifiers → keyenc Mods（shift1/ctrl2/alt4/super8）。
//! - 粘贴/焦点：按**内嵌模拟器的会话模式位**编码（`SessionVt::modes` 的
//!   bracketed_paste/focus_events——它从字节流自行跟踪，spec 需求 4）；导航
//!   焦点期间的失焦/复得注入由 app 层调用 [`encode_focus`] 完成。
//! - RESIZE：仅交互实例在首次用户交互后下发（镜像实例 NEVER，r2 N-4）——
//!   交互与否是 app 层状态，本模块只提供编码。
//!
//! 鼠标编码随 4.4 的鼠标归属路由一起接（路由决定事件到不了编码器——先有路由
//! 后有编码）。消费方 = app 主循环（任务组 4）——期间豁免 dead_code。

#![allow(dead_code)]

use homeway_core::term::frames;
use homeway_core::term::vt::SessionVt;
use homeway_core::term::keyenc::{self, Key, KeyAction, KeyFlavor, KeyEvent, Mods};

/// crossterm 修饰位 → keyenc Mods（位序 = ghostty：shift1/ctrl2/alt4/super8）。
fn map_mods(m: crossterm::event::KeyModifiers) -> Mods {
    use crossterm::event::KeyModifiers as KM;
    let mut v = 0u16;
    if m.contains(KM::SHIFT) {
        v |= 1;
    }
    if m.contains(KM::CONTROL) {
        v |= 2;
    }
    if m.contains(KM::ALT) {
        v |= 4;
    }
    if m.contains(KM::SUPER) {
        v |= 8;
    }
    Mods(v)
}

/// 可打印字符 → ghostty 物理键码（大写回落到小写基键——shift 位由调用方加）。
fn key_of_char(c: char) -> Option<Key> {
    let base = c.to_ascii_lowercase();
    let code = match base {
        'a'..='z' => 20 + (base as u16 - 'a' as u16),
        '0'..='9' => 6 + (base as u16 - '0' as u16),
        '`' => 1,
        '\\' => 2,
        '[' => 3,
        ']' => 4,
        ',' => 5,
        '=' => 16,
        '-' => 46,
        '.' => 47,
        '\'' => 48,
        ';' => 49,
        '/' => 50,
        ' ' => 63,
        _ => return None,
    };
    Some(Key(code))
}

/// 功能键 → ghostty 键码（与 fixtures/vectors/term_keyenc.json 的 keys 表对齐）。
fn key_of_functional(code: &crossterm::event::KeyCode) -> Option<Key> {
    use crossterm::event::KeyCode as KC;
    Some(Key(match code {
        KC::Enter => 58,
        KC::Tab => 64,
        KC::Backspace => 53,
        KC::Esc => 120,
        KC::Left => 76,
        KC::Right => 77,
        KC::Up => 78,
        KC::Down => 75,
        KC::Home => 71,
        KC::End => 69,
        KC::PageUp => 74,
        KC::PageDown => 73,
        KC::Delete => 68,
        KC::Insert => 72,
        KC::F(n) if (1..=12).contains(n) => 120 + *n as u16, // f1=121..f12=132
        _ => return None,
    }))
}

/// 键事件编码（会话键编码口径 = 内嵌模拟器的模式快照，r2 N-3 定案）。
/// crossterm 事件 → keyenc `KeyEvent`（表外字符 = 码点直给；上屏文本按 ghostty
/// 语义：有 ctrl/alt/super 时清空——控制字节/前缀由编码器产出）。
/// 表外且不可打印的键返回 `None`（调用方丢弃——不产生垃圾字节）。
pub(crate) fn encode_key_event(ev: &crossterm::event::KeyEvent, vt: &SessionVt) -> Option<Vec<u8>> {
    use crossterm::event::KeyCode as KC;
    use crossterm::event::KeyEventKind as KKK;
    let mods = map_mods(ev.modifiers);
    let action = match ev.kind {
        KKK::Press => KeyAction::Press,
        KKK::Repeat => KeyAction::Repeat,
        KKK::Release => KeyAction::Release,
    };
    let (key, text): (Key, String) = match &ev.code {
        KC::Char(c) => {
            let base = key_of_char(*c).unwrap_or(Key(*c as u16));
            let shifted = c.is_uppercase();
            let m = if shifted { Mods(mods.0 | 1) } else { mods };
            let plain = m.0 & (2 | 4 | 8) == 0; // ctrl/alt/super 不带上屏文本
            let text = if plain { c.to_string() } else { String::new() };
            (base, text)
        }
        other => (key_of_functional(other)?, String::new()),
    };
    let kev = KeyEvent { key, action, mods, text: &text, composing: false };
    Some(vt.encode_key(&kev, KeyFlavor::host_default()))
}

/// 粘贴编码：按会话括号粘贴位（DEC 2004）决定包裹（spec 需求 4）。
/// **分片**（r4 中1）：帧载荷上限 64KiB（u16 len），超限粘贴按片拆分——
/// 首片带起始序列、末片带结束序列，中间片裸文本；每片独立成 DATA 帧。
pub(crate) fn encode_paste_fragments(text: &str, vt: &SessionVt) -> Option<Vec<Vec<u8>>> {
    const CHUNK: usize = 48 * 1024; // 留足括号序列与帧头余量（上限 65535）
    let bracketed = vt.modes().bracketed_paste;
    if !bracketed {
        // 未启用 2004：原样文本也要分片（超限文本同样会被 encode_frame 截断）。
        if text.is_empty() {
            return None;
        }
        return Some(
            text.as_bytes()
                .chunks(CHUNK)
                .map(|c| c.to_vec())
                .collect(),
        );
    }
    let mut out = Vec::new();
    let bytes = text.as_bytes();
    if bytes.is_empty() {
        return keyenc::encode_paste_part(b"", true, true, true).map(|b| vec![b]);
    }
    for (i, chunk) in bytes.chunks(CHUNK).enumerate() {
        let open = i == 0;
        let close = i == bytes.chunks(CHUNK).count() - 1;
        out.push(keyenc::encode_paste_part(chunk, true, open, close)?);
    }
    Some(out)
}

/// 焦点事件编码：按会话焦点上报位（DEC 1004）决定是否注入；未启用 ⇒ `None`
/// （不产生垃圾字节）。导航焦点期间的失焦/复得也走这里（app 层驱动）。
pub(crate) fn encode_focus(gained: bool, vt: &SessionVt) -> Option<Vec<u8>> {
    vt.modes().focus_events.then(|| keyenc::encode_focus(gained))
}

/// RESIZE 帧编码（仅交互实例在首次用户交互后由 app 层调用；镜像实例 NEVER）。
pub(crate) fn encode_resize(cols: u16, rows: u16) -> Vec<u8> {
    frames::enc_resize(cols, rows)
}

/// 鼠标事件编码（tasks 3.5/4.4b）：会话启用上报时由 runtime 调用（路由在前）。
/// 坐标折算到 pane 内（出口网格坐标即 pane 坐标——会话尺寸 = 对齐尺寸）。
pub(crate) fn encode_mouse_event(
    m: &crossterm::event::MouseEvent,
    pane: ratatui::layout::Rect,
    vt: &SessionVt,
) -> Option<Vec<u8>> {
    use homeway_core::term::keyenc::MouseAction;
    if m.column < pane.x || m.row < pane.y {
        return None;
    }
    let (x, y) = (m.column - pane.x, m.row - pane.y);
    let (action, button) = match m.kind {
        crossterm::event::MouseEventKind::Down(b) => (MouseAction::Press, cmouse_to_key(b)),
        crossterm::event::MouseEventKind::Up(b) => (MouseAction::Release, cmouse_to_key(b)),
        crossterm::event::MouseEventKind::Drag(b) => (MouseAction::Motion, cmouse_to_key(b)),
        crossterm::event::MouseEventKind::Moved => (MouseAction::Motion, homeway_core::term::keyenc::MouseButton::UNKNOWN),
        crossterm::event::MouseEventKind::ScrollUp => (MouseAction::Press, homeway_core::term::keyenc::MouseButton(4)),
        crossterm::event::MouseEventKind::ScrollDown => (MouseAction::Press, homeway_core::term::keyenc::MouseButton(5)),
        _ => return None,
    };
    let mods = map_mods(m.modifiers);
    Some(vt.encode_mouse(&homeway_core::term::keyenc::MouseEvent { action, button, mods, x, y }))
}

fn cmouse_to_key(b: crossterm::event::MouseButton) -> homeway_core::term::keyenc::MouseButton {
    use homeway_core::term::keyenc::MouseButton as MB;
    match b {
        crossterm::event::MouseButton::Left => MB::LEFT,
        crossterm::event::MouseButton::Right => MB::RIGHT,
        crossterm::event::MouseButton::Middle => MB(3),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vt() -> SessionVt {
        SessionVt::new(80, 24, 1000).expect("vt")
    }

    fn key(code: crossterm::event::KeyCode, mods: crossterm::event::KeyModifiers) -> crossterm::event::KeyEvent {
        crossterm::event::KeyEvent::new(code, mods)
    }

    #[test]
    fn plain_char_encode_as_text() {
        let v = vt();
        let out = encode_key_event(&key(crossterm::event::KeyCode::Char('a'), crossterm::event::KeyModifiers::NONE), &v)
            .expect("可编码");
        assert_eq!(out, b"a");
        let out = encode_key_event(&key(crossterm::event::KeyCode::Char('A'), crossterm::event::KeyModifiers::SHIFT), &v)
            .expect("可编码");
        assert_eq!(out, b"A");
    }

    #[test]
    fn ctrl_c_maps_to_c0() {
        let v = vt();
        let out = encode_key_event(&key(crossterm::event::KeyCode::Char('c'), crossterm::event::KeyModifiers::CONTROL), &v)
            .expect("可编码");
        assert_eq!(out, vec![0x03], "Ctrl+C = ETX（ghostty ctrlSeq 表）");
    }

    #[test]
    fn enter_and_arrow_legacy_sequences() {
        let v = vt();
        let enter = encode_key_event(&key(crossterm::event::KeyCode::Enter, crossterm::event::KeyModifiers::NONE), &v)
            .expect("可编码");
        assert_eq!(enter, b"\r", "Enter legacy = CR");
        let up = encode_key_event(&key(crossterm::event::KeyCode::Up, crossterm::event::KeyModifiers::NONE), &v)
            .expect("可编码");
        assert_eq!(up, b"\x1b[A", "DECCKM 关 ⇒ CSI A（光标键普通模式）");
    }

    #[test]
    fn kitty_mode_switches_encoding() {
        let mut v = vt();
        // 内层程序启用 kitty 消歧（flags=1）——模拟器从字节流自行跟踪；Escape
        // 是消歧档的旗舰用例（legacy = 裸 ESC 0x1b，消歧 = CSI 27u）。
        v.write(b"\x1b[>1u");
        assert_ne!(v.key_options().kitty_flags, 0, "kitty 协议已激活");
        let out = encode_key_event(&key(crossterm::event::KeyCode::Esc, crossterm::event::KeyModifiers::NONE), &v)
            .expect("可编码");
        assert_eq!(out, b"\x1b[27u".to_vec().as_slice(), "消歧档：Escape = CSI 27u");
        // 纯文本字符在消歧档仍走原文本（keyenc 语义：flags=1 不报全键）。
        let a = encode_key_event(&key(crossterm::event::KeyCode::Char('a'), crossterm::event::KeyModifiers::NONE), &v)
            .expect("可编码");
        assert_eq!(a, b"a");
    }

    #[test]
    fn paste_wraps_only_when_session_enables_2004() {
        let mut v = vt();
        let raw = "ls 発";
        let plain = encode_paste_fragments(raw, &v).expect("未启用也有原文本");
        assert_eq!(plain, vec![raw.as_bytes().to_vec()], "2004 关 = 原样文本（单片）");
        v.write(b"\x1b[?2004h");
        let frags = encode_paste_fragments(raw, &v).expect("启用后包裹");
        assert_eq!(frags.len(), 1, "短文本单片");
        let wrapped = &frags[0];
        assert!(wrapped.starts_with(b"\x1b[200~"), "括号粘贴起始序列");
        assert!(wrapped.ends_with(b"\x1b[201~"), "括号粘贴结束序列");
        assert!(wrapped.windows(raw.len()).any(|w| w == raw.as_bytes()), "正文完整保留");
    }

    #[test]
    fn paste_long_text_fragments_with_bracket_pairs() {
        // r4 中1：超限文本分片——首片带起始、末片带结束、单片 ≤48KiB、正文无损。
        let mut v = vt();
        v.write(b"\x1b[?2004h");
        let big = "x".repeat(120 * 1024);
        let frags = encode_paste_fragments(&big, &v).expect("分片");
        assert!(frags.len() >= 3, "120KiB ⇒ 至少 3 片：{}", frags.len());
        assert!(frags[0].starts_with(b"\x1b[200~"), "首片带起始");
        assert!(frags.last().unwrap().ends_with(b"\x1b[201~"), "末片带结束");
        for (i, f) in frags.iter().enumerate() {
            assert!(f.len() <= 64 * 1024, "片 {i} 超帧上限");
            if i > 0 {
                assert!(!f.starts_with(b"\x1b[200~"), "中间片不带起始序列");
            }
            if i + 1 < frags.len() {
                assert!(!f.ends_with(b"\x1b[201~"), "非末片不带结束序列");
            }
        }
        // 正文无损：内容全为 'x'（0x78 不出现在包裹序列里）⇒ 按字节计数。
        let xcount: usize = frags.iter().map(|f| f.iter().filter(|&&b| b == b'x').count()).sum();
        assert_eq!(xcount, big.len(), "正文无损");
    }

    #[test]
    fn focus_gated_by_session_1004() {
        let mut v = vt();
        assert_eq!(encode_focus(true, &v), None, "会话未启用 1004 ⇒ 不注入");
        v.write(b"\x1b[?1004h");
        assert_eq!(encode_focus(true, &v).as_deref(), Some(b"\x1b[I".as_slice()), "获得焦点");
        assert_eq!(encode_focus(false, &v).as_deref(), Some(b"\x1b[O".as_slice()), "失去焦点");
    }

    #[test]
    fn resize_frame_roundtrip() {
        let f = encode_resize(120, 40);
        assert_eq!(f.len(), 4, "RESIZE = cols u16 + rows u16");
    }
}
