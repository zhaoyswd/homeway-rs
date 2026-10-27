use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use homeway_core::term::{keyenc, vt::SessionVt};

fn modifiers(mods: KeyModifiers) -> keyenc::Mods {
    keyenc::Mods(
        u16::from(mods.contains(KeyModifiers::SHIFT))
            | (u16::from(mods.contains(KeyModifiers::CONTROL)) << 1)
            | (u16::from(mods.contains(KeyModifiers::ALT)) << 2)
            | (u16::from(mods.contains(KeyModifiers::SUPER)) << 3),
    )
}

/// 兼容只有ATTACHED legacy位的服务。先应用于空VT，回放与新服务权威检查点随后覆盖。
/// ALT_SCREEN由原raw回放的屏幕前序与历史负责，此处绝不重开/清屏。
pub(super) fn attached_modes(vt: &mut SessionVt, bits: u32) {
    use homeway_core::term::codec::mode_bits as m;
    use std::fmt::Write as _;
    let mut sequence = String::new();
    for (number, flag) in [(1, m::DECCKM), (1004, m::FOCUS), (2004, m::BRACKETED)] {
        let _ = write!(
            sequence,
            "\x1b[?{number}{}",
            if bits & flag != 0 { 'h' } else { 'l' }
        );
    }
    // reset其它mouse模式也会清last-set值，所以必须先全部reset，再只重放启用项。
    sequence.push_str("\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l");
    for (number, flag) in [
        (1000, m::MOUSE_1000),
        (1002, m::MOUSE_1002),
        (1003, m::MOUSE_1003),
        (1006, m::MOUSE_1006),
    ] {
        if bits & flag != 0 {
            let _ = write!(sequence, "\x1b[?{number}h");
        }
    }
    vt.write(sequence.as_bytes());
}

pub(super) fn key(vt: &SessionVt, event: KeyEvent) -> Vec<u8> {
    let mut text = String::new();
    let code = match event.code {
        KeyCode::Char(c) => {
            text.push(c);
            match c.to_ascii_lowercase() {
                'a'..='z' => 20 + c.to_ascii_lowercase() as u16 - 'a' as u16,
                '0'..='9' => 6 + c as u16 - '0' as u16,
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
                _ => 0,
            }
        }
        KeyCode::Enter => 58,
        KeyCode::Esc => 120,
        KeyCode::Backspace => 53,
        KeyCode::Tab | KeyCode::BackTab => 64,
        KeyCode::Left => 76,
        KeyCode::Right => 77,
        KeyCode::Up => 78,
        KeyCode::Down => 75,
        KeyCode::Home => 71,
        KeyCode::End => 69,
        KeyCode::PageUp => 74,
        KeyCode::PageDown => 73,
        KeyCode::Insert => 72,
        KeyCode::Delete => 68,
        KeyCode::F(n @ 1..=25) => 120 + u16::from(n),
        _ => return Vec::new(),
    };
    let mut mods = modifiers(event.modifiers);
    if event.code == KeyCode::BackTab {
        mods.0 |= 1;
    }
    let action = match event.kind {
        KeyEventKind::Press => keyenc::KeyAction::Press,
        KeyEventKind::Repeat => keyenc::KeyAction::Repeat,
        KeyEventKind::Release => keyenc::KeyAction::Release,
    };
    // 外层终端已把 Option/Alt 组合解释成字符；ASCII Alt 前缀须显式保留。
    vt.encode_key(
        &keyenc::KeyEvent {
            key: keyenc::Key(code),
            action,
            mods,
            text: &text,
            composing: false,
        },
        keyenc::KeyFlavor::AltEscPrefix,
    )
}

pub(super) fn mouse(vt: &SessionVt, event: MouseEvent) -> Vec<u8> {
    let button = |b| match b {
        MouseButton::Left => keyenc::MouseButton::LEFT,
        MouseButton::Right => keyenc::MouseButton::RIGHT,
        MouseButton::Middle => keyenc::MouseButton::MIDDLE,
    };
    let (action, button) = match event.kind {
        MouseEventKind::Down(b) => (keyenc::MouseAction::Press, button(b)),
        MouseEventKind::Up(b) => (keyenc::MouseAction::Release, button(b)),
        MouseEventKind::Drag(b) => (keyenc::MouseAction::Motion, button(b)),
        MouseEventKind::Moved => (keyenc::MouseAction::Motion, keyenc::MouseButton::UNKNOWN),
        MouseEventKind::ScrollUp => (keyenc::MouseAction::Press, keyenc::MouseButton::FOUR),
        MouseEventKind::ScrollDown => (keyenc::MouseAction::Press, keyenc::MouseButton::FIVE),
        _ => return Vec::new(),
    };
    vt.encode_mouse(&keyenc::MouseEvent {
        action,
        button,
        mods: modifiers(event.modifiers),
        x: event.column,
        y: event.row,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_attached_modes_restore_mouse_and_paste_without_switching_screen() {
        use homeway_core::term::codec::mode_bits as m;
        let mut vt = SessionVt::new(80, 24, 0).unwrap();
        vt.write(b"\x1b[?1049hKEEP");
        attached_modes(
            &mut vt,
            m::MOUSE_1000 | m::MOUSE_1006 | m::BRACKETED | m::DECCKM,
        );
        assert!(vt.modes().bracketed_paste && vt.modes().cursor_keys_app);
        assert_eq!(vt.modes().screen, homeway_core::term::vt::Screen::Alternate);
        assert!(vt.screen_text().contains("KEEP"));
        assert!(!mouse(
            &vt,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 0,
                row: 0,
                modifiers: KeyModifiers::NONE
            }
        )
        .is_empty());
    }
    #[test]
    fn keys_use_session_modes_and_preserve_controls_and_unicode() {
        let mut vt = SessionVt::new(80, 24, 0).unwrap();
        assert_eq!(
            key(
                &vt,
                KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)
            ),
            b"\x03"
        );
        assert_eq!(
            key(&vt, KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE)),
            b"\t"
        );
        assert_eq!(
            key(&vt, KeyEvent::new(KeyCode::Char('中'), KeyModifiers::NONE)),
            "中".as_bytes()
        );
        assert_eq!(
            key(&vt, KeyEvent::new(KeyCode::Up, KeyModifiers::NONE)),
            b"\x1b[A"
        );
        vt.write(b"\x1b[?1h");
        assert_eq!(
            key(&vt, KeyEvent::new(KeyCode::Up, KeyModifiers::NONE)),
            b"\x1bOA"
        );
    }
}
