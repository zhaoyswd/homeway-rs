//! TUI 的有界后台工作队列；所有控制面和终端拨号都在 UI 线程之外。
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use homeway_core::daemon::client::ControlClient;
use homeway_core::daemon::proto::{HostBrief, HostState, OpError, SocksStatusResult};
use homeway_core::daemon::vocab::OpName;
use serde::Deserialize;
use serde_json::{json, Value};

pub(super) use super::transport::Attach;
use super::transport::TermConnection;

const REQUEST_BUDGET: Duration = Duration::from_secs(3);

#[derive(Debug, thiserror::Error)]
pub(super) enum Error {
    #[error("控制面未就绪：{0}")]
    Bootstrap(String),
    #[error("控制请求失败：{0}")]
    Control(OpError),
    #[error("连接失败：{0}")]
    Client(#[from] homeway_core::daemon::client::ClientErr),
    #[error("终端 IO：{0}")]
    Io(#[from] std::io::Error),
    #[error("终端读失败：{0}")]
    Read(#[from] homeway_core::term::wire::ReadFail),
    #[error("终端写失败：{0}")]
    Write(homeway_core::term::wire::WriteFail),
    #[error("终端帧无效：{0}")]
    Frame(#[from] homeway_core::term::frames::FrameError),
    #[error("服务数据无效：{0}")]
    Json(#[from] serde_json::Error),
    #[error("请求队列已满，请稍后重试")]
    Busy,
    #[error("后台连接已关闭")]
    Closed,
    #[error("终端服务 {code}：{message}")]
    Session { code: String, message: String },
    #[error("终端协议应答不符（op={0:#x}）")]
    Unexpected(u8),
    #[error("远程终端流已断开（{0}）；会话仍保留，可重新选择接入")]
    StreamEnded(String),
    #[error("远端在预算内未应答，请稍后重试")]
    Timeout,
    #[error("目标终端不支持观察接入；请升级目标主机的 homeway 后重试")]
    ObserverUnavailable,
    #[error("本地终端未就绪；请用包含 tui 的版本启动 homeway 统一进程（不会自动重启现有守护）")]
    LocalUnavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) enum Target {
    Local,
    Remote(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LinkState {
    Direct,
    Relay,
    Offline,
}

#[derive(Debug, Clone)]
pub(super) struct Host {
    pub id: String,
    pub name: String,
    pub state: LinkState,
    pub endpoint: Option<String>,
    pub rtt_ms: Option<i64>,
    pub proxy: Option<u16>,
}

fn host_from_snapshot(brief: HostBrief, state: Option<&HostState>, proxy: Option<u16>) -> Host {
    // 只认运行期ready路径；凭证中的候选端点不能证明当前连接方式或延迟。
    let link = state
        .filter(|state| state.state == "ready")
        .and_then(|state| state.link.as_ref())
        .filter(|link| matches!(link.via.as_str(), "direct" | "relay"));
    Host {
        name: brief
            .name
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| brief.id.chars().take(12).collect()),
        state: match link {
            Some(link) if link.via == "relay" => LinkState::Relay,
            Some(_) => LinkState::Direct,
            None => LinkState::Offline,
        },
        endpoint: link
            .filter(|link| !link.ep.is_empty())
            .map(|link| link.ep.clone()),
        rtt_ms: link.filter(|link| link.rtt_ms >= 0).map(|link| link.rtt_ms),
        id: brief.id,
        proxy,
    }
}

#[derive(Debug, Clone, Default)]
pub(super) struct Snapshot {
    pub serve_on: bool,
    pub relay_on: bool,
    pub hosts: Vec<Host>,
    mutation_epoch: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum SessionState {
    Working,
    Idle,
    Blocked,
    #[default]
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub(super) struct Session {
    pub name: String,
    pub title: String,
    pub agent: String,
    #[serde(rename = "agentLabel")]
    pub agent_label: Option<String>,
    #[serde(rename = "stateV2")]
    pub state: SessionState,
    pub cwd: Option<String>,
    #[serde(rename = "lastActiveMs")]
    pub last_active_ms: i64,
    pub pid: i32,
    /// 仅由本机真实进程祖先链判定，永不相信远端同号 PID 或会话名字。
    #[serde(skip)]
    pub self_session: bool,
}

fn mark_self_sessions(
    target: &Target,
    sessions: &mut [Session],
    pid: i32,
    procs: &[homeway_core::term::agent::ProcInfo],
    confirmed: &mut std::collections::BTreeMap<String, i32>,
) {
    if *target != Target::Local {
        for session in sessions {
            session.self_session = false;
        }
        return;
    }
    // 只有仍然存在的同名同PID可保留已确证身份；采样缺口不撤销真实祖先证据。
    confirmed.retain(|name, pid| {
        sessions
            .iter()
            .any(|session| session.name == *name && session.pid == *pid)
    });
    let mut ancestors = std::collections::HashSet::new();
    let mut current = pid;
    while current > 0 && ancestors.insert(current) {
        let Some(process) = procs.iter().find(|process| process.pid == current) else {
            break;
        };
        current = process.ppid;
    }
    for session in sessions {
        session.self_session = session.pid > 0
            && (ancestors.contains(&session.pid)
                || confirmed.get(&session.name) == Some(&session.pid));
        if session.self_session {
            confirmed.insert(session.name.clone(), session.pid);
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(super) enum Role {
    Exit,
    Relay,
}

pub(super) enum Command {
    Refresh,
    Sessions { target: Target, epoch: u64 },
    AddHost { name: String, token: String },
    RemoveHost { id: String },
    SetRole { role: Role, on: bool },
    SetProxy { id: String, on: bool },
    NewSession { target: Target, epoch: u64 },
    KillSession { target: Target, name: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Confirmation {
    Snapshot,
    Sessions(Target),
    None,
}

pub(super) enum Event {
    Snapshot(Snapshot),
    Sessions {
        target: Target,
        epoch: u64,
        result: Result<Vec<Session>, Error>,
    },
    Created {
        target: Target,
        name: String,
        epoch: u64,
    },
    Notice {
        message: String,
        clipboard: Option<String>,
        confirmation: Confirmation,
    },
    Warning {
        message: String,
        error: Error,
        confirmation: Confirmation,
    },
    RefreshFailed(Error),
    CreateFailed {
        target: Target,
        epoch: u64,
        error: Error,
    },
    Failed(Error),
}

pub(super) struct Backend {
    commands: mpsc::SyncSender<Command>,
    events: mpsc::Receiver<Event>,
    alive: Arc<AtomicBool>,
    sessions: Arc<SessionQueries>,
    refresh: mpsc::SyncSender<()>,
    state_dir: PathBuf,
    bootstrap_started: AtomicBool,
    bootstrap_events: mpsc::SyncSender<Event>,
    mutation_epoch: Arc<AtomicU64>,
}

#[derive(Default)]
struct SessionQueries {
    pending: Mutex<Option<(Target, u64)>>,
    changed: Condvar,
}

impl Backend {
    pub fn start(state_dir: PathBuf) -> Self {
        let (commands, rx) = mpsc::sync_channel(16);
        let (tx, events) = mpsc::sync_channel(32);
        let alive = Arc::new(AtomicBool::new(true));
        let mutation_epoch = Arc::new(AtomicU64::new(0));
        let (refresh, refresh_requests) = mpsc::sync_channel(1);
        let refreshing = Arc::clone(&alive);
        let refresh_events = tx.clone();
        let refresh_state = state_dir.clone();
        let refresh_epoch = Arc::clone(&mutation_epoch);
        let refresh_dirty = refresh.clone();
        std::thread::spawn(move || {
            let mut worker = Worker {
                state_dir: refresh_state,
                control: None,
                self_sessions: Default::default(),
            };
            while refreshing.load(Ordering::Acquire) {
                match refresh_requests.recv_timeout(Duration::from_millis(100)) {
                    Ok(()) => {}
                    Err(mpsc::RecvTimeoutError::Timeout) => continue,
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                }
                let before = refresh_epoch.load(Ordering::Acquire);
                let event = match worker.snapshot() {
                    Ok(mut snapshot) => {
                        if refresh_epoch.load(Ordering::Acquire) != before {
                            let _ = refresh_dirty.try_send(());
                            continue;
                        }
                        snapshot.mutation_epoch = before;
                        Event::Snapshot(snapshot)
                    }
                    Err(error) => {
                        if let Some(client) = worker.control.take() {
                            client.close();
                        }
                        Event::RefreshFailed(error)
                    }
                };
                if !deliver(&refresh_events, event, &refreshing) {
                    break;
                }
            }
        });
        let sessions = Arc::new(SessionQueries::default());
        let query_sessions = Arc::clone(&sessions);
        let query_alive = Arc::clone(&alive);
        let query_events = tx.clone();
        let query_state = state_dir.clone();
        // 选主机查询独立于修改操作，只保留最新一次待查询目标；不排16次旧离线请求。
        std::thread::spawn(move || {
            let mut worker = Worker {
                state_dir: query_state,
                control: None,
                self_sessions: Default::default(),
            };
            while query_alive.load(Ordering::Acquire) {
                let target = {
                    let pending = query_sessions
                        .pending
                        .lock()
                        .unwrap_or_else(|error| error.into_inner());
                    let (mut pending, _) = query_sessions
                        .changed
                        .wait_timeout_while(pending, Duration::from_millis(100), |pending| {
                            pending.is_none() && query_alive.load(Ordering::Acquire)
                        })
                        .unwrap_or_else(|error| error.into_inner());
                    pending.take()
                };
                let Some((target, epoch)) = target else {
                    continue;
                };
                if !query_alive.load(Ordering::Acquire) {
                    break;
                }
                let result = worker.sessions(&target);
                if !deliver(
                    &query_events,
                    Event::Sessions {
                        target,
                        epoch,
                        result,
                    },
                    &query_alive,
                ) {
                    break;
                }
            }
        });
        let running = Arc::clone(&alive);
        let dirty = refresh.clone();
        let mutation_state = state_dir.clone();
        let bootstrap_events = tx.clone();
        let committed_epoch = Arc::clone(&mutation_epoch);
        std::thread::spawn(move || {
            let mut worker = Worker {
                state_dir: mutation_state,
                control: None,
                self_sessions: Default::default(),
            };
            while running.load(Ordering::Acquire) {
                let command = match rx.recv_timeout(Duration::from_millis(100)) {
                    Ok(command) => command,
                    Err(mpsc::RecvTimeoutError::Timeout) => continue,
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                };
                let creating = match &command {
                    Command::NewSession { target, epoch } => Some((target.clone(), *epoch)),
                    _ => None,
                };
                let result = worker.execute(command);
                match result {
                    Ok(events) => {
                        committed_epoch.fetch_add(1, Ordering::AcqRel);
                        for event in events {
                            if !deliver(&tx, event, &running) {
                                break;
                            }
                        }
                        // 动作结果先送达；独立快照线程刷新，不挡下一条修改操作。
                        let _ = dirty.try_send(());
                    }
                    Err(error) => {
                        // 重建坏连接；下一次刷新可以从守护重启中恢复。
                        if let Some(client) = worker.control.take() {
                            client.close();
                        }
                        let event = match creating {
                            Some((target, epoch)) => Event::CreateFailed {
                                target,
                                epoch,
                                error,
                            },
                            None => Event::Failed(error),
                        };
                        let _ = deliver(&tx, event, &running);
                    }
                }
            }
        });
        Self {
            commands,
            events,
            alive,
            sessions,
            refresh,
            state_dir,
            bootstrap_started: AtomicBool::new(false),
            bootstrap_events,
            mutation_epoch,
        }
    }

    pub fn send(&self, command: Command) -> Result<(), Error> {
        if !self.alive.load(Ordering::Acquire) {
            return Err(Error::Closed);
        }
        if matches!(command, Command::Refresh) {
            // 冷启动仅有一个独立bootstrap任务，不占3秒快照或修改lane。
            if !self.bootstrap_started.swap(true, Ordering::AcqRel) {
                let state = self.state_dir.clone();
                let alive = Arc::clone(&self.alive);
                let events = self.bootstrap_events.clone();
                let refresh = self.refresh.clone();
                std::thread::spawn(move || {
                    if !alive.load(Ordering::Acquire) {
                        return;
                    }
                    match crate::daemon_cli::dial_control_spawn_quiet(
                        &state,
                        "homeway-tui-bootstrap",
                    ) {
                        Ok(client) => {
                            client.close();
                            let _ = refresh.try_send(());
                        }
                        Err(error) => {
                            let _ = deliver(
                                &events,
                                Event::Warning {
                                    message: "本机服务未就绪".into(),
                                    error: Error::Bootstrap(error),
                                    confirmation: Confirmation::None,
                                },
                                &alive,
                            );
                        }
                    }
                });
            }
            return match self.refresh.try_send(()) {
                Ok(()) | Err(mpsc::TrySendError::Full(())) => Ok(()),
                Err(mpsc::TrySendError::Disconnected(())) => Err(Error::Closed),
            };
        }
        if let Command::Sessions { target, epoch } = command {
            *self
                .sessions
                .pending
                .lock()
                .unwrap_or_else(|error| error.into_inner()) = Some((target, epoch));
            self.sessions.changed.notify_one();
            return Ok(());
        }
        self.commands
            .try_send(command)
            .map_err(|error| match error {
                mpsc::TrySendError::Full(_) => Error::Busy,
                mpsc::TrySendError::Disconnected(_) => Error::Closed,
            })
    }

    pub fn try_event(&self) -> Option<Event> {
        while let Ok(event) = self.events.try_recv() {
            if matches!(&event, Event::Snapshot(snapshot) if snapshot.mutation_epoch != self.mutation_epoch.load(Ordering::Acquire))
            {
                let _ = self.refresh.try_send(());
                continue;
            }
            return Some(event);
        }
        None
    }
}

impl Drop for Backend {
    fn drop(&mut self) {
        self.alive.store(false, Ordering::Release);
        self.sessions.changed.notify_all();
    }
}

pub(super) fn deliver<T>(tx: &mpsc::SyncSender<T>, mut value: T, alive: &AtomicBool) -> bool {
    while alive.load(Ordering::Acquire) {
        match tx.try_send(value) {
            Ok(()) => return true,
            Err(mpsc::TrySendError::Full(back)) => {
                value = back;
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(mpsc::TrySendError::Disconnected(_)) => return false,
        }
    }
    false
}

struct Worker {
    state_dir: PathBuf,
    control: Option<Arc<ControlClient>>,
    self_sessions: std::collections::BTreeMap<String, i32>,
}

impl Drop for Worker {
    fn drop(&mut self) {
        if let Some(client) = &self.control {
            client.close();
        }
    }
}

impl Worker {
    fn client(&mut self) -> Result<&Arc<ControlClient>, Error> {
        if self.control.is_none() {
            self.control = Some(
                crate::daemon_cli::dial_control_spawn_quiet(&self.state_dir, "homeway-tui")
                    .map_err(Error::Bootstrap)?,
            );
        }
        Ok(self.control.as_ref().expect("刚设置控制面连接"))
    }

    fn request(&mut self, operation: OpName, args: Option<Value>) -> Result<Value, Error> {
        let budget = match operation {
            OpName::HostAdd => Duration::from_secs(20),
            OpName::ServeStart
            | OpName::ServeStop
            | OpName::ServeRestart
            | OpName::ServeToken
            | OpName::RelayStart
            | OpName::RelayStop
            | OpName::RelayRestart
            | OpName::RelayToken => Duration::from_secs(10),
            _ => REQUEST_BUDGET,
        };
        self.client()?
            .request(operation.as_str(), args, budget)
            .map_err(Error::Control)
    }

    fn snapshot(&mut self) -> Result<Snapshot, Error> {
        self.snapshot_with_budget(REQUEST_BUDGET)
    }

    fn snapshot_request(
        &mut self,
        operation: OpName,
        args: Option<Value>,
        deadline: Instant,
    ) -> Result<Value, Error> {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(Error::Timeout);
        }
        if self.control.is_none() {
            self.control = Some(
                ControlClient::dial_with_timeout(
                    &self.state_dir.join("control.sock"),
                    "cli",
                    "homeway-tui-refresh",
                    remaining,
                )?
                .0,
            );
        }
        self.control
            .as_ref()
            .expect("刚设置刷新连接")
            .request_deadline(operation.as_str(), args, deadline)
            .map_err(Error::Control)
    }

    fn snapshot_with_budget(&mut self, budget: Duration) -> Result<Snapshot, Error> {
        let deadline = Instant::now() + budget;
        #[derive(Deserialize)]
        struct Hosts {
            hosts: Vec<HostBrief>,
        }
        #[derive(Deserialize)]
        struct States {
            hosts: Vec<HostState>,
            generation: String,
        }
        let hosts: Hosts =
            serde_json::from_value(self.snapshot_request(OpName::HostList, None, deadline)?)?;
        let states: States =
            serde_json::from_value(self.snapshot_request(OpName::SnapshotGet, None, deadline)?)?;
        // 沿用控制面视图声明；空事件域避免在UI轮询快照时积压未消费事件。
        let view = hosts
            .hosts
            .iter()
            .map(|h| format!("host={}", h.id))
            .collect::<Vec<_>>()
            .join(",");
        self.snapshot_request(
            OpName::EventsSubscribe,
            Some(json!({"domains":[],"view":view,"generation":states.generation})),
            deadline,
        )?;
        let socks: SocksStatusResult =
            serde_json::from_value(self.snapshot_request(OpName::SocksStatus, None, deadline)?)?;
        let serve = self.snapshot_request(OpName::ServeStatus, None, deadline)?;
        let relay = self.snapshot_request(OpName::RelayStatus, None, deadline)?;
        Ok(Snapshot {
            serve_on: serve["state"] == "running",
            relay_on: matches!(relay["state"].as_str(), Some("running" | "listening")),
            mutation_epoch: 0,
            hosts: hosts
                .hosts
                .into_iter()
                .map(|host| {
                    let state = states.hosts.iter().find(|s| s.id == host.id);
                    let proxy = socks
                        .socks
                        .iter()
                        .find(|s| s.host == host.id && s.on)
                        .map(|s| s.listen);
                    host_from_snapshot(host, state, proxy)
                })
                .collect(),
        })
    }

    fn sessions(&mut self, target: &Target) -> Result<Vec<Session>, Error> {
        #[derive(Deserialize)]
        struct Sessions {
            sessions: Vec<Session>,
        }
        let mut connection =
            TermConnection::connect_with_budget(&self.state_dir, target, REQUEST_BUDGET)?;
        let frame = connection.round_trip(homeway_core::term::frames::Op::LIST, &[])?;
        let mut sessions: Sessions = serde_json::from_slice(&frame.payload)?;
        if *target == Target::Local && !sessions.sessions.is_empty() {
            mark_self_sessions(
                target,
                &mut sessions.sessions,
                std::process::id() as i32,
                &homeway_core::term::agent::read_procs(),
                &mut self.self_sessions,
            );
        } else if *target == Target::Local {
            self.self_sessions.clear();
        }
        sessions
            .sessions
            .sort_by_key(|s| std::cmp::Reverse(s.last_active_ms));
        Ok(sessions.sessions)
    }

    fn execute(&mut self, command: Command) -> Result<Vec<Event>, Error> {
        use homeway_core::term::frames::{self, Op};
        let notice = |message: &str, confirmation: Confirmation| Event::Notice {
            message: message.to_owned(),
            clipboard: None,
            confirmation,
        };
        match command {
            Command::Refresh => Err(Error::Closed), // send已分流到独立刷新lane。
            Command::Sessions { target, epoch } => {
                let result = self.sessions(&target);
                Ok(vec![Event::Sessions {
                    target,
                    epoch,
                    result,
                }])
            }
            Command::AddHost { name, token } => {
                self.request(OpName::HostAdd, Some(json!({"name":name,"token":token})))?;
                Ok(vec![notice("主机已添加", Confirmation::Snapshot)])
            }
            Command::RemoveHost { id } => {
                self.request(OpName::HostRemove, Some(json!({"host":id})))?;
                Ok(vec![notice("主机已删除", Confirmation::Snapshot)])
            }
            Command::SetRole { role, on } => {
                let label = match role {
                    Role::Exit => "出口",
                    Role::Relay => "中继",
                };
                let operation = match (role, on) {
                    (Role::Exit, true) => OpName::ServeStart,
                    (Role::Exit, false) => OpName::ServeStop,
                    (Role::Relay, true) => OpName::RelayStart,
                    (Role::Relay, false) => OpName::RelayStop,
                };
                self.request(operation, None)?;
                let token = if on {
                    self.request(
                        match role {
                            Role::Exit => OpName::ServeToken,
                            Role::Relay => OpName::RelayToken,
                        },
                        None,
                    )
                    .and_then(|token| {
                        token["token"]
                            .as_str()
                            .filter(|s| !s.is_empty())
                            .map(str::to_owned)
                            .ok_or(Error::Unexpected(0))
                    })
                    .map(Some)
                } else {
                    Ok(None)
                };
                let events = match token {
                    Ok(clipboard) => vec![Event::Notice {
                        message: format!("{label}已{}", if on { "启动" } else { "关闭" }),
                        clipboard,
                        confirmation: Confirmation::Snapshot,
                    }],
                    Err(error) => vec![Event::Warning {
                        message: format!("{label}已启动，但未能取得 token"),
                        error,
                        confirmation: Confirmation::Snapshot,
                    }],
                };
                Ok(events)
            }
            Command::SetProxy { id, on } => {
                let result = self.request(
                    if on {
                        OpName::SocksOn
                    } else {
                        OpName::SocksOff
                    },
                    Some(json!({"host":id})),
                )?;
                let clipboard = if on {
                    let port = result["listen"]
                        .as_u64()
                        .filter(|p| *p > 0 && *p <= u16::MAX as u64);
                    let Some(port) = port else {
                        return Ok(vec![Event::Warning {
                            message: "代理已启动，但未能取得监听地址".into(),
                            error: Error::Unexpected(0),
                            confirmation: Confirmation::Snapshot,
                        }]);
                    };
                    Some(format!("socks5h://127.0.0.1:{port}"))
                } else {
                    None
                };
                Ok(vec![Event::Notice {
                    message: format!("代理已{}", if on { "启动" } else { "关闭" }),
                    clipboard,
                    confirmation: Confirmation::Snapshot,
                }])
            }
            Command::NewSession { target, epoch } => {
                let mut bytes = [0; 4];
                getrandom::getrandom(&mut bytes)
                    .map_err(|e| Error::Io(std::io::Error::other(e.to_string())))?;
                let name = format!("tui-{:08x}", u32::from_ne_bytes(bytes));
                let mut connection = TermConnection::connect(&self.state_dir, &target)?;
                let frame = connection.round_trip(Op::CREATE, &frames::enc_create(0, &name))?;
                if frame.op != Op::OK {
                    return Err(Error::Unexpected(frame.op.0));
                }
                Ok(vec![Event::Created {
                    target,
                    name,
                    epoch,
                }])
            }
            Command::KillSession { target, name } => {
                let mut connection = TermConnection::connect(&self.state_dir, &target)?;
                let frame = connection.round_trip(Op::KILL, &frames::enc_name(&name))?;
                if frame.op != Op::OK {
                    return Err(Error::Unexpected(frame.op.0));
                }
                // UI接到成功Notice后走latest-only会话查询，KILL不等待后续LIST。
                Ok(vec![notice("会话已关闭", Confirmation::Sessions(target))])
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn self_session_requires_local_ancestor_pid_not_name_or_remote_pid() {
        use homeway_core::term::agent::ProcInfo;
        let procs = vec![
            ProcInfo {
                pid: 30,
                ppid: 20,
                ..Default::default()
            },
            ProcInfo {
                pid: 20,
                ppid: 10,
                ..Default::default()
            },
            ProcInfo {
                pid: 10,
                ppid: 1,
                ..Default::default()
            },
        ];
        let mut confirmed = Default::default();
        let mut sessions = vec![
            Session {
                name: "carrier".into(),
                pid: 10,
                ..Default::default()
            },
            Session {
                name: "same-name".into(),
                pid: 999,
                ..Default::default()
            },
            Session {
                pid: 0,
                ..Default::default()
            },
        ];
        mark_self_sessions(
            &Target::Remote("other-machine".into()),
            &mut sessions,
            30,
            &procs,
            &mut confirmed,
        );
        assert!(sessions.iter().all(|s| !s.self_session));
        mark_self_sessions(&Target::Local, &mut sessions, 30, &procs, &mut confirmed);
        assert!(sessions[0].self_session);
        assert!(!sessions[1].self_session && !sessions[2].self_session);
        let cycle = vec![
            ProcInfo {
                pid: 30,
                ppid: 20,
                ..Default::default()
            },
            ProcInfo {
                pid: 20,
                ppid: 30,
                ..Default::default()
            },
        ];
        mark_self_sessions(&Target::Local, &mut sessions, 30, &cycle, &mut confirmed);
        assert!(
            sessions[0].self_session,
            "采样缺失仍保留已经确证的同名同PID祖先"
        );
        mark_self_sessions(
            &Target::Remote("alias".into()),
            &mut sessions,
            30,
            &[],
            &mut confirmed,
        );
        assert!(
            sessions.iter().all(|session| !session.self_session),
            "远端不得沿用本机祖先证据"
        );
        sessions[0].pid = 11;
        mark_self_sessions(&Target::Local, &mut sessions, 30, &[], &mut confirmed);
        assert!(
            !sessions[0].self_session && confirmed.is_empty(),
            "PID变化清理旧证据"
        );
        sessions[0].pid = 10;
        mark_self_sessions(&Target::Local, &mut sessions, 30, &procs, &mut confirmed);
        assert!(sessions[0].self_session);
        sessions.clear();
        mark_self_sessions(&Target::Local, &mut sessions, 30, &[], &mut confirmed);
        assert!(confirmed.is_empty(), "会话消失清理旧证据");
    }
    use homeway_core::daemon::{
        frame as control_frame,
        proto::{HelloBody, RequestBody, ResponseBody, WelcomeBody},
    };
    use std::os::unix::net::{UnixListener, UnixStream};

    fn temporary(tag: &str) -> PathBuf {
        let root = PathBuf::from("/tmp").join(format!(
            "hw-tui-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    fn control_read(socket: &mut UnixStream) -> Option<Value> {
        let head = control_frame::read_header(socket).ok()?;
        serde_json::from_slice(&control_frame::read_body(socket, head).ok()?).ok()
    }

    fn control_welcome(socket: &mut UnixStream) -> String {
        let hello: HelloBody = serde_json::from_value(control_read(socket).unwrap()).unwrap();
        control_frame::write_frame(
            socket,
            control_frame::Op::Welcome,
            &serde_json::to_vec(&WelcomeBody {
                server_version: "test".into(),
                generation: "g".into(),
                server_seq: 0,
            })
            .unwrap(),
        )
        .unwrap();
        hello.frontend.name
    }

    fn control_response(socket: &mut UnixStream, request: &RequestBody, result: Value) {
        control_frame::write_frame(
            socket,
            control_frame::Op::Rsp,
            &serde_json::to_vec(&ResponseBody {
                corr: request.corr,
                ok: true,
                error: None,
                detail: None,
                result: Some(result),
            })
            .unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn blocked_refresh_does_not_block_mutation_or_hide_successful_proxy_phase() {
        let root = temporary("refresh-lanes");
        let listener = UnixListener::bind(root.join("control.sock")).unwrap();
        let (entered_tx, entered) = mpsc::channel();
        let (release_tx, release) = mpsc::channel();
        let release = Arc::new(Mutex::new(release));
        let server = std::thread::spawn(move || {
            let mut workers = Vec::new();
            for _ in 0..3 {
                let (mut socket, _) = listener.accept().unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let entered = entered_tx.clone();
                let release = Arc::clone(&release);
                workers.push(std::thread::spawn(move || {
                    let name = control_welcome(&mut socket);
                    while let Some(value) = control_read(&mut socket) {
                        let request: RequestBody = serde_json::from_value(value).unwrap();
                        if name == "homeway-tui-refresh" && request.op == "host.list" {
                            entered.send(()).unwrap();
                            release
                                .lock()
                                .unwrap()
                                .recv_timeout(Duration::from_secs(5))
                                .unwrap();
                        }
                        let result = match request.op.as_str() {
                            "host.list" => json!({"hosts":[]}),
                            "snapshot.get" => json!({"hosts":[],"generation":"g"}),
                            "events.subscribe" => json!({}),
                            "socks.status" => json!({"socks":[]}),
                            "serve.status" | "relay.status" => json!({"state":"stopped"}),
                            "socks.on" => json!({"listen":0}),
                            _ => panic!("unexpected operation {}", request.op),
                        };
                        control_response(&mut socket, &request, result);
                    }
                }));
            }
            for worker in workers {
                worker.join().unwrap();
            }
        });
        let backend = Backend::start(root.clone());
        backend.send(Command::Refresh).unwrap();
        entered.recv_timeout(Duration::from_secs(3)).unwrap();
        backend
            .send(Command::SetProxy {
                id: "private".into(),
                on: true,
            })
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if let Some(Event::Warning { message, .. }) = backend.try_event() {
                assert!(message.contains("代理已启动"));
                break;
            }
            assert!(Instant::now() < deadline, "修改操作被等待中的刷新请求阻塞");
            std::thread::sleep(Duration::from_millis(2));
        }
        drop(backend);
        release_tx.send(()).unwrap();
        server.join().unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn snapshot_uses_one_total_deadline_for_multiple_requests() {
        let root = temporary("snapshot-budget");
        let listener = UnixListener::bind(root.join("control.sock")).unwrap();
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_millis(500)))
                .unwrap();
            control_welcome(&mut socket);
            let request: RequestBody =
                serde_json::from_value(control_read(&mut socket).unwrap()).unwrap();
            std::thread::sleep(Duration::from_millis(50));
            control_response(&mut socket, &request, json!({"hosts":[]}));
            let request: RequestBody =
                serde_json::from_value(control_read(&mut socket).unwrap()).unwrap();
            assert_eq!(request.op, "snapshot.get");
            // 不再回答：正确预算到点由调用方close，旧分步3s预算会等到服务端500ms断链。
            assert!(control_read(&mut socket).is_none());
        });
        let mut worker = Worker {
            state_dir: root.clone(),
            control: None,
            self_sessions: Default::default(),
        };
        let result = worker.snapshot_with_budget(Duration::from_millis(120));
        assert!(matches!(result, Err(Error::Control(ref error)) if error.code == "timeout"));
        drop(worker);
        server.join().unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn queued_snapshot_from_before_commit_cannot_overwrite_new_state() {
        let root = temporary("stale-snapshot");
        let backend = Backend::start(root.clone());
        backend
            .bootstrap_events
            .try_send(Event::Snapshot(Snapshot::default()))
            .ok()
            .unwrap();
        backend.mutation_epoch.store(1, Ordering::Release);
        backend
            .bootstrap_events
            .try_send(Event::Notice {
                message: "主机已添加".into(),
                clipboard: None,
                confirmation: Confirmation::Snapshot,
            })
            .ok()
            .unwrap();
        assert!(
            matches!(backend.try_event(), Some(Event::Notice { message, .. }) if message == "主机已添加")
        );
        drop(backend);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn role_start_success_is_preserved_when_token_query_fails() {
        let root = temporary("role-warning");
        let listener = UnixListener::bind(root.join("control.sock")).unwrap();
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            control_welcome(&mut socket);
            let start: RequestBody =
                serde_json::from_value(control_read(&mut socket).unwrap()).unwrap();
            assert_eq!(start.op, "serve.start");
            control_response(&mut socket, &start, json!({"action":"started"}));
            let token: RequestBody =
                serde_json::from_value(control_read(&mut socket).unwrap()).unwrap();
            assert_eq!(token.op, "serve.token");
            control_frame::write_frame(
                &mut socket,
                control_frame::Op::Rsp,
                &serde_json::to_vec(&ResponseBody {
                    corr: token.corr,
                    ok: false,
                    error: Some("not_ready".into()),
                    detail: None,
                    result: None,
                })
                .unwrap(),
            )
            .unwrap();
            assert!(control_read(&mut socket).is_none());
        });
        let (client, _) =
            ControlClient::dial(&root.join("control.sock"), "cli", "phase-test").unwrap();
        let mut worker = Worker {
            state_dir: root.clone(),
            control: Some(client),
            self_sessions: Default::default(),
        };
        let events = worker
            .execute(Command::SetRole {
                role: Role::Exit,
                on: true,
            })
            .unwrap();
        assert!(
            matches!(events.as_slice(), [Event::Warning { message, .. }] if message.contains("出口已启动") && message.contains("token"))
        );
        drop(worker);
        server.join().unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn successful_kill_returns_notice_without_waiting_for_list() {
        use homeway_core::term::{
            frames::{self, Op},
            wire::FrameIo,
        };
        let root = temporary("kill-phase");
        let listener = UnixListener::bind(root.join("term.sock")).unwrap();
        let server = std::thread::spawn(move || {
            let (socket, _) = listener.accept().unwrap();
            let mut io = FrameIo::new(socket);
            io.write_frame(Op::GREETING, &frames::enc_greeting(), REQUEST_BUDGET)
                .unwrap();
            assert_eq!(io.read_frame_deadline(REQUEST_BUDGET).unwrap().op, Op::KILL);
            io.write_frame(Op::OK, &[], REQUEST_BUDGET).unwrap();
            assert!(io.read_frame_deadline(REQUEST_BUDGET).is_err());
        });
        let mut worker = Worker {
            state_dir: root.clone(),
            control: None,
            self_sessions: Default::default(),
        };
        let events = worker
            .execute(Command::KillSession {
                target: Target::Local,
                name: "s".into(),
            })
            .unwrap();
        assert!(
            matches!(events.as_slice(), [Event::Notice { message, clipboard: None, .. }] if message == "会话已关闭")
        );
        server.join().unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn host_labels_only_use_current_ready_path_and_optional_measurements() {
        let brief = HostBrief {
            id: "12".repeat(32),
            name: Some("host".into()),
            added_at: 0,
        };
        let mut state: HostState = serde_json::from_value(json!({"id":brief.id,"state":"ready","link":{"via":"direct","ep":"127.0.0.1:42651","rttMs":3,"at":42}})).unwrap();
        let direct = host_from_snapshot(brief.clone(), Some(&state), Some(1080));
        assert_eq!(direct.state, LinkState::Direct);
        assert_eq!(direct.endpoint.as_deref(), Some("127.0.0.1:42651"));
        assert_eq!(direct.rtt_ms, Some(3));
        state.link.as_mut().unwrap().via = "relay".into();
        assert_eq!(
            host_from_snapshot(brief.clone(), Some(&state), None).state,
            LinkState::Relay
        );
        state.link.as_mut().unwrap().rtt_ms = -1;
        state.link.as_mut().unwrap().ep.clear();
        let missing = host_from_snapshot(brief.clone(), Some(&state), None);
        assert_eq!((missing.endpoint, missing.rtt_ms), (None, None));
        state.state = "failed".into();
        let stale = host_from_snapshot(brief.clone(), Some(&state), None);
        assert_eq!(stale.state, LinkState::Offline);
        assert_eq!((stale.endpoint, stale.rtt_ms), (None, None));
        state.state = "ready".into();
        state.link.as_mut().unwrap().via = "none".into();
        assert_eq!(
            host_from_snapshot(brief, Some(&state), None).state,
            LinkState::Offline
        );
    }
    #[test]
    fn service_session_fields_keep_detection_and_cwd() {
        let session: Session = serde_json::from_value(json!({"name":"s","agent":"codex","stateV2":"blocked","cwd":"/work/project","lastActiveMs":42})).unwrap();
        assert_eq!(session.state, SessionState::Blocked);
        assert_eq!(session.agent, "codex");
        assert_eq!(session.cwd.as_deref(), Some("/work/project"));
        let unknown: Session = serde_json::from_value(json!({"stateV2":"future"})).unwrap();
        assert_eq!(unknown.state, SessionState::Unknown);
    }
    #[test]
    fn bounded_deliver_observes_cancellation() {
        let (tx, _rx) = mpsc::sync_channel(1);
        tx.send(1).unwrap();
        let alive = AtomicBool::new(false);
        assert!(!deliver(&tx, 2, &alive));
    }

    #[test]
    fn latest_session_query_keeps_epoch_and_skips_old_pending_targets() {
        use homeway_core::term::{
            frames::{self, Op},
            wire::FrameIo,
        };
        use std::os::unix::net::UnixListener;
        let root = PathBuf::from("/tmp").join(format!(
            "hw-tui-query-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let listener = UnixListener::bind(root.join("term.sock")).unwrap();
        let (entered_tx, entered) = mpsc::channel();
        let (release_tx, release) = mpsc::channel();
        let server = std::thread::spawn(move || {
            for index in 0..2 {
                let (socket, _) = listener.accept().unwrap();
                let mut io = FrameIo::new(socket);
                io.write_frame(Op::GREETING, &frames::enc_greeting(), REQUEST_BUDGET)
                    .unwrap();
                assert_eq!(io.read_frame_deadline(REQUEST_BUDGET).unwrap().op, Op::LIST);
                if index == 0 {
                    entered_tx.send(()).unwrap();
                    release.recv_timeout(REQUEST_BUDGET).unwrap();
                }
                io.write_frame(Op::LIST, br#"{"sessions":[]}"#, REQUEST_BUDGET)
                    .unwrap();
            }
        });
        let backend = Backend::start(root.clone());
        backend
            .send(Command::Sessions {
                target: Target::Local,
                epoch: 1,
            })
            .unwrap();
        entered.recv_timeout(REQUEST_BUDGET).unwrap();
        for epoch in 2..=200 {
            backend
                .send(Command::Sessions {
                    target: Target::Local,
                    epoch,
                })
                .unwrap();
        }
        release_tx.send(()).unwrap();
        let deadline = std::time::Instant::now() + REQUEST_BUDGET;
        let mut epochs = Vec::new();
        while epochs.len() < 2 {
            if let Some(Event::Sessions { epoch, result, .. }) = backend.try_event() {
                assert!(result.is_ok());
                epochs.push(epoch);
            }
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(epochs, [1, 200]);
        drop(backend);
        server.join().unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }
}
