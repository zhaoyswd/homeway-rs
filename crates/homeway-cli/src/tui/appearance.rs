//! Terminal background detection through the same event reader used for keyboard input.
//!
//! Crossterm 0.29 exposes OSC replies as `Alt+]`, payload keys and BEL/`Alt+\\`.
//! Keep the reply out of both management input and the attached PTY. There is no second
//! reader competing for stdin, and a nonmatching prefix is returned unchanged.
use std::time::{Duration, Instant};

use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use super::view::Theme;

pub(super) const QUERY: &[u8] = b"\x1b]11;?\x1b\\";
const QUERY_INTERVAL: Duration = Duration::from_secs(5);
const PREFIX_WAIT: Duration = Duration::from_millis(100);
const REPLY_WAIT: Duration = Duration::from_millis(500);
const MAX_REPLY_EVENTS: usize = 128;

pub(super) struct Appearance {
    theme: Theme,
    next_query: Instant,
    pending: Vec<Event>,
    payload: String,
    started: Option<Instant>,
    reply: bool,
    probe_disabled: bool,
    escape: bool,
}

impl Appearance {
    pub(super) fn new(now: Instant, colorfgbg: Option<&str>) -> Self {
        Self {
            theme: colorfgbg.and_then(hint).unwrap_or_default(),
            next_query: now,
            pending: Vec::new(),
            payload: String::new(),
            started: None,
            reply: false,
            probe_disabled: false,
            escape: false,
        }
    }

    pub(super) fn theme(&self) -> Theme {
        self.theme
    }

    pub(super) fn query_due(&mut self, now: Instant) -> bool {
        if self.probe_disabled || self.started.is_some() || now < self.next_query {
            return false;
        }
        self.next_query = now + QUERY_INTERVAL;
        true
    }

    pub(super) fn focus_gained(&mut self, now: Instant) {
        if !self.probe_disabled {
            self.next_query = now;
        }
    }

    /// Only a complete, valid, terminated report is consumed. An incomplete or
    /// nonmatching candidate is returned as the original events, never guessed away.
    /// After an invalid report, disable probes for this run to protect ordinary input.
    pub(super) fn expire(&mut self, now: Instant) -> Vec<Event> {
        if self.started.is_none_or(|start| {
            now.duration_since(start) < if self.reply { REPLY_WAIT } else { PREFIX_WAIT }
        }) {
            return Vec::new();
        }
        if self.reply {
            self.probe_disabled = true;
        }
        self.finish(false)
    }

    pub(super) fn filter(&mut self, event: Event, now: Instant) -> Vec<Event> {
        let mut ready = self.expire(now);
        if self.started.is_none() {
            if osc_start(&event) {
                self.started = Some(now);
                self.pending.push(event);
            } else {
                ready.push(event);
            }
            return ready;
        }
        if self.reply && (osc_end(&event) || (self.escape && is_backslash(&event))) {
            self.pending.push(event);
            if let Some(theme) = background(&self.payload[3..]) {
                self.theme = theme;
                ready.extend(self.finish(true));
            } else {
                self.probe_disabled = true;
                ready.extend(self.finish(false));
            }
            return ready;
        }
        if self.reply && is_escape(&event) {
            self.escape = true;
            self.pending.push(event);
            return ready;
        }
        if let Some(character) = report_char(&event) {
            if self.escape {
                // A split ESC followed by an ordinary key is keyboard input.
                ready.extend(self.finish(false));
                ready.push(event);
                self.probe_disabled = true;
                return ready;
            }
            self.payload.push(character);
            self.pending.push(event);
            if !self.reply {
                if self.payload == "11;" {
                    self.reply = true;
                } else if !"11;".starts_with(&self.payload) {
                    ready.extend(self.finish(false));
                }
            } else if self.pending.len() >= MAX_REPLY_EVENTS {
                self.probe_disabled = true;
                ready.extend(self.finish(false));
            }
        } else {
            // Preserve ordering for unrelated keys, mouse, resize, focus or paste.
            // Their presence makes this candidate ambiguous rather than a report.
            if self.reply {
                self.probe_disabled = true;
            }
            ready.extend(self.finish(false));
            ready.push(event);
        }
        ready
    }

    fn finish(&mut self, consumed: bool) -> Vec<Event> {
        let ready = if consumed {
            self.pending.clear();
            Vec::new()
        } else {
            std::mem::take(&mut self.pending)
        };
        self.payload.clear();
        self.started = None;
        self.reply = false;
        self.escape = false;
        ready
    }
}

fn is_escape(event: &Event) -> bool {
    matches!(
        event,
        Event::Key(KeyEvent {
            code: KeyCode::Esc,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            ..
        })
    )
}

fn is_backslash(event: &Event) -> bool {
    matches!(
        event,
        Event::Key(KeyEvent {
            code: KeyCode::Char('\\'),
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            ..
        })
    )
}

fn osc_start(event: &Event) -> bool {
    matches!(
        event,
        Event::Key(KeyEvent {
            code: KeyCode::Char(']'),
            modifiers: KeyModifiers::ALT,
            kind: KeyEventKind::Press,
            ..
        })
    )
}

fn osc_end(event: &Event) -> bool {
    matches!(
        event,
        Event::Key(KeyEvent {
            code: KeyCode::Char('g'),
            modifiers: KeyModifiers::CONTROL,
            kind: KeyEventKind::Press,
            ..
        }) | Event::Key(KeyEvent {
            code: KeyCode::Char('\\'),
            modifiers: KeyModifiers::ALT,
            kind: KeyEventKind::Press,
            ..
        })
    )
}

fn report_char(event: &Event) -> Option<char> {
    match event {
        Event::Key(KeyEvent {
            code: KeyCode::Char(character),
            modifiers,
            kind: KeyEventKind::Press,
            ..
        }) if character.is_ascii_graphic()
            && (*modifiers == KeyModifiers::NONE || *modifiers == KeyModifiers::SHIFT) =>
        {
            Some(*character)
        }
        _ => None,
    }
}

fn hint(value: &str) -> Option<Theme> {
    match value.rsplit(';').next()?.parse::<u8>().ok()? {
        0..=6 | 8 => Some(Theme::Dark),
        7 | 9..=15 => Some(Theme::Light),
        _ => None,
    }
}

fn background(value: &str) -> Option<Theme> {
    let rgb = if let Some(rgb) = value.strip_prefix("rgb:") {
        let mut channels = rgb.split('/');
        let channels_rgb = [
            channel(channels.next()?)?,
            channel(channels.next()?)?,
            channel(channels.next()?)?,
        ];
        if channels.next().is_some() {
            return None;
        }
        channels_rgb
    } else {
        let hex = value.strip_prefix('#')?;
        if !hex.is_ascii() || !matches!(hex.len(), 3 | 6 | 9 | 12) {
            return None;
        }
        let len = hex.len() / 3;
        [
            channel(&hex[..len])?,
            channel(&hex[len..len * 2])?,
            channel(&hex[len * 2..])?,
        ]
    };
    let linear = rgb.map(|value| {
        if value <= 0.04045 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    });
    let luminance = 0.2126 * linear[0] + 0.7152 * linear[1] + 0.0722 * linear[2];
    Some(if luminance > 0.179 {
        Theme::Light
    } else {
        Theme::Dark
    })
}

fn channel(hex: &str) -> Option<f64> {
    if !(1..=4).contains(&hex.len()) || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let maximum = (1u32 << (hex.len() * 4)) - 1;
    Some(f64::from(u16::from_str_radix(hex, 16).ok()?) / f64::from(maximum))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(character: char) -> Event {
        Event::Key(KeyEvent::new(
            KeyCode::Char(character),
            if character.is_ascii_uppercase() {
                KeyModifiers::SHIFT
            } else {
                KeyModifiers::NONE
            },
        ))
    }

    fn report(appearance: &mut Appearance, value: &str, bel: bool, now: Instant) -> Vec<Event> {
        let mut result = appearance.filter(
            Event::Key(KeyEvent::new(KeyCode::Char(']'), KeyModifiers::ALT)),
            now,
        );
        for c in format!("11;{value}").chars() {
            result.extend(appearance.filter(key(c), now));
        }
        result.extend(appearance.filter(
            Event::Key(KeyEvent::new(
                KeyCode::Char(if bel { 'g' } else { '\\' }),
                if bel {
                    KeyModifiers::CONTROL
                } else {
                    KeyModifiers::ALT
                },
            )),
            now,
        ));
        result
    }

    #[test]
    fn osc11_bel_st_and_common_rgb_depths() {
        let now = Instant::now();
        let mut appearance = Appearance::new(now, Some("0;15"));
        assert_eq!(appearance.theme(), Theme::Light);
        for value in ["rgb:00/00/00", "rgb:0000/0000/0000", "#000"] {
            assert!(report(&mut appearance, value, true, now).is_empty());
            assert_eq!(appearance.theme(), Theme::Dark);
        }
        for value in [
            "rgb:FF/FF/FF",
            "rgb:ffff/ffff/ffff",
            "#ffffff",
            "#ffffffffffff",
        ] {
            assert!(report(&mut appearance, value, false, now).is_empty());
            assert_eq!(appearance.theme(), Theme::Light);
        }
        assert_eq!(background("rgb:zz/00/ff"), None);
        assert_eq!(background("rgb:ff/ff/ff/ff"), None);
    }

    #[test]
    fn ordinary_events_paste_and_unmatched_prefix_are_preserved() {
        let now = Instant::now();
        let mut appearance = Appearance::new(now, None);
        let first = Event::Key(KeyEvent::new(KeyCode::Char(']'), KeyModifiers::ALT));
        assert!(appearance.filter(first.clone(), now).is_empty());
        assert_eq!(
            appearance.filter(key('x'), now),
            vec![first.clone(), key('x')]
        );
        assert_eq!(appearance.filter(key('n'), now), vec![key('n')]);
        let paste = Event::Paste("\x1b]11;rgb:ff/ff/ff\x07".into());
        assert_eq!(appearance.filter(paste.clone(), now), vec![paste]);
        assert_eq!(appearance.theme(), Theme::Dark);
        assert!(appearance.filter(first.clone(), now).is_empty());
        assert_eq!(appearance.expire(now + PREFIX_WAIT), vec![first]);
    }

    #[test]
    fn fragmented_late_reply_and_adjacent_keys() {
        let now = Instant::now();
        let mut appearance = Appearance::new(now, None);
        assert_eq!(appearance.filter(key('q'), now), vec![key('q')]);
        assert!(appearance
            .filter(
                Event::Key(KeyEvent::new(KeyCode::Char(']'), KeyModifiers::ALT)),
                now
            )
            .is_empty());
        for c in "11;rgb:ffff/ffff/ffff".chars() {
            assert!(appearance
                .filter(key(c), now + Duration::from_millis(10))
                .is_empty());
        }
        assert!(appearance
            .filter(
                Event::Key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL)),
                now + Duration::from_millis(300)
            )
            .is_empty());
        assert_eq!(appearance.theme(), Theme::Light);
        assert_eq!(appearance.filter(key('z'), now), vec![key('z')]);
        assert!(report(
            &mut appearance,
            "rgb:00/00/00",
            false,
            now + Duration::from_secs(50)
        )
        .is_empty());
        assert_eq!(appearance.theme(), Theme::Dark);
    }

    #[test]
    fn unsupported_query_never_blocks_keys_and_detection_is_bounded() {
        let now = Instant::now();
        let mut appearance = Appearance::new(now, Some("invalid"));
        assert!(appearance.query_due(now));
        assert!(!appearance.query_due(now));
        assert_eq!(appearance.filter(key('a'), now), vec![key('a')]);
        assert!(appearance.query_due(now + QUERY_INTERVAL));
        appearance.filter(
            Event::Key(KeyEvent::new(KeyCode::Char(']'), KeyModifiers::ALT)),
            now,
        );
        for c in "11;rgb:fff".chars() {
            appearance.filter(key(c), now);
        }
        let returned = appearance.expire(now + REPLY_WAIT);
        assert_eq!(returned.len(), "11;rgb:fff".len() + 1);
        assert!(appearance.pending.is_empty());
        assert_eq!(
            appearance.filter(key('b'), now + REPLY_WAIT),
            vec![key('b')]
        );
        assert!(!appearance.query_due(now + Duration::from_secs(100)));
    }

    #[test]
    fn bad_or_truncated_reports_return_every_original_event_and_stop_probes() {
        let now = Instant::now();
        for ending in [true, false] {
            let mut appearance = Appearance::new(now, None);
            let returned = report(&mut appearance, "rgb:not/a/color", ending, now);
            assert_eq!(returned.len(), 1 + "11;rgb:not/a/color".len() + 1);
            assert_eq!(appearance.theme(), Theme::Dark);
            assert!(!appearance.query_due(now + QUERY_INTERVAL));
        }
        let mut appearance = Appearance::new(now, None);
        let mut original = vec![Event::Key(KeyEvent::new(
            KeyCode::Char(']'),
            KeyModifiers::ALT,
        ))];
        original.extend("11;rgb:fff".chars().map(key));
        for event in &original {
            assert!(appearance.filter(event.clone(), now).is_empty());
        }
        assert_eq!(appearance.expire(now + REPLY_WAIT), original);
        // In particular b/abc must not be guessed to be a late hexadecimal tail.
        for c in "abc".chars() {
            assert_eq!(appearance.filter(key(c), now + REPLY_WAIT), vec![key(c)]);
        }
        assert!(report(
            &mut appearance,
            "rgb:ffff/ffff/ffff",
            true,
            now + Duration::from_secs(20)
        )
        .is_empty());
        assert_eq!(
            appearance.theme(),
            Theme::Light,
            "a late complete new report is still recognized"
        );
    }

    #[test]
    fn split_st_and_nonmatching_paste_resize_preserve_event_order() {
        let now = Instant::now();
        let mut appearance = Appearance::new(now, None);
        appearance.filter(
            Event::Key(KeyEvent::new(KeyCode::Char(']'), KeyModifiers::ALT)),
            now,
        );
        for c in "11;#ffffff".chars() {
            appearance.filter(key(c), now);
        }
        assert!(appearance
            .filter(
                Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
                now
            )
            .is_empty());
        assert!(appearance.filter(key('\\'), now).is_empty());
        assert_eq!(appearance.theme(), Theme::Light);
        for next in [
            Event::Resize(100, 40),
            Event::Paste("literal \x1b]11;#000\x07".into()),
        ] {
            let first = Event::Key(KeyEvent::new(KeyCode::Char(']'), KeyModifiers::ALT));
            assert!(appearance.filter(first.clone(), now).is_empty());
            assert_eq!(appearance.filter(next.clone(), now), vec![first, next]);
        }
    }
}
