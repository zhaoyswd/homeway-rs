//! 独立管理客户端；宿主只持同步流，不构造联网凭证或设备注册。
use std::io;
use std::net::{Shutdown, SocketAddr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use quinn::{Connection, Endpoint, EndpointConfig, VarInt};

use super::relay_sock::{ClientSock, RelayTable, SockStats};
use crate::web;
use crate::{RpkPublicKey, SvcStream, WebCredential};

pub struct WebDial {
    relay: SocketAddr,
    label: [u8; 8],
    pin: RpkPublicKey,
    credential: WebCredential,
}
impl WebDial {
    /// relay 必须是本机实际中继地址，wildcard 由宿主先归一。
    pub fn new(
        relay: SocketAddr,
        label: [u8; 8],
        pin: RpkPublicKey,
        credential: WebCredential,
    ) -> Self {
        Self {
            relay: crate::sysio::unmap_v4_mapped(relay),
            label,
            pin,
            credential,
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum WebClientError {
    #[error("管理连接 IO：{0}")]
    Io(#[from] io::Error),
    #[error("管理连接在预算内未完成")]
    Deadline,
    #[error("目标尚不支持 Web 管理或认证失败，请升级/核对凭证")]
    Authentication,
    #[error("目标管理资源忙")]
    Capacity,
    #[error("目标管理能力不可用")]
    Unavailable,
    #[error("中继管理腿确认失败")]
    Relay,
    #[error("目标管理协议非法")]
    Protocol,
    #[error("管理凭证已撤销")]
    Revoked,
    #[error("管理连接失败：{0}")]
    Transport(#[source] Box<dyn std::error::Error + Send + Sync>),
    #[error("管理宿主线程已结束")]
    Closed,
}

fn connection_error(error: quinn::ConnectionError) -> WebClientError {
    if let quinn::ConnectionError::ApplicationClosed(close) = &error {
        return match u64::from(close.error_code) {
            0x70 => WebClientError::Authentication,
            0x71 => WebClientError::Protocol,
            0x72 => WebClientError::Deadline,
            0x73 => WebClientError::Capacity,
            0x74 => WebClientError::Revoked,
            0x75 => WebClientError::Relay,
            0x76 => WebClientError::Unavailable,
            _ => WebClientError::Authentication,
        };
    }
    WebClientError::Transport(Box::new(error))
}

pub struct WebClient {
    stream: SvcStream,
    local: SocketAddr,
    cancel: Arc<AtomicBool>,
    alive: Arc<AtomicBool>,
    driver: Option<JoinHandle<()>>,
}
impl WebClient {
    pub fn connect(dial: WebDial, budget: Duration) -> Result<Self, WebClientError> {
        let budget = budget.min(Duration::from_secs(10));
        if budget.is_zero() {
            return Err(WebClientError::Deadline);
        }
        if dial.relay.ip().is_unspecified() || dial.relay.port() == 0 {
            return Err(WebClientError::Protocol);
        }
        let (stream, pump) = crate::sysio::pair()?;
        pump.set_nonblocking(true)?;
        let cancel = Arc::new(AtomicBool::new(false));
        let alive = Arc::new(AtomicBool::new(true));
        let stop = Arc::clone(&cancel);
        let living = Arc::clone(&alive);
        let (ready, receive) = mpsc::sync_channel(1);
        let started = Instant::now();
        let driver = thread::Builder::new().name("hw-web-quic".into()).spawn(move || {
            struct Exited(Arc<AtomicBool>);
            impl Drop for Exited { fn drop(&mut self) { self.0.store(false, Ordering::Release); } }
            let _exited = Exited(living);
            let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                Ok(runtime) => runtime,
                Err(error) => { let _ = ready.send(Err(WebClientError::Io(error))); return; }
            };
            runtime.block_on(async {
                let connecting = tokio::time::timeout(budget.saturating_sub(started.elapsed()), connect(dial));
                let result = tokio::select! {
                    result = connecting => result.map_err(|_| WebClientError::Deadline).and_then(|result| result),
                    _ = cancelled(&stop) => Err(WebClientError::Closed),
                };
                let (endpoint, conn, send, recv, local) = match result {
                    Ok(value) => value,
                    Err(error) => { let _ = ready.send(Err(error)); return; }
                };
                if ready.send(Ok(local)).is_err() { return; }
                #[cfg(unix)]
                let socket = tokio::net::UnixStream::from_std(pump);
                #[cfg(windows)]
                let socket = tokio::net::TcpStream::from_std(pump);
                let Ok(socket) = socket else { return; };
                let (mut read, mut write) = tokio::io::split(socket);
                let mut send = send;
                let mut recv = recv;
                tokio::select! {
                    _ = async { tokio::join!(crate::exit::pump::upstream(&mut recv, &mut write), crate::exit::pump::downstream(&mut read, &mut send)); } => {}
                    _ = cancelled(&stop) => {}
                    _ = conn.closed() => {}
                }
                conn.close(VarInt::from_u32(0), b"web client closed");
                endpoint.close(VarInt::from_u32(0), b"web client closed");
                // runtime 不能紧接close销毁：须让UDP驱动真正发出CONNECTION_CLOSE，
                // 否则目标只能等idle期限释放管理名额。
                let _ = tokio::time::timeout(Duration::from_millis(250), endpoint.wait_idle()).await;
            });
        })?;
        match receive.recv_timeout(budget.saturating_sub(started.elapsed())) {
            Ok(Ok(local)) => Ok(Self {
                stream,
                local,
                cancel,
                alive,
                driver: Some(driver),
            }),
            result => {
                cancel.store(true, Ordering::Release);
                let _ = stream.shutdown(Shutdown::Both);
                let _ = driver.join();
                match result {
                    Ok(Err(error)) => Err(error),
                    Err(mpsc::RecvTimeoutError::Timeout) => Err(WebClientError::Deadline),
                    _ => Err(WebClientError::Closed),
                }
            }
        }
    }
    pub fn stream(&self) -> &SvcStream {
        &self.stream
    }
    pub fn try_clone(&self) -> io::Result<SvcStream> {
        self.stream.try_clone()
    }
    pub fn local_addr(&self) -> SocketAddr {
        self.local
    }
    pub fn alive(&self) -> bool {
        self.alive.load(Ordering::Acquire)
    }
    pub fn is_closed(&self) -> bool {
        !self.alive()
    }
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Release);
        let _ = self.stream.shutdown(Shutdown::Both);
    }
}
impl Drop for WebClient {
    fn drop(&mut self) {
        self.cancel();
        if let Some(driver) = self.driver.take() {
            let _ = driver.join();
        }
    }
}

async fn cancelled(cancel: &AtomicBool) {
    while !cancel.load(Ordering::Acquire) {
        tokio::time::sleep(web::REVOKE_POLL).await;
    }
}

async fn connect(
    dial: WebDial,
) -> Result<
    (
        Endpoint,
        Connection,
        quinn::SendStream,
        quinn::RecvStream,
        SocketAddr,
    ),
    WebClientError,
> {
    let routes = RelayTable::new();
    routes.pin(dial.relay, dial.label);
    let stats = Arc::new(Mutex::new(SockStats::default()));
    let log: crate::Logf = Arc::new(|_| {});
    // 明确绑定目的本机IP，返回精确的中继 assoc 源地址；不输出 wildcard。
    let (socket, local) =
        ClientSock::open(SocketAddr::new(dial.relay.ip(), 0), routes, stats, log)?;
    let endpoint = Endpoint::new_with_abstract_socket(
        EndpointConfig::default(),
        None,
        socket,
        Arc::new(quinn::TokioRuntime),
    )?;
    let crypto = crate::exit::rpk::client_pin::client_crypto_config(dial.pin)
        .map_err(|e| WebClientError::Transport(Box::new(e)))?;
    let mut config = quinn::ClientConfig::new(crypto);
    config.transport_config(crate::exit::transport::transport_config_with(
        crate::exit::transport::INITIAL_MTU,
        crate::tuning::StreamLimits::design(),
        crate::tuning::CcChoice::default(),
    ));
    let connection = endpoint
        .connect_with(config, dial.relay, super::server_name())
        .map_err(|error| WebClientError::Transport(Box::new(error)))?
        .await
        .map_err(connection_error)?;
    let (mut send, mut recv) = connection.open_bi().await.map_err(connection_error)?;
    send.write_all(&web::hello(dial.credential.key_id()))
        .await
        .map_err(|error| WebClientError::Transport(Box::new(error)))?;
    let mut challenge = [0u8; web::CHALLENGE_LEN];
    if recv.read_exact(&mut challenge).await.is_err() {
        return Err(connection
            .close_reason()
            .map(connection_error)
            .unwrap_or(WebClientError::Authentication));
    }
    if &challenge[..3] != b"WC\x01" {
        return Err(WebClientError::Protocol);
    }
    let nonce: [u8; 16] = challenge[3..]
        .try_into()
        .map_err(|_| WebClientError::Protocol)?;
    let mut exporter = [0u8; 32];
    connection
        .export_keying_material(&mut exporter, web::EXPORTER_LABEL, b"")
        .map_err(|_| WebClientError::Protocol)?;
    send.write_all(&dial.credential.proof(&nonce, &exporter))
        .await
        .map_err(|error| WebClientError::Transport(Box::new(error)))?;
    let mut accepted = [0u8; 3];
    if recv.read_exact(&mut accepted).await.is_err() {
        return Err(connection
            .close_reason()
            .map(connection_error)
            .unwrap_or(WebClientError::Authentication));
    }
    if accepted != web::ACCEPT {
        return Err(WebClientError::Protocol);
    }
    Ok((endpoint, connection, send, recv, local))
}
