//! homeway-cli —— 测试/运维命令面。
//!
//! R2 实装 `connect`（Session 服务会话形态：暖机/巡检/恢复阶梯/整会话重建都在
//! Session 内）+ 故障注入与时间窗测试钩子（`--inject`/`--recover-from`，test-seams）。
//!
//! 判据行输出对齐 `docs/INTEROP-CRITERIA.md`（模板逐串）；**Session 的一切行带
//! `服务会话: ` 前缀**（R2 前缀口径硬规则——与 Go 客户端真日志逐字一致）；C8
//! （`warmup pong: 就绪（判据=wg）`）是 APP 核形态文案，按 R1 登记在 CLI 层打出。

use std::net::SocketAddrV4;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use homeway_core::session::{Level, Session, SessionConfig, SessState};
use homeway_core::speedtest::{self, Params};
use homeway_core::token;
use homeway_core::wgcore::ConnErr;

mod relay_cli;
mod serve_cli;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("token") => cmd_token(&args[2..]),
        Some("serve") => {
            if args.get(2).map(String::as_str) == Some("token") {
                serve_cli::cmd_serve_token(&args[3..]);
            } else {
                serve_cli::cmd_serve(&args[2..]);
            }
        }
        Some("relay") => relay_cli::cmd_relay(&args[2..]),
        Some("connect") => cmd_connect(&args[2..]),
        Some("speedtest") => cmd_speedtest(&args[2..]),
        Some("files") => cmd_files(&args[2..]),
        Some("dnstest") => cmd_dnstest(&args[2..]),
        Some("portfwd") => cmd_portfwd(&args[2..]),
        _ => {
            eprintln!(
"homeway-cli——可用：\n  token <hmw1…> [--dead-direct]（改写输出：Direct 端点 → 死端口——矩阵中继段注入缝）\n  connect --token <hmw1…> [--identity-dir <dir>] [--endpoint-cache-dir <dir>]\n      [--speedtest] [--dial <ip:port>] [--hold <secs>] [--probe N] [--status-json]\n      [--recover-from <1|2|3> [--recover-cause <s>]] [--inject poison-socket|relay-lock]（注入需 test-seams 构建）\n  speedtest --token <hmw1…> [--identity-dir D] [--endpoint-cache-dir D] [--rounds N] [--hold]（同会话 N 轮——A/B 轮次口径）"
            );
            std::process::exit(2);
        }
    }
}

fn cmd_token(args: &[String]) {
    // `token <hmw1…> --dead-direct`：解析后把 Direct 端点改指 127.0.0.1:1 重编码输出
    //（矩阵中继段的 Go 客户端注入缝——Go host add 无 dead-direct flag；crc4 无密钥
    // 重算即被两侧接受，评审确认可行）。与 --token 的注入语义一致（connect 侧）。
    // `--loopback-only`：Direct 端点的非回环 IPv4 换 127.0.0.1 同端口（R6 前置批 ⑤
    // 对照实验缝：同机拓扑里客户端赛跑可能采纳本机 LAN IP，出口发往它的 UDP 走
    // en0 环回路径——实测 18.3µs/包 vs lo0 5.5µs/包（3.3 倍），强制回环可分离
    //「endpoint 路径成本」与「实现栈成本」）。
    let dead_direct = args.iter().any(|a| a == "--dead-direct");
    let loopback_only = args.iter().any(|a| a == "--loopback-only");
    let input = args.iter().find(|a| !a.starts_with("--")).cloned();
    let Some(s) = input else {
        eprintln!("用法：homeway-cli token <hmw1…> [--dead-direct] [--loopback-only]");
        std::process::exit(2);
    };
    if dead_direct || loopback_only {
        let mut t = match token::decode(&s) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("解析失败：{e}");
                std::process::exit(1);
            }
        };
        for e in &mut t.endpoints {
            if e.kind == token::EndpointKind::Direct {
                if dead_direct {
                    e.addr = "127.0.0.1:1".to_owned();
                } else if let Some((_, port)) = e.addr.rsplit_once(':') {
                    // 非回环 IPv4 → 127.0.0.1 同端口；IPv6 端点（含 ':'）不动——本缝
                    // 只针对同机 LAN 形态
                    if e.addr.split('.').count() == 4 && !e.addr.starts_with("127.0.0.1:") {
                        e.addr = format!("127.0.0.1:{port}");
                    }
                }
            }
        }
        let eps: Vec<token::EndpointRef<'_>> =
            t.endpoints.iter().map(|e| token::EndpointRef::new(&e.addr, e.kind)).collect();
        let spec = token::TokenSpec { peer_id: &t.peer_id, secret: &t.secret, endpoints: &eps };
        match token::encode(&spec) {
            Ok(out) => {
                println!("{out}");
                return;
            }
            Err(e) => {
                eprintln!("重编码失败：{e}");
                std::process::exit(1);
            }
        }
    }
    match token::decode(&s) {
        Ok(t) => {
            println!("peer_id  = {}", hex_str(t.peer_id.as_bytes()));
            println!("secret   = {}", hex_str(t.secret.as_bytes()));
            for e in &t.endpoints {
                println!("endpoint = {} ({:?})", e.addr, e.kind);
            }
        }
        Err(e) => {
            eprintln!("解析失败：{e}");
            std::process::exit(1);
        }
    }
}

struct ConnectArgs {
    tok: Option<String>,
    identity_dir: Option<PathBuf>,
    cache_dir: Option<PathBuf>,
    do_speedtest: bool,
    dial: Option<SocketAddrV4>,
    hold: u64,
    probe: u32,
    status_json: bool,
    recover_from: Option<i64>,
    recover_cause: String,
    recover_delay: u64,
    inject: Option<String>,
    /// 测试注入：token 的直连端点改指死端口（127.0.0.1:1）——压出「只有中继可达」
    /// 形态（DirectFirst 解锁 + via=relay）。
    dead_direct: bool,
}

fn parse_connect(args: &[String]) -> ConnectArgs {
    let mut a = ConnectArgs {
        tok: None,
        identity_dir: None,
        cache_dir: None,
        do_speedtest: false,
        dial: None,
        hold: 0,
        probe: 0,
        status_json: false,
        recover_from: None,
        recover_cause: "测试注入".to_owned(),
        recover_delay: 0,
        inject: None,
        dead_direct: false,
    };
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--token" => {
                i += 1;
                a.tok = args.get(i).cloned();
            }
            "--identity-dir" => {
                i += 1;
                a.identity_dir = args.get(i).map(PathBuf::from);
            }
            "--endpoint-cache-dir" => {
                i += 1;
                a.cache_dir = args.get(i).map(PathBuf::from);
            }
            "--speedtest" => a.do_speedtest = true,
            "--dial" => {
                i += 1;
                a.dial = args.get(i).and_then(|s| s.parse().ok());
                if a.dial.is_none() {
                    eprintln!("--dial 需要 <ipv4:port>（如 192.168.3.12:9999）");
                    std::process::exit(2);
                }
            }
            "--hold" => {
                i += 1;
                a.hold = args.get(i).and_then(|s| s.parse().ok()).unwrap_or(0);
            }
            "--probe" => {
                i += 1;
                a.probe = args.get(i).and_then(|s| s.parse().ok()).unwrap_or(0);
            }
            "--status-json" => a.status_json = true,
            "--recover-from" => {
                i += 1;
                a.recover_from = args.get(i).and_then(|s| s.parse().ok());
            }
            "--recover-delay" => {
                i += 1;
                a.recover_delay = args.get(i).and_then(|s| s.parse().ok()).unwrap_or(0);
            }
            "--recover-cause" => {
                i += 1;
                a.recover_cause = args.get(i).cloned().unwrap_or_else(|| "测试注入".into());
            }
            "--dead-direct" => a.dead_direct = true,
            "--inject" => {
                i += 1;
                a.inject = args.get(i).cloned();
            }
            other => {
                eprintln!("未知参数：{other}");
                std::process::exit(2);
            }
        }
        i += 1;
    }
    a
}

fn cmd_connect(args: &[String]) {
    let a = parse_connect(args);
    let Some(tok) = a.tok.clone() else {
        eprintln!("用法：homeway-cli connect --token <hmw1…> […]");
        std::process::exit(2);
    };
    let mut t = match token::decode(&tok) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("token 解析失败：{e}");
            std::process::exit(1);
        }
    };
    if a.dead_direct {
        for e in &mut t.endpoints {
            if e.kind == token::EndpointKind::Direct {
                e.addr = "127.0.0.1:1".to_owned();
            }
        }
        println!("inject: token 直连端点已改指死端口（127.0.0.1:1）——只有中继可达");
    }

    let logf: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(|s: &str| println!("{s}"));
    let mut session = match Session::start(SessionConfig {
        token: t,
        identity_dir: a.identity_dir.clone().or_else(|| Some(PathBuf::from("identity"))),
        endpoint_cache_dir: a.cache_dir.clone(),
        logf: Arc::clone(&logf),
        relay_only: a.inject.as_deref() == Some("relay-lock"),
    }) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("会话建立失败：{e}");
            std::process::exit(1);
        }
    };
    let snap = session.snapshot();
    if snap.state == SessState::Failed {
        eprintln!("会话失败收工：{}", snap.reason);
        std::process::exit(1);
    }
    // C8（APP 核形态判据，按 R1 登记在 CLI 层打出、一次会话一次）
    println!("warmup pong: 就绪（判据=wg）");

    // ---- 故障注入（test-seams 构建；普通构建给出可判定错误）----
    if let Some(what) = &a.inject {
        inject(&session, what);
    }

    // ---- 恢复钩子（时间窗实测：阶梯直测面；delay = 先给外部注入留窗口）----
    if let Some(from) = a.recover_from {
        if a.recover_delay > 0 {
            println!("recover: 等待 {}s 后起跑（注入窗口）", a.recover_delay);
            std::thread::sleep(Duration::from_secs(a.recover_delay));
        }
        let lvl = Level::clamp(from);
        let t0 = Instant::now();
        let rc = session.recover(lvl, &a.recover_cause);
        println!(
            "recover: from={} rc={} 耗时={}ms（ladder 判据行见上）",
            from,
            rc.as_rc(),
            t0.elapsed().as_millis()
        );
    }

    // ---- status-json（2f 契约面）----
    if a.status_json {
        println!("{}", homeway_core::status_json::snapshot_json(&session.snapshot()));
    }

    // ---- 手动探测拍（恢复性验证用）----
    for _ in 0..a.probe {
        match session.client().path_probe(Duration::from_secs(10)) {
            Ok(()) => println!("probe: ok"),
            Err(e) => println!("probe: 失败（{e}）"),
        }
    }

    // ---- transit 产出步骤（--dial 非环回）----
    if let Some(dst) = a.dial {
        match transit_dial(&session, dst) {
            Ok(n) => println!("transit: 经隧道拨 {dst} 成功（收 {n} 字节）"),
            Err(e) => eprintln!("transit: 经隧道拨 {dst} 回读未成（{e}）——出口侧 transit 行已产出，继续"),
        }
    }

    // ---- speedtest（可选；直连面，不走 healing）----
    if a.do_speedtest {
        let client = session.client();
        match speedtest::run(client.as_ref(), Params::default(), &|s| println!("{s}")) {
            Ok(r) => {
                println!(
                    "speedtest: 摘要 down={:.0}Mbps up={:.0}Mbps",
                    r.down_bps * 8.0 / 1e6,
                    r.up_bps * 8.0 / 1e6
                );
            }
            Err(e) => {
                eprintln!("speedtest 失败：{e}");
                session.stop();
                std::process::exit(1);
            }
        }
    }

    // ---- 保持（巡检在 Session 内跑；缺省 0 = 立即收工）----
    if a.hold > 0 {
        let deadline = Instant::now() + Duration::from_secs(a.hold);
        while Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(500));
        }
    }

    let e = session.client().snapshot();
    println!("收工：WG 传输层累计 rx={}B tx={}B", e.rx, e.tx);
    session.stop();
}

fn inject(session: &Session, what: &str) {
    match what {
        "poison-socket" => {
            #[cfg(feature = "test-seams")]
            {
                session.client().debug_poison_socket();
                println!("inject: UDP socket 已置为失效形态（EBADF 模拟）");
            }
            #[cfg(not(feature = "test-seams"))]
            {
                let _ = session;
                eprintln!("inject: 需要 test-seams 构建（cargo build -p homeway-cli --features homeway-core/test-seams）");
                std::process::exit(2);
            }
        }
        "relay-lock" => {
            #[cfg(feature = "test-seams")]
            {
                session.debug_suppress_hints();
                println!("inject: 中继锁定（非中继源按从未到达处理——模拟直连全断的真机中继形态）");
            }
            #[cfg(not(feature = "test-seams"))]
            {
                let _ = session;
                eprintln!("inject: 需要 test-seams 构建（cargo build -p homeway-cli --features homeway-core/test-seams）");
                std::process::exit(2);
            }
        }
        other => {
            eprintln!("未知注入：{other}（可用：poison-socket / relay-lock）");
            std::process::exit(2);
        }
    }
}

/// 经隧道拨任意 v4 目标（transit 判据产出步骤）：建连 → 读到对端数据或 EOF 即证通。
fn transit_dial(session: &Session, dst: SocketAddrV4) -> Result<usize, ConnErr> {
    let id = session.client().connect(dst)?;
    // 先写一段载荷：echo 类目标只在收到数据后回显（只 connect+read 会一直阻塞——
    // R1 旧形态的目标是主动发横幅的服务）；写完读首块回显即证通收工
    let probe = b"transit-probe-payload-64b-0123456789abcdef0123456789abcdef";
    let mut off = 0usize;
    let mut zero = 0u32;
    while off < probe.len() {
        match session.client().write(id, probe[off..].to_vec()) {
            Ok(w) if w > 0 => off += w,
            _ => {
                zero += 1;
                if zero > 100_000 {
                    eprintln!("transit: 写探测载荷无进展");
                    let _ = session.client().close(id);
                    return Err(ConnErr::Timeout);
                }
                std::thread::yield_now();
            }
        }
    }
    // 证通即收：echo 类目标不主动 EOF——读到首块回显（或对端 FIN）就关（原「读到
    // 16MB 或 EOF」形态在 echo 目标上会阻塞到会话收工，E11 关闭行拖 5 分钟）
    let got = match session.client().read(id) {
        Ok(chunk) => chunk.len(),
        Err(ConnErr::Closed) => 0, // 对端 FIN——连接本身已证通
        Err(e) => {
            let _ = session.client().close(id);
            return Err(e);
        }
    };
    let _ = session.client().close(id);
    Ok(got)
}

// ---------- speedtest 独立动词（A/B 轮次口径：轮 = 同一会话内一次 run；评审 ③-1） ----------

/// `homeway-cli speedtest --token <hmw1…> [--identity-dir D] [--endpoint-cache-dir D]
///  [--rounds N] [--hold]`——建一次会话跑 N 轮（每轮自带 2s warmup，与 Go daemon
/// `speedtest --state -host` 常驻同会话口径一致）。`--hold` = 跑完保持会话（RSS 采样）。
fn cmd_speedtest(args: &[String]) {
    let mut tok: Option<String> = None;
    let mut identity_dir: Option<PathBuf> = None;
    let mut cache_dir: Option<PathBuf> = None;
    let mut rounds: u32 = 3;
    let mut hold = false;
    let mut dead_direct = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--token" => { i += 1; tok = args.get(i).cloned(); }
            "--identity-dir" => { i += 1; identity_dir = args.get(i).map(PathBuf::from); }
            "--endpoint-cache-dir" => { i += 1; cache_dir = args.get(i).map(PathBuf::from); }
            "--rounds" => { i += 1; rounds = args.get(i).and_then(|s| s.parse().ok()).unwrap_or(3); }
            "--hold" => hold = true,
            "--dead-direct" => dead_direct = true,
            other => { eprintln!("未知参数：{other}"); std::process::exit(2); }
        }
        i += 1;
    }
    let Some(tok) = tok else {
        eprintln!("用法：homeway-cli speedtest --token <hmw1…> [--identity-dir D] [--endpoint-cache-dir D] [--rounds N] [--hold]");
        std::process::exit(2);
    };
    let mut t = match token::decode(&tok) {
        Ok(t) => t,
        Err(e) => { eprintln!("token 解析失败：{e}"); std::process::exit(1); }
    };
    if dead_direct {
        for e in &mut t.endpoints {
            if e.kind == token::EndpointKind::Direct {
                e.addr = "127.0.0.1:1".to_owned();
            }
        }
        println!("inject: token 直连端点已改指死端口（127.0.0.1:1）——只有中继可达");
    }
    let logf: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(|s: &str| println!("{s}"));
    let mut session = match Session::start(SessionConfig {
        token: t,
        identity_dir: identity_dir.or_else(|| Some(PathBuf::from("identity"))),
        endpoint_cache_dir: cache_dir,
        logf: Arc::clone(&logf),
        relay_only: false,
    }) {
        Ok(s) => s,
        Err(e) => { eprintln!("会话建立失败：{e}"); std::process::exit(1); }
    };
    if session.snapshot().state == SessState::Failed {
        eprintln!("会话失败收工");
        std::process::exit(1);
    }
    println!("warmup pong: 就绪（判据=wg）");
    let client = session.client();
    for r in 1..=rounds {
        match speedtest::run(client.as_ref(), Params::default(), &|s| println!("{s}")) {
            Ok(res) => {
                println!(
                    "round {}/{}: down={:.0}Mbps up={:.0}Mbps",
                    r, rounds,
                    res.down_bps * 8.0 / 1e6,
                    res.up_bps * 8.0 / 1e6
                );
            }
            Err(e) => {
                eprintln!("round {r} 失败（reason={}）：{e}", e.reason());
                session.stop();
                std::process::exit(1);
            }
        }
    }
    if hold {
        println!("speedtest: 会话保持中（Ctrl-C 收工）");
        loop {
            std::thread::sleep(Duration::from_secs(3600));
        }
    }
    let e = session.client().snapshot();
    println!("收工：WG 传输层累计 rx={}B tx={}B", e.rx, e.tx);
    session.stop();
}

// ---------- files 动词（每命令一条流；拨号走 healing） ----------

fn cmd_files(args: &[String]) {
    // files <verb> --token <hmw1> [--identity-dir D] [--dead-direct] [--inject no-hint]
    //   [--rate-limit <bytes/s>] <path> [<local>]（--rate-limit 缺省 2MiB/s 发送端速率
    //   义务；0 = 不限、风险自担——对齐 Go files-cli 1.4）
    let mut tok: Option<String> = None;
    let mut identity_dir: Option<PathBuf> = None;
    let mut dead_direct = false;
    let mut inject_what: Option<String> = None;
    let mut rate_limit: Option<i64> = None;
    let mut rest: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--token" => {
                i += 1;
                tok = args.get(i).cloned();
            }
            "--identity-dir" => {
                i += 1;
                identity_dir = args.get(i).map(PathBuf::from);
            }
            "--dead-direct" => dead_direct = true,
            "--inject" => {
                i += 1;
                inject_what = args.get(i).cloned();
            }
            "--rate-limit" => {
                i += 1;
                let v = args.get(i).and_then(|v| v.parse::<i64>().ok());
                match v {
                    Some(n) if n >= 0 => rate_limit = Some(n),
                    Some(_) => {
                        eprintln!("--rate-limit 不接受负值（Go 同款拒绝）——0 = 不限、正数 = bytes/s");
                        std::process::exit(2);
                    }
                    None => {
                        eprintln!("--rate-limit 需要非负整数 bytes/s（如 250000；0 = 不限）");
                        std::process::exit(2);
                    }
                }
            }
            other => rest.push(other.to_owned()),
        }
        i += 1;
    }
    let Some(tok) = tok else {
        eprintln!("用法：homeway-cli files <list|stat|mkdir|read|download|upload> --token <hmw1> [--identity-dir D] [--rate-limit B/s（缺省 2MiB/s；0 不限）] <远端路径> [<本地路径>]");
        std::process::exit(2);
    };
    let mut t = match token::decode(&tok) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("token 解析失败：{e}");
            std::process::exit(1);
        }
    };
    let verb = rest.first().cloned().unwrap_or_default();
    let path = rest.get(1).cloned().unwrap_or_default();
    let local = rest.get(2).cloned().unwrap_or_default();
    if verb.is_empty() || path.is_empty() {
        eprintln!("用法：homeway-cli files <verb> --token <hmw1> [--dead-direct] [--inject relay-lock] [--rate-limit B/s] <远端路径> [<本地路径>]");
        std::process::exit(2);
    }
    if dead_direct {
        for e in &mut t.endpoints {
            if e.kind == token::EndpointKind::Direct {
                e.addr = "127.0.0.1:1".to_owned();
            }
        }
        println!("inject: token 直连端点已改指死端口（127.0.0.1:1）——只有中继可达");
    }
    let logf: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(|s: &str| println!("{s}"));
    let mut session = match Session::start(SessionConfig {
        token: t,
        identity_dir: identity_dir.or_else(|| Some(PathBuf::from("identity"))),
        endpoint_cache_dir: None,
        logf: Arc::clone(&logf),
        relay_only: inject_what.as_deref() == Some("relay-lock"),
    }) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("会话建立失败：{e}");
            std::process::exit(1);
        }
    };
    if let Some(what) = &inject_what {
        inject(&session, what);
    }
    if session.snapshot().state == SessState::Failed {
        eprintln!("会话失败收工");
        std::process::exit(1);
    }
    let budget = Duration::from_secs(30);
    let r: Result<(), homeway_core::files::FilesError> = match verb.as_str() {
        "list" => homeway_core::files::list(&session, budget, &path).map(|entries| {
            for e in entries {
                let kind = if e.is_dir { "dir " } else { "file" };
                println!("{kind} {:>12}  {}", e.size, e.name);
            }
        }),
        "stat" => homeway_core::files::stat(&session, budget, &path).map(|e| {
            println!("{} {} size={} mtimeMs={} mode={}", if e.is_dir { "dir" } else { "file" }, e.name, e.size, e.mtime_ms, e.mode);
        }),
        "mkdir" => homeway_core::files::mkdir(&session, budget, &path).map(|_| println!("已建目录 {path}")),
        "read" => homeway_core::files::read(&session, budget, &path, "", 1 << 20).map(|r| {
            print!("{}", r.text);
        }),
        "download" => {
            let mut out: Box<dyn std::io::Write> = if local == "-" {
                Box::new(std::io::stdout())
            } else {
                Box::new(std::fs::File::create(&local).unwrap_or_else(|e| {
                    eprintln!("本地文件建不了（{local}）：{e}");
                    std::process::exit(1);
                }))
            };
            homeway_core::files::download(&session, budget, &path, &mut out, |size| {
                eprintln!("服务端声明 {size} 字节");
            })
            .map(|n| println!("下载完成 {n} 字节 → {local}"))
        }
        "upload" => {
            let mut f = std::fs::File::open(&local).unwrap_or_else(|e| {
                eprintln!("本地文件打不开（{local}）：{e}");
                std::process::exit(1);
            });
            let size = f.metadata().map(|m| m.len() as i64).unwrap_or(0);
            // 发送端速率义务：缺省 2MiB/s（对齐 Go files-cli；--rate-limit 可改/0 不限）
            let rate = rate_limit.unwrap_or(homeway_core::files::DEFAULT_RATE_LIMIT);
            let mut limiter = homeway_core::files::UploadLimiter::new(rate);
            homeway_core::files::upload(&session, budget, &path, &mut f, size, |done| {
                if done % (64 << 20) == 0 {
                    eprintln!("上传进度 {done}/{size}");
                }
            }, limiter.as_mut())
            .map(|n| println!("上传完成 {n} 字节 → {path}"))
        }
        other => {
            eprintln!("未知动词：{other}");
            std::process::exit(2);
        }
    };
    session.stop();
    if let Err(e) = r {
        eprintln!("files {verb} 失败：{e}");
    }
}

// ---------- dnstest（DNS 代答实测 + E12 出口 UDP 面采样；R3-3f 判据产出步骤） ----------

/// `homeway-cli dnstest --token <hmw1> [--identity-dir D] [--mode tcp5300|udp53|leg] <域名>`
///   tcp5300：隧道 IP:<解析腿端口> TCP（客户端远程解析腿——qtcp 计数面）
///   udp53：隧道 IP:53 UDP（手机声明的 DNS——栈内 listener 面，q 计数）
///   leg：8.8.8.8:53 UDP（非隧道 IP 的 :53——拦截层进程内腿 + E12 dns 会话行）
fn cmd_dnstest(args: &[String]) {
    let mut tok: Option<String> = None;
    let mut identity_dir: Option<PathBuf> = None;
    let mut mode = "tcp5300".to_owned();
    let mut name = String::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--token" => {
                i += 1;
                tok = args.get(i).cloned();
            }
            "--identity-dir" => {
                i += 1;
                identity_dir = args.get(i).map(PathBuf::from);
            }
            "--mode" => {
                i += 1;
                mode = args.get(i).cloned().unwrap_or_else(|| "tcp5300".into());
            }
            other if !other.starts_with('-') => name = other.to_owned(),
            other => {
                eprintln!("未知参数：{other}");
                std::process::exit(2);
            }
        }
        i += 1;
    }
    let Some(tok) = tok else {
        eprintln!("用法：homeway-cli dnstest --token <hmw1…> [--mode tcp5300|udp53|leg] <域名>");
        std::process::exit(2);
    };
    if name.is_empty() {
        eprintln!("dnstest 需要 <域名>（如 example.com）");
        std::process::exit(2);
    }
    let t = match token::decode(&tok) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("token 解析失败：{e}");
            std::process::exit(1);
        }
    };
    let logf: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(|s: &str| println!("{s}"));
    let mut session = match homeway_core::session::Session::start(homeway_core::session::SessionConfig {
        token: t,
        identity_dir: identity_dir.or_else(|| Some(PathBuf::from("identity"))),
        endpoint_cache_dir: None,
        logf: Arc::clone(&logf),
        relay_only: false,
    }) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("会话建立失败：{e}");
            std::process::exit(1);
        }
    };
    if session.snapshot().state == SessState::Failed {
        eprintln!("会话失败收工");
        std::process::exit(1);
    }
    // 构造 A 查询
    let mut q = Vec::new();
    q.extend_from_slice(&0x1234u16.to_be_bytes());
    q.extend_from_slice(&[0x01, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0]);
    for label in name.split('.') {
        q.push(label.len() as u8);
        q.extend_from_slice(label.as_bytes());
    }
    q.push(0);
    q.extend_from_slice(&[0x00, 0x01, 0x00, 0x01]);

    let client = session.client();
    let r = match mode.as_str() {
        "tcp5300" => dns_tcp(&client, &q),
        "udp53" => dns_udp(&client, &q, homeway_core::wgcore::SERVER_TUNNEL_IP, 53),
        "leg" => dns_udp(&client, &q, std::net::Ipv4Addr::new(8, 8, 8, 8), 53),
        other => {
            eprintln!("未知 --mode {other:?}（可用：tcp5300 / udp53 / leg）");
            session.stop();
            std::process::exit(2);
        }
    };
    session.stop();
    match r {
        Ok(resp) => {
            let rcode = resp.get(3).map(|b| b & 0x0F).unwrap_or(9);
            let anc = resp.get(6).and_then(|h| resp.get(7).map(|l| (u16::from(*h) << 8) | u16::from(*l))).unwrap_or(0);
            println!("dnstest[{mode}] {name}: rcode={rcode} answers={anc} bytes={}", resp.len());
            if rcode == 0 && anc > 0 {
                println!("（出口侧应见 dns: 计数行——E22 判据）");
            }
        }
        Err(e) => {
            eprintln!("dnstest[{mode}] 失败：{e}");
            std::process::exit(1);
        }
    }
}

fn dns_tcp(client: &homeway_core::wgcore::Client, q: &[u8]) -> Result<Vec<u8>, String> {
    let dst = SocketAddrV4::new(homeway_core::wgcore::SERVER_TUNNEL_IP, 5300);
    let id = client.connect(dst).map_err(|e| e.to_string())?;
    let mut frame = Vec::with_capacity(2 + q.len());
    frame.extend_from_slice(&(q.len() as u16).to_be_bytes());
    frame.extend_from_slice(q);
    // 写
    let mut off = 0;
    while off < frame.len() {
        let n = client.write(id, frame[off..].to_vec()).map_err(|e| e.to_string())?;
        if n == 0 {
            std::thread::yield_now();
            continue;
        }
        off += n;
    }
    // 读：2B 长度 + 报文
    let mut buf = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(6);
    while buf.len() < 2 && Instant::now() < deadline {
        match client.read(id) {
            Ok(chunk) if !chunk.is_empty() => buf.extend_from_slice(&chunk),
            Err(homeway_core::wgcore::ConnErr::Closed) => break,
            _ => std::thread::sleep(Duration::from_millis(20)),
        }
    }
    if buf.len() < 2 {
        let _ = client.close(id);
        return Err("6s 内未读到响应长度前缀".into());
    }
    let mlen = u16::from_be_bytes([buf[0], buf[1]]) as usize;
    while buf.len() < 2 + mlen && Instant::now() < deadline {
        match client.read(id) {
            Ok(chunk) if !chunk.is_empty() => buf.extend_from_slice(&chunk),
            Err(homeway_core::wgcore::ConnErr::Closed) => break,
            _ => std::thread::sleep(Duration::from_millis(20)),
        }
    }
    let _ = client.close(id);
    if buf.len() < 2 + mlen {
        return Err(format!("响应不完整（{}/{}）", buf.len() - 2, mlen));
    }
    Ok(buf[2..2 + mlen].to_vec())
}

fn dns_udp(
    client: &homeway_core::wgcore::Client,
    q: &[u8],
    ip: std::net::Ipv4Addr,
    port: u16,
) -> Result<Vec<u8>, String> {
    let (id, local_port) = client.udp_open().map_err(|e| e.to_string())?;
    println!("dnstest: UDP 源端口 {local_port} → {ip}:{port}");
    client
        .udp_send(id, SocketAddrV4::new(ip, port), q.to_vec())
        .map_err(|e| e.to_string())?;
    // 收（整体预算 6s——recv 阻塞面由看门狗线程兜）
    let (tx, rx) = std::sync::mpsc::channel();
    let c2 = unsafe_client(client);
    std::thread::spawn(move || {
        let r = c2.udp_recv(id).map_err(|e| e.to_string());
        let _ = tx.send(r);
    });
    match rx.recv_timeout(Duration::from_secs(6)) {
        Ok(Ok((data, from))) => {
            let _ = client.udp_close(id);
            println!("dnstest: 应答来自 {from}（{} 字节）", data.len());
            Ok(data)
        }
        Ok(Err(e)) => Err(e),
        Err(_) => {
            let _ = client.udp_close(id);
            Err("6s 内无应答".into())
        }
    }
}

/// udp_recv 的跨线程调用面（Client 是 & 引用——Send 边界用原始指针横传；调用方
/// 保证生命周期（session 活到函数尾）——测试动词专用，不进 core）。
fn unsafe_client(c: &homeway_core::wgcore::Client) -> &'static homeway_core::wgcore::Client {
    unsafe { &*(c as *const _) }
}

// ---------- portfwd（本地 127.0.0.1 监听 → 经隧道拨目标；CLI 测试动词） ----------

fn cmd_portfwd(args: &[String]) {
    let mut tok: Option<String> = None;
    let mut identity_dir: Option<PathBuf> = None;
    let mut maps: Vec<(u16, Option<SocketAddrV4>)> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--token" => {
                i += 1;
                tok = args.get(i).cloned();
            }
            "--identity-dir" => {
                i += 1;
                identity_dir = args.get(i).map(PathBuf::from);
            }
            "--map" => {
                i += 1;
                let spec = args.get(i).cloned().unwrap_or_default();
                let parts: Vec<&str> = spec.split(':').collect();
                let parsed = match parts.as_slice() {
                    [l, tport] => (
                        l.parse().ok(),
                        tport.parse().ok().map(|p: u16| {
                            SocketAddrV4::new(homeway_core::wgcore::SERVER_TUNNEL_IP, p)
                        }),
                    ),
                    // v6 形态暂不支持（本地实例恒 v4 字面量；`ip:port` 已由 3 段分支覆盖）
                    [l, ip, tport] => (
                        l.parse().ok(),
                        ip.parse().ok().zip(tport.parse().ok()).map(|(a, p)| SocketAddrV4::new(a, p)),
                    ),
                    _ => (None, None),
                };
                match parsed {
                    (Some(listen), target) if listen > 0 => maps.push((listen, target)),
                    _ => {
                        eprintln!("--map 需要 <listen>:<ip:port|port> 形态（得 {spec}）");
                        std::process::exit(2);
                    }
                }
            }
            other => {
                eprintln!("未知参数：{other}");
                std::process::exit(2);
            }
        }
        i += 1;
    }
    let Some(tok) = tok else {
        eprintln!("用法：homeway-cli portfwd --token <hmw1> --map 15432:5432 [--map 15433:1.2.3.4:5432]");
        std::process::exit(2);
    };
    if maps.is_empty() {
        eprintln!("至少一条 --map");
        std::process::exit(2);
    }
    let t = match token::decode(&tok) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("token 解析失败：{e}");
            std::process::exit(1);
        }
    };
    let logf: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(|s: &str| println!("{s}"));
    let session = match Session::start(SessionConfig {
        token: t,
        identity_dir: identity_dir.or_else(|| Some(PathBuf::from("identity"))),
        endpoint_cache_dir: None,
        logf: Arc::clone(&logf),
        relay_only: false,
    }) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("会话建立失败：{e}");
            std::process::exit(1);
        }
    };
    if session.snapshot().state == SessState::Failed {
        eprintln!("会话失败收工");
        std::process::exit(1);
    }
    // CLI 测试动词形态：Session 泄漏成 'static（本命令永不返回；单条失败只记
    // 状态、不阻断其它映射——Go setPortForwards 同义）
    let sess: &'static Session = Box::leak(Box::new(session));
    for (listen, target) in maps {
        let target = target.map(|t| {
            if t.ip() == &homeway_core::wgcore::SERVER_TUNNEL_IP && t.port() == 0 {
                SocketAddrV4::new(homeway_core::wgcore::SERVER_TUNNEL_IP, listen)
            } else {
                t
            }
        });
        let target_text = match target {
            Some(t) if t.ip() != &homeway_core::wgcore::SERVER_TUNNEL_IP => format!("{}:{}", t.ip(), t.port()),
            Some(t) => {
                if t.port() == listen {
                    "主机（同端口）".to_owned()
                } else {
                    format!("主机:{}", t.port())
                }
            }
            None => "主机（同端口）".to_owned(),
        };
        let listener = match std::net::TcpListener::bind(("127.0.0.1", listen)) {
            Ok(l) => l,
            Err(e) => {
                println!("port-forward: 监听 127.0.0.1:{listen} 失败（{e}）——该条映射不可用，不影响隧道 [code={}]", homeway_core::PortfwdErr::BindFailed);
                continue;
            }
        };
        println!("port-forward: 127.0.0.1:{listen} -> {target_text} 监听中");
        let dst = target.expect("解析期已保证 Some");
        std::thread::spawn(move || {
            for conn in std::iter::repeat_with(|| listener.accept().map(|(c, _)| c)) {
                let local_conn = match conn {
                    Ok(c) => c,
                    Err(_) => continue,
                };
                // 拨远端（恢复感知；15s = Go portfwd dialTimeout）
                let Ok(remote_id) = sess.healing_dial_addr(dst, Duration::from_secs(15)) else {
                    continue;
                };
                let w_conn = match local_conn.try_clone() {
                    Ok(c) => c,
                    Err(_) => {
                        let _ = sess.client().close(remote_id);
                        continue;
                    }
                };
                // 上行：本地 → 隧道
                std::thread::spawn(move || {
                    let mut lc = local_conn;
                    let mut buf = [0u8; 16 * 1024];
                    loop {
                        match std::io::Read::read(&mut lc, &mut buf) {
                            Ok(0) | Err(_) => {
                                let _ = sess.client().shutdown(remote_id);
                                return;
                            }
                            Ok(n) => {
                                let mut off = 0;
                                while off < n {
                                    match sess.client().write(remote_id, buf[off..n].to_vec()) {
                                        Ok(w) if w > 0 => off += w,
                                        _ => {
                                            let _ = lc.shutdown(std::net::Shutdown::Both);
                                            return;
                                        }
                                    }
                                }
                            }
                        }
                    }
                });
                // 下行：隧道 → 本地
                std::thread::spawn(move || {
                    let mut wc = w_conn;
                    loop {
                        match sess.client().read(remote_id) {
                            Ok(chunk) if !chunk.is_empty() => {
                                let mut off = 0;
                                while off < chunk.len() {
                                    match std::io::Write::write(&mut wc, &chunk[off..]) {
                                        Ok(w) if w > 0 => off += w,
                                        _ => {
                                            let _ = sess.client().close(remote_id);
                                            return;
                                        }
                                    }
                                }
                            }
                            _ => {
                                let _ = wc.shutdown(std::net::Shutdown::Both);
                                return;
                            }
                        }
                    }
                });
            }
        });
    }
    println!("portfwd: 全部映射已起（Ctrl-C 收工）");
    loop {
        std::thread::sleep(Duration::from_secs(3600));
    }
}

fn hex_str(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}
