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

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("token") => cmd_token(&args[2..]),
        Some("connect") => cmd_connect(&args[2..]),
        Some("files") => cmd_files(&args[2..]),
        Some("portfwd") => cmd_portfwd(&args[2..]),
        _ => {
            eprintln!(
                "homeway-cli——可用：\n  token <hmw1…>\n  connect --token <hmw1…> [--identity-dir <dir>] [--endpoint-cache-dir <dir>]\n      [--speedtest] [--dial <ip:port>] [--hold <secs>] [--probe N] [--status-json]\n      [--recover-from <1|2|3> [--recover-cause <s>]] [--inject poison-socket]（注入需 test-seams 构建）"
            );
            std::process::exit(2);
        }
    }
}

fn cmd_token(args: &[String]) {
    match args.first() {
        Some(s) => match token::decode(s) {
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
        },
        None => {
            eprintln!("用法：homeway-cli token <hmw1…>");
            std::process::exit(2);
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
        other => {
            eprintln!("未知注入：{other}（可用：poison-socket）");
            std::process::exit(2);
        }
    }
}

/// 经隧道拨任意 v4 目标（transit 判据产出步骤）：建连 → 读到对端数据或 EOF 即证通。
fn transit_dial(session: &Session, dst: SocketAddrV4) -> Result<usize, ConnErr> {
    let id = session.client().connect(dst)?;
    let mut got = 0usize;
    loop {
        match session.client().read(id) {
            Ok(chunk) => {
                if chunk.is_empty() {
                    break; // EOF（对端关）——连接本身已证通
                }
                got += chunk.len();
                if got > 16 * 1024 * 1024 {
                    break; // 测试面护栏：16MB 足够证通
                }
            }
            Err(ConnErr::Closed) => {
                // 对端 FIN（CloseWait EOF）——干净收尾：已收字节即结果
                break;
            }
            Err(e) => {
                let _ = session.client().close(id);
                return Err(e);
            }
        }
    }
    let _ = session.client().close(id);
    Ok(got)
}

// ---------- files 动词（每命令一条流；拨号走 healing） ----------

fn cmd_files(args: &[String]) {
    // files <verb> --token <hmw1> [--identity-dir D] <path> [<local>]
    let mut tok: Option<String> = None;
    let mut identity_dir: Option<PathBuf> = None;
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
            other => rest.push(other.to_owned()),
        }
        i += 1;
    }
    let Some(tok) = tok else {
        eprintln!("用法：homeway-cli files <list|stat|mkdir|read|download|upload> --token <hmw1> [--identity-dir D] <远端路径> [<本地路径>]");
        std::process::exit(2);
    };
    let t = match token::decode(&tok) {
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
        eprintln!("用法：homeway-cli files <verb> --token <hmw1> <远端路径> [<本地路径>]");
        std::process::exit(2);
    }
    let logf: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(|s: &str| println!("{s}"));
    let mut session = match Session::start(SessionConfig {
        token: t,
        identity_dir: identity_dir.or_else(|| Some(PathBuf::from("identity"))),
        endpoint_cache_dir: None,
        logf: Arc::clone(&logf),
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
            homeway_core::files::upload(&session, budget, &path, &mut f, size, |done| {
                if done % (64 << 20) == 0 {
                    eprintln!("上传进度 {done}/{size}");
                }
            })
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
                    [l, ip1, ip2, ip3, tport] => {
                        let ip = format!("{ip1}.{ip2}.{ip3}").parse().ok();
                        (l.parse().ok(), ip.zip(tport.parse().ok()).map(|(a, p)| SocketAddrV4::new(a, p)))
                    }
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
                println!("port-forward: 监听 127.0.0.1:{listen} 失败（{e}）——该条映射不可用，不影响隧道 [code=bind_failed]");
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
