use std::collections::HashMap;
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering},
};
use std::time::{Duration, Instant};

use base64::Engine;
use homeway_core::relay::registry::RegistryHandle;
use homeway_quic::{ServiceIntake, WebAuth, WebClient, WebDial, WebGrant, WebManagementConfig};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{
    Error,
    config::Config,
    credential::{self, Identity, Token},
    http, pty,
};

#[cfg(test)]
thread_local! { static FAIL_RECONCILE_READ: std::cell::Cell<bool> = const { std::cell::Cell::new(false) }; }

fn read_committed_credential(
    state: &std::path::Path,
) -> Result<homeway_quic::WebCredential, Error> {
    #[cfg(test)]
    if FAIL_RECONCILE_READ.with(std::cell::Cell::get) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "test-only committed credential read failure",
        )
        .into());
    }
    credential::read(state)?.ok_or(Error::Invalid("新凭证文件不可见"))
}

#[derive(Default)]
pub(crate) struct RuntimeInfo {
    pub serve_on: bool,
    pub registry: Option<RegistryHandle>,
}
type RuntimeProvider = Arc<dyn Fn() -> RuntimeInfo + Send + Sync>;

pub(crate) struct Host {
    pub state: PathBuf,
    pub auth: WebAuth,
    pub terminal_available: bool,
    identity: Mutex<Option<Identity>>,
    credential_lock: Mutex<()>,
    management: WebManagementConfig,
    stop: Arc<AtomicBool>,
    count: Arc<AtomicUsize>,
    reaper: RemoteReaper,
    runtime: Mutex<Option<RuntimeProvider>>,
    phase: AtomicU8,
    action: Mutex<()>,
    server: Mutex<Option<http::Server>>,
    reason: Mutex<Option<String>>,
    agent: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl Host {
    pub fn new(state: PathBuf, terminal_available: bool) -> Result<Arc<Self>, Error> {
        let auth = WebAuth::empty();
        let saved = credential::read(&state)?;
        let identity = if let Some(saved) = saved {
            auth.install(saved);
            Some(Identity::load(&state)?)
        } else {
            None
        };
        let (intake, tx) = ServiceIntake::quic_only(8)?;
        let stop = Arc::new(AtomicBool::new(false));
        let count = Arc::new(AtomicUsize::new(0));
        let host = Arc::new(Self {
            state: state.clone(),
            auth: auth.clone(),
            terminal_available,
            identity: Mutex::new(identity),
            credential_lock: Mutex::new(()),
            management: WebManagementConfig::new(auth, tx),
            stop: stop.clone(),
            count: count.clone(),
            reaper: RemoteReaper::new()?,
            runtime: Mutex::new(None),
            phase: AtomicU8::new(0),
            action: Mutex::new(()),
            server: Mutex::new(None),
            reason: Mutex::new(None),
            agent: Mutex::new(None),
        });
        let agent = std::thread::Builder::new()
            .name("homeway-web-agent".into())
            .spawn(move || {
                while !stop.load(Ordering::Acquire) {
                    match intake.accept() {
                        Ok(conn) => {
                            if terminal_available {
                                let _ = pty::launch(state.clone(), conn, stop.clone(), &count);
                            }
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                        Err(_) => break,
                    }
                }
            })?;
        *host.agent.lock().unwrap_or_else(|e| e.into_inner()) = Some(agent);
        Ok(host)
    }
    pub fn management_config(&self) -> WebManagementConfig {
        self.management.clone()
    }
    pub fn set_runtime(&self, provider: RuntimeProvider) {
        *self.runtime.lock().unwrap_or_else(|e| e.into_inner()) = Some(provider);
    }
    pub fn runtime(&self) -> RuntimeInfo {
        self.runtime
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map_or_else(RuntimeInfo::default, |p| p())
    }
    pub fn bootstrap(self: &Arc<Self>) -> Result<(), Error> {
        if Config::load(&self.state)?.enabled {
            self.operation("webserve.start", json!({}))?;
        }
        Ok(())
    }
    pub fn shutdown(&self) {
        if let Some(server) = self.server.lock().unwrap_or_else(|e| e.into_inner()).take() {
            drop(server);
        }
        self.stop.store(true, Ordering::Release);
        let agent = self.agent.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(agent) = agent {
            let _ = agent.join();
        }
    }
    pub fn operation(self: &Arc<Self>, op: &str, args: Value) -> Result<Value, Error> {
        if matches!(op, "webserve.token" | "webserve.token.rotate") {
            let _lock = self
                .credential_lock
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let identity = Identity::load(&self.state)?;
            let credential = match credential::obtain(&self.state, op.ends_with(".rotate")) {
                Ok(credential) => credential,
                Err(error) => {
                    if matches!(&error, Error::Persistence(persistence) if persistence.commit_visible())
                    {
                        // rename已发生：与可见磁盘重对账，仍返回耐久性错误；不能当未提交。
                        let recovered = read_committed_credential(&self.state);
                        match recovered {
                            Ok(credential) => {
                                self.auth.install(credential);
                                *self.identity.lock().unwrap_or_else(|e| e.into_inner()) =
                                    Some(identity);
                            }
                            Err(source) => {
                                self.auth.clear();
                                *self.identity.lock().unwrap_or_else(|e| e.into_inner()) = None;
                                return Err(Error::CredentialRecovery {
                                    source: Box::new(source),
                                });
                            }
                        }
                    }
                    return Err(error);
                }
            };
            self.auth.install(credential.clone());
            *self.identity.lock().unwrap_or_else(|e| e.into_inner()) = Some(identity);
            let mut token = Token {
                identity,
                credential,
            }
            .encode();
            let mut result = serde_json::Map::new();
            result.insert("token".into(), Value::String(std::mem::take(&mut *token)));
            return Ok(Value::Object(result));
        }
        if op == "webserve.status" {
            return self.status();
        }
        let _action = self.action.lock().unwrap_or_else(|e| e.into_inner());
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Params {
            port: Option<u16>,
            public_origin: Option<String>,
        }
        let args: Params = serde_json::from_value(args)?;
        let mut cfg = Config::load(&self.state)?;
        if let Some(port) = args.port {
            cfg.port = port;
        }
        if let Some(origin) = args.public_origin {
            cfg.public_origin = Some(origin);
        }
        cfg.validate()?;
        if op == "webserve.stop" {
            cfg.enabled = false;
            cfg.save(&self.state)?;
            let old = self.server.lock().unwrap_or_else(|e| e.into_inner()).take();
            drop(old);
            *self.reason.lock().unwrap_or_else(|e| e.into_inner()) = None;
            self.phase.store(0, Ordering::Release);
            return self.status();
        }
        if !matches!(op, "webserve.start" | "webserve.restart") {
            return Err(Error::Invalid("未知Web控制操作"));
        }
        if !self.terminal_available {
            return Err(Error::Invalid(
                "本机终端能力不可用，请检查HOMEWAY_TERM和统一进程版本",
            ));
        }
        if op == "webserve.start" {
            if let Some(server) = self
                .server
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .as_ref()
            {
                if server.alive() {
                    if server.config() != &cfg && {
                        let mut active = server.config().clone();
                        active.enabled = cfg.enabled;
                        active != cfg
                    } {
                        return Err(Error::Invalid("Web入口已运行且配置不同，请使用restart"));
                    }
                    return Ok(
                        json!({"action":"already","listen":format!("127.0.0.1:{}",cfg.port)}),
                    );
                }
            }
        }
        let old = self.server.lock().unwrap_or_else(|e| e.into_inner()).take();
        drop(old);
        cfg.enabled = true;
        cfg.save(&self.state)?;
        self.phase.store(1, Ordering::Release);
        match http::Server::start(self.clone(), cfg) {
            Ok(server) => {
                *self.server.lock().unwrap_or_else(|e| e.into_inner()) = Some(server);
                *self.reason.lock().unwrap_or_else(|e| e.into_inner()) = None;
                self.phase.store(2, Ordering::Release);
            }
            Err(e) => {
                *self.reason.lock().unwrap_or_else(|e| e.into_inner()) = Some(e.to_string());
                self.phase.store(3, Ordering::Release);
                return Err(e);
            }
        }
        self.status()
    }
    pub fn status(&self) -> Result<Value, Error> {
        let cfg = Config::load(&self.state)?;
        let server = self.server.lock().unwrap_or_else(|e| e.into_inner());
        let running = server.as_ref().is_some_and(http::Server::alive);
        let reason = self
            .reason
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let info = self.runtime();
        let registry = info.registry.as_ref().filter(|r| r.address().is_some());
        Ok(
            json!({"enabled":cfg.enabled,"state":if running{"running"}else if self.phase.load(Ordering::Acquire)==1{"starting"}else if reason.is_some()||self.phase.load(Ordering::Acquire)==2{"failed"}else{"disabled"},"listen":if running{Some(format!("127.0.0.1:{}",server.as_ref().expect("已验").config().port))}else{None},"port":cfg.port,"public_origin":cfg.public_origin,"reason":reason,"local_tui_available":self.terminal_available,"remote_agent_available":self.terminal_available&&info.serve_on&&self.auth.is_configured(),"relay_gateway_available":registry.is_some(),"relay_epoch":registry.map(RegistryHandle::epoch),"relay_counts":registry.map(RegistryHandle::counts),"ui_count":self.count.load(Ordering::Acquire)}),
        )
    }
    fn local(&self, grant: WebGrant) -> Result<Target, Error> {
        if !self.terminal_available {
            return Err(Error::Invalid("目标终端不可用"));
        }
        Ok(Target::Local(grant))
    }
    fn authenticate(&self, text: &str) -> Result<Target, Error> {
        let token = Token::decode(text)?;
        if self
            .identity
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            == Some(&token.identity)
        {
            return self.local(
                self.auth
                    .authenticate(
                        token.credential.key_id(),
                        token.credential.secret().as_bytes(),
                    )
                    .ok_or(Error::Unauthorized)?,
            );
        }
        let registry = self.runtime().registry.ok_or(Error::Unauthorized)?;
        let (address, label) = registry
            .lookup(&token.identity.public_key)
            .ok_or(Error::Unauthorized)?;
        let epoch = registry.epoch();
        let lifetime = pty::Permit::acquire(&self.reaper.count)?;
        let client = WebClient::connect(
            WebDial::new(address, label, token.identity.pin, token.credential),
            Duration::from_secs(10),
        )?;
        Ok(Target::Remote(Remote {
            client: Some(client),
            lifetime: Some(lifetime),
            reaper: self.reaper.clone(),
            registry,
            epoch,
            label,
            public_key: token.identity.public_key,
        }))
    }
    fn endpoint(&self, target: Target, permit: pty::Permit) -> Result<Endpoint, Error> {
        if !target.valid() {
            return Err(Error::Unauthorized);
        }
        let stream = match &target {
            Target::Local(_) => {
                let (a, b) = UnixStream::pair()?;
                pty::launch(self.state.clone(), b, self.stop.clone(), &self.count)?;
                a
            }
            Target::Remote(remote) => remote
                .client
                .as_ref()
                .ok_or(Error::Unauthorized)?
                .try_clone()?,
        };
        Ok(Endpoint {
            stream,
            target,
            _permit: permit,
        })
    }
}

#[derive(Clone)]
struct RemoteReaper {
    count: Arc<AtomicUsize>,
    tx: std::sync::mpsc::SyncSender<ReapJob>,
}
struct ReapJob {
    client: WebClient,
    permit: pty::Permit,
    registry: RegistryHandle,
    epoch: u64,
    label: [u8; 8],
    source: std::net::SocketAddr,
    cutoff: Instant,
}
impl RemoteReaper {
    fn new() -> Result<Self, Error> {
        let (tx, rx) = std::sync::mpsc::sync_channel::<ReapJob>(8);
        std::thread::Builder::new()
            .name("homeway-web-reap".into())
            .spawn(move || {
                while let Ok(job) = rx.recv() {
                    let ReapJob {
                        client,
                        permit,
                        registry,
                        epoch,
                        label,
                        source,
                        cutoff,
                    } = job;
                    // 保留中继路径供CONNECTION_CLOSE刷出，再按原关闭时刻精确销账。
                    drop(client);
                    if !registry.release_association(epoch, label, source, cutoff)
                        && registry.address().is_some()
                    {
                        eprintln!("Web管理中继销账队列暂忙，将由有界回收补偿");
                    }
                    drop(permit);
                }
            })?;
        Ok(Self {
            count: Arc::new(AtomicUsize::new(0)),
            tx,
        })
    }
}

pub(super) enum Target {
    Local(WebGrant),
    Remote(Remote),
}
impl Target {
    pub fn valid(&self) -> bool {
        match self {
            Self::Local(g) => g.is_valid(),
            Self::Remote(r) => {
                r.client.as_ref().is_some_and(WebClient::alive)
                    && r.registry.epoch() == r.epoch
                    && r.registry.contains(&r.public_key)
            }
        }
    }
}
pub(super) struct Remote {
    client: Option<WebClient>,
    lifetime: Option<pty::Permit>,
    reaper: RemoteReaper,
    registry: RegistryHandle,
    epoch: u64,
    label: [u8; 8],
    public_key: [u8; 32],
}
impl Drop for Remote {
    fn drop(&mut self) {
        if let (Some(client), Some(permit)) = (self.client.take(), self.lifetime.take()) {
            let source = client.local_addr();
            let cutoff = Instant::now();
            let job = ReapJob {
                client,
                permit,
                registry: self.registry.clone(),
                epoch: self.epoch,
                label: self.label,
                source,
                cutoff,
            };
            // 活动及待回收连接合计8，正常队列不可能满；同步join只在reaper线程。
            if self.reaper.tx.try_send(job).is_err() {
                eprintln!("Web连接回收worker已终止");
            }
        }
    }
}

pub(super) struct Endpoint {
    pub stream: UnixStream,
    target: Target,
    _permit: pty::Permit,
}
impl Endpoint {
    pub fn valid(&self) -> bool {
        self.target.valid()
    }
}
impl Drop for Endpoint {
    fn drop(&mut self) {
        let _ = self.stream.shutdown(Shutdown::Both);
    }
}

pub(super) struct Ticket {
    pub target: Target,
    pub permit: pty::Permit,
    pub deadline: Instant,
}
pub(super) struct Gateway {
    pub host: Arc<Host>,
    pub stop: Arc<AtomicBool>,
    pub count: Arc<AtomicUsize>,
    tickets: Mutex<HashMap<[u8; 32], Ticket>>,
    rate: Mutex<(Instant, u32)>,
}
impl Gateway {
    pub fn new(host: Arc<Host>, stop: Arc<AtomicBool>) -> Arc<Self> {
        Arc::new(Self {
            host,
            stop,
            count: Arc::new(AtomicUsize::new(0)),
            tickets: Mutex::new(HashMap::new()),
            rate: Mutex::new((Instant::now(), 0)),
        })
    }
    pub fn reserve(&self) -> Result<pty::Permit, Error> {
        {
            let mut rate = self.rate.lock().unwrap_or_else(|e| e.into_inner());
            if rate.0.elapsed() >= Duration::from_secs(60) {
                *rate = (Instant::now(), 0);
            }
            if rate.1 >= 60 {
                return Err(Error::Busy);
            }
            rate.1 += 1;
        }
        pty::Permit::acquire(&self.count)
    }
    pub fn login(
        &self,
        text: &str,
        permit: pty::Permit,
        request_alive: &AtomicBool,
    ) -> Result<String, Error> {
        let target = self.host.authenticate(text)?;
        if !request_alive.load(Ordering::Acquire)
            || self.stop.load(Ordering::Acquire)
            || !target.valid()
        {
            return Err(Error::Unauthorized);
        }
        let mut ticket = [0u8; 32];
        getrandom::getrandom(&mut ticket).map_err(|_| Error::Random)?;
        self.tickets
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(
                ticket,
                Ticket {
                    target,
                    permit,
                    deadline: Instant::now() + Duration::from_secs(30),
                },
            );
        Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(ticket))
    }
    pub fn consume(&self, text: &str) -> Result<Endpoint, Error> {
        let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(text)
            .map_err(|_| Error::Unauthorized)?;
        let key: [u8; 32] = raw.try_into().map_err(|_| Error::Unauthorized)?;
        let ticket = self
            .tickets
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&key)
            .ok_or(Error::Unauthorized)?;
        if ticket.deadline <= Instant::now() || self.stop.load(Ordering::Acquire) {
            return Err(Error::Unauthorized);
        }
        self.host.endpoint(ticket.target, ticket.permit)
    }
    pub fn discard(&self, text: &str) {
        if let Ok(bytes) = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(text) {
            if let Ok(key) = <[u8; 32]>::try_from(bytes) {
                let ticket = self
                    .tickets
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&key);
                drop(ticket);
            }
        }
    }
    pub fn cleanup(&self) {
        let mut expired = Vec::new();
        {
            let mut tickets = self.tickets.lock().unwrap_or_else(|e| e.into_inner());
            let keys: Vec<_> = tickets
                .iter()
                .filter(|(_, t)| {
                    t.deadline <= Instant::now()
                        || !t.target.valid()
                        || self.stop.load(Ordering::Acquire)
                })
                .map(|(k, _)| *k)
                .collect();
            for key in keys {
                if let Some(t) = tickets.remove(&key) {
                    expired.push(t);
                }
            }
        }
        drop(expired);
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(agent) = self
            .agent
            .get_mut()
            .unwrap_or_else(|e| e.into_inner())
            .take()
        {
            let _ = agent.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn host() -> Arc<Host> {
        let mut id = [0; 8];
        getrandom::getrandom(&mut id).unwrap();
        Host::new(
            std::env::temp_dir().join(format!("homeway-web-host-{:x}", u64::from_ne_bytes(id))),
            true,
        )
        .unwrap()
    }
    fn token(host: &Arc<Host>, rotate: bool) -> String {
        host.operation(
            if rotate {
                "webserve.token.rotate"
            } else {
                "webserve.token"
            },
            json!({}),
        )
        .unwrap()["token"]
            .as_str()
            .unwrap()
            .to_owned()
    }
    #[test]
    fn credential_commit_failures_preserve_or_reconcile_live_auth_explicitly() {
        use credential::{FailurePoint, with_write_failure};
        let host = host();
        let old = Token::decode(&token(&host, false)).unwrap();
        let old_grant = host
            .auth
            .authenticate(old.credential.key_id(), old.credential.secret().as_bytes())
            .unwrap();
        let disk = zeroize::Zeroizing::new(std::fs::read(host.state.join("web/auth.key")).unwrap());
        let before = with_write_failure(FailurePoint::BeforeRename, || {
            host.operation("webserve.token.rotate", json!({}))
        });
        assert!(matches!(before, Err(Error::Persistence(ref e)) if !e.commit_visible()));
        assert!(old_grant.is_valid());
        assert_eq!(
            &*disk,
            &std::fs::read(host.state.join("web/auth.key")).unwrap()
        );
        let committed = with_write_failure(FailurePoint::DirectorySync, || {
            host.operation("webserve.token.rotate", json!({}))
        });
        assert!(matches!(committed, Err(Error::Persistence(ref e)) if e.commit_visible()));
        assert!(!old_grant.is_valid());
        let visible = credential::read(&host.state).unwrap().unwrap();
        let visible_grant = host
            .auth
            .authenticate(visible.key_id(), visible.secret().as_bytes())
            .unwrap();
        FAIL_RECONCILE_READ.with(|flag| flag.set(true));
        let failed = with_write_failure(FailurePoint::DirectorySync, || {
            host.operation("webserve.token.rotate", json!({}))
        });
        FAIL_RECONCILE_READ.with(|flag| flag.set(false));
        assert!(matches!(failed, Err(Error::CredentialRecovery { .. })));
        assert!(!visible_grant.is_valid());
        assert!(
            !host.auth.is_configured(),
            "提交后无法重读必须fail closed，不能保旧授权"
        );
        let _ = token(&host, false);
        assert!(
            host.auth.is_configured(),
            "修复IO后重新获取可恢复当前磁盘凭证"
        );
        host.shutdown();
        assert!(host.agent.lock().unwrap().is_none());
        std::fs::remove_dir_all(&host.state).unwrap();
    }

    #[test]
    fn idle_agent_shutdown_joins_without_an_intake_connection() {
        let host = host();
        let started = Instant::now();
        host.shutdown();
        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(host.agent.lock().unwrap().is_none());
        let _ = std::fs::remove_dir_all(&host.state);
    }

    #[test]
    fn local_tickets_share_eight_slots_expire_and_revoke() {
        let host = host();
        let token = token(&host, false);
        let gateway = Gateway::new(host.clone(), Arc::new(AtomicBool::new(false)));
        let alive = AtomicBool::new(true);
        let mut tickets = Vec::new();
        for _ in 0..8 {
            tickets.push(
                gateway
                    .login(&token, gateway.reserve().unwrap(), &alive)
                    .unwrap(),
            );
        }
        assert!(gateway.reserve().is_err());
        assert_eq!(gateway.count.load(Ordering::Acquire), 8);
        for ticket in gateway.tickets.lock().unwrap().values_mut() {
            ticket.deadline = Instant::now() - Duration::from_secs(1);
        }
        gateway.cleanup();
        assert_eq!(gateway.count.load(Ordering::Acquire), 0);
        assert!(gateway.consume(&tickets[0]).is_err());
        let ticket = gateway
            .login(&token, gateway.reserve().unwrap(), &alive)
            .unwrap();
        let _ = self::token(&host, true);
        gateway.cleanup();
        assert!(gateway.consume(&ticket).is_err());
        assert_eq!(gateway.count.load(Ordering::Acquire), 0);
        host.shutdown();
        std::fs::remove_dir_all(&host.state).unwrap();
    }
    #[test]
    fn remote_token_is_not_dialed_without_live_relay() {
        let first = host();
        let second = host();
        let first_token = token(&first, false);
        let second_token = token(&second, false);
        assert!(first.authenticate(&first_token).is_ok());
        assert!(matches!(
            first.authenticate(&second_token),
            Err(Error::Unauthorized)
        ));
        first.shutdown();
        second.shutdown();
        std::fs::remove_dir_all(&first.state).unwrap();
        std::fs::remove_dir_all(&second.state).unwrap();
    }
    #[test]
    fn cancelling_login_never_leaves_a_ticket() {
        let host = host();
        let token = token(&host, false);
        let gateway = Gateway::new(host.clone(), Arc::new(AtomicBool::new(false)));
        assert!(
            gateway
                .login(&token, gateway.reserve().unwrap(), &AtomicBool::new(false))
                .is_err()
        );
        assert_eq!(gateway.count.load(Ordering::Acquire), 0);
        assert!(gateway.tickets.lock().unwrap().is_empty());
        host.shutdown();
        std::fs::remove_dir_all(&host.state).unwrap();
    }

    #[test]
    fn web_configuration_does_not_mutate_old_config_and_restart_is_explicit() {
        let host = host();
        std::fs::create_dir_all(&host.state).unwrap();
        let original = "[serve]\nenabled = false\n[relay]\nenabled = false\n";
        std::fs::write(host.state.join("config.toml"), original).unwrap();
        let _ = token(&host, false);
        let occupied = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = occupied.local_addr().unwrap().port();
        assert!(
            host.operation("webserve.start", json!({"port":port}))
                .is_err()
        );
        assert_eq!(host.status().unwrap()["state"], "failed");
        let stopped = host.operation("webserve.stop", json!({})).unwrap();
        assert_eq!(stopped["state"], "disabled");
        assert!(stopped["reason"].is_null());
        drop(occupied);
        assert_eq!(
            host.operation("webserve.start", json!({"port":port}))
                .unwrap()["state"],
            "running"
        );
        assert_eq!(
            host.operation("webserve.start", json!({"port":port}))
                .unwrap()["action"],
            "already"
        );
        let before = std::fs::read(host.state.join("webserve.toml")).unwrap();
        assert!(
            host.operation(
                "webserve.start",
                json!({"port":if port==65535{port-1}else{port+1}})
            )
            .is_err()
        );
        assert_eq!(
            std::fs::read(host.state.join("webserve.toml")).unwrap(),
            before
        );
        host.operation("webserve.stop", json!({})).unwrap();
        assert_eq!(host.status().unwrap()["state"], "disabled");
        assert_eq!(
            std::fs::read_to_string(host.state.join("config.toml")).unwrap(),
            original
        );
        assert!(
            !crate::serve_cli::load_config_strict(&host.state)
                .unwrap()
                .serve
                .enabled
        );
        host.shutdown();
        std::fs::remove_dir_all(&host.state).unwrap();
    }
}
