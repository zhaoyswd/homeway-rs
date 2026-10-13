//! 桥接流的**传输中立**抽象（WIN 批 2026-10-13 从 `bridge_host` 拆出）。
//!
//! 拆出动机：`BridgeStream`/`WriteHalf` 两个 trait 与「出口虚拟端口」常量被
//! **跨平台面**消费（`host_session`/`quic_stream`/`portfwd`——CLI/桌面 daemon 的
//! 会话与承载面），而 `bridge_host` 本体（UDS 三桥的监听/装配）是 App 面、在
//! windows 上不装配（`docs/reviews/WIN-design.md` §D3）。中立件落本模块；桥装配
//! 面留在 `bridge_host`（`#[cfg(not(windows))]`）。

use std::io::{Read, Write};

/// 桥接流抽象（工单⑤ dial_port 接缝：远端可以是本机 UDS（服务会话直拨形态），
/// 也可以是「经隧道会话的流 id」——TunnelExec 注入 SessionConn）。
pub trait BridgeStream: Send {
    /// 拆成读/写两半（两半可在不同线程并发使用——泵模型的前提；UDS = try_clone，
    /// 会话流 = 各自持 `Arc<Client> + id` 的半句柄）。
    fn into_halves(
        self: Box<Self>,
    ) -> std::io::Result<(Box<dyn Read + Send>, Box<dyn WriteHalf + Send>)>;
}

/// 单向写端的半关闭（EOF 传播：UDS = shutdown(WRITE)；会话流 = 发 FIN——
/// files 的「write 关流即取消」靠它）。
pub trait WriteHalf: Write {
    fn close_write(&mut self);
}

/// UDS 流的两半与半关闭（unix 面——windows 不装配桥，无该 impl）。
#[cfg(unix)]
impl BridgeStream for std::os::unix::net::UnixStream {
    fn into_halves(
        self: Box<Self>,
    ) -> std::io::Result<(Box<dyn Read + Send>, Box<dyn WriteHalf + Send>)> {
        let r = (*self).try_clone()?;
        let w: Box<dyn WriteHalf + Send> = self;
        Ok((Box::new(r), w))
    }
}

#[cfg(unix)]
impl WriteHalf for std::os::unix::net::UnixStream {
    fn close_write(&mut self) {
        let _ = self.shutdown(std::net::Shutdown::Write);
    }
}

/// 本机 TCP 流的半关闭（Q-F-B F2-3/N3：portfwd 的本地监听口是 `TcpStream`，泵复用
/// 需要它也能传播 FIN——`shutdown(Write)` 与 UDS 形态同义）。
impl WriteHalf for std::net::TcpStream {
    fn close_write(&mut self) {
        let _ = self.shutdown(std::net::Shutdown::Write);
    }
}

/// 会话流的读写两半统一包装面（读半纯 `dyn Read`；写半 `dyn WriteHalf`）。
impl WriteHalf for Box<dyn WriteHalf + Send> {
    fn close_write(&mut self) {
        (**self).close_write()
    }
}

/// 出口虚拟端口（桥那头要拨到的**服务标识**；M3 起 QUIC 档不再拨端口，而是按
/// 这些端口选出 `STREAM[tag]`——映射单源 = `facade/quic_stream.rs::tag_for_port`；
/// 端口号本身仍是**配置面**（E1 的值域/字段不变））。
pub mod port {
    /// files 原生协议（app_files_native.go filesNativePort；QUIC 档 ⇒ `STREAM[tag=1]`）。
    pub const FILES: u16 = 7802;
    /// term 服务（HOMEWAY_TERM_PORT 可改——与本常量同源环境变量；QUIC 档 ⇒ `STREAM[tag=2]`）。
    pub const TERM: u16 = 7724;
    /// speedtest 服务（app_speedtest.go speedtestServicePort；QUIC 档 ⇒ `STREAM[tag=3]`）。
    pub const SPEEDTEST: u16 = 7803;
    /// 客户端远程解析腿（**clientcore 写死 5300**；Go `socksDNSPort` 同值；QUIC 档 ⇒
    /// `STREAM[tag=6]`（dns-resolve，DNS-STREAM 批）。单源引用方 = `daemon/carriers/dnsq.rs`
    /// 的 `SOCKS_DNS_PORT`（别再各写一份字面 5300）。
    pub const DNS_RESOLVE: u16 = 5300;
}

/// term 端口的可变面（HOMEWAY_TERM_PORT 同源；测试/特殊部署用）。
pub fn term_port() -> u16 {
    std::env::var("HOMEWAY_TERM_PORT")
        .ok()
        .and_then(|v| v.parse::<u16>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(port::TERM)
}
