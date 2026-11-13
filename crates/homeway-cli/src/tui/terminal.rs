use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};

use crossterm::{cursor, event, execute, terminal};

static STOP: AtomicBool = AtomicBool::new(false);

/// Keep Ratatui's destructor from panicking while reporting a revoked terminal.
/// The normal writer always propagates errors; only final cursor cleanup may
/// replace an unusable output with a sink, after rendering has ended.
pub(super) struct RenderTerminal(
    ratatui::Terminal<ratatui::backend::CrosstermBackend<Output>>,
);

pub(super) enum Output {
    Active(io::Stdout),
    Closed,
}

impl Write for Output {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        match self {
            Self::Active(output) => output.write(bytes),
            Self::Closed => Ok(bytes.len()),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Self::Active(output) => output.flush(),
            Self::Closed => Ok(()),
        }
    }
}

impl RenderTerminal {
    pub(super) fn new(output: io::Stdout) -> io::Result<Self> {
        ratatui::Terminal::new(ratatui::backend::CrosstermBackend::new(Output::Active(output)))
            .map(Self)
    }
}

impl std::ops::Deref for RenderTerminal {
    type Target = ratatui::Terminal<ratatui::backend::CrosstermBackend<Output>>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::ops::DerefMut for RenderTerminal {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl Drop for RenderTerminal {
    fn drop(&mut self) {
        if self.0.show_cursor().is_err() {
            // Ratatui 0.30 reports cursor restoration failure with eprintln! in
            // Drop. If stderr was revoked too, that diagnostic itself panics.
            // Clear its hidden-cursor state through the normal API against a
            // destruction-only sink; TerminalGuard still restores raw/alt mode.
            *self.0.backend_mut() = ratatui::backend::CrosstermBackend::new(Output::Closed);
            let _ = self.0.show_cursor();
        }
    }
}

pub(super) struct TerminalGuard {
    previous: Vec<(i32, libc::sigaction)>,
}

extern "C" fn stop(_: libc::c_int) {
    STOP.store(true, Ordering::Relaxed);
}

pub(super) fn stopping() -> bool {
    STOP.load(Ordering::Relaxed)
}

pub(super) fn restore() {
    let _ = terminal::disable_raw_mode();
    let _ = execute!(
        io::stdout(),
        event::DisableBracketedPaste,
        event::DisableFocusChange,
        event::DisableMouseCapture,
        event::PopKeyboardEnhancementFlags,
        cursor::Show,
        terminal::LeaveAlternateScreen
    );
}

impl TerminalGuard {
    pub(super) fn enter() -> io::Result<Self> {
        STOP.store(false, Ordering::Relaxed);
        let mut guard = Self {
            previous: Vec::new(),
        };
        for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
            // sigaction is initialized before installation, and only atomics are used in the handler.
            let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
            let mut old: libc::sigaction = unsafe { std::mem::zeroed() };
            action.sa_sigaction = stop as *const () as usize;
            unsafe {
                libc::sigemptyset(&mut action.sa_mask);
            }
            if unsafe { libc::sigaction(signal, &action, &mut old) } != 0 {
                return Err(io::Error::last_os_error());
            }
            guard.previous.push((signal, old));
        }
        terminal::enable_raw_mode()?;
        execute!(
            io::stdout(),
            terminal::EnterAlternateScreen,
            cursor::Hide,
            event::EnableMouseCapture,
            event::EnableBracketedPaste,
            event::EnableFocusChange,
            event::PushKeyboardEnhancementFlags(
                event::KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                    | event::KeyboardEnhancementFlags::REPORT_EVENT_TYPES
            )
        )?;
        io::stdout().flush()?;
        Ok(guard)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        restore();
        for (signal, old) in &self.previous {
            unsafe {
                libc::sigaction(*signal, old, std::ptr::null_mut());
            }
        }
    }
}

pub(super) fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        STOP.store(true, Ordering::Relaxed);
        restore();
        previous(info);
    }));
}
