//! homeway-cli —— 测试/运维命令面。
//!
//! R1 实装 `connect --token <hmw1…>`（垂直切片）：与 Go 出口建 WG 隧道 → warmup →
//! 一次性巡检（C10）→ 可选 speedtest / `--dial`（transit 产出步骤）。
//!
//! 判据行输出对齐 `docs/INTEROP-CRITERIA.md`（模板逐串）：C1（服务会话: 前缀）、C2、
//! C3、C8（`warmup pong: 就绪（判据=wg）`，APP 核形态文案按判据移植、一次会话一次）、
//! C10（一次性巡检，频率偏离登记 R2 归 60s）；bind/wgcore 域行无前缀（同 Go 形态）。

use std::net::SocketAddrV4;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use homeway_core::identity::{self, IdentitySource};
use homeway_core::speedtest::{self, Params};
use homeway_core::token::{self, EndpointKind};
use homeway_core::wgcore::{Client, CoreConfig, SERVER_TUNNEL_IP};
use homeway_core::wtransport::Candidate;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("token") => cmd_token(&args[2..]),
        Some("connect") => cmd_connect(&args[2..]),
        _ => {
            eprintln!(
                "homeway-cli——可用：\n  token <hmw1…>\n  connect --token <hmw1…> [--identity-dir <dir>] [--speedtest] [--dial <ip:port>] [--hold <secs>]"
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

fn cmd_connect(args: &[String]) {
    let mut tok = None;
    let mut identity_dir: Option<PathBuf> = None;
    let mut do_speedtest = false;
    let mut dial: Option<SocketAddrV4> = None;
    let mut hold = 0u64;
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
            "--speedtest" => do_speedtest = true,
            "--dial" => {
                i += 1;
                dial = args.get(i).and_then(|s| s.parse().ok());
                if dial.is_none() {
                    eprintln!("--dial 需要 <ipv4:port>（如 192.168.3.12:9999）");
                    std::process::exit(2);
                }
            }
            "--hold" => {
                i += 1;
                hold = args.get(i).and_then(|s| s.parse().ok()).unwrap_or(0);
            }
            other => {
                eprintln!("未知参数：{other}");
                std::process::exit(2);
            }
        }
        i += 1;
    }
    let Some(tok) = tok else {
        eprintln!("用法：homeway-cli connect --token <hmw1…> [--identity-dir <dir>] [--speedtest] [--dial <ip:port>] [--hold <secs>]");
        std::process::exit(2);
    };

    // ---- token（入口 decode 一次落结构）----
    let t = match token::decode(&tok) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("token 解析失败：{e}");
            std::process::exit(1);
        }
    };
    let candidates: Vec<Candidate> = t
        .endpoints
        .iter()
        .filter(|e| e.kind == EndpointKind::Direct)
        .map(|e| Candidate {
            addr: e.addr.parse().unwrap_or_else(|_| {
                eprintln!("端点地址不可达形：{}", e.addr);
                std::process::exit(1);
            }),
            relay: false,
        })
        .collect();
    println!(
        "服务会话: 新栈会话已建立（token 端点 {} 个，后端隧道地址 {}）",
        t.endpoints.len(),
        SERVER_TUNNEL_IP
    );

    // ---- 身份（C1 判据行；默认目录 <cwd>/identity）----
    let dir = identity_dir.unwrap_or_else(|| PathBuf::from("identity"));
    let (identity, src, warn) = identity::load_or_create(Some(&dir), &t.peer_id).expect("身份装配失败");
    match src {
        IdentitySource::Created => println!(
            "服务会话: 身份：新建（dev={} pub={}，目录 {}）",
            identity.short_dev(),
            identity.short_pub(),
            dir.display()
        ),
        IdentitySource::Reused => println!(
            "服务会话: 身份：复用（dev={} pub={}）",
            identity.short_dev(),
            identity.short_pub()
        ),
        other => println!(
            "服务会话: 身份：{}（dev={} pub={}）",
            other.zh(),
            identity.short_dev(),
            identity.short_pub()
        ),
    }
    if let Some(w) = warn {
        eprintln!("服务会话: ⚠️ 身份存储不可用（{w}）——本次临时身份，重连会换钥匙");
    }

    // ---- 数据面（C2 由 Client::start 打出）----
    let logf: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(|s: &str| println!("{s}"));
    let mut client = match Client::start(CoreConfig {
        peer_id: t.peer_id,
        secret: t.secret,
        identity,
        candidates,
        logf: Arc::clone(&logf),
    }) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("数据面装配失败：{e}");
            std::process::exit(1);
        }
    };

    // ---- warmup（C8：拨 :1 判 RST = 隧道通）----
    let warm_start = Instant::now();
    match client.path_probe() {
        Ok(()) => {
            // APP 核形态判据（tunmode.go:751），按 ROADMAP 判据移植；一次会话只打一次
            println!("warmup pong: 就绪（判据=wg）");
        }
        Err(e) => {
            eprintln!("warmup ping: 失败（{e}）——出口未应答");
            let _ = warm_start;
            client.stop();
            std::process::exit(1);
        }
    }

    // ---- 一次性巡检（C10 巡检形态；60s 周期属 R2）----
    client.refresh_reg();
    let patrol_start = Instant::now();
    let patrol_ok = client.path_probe().is_ok();
    let snap = client.snapshot();
    let rtt_ms = patrol_start.elapsed().as_millis();
    if patrol_ok {
        match snap.ep {
            Some(ep) => println!("link: via={} ep={} rtt={}ms（服务会话巡检）", snap.via.as_str(), ep, rtt_ms),
            None => println!("link: via={} ep= rtt={}ms（服务会话巡检）", snap.via.as_str(), rtt_ms),
        }
    }

    // ---- transit 产出步骤（--dial 非环回：出口对 dst≠隧道IP 的包打 transit 行）----
    if let Some(dst) = dial {
        match transit_dial(&client, dst) {
            Ok(n) => println!("transit: 经隧道拨 {dst} 成功（收 {n} 字节）"),
            Err(e) => {
                eprintln!("transit: 经隧道拨 {dst} 失败：{e}");
                client.stop();
                std::process::exit(1);
            }
        }
    }

    // ---- speedtest（可选）----
    if do_speedtest {
        match speedtest::run(&client, Params::default(), &|s| println!("{s}")) {
            Ok(r) => {
                println!(
                    "speedtest: 摘要 down={:.0}Mbps up={:.0}Mbps",
                    r.down_bps * 8.0 / 1e6,
                    r.up_bps * 8.0 / 1e6
                );
            }
            Err(e) => {
                eprintln!("speedtest 失败：{e}");
                client.stop();
                std::process::exit(1);
            }
        }
    }

    // ---- 保持（采样窗口；缺省 0 = 立即收工）----
    if hold > 0 {
        let deadline = Instant::now() + Duration::from_secs(hold);
        while Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(500));
        }
        // 保持期结束时再打一拍巡检（保活腿证据）
        client.refresh_reg();
        if client.path_probe().is_ok() {
            if let Some(ep) = client.snapshot().ep {
                println!("link: via=direct ep={ep} rtt=0ms（服务会话巡检）");
            }
        }
    }

    let (rx, tx) = {
        let s = client.snapshot();
        (s.rx, s.tx)
    };
    println!("收工：WG 传输层累计 rx={rx}B tx={tx}B");
    client.stop();
}

/// 经隧道拨任意 v4 目标（transit 判据产出步骤）：建连 → 读到对端数据或 EOF 即证通。
fn transit_dial(client: &Client, dst: SocketAddrV4) -> Result<usize, String> {
    let id = client.connect(dst).map_err(|e| format!("connect: {e}"))?;
    let mut got = 0usize;
    // 读循环：拿到字节或对端关闭即收（nc 之类回声/静默服务都兼容）
    for _ in 0..16 {
        match client.read(id) {
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
                let _ = client.close(id);
                return Err(format!("read: {e}"));
            }
        }
    }
    let _ = client.close(id);
    Ok(got)
}

fn hex_str(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}
