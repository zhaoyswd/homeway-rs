//! 独立本机Web入口；远程管理agent不随HTTP入口启停。
mod config;
mod credential;
mod host;
mod http;
mod protocol;
mod pty;

use homeway_core::daemon::{client::ControlClient, vocab::OpName};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::time::Duration;

pub(crate) use host::{Host, RuntimeInfo};

#[derive(Debug, thiserror::Error)]
pub(crate) enum Error {
    #[error(transparent)]
    Token(#[from] credential::TokenError),
    #[error(transparent)]
    Persistence(#[from] credential::PersistError),
    #[error("Web凭证已替换但重读失败，管理认证已暂停；请修复后重新获取token：{source}")]
    CredentialRecovery {
        #[source]
        source: Box<Error>,
    },
    #[error("Web IO：{0}")]
    Io(#[from] std::io::Error),
    #[error("Web配置：{0}")]
    Parse(#[from] toml::de::Error),
    #[error("Web配置序列化：{0}")]
    Serialize(#[from] toml::ser::Error),
    #[error("Web数据格式无效")]
    Json(#[from] serde_json::Error),
    #[error("{0}")]
    Invalid(&'static str),
    #[error("Web管理资源忙，请稍后重试")]
    Busy,
    #[error("Web管理请求超时")]
    Timeout,
    #[error("Web凭证认证失败或目标不可用")]
    Unauthorized,
    #[error("系统随机数不可用")]
    Random,
    #[error("实例身份：{0}")]
    Identity(homeway_core::server::state::StateError),
    #[error("实例RPK：{0}")]
    Rpk(homeway_quic::RpkErr),
    #[error("目标管理连接：{0}")]
    Remote(#[from] homeway_quic::WebClientError),
    #[error("控制面：{0}")]
    Control(#[from] homeway_core::daemon::client::ClientErr),
    #[error("控制面操作：{0}")]
    Operation(homeway_core::daemon::proto::OpError),
    #[error("{0}")]
    Lock(#[from] homeway_core::nodestate::LockError),
}

impl Error {
    pub(crate) fn into_backend(self) -> homeway_core::daemon::proto::BackendErr {
        use homeway_core::daemon::proto::{BackendErr, WebFailure};
        use homeway_quic::WebClientError;
        let category = match &self {
            Self::Unauthorized => WebFailure::Unauthorized,
            Self::Busy => WebFailure::Busy,
            Self::Timeout => WebFailure::Timeout,
            Self::Io(_) => WebFailure::Io,
            Self::Persistence(error) if error.commit_visible() => WebFailure::DurabilityUncertain,
            Self::CredentialRecovery { .. } => WebFailure::RecoveryRequired,
            Self::Persistence(_) => WebFailure::Io,
            Self::Control(_) => WebFailure::Unavailable,
            Self::Remote(WebClientError::Authentication | WebClientError::Revoked) => {
                WebFailure::Unauthorized
            }
            Self::Remote(WebClientError::Capacity) => WebFailure::Busy,
            Self::Remote(WebClientError::Deadline) => WebFailure::Timeout,
            Self::Remote(_) => WebFailure::Unavailable,
            _ => WebFailure::Invalid,
        };
        BackendErr::Web {
            category,
            source: Some(Box::new(self)),
        }
    }
}

pub(crate) fn cmd(args: &[String]) {
    if let Err(error) = run(args) {
        eprintln!("homeway webserve：{error}");
        std::process::exit(1);
    }
}

fn run(args: &[String]) -> Result<(), Error> {
    if args.is_empty() || args.iter().any(|a| a == "--help" || a == "-h") {
        println!(
            "用法：homeway webserve start|stop|restart|status|token [rotate]\n  --port P       固定本机端口（默认18080）\n  --origin URL   反代公开HTTPS origin\n  --state DIR    私有测试实例\n  --json         JSON状态\nHTTP/WS固定监听127.0.0.1；token不启动任何服务。"
        );
        return Ok(());
    }
    let verb = args[0].as_str();
    let mut state: PathBuf = crate::unified_cli::default_state_dir();
    let mut params = serde_json::Map::new();
    let mut rotate = false;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "rotate" if verb == "token" => rotate = true,
            "--state" => {
                i += 1;
                state = PathBuf::from(args.get(i).ok_or(Error::Invalid("--state缺少路径"))?);
            }
            "--port" if matches!(verb, "start" | "restart") => {
                i += 1;
                let port = args
                    .get(i)
                    .and_then(|v| v.parse::<u16>().ok())
                    .filter(|p| *p != 0)
                    .ok_or(Error::Invalid("--port必须为1..65535"))?;
                params.insert("port".into(), json!(port));
            }
            "--origin" if matches!(verb, "start" | "restart") => {
                i += 1;
                params.insert(
                    "public_origin".into(),
                    json!(args.get(i).ok_or(Error::Invalid("--origin缺少URL"))?),
                );
            }
            "--json" => {}
            _ => return Err(Error::Invalid("未知webserve参数")),
        }
        i += 1;
    }
    let op = match verb {
        "start" => OpName::WebserveStart,
        "stop" => OpName::WebserveStop,
        "restart" => OpName::WebserveRestart,
        "status" => OpName::WebserveStatus,
        "token" if rotate => OpName::WebserveRotate,
        "token" => OpName::WebserveToken,
        _ => return Err(Error::Invalid("未知webserve子命令")),
    };
    if verb == "token" {
        if let Ok((client, _)) = ControlClient::dial_with_timeout(
            &state.join("control.sock"),
            "cli",
            "homeway-web-token",
            Duration::from_secs(2),
        ) {
            let mut out = request(&client, op, Value::Object(params))?;
            client.close();
            let value = out.get_mut("token").map(Value::take);
            let token = match value {
                Some(Value::String(token)) => zeroize::Zeroizing::new(token),
                _ => return Err(Error::Invalid("守护返回无Web凭证")),
            };
            println!("{}", *token);
            return Ok(());
        }
        let _lock = homeway_core::nodestate::InstanceLock::acquire(&state, "webserve-token")?;
        let identity = credential::Identity::load(&state)?;
        let credential = credential::obtain(&state, rotate)?;
        println!(
            "{}",
            *credential::Token {
                identity,
                credential
            }
            .encode()
        );
        return Ok(());
    }
    let client = crate::daemon_cli::dial_control_spawn(
        &state,
        "homeway-webserve",
        !matches!(verb, "start" | "restart"),
    )
    .map_err(|_| Error::Invalid("统一进程控制面不可用，请先启动homeway；不会重启现有实例"))?;
    let out = request(&client, op, Value::Object(params))?;
    client.close();
    println!("{}", serde_json::to_string_pretty(&out)?);
    Ok(())
}

fn request(client: &ControlClient, op: OpName, args: Value) -> Result<Value, Error> {
    client
        .request(op.as_str(), Some(args), Duration::from_secs(15))
        .map_err(|e| {
            if e.code == "unknown_op" {
                Error::Invalid("请升级正在运行的homeway守护；不会自动启动第二实例")
            } else {
                Error::Operation(e)
            }
        })
}
