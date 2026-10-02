//! `homeway-cli serve` 命令面（R3-3f；语义真源 `cmd/homeway` 的 serve 子命令 +
//! `internal/daemon/servegroup_cli.go` 的 token 族——裁剪面：无 daemon 期望态，
//! 只有前台 `serve` 与纯读/纯文件操作的 `serve token [list|revoke]`）。
//!
//! 配置覆盖序 = flag 显式设值 > config.toml（`<state>/config.toml`，[serve] 节同
//! schema）> 内置默认（nodeconfig 口径）。`--state` 是引导 flag，不进 config。

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use homeway_core::server::engine::{BindMode, ServeConfig, ServeEngine};

/// config.toml 的 [serve] 节（fileServe 子集；deny_unknown = Go 的 typo 保护同口径）。
#[derive(serde::Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct FileServe {
    #[serde(default)]
    #[allow(dead_code)]
    enabled: bool,
    #[serde(default)]
    listen: Option<u16>,
    #[serde(default)]
    bind_interface: Option<String>,
    #[serde(default)]
    upnp: Option<bool>,
    #[serde(default)]
    stun: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    stun6: Option<String>,
    #[serde(default)]
    relay: Option<String>,
    #[serde(default)]
    max_peers: Option<usize>,
    #[serde(default)]
    peer_ttl: Option<String>,
    #[serde(default)]
    public_endpoint: Option<String>,
    #[serde(default)]
    dns_port: Option<u16>,
    #[serde(default)]
    files_root: Option<String>,
}

#[derive(serde::Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct FileRelay {
    #[serde(default)]
    enabled: bool,
    #[serde(default)]
    #[allow(dead_code)]
    listen: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    advertise: Option<String>,
}

#[derive(serde::Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct FileConfig {
    #[serde(default)]
    serve: FileServe,
    #[serde(default)]
    relay: FileRelay,
}

/// Go 时长串（"15s"/"168h"/"0s"；支持 s/m/h 组合）。
fn parse_go_duration(s: &str) -> Option<Duration> {
    let s = s.trim();
    if s == "0" || s == "0s" {
        return Some(Duration::ZERO);
    }
    let mut total = Duration::ZERO;
    let mut rest = s;
    while !rest.is_empty() {
        let num_end = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
        if num_end == 0 {
            return None;
        }
        let n: u64 = rest[..num_end].parse().ok()?;
        let unit = rest[num_end..].chars().next()?;
        let mult = match unit {
            's' => 1,
            'm' => 60,
            'h' => 3600,
            _ => return None,
        };
        total += Duration::from_secs(n * mult);
        rest = &rest[num_end + 1..];
    }
    Some(total)
}

struct ServeFlags {
    state: Option<PathBuf>,
    listen: Option<u16>,
    bind_interface: Option<String>,
    upnp: Option<bool>,
    stun: Option<String>,
    stun6: Option<String>,
    peer_ttl: Option<Duration>,
    max_peers: Option<usize>,
    public_endpoint: Option<String>,
    dns_port: Option<u16>,
    files_root: Option<String>,
    verbose: bool,
    /// 位置参数（serve 不接受）。
    extra: Vec<String>,
}

fn parse_serve_flags(args: &[String]) -> ServeFlags {
    let mut f = ServeFlags {
        state: None,
        listen: None,
        bind_interface: None,
        upnp: None,
        stun: None,
        stun6: None,
        peer_ttl: None,
        max_peers: None,
        public_endpoint: None,
        dns_port: None,
        files_root: None,
        verbose: false,
        extra: Vec::new(),
    };
    let mut i = 0;
    while i < args.len() {
        // Go flag 风格：--flag value / --flag=value；布尔 flag 可 --flag=false
        let stripped = args[i].strip_prefix("--").or_else(|| args[i].strip_prefix('-'));
        let Some(body) = stripped else {
            f.extra.push(args[i].clone());
            i += 1;
            continue;
        };
        let (name, inline) = match body.split_once('=') {
            Some((k, v)) => (k.to_owned(), Some(v.to_owned())),
            None => (body.to_owned(), None),
        };
        // 值形 flag：内联取值（--flag=value）或吞下一参；布尔 flag 可 --flag=false
        // （裸布尔不吞下一参——Go flag 同形）。
        let take_val = |i: &mut usize| -> Option<String> {
            if let Some(v) = inline.clone() {
                return Some(v);
            }
            *i += 1;
            args.get(*i).cloned()
        };
        let mut j = i;
        match name.as_str() {
            "state" => f.state = take_val(&mut j).map(PathBuf::from),
            "listen" => f.listen = take_val(&mut j).and_then(|v| v.parse().ok()),
            "bind-interface" => f.bind_interface = take_val(&mut j),
            "upnp" => {
                f.upnp = match inline.as_deref() {
                    Some("false") | Some("0") => Some(false),
                    Some("true") | Some("1") | None => Some(true), // 裸布尔 flag
                    Some(_) => Some(true),
                };
            }
            "stun" => f.stun = take_val(&mut j),
            "stun6" => f.stun6 = take_val(&mut j),
            "peer-ttl" => {
                if let Some(v) = take_val(&mut j) {
                    f.peer_ttl = parse_go_duration(&v);
                    if f.peer_ttl.is_none() {
                        eprintln!("--peer-ttl 非法（{v:?}——时长串，如 15s / 168h；0 = 关闭）");
                        std::process::exit(2);
                    }
                }
            }
            "max-peers" => f.max_peers = take_val(&mut j).and_then(|v| v.parse().ok()),
            "public-endpoint" => f.public_endpoint = take_val(&mut j),
            "dns-port" => f.dns_port = take_val(&mut j).and_then(|v| v.parse().ok()),
            "files-root" => f.files_root = take_val(&mut j),
            "verbose" => f.verbose = true,
            other => {
                eprintln!("未知参数：--{other}");
                std::process::exit(2);
            }
        }
        i = j + 1;
    }
    f
}

/// 组装 ServeConfig（flag > config.toml > 默认）。
pub fn assemble(args: &[String]) -> ServeConfig {
    let f = parse_serve_flags(args);
    if !f.extra.is_empty() {
        eprintln!("serve 不接受位置参数（得 {:?}）", f.extra);
        std::process::exit(2);
    }
    let state_dir = f.state.clone().unwrap_or_else(|| PathBuf::from("."));
    let mut cfg = ServeConfig {
        state_dir,
        verbose: f.verbose,
        ..Default::default()
    };
    // config.toml（存在即解析——非法报错退出，不静默按默认；缺失 = 内置默认）
    let cfg_path = cfg.state_dir.join("config.toml");
    if cfg_path.exists() {
        let body = match std::fs::read_to_string(&cfg_path) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("{}: 读取失败：{e}", cfg_path.display());
                std::process::exit(1);
            }
        };
        let fc: FileConfig = match toml::from_str(&body) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("{}: {e}", cfg_path.display());
                std::process::exit(1);
            }
        };
        if let Some(v) = fc.serve.listen {
            cfg.listen_port = v;
        }
        if let Some(v) = &fc.serve.bind_interface {
            cfg.bind_iface = parse_bind_iface(v);
        }
        if let Some(v) = fc.serve.upnp {
            cfg.upnp = v;
        }
        if let Some(v) = &fc.serve.stun {
            cfg.stun = v.clone();
        }
        if let Some(v) = fc.serve.max_peers {
            cfg.max_devices = v;
        }
        if let Some(v) = &fc.serve.peer_ttl {
            match parse_go_duration(v) {
                Some(d) => cfg.peer_ttl = d,
                None => {
                    eprintln!("{}: serve.peer_ttl {v:?} 非法（时长串，如 \"168h\"；须 ≥ 0，0 = 关闭 TTL 回收）", cfg_path.display());
                    std::process::exit(1);
                }
            }
        }
        if let Some(v) = &fc.serve.public_endpoint {
            cfg.public_endpoint = v.clone();
        }
        if let Some(v) = fc.serve.dns_port {
            cfg.dns_port = v;
        }
        if let Some(v) = &fc.serve.files_root {
            cfg.files_root = Some(PathBuf::from(v));
        }
        // relay 节：R4 接线——本期解析后拒绝并打一行（设计 §0 裁剪面）
        if fc.serve.relay.as_deref().map(|r| !r.is_empty()).unwrap_or(false) {
            println!("serve.relay 已配置但 R4（中继）未接线——注册腿不起");
        }
        if fc.relay.enabled {
            println!("[relay] enabled=true 但 R4（中继）未接线——中继角色不起");
        }
    }
    // flag 覆盖
    if let Some(v) = f.listen {
        cfg.listen_port = v;
    }
    if let Some(v) = &f.bind_interface {
        cfg.bind_iface = parse_bind_iface(v);
    }
    if let Some(v) = f.upnp {
        cfg.upnp = v;
    }
    if let Some(v) = &f.stun {
        cfg.stun = v.clone();
    }
    if let Some(v) = f.stun6 {
        if v.is_empty() {
            // 关 v6 校验（v6 面本就登记 R5——占位对齐 flag 面）
        }
    }
    if let Some(v) = f.peer_ttl {
        cfg.peer_ttl = v;
    }
    if let Some(v) = f.max_peers {
        cfg.max_devices = v;
    }
    if let Some(v) = &f.public_endpoint {
        cfg.public_endpoint = v.clone();
    }
    if let Some(v) = f.dns_port {
        cfg.dns_port = v;
    }
    if let Some(v) = &f.files_root {
        cfg.files_root = Some(PathBuf::from(v));
    }
    cfg
}

fn parse_bind_iface(v: &str) -> BindMode {
    match v {
        "auto" => BindMode::Auto,
        "none" | "off" => BindMode::Off,
        name => BindMode::Explicit(name.to_owned()),
    }
}

/// `homeway-cli serve [...]`：前台出口（Ctrl-C / SIGTERM 有序收工）。
pub fn cmd_serve(args: &[String]) {
    let cfg = assemble(args);
    let verbose = cfg.verbose;
    let logf: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(|s: &str| println!("{s}"));
    let dlogf: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(move |s: &str| {
        if verbose {
            println!("{s}");
        }
    });
    let upnp_used = cfg.upnp;
    let engine = match ServeEngine::start(cfg, Arc::clone(&logf), Arc::clone(&dlogf)) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("serve 启动失败：{e}");
            std::process::exit(1);
        }
    };
    // SIGTERM/SIGINT → 有序收工（D5 + UPnP 退出缩租）
    install_stop_signals();
    println!("（serve 前台运行中——Ctrl-C 收工）");
    let _ = wait_stop_pipe();
    if upnp_used {
        homeway_core::server::engine::shrink_upnp_lease(&engine, &logf);
    }
    engine.shutdown(Duration::from_secs(2));
}

/// 信号 → 自写管道一字节（async-signal-safe 面）。
static STOP_FD: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(-1);

extern "C" fn on_stop_signal(_sig: i32) {
    let fd = STOP_FD.load(std::sync::atomic::Ordering::SeqCst);
    if fd >= 0 {
        unsafe {
            let b = b"x";
            libc::write(fd, b.as_ptr().cast(), 1);
        }
    }
}

static STOP_PIPE: std::sync::OnceLock<(i32, i32)> = std::sync::OnceLock::new();

fn install_stop_signals() {
    let (r, w) = *STOP_PIPE.get_or_init(|| unsafe {
        let mut fds = [0i32; 2];
        libc::pipe(fds.as_mut_ptr());
        (fds[0], fds[1])
    });
    STOP_FD.store(w, std::sync::atomic::Ordering::SeqCst);
    unsafe {
        let h = on_stop_signal as extern "C" fn(i32) as libc::sighandler_t;
        libc::signal(libc::SIGTERM, h);
        libc::signal(libc::SIGINT, h);
    }
    let _ = r;
}

fn wait_stop_pipe() -> bool {
    let (r, _) = *STOP_PIPE.get_or_init(|| (0, 0));
    let mut b = [0u8; 1];
    unsafe { libc::read(r, b.as_mut_ptr().cast(), 1) >= 0 }
}

// ---------- serve token [list|revoke]（纯读/纯文件操作） ----------

pub fn cmd_serve_token(args: &[String]) {
    // flag 之后的第一个非 flag 位置参数 = 动词（list / revoke <id>；无动词 = reveal）
    let mut verb: Option<String> = None;
    let mut rest: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if a == "--state" {
            rest.push(a.clone());
            if let Some(v) = args.get(i + 1) {
                rest.push(v.clone());
            }
            i += 2;
            continue;
        }
        if a == "--reason" {
            rest.push(a.clone());
            if let Some(v) = args.get(i + 1) {
                rest.push(v.clone());
            }
            i += 2;
            continue;
        }
        if a.starts_with('-') {
            rest.push(a.clone());
            i += 1;
            continue;
        }
        if verb.is_none() {
            verb = Some(a.clone());
        } else {
            rest.push(a.clone());
        }
        i += 1;
    }
    match verb.as_deref() {
        None => token_reveal(&rest),
        Some("list") => token_list(&rest),
        Some("revoke") => token_revoke(&rest),
        Some(other) => {
            eprintln!("serve token 不认识的动词 {other:?}（可用：list / revoke <id>）");
            std::process::exit(2);
        }
    }
}

fn token_state_dir(args: &[String]) -> PathBuf {
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--state" {
            return args.get(i + 1).map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
        }
        i += 1;
    }
    PathBuf::from(".")
}

/// reveal：完整凭证只经本命令族（来源注记 = 台账末行——写入纪律下末行 = 最近在用）。
fn token_reveal(args: &[String]) {
    let state = token_state_dir(args);
    let st = match homeway_core::server::state::State::open(&state.join("serve")) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("state 打开失败：{e}");
            std::process::exit(1);
        }
    };
    let Some(tok) = st.last_token().ok().flatten() else {
        eprintln!("台账为空（出口从未铸出 token）：先 `homeway-cli serve`，等首轮端点探测后重试");
        std::process::exit(1);
    };
    let eps: Vec<homeway_core::token::EndpointRef> = tok
        .endpoints
        .iter()
        .map(|e| homeway_core::token::EndpointRef::new(e.addr.as_str(), e.kind))
        .collect();
    let s = match homeway_core::token::encode(&homeway_core::token::TokenSpec {
        peer_id: &tok.peer_id,
        secret: &tok.secret,
        endpoints: &eps,
    }) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("token 编码失败：{e}");
            std::process::exit(1);
        }
    };
    println!("serve token：{s}");
    println!("来源：台账末行（进程未跑/角色未装配——写入纪律下末行 = 最近在用 token）");
    if !tok.endpoints.is_empty() {
        let addrs: Vec<&str> = tok.endpoints.iter().map(|e| e.addr.as_str()).collect();
        println!("端点：{}", addrs.join("、"));
    }
}

/// 台账只读列表（id / 签发 / 状态 / 端点 / 掩码）。
fn token_list(args: &[String]) {
    let state = token_state_dir(args);
    let st = match homeway_core::server::state::State::open(&state.join("serve")) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("state 打开失败：{e}");
            std::process::exit(1);
        }
    };
    let ledger = match st.ledger() {
        Ok(l) => l,
        Err(e) => {
            eprintln!("台账读取失败：{e}");
            std::process::exit(1);
        }
    };
    if ledger.is_empty() {
        println!("台账为空（出口从未铸出 token）：先 `homeway-cli serve`，等首轮端点探测后重试");
        return;
    }
    println!("{:<8}  {:<20}  {:<14}  端点/凭证（掩码）", "id", "签发", "状态");
    let mut creds = std::collections::HashSet::new();
    for (i, e) in ledger.iter().enumerate() {
        creds.insert(e.id.clone());
        let mut st_str = if i == ledger.len() - 1 { "有效·末行(在用)" } else { "有效" }.to_owned();
        if e.revoked {
            st_str = if e.reason.is_empty() { "已吊销".to_owned() } else { format!("已吊销({})", e.reason) };
        }
        let eps = if e.endpoints.is_empty() {
            "(无端点)".to_owned()
        } else {
            e.endpoints.iter().map(|x| x.addr.clone()).collect::<Vec<_>>().join(",")
        };
        println!("{:<8}  {:<20}  {:<10}  {}  {}", e.id, e.issued, st_str, mask_secret(&e.secret), eps);
    }
    if let Ok(revs) = st.revocations() {
        if !revs.is_empty() {
            println!("\n吊销表（revoked.jsonl，{} 条）：", revs.len());
            for (id, secret, at, reason) in revs {
                println!("  {id:<8}  {at:<20}  {}  {reason}", mask_secret(&secret));
            }
        }
    }
    println!("\n共 {} 行 / {} 枚凭证（id 相同 = 同一凭证的多轮铸出）", ledger.len(), creds.len());
    println!("吊销：homeway-cli serve token revoke <id>（即时对新注册生效；已登记设备随出口重启清空）");
}

fn mask_secret(s: &str) -> String {
    if s.len() <= 8 {
        return "***".to_owned();
    }
    format!("{}…{}", &s[..6], &s[s.len() - 4..])
}

/// 吊销一枚凭证 id（写吊销表；对在跑出口经跟随读秒级对新注册生效）。
fn token_revoke(args: &[String]) {
    let mut state = PathBuf::from(".");
    let mut reason = "manual".to_owned();
    let mut id = String::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--state" => {
                state = args.get(i + 1).map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
                i += 1; // 值参跳过
            }
            "--reason" => {
                reason = args.get(i + 1).cloned().unwrap_or_else(|| "manual".into());
                i += 1;
            }
            other if !other.starts_with('-') => id = other.to_owned(),
            _ => {}
        }
        i += 1;
    }
    if id.is_empty() {
        eprintln!("serve token revoke 需要 <id>（先 `homeway-cli serve token list` 查）");
        std::process::exit(2);
    }
    let st = match homeway_core::server::state::State::open(&state.join("serve")) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("state 打开失败：{e}");
            std::process::exit(1);
        }
    };
    let ledger = match st.ledger() {
        Ok(l) => l,
        Err(e) => {
            eprintln!("台账读取失败：{e}");
            std::process::exit(1);
        }
    };
    let Some(entry) = ledger.iter().find(|e| e.id == id) else {
        eprintln!("台账里没有 id={id}（先 `homeway-cli serve token list` 查）");
        std::process::exit(1);
    };
    let secret_b64 = entry.secret.clone();
    use base64::Engine as _;
    let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(secret_b64.as_bytes())
        .ok()
        .and_then(|v| <[u8; 32]>::try_from(v).ok());
    let Some(secret) = raw else {
        eprintln!("台账行 secret 非法（{id}）");
        std::process::exit(1);
    };
    match st.revoke(&secret, &reason) {
        Ok(already) => {
            if already {
                println!("id={id} 已在吊销表（幂等——不重复追加）");
            } else {
                println!("已吊销 id={id}（写 revoked.jsonl；在跑出口 ≤1s 跟随生效——对新注册即时拒）");
            }
            println!("彻底清场（拆已在线设备）：重启出口（设备表随角色重建清空）；在用凭证被吊销时重启会自动铸新");
        }
        Err(e) => {
            eprintln!("吊销失败：{e}");
            std::process::exit(1);
        }
    }
}
