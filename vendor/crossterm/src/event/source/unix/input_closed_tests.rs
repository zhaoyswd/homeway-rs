//! Regression tests shared by the two Unix event backends.
use std::{io, io::Write, os::unix::net::UnixStream, sync::mpsc, time::Duration};

use super::UnixInternalEventSource;
use crate::event::{source::EventSource, Event, InternalEvent, KeyCode};
use crate::terminal::sys::file_descriptor::FileDesc;

fn source(reader: UnixStream) -> UnixInternalEventSource {
    reader.set_nonblocking(true).unwrap();
    #[cfg(feature = "libc")]
    let fd = {
        use std::os::fd::IntoRawFd;
        FileDesc::new(reader.into_raw_fd(), true)
    };
    #[cfg(not(feature = "libc"))]
    let fd = FileDesc::Owned(reader.into());
    UnixInternalEventSource::from_file_descriptor(fd).unwrap()
}

#[test]
fn eof_is_reported_without_spinning() {
    let (reader, writer) = UnixStream::pair().unwrap();
    let mut source = source(reader);
    drop(writer);
    // A regressed backend must fail this test instead of hanging the test runner.
    // The enclosing test process owns the detached thread and exits after failure.
    let (tx, rx) = mpsc::channel();
    let worker = std::thread::spawn(move || tx.send(source.try_read(None)).unwrap());
    let error = rx
        .recv_timeout(Duration::from_secs(2))
        .expect("event source spun after EOF")
        .unwrap_err();
    worker.join().unwrap();
    assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);
}

#[test]
fn idle_input_waits_and_remains_readable() {
    let (reader, mut writer) = UnixStream::pair().unwrap();
    let mut source = source(reader);
    let start = std::time::Instant::now();
    assert!(source
        .try_read(Some(Duration::from_millis(40)))
        .unwrap()
        .is_none());
    assert!(start.elapsed() >= Duration::from_millis(20));
    writer.write_all(b"a").unwrap();
    assert!(
        matches!(source.try_read(Some(Duration::from_secs(1))).unwrap(),
        Some(InternalEvent::Event(Event::Key(key))) if key.code == KeyCode::Char('a'))
    );
}

#[test]
fn complete_events_precede_terminal_closure() {
    let (reader, mut writer) = UnixStream::pair().unwrap();
    let mut source = source(reader);
    let input = vec![b'a'; 4096];
    writer.write_all(&input).unwrap();
    drop(writer);
    for expected in input.into_iter().map(char::from) {
        assert!(
            matches!(source.try_read(Some(Duration::from_secs(1))).unwrap(),
            Some(InternalEvent::Event(Event::Key(key))) if key.code == KeyCode::Char(expected))
        );
    }
    assert_eq!(
        source
            .try_read(Some(Duration::from_secs(1)))
            .unwrap_err()
            .kind(),
        io::ErrorKind::UnexpectedEof
    );
}

#[cfg(not(feature = "use-dev-tty"))]
#[test]
fn unexpected_read_error_is_propagated() {
    // A valid, owned write-only FD reliably makes read fail with EBADF. This
    // exercises the actual OS error path without closing/reusing a borrowed FD.
    let file = std::fs::OpenOptions::new()
        .write(true)
        .open("/dev/null")
        .unwrap();
    #[cfg(feature = "libc")]
    let fd = {
        use std::os::fd::IntoRawFd;
        FileDesc::new(file.into_raw_fd(), true)
    };
    #[cfg(not(feature = "libc"))]
    let fd = FileDesc::Owned(file.into());
    let (tx, rx) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        tx.send(UnixInternalEventSource::read_tty(
            &fd,
            &mut [0; 1024],
            &mut super::Parser::default(),
            false,
        ))
        .unwrap();
    });
    let error = rx
        .recv_timeout(Duration::from_secs(2))
        .expect("event source spun after a terminal read error")
        .unwrap_err();
    worker.join().unwrap();
    assert!(error.raw_os_error().is_some());
    assert_ne!(error.kind(), io::ErrorKind::UnexpectedEof);
}

#[cfg(feature = "bracketed-paste")]
#[test]
fn closed_input_delivers_a_large_utf8_paste_before_eof() {
    let (reader, mut writer) = UnixStream::pair().unwrap();
    let mut source = source(reader);
    let text = "中🙂".repeat(1024);
    writer.write_all(b"\x1b[200~").unwrap();
    writer.write_all(text.as_bytes()).unwrap();
    writer.write_all(b"\x1b[201~").unwrap();
    drop(writer);
    assert!(
        matches!(source.try_read(Some(Duration::from_secs(1))).unwrap(),
        Some(InternalEvent::Event(Event::Paste(actual))) if actual == text)
    );
    assert_eq!(
        source
            .try_read(Some(Duration::from_secs(1)))
            .unwrap_err()
            .kind(),
        io::ErrorKind::UnexpectedEof
    );
}
