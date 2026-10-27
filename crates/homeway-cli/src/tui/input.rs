use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use homeway_core::term::{
    frames::{self, InputEvent},
    keyenc,
};

fn modifiers(mods: KeyModifiers) -> keyenc::Mods {
    keyenc::Mods(
        u16::from(mods.contains(KeyModifiers::SHIFT))
            | (u16::from(mods.contains(KeyModifiers::CONTROL)) << 1)
            | (u16::from(mods.contains(KeyModifiers::ALT)) << 2)
            | (u16::from(mods.contains(KeyModifiers::SUPER)) << 3),
    )
}

pub(super) fn key(event: KeyEvent) -> Option<InputEvent> {
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
        _ => return None,
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
    Some(InputEvent::Key {
        key: code,
        action: action as u8,
        mods: mods.0,
        text,
    })
}

pub(super) fn mouse(event: MouseEvent) -> Option<InputEvent> {
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
        _ => return None,
    };
    Some(InputEvent::Mouse {
        action: action as u8,
        button: button.0,
        mods: modifiers(event.modifiers).0,
        x: event.column,
        y: event.row,
    })
}

/// Keep paste transaction boundaries while splitting only at valid UTF-8 boundaries.
pub(super) fn paste(text: &str) -> Vec<InputEvent> {
    if text.is_empty() {
        return Vec::new();
    }
    let mut chunks = Vec::new();
    let mut remaining = text;
    while !remaining.is_empty() {
        let mut end = remaining.len().min(8 << 10);
        while !remaining.is_char_boundary(end) {
            end -= 1;
        }
        chunks.push(&remaining[..end]);
        remaining = &remaining[end..];
    }
    let count = chunks.len();
    chunks
        .into_iter()
        .enumerate()
        .map(|(i, text)| InputEvent::Text {
            flags: frames::text_bits::PASTE
                | if i > 0 { frames::text_bits::CONT } else { 0 }
                | if i + 1 < count {
                    frames::text_bits::MORE
                } else {
                    0
                },
            text: text.into(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn abstract_keys_preserve_controls_unicode_and_event_kinds() {
        for (code, modifiers, want_key, want_text) in [
            (KeyCode::Char('c'), KeyModifiers::CONTROL, 22, "c"),
            (KeyCode::Tab, KeyModifiers::NONE, 64, ""),
            (KeyCode::Char('中'), KeyModifiers::NONE, 0, "中"),
            (KeyCode::Up, KeyModifiers::NONE, 78, ""),
        ] {
            let event = key(KeyEvent::new(code, modifiers)).unwrap();
            assert_eq!(
                event,
                InputEvent::Key {
                    key: want_key,
                    mods: self::modifiers(modifiers).0,
                    action: 1,
                    text: want_text.into()
                }
            );
            assert_eq!(
                frames::dec_input(&frames::enc_input(&event)).unwrap(),
                event
            );
        }
        assert!(matches!(
            key(KeyEvent::new_with_kind(
                KeyCode::Char('a'),
                KeyModifiers::NONE,
                KeyEventKind::Release
            )),
            Some(InputEvent::Key { action: 0, .. })
        ));
    }

    #[test]
    fn paste_chunks_are_utf8_and_form_one_transaction() {
        let text = "中🦀e\u{301}".repeat(4096);
        let chunks = paste(&text);
        let mut actual = String::new();
        for (i, event) in chunks.iter().enumerate() {
            let InputEvent::Text { flags, text } = event else {
                panic!("text")
            };
            assert_eq!(flags & frames::text_bits::CONT != 0, i > 0);
            assert_eq!(flags & frames::text_bits::MORE != 0, i + 1 < chunks.len());
            assert!(frames::enc_input(event).len() <= frames::DATA_CHUNK);
            actual.push_str(text);
        }
        assert_eq!(actual, text);
    }
}
