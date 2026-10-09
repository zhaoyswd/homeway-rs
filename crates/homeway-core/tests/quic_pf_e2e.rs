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
use homeway_core::token::{self, EndpointKind, EndpointRef, TokenSpec};

/// 轮询上界（只判上界——flake 口径②）。
const WAIT: Duration = Duration::from_secs(30);
/// 泄漏用例的失败拨号次数（> 62 = 岛内服务流容量；见文件头③）。
const LEAK_DIALS: u32 = 80;
/// S4 falsify：误判率的分母（预登记「≤1/5」）。
const RECOVER_ROUNDS: usize = 5;
/// S4 falsify：下推耗时上界（预登记 8s；实测正常形态 0.7s/2.1s 两档）。
const RECOVER_LIMIT: Duration = Duration::from_secs(8);
/// 本机 LAN 直连地址的**缺省值**（开发机）：设计 §0.4-2 要求「直连网段」——非直连网段会被
/// 本机 fake-IP 代理接住，测到的就不是我方拨号腿。
///
/// **换机/多网段环境**用 env `HOMEWAY_PF_E2E_LAN_IP=<ipv4>` 覆盖（代码门 r21 F4：此前写死常量，
/// 换机时形态 3/4 会以「echo 可在指定地址绑定」这种**不指向环境**的报错失败）。
const LAN_IP_DEFAULT: &str = "192.168.3.12";

/// 本机 LAN 直连地址（见 [`LAN_IP_DEFAULT`]）。
fn lan_ip() -> Ipv4Addr {
    match std::env::var("HOMEWAY_PF_E2E_LAN_IP") {
        Ok(s) => s
            .parse()
            .unwrap_or_else(|_| panic!("HOMEWAY_PF_E2E_LAN_IP={s:?} 不是 IPv4 字面量")),
        Err(_) => LAN_IP_DEFAULT.parse().expect("缺省 LAN 地址可解析"),
    }
}
/// 本机实测**黑洞**目标（设计 §12.1-④ 指定；SYN 无回音 ⇒ 出口 10s 预算到点如实给 `0x26`）。
const BLACKHOLE: &str = "169.254.169.254:80";

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

// ===========================================================================
// S5：台架小件（楔子代理 / token 改指 / 指定地址 echo / 行扫描）
// ===========================================================================

/// 出口 QUIC 端点端口（token 里 `kind=Quic` 的第一条）。
fn exit_quic_port(token_str: &str) -> u16 {
    let tok = token::decode(token_str).expect("token 可解");
    tok.endpoints
        .iter()
        .find(|e| e.kind == EndpointKind::Quic)
        .and_then(|e| e.addr.rsplit(':').next()?.parse().ok())
        .expect("token 里必须有 Quic 端点（否则岛起不来）")
}

/// 把 token 的 **Quic 端点**改指向 `127.0.0.1:<port>`（密钥/rpk/其余端点原样）——
/// 楔子代理注入用（照 `quic_ladder_e2e.rs` 的 `pport` 形态）。
fn retarget_token_quic(token_str: &str, port: u16) -> String {
    let tok = token::decode(token_str).expect("token 可解");
    let addr = format!("127.0.0.1:{port}");
    let raw: Vec<String> = tok
        .endpoints
        .iter()
        .map(|e| {
            if e.kind == EndpointKind::Quic {
                addr.clone()
            } else {
                e.addr.clone()
            }
        })
        .collect();
    let eps: Vec<EndpointRef> = tok
        .endpoints
        .iter()
        .zip(raw.iter())
        .map(|(e, a)| EndpointRef::new(a.as_str(), e.kind))
        .collect();
    token::encode(&TokenSpec {
        peer_id: &tok.peer_id,
        secret: &tok.secret,
        endpoints: &eps,
        rpk: tok.rpk.as_ref(),
    })
    .expect("改指后的 token 可编码")
}

/// UDP 楔子代理句柄（`Drop` 收尸 + 删控制文件——panic 路径也不留孤儿进程/残留窗口）。
struct Wedge {
    child: std::process::Child,
    ctrl: PathBuf,
    port: u16,
}

impl Wedge {
    /// 黑洞窗口开关（控制文件存在 ⇒ 静默丢弃一切）。
    fn blackhole(&self, on: bool) {
        if on {
            std::fs::write(&self.ctrl, b"1").expect("控制文件可写");
        } else {
            let _ = std::fs::remove_file(&self.ctrl);
        }
    }
}

impl Drop for Wedge {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_file(&self.ctrl);
    }
}

/// 起一枚 UDP 楔子代理（`tools/quic-wedge-proxy.py`）。就绪判据 = 该端口已被占住。
fn start_wedge(upstream_port: u16) -> Wedge {
    let script = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tools/quic-wedge-proxy.py");
    let probe = std::net::UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).expect("探端口");
    let port = probe.local_addr().expect("本地地址").port();
    drop(probe);
    let ctrl = std::env::temp_dir().join(format!("hw-m4pf-wedge-{port}.ctrl"));
    let _ = std::fs::remove_file(&ctrl);
    let mut child = std::process::Command::new("python3")
        .arg(&script)
        .arg(format!("127.0.0.1:{port}"))
        .arg(format!("127.0.0.1:{upstream_port}"))
        .arg(&ctrl)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .expect("楔子代理可起");
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if std::net::UdpSocket::bind((Ipv4Addr::LOCALHOST, port)).is_err() {
            return Wedge { child, ctrl, port };
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = child.kill();
    let _ = child.wait();
    panic!("楔子代理未在 5s 内绑上端口 {port}");
}

/// 在**指定地址**起一枚回环/任意地址 echo（形态 3/4 的 `{ip}:{port}` 目标面）。
///
/// 绑定失败 = 环境不符（该地址不在本机 / 已被占）⇒ 报错点明「可用 `HOMEWAY_PF_E2E_LAN_IP` 覆盖」
/// （代码门 r21 F4：不把环境事实伪装成「代码写错」）。
fn spawn_echo_at(addr: std::net::SocketAddrV4) -> (u16, Arc<std::sync::atomic::AtomicBool>) {
    let ln = TcpListener::bind(addr).unwrap_or_else(|e| {
        panic!(
            "echo 无法绑到 {addr}（{e}）——该地址须是本机直连网段地址；用 env HOMEWAY_PF_E2E_LAN_IP=<ipv4> 指定"
        )
    });
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

/// 某条映射在状态 JSON 里的对象（按 listen 找）。
fn rule_state(core: &Arc<ClientCore>, listen: u16) -> serde_json::Value {
    let v: serde_json::Value = serde_json::from_str(&core.tun_status()).expect("tun_status 是 JSON");
    v["portForwards"]
        .as_array()
        .and_then(|arr| arr.iter().find(|r| r["listen"] == listen))
        .cloned()
        .unwrap_or_else(|| panic!("状态里没有 listen={listen} 的映射：{v}"))
}

/// 世代日志里 `needle` 的行数（`quic_pf_e2e` 的日志面读数）。
fn gen_lines(gen_log: &PathBuf) -> Vec<String> {
    std::fs::read_to_string(gen_log)
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
}

// ===========================================================================
// S4：NAPI 恢复下推的 falsify 三指标（预登记，设计 §5.3 / §15-1）
// ===========================================================================

/// **判据（S4 falsify 三指标；本地面）**：
/// ① **误判率 ≤ 1/5**：黑障期间下推返 `-1` 后，「**无外部条件变化**下 10s 内岛内自愈」的次数
///    ≤ 1 次（本机等价判据 = 返 `-1` 后**立即**再推一次仍为 `-1`：黑障未除而岛已自愈 = 误判）；
/// ② **零 `RECOVER` 族行**：全程世代日志里 `RECOVER ` 行数 = 0（岛档不跑 WG 阶梯）；
/// ③ **耗时 ≤ 8s**：每次下推的墙钟 ≤ 8s（上界口径见 `recover_downpush_on_island` 的注释）。
///
/// 另采两枚**边界读数**（不设判据，只如实登记）：黑洞解除后岛内阶梯自愈耗时；
/// 以及岛档正常形态（路径通）下推的耗时与返码（应为 `0` 且远小于 1s）。
#[test]
#[ignore = "端到端（NAPI 恢复下推 × 楔子黑障）：需本地 QUIC 出口在跑（tools/quic-pf-e2e.sh 驱动）"]
fn recover_downpush_on_island_satisfies_falsify_metrics() {
    let token_str = std::env::var("HOMEWAY_ISLAND_E2E_TOKEN").expect("须给 HOMEWAY_ISLAND_E2E_TOKEN");
    let exit_port = exit_quic_port(&token_str);
    let proxy = start_wedge(exit_port);
    let pport = proxy.port;
    let tok = retarget_token_quic(&token_str, pport);
    let (core, gen_log, _peer, _exit_l, _tun) = start_generation(&tok, "[]", "s4rec");
    // 前提：岛必须真在世（否则「恢复下推」落 WG 原路 ⇒ 本用例假绿）
    let deadline = Instant::now() + WAIT;
    loop {
        let v: serde_json::Value = serde_json::from_str(&core.tun_status()).expect("JSON");
        if v["quic"]["connections"] == 1 {
            break;
        }
        assert!(Instant::now() < deadline, "世代未在 {WAIT:?} 内上岛：{v}");
        std::thread::sleep(Duration::from_millis(50));
    }
    println!("[pf-e2e] s4：岛上就位（exit_port={exit_port} 经楔子 {pport}）");

    let mut misjudged = 0usize;
    let mut worst = Duration::ZERO;
    let mut heal_times: Vec<Duration> = Vec::new();
    for round in 1..=RECOVER_ROUNDS {
        // 注入：黑洞窗口（控制文件存在 ⇒ 楔子静默丢弃）
        proxy.blackhole(true);
        std::thread::sleep(Duration::from_millis(30));
        let t0 = Instant::now();
        let rc = core.tun_recover(3);
        let elapsed = t0.elapsed();
        worst = worst.max(elapsed);
        println!("[pf-e2e] s4 轮次{round}：黑障下推 rc={rc} elapsed={elapsed:?}");
        assert_eq!(rc, -1, "黑障（两次探都失败）⇒ -1");
        assert!(
            elapsed <= RECOVER_LIMIT,
            "轮次{round}：耗时 {elapsed:?} 超预登记上界 {RECOVER_LIMIT:?}"
        );
        // ① 误判面：黑障未除 ⇒ 立即再推一次仍须为 -1（若为 0 = 岛已自愈而返了 -1 = 误判）
        let t1 = Instant::now();
        let rc2 = core.tun_recover(3);
        println!(
            "[pf-e2e] s4 轮次{round}：黑障复查下推 rc={rc2} elapsed={:?}",
            t1.elapsed()
        );
        if rc2 == 0 {
            misjudged += 1;
        }
        // 解除：等岛内阶梯自愈（边界读数：自愈耗时）
        proxy.blackhole(false);
        let healt0 = Instant::now();
        let mut healed = None;
        while healt0.elapsed() < Duration::from_secs(10) {
            if core.tun_recover(3) == 0 {
                healed = Some(healt0.elapsed());
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let dt = healed.expect("黑洞解除后 10s 内必须自愈（复探成功 ⇒ 0）");
        heal_times.push(dt);
        println!("[pf-e2e] s4 轮次{round}：黑洞解除后自愈 {dt:?}");
    }
    // ② 零 RECOVER 族行（整轮世代日志）
    let lines = gen_lines(&gen_log);
    let recover_lines: Vec<&String> = lines.iter().filter(|l| l.starts_with("RECOVER ")).collect();
    println!(
        "[pf-e2e] s4：RECOVER 族行 = {}（耗时最坏 {worst:?}；自愈耗时 {:?}）",
        recover_lines.len(),
        heal_times
    );
    assert!(
        recover_lines.is_empty(),
        "岛档下推不得产 RECOVER 族行（= 不跑 WG 阶梯）：{recover_lines:?}"
    );
    // 岛档归因行在场（每轮两推 ⇒ ≥ 2×RECOVER_ROUNDS 条）
    let push_lines: Vec<&String> = lines
        .iter()
        .filter(|l| l.contains("quic: 恢复下推（扩展下推(档位 3)）——岛快探+复探：失败（"))
        .collect();
    println!("[pf-e2e] s4：岛档归因行 = {} 条", push_lines.len());
    assert!(
        push_lines.len() >= 2 * RECOVER_ROUNDS,
        "每轮两推各一行：实得 {} 行",
        push_lines.len()
    );
    // ① 判据
    println!("[pf-e2e] s4：误判 = {misjudged}/{RECOVER_ROUNDS}");
    assert!(
        misjudged * 5 <= RECOVER_ROUNDS,
        "误判率须 ≤1/5：实得 {misjudged}/{RECOVER_ROUNDS}"
    );
    // ③ 判据（墙钟上界）
    assert!(worst <= RECOVER_LIMIT, "最坏耗时 {worst:?} 超 {RECOVER_LIMIT:?}");

    // 岛档正常形态（路径通）：0 且远小于 1s（同源对照——证明返 0 不是「什么都不做」）
    let t2 = Instant::now();
    let rc = core.tun_recover(3);
    println!("[pf-e2e] s4：康复后下推 rc={rc} elapsed={:?}", t2.elapsed());
    assert_eq!(rc, 0, "路径通 ⇒ 0（快探通过）");
    let ok_line = "quic: 恢复下推（扩展下推(档位 3)）——岛快探：通过";
    assert!(
        gen_lines(&gen_log).iter().any(|l| l.contains(ok_line)),
        "首探通过 ⇒ 行文不带「+复探」（逐字）：{ok_line}"
    );

    let _ = core.tun_stop();
    drop(proxy); // 收尸 + 清控制文件（Drop 里做）
}

// ===========================================================================
// S5：四形态（`pf_target_text`）× 失败归因 —— 逐格实测（本机）
// ===========================================================================

/// **判据（设计 §4 的四形态 × 失败归因表；本机可测格）**：同一世代装 6 条映射
/// （形态 2 / 3 / 4 / 显式回环 / 形态 2 未监听 / 形态 4 未监听）逐格给**行文 + rc**：
/// - 形态 2「主机:{port}」与形态 4「{ip}:{port}」成功 ⇒ 字节逐字往返；
/// - 形态 3「{ip}:{listen}」成功 ⇒ 同上（`target_port=0` ⇒ 线上 = `{ip}:{listen}`）；
/// - **显式回环**（`targetIp=127.0.0.1`）⇒ 不被 A8 拒（**「回环拒绝」在 M4 不存在**的正证）；
/// - 未监听 ⇒ 出口 `目标拨号失败（…：Connection refused…）` + `0x25`；本机行
///   `拨号失败 #1: QUIC 服务流：目标拒绝（目标拒绝）`；本机应用读到 **RST**。
///
/// 本机**不可测**的格（如实登记，不冒充）：形态 1「主机（同端口）」——同机自环
/// （客户端 pf 监听器与出口目标同机同址同端口）⇒ 归真机（`targetIp=""`+`targetPort=0`
/// 是 App 表单形态，真机 R1-S① 覆盖的是形态 2）。
///
/// **运行前提（出口行节流 = 既有语义 `log_due(n) = n≤3 ∨ n%100=0`，M4 零改动；两个独立窗：
/// 受理行共用 `streams_open`、拒行共用 `stream_refused`，各自跨 tag 共享——代码门 r21 F11）**：
/// 本用例的两条归因行须落在**新起出口**的头三次额度内 ⇒ 由 `tools/quic-pf-e2e.sh` 保证
/// （顺序 = ride → 0x26 → **forms** → leak → s4，且驱动先 `wipe`+`start`）；手工单跑前请先
/// `tools/local-rust-exit.sh wipe 1 && tools/local-rust-exit.sh start 1`。
#[test]
#[ignore = "端到端（portfwd 四形态 × 归因）：需本地 QUIC 出口在跑（tools/quic-pf-e2e.sh 驱动）"]
fn port_forward_target_forms_and_failure_attribution() {
    let token_str = std::env::var("HOMEWAY_ISLAND_E2E_TOKEN").expect("须给 HOMEWAY_ISLAND_E2E_TOKEN");
    let _exit_log = PathBuf::from(
        std::env::var("HOMEWAY_ISLAND_E2E_EXIT_LOG").expect("须给 HOMEWAY_ISLAND_E2E_EXIT_LOG"),
    );
    let (echo2, stop2) = spawn_echo(); // 形态 2 的目标（回环任意端口）
    let lan = lan_ip(); // LAN 直连地址（缺省 192.168.3.12；env HOMEWAY_PF_E2E_LAN_IP 覆盖）
    let l2 = free_port();
    let l3 = free_port();
    let l4 = free_port();
    let l5 = free_port();
    let l6 = free_port();
    let l7 = free_port();
    let dead = free_port();
    // 形态 3 的目标 = `{ip}:{listen}`：echo 必须恰好监听 `{lan}` 的 `l3`（LAN 地址见 `lan_ip()`）
    let (echo3, stop3) = spawn_echo_at(
        format!("{lan}:{l3}").parse().expect("LAN:t 可解析"),
    );
    assert_eq!(echo3, l3, "形态 3 的 echo 必须落在 {lan}:{l3}");
    // 形态 4 的目标 = `{ip}:{port}`（显式端口）
    let (echo4, stop4) = spawn_echo_at(format!("{lan}:0").parse().expect("LAN:0 可解析"));
    // 显式回环格（`targetIp=127.0.0.1` + 独立 echo）
    let (echo_r, stop_r) = spawn_echo();
    let rules = format!(
        r#"[{{"listen":{l2},"targetIp":"","targetPort":{echo2}}},
            {{"listen":{l3},"targetIp":"{lan}","targetPort":0}},
            {{"listen":{l4},"targetIp":"{lan}","targetPort":{echo4}}},
            {{"listen":{l5},"targetIp":"127.0.0.1","targetPort":{echo_r}}},
            {{"listen":{l6},"targetIp":"","targetPort":{dead}}},
            {{"listen":{l7},"targetIp":"{lan}","targetPort":{dead}}}]"#
    );
    let (core, gen_log, _peer, exit_log, _tun) = start_generation(&token_str, &rules, "forms");
    let (l2d, l3d, l4d, l5d, l6d, l7d) = (l2, l3, l4, l5, l6, l7);
    for l in [l2d, l3d, l4d, l5d, l6d, l7d] {
        wait_listening(&core, l, WAIT);
    }
    // 文案逐格（NAPI 面口径 = `pf_target_text` 四形态）
    println!(
        "[pf-e2e] forms: 形态2={} 形态3={} 形态4={} 回环={} 未监听A={} 未监听B={}",
        rule_state(&core, l2d)["target"],
        rule_state(&core, l3d)["target"],
        rule_state(&core, l4d)["target"],
        rule_state(&core, l5d)["target"],
        rule_state(&core, l6d)["target"],
        rule_state(&core, l7d)["target"]
    );
    assert_eq!(rule_state(&core, l2d)["target"], format!("主机:{echo2}"));
    assert_eq!(rule_state(&core, l3d)["target"], format!("{lan}:{l3}"));
    assert_eq!(rule_state(&core, l4d)["target"], format!("{lan}:{echo4}"));
    assert_eq!(rule_state(&core, l5d)["target"], format!("127.0.0.1:{echo_r}"));
    // ---- spec R1① 的**行为断言**：映射**仅回环**（同端口在 LAN 地址上不得监听）----
    let lan_hit = std::net::TcpStream::connect_timeout(
        &format!("{lan}:{l2d}").parse().expect("LAN:listen 可解析"),
        Duration::from_secs(3),
    );
    assert!(
        lan_hit.is_err(),
        "spec R1①：映射只许在 127.0.0.1 上监听（不暴露局域网）——{lan}:{l2d} 却被接住"
    );
    println!("[pf-e2e] forms R1①：{lan}:{l2d} 连接被拒（仅回环）");
    // ---- spec R1② 的**行为断言**：pf 往返**不骑 TUN 数据面**（岛 TUN 包计数不动）----
    let before = {
        let v: serde_json::Value = serde_json::from_str(&core.tun_status()).expect("JSON");
        (
            v["quic"]["packetsIn"].as_u64().unwrap_or(0),
            v["quic"]["packetsOut"].as_u64().unwrap_or(0),
        )
    };
    // 成功格：逐格往返（字节逐字）
    for (tag, listen) in [
        ("形态2 主机:{port}", l2d),
        ("形态3 {ip}:{listen}", l3d),
        ("形态4 {ip}:{port}", l4d),
        ("显式回环 127.0.0.1", l5d),
    ] {
        let payload = format!("m4-{listen}").into_bytes();
        let got = round_trip(listen, &payload).unwrap_or_else(|e| panic!("{tag} 必通：{e}"));
        assert_eq!(got, payload, "{tag} 字节逐字往返");
        println!("[pf-e2e] forms {tag}: round_trip {}/{}B", got.len(), payload.len());
    }
    let after = {
        let v: serde_json::Value = serde_json::from_str(&core.tun_status()).expect("JSON");
        (
            v["quic"]["packetsIn"].as_u64().unwrap_or(0),
            v["quic"]["packetsOut"].as_u64().unwrap_or(0),
        )
    };
    println!("[pf-e2e] forms R1②：岛 TUN 包计数 before={before:?} after={after:?}（四次往返后不变）");
    assert_eq!(
        before, after,
        "spec R1②：pf 往返不得骑 TUN 数据面（岛 TUN 包计数须不变）"
    );
    // 失败格 A（形态 2 未监听）与 B（形态 4 未监听）：本机应用读 RST + 两侧行文
    for (tag, listen, want_target, want_exit) in [
        (
            "形态2 未监听",
            l6d,
            format!("主机:{dead}"),
            format!("目标拨号失败（127.0.0.1:{dead}：Connection refused (os error 61)）"),
        ),
        (
            "形态4 未监听",
            l7d,
            format!("{lan}:{dead}"),
            format!("目标拨号失败（{lan}:{dead}：Connection refused (os error 61)）"),
        ),
    ] {
        let e = round_trip(listen, b"x").expect_err("死端口必失败");
        println!("[pf-e2e] forms {tag}: 本机读 = {:?}（{e}）｜target={want_target}", e.kind());
        assert_eq!(e.kind(), std::io::ErrorKind::ConnectionReset, "{tag}：RST 形态");
        // 出口侧归因行（E-q5 族的 M4 取值；`wait_line` 有界等）
        let line = wait_line(&exit_log, &want_exit, WAIT).unwrap_or_else(|| {
            panic!("{tag}：出口须有归因行 {want_exit:?}")
        });
        println!("[pf-e2e] forms {tag}: 出口行 = {line}");
        assert!(
            line.contains("0x25"),
            "{tag}：未监听 ⇒ 0x25（拒码）：{line}"
        );
        assert!(
            !line.contains("0x26"),
            "{tag}：本格不得出 0x26：{line}"
        );
    }
    // 本机侧行文（链 A；`pf_target_text` 逐格）
    let lines = gen_lines(&gen_log);
    let dead_lines: Vec<&String> = lines.iter().filter(|l| l.contains("拨号失败 #")).collect();
    for l in &dead_lines {
        println!("[pf-e2e] forms 本机行 = {l}");
    }
    assert!(
        dead_lines
            .iter()
            .any(|l| l.contains(&format!("-> 主机:{dead} 拨号失败")) && l.contains("目标拒绝")),
        "形态 2 的链 A 行文（含目标拒绝）：{dead_lines:?}"
    );
    assert!(
        dead_lines
            .iter()
            .any(|l| l.contains(&format!("-> {lan}:{dead} 拨号失败")) && l.contains("目标拒绝")),
        "形态 4 的链 A 行文：{dead_lines:?}"
    );
    // 计数面（R3：`pfFails` 逐条 +1；`conns` 随连接 ±1）
    let v: serde_json::Value = serde_json::from_str(&core.tun_status()).expect("JSON");
    assert_eq!(v["stats"]["pfFails"], 2, "两条失败格各 +1：{v}");
    assert!(v["stats"]["pfAccepted"].as_u64().unwrap_or(0) >= 4, "四条成功格：{v}");

    // R3 判据③（`conns` 随连接 ±1：持有一条连接时 = 1，关闭后回 0）
    {
        use std::io::Write;
        let mut held = TcpStream::connect((Ipv4Addr::LOCALHOST, l2d)).expect("可连");
        held.write_all(b"hold").expect("可写");
        let mut buf = [0u8; 4];
        use std::io::Read;
        held.read_exact(&mut buf).expect("echo 回");
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let st = rule_state(&core, l2d);
            if st["conns"] == 1 {
                println!("[pf-e2e] forms: 持有连接时 conns=1（state={} code={} err={}）", st["state"], st["code"], st["err"]);
                break;
            }
            assert!(Instant::now() < deadline, "conns 未在 5s 内变 1：{st}");
            std::thread::sleep(Duration::from_millis(50));
        }
        drop(held);
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let st = rule_state(&core, l2d);
            if st["conns"] == 0 {
                break;
            }
            assert!(Instant::now() < deadline, "conns 未回 0：{st}");
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    let _ = core.tun_stop();
    stop2.store(true, std::sync::atomic::Ordering::SeqCst);
    stop3.store(true, std::sync::atomic::Ordering::SeqCst);
    stop4.store(true, std::sync::atomic::Ordering::SeqCst);
    stop_r.store(true, std::sync::atomic::Ordering::SeqCst);
}

// ===========================================================================
// S5：`0x26` 的**真面**（出口 10s 拨号预算到点 ⇒ 精确归因）
// ===========================================================================

/// **判据（设计 §4 的「拨号失败」格；S1–S3 交下的 `0x26` 真 socket 判据）**：
/// 目标 = 本机**黑洞** `169.254.169.254:80`（设计 §12.1-④ 指定；SYN 无回音）⇒
/// 出口 `TcpStream::connect` 吃满 10s 预算 ⇒ `reset(0x26)` + 行
/// `目标拨号超时（169.254.169.254:80；预算 10s）` ⇒ 客户端 `StreamErr::Timeout` ⇒
/// 本机行 `拨号失败 #1: QUIC 服务流：服务流超时（服务流超时）` ⇒ 本机应用（约 10s 后）读 RST。
///
/// **环境依赖（如实登记）**：本格依赖「该地址在本机是黑洞」这一**机器事实**（同设计 §12.1
/// 的登记口径）：若换机器后它变为快速 `EHOSTUNREACH`，本用例会红——那是环境事实变了，
/// 不是产品回归（读数里两行 errno 原文可判）。
///
/// **运行前提**同 forms 用例（出口拒绝行节流 ⇒ 须新起出口；本用例排在驱动脚本第 2 位）。
#[test]
#[ignore = "端到端（0x26 真面：黑洞目标的拨号超时）：需本地 QUIC 出口在跑（tools/quic-pf-e2e.sh 驱动）"]
fn port_forward_dial_timeout_reports_0x26_with_exit_line() {
    let token_str = std::env::var("HOMEWAY_ISLAND_E2E_TOKEN").expect("须给 HOMEWAY_ISLAND_E2E_TOKEN");
    let exit_log = PathBuf::from(
        std::env::var("HOMEWAY_ISLAND_E2E_EXIT_LOG").expect("须给 HOMEWAY_ISLAND_E2E_EXIT_LOG"),
    );
    let (bh_ip, bh_port) = BLACKHOLE.split_once(':').expect("黑洞形态 ip:port");
    let listen = free_port();
    let rules = format!(
        r#"[{{"listen":{listen},"targetIp":"{bh_ip}","targetPort":{bh_port}}}]"#
    );
    let (core, gen_log, _peer, _exit_log, _tun) = start_generation(&token_str, &rules, "t26");
    wait_listening(&core, listen, WAIT);
    let t0 = Instant::now();
    let e = round_trip(listen, b"x").expect_err("黑洞目标必失败");
    let elapsed = t0.elapsed();
    println!(
        "[pf-e2e] 0x26: 本机读 = {:?}（{e}）after {elapsed:?}",
        e.kind()
    );
    assert_eq!(e.kind(), std::io::ErrorKind::ConnectionReset, "RST 形态");
    let want = format!("目标拨号超时（{BLACKHOLE}；预算 10s）");
    let line = wait_line(&exit_log, &want, WAIT).expect("出口须有超期归因行");
    println!("[pf-e2e] 0x26: 出口行 = {line}");
    assert!(line.contains("0x26"), "超期 ⇒ 0x26：{line}");
    assert!(
        !line.contains("目标拨号失败"),
        "超期不得伪装成「拨号失败」：{line}"
    );
    let lines = gen_lines(&gen_log);
    let local = lines
        .iter()
        .find(|l| l.contains("拨号失败 #1:"))
        .expect("本机链 A 行须在场");
    println!("[pf-e2e] 0x26: 本机行 = {local}");
    assert!(local.contains("QUIC 服务流：服务流超时"), "本机归因：{local}");
    assert!(
        elapsed >= Duration::from_secs(9) && elapsed <= Duration::from_secs(13),
        "本机可见时点 ≈ 出口 10s 预算（实测 {elapsed:?}）"
    );
    let _ = core.tun_stop();
}
