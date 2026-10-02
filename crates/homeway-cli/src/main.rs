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
    inject: Option<String>,
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
        inject: None,
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
            "--recover-cause" => {
                i += 1;
                a.recover_cause = args.get(i).cloned().unwrap_or_else(|| "测试注入".into());
            }
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
    let t = match token::decode(&tok) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("token 解析失败：{e}");
            std::process::exit(1);
        }
    };

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

    // ---- 恢复钩子（时间窗实测：阶梯直测面）----
    if let Some(from) = a.recover_from {
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
    for _ in 0..16 {
        match session.client().read(id) {
            Ok(chunk) => {
                if chunk.is_empty() {
                    break; // EOF（对端关）——连接本身已证通
                }
                got += chunk.len();
                if got > 64 * 1024 {
                    break;
                }
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

fn hex_str(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}
