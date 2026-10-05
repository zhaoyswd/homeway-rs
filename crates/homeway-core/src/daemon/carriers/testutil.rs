//! carriers 单测的公共件：假拨号缝（拨到本地 echo/目标 TCP listener——同一
//! CarrierConn 形态）与临时状态目录。

use std::sync::{Arc, Mutex};
use std::time::Duration;


use super::super::StreamConn;
use super::{CarrierConn, CarrierDial, DialErr};

/// 本地 TCP 连接的 StreamConn 面（测试拨号缝的产物——真 socket、真 EOF/RST 语义）。
pub(super) struct TcpIo {
    rd: Mutex<std::net::TcpStream>,
    wr: Mutex<std::net::TcpStream>,
}

impl TcpIo {
    pub fn new(s: std::net::TcpStream) -> TcpIo {
        let wr = s.try_clone().expect("clone");
        TcpIo { rd: Mutex::new(s), wr: Mutex::new(wr) }
    }
}

impl StreamConn for TcpIo {
    fn read_chunk(&self) -> std::io::Result<Vec<u8>> {
        use std::io::Read as _;
        let mut s = self.rd.lock().unwrap();
        let mut buf = [0u8; 16 * 1024];
        let n = s.read(&mut buf)?;
        Ok(buf[..n].to_vec())
    }

    fn write_chunk(&self, data: &[u8]) -> std::io::Result<usize> {
        use std::io::Write as _;
        self.wr.lock().unwrap().write_all(data)?;
        Ok(data.len())
    }

    fn shutdown_write(&self) {
        let _ = self.wr.lock().unwrap().shutdown(std::net::Shutdown::Write);
    }

    fn close(&self) {
        let _ = self.wr.lock().unwrap().shutdown(std::net::Shutdown::Both);
        let _ = self.rd.lock().unwrap().shutdown(std::net::Shutdown::Both);
    }
}

/// 假测速腿（speed 面不进这些测试——占位实现）。
struct NoopSpeed;

impl crate::speedtest::SpeedConn for NoopSpeed {
    fn write_frame(&self, _d: &[u8]) -> Result<(), crate::speedtest::SpeedtestError> {
        Ok(())
    }
    fn read_some(&self) -> Result<Vec<u8>, crate::speedtest::SpeedtestError> {
        Ok(Vec::new())
    }
    fn kill(&self) {}
}

pub(super) fn tcp_conn(s: std::net::TcpStream) -> CarrierConn {
    CarrierConn { io: Arc::new(TcpIo::new(s)), speed: Arc::new(NoopSpeed) }
}

/// 测试拨号缝：记录全部 (host, port/addr) 拨号请求；按表把 `出口端口` 派发到
/// 本地 listener（None = 拨号失败 Other）。
pub(super) struct FakeDial {
    /// 端口 → 本地目标 listener 的客户端连接构造面（「出口自己同端口」的模拟）。
    pub port_map: Mutex<std::collections::HashMap<u16, std::net::SocketAddr>>,
    /// 任意目标（socks 的 IP:port）→ 本地目标。
    pub addr_map: Mutex<std::collections::HashMap<std::net::SocketAddrV4, std::net::SocketAddr>>,
    /// 拨号请求记录（断言用）。
    pub calls: Mutex<Vec<String>>,
    /// 拨号失败注入（Some(err) = 全部失败）。
    pub fail_with: Mutex<Option<DialErr>>,
}

impl FakeDial {
    pub fn new() -> Arc<FakeDial> {
        Arc::new(FakeDial {
            port_map: Mutex::new(std::collections::HashMap::new()),
            addr_map: Mutex::new(std::collections::HashMap::new()),
            calls: Mutex::new(Vec::new()),
            fail_with: Mutex::new(None),
        })
    }

    pub fn carrier_dial(self: &Arc<Self>) -> CarrierDial {
        let d1 = Arc::clone(self);
        let dial_port = Arc::new(
            move |host: &str, port: u16| -> Result<CarrierConn, DialErr> {
                if let Some(e) = d1.fail_with.lock().unwrap().clone() {
                    return Err(e);
                }
                d1.calls.lock().unwrap().push(format!("{host}:{port}"));
                let dst = d1
                    .port_map
                    .lock()
                    .unwrap()
                    .get(&port)
                    .copied()
                    .ok_or_else(|| DialErr::Other(format!("无模拟端口 {port}")))?;
                let s = std::net::TcpStream::connect(dst)
                    .map_err(|e| DialErr::Other(e.to_string()))?;
                Ok(tcp_conn(s))
            },
        );
        let d2 = Arc::clone(self);
        let dial = Arc::new(
            move |host: &str, dst: std::net::SocketAddrV4| -> Result<CarrierConn, DialErr> {
                if let Some(e) = d2.fail_with.lock().unwrap().clone() {
                    return Err(e);
                }
                d2.calls.lock().unwrap().push(format!("{host}->{dst}"));
                let to = d2
                    .addr_map
                    .lock()
                    .unwrap()
                    .get(&dst)
                    .copied()
                    .ok_or_else(|| DialErr::Other(format!("无模拟目标 {dst}")))?;
                let s = std::net::TcpStream::connect(to)
                    .map_err(|e| DialErr::Other(e.to_string()))?;
                Ok(tcp_conn(s))
            },
        );
        CarrierDial { dial_port, dial }
    }
}

/// 本地 echo listener（收到即回写；关停 = drop 句柄面）。
pub(super) fn echo_listener() -> (std::net::SocketAddr, std::thread::JoinHandle<()>) {
    let ln = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = ln.local_addr().unwrap();
    let h = std::thread::spawn(move || {
        for c in ln.incoming() {
            let Ok(c) = c else { return };
            let Ok(r) = c.try_clone() else { continue };
            std::thread::spawn(move || {
                use std::io::{Read, Write};
                let mut c = c;
                let mut r = r;
                let mut buf = [0u8; 4096];
                loop {
                    match c.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            if r.write_all(&buf[..n]).is_err() {
                                break;
                            }
                        }
                    }
                }
            });
        }
    });
    (addr, h)
}

/// 临时状态目录。
pub(super) fn temp_dir(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!(
        "hw-carriers-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .subsec_nanos()
    ));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// 等条件成立（100ms 节拍；超时 panic 带标签）。
pub(super) fn eventually<F: FnMut() -> bool>(budget: Duration, tag: &str, mut f: F) {
    let deadline = std::time::Instant::now() + budget;
    while std::time::Instant::now() < deadline {
        if f() {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("等待超时：{tag}");
}
