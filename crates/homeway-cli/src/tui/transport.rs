//! TUI surface 观察腿：有界下行队列、可取消连接与独立写线程。
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use homeway_core::daemon::client::{ClientStream, ControlClient};
use homeway_core::term::frames::{self, caps, Frame, Op};
use homeway_core::term::wire::FrameIo;

use super::backend::{deliver, Error, Target};

const IO_BUDGET: Duration = Duration::from_secs(10);

enum Connection {
    Local(FrameIo),
    Remote {
        client: Arc<ControlClient>,
        stream: Arc<ClientStream>,
        buffered: Vec<u8>,
    },
}

pub(super) struct TermConnection {
    inner: Connection,
    deadline: Instant,
    features: u32,
}

impl Drop for TermConnection {
    fn drop(&mut self) {
        match &self.inner {
            Connection::Local(io) => {
                if let Ok(socket) = io.try_clone_stream() {
                    let _ = socket.shutdown(Shutdown::Both);
                }
            }
            Connection::Remote { client, .. } => client.close(),
        }
    }
}

impl TermConnection {
    pub fn connect(state: &Path, target: &Target) -> Result<Self, Error> {
        Self::connect_with_budget(state, target, IO_BUDGET)
    }

    pub fn connect_with_budget(
        state: &Path,
        target: &Target,
        budget: Duration,
    ) -> Result<Self, Error> {
        let deadline = Instant::now() + budget;
        let inner = match target {
            Target::Local => {
                let stream = crate::local_term::connect_local_terminal_with_budget(
                    state,
                    deadline.saturating_duration_since(Instant::now()),
                )
                .map_err(|error| {
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
                    ) {
                        Error::LocalUnavailable
                    } else {
                        Error::Io(error)
                    }
                })?;
                Connection::Local(FrameIo::new(stream))
            }
            Target::Remote(host) => {
                let (client, _) = ControlClient::dial_with_timeout(
                    &state.join("control.sock"),
                    "cli",
                    "homeway-tui-term",
                    deadline.saturating_duration_since(Instant::now()),
                )?;
                let stream = match client.open_stream(
                    "term",
                    host,
                    deadline.saturating_duration_since(Instant::now()),
                ) {
                    Ok(stream) => stream,
                    Err(error) => {
                        client.close();
                        return Err(Error::Control(error));
                    }
                };
                Connection::Remote {
                    client,
                    stream,
                    buffered: Vec::new(),
                }
            }
        };
        let mut connection = Self {
            inner,
            deadline,
            features: 0,
        };
        let greeting = connection.read(Some(IO_BUDGET))?;
        if greeting.op != Op::GREETING {
            return Err(Error::Unexpected(greeting.op.0));
        }
        let (version, features) = frames::dec_greeting(&greeting.payload)?;
        if version != frames::PROTO_VER {
            return Err(Error::ObserverUnavailable);
        }
        connection.features = features;
        Ok(connection)
    }

    fn read(&mut self, budget: Option<Duration>) -> Result<Frame, Error> {
        let budget = budget
            .map(|budget| budget.min(self.deadline.saturating_duration_since(Instant::now())));
        if budget.is_some_and(|budget| budget.is_zero()) {
            return Err(Error::Timeout);
        }
        match &mut self.inner {
            Connection::Local(io) => match budget {
                Some(budget) => io.read_frame_deadline(budget),
                None => io.read_frame(),
            }
            .map_err(Error::Read),
            Connection::Remote {
                stream, buffered, ..
            } => {
                let deadline = budget.map(|budget| Instant::now() + budget);
                loop {
                    if buffered.len() >= 3 {
                        let len = u16::from_le_bytes([buffered[1], buffered[2]]) as usize;
                        if buffered.len() >= len + 3 {
                            let frame = Frame {
                                op: Op(buffered[0]),
                                payload: buffered[3..len + 3].to_vec(),
                            };
                            buffered.drain(..len + 3);
                            return Ok(frame);
                        }
                    }
                    let chunk = match deadline {
                        Some(deadline) => {
                            let remaining = deadline.saturating_duration_since(Instant::now());
                            if remaining.is_zero() {
                                return Err(Error::Timeout);
                            }
                            stream.recv_timeout(remaining)
                        }
                        None => stream.recv_wait(),
                    };
                    match chunk {
                        Some(chunk) => buffered.extend_from_slice(&chunk),
                        None => {
                            return Err(match stream.end_reason() {
                                Some(reason) => Error::StreamEnded(reason),
                                None => Error::Timeout,
                            })
                        }
                    }
                }
            }
        }
    }

    fn write(&mut self, op: Op, payload: &[u8]) -> Result<(), Error> {
        let remaining = self.deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(Error::Timeout);
        }
        match &mut self.inner {
            Connection::Local(io) => io.write_frame(op, payload, remaining).map_err(Error::Write),
            Connection::Remote { client, stream, .. } => client
                .stream_send(stream, &frames::encode_frame(op, payload))
                .map_err(Error::Client),
        }
    }

    pub fn round_trip(&mut self, op: Op, payload: &[u8]) -> Result<Frame, Error> {
        self.write(op, payload)?;
        let frame = self.read(Some(IO_BUDGET))?;
        check_error(&frame)?;
        let expected = if op == Op::LIST { Op::LIST } else { Op::OK };
        if frame.op != expected {
            return Err(Error::Unexpected(frame.op.0));
        }
        Ok(frame)
    }

    fn closer(&self) -> Result<Closer, Error> {
        match &self.inner {
            Connection::Local(io) => Ok(Closer::Local(io.try_clone_stream()?)),
            Connection::Remote { client, .. } => Ok(Closer::Remote(Arc::clone(client))),
        }
    }

    fn writer(&self) -> Result<Writer, Error> {
        match &self.inner {
            Connection::Local(io) => Ok(Writer::Local(FrameIo::new(io.try_clone_stream()?))),
            Connection::Remote { client, stream, .. } => Ok(Writer::Remote {
                client: Arc::clone(client),
                stream: Arc::clone(stream),
            }),
        }
    }
}

fn check_error(frame: &Frame) -> Result<(), Error> {
    if frame.op == Op::ERROR {
        let (code, message) = frames::dec_error(&frame.payload)?;
        return Err(Error::Session { code, message });
    }
    Ok(())
}

enum Closer {
    Local(UnixStream),
    Remote(Arc<ControlClient>),
}
impl Closer {
    fn close(&self) {
        match self {
            Self::Local(socket) => {
                let _ = socket.shutdown(Shutdown::Both);
            }
            Self::Remote(client) => client.close(),
        }
    }
}

enum Writer {
    Local(FrameIo),
    Remote {
        client: Arc<ControlClient>,
        stream: Arc<ClientStream>,
    },
}
impl Writer {
    fn write(&mut self, op: Op, data: &[u8]) -> Result<(), Error> {
        match self {
            Self::Local(io) => io.write_frame(op, data, IO_BUDGET).map_err(Error::Write),
            Self::Remote { client, stream } => client
                .stream_send(stream, &frames::encode_frame(op, data))
                .map_err(Error::Client),
        }
    }
}

pub(super) struct AttachEvent {
    pub generation: u64,
    pub frame: Result<Frame, Error>,
}

struct WriteCommand {
    op: Op,
    data: Vec<u8>,
}

struct Cancellation {
    alive: AtomicBool,
    closer: Mutex<Option<Closer>>,
}
impl Cancellation {
    fn close(&self) {
        self.alive.store(false, Ordering::Release);
        if let Some(closer) = self
            .closer
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .as_ref()
        {
            closer.close();
        }
    }
}

pub(super) struct Attach {
    commands: mpsc::SyncSender<WriteCommand>,
    events: mpsc::Receiver<AttachEvent>,
    cancel: Arc<Cancellation>,
}

impl Attach {
    pub fn start(
        state: PathBuf,
        target: Target,
        name: String,
        cols: u16,
        rows: u16,
        generation: u64,
    ) -> Self {
        let (commands, writes) = mpsc::sync_channel::<WriteCommand>(64);
        let (events_tx, events) = mpsc::sync_channel(128);
        let cancel = Arc::new(Cancellation {
            alive: AtomicBool::new(true),
            closer: Mutex::new(None),
        });
        let running = Arc::clone(&cancel);
        std::thread::spawn(move || {
            let run = || -> Result<(), Error> {
                let mut connection = TermConnection::connect(&state, &target)?;
                let required = frames::features::SURFACE
                    | frames::features::PROTO_VER
                    | frames::features::OBSERVER;
                if connection.features & required != required {
                    return Err(Error::ObserverUnavailable);
                }
                *running
                    .closer
                    .lock()
                    .unwrap_or_else(|error| error.into_inner()) = Some(connection.closer()?);
                if !running.alive.load(Ordering::Acquire) {
                    return Ok(());
                }
                let platform = caps::KEY_ALT_ESC_PREFIX;
                let identifier = format!("tui-{}-{generation}", std::process::id());
                let hello = frames::enc_hello(
                    cols.max(1),
                    rows.max(1),
                    frames::hello_flags::OBSERVER,
                    &name,
                    &frames::enc_hello_tail(
                        caps::SURFACE | caps::PROTO_VER | platform,
                        &identifier,
                    ),
                );
                connection.write(Op::HELLO, &hello)?;
                let attached = connection.read(Some(IO_BUDGET))?;
                check_error(&attached)?;
                if attached.op != Op::ATTACHED {
                    return Err(Error::Unexpected(attached.op.0));
                }
                frames::dec_attached(&attached.payload)?;
                if !deliver(
                    &events_tx,
                    AttachEvent {
                        generation,
                        frame: Ok(attached),
                    },
                    &running.alive,
                ) {
                    return Ok(());
                }
                let mut writer = connection.writer()?;
                let writing = Arc::clone(&running);
                let write_events = events_tx.clone();
                std::thread::spawn(move || {
                    while writing.alive.load(Ordering::Acquire) {
                        match writes.recv_timeout(Duration::from_millis(100)) {
                            Ok(command) => {
                                if let Err(error) = writer.write(command.op, &command.data) {
                                    let _ = deliver(
                                        &write_events,
                                        AttachEvent {
                                            generation,
                                            frame: Err(error),
                                        },
                                        &writing.alive,
                                    );
                                    break;
                                }
                            }
                            Err(mpsc::RecvTimeoutError::Timeout) => continue,
                            Err(mpsc::RecvTimeoutError::Disconnected) => break,
                        }
                    }
                    writing.close();
                });
                while running.alive.load(Ordering::Acquire) {
                    let frame = connection.read(None)?;
                    let ended = frame.op == Op::ENDED;
                    if !deliver(
                        &events_tx,
                        AttachEvent {
                            generation,
                            frame: Ok(frame),
                        },
                        &running.alive,
                    ) || ended
                    {
                        break;
                    }
                }
                Ok(())
            };
            if let Err(error) = run() {
                let _ = deliver(
                    &events_tx,
                    AttachEvent {
                        generation,
                        frame: Err(error),
                    },
                    &running.alive,
                );
            }
            running.close();
        });
        Self {
            commands,
            events,
            cancel,
        }
    }

    fn send(&self, op: Op, data: Vec<u8>) -> Result<(), Error> {
        if !self.cancel.alive.load(Ordering::Acquire) {
            return Err(Error::Closed);
        }
        self.commands
            .try_send(WriteCommand { op, data })
            .map_err(|error| match error {
                mpsc::TrySendError::Full(_) => Error::Busy,
                mpsc::TrySendError::Disconnected(_) => Error::Closed,
            })
    }

    pub fn send_frame(&self, op: Op, data: Vec<u8>) -> Result<(), Error> {
        if data.len() > frames::DATA_CHUNK {
            return Err(Error::Busy);
        }
        self.send(op, data)
    }
    pub fn fetch_rows(
        &self,
        request: homeway_core::term::codec::FetchRowsReq,
    ) -> Result<(), Error> {
        self.send_frame(
            Op::FETCH_ROWS,
            homeway_core::term::codec::enc_fetch_rows_req(request),
        )
    }
    pub fn fetch_snapshot(&self) -> Result<(), Error> {
        self.send_frame(Op::FETCH_SNAPSHOT, Vec::new())
    }
    pub fn resize(&self, cols: u16, rows: u16) -> Result<(), Error> {
        self.send(Op::RESIZE, frames::enc_resize(cols.max(1), rows.max(1)))
    }
    pub fn try_event(&self) -> Option<AttachEvent> {
        self.events.try_recv().ok()
    }
}
impl Drop for Attach {
    fn drop(&mut self) {
        self.cancel.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;

    #[test]
    fn resize_queue_full_retries_the_same_geometry() {
        let (commands, queued) = mpsc::sync_channel(1);
        commands
            .send(WriteCommand {
                op: Op::DATA,
                data: vec![b'x'],
            })
            .unwrap();
        let (_events_tx, events) = mpsc::sync_channel(1);
        let attach = Attach {
            commands,
            events,
            cancel: Arc::new(Cancellation {
                alive: AtomicBool::new(true),
                closer: Mutex::new(None),
            }),
        };
        let mut app = super::super::App::new(super::super::Options {
            state: PathBuf::from("/tmp/hw-tui-resize-queue-test"),
        });
        app.attach = Some(attach);
        app.resize(90, 25);
        assert_eq!(app.size, (90, 25));
        assert_eq!(app.sent_size, None);
        assert_eq!(queued.recv().unwrap().op, Op::DATA);
        app.resize(90, 25);
        assert_eq!(app.sent_size, Some((90, 25)));
        let frame = queued.recv().unwrap();
        assert_eq!(frame.op, Op::RESIZE);
        assert_eq!(frames::dec_resize(&frame.data).unwrap(), (90, 25));
    }

    #[test]
    fn observer_handshake_preserves_surface_and_only_writes_explicit_input() {
        use homeway_core::term::codec;
        let root = PathBuf::from("/tmp").join(format!(
            "hw-tui-surface-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let listener = UnixListener::bind(root.join("term.sock")).unwrap();
        let (idle_tx, idle) = mpsc::channel();
        let (observed_tx, observed) = mpsc::channel();
        let payload = codec::fragment_payload(&codec::gzip_bytes(&codec::enc_snapshot_body(
            &codec::SnapshotBody {
                cols: 120,
                rows: 30,
                seq: 7,
                grid: codec::encode_grid(120, 30, &[]),
                ..Default::default()
            },
        )))
        .remove(0);
        let expected = payload.clone();
        let server = std::thread::spawn(move || {
            let (socket, _) = listener.accept().unwrap();
            let mut io = FrameIo::new(socket);
            io.write_frame(Op::GREETING, &frames::enc_greeting(), IO_BUDGET)
                .unwrap();
            let hello = io.read_frame_deadline(IO_BUDGET).unwrap();
            assert_eq!(hello.op, Op::HELLO);
            let (cols, rows, flags, name, tail) = frames::dec_hello(&hello.payload).unwrap();
            assert_eq!(
                (cols, rows, flags, name.as_str()),
                (80, 24, frames::hello_flags::OBSERVER, "session")
            );
            assert_ne!(
                frames::dec_hello_tail(tail).unwrap().caps & caps::SURFACE,
                0
            );
            io.write_frame(
                Op::ATTACHED,
                &frames::enc_attached(120, 30, 0, 0, 3, "session"),
                IO_BUDGET,
            )
            .unwrap();
            io.write_frame(Op::SNAPSHOT, &payload, IO_BUDGET).unwrap();
            io.write_frame(Op::SNAPSHOT_DONE, &[], IO_BUDGET).unwrap();
            assert!(
                io.read_frame_deadline(Duration::from_millis(50)).is_err(),
                "surface只观察不自动答复或抢几何"
            );
            idle_tx.send(()).unwrap();
            observed_tx
                .send(io.read_frame_deadline(IO_BUDGET).unwrap())
                .unwrap();
        });
        let attach = Attach::start(root.clone(), Target::Local, "session".into(), 80, 24, 27);
        let deadline = Instant::now() + IO_BUDGET;
        let mut saw_snapshot = false;
        loop {
            if let Some(event) = attach.try_event() {
                assert_eq!(event.generation, 27);
                let frame = event.frame.unwrap();
                if frame.op == Op::SNAPSHOT {
                    assert_eq!(frame.payload, expected);
                    saw_snapshot = true;
                }
                if frame.op == Op::SNAPSHOT_DONE {
                    break;
                }
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(saw_snapshot);
        idle.recv_timeout(IO_BUDGET).unwrap();
        let input = frames::InputEvent::Text {
            flags: 0,
            text: "input".into(),
        };
        attach
            .send_frame(Op::INPUT, frames::enc_input(&input))
            .unwrap();
        let frame = observed.recv_timeout(IO_BUDGET).unwrap();
        assert_eq!(frame.op, Op::INPUT);
        assert_eq!(frames::dec_input(&frame.payload).unwrap(), input);
        drop(attach);
        server.join().unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn old_server_is_rejected_before_sending_observer_hello() {
        let root = PathBuf::from("/tmp").join(format!(
            "hw-tui-old-surface-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let listener = UnixListener::bind(root.join("term.sock")).unwrap();
        let server = std::thread::spawn(move || {
            let (socket, _) = listener.accept().unwrap();
            let mut io = FrameIo::new(socket);
            io.write_frame(Op::GREETING, &[1, 0x7f, 0, 0, 0], IO_BUDGET)
                .unwrap();
            assert!(
                io.read_frame_deadline(IO_BUDGET).is_err(),
                "旧服务不能收到有副作用的HELLO"
            );
        });
        let attach = Attach::start(root.clone(), Target::Local, "session".into(), 80, 24, 28);
        let deadline = Instant::now() + IO_BUDGET;
        loop {
            if let Some(event) = attach.try_event() {
                assert!(matches!(event.frame, Err(Error::ObserverUnavailable)));
                break;
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(2));
        }
        drop(attach);
        server.join().unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn dropping_attach_disconnects_without_killing_session() {
        let root = std::env::temp_dir().join(format!("homeway-tui-drop-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let sock = root.join("term.sock");
        let listener = UnixListener::bind(&sock).unwrap();
        let (observed_tx, observed) = mpsc::channel();
        let server = std::thread::spawn(move || {
            let (socket, _) = listener.accept().unwrap();
            let mut io = FrameIo::new(socket);
            io.write_frame(Op::GREETING, &frames::enc_greeting(), IO_BUDGET)
                .unwrap();
            let hello = io.read_frame_deadline(IO_BUDGET).unwrap();
            assert_eq!(hello.op, Op::HELLO);
            io.write_frame(
                Op::ATTACHED,
                &frames::enc_attached(80, 24, 0, 0, 3, "session"),
                IO_BUDGET,
            )
            .unwrap();
            observed_tx
                .send(io.read_frame_deadline(IO_BUDGET).map(|frame| frame.op))
                .unwrap();
        });
        let attach = Attach::start(root.clone(), Target::Local, "session".into(), 80, 24, 9);
        let deadline = Instant::now() + IO_BUDGET;
        loop {
            if let Some(event) = attach.try_event() {
                assert_eq!(event.generation, 9);
                assert_eq!(event.frame.unwrap().op, Op::ATTACHED);
                break;
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        drop(attach);
        assert!(observed.recv_timeout(IO_BUDGET).unwrap().is_err());
        server.join().unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }
}
