//! 唯一获准使用运行时的CLI Web控制面；与QUIC数据岛分开。
use std::net::TcpListener;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

use axum::{
    Extension, Router,
    body::{Body, to_bytes},
    extract::{
        State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    http::{Request, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use futures_util::SinkExt;
use hyper_util::{
    rt::{TokioIo, TokioTimer},
    service::TowerToHyperService,
};
use serde::Deserialize;
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use zeroize::Zeroizing;

use super::{
    Error,
    config::Config,
    host::{Gateway, Host},
    protocol::{self, Op},
};

pub(super) struct Server {
    cfg: Config,
    stop: Arc<AtomicBool>,
    alive: Arc<AtomicBool>,
    join: Option<std::thread::JoinHandle<()>>,
}
impl Server {
    pub fn start(host: Arc<Host>, cfg: Config) -> Result<Self, Error> {
        let listener = TcpListener::bind(("127.0.0.1", cfg.port))?;
        listener.set_nonblocking(true)?;
        let stop = Arc::new(AtomicBool::new(false));
        let alive = Arc::new(AtomicBool::new(true));
        let stop_t = stop.clone();
        let alive_t = alive.clone();
        let cfg_t = cfg.clone();
        let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(1);
        let join = std::thread::Builder::new()
            .name("homeway-web-http".into())
            .spawn(move || {
                struct Alive(Arc<AtomicBool>);
                impl Drop for Alive {
                    fn drop(&mut self) {
                        self.0.store(false, Ordering::Release);
                    }
                }
                let _alive = Alive(alive_t);
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .max_blocking_threads(8)
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(_) => {
                        let _ = ready_tx.send(false);
                        return;
                    }
                };
                runtime.block_on(async move {
                    let listener = match tokio::net::TcpListener::from_std(listener) {
                        Ok(listener) => listener,
                        Err(_) => {
                            let _ = ready_tx.send(false);
                            return;
                        }
                    };
                    let gateway = Gateway::new(host, stop_t.clone());
                    let state = App {
                        gateway: gateway.clone(),
                        cfg: cfg_t,
                        unauthed: Arc::new(Semaphore::new(8)),
                    };
                    let app = Router::new()
                        .route("/", get(index))
                        .route("/app.js", get(app_js))
                        .route("/style.css", get(style))
                        .route("/xterm.js", get(xterm))
                        .route("/xterm.css", get(xterm_css))
                        .route("/fit.js", get(fit))
                        .route("/unicode11.js", get(unicode11))
                        .route("/api/login", post(login))
                        .route("/ws", get(upgrade))
                        .layer(axum::extract::DefaultBodyLimit::max(4096))
                        .layer(middleware::from_fn_with_state(state.clone(), guard))
                        .with_state(state);
                    let connections = Arc::new(Semaphore::new(32));
                    let mut tasks = tokio::task::JoinSet::new();
                    let mut tick = tokio::time::interval(Duration::from_millis(100));
                    let _ = ready_tx.send(true);
                    loop {
                        tokio::select! {
                            accepted = listener.accept() => {
                                let Ok((socket, peer)) = accepted else { break; };
                                if !peer.ip().is_loopback() {
                                    continue;
                                }
                                let Ok(permit) = connections.clone().try_acquire_owned() else {
                                    continue;
                                };
                                let permit = Arc::new(permit);
                                let app = app.clone().layer(Extension(permit.clone()));
                                tasks.spawn(async move {
                                    let _permit = permit;
                                    let service = TowerToHyperService::new(app);
                                    let mut builder = hyper::server::conn::http1::Builder::new();
                                    builder.keep_alive(false)
                                        .max_buf_size(16384)
                                        .max_headers(32)
                                        .timer(TokioTimer::new())
                                        .header_read_timeout(Duration::from_secs(5));
                                    let _ = builder.serve_connection(TokioIo::new(socket), service)
                                        .with_upgrades().await;
                                });
                            }
                            _ = tick.tick() => {
                                gateway.cleanup();
                                if stop_t.load(Ordering::Acquire) { break; }
                            }
                            Some(_) = tasks.join_next(), if !tasks.is_empty() => {}
                        }
                    }
                    stop_t.store(true, Ordering::Release);
                    gateway.cleanup();
                    tasks.abort_all();
                    while tasks.join_next().await.is_some() {}
                });
                runtime.shutdown_timeout(Duration::from_secs(11));
            })?;
        if ready_rx.recv_timeout(Duration::from_secs(5)) != Ok(true) {
            stop.store(true, Ordering::Release);
            let _ = join.join();
            return Err(Error::Invalid("Web运行时启动失败"));
        }
        Ok(Self {
            cfg,
            stop,
            alive,
            join: Some(join),
        })
    }
    pub fn config(&self) -> &Config {
        &self.cfg
    }
    pub fn alive(&self) -> bool {
        self.alive.load(Ordering::Acquire)
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

#[derive(Clone)]
struct App {
    gateway: Arc<Gateway>,
    cfg: Config,
    unauthed: Arc<Semaphore>,
}

async fn guard(State(app): State<App>, request: Request<Body>, next: Next) -> Response {
    let hosts: Vec<_> = request.headers().get_all(header::HOST).iter().collect();
    if request.version() != axum::http::Version::HTTP_11
        || hosts.len() != 1
        || !hosts[0].to_str().is_ok_and(|h| app.cfg.permits_host(h))
    {
        return secure_response(StatusCode::FORBIDDEN.into_response());
    }
    if request.uri().query().is_some() {
        return secure_response(StatusCode::BAD_REQUEST.into_response());
    }
    let response = match tokio::time::timeout(Duration::from_secs(15), next.run(request)).await {
        Ok(r) => r,
        Err(_) => StatusCode::REQUEST_TIMEOUT.into_response(),
    };
    secure_response(response)
}

fn secure_response(mut response: Response) -> Response {
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    response.headers_mut().insert(header::CONTENT_SECURITY_POLICY,header::HeaderValue::from_static("default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; connect-src 'self'; object-src 'none'; base-uri 'none'; frame-ancestors 'none'; form-action 'self'"));
    response.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        header::HeaderValue::from_static("nosniff"),
    );
    response
}
pub(super) enum AuthorityCheck {
    Invalid,
    Local,
    PublicCandidate,
}

pub(super) fn check_authority(value: &str, port: u16) -> AuthorityCheck {
    if value.contains('@') {
        return AuthorityCheck::Invalid;
    }
    let Ok(authority) = value.parse::<axum::http::uri::Authority>() else {
        return AuthorityCheck::Invalid;
    };
    if (authority.host() == "127.0.0.1" || authority.host().eq_ignore_ascii_case("localhost"))
        && authority.port_u16().unwrap_or(80) == port
    {
        AuthorityCheck::Local
    } else {
        AuthorityCheck::PublicCandidate
    }
}

fn origin(app: &App, headers: &axum::http::HeaderMap) -> bool {
    let values: Vec<_> = headers.get_all(header::ORIGIN).iter().collect();
    values.len() == 1
        && values[0]
            .to_str()
            .is_ok_and(|value| app.cfg.permits_origin(value))
}
fn error(error: &Error) -> Response {
    use homeway_quic::WebClientError as Remote;
    let (status, code, message) = match error {
        Error::Token(super::credential::TokenError::UnsupportedVersion) => (
            StatusCode::BAD_REQUEST,
            "unsupported_version",
            "不支持的Web凭证版本，请升级homeway或重新获取凭证",
        ),
        Error::Token(super::credential::TokenError::Malformed) => (
            StatusCode::BAD_REQUEST,
            "malformed_token",
            "Web凭证格式无效，请完整复制凭证",
        ),
        Error::Busy | Error::Remote(Remote::Capacity) => (
            StatusCode::TOO_MANY_REQUESTS,
            "busy",
            "Web管理资源忙，请稍后重试",
        ),
        Error::Timeout => (StatusCode::REQUEST_TIMEOUT, "timeout", "Web管理请求超时"),
        Error::Remote(Remote::Deadline) => (
            StatusCode::GATEWAY_TIMEOUT,
            "timeout",
            "目标管理连接超时，请稍后重试",
        ),
        Error::Unauthorized | Error::Remote(Remote::Authentication | Remote::Revoked) => (
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "Web凭证认证失败或已撤销",
        ),
        Error::Remote(Remote::Protocol) => (
            StatusCode::BAD_REQUEST,
            "protocol_error",
            "目标管理协议不兼容，请升级目标主机",
        ),
        Error::Remote(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            "unavailable",
            "目标管理能力暂不可用，请检查目标与中继状态",
        ),
        _ => (
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "Web管理请求无效",
        ),
    };
    (status, axum::Json(json!({"code":code,"message":message}))).into_response()
}

fn terminal_error(data: &[u8]) -> (&'static str, &'static str, u16) {
    match data.first() {
        Some(2) => (
            "terminal_unavailable",
            "目标终端已禁用或不支持，请检查目标能力",
            1008,
        ),
        Some(3) => ("busy", "目标终端资源忙，请稍后重试", 1013),
        Some(4) => ("terminal_io", "目标终端进程或IO失败，请检查目标状态", 1011),
        Some(5) => (
            "control_unavailable",
            "目标本机控制面不可用，请检查统一进程",
            1011,
        ),
        Some(6) => ("revoked", "目标Web凭证已撤销，请重新获取凭证", 1008),
        _ => ("protocol_error", "目标终端协议非法，请升级目标主机", 1002),
    }
}

async fn login(State(app): State<App>, request: Request<Body>) -> Response {
    if !origin(&app, request.headers()) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let body =
        match tokio::time::timeout(Duration::from_secs(5), to_bytes(request.into_body(), 4096))
            .await
        {
            Ok(Ok(b)) => Zeroizing::new(b.to_vec()),
            _ => return StatusCode::BAD_REQUEST.into_response(),
        };
    let gateway = app.gateway.clone();
    let permit = match gateway.reserve() {
        Ok(p) => p,
        Err(e) => return error(&e),
    };
    let request_alive = Arc::new(AtomicBool::new(true));
    struct RequestGuard(Arc<AtomicBool>);
    impl Drop for RequestGuard {
        fn drop(&mut self) {
            self.0.store(false, Ordering::Release);
        }
    }
    let _request_guard = RequestGuard(request_alive.clone());
    let result = tokio::task::spawn_blocking(move || {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Login<'a> {
            token: &'a str,
        }
        let input: Login<'_> = serde_json::from_slice(&body)?;
        gateway.login(input.token, permit, &request_alive)
    })
    .await;
    match result {
        Ok(Ok(ticket)) => axum::Json(json!({"ticket":ticket,"expires_in":30})).into_response(),
        Ok(Err(e)) => error(&e),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

async fn upgrade(
    State(app): State<App>,
    Extension(permit): Extension<Arc<OwnedSemaphorePermit>>,
    headers: axum::http::HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    if !origin(&app, &headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Ok(unauthed) = app.unauthed.clone().try_acquire_owned() else {
        return StatusCode::TOO_MANY_REQUESTS.into_response();
    };
    ws.max_frame_size(65536)
        .max_message_size(65536)
        .on_upgrade(move |socket| serve_ws(socket, app.gateway, permit, unauthed))
}

fn discard_identified_ticket(gateway: &Gateway, text: &str) {
    #[derive(Deserialize)]
    struct Identified<'a> {
        #[serde(borrow)]
        ticket: Option<&'a str>,
    }
    if let Ok(Identified {
        ticket: Some(ticket),
    }) = serde_json::from_str(text)
    {
        gateway.discard(ticket);
    }
}

async fn serve_ws(
    mut socket: WebSocket,
    gateway: Arc<Gateway>,
    _connection: Arc<OwnedSemaphorePermit>,
    unauthed: OwnedSemaphorePermit,
) {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Auth {
        r#type: String,
        ticket: String,
        cols: u16,
        rows: u16,
    }
    let auth = match tokio::time::timeout(Duration::from_secs(5), socket.recv()).await {
        Ok(Some(Ok(Message::Text(text)))) if text.len() <= 2048 => {
            match serde_json::from_str::<Auth>(&text) {
                Ok(auth) => auth,
                Err(_) => {
                    discard_identified_ticket(&gateway, &text);
                    close_ws(&mut socket, 1002).await;
                    return;
                }
            }
        }
        Ok(Some(Err(error))) => {
            close_ws(&mut socket, ws_error_code(&error)).await;
            return;
        }
        Ok(Some(Ok(Message::Text(text)))) => {
            discard_identified_ticket(&gateway, &text);
            close_ws(&mut socket, 1009).await;
            return;
        }
        _ => {
            close_ws(&mut socket, 1008).await;
            return;
        }
    };
    if auth.r#type != "auth" || protocol::size(auth.cols, auth.rows).is_err() {
        gateway.discard(&auth.ticket);
        close_ws(&mut socket, 1008).await;
        return;
    }
    let endpoint = match gateway.consume(&auth.ticket) {
        Ok(endpoint) => endpoint,
        Err(_) => {
            close_ws(&mut socket, 1008).await;
            return;
        }
    };
    drop(unauthed);
    let stream = match endpoint.stream.try_clone().and_then(|s| {
        s.set_nonblocking(true)?;
        Ok(s)
    }) {
        Ok(s) => s,
        Err(_) => return,
    };
    let stream = match tokio::net::UnixStream::from_std(stream) {
        Ok(s) => s,
        Err(_) => return,
    };
    let (mut read, mut write) = stream.into_split();
    let mut open = vec![1];
    open.extend_from_slice(&protocol::size(auth.cols, auth.rows).expect("尺寸已验"));
    if send_frame(&mut write, Op::Open, &open).await.is_err() {
        return;
    }
    if socket
        .send(Message::Text("{\"type\":\"ready\"}".into()))
        .await
        .is_err()
    {
        return;
    }
    let (tx, mut rx) = tokio::sync::mpsc::channel::<(u8, Vec<u8>)>(16);
    let reader = tokio::spawn(async move {
        loop {
            let mut head = [0; 5];
            if read.read_exact(&mut head).await.is_err() {
                break;
            }
            let n = u32::from_be_bytes(head[1..].try_into().expect("定长头")) as usize;
            if n > 65536 {
                break;
            }
            let mut data = vec![0; n];
            if read.read_exact(&mut data).await.is_err() {
                break;
            }
            if tx.send((head[0], data)).await.is_err() {
                break;
            }
        }
    });
    let mut tick = tokio::time::interval(Duration::from_millis(250));
    let mut last = std::time::Instant::now();
    let mut ping = last;
    let mut close_code = 1000;
    loop {
        tokio::select! {
            incoming = socket.recv() => {
                match incoming {
                    Some(Ok(Message::Binary(data))) => {
                        last = std::time::Instant::now();
                        if data.is_empty() { close_code=1002; break; }
                        if send_frame(&mut write,Op::Input,&data).await.is_err() { close_code=1011; break; }
                    }
                    Some(Ok(Message::Text(text))) => {
                        last = std::time::Instant::now();
                        #[derive(Deserialize)]
                        #[serde(tag="type",deny_unknown_fields)]
                        enum Control {
                            #[serde(rename="close")] Close,
                            #[serde(rename="resize")] Resize { cols:u16, rows:u16 },
                        }
                        match serde_json::from_str::<Control>(&text) {
                            Ok(Control::Close) => break,
                            Ok(Control::Resize { cols,rows }) => {
                                let Ok(size)=protocol::size(cols,rows) else { close_code=1002; break; };
                                if send_frame(&mut write,Op::Resize,&size).await.is_err() { close_code=1011; break; }
                            }
                            Err(_) => { close_code=1002; break; }
                        }
                    }
                    Some(Ok(Message::Pong(_))) | Some(Ok(Message::Ping(_))) => last=std::time::Instant::now(),
                    Some(Err(error)) => { close_code=ws_error_code(&error); break; }
                    _ => break,
                }
            }
            output = rx.recv() => {
                let Some((op,data))=output else { close_code=1011; break; };
                match op {
                    4 => { if !send_ws(&mut socket,Message::Binary(data.into())).await { close_code=1011; break; } }
                    5 => break,
                    6 => {
                        let (code,message,close)=terminal_error(&data);
                        let _=send_ws(&mut socket,Message::Text(json!({"type":"error","code":code,"message":message}).to_string().into())).await;
                        close_code=close; break;
                    }
                    7 if data.len()==4 => {
                        let status=i32::from_be_bytes(data.try_into().expect("定长"));
                        let _=send_ws(&mut socket,Message::Text(json!({"type":"exit","status":status}).to_string().into())).await;
                        break;
                    }
                    _ => { close_code=1002; break; }
                }
            }
            _ = tick.tick() => {
                if gateway.stop.load(Ordering::Acquire) || !endpoint.valid() || last.elapsed()>Duration::from_secs(60) { close_code=1008; break; }
                if ping.elapsed()>Duration::from_secs(20) {
                    if !send_ws(&mut socket,Message::Ping(Vec::new().into())).await { close_code=1011; break; }
                    ping=std::time::Instant::now();
                }
            }
        }
    }
    reader.abort();
    let _ = write.shutdown().await;
    drop(endpoint);
    close_ws(&mut socket, close_code).await;
}

async fn send_ws(socket: &mut WebSocket, message: Message) -> bool {
    matches!(
        tokio::time::timeout(Duration::from_secs(10), socket.send(message)).await,
        Ok(Ok(()))
    )
}

fn ws_error_code(error: &axum::Error) -> u16 {
    match std::error::Error::source(error)
        .and_then(|source| source.downcast_ref::<tungstenite::Error>())
    {
        Some(tungstenite::Error::Capacity(_)) => 1009,
        Some(tungstenite::Error::Protocol(_)) => 1002,
        _ => 1011,
    }
}
async fn close_ws(socket: &mut WebSocket, code: u16) {
    let frame = axum::extract::ws::CloseFrame {
        code,
        reason: match code {
            1009 => "message too large",
            1008 => "authentication required",
            1002 => "invalid message",
            _ => "connection closed",
        }
        .into(),
    };
    let _ = tokio::time::timeout(
        Duration::from_millis(250),
        socket.send(Message::Close(Some(frame))),
    )
    .await;
    let _ = tokio::time::timeout(Duration::from_millis(250), socket.close()).await;
}

async fn send_frame(
    write: &mut tokio::net::unix::OwnedWriteHalf,
    op: Op,
    data: &[u8],
) -> Result<(), Error> {
    let mut frame = Vec::with_capacity(data.len() + 5);
    protocol::write(&mut frame, op, data)?;
    tokio::time::timeout(Duration::from_secs(10), write.write_all(&frame))
        .await
        .map_err(|_| Error::Timeout)??;
    Ok(())
}

fn asset(content: &'static str, content_type: &'static str) -> Response {
    ([(header::CONTENT_TYPE, content_type)], content).into_response()
}
async fn index() -> Response {
    asset(
        include_str!("assets/index.html"),
        "text/html; charset=utf-8",
    )
}
async fn app_js() -> Response {
    asset(
        include_str!("assets/app.js"),
        "text/javascript; charset=utf-8",
    )
}
async fn style() -> Response {
    asset(include_str!("assets/style.css"), "text/css; charset=utf-8")
}
async fn xterm() -> Response {
    asset(
        include_str!("assets/xterm.js"),
        "text/javascript; charset=utf-8",
    )
}
async fn xterm_css() -> Response {
    asset(include_str!("assets/xterm.css"), "text/css; charset=utf-8")
}
async fn fit() -> Response {
    asset(
        include_str!("assets/fit.js"),
        "text/javascript; charset=utf-8",
    )
}

async fn unicode11() -> Response {
    asset(
        include_str!("assets/unicode11.js"),
        "text/javascript; charset=utf-8",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    fn setup() -> (Arc<Host>, Server, u16) {
        let reserve = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = reserve.local_addr().unwrap().port();
        drop(reserve);
        let mut id = [0; 8];
        getrandom::getrandom(&mut id).unwrap();
        let root =
            std::env::temp_dir().join(format!("homeway-web-http-{:x}", u64::from_ne_bytes(id)));
        let host = Host::new(root, true).unwrap();
        let server = Server::start(
            host.clone(),
            Config {
                port,
                ..Config::default()
            },
        )
        .unwrap();
        (host, server, port)
    }
    fn raw(port: u16, text: &str) -> String {
        let mut stream = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(8)))
            .unwrap();
        stream.write_all(text.as_bytes()).unwrap();
        let mut out = String::new();
        let _ = stream.read_to_string(&mut out);
        out
    }
    #[test]
    fn remote_http_status_and_terminal_errors_keep_fixed_categories() {
        use homeway_quic::WebClientError as Remote;
        for (remote, expected) in [
            (Remote::Authentication, 401),
            (Remote::Revoked, 401),
            (Remote::Capacity, 429),
            (Remote::Deadline, 504),
            (Remote::Relay, 503),
            (Remote::Unavailable, 503),
            (Remote::Closed, 503),
            (Remote::Io(std::io::Error::other("hww1-do-not-echo")), 503),
            (Remote::Protocol, 400),
        ] {
            let response = error(&Error::Remote(remote));
            assert_eq!(response.status().as_u16(), expected);
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let bytes = runtime
                .block_on(to_bytes(response.into_body(), 4096))
                .unwrap();
            assert!(!String::from_utf8_lossy(&bytes).contains("hww1-do-not-echo"));
        }
        for (code, expected, close) in [
            (1, "protocol_error", 1002),
            (2, "terminal_unavailable", 1008),
            (3, "busy", 1013),
            (4, "terminal_io", 1011),
            (5, "control_unavailable", 1011),
            (6, "revoked", 1008),
        ] {
            let mut payload = vec![code];
            payload.extend_from_slice(b"hww1-do-not-echo");
            let (actual, message, actual_close) = terminal_error(&payload);
            assert_eq!((actual, actual_close), (expected, close));
            assert!(!message.contains("hww1"));
        }
    }

    #[test]
    fn invalid_auth_discards_only_unambiguously_identified_tickets() {
        let (host, server, _) = setup();
        let out = host.operation("webserve.token", json!({})).unwrap();
        let token = out["token"].as_str().unwrap();
        let gateway = Gateway::new(host.clone(), Arc::new(AtomicBool::new(false)));
        let ticket = gateway
            .login(token, gateway.reserve().unwrap(), &AtomicBool::new(true))
            .unwrap();
        discard_identified_ticket(
            &gateway,
            &format!("{{\"ticket\":\"{ticket}\",\"cols\":\"invalid\"}}"),
        );
        assert!(gateway.consume(&ticket).is_err());
        let ticket = gateway
            .login(token, gateway.reserve().unwrap(), &AtomicBool::new(true))
            .unwrap();
        discard_identified_ticket(&gateway, &format!("{{\"ticket\":\"{ticket}\""));
        // 语法损坏时不能猜归属；仍占有一个既有30秒有界票据，而非误删。
        assert_eq!(gateway.count.load(Ordering::Acquire), 1);
        gateway.discard(&ticket);
        assert_eq!(gateway.count.load(Ordering::Acquire), 0);
        drop(server);
        host.shutdown();
        std::fs::remove_dir_all(&host.state).unwrap();
    }

    #[test]
    fn real_http_checks_host_origin_body_and_static_csp() {
        let (host, server, port) = setup();
        let out = raw(
            port,
            &format!("GET / HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n"),
        );
        assert!(out.starts_with("HTTP/1.1 200"));
        assert!(out.to_lowercase().contains("content-security-policy"));
        assert!(out.contains("Web 管理凭证"));
        for request in [
            "GET / HTTP/1.1\r\nHost: user@127.0.0.1:18080\r\n\r\n".to_owned(),
            format!("GET /?token=forbidden HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n"),
        ] {
            let denied = raw(port, &request).to_lowercase();
            assert!(denied.starts_with("http/1.1 403") || denied.starts_with("http/1.1 400"));
            assert!(denied.contains("cache-control: no-store"));
            assert!(denied.contains("content-security-policy:"));
            assert!(denied.contains("x-content-type-options: nosniff"));
        }

        assert!(
            raw(port, "GET / HTTP/1.1\r\nHost: evil.invalid\r\n\r\n").starts_with("HTTP/1.1 403")
        );
        assert!(raw(port,&format!("POST /api/login HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nOrigin: https://evil.invalid\r\nContent-Length: 2\r\n\r\n{{}}")).starts_with("HTTP/1.1 403"));
        assert!(raw(port,&format!("POST /api/login HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nOrigin: http://127.0.0.1:{port}\r\nContent-Length: 5000\r\n\r\n{}","x".repeat(5000))).starts_with("HTTP/1.1 400"));
        drop(server);
        host.shutdown();
        let _ = std::fs::remove_dir_all(&host.state);
    }
    #[test]
    fn websocket_capacity_classification_is_typed() {
        let e = axum::Error::new(tungstenite::Error::Capacity(
            tungstenite::error::CapacityError::MessageTooLong {
                size: 65537,
                max_size: 65536,
            },
        ));
        assert_eq!(ws_error_code(&e), 1009);
    }
}
