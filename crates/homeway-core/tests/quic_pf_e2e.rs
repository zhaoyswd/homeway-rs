//! **portfwd 面**的本地端到端（M4 S1–S3 的收口读数台；S5 在此扩「四形态 × 三失败」与
//! spec R1–R5 的本机面）。
//!
//! 走的是**真产品路径**：`ClientCore::tun_prepare(transport=quic, portForwards=[…])` +
//! `tun_attach(假 TUN fd)` ⇒ 世代装配 ⇒ pf 运行时真监听 `127.0.0.1:<listen>` ⇒ 本机应用连它
//! ⇒ 经 `STREAM[dial]` 拨到**出口**（出口 dial 腿真拨 OS）⇒ 出口回环上的真 echo ⇒ 回程。
//!
//! **判据（M4）**：
//! ① 映射的建立与访问（spec R1-S①）：真监听 + 字节逐字往返 + 出口侧 `tag=dial` 受理行；
//! ② **负判据**：本轮出口日志**零** `intercept: tcp exempt …:<listen>`——服务流不再走 WG
//!    服务腿（与 M3 S3 的 app-core 用例同款负判据）；
//! ③ **流槽泄漏判据（r19 H1；S2 完成判据）**：连续 N（> 62 = 岛内服务流容量）次「目标拒绝」
//!    失败拨号后，**第 N 次的拒因仍是「目标拒绝」而不是「入口队列满」**（= 失败路径真关流），
//!    且随后同一世代上的另一条映射仍能完成往返。
//!    **N 取 80 而不是 63**：pf 的失败行有节流（首 5 + 每 20：`portfwd.rs` 的
//!    `n <= 5 || n % 20 == 0`）⇒ 只有 #60/#80 这类行可见；80 > 62 幅度更大，等效且可读。
//!
//! **环境契约**（与 `quic_island_e2e.rs` 同一套；驱动脚本 `tools/quic-pf-e2e.sh` = S5 交付）：
//! - `HOMEWAY_ISLAND_E2E_TOKEN`：出口 token（`tools/local-rust-exit.sh token N` 的输出里抽 `hmw1…`）
//! - `HOMEWAY_ISLAND_E2E_EXIT_LOG`：出口 stdout 日志
//!
//! **本机禁网提示（设计 §0.4-2）**：本机 `utun4` 上有 fake-IP 代理且是默认路由 ⇒
//! 「非直连网段」的目标会被代理接住 ⇒ 本文件的**出口侧目标一律用回环**（`targetIp` 空 =
//! 「出口本机」语义位 ⇒ wire `127.0.0.1`），不拿「能不能连外网」当判据。

use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::os::fd::AsRawFd as _;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use homeway_core::facade::demand::DemandSignals;
use homeway_core::facade::tun_exec::TunnelExec;
use homeway_core::facade::ClientCore;

/// 轮询上界（只判上界——flake 口径②）。
const WAIT: Duration = Duration::from_secs(30);
/// 泄漏用例的失败拨号次数（> 62 = 岛内服务流容量；见文件头③）。
const LEAK_DIALS: u32 = 80;

/// 本机空闲 TCP 端口（bind 后立即关；**不钉固定端口**——同树并发跑互撞）。
fn free_port() -> u16 {
    let l = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("探测端口可绑");
    let p = l.local_addr().expect("探测地址").port();
    drop(l);
    p
}

/// 起一枚回环 echo（真目标；读多少回多少并在 EOF 时半关）。
fn spawn_echo() -> (u16, Arc<std::sync::atomic::AtomicBool>) {
    let ln = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("echo 可绑");
    let port = ln.local_addr().expect("echo 地址").port();
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let s2 = Arc::clone(&stop);
    std::thread::spawn(move || {
        use std::io::{Read, Write};
        for c in ln.incoming() {
            if s2.load(std::sync::atomic::Ordering::SeqCst) {
                break;
            }
            let Ok(mut c) = c else { break };
            let mut buf = [0u8; 4096];
            loop {
                match c.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if c.write_all(&buf[..n]).is_err() {
                            break;
                        }
                    }
                }
            }
            let _ = c.shutdown(std::net::Shutdown::Write);
        }
    });
    (port, stop)
}

/// 起一枚已连上本地出口的世代（`tun_prepare` + `tun_attach`；返回 core / 隧道日志路径 / 假 TUN 端 / 出口日志）。
fn start_generation(
    token_str: &str,
    rules_json: &str,
    tag: &str,
) -> (
    Arc<ClientCore>,
    PathBuf,
    std::os::unix::net::UnixDatagram,
    PathBuf,
    std::os::unix::net::UnixDatagram,
) {
    let exit_log = PathBuf::from(
        std::env::var("HOMEWAY_ISLAND_E2E_EXIT_LOG").expect("须给 HOMEWAY_ISLAND_E2E_EXIT_LOG"),
    );
    let dir = std::env::temp_dir().join(format!("hw-m4-pf-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("临时目录可建");
    let out = dir.join("gen.log");
    let ident_dir = dir.join("identity");
    let _ = std::fs::remove_file(&out);
    let cfg = format!(
        r#"{{"token":"{token_str}","out":"{}","identityDir":"{}","transport":"quic","portForwards":{rules_json}}}"#,
        out.display(),
        ident_dir.display()
    );
    let demand = Arc::new(DemandSignals::new());
    let exec = TunnelExec::new(Arc::clone(&demand));
    let core = Arc::new(ClientCore::with_shared(exec, demand));
    assert_eq!(core.tun_prepare(&cfg, true), 0, "prepare 受理");
    let deadline = Instant::now() + WAIT;
    while !core.tun_status().contains("\"state\":\"ready\"") && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        core.tun_status().contains("\"state\":\"ready\""),
        "世代须 ready：{}",
        core.tun_status()
    );
    // 假 TUN fd（socketpair(DGRAM)，照 `quic_island_e2e.rs` 先例）：pf 的装表在 attach 段。
    let (tun, peer) = std::os::unix::net::UnixDatagram::pair().expect("socketpair(DGRAM)");
    assert_eq!(core.tun_attach(tun.as_raw_fd(), 1280), 0, "attach 受理（fd 交岛）");
    let deadline = Instant::now() + WAIT;
    while !core.tun_status().contains("\"state\":\"attached\"") && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(core.tun_status().contains("\"state\":\"attached\""), "世代须 attached");
    // **假 TUN 的两端都要活着**（岛只借 fd、不持所有权 ⇒ 本端一 drop，岛读就 EBADF ⇒
    // 世代转 unhealthy；照 `quic_island_e2e.rs` 先例）
    (core, out, peer, exit_log, tun)
}

/// 等某条映射进入 `listening`（状态面真值；`portForwards[].state`）。
fn wait_listening(core: &Arc<ClientCore>, listen: u16, wait: Duration) -> serde_json::Value {
    let deadline = Instant::now() + wait;
    loop {
        let v: serde_json::Value =
            serde_json::from_str(&core.tun_status()).expect("tun_status 是 JSON");
        if let Some(arr) = v["portForwards"].as_array() {
            if let Some(r) = arr.iter().find(|r| r["listen"] == listen) {
                if r["state"] == "listening" {
                    return r.clone();
                }
            }
        }
        assert!(Instant::now() < deadline, "映射 {listen} 未在 {wait:?} 内进入 listening");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// 经本机映射做一次往返（返回读到的字节；失败给 `Err`）。
fn round_trip(listen: u16, payload: &[u8]) -> std::io::Result<Vec<u8>> {
    use std::io::{Read, Write};
    let mut c = TcpStream::connect((Ipv4Addr::LOCALHOST, listen))?;
    c.set_read_timeout(Some(WAIT)).ok();
    c.write_all(payload)?;
    let mut got = vec![0u8; payload.len()];
    c.read_exact(&mut got)?;
    Ok(got)
}

/// 有界等日志文件里出现 `needle` 的行（返回该行）。
fn wait_line(path: &PathBuf, needle: &str, wait: Duration) -> Option<String> {
    let deadline = Instant::now() + wait;
    while Instant::now() < deadline {
        if let Ok(s) = std::fs::read_to_string(path) {
            if let Some(l) = s.lines().find(|l| l.contains(needle)) {
                return Some(l.to_owned());
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    None
}

/// **判据（①②：映射建立与访问 + 负判据）**：`targetIp=""`（= 「出口本机」语义位）⇒
/// wire `127.0.0.1:<targetPort>` ⇒ 出口 dial 腿真拨回环 ⇒ 字节逐字往返；出口侧出现
/// `tag=dial` 受理行；**零** `intercept: tcp exempt`（服务流不再走 WG 服务腿）。
#[test]
#[ignore = "端到端（portfwd 经 STREAM[dial]）：需本地 QUIC 出口在跑（S5 的驱动脚本）"]
fn port_forward_rides_stream_dial_against_local_exit() {
    let token_str = std::env::var("HOMEWAY_ISLAND_E2E_TOKEN").expect("须给 HOMEWAY_ISLAND_E2E_TOKEN");
    let (echo_port, echo_stop) = spawn_echo();
    let listen = free_port();
    let rules = format!(r#"[{{"listen":{listen},"targetIp":"","targetPort":{echo_port}}}]"#);
    let (core, gen_log, _peer, exit_log, _tun) = start_generation(&token_str, &rules, "ok");
    let exit_log0 = std::fs::read_to_string(&exit_log)
        .map(|s| s.lines().count())
        .unwrap_or(0);
    let st = wait_listening(&core, listen, WAIT);
    println!("[pf-e2e] listening: listen={listen} target={}", st["target"]);
    assert_eq!(st["target"], format!("主机:{echo_port}"), "目标文案（NAPI 面口径）");
    assert_eq!(st["code"], "", "成功态空码");
    assert_eq!(st["conns"], 0, "尚无连接");

    let payload = b"hw-m4-pf-dial";
    let got = round_trip(listen, payload).expect("映射必须真可连（本机 → pf → STREAM[dial] → 出口回环 echo）");
    assert_eq!(got, payload, "字节逐字往返");
    println!(
        "[pf-e2e] round_trip ok: {}/{}B",
        got.len(),
        payload.len()
    );

    // 出口侧证据：`tag=dial` 受理行（= 拨号成功；E-q5 族的 M4 取值）
    let line = wait_line(&exit_log, "服务流已受理（tag=dial", WAIT).expect("出口须记 dial 受理行");
    println!("[pf-e2e] exit.accept_line={line}");
    assert!(line.contains("dev="), "受理行须带设备短指纹：{line}");
    // 负判据：本轮出口日志零 `intercept: tcp exempt`（QUIC 档的 dial 流不经过 WG 服务腿）
    let tail: String = std::fs::read_to_string(&exit_log)
        .unwrap_or_default()
        .lines()
        .skip(exit_log0)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        !tail.contains("intercept: tcp exempt"),
        "QUIC 档的 dial 流不得经 WG 服务腿（intercept 豁免臂）：{tail}"
    );
    // 世代日志侧：N-d 档位行 = quic（本用例走真 QUIC 承载）+ pf 无失败
    let gen = std::fs::read_to_string(&gen_log).unwrap_or_default();
    assert!(
        gen.lines().any(|l| l.contains("transport: 本世代 L3 承载 =") && l.contains("quic")),
        "N-d 须声明 quic 档"
    );
    let v: serde_json::Value = serde_json::from_str(&core.tun_status()).expect("JSON");
    assert_eq!(v["stats"]["pfAccepted"], 1, "准入 1 次：{v}");
    assert_eq!(v["stats"]["pfFails"], 0, "零拨号失败：{v}");
    // 往返收口后 conns 归零（`FlowGuard` 的 RAII 回退：两向泵都收工才减；客户端 socket
    // 关闭后泵退出有一拍延迟 ⇒ 有界等）
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let v: serde_json::Value = serde_json::from_str(&core.tun_status()).expect("JSON");
        if v["portForwards"][0]["conns"] == 0 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "往返收口后 conns 未归零（`FlowGuard` 泄漏？）：{v}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }

    let _ = core.tun_stop();
    echo_stop.store(true, std::sync::atomic::Ordering::SeqCst);
}

/// **判据（③：流槽泄漏，r19 H1 / S2 完成判据）**：同一世代上连续 `LEAK_DIALS`(80) 次
/// 「目标拒绝」（死端口）失败拨号 ⇒ 第 80 次的拒因仍是**「目标拒绝」而非「入口队列满」**
/// （= 失败路径真发 `Cmd::StreamClose`，岛内流槽没漏）；此后同世代上另一条映射
/// （活 echo）仍能完成往返（正证：槽位与拨号能力都还在）。
#[test]
#[ignore = "端到端（portfwd 经 STREAM[dial]）：需本地 QUIC 出口在跑（S5 的驱动脚本）"]
fn stream_dial_failures_do_not_leak_island_stream_slots() {
    let token_str = std::env::var("HOMEWAY_ISLAND_E2E_TOKEN").expect("须给 HOMEWAY_ISLAND_E2E_TOKEN");
    let (echo_port, echo_stop) = spawn_echo();
    let dead_port = free_port(); // 绑后即关 ⇒ 该端口此刻无监听（出口侧 ECONNREFUSED）
    let listen_dead = free_port();
    let listen_live = free_port();
    let rules = format!(
        r#"[{{"listen":{listen_dead},"targetIp":"","targetPort":{dead_port}}},
            {{"listen":{listen_live},"targetIp":"","targetPort":{echo_port}}}]"#
    );
    let (core, gen_log, _peer, _exit_log, _tun) = start_generation(&token_str, &rules, "leak");
    wait_listening(&core, listen_dead, WAIT);
    wait_listening(&core, listen_live, WAIT);

    for i in 1..=LEAK_DIALS {
        // 每次连接都必须失败（拨号失败 ⇒ 本机侧 RST；读得 Err 或 EOF，绝不该有数据）
        match round_trip(listen_dead, b"x") {
            Err(_) => {}
            Ok(got) => panic!("第 {i} 次拨号（死端口）不该成功：读到 {got:?}"),
        }
    }
    // 失败计数 = 真值（80 次都走完了拨号链）
    let v: serde_json::Value = serde_json::from_str(&core.tun_status()).expect("JSON");
    let fails = v["stats"]["pfFails"].as_u64().unwrap_or(0);
    println!("[pf-e2e] leak: dials={LEAK_DIALS} pfFails={fails}");
    assert_eq!(fails, u64::from(LEAK_DIALS), "每一次都必须落到「拨号失败」计数上");

    // **泄漏判据**：第 80 次的拒因（pf 失败行节流：首 5 + 每 20 ⇒ #80 可见）
    let needle = format!("拨号失败 #{LEAK_DIALS}:");
    let line = wait_line(&gen_log, &needle, WAIT).expect("第 80 次失败行须可见（节流口径首 5 + 每 20）");
    println!("[pf-e2e] leak.line={line}");
    assert!(
        line.contains("目标拒绝"),
        "第 {LEAK_DIALS} 次的拒因必须是「目标拒绝」（失败路径真关流）——若此处出现「入口队列满」\
         即流槽泄漏（岛内 62 槽被失败拨号占满）：{line}"
    );
    assert!(
        !line.contains("入口队列满"),
        "不得出现 Busy 归因：{line}"
    );

    // 正证：失败 80 次之后，同一世代上另一条映射仍能完成往返
    let payload = b"after-leak";
    let got = round_trip(listen_live, payload).expect("失败 80 次后 live 映射仍须可连（槽位未被占满）");
    assert_eq!(got, payload, "字节逐字往返");
    println!("[pf-e2e] leak.after_round_trip={}/{}B", got.len(), payload.len());
    let v: serde_json::Value = serde_json::from_str(&core.tun_status()).expect("JSON");
    assert_eq!(v["stats"]["pfFails"], u64::from(LEAK_DIALS), "往返不改失败计数");
    assert!(v["stats"]["pfAccepted"].as_u64().unwrap_or(0) >= 1, "有成功受理");

    let _ = core.tun_stop();
    echo_stop.store(true, std::sync::atomic::Ordering::SeqCst);
}
