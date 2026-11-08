//! 每个Web界面直接运行一个TUI；从不注册成用户工作会话。
use std::collections::VecDeque;
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::time::{Duration, Instant};

use super::{
    Error,
    protocol::{self, Op},
};

const CAP: usize = 2 * 1024 * 1024;
pub(super) struct Permit(Arc<AtomicUsize>);
impl Permit {
    pub fn acquire(count: &Arc<AtomicUsize>) -> Result<Self, Error> {
        let mut current = count.load(Ordering::Acquire);
        loop {
            if current >= 8 {
                return Err(Error::Busy);
            }
            match count.compare_exchange_weak(
                current,
                current + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Ok(Self(count.clone())),
                Err(now) => current = now,
            }
        }
    }
}
impl Drop for Permit {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

pub(super) fn launch(
    state: PathBuf,
    mut socket: UnixStream,
    stop: Arc<AtomicBool>,
    count: &Arc<AtomicUsize>,
) -> Result<(), Error> {
    let permit = match Permit::acquire(count) {
        Ok(permit) => permit,
        Err(error) => {
            report_error(&mut socket, &error);
            return Err(error);
        }
    };
    // 保留一份错误回执口，线程创建失败也不能让已认证连接静默EOF。
    let mut failure = match socket.try_clone() {
        Ok(socket) => socket,
        Err(error) => {
            let error = Error::Io(error);
            report_error(&mut socket, &error);
            return Err(error);
        }
    };
    match std::thread::Builder::new()
        .name("homeway-web-tui".into())
        .spawn(move || {
            let _permit = permit;
            if let Err(error) = serve(&state, &mut socket, &stop) {
                report_error(&mut socket, &error);
            }
        }) {
        Ok(_) => Ok(()),
        Err(error) => {
            let error = Error::Io(error);
            report_error(&mut failure, &error);
            Err(error)
        }
    }
}

fn report_error(socket: &mut UnixStream, error: &Error) {
    let (code, message): (u8, &[u8]) = match error {
        Error::Busy => (3, b"terminal capacity unavailable"),
        Error::Invalid(_) => (1, b"invalid terminal management frame"),
        Error::Unauthorized => (6, b"management credential revoked"),
        Error::Control(_) => (5, b"local control service unavailable"),
        _ => (4, b"terminal process or IO failed"),
    };
    let mut payload = Vec::with_capacity(message.len() + 1);
    payload.push(code);
    payload.extend_from_slice(message);
    let mut data = VecDeque::new();
    if queue(&mut data, Op::Error, &payload).is_err() || queue(&mut data, Op::Close, &[]).is_err() {
        return;
    }
    let _ = socket.set_nonblocking(true);
    let deadline = Instant::now() + Duration::from_millis(100);
    while !data.is_empty() && Instant::now() < deadline {
        match socket.write(data.make_contiguous()) {
            Ok(0) => break,
            Ok(n) => {
                data.drain(..n);
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                let mut poll = libc::pollfd {
                    fd: socket.as_raw_fd(),
                    events: libc::POLLOUT,
                    revents: 0,
                };
                // SAFETY: poll只借用本次栈上的有效pollfd，不转移socket所有权。
                unsafe {
                    libc::poll(&mut poll, 1, 10);
                }
            }
            Err(_) => break,
        }
    }
}

struct Ui {
    master: Option<File>,
    child: Option<Child>,
    status: Option<std::process::ExitStatus>,
}
impl Ui {
    fn command(state: &Path) -> Result<Command, Error> {
        let mut command = Command::new(std::env::current_exe()?);
        command
            .arg("tui")
            .arg("--no-spawn")
            .arg("--state")
            .arg(state)
            .env_clear();
        for key in [
            "HOME", "PATH", "USER", "LOGNAME", "SHELL", "LANG", "LC_ALL", "LC_CTYPE", "TMPDIR",
        ] {
            if let Some(value) = std::env::var_os(key) {
                command.env(key, value);
            }
        }
        command
            .env("TERM", "xterm-256color")
            .env("COLORTERM", "truecolor")
            .env("COLORFGBG", "15;0")
            .env("HOMEWAY_WEB_TUI", "1");
        Ok(command)
    }
    fn spawn(state: &Path, cols: u16, rows: u16) -> Result<Self, Error> {
        let (control, _) = homeway_core::daemon::client::ControlClient::dial_with_timeout(
            &state.join("control.sock"),
            "cli",
            "homeway-web-tui",
            Duration::from_secs(1),
        )?;
        control.close();
        Self::spawn_command(Self::command(state)?, cols, rows)
    }
    fn spawn_command(mut command: Command, cols: u16, rows: u16) -> Result<Self, Error> {
        protocol::size(cols, rows)?;
        let (mut master, mut slave) = (-1, -1);
        let mut ws = libc::winsize {
            ws_col: cols,
            ws_row: rows,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        if unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &raw mut ws,
            )
        } != 0
        {
            return Err(std::io::Error::last_os_error().into());
        }
        let master = unsafe { File::from_raw_fd(master) };
        let slave = unsafe { File::from_raw_fd(slave) };
        for fd in [master.as_raw_fd(), slave.as_raw_fd()] {
            if unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } == -1 {
                return Err(std::io::Error::last_os_error().into());
            }
        }
        command
            .stdin(Stdio::from(slave.try_clone()?))
            .stdout(Stdio::from(slave.try_clone()?))
            .stderr(Stdio::from(slave));
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let flags = unsafe { libc::fcntl(master.as_raw_fd(), libc::F_GETFL) };
        if flags < 0
            || unsafe { libc::fcntl(master.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) }
                < 0
        {
            return Err(std::io::Error::last_os_error().into());
        }
        let child = command.spawn()?;
        Ok(Self {
            master: Some(master),
            child: Some(child),
            status: None,
        })
    }
    fn resize(&self, cols: u16, rows: u16) -> Result<(), Error> {
        protocol::size(cols, rows)?;
        let ws = libc::winsize {
            ws_col: cols,
            ws_row: rows,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        if unsafe {
            libc::ioctl(
                self.master
                    .as_ref()
                    .ok_or(Error::Invalid("界面PTY已关闭"))?
                    .as_raw_fd(),
                libc::TIOCSWINSZ as _,
                &ws,
            )
        } < 0
        {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(())
    }
    fn poll_exit(&mut self) -> std::io::Result<Option<i32>> {
        if let Some(status) = self.status {
            return Ok(Some(status.code().unwrap_or(-1)));
        }
        let Some(child) = self.child.as_mut() else {
            return Ok(Some(-1));
        };
        if let Some(status) = child.try_wait()? {
            self.status = Some(status);
            return Ok(Some(status.code().unwrap_or(-1)));
        }
        Ok(None)
    }
    fn terminate(&mut self) {
        // 先关闭master，确保背压中的TTY读写解除；仅发信号而保留master
        // 在部分内核上会使子进程继续挂在终端写路径，延迟SIGKILL与收尸。
        drop(self.master.take());
        let Some(mut child) = self.child.take() else {
            return;
        };
        match child.try_wait() {
            Ok(Some(status)) => {
                self.status = Some(status);
                return;
            }
            // 无法确认仍为自有的未回收子进程时，不对可能复用的PID发信号。
            Err(_) => return,
            Ok(None) => {}
        }
        // SAFETY: Child仍拥有且刚确认尚未退出的正PID；只针对本界面，不发进程组信号。
        unsafe {
            libc::kill(child.id() as i32, libc::SIGHUP);
        }
        if let Some(status) = wait_child(&mut child, Duration::from_millis(500)) {
            self.status = Some(status);
            return;
        }
        let _ = child.kill();
        if let Some(status) = wait_child(&mut child, Duration::from_millis(500)) {
            self.status = Some(status);
            return;
        }
        // 极端不可杀状态不阻塞管理线程；收割者只持有这个Child，不重用裸PID。
        if let Err(error) = std::thread::Builder::new()
            .name("homeway-web-child-reap".into())
            .spawn(move || {
                let _ = child.wait();
            })
        {
            eprintln!("homeway: Web TUI child reaper unavailable: {error}");
        }
    }
}
impl Drop for Ui {
    fn drop(&mut self) {
        self.terminate();
    }
}

fn wait_child(child: &mut Child, budget: Duration) -> Option<std::process::ExitStatus> {
    let deadline = Instant::now() + budget;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status),
            Err(_) => return None,
            Ok(None) => {}
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn queue(out: &mut VecDeque<u8>, op: Op, data: &[u8]) -> Result<(), Error> {
    if out.len() + data.len() + 5 > CAP {
        return Err(Error::Busy);
    }
    let mut frame = Vec::with_capacity(data.len() + 5);
    protocol::write(&mut frame, op, data)?;
    out.extend(frame);
    Ok(())
}

fn serve(state: &Path, socket: &mut UnixStream, stop: &AtomicBool) -> Result<(), Error> {
    serve_with(socket, stop, |cols, rows| Ui::spawn(state, cols, rows))
}

fn serve_with(
    socket: &mut UnixStream,
    stop: &AtomicBool,
    mut spawn: impl FnMut(u16, u16) -> Result<Ui, Error>,
) -> Result<(), Error> {
    socket.set_nonblocking(true)?;
    let mut ui: Option<Ui> = None;
    let mut incoming = Vec::new();
    let mut to_pty = VecDeque::new();
    let mut outgoing = VecDeque::new();
    let mut buf = [0u8; 16384];
    let open_deadline = Instant::now() + Duration::from_secs(30);
    let mut output_stall: Option<Instant> = None;
    let mut input_stall: Option<Instant> = None;
    let mut eof_at: Option<Instant> = None;
    let mut ending: Option<i32> = None;
    let mut exit_queued = false;
    loop {
        if stop.load(Ordering::Acquire) {
            return Ok(());
        }
        if ui.is_none() && Instant::now() >= open_deadline {
            return Err(Error::Timeout);
        }
        if !outgoing.is_empty() {
            match socket.write(outgoing.make_contiguous()) {
                Ok(0) => return Ok(()),
                Ok(n) => {
                    outgoing.drain(..n);
                }
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    output_stall.get_or_insert_with(Instant::now);
                }
                Err(error) => return Err(error.into()),
            }
        }
        if outgoing.is_empty() {
            output_stall = None;
        }
        if output_stall.is_some_and(|since| since.elapsed() >= Duration::from_secs(10)) {
            return Err(Error::Timeout);
        }
        if exit_queued && outgoing.is_empty() {
            return Ok(());
        }
        if ending.is_none() && incoming.len() < protocol::MAX_FRAME + 5 {
            let available = buf.len().min(protocol::MAX_FRAME + 5 - incoming.len());
            match socket.read(&mut buf[..available]) {
                Ok(0) if incoming.is_empty() => return Ok(()),
                Ok(0) => return Err(Error::Invalid("管理终端帧截断")),
                Ok(n) => incoming.extend_from_slice(&buf[..n]),
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                    ) => {}
                Err(error) => return Err(error.into()),
            }
        }
        for _ in 0..16 {
            if incoming.len() < 5 || ending.is_some() {
                break;
            }
            let length = u32::from_be_bytes(incoming[1..5].try_into().expect("定长头")) as usize;
            if length > protocol::MAX_FRAME {
                return Err(Error::Invalid("管理终端帧超限"));
            }
            if incoming.len() < length + 5 {
                break;
            }
            if incoming[0] == Op::Input as u8 && to_pty.len() + length > CAP {
                break;
            }
            let (op, payload) = protocol::read(&mut &incoming[..length + 5])?;
            incoming.drain(..length + 5);
            match op {
                Op::Open if ui.is_none() => {
                    let (cols, rows) = protocol::parse_size(&payload[1..])?;
                    ui = Some(spawn(cols, rows)?);
                }
                Op::Input if ui.is_some() => to_pty.extend(payload),
                Op::Resize if ui.is_some() => {
                    let (cols, rows) = protocol::parse_size(&payload)?;
                    ui.as_ref().expect("已验").resize(cols, rows)?;
                }
                Op::Close => return Ok(()),
                _ => return Err(Error::Invalid("管理终端帧顺序或方向错误")),
            }
        }
        if let Some(child) = ui.as_mut() {
            if ending.is_none() && !to_pty.is_empty() {
                match child
                    .master
                    .as_mut()
                    .expect("active PTY")
                    .write(to_pty.make_contiguous())
                {
                    Ok(0) => return Err(std::io::Error::from(std::io::ErrorKind::WriteZero).into()),
                    Ok(n) => {
                        to_pty.drain(..n);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        input_stall.get_or_insert_with(Instant::now);
                    }
                    Err(error) => return Err(error.into()),
                }
            }
            if to_pty.is_empty() {
                input_stall = None;
            }
            if input_stall.is_some_and(|since| since.elapsed() >= Duration::from_secs(10)) {
                return Err(Error::Timeout);
            }
            if eof_at.is_none() && outgoing.len() + 5 < CAP {
                let available = buf.len().min(CAP - outgoing.len() - 5);
                match child
                    .master
                    .as_mut()
                    .expect("active PTY")
                    .read(&mut buf[..available])
                {
                    Ok(0) => {
                        eof_at = Some(Instant::now());
                    }
                    Ok(n) => queue(&mut outgoing, Op::Output, &buf[..n])?,
                    Err(error)
                        if matches!(
                            error.kind(),
                            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                        ) => {}
                    Err(error) if error.raw_os_error() == Some(libc::EIO) => {
                        eof_at = Some(Instant::now());
                    }
                    Err(error) => return Err(error.into()),
                }
            }
            if let Some(eof) = eof_at {
                if let Some(status) = child.poll_exit()? {
                    ending = Some(status);
                } else if eof.elapsed() >= Duration::from_millis(500) {
                    child.terminate();
                    ending = Some(child.status.and_then(|status| status.code()).unwrap_or(-1));
                }
            } else {
                // 子进程退出不表示PTY已排空；继续read直到EOF后才投EXIT。
                let _ = child.poll_exit()?;
            }
        }
        if let Some(status) = ending {
            if !exit_queued && outgoing.len() + 14 <= CAP {
                queue(&mut outgoing, Op::Exit, &status.to_be_bytes())?;
                queue(&mut outgoing, Op::Close, &[])?;
                exit_queued = true;
            }
        }
        let mut polls = [
            libc::pollfd {
                fd: socket.as_raw_fd(),
                events: (if ending.is_none() && incoming.len() < protocol::MAX_FRAME + 5 {
                    libc::POLLIN
                } else {
                    0
                }) | if outgoing.is_empty() {
                    0
                } else {
                    libc::POLLOUT
                },
                revents: 0,
            },
            libc::pollfd {
                fd: ui
                    .as_ref()
                    .and_then(|child| child.master.as_ref())
                    .map_or(-1, AsRawFd::as_raw_fd),
                events: (if eof_at.is_none() && outgoing.len() + 5 < CAP {
                    libc::POLLIN
                } else {
                    0
                }) | if to_pty.is_empty() || ending.is_some() {
                    0
                } else {
                    libc::POLLOUT
                },
                revents: 0,
            },
        ];
        // SAFETY: poll只借用活socket/master和本次栈上的两条pollfd。
        if unsafe { libc::poll(polls.as_mut_ptr(), polls.len() as _, 50) } < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::Interrupted {
                return Err(error.into());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Shutdown;
    use std::sync::mpsc;
    use std::thread::{self, JoinHandle};

    struct Session {
        socket: UnixStream,
        stop: Arc<AtomicBool>,
        child: mpsc::Receiver<u32>,
        worker: Option<JoinHandle<Result<(), Error>>>,
    }
    impl Session {
        fn new(command: Command) -> Self {
            let (socket, mut serving) = UnixStream::pair().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            socket
                .set_write_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let stop = Arc::new(AtomicBool::new(false));
            let stopped = stop.clone();
            let (child, receive) = mpsc::sync_channel(1);
            let worker = thread::spawn(move || {
                let mut command = Some(command);
                let result = serve_with(&mut serving, &stopped, |cols, rows| {
                    let ui = Ui::spawn_command(command.take().expect("only one OPEN"), cols, rows)?;
                    child.send(ui.child.as_ref().unwrap().id()).unwrap();
                    Ok(ui)
                });
                if let Err(error) = &result {
                    report_error(&mut serving, error);
                }
                result
            });
            Self {
                socket,
                stop,
                child: receive,
                worker: Some(worker),
            }
        }
        fn open(&mut self) -> u32 {
            let mut payload = vec![1];
            payload.extend_from_slice(&protocol::size(80, 24).unwrap());
            protocol::write(&mut self.socket, Op::Open, &payload).unwrap();
            self.child.recv_timeout(Duration::from_secs(2)).unwrap()
        }
        fn join(&mut self) -> Result<(), Error> {
            self.worker.take().unwrap().join().unwrap()
        }
        fn cancel(&mut self) -> Result<(), Error> {
            self.stop.store(true, Ordering::Release);
            let _ = self.socket.shutdown(Shutdown::Both);
            self.join()
        }
    }
    impl Drop for Session {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Release);
            let _ = self.socket.shutdown(Shutdown::Both);
            if let Some(worker) = self.worker.take() {
                let _ = worker.join();
            }
        }
    }

    fn raw_command(program: &str) -> Command {
        let mut command = Command::new(program);
        // 真实PTY测试用raw slave，避免行规程回显/换行转换掩盖字节顺序。
        unsafe {
            command.pre_exec(|| {
                let mut state = std::mem::zeroed::<libc::termios>();
                if libc::tcgetattr(0, &mut state) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                libc::cfmakeraw(&mut state);
                if libc::tcsetattr(0, libc::TCSANOW, &state) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        command
    }

    fn assert_reaped(pid: u32) {
        let mut status = 0;
        // SAFETY: 只检查本测试先前持有的子PID，不发送信号。
        let result = unsafe { libc::waitpid(pid as i32, &mut status, libc::WNOHANG) };
        assert_eq!(result, -1, "child must already be reaped");
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ECHILD)
        );
    }

    #[test]
    fn web_pty_output_is_byte_exact_and_drained_before_exit_close() {
        let expected = "0123456789abcdef".repeat(5000).into_bytes();
        let mut command = raw_command("/usr/bin/printf");
        command.arg(std::str::from_utf8(&expected).unwrap());
        let mut session = Session::new(command);
        let pid = session.open();
        let mut observed = Vec::new();
        let mut exited = false;
        loop {
            let (op, payload) = protocol::read(&mut session.socket).unwrap();
            match op {
                Op::Output => {
                    assert!(!exited);
                    observed.extend(payload);
                }
                Op::Exit => {
                    assert_eq!(observed, expected);
                    assert_eq!(payload, 0i32.to_be_bytes());
                    exited = true;
                }
                Op::Close => {
                    assert!(exited);
                    break;
                }
                _ => panic!("unexpected output frame"),
            }
        }
        session.join().unwrap();
        assert_reaped(pid);
    }

    #[test]
    fn web_pty_input_resize_and_prompt_output_do_not_wait_for_a_full_buffer() {
        let mut session = Session::new(raw_command("/bin/cat"));
        let pid = session.open();
        protocol::write(
            &mut session.socket,
            Op::Resize,
            &protocol::size(101, 31).unwrap(),
        )
        .unwrap();
        let expected = "中文🙂 input".as_bytes();
        let started = Instant::now();
        protocol::write(&mut session.socket, Op::Input, expected).unwrap();
        let mut observed = Vec::new();
        while observed.len() < expected.len() {
            let (op, payload) = protocol::read(&mut session.socket).unwrap();
            assert_eq!(op, Op::Output);
            observed.extend(payload);
        }
        assert_eq!(observed, expected);
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "small output should flush promptly"
        );
        protocol::write(&mut session.socket, Op::Close, &[]).unwrap();
        session.join().unwrap();
        assert_reaped(pid);
    }

    #[test]
    fn web_pty_exit_status_follows_all_final_output() {
        let mut command = raw_command("/bin/sh");
        command.args(["-c", "printf final-output; exit 7"]);
        let mut session = Session::new(command);
        let pid = session.open();
        let mut output = Vec::new();
        loop {
            let (op, payload) = protocol::read(&mut session.socket).unwrap();
            match op {
                Op::Output => output.extend(payload),
                Op::Exit => {
                    assert_eq!(&output, b"final-output");
                    assert_eq!(payload, 7i32.to_be_bytes());
                }
                Op::Close => break,
                _ => panic!("unexpected response"),
            }
        }
        session.join().unwrap();
        assert_reaped(pid);
    }

    #[test]
    fn web_pty_backpressure_cancel_does_not_block_and_reaps_hup_resistant_child() {
        let mut command = raw_command("/bin/sh");
        command.args(["-c", "trap '' HUP; printf READY; while :; do printf '0123456789abcdef0123456789abcdef'; done"]);
        let mut session = Session::new(command);
        let pid = session.open();
        let (op, _) = protocol::read(&mut session.socket).unwrap();
        assert_eq!(op, Op::Output);
        // 先缩小消费者缓冲并停止读取，让真实PTY/传输积压，而非用mock阻塞写。
        let size: libc::c_int = 4096;
        unsafe {
            libc::setsockopt(
                session.socket.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_RCVBUF,
                (&size as *const libc::c_int).cast(),
                std::mem::size_of_val(&size) as _,
            );
        }
        let (_tx, rx) = mpsc::channel::<()>();
        let _ = rx.recv_timeout(Duration::from_millis(100));
        let started = Instant::now();
        let _ = session.cancel();
        assert!(
            started.elapsed() < Duration::from_millis(1500),
            "cancel must not wait for writable PTY or peer"
        );
        assert_reaped(pid);
    }

    #[test]
    fn web_pty_protocol_error_and_busy_launch_report_error_without_spawn() {
        let mut session = Session::new(raw_command("/bin/cat"));
        protocol::write(&mut session.socket, Op::Input, b"before-open").unwrap();
        let (op, payload) = protocol::read(&mut session.socket).unwrap();
        assert_eq!(op, Op::Error);
        assert_eq!(payload[0], 1);
        assert!(matches!(session.join(), Err(Error::Invalid(_))));
        assert!(session.child.try_recv().is_err());

        let (mut client, server) = UnixStream::pair().unwrap();
        let count = Arc::new(AtomicUsize::new(8));
        assert!(matches!(
            launch(
                PathBuf::from("/not-used"),
                server,
                Arc::new(AtomicBool::new(false)),
                &count
            ),
            Err(Error::Busy)
        ));
        let (op, payload) = protocol::read(&mut client).unwrap();
        assert_eq!(op, Op::Error);
        assert_eq!(payload[0], 3);
        assert_eq!(count.load(Ordering::Acquire), 8);
    }

    #[test]
    fn web_pty_tui_command_is_fixed_no_spawn_and_missing_control_has_no_side_effect() {
        let state = std::env::temp_dir().join(format!(
            "homeway-web-nosvc-{}-{}",
            std::process::id(),
            Instant::now().elapsed().as_nanos()
        ));
        assert!(!state.exists());
        let command = Ui::command(&state).unwrap();
        assert_eq!(command.get_program(), std::env::current_exe().unwrap());
        assert!(
            command
                .get_envs()
                .any(|(name, value)| name == "HOMEWAY_WEB_TUI"
                    && value == Some(std::ffi::OsStr::new("1")))
        );
        assert!(
            !command
                .get_envs()
                .any(|(name, _)| name == "SSH_CONNECTION" || name == "SSH_TTY")
        );
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            vec![
                std::ffi::OsStr::new("tui"),
                std::ffi::OsStr::new("--no-spawn"),
                std::ffi::OsStr::new("--state"),
                state.as_os_str()
            ]
        );
        assert!(matches!(Ui::spawn(&state, 80, 24), Err(Error::Control(_))));
        assert!(!state.exists());
    }
}
