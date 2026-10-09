//! M3 S4：**快探阶梯的故障注入 e2e**（设计 §3.1/§3.2/§3.3）。
//!
//! 四条用例（每条都对本地私有出口实例注入真故障，**绝不碰现役**）：
//!
//! 1. `kill9_then_immediate_restart_recovers_within_t_recv`（相位 A：出口停机 < `T_detect`）
//! 2. `kill9_then_restart_after_five_seconds_recovers_within_t_recv`（相位 B：停机 5s）
//! 3. `transient_blackhole_window_only_jitters_and_takes_no_action`（负向①：1.5s 黑洞 ⇒ 只记抖动）
//! 4. `wedge_without_close_must_escalate_to_an_action`（负向②：回显永不返回、连接不关 ⇒ 必有动作）
//!
//! **判据（§3.3 写死）**：`T_recv ≤ 3.5s`，起点 = 出口的 **E1 `serve 就绪` 行时刻**
//! （本用例以 5ms 粒度轮询日志文件读取该行首次出现的时刻 ⇒ 观测延迟 ≤5ms，且方向恒为
//! **偏晚**（保守）；客户端首个回显成功 = 岛 `ladder_probe_ok` 计数增长（原子直读，见
//! `Island::snapshot`），同一粒度轮询）。`T_detect` 单独登记：以岛侧判据行（C18 族）的
//! **落纸时刻**（`Logf` 回调就地取时）算「出口最后一次可达 → 复探失败定音」。
//!
//! 环境契约（`tools/quic-ladder-e2e.sh` 设定）：
//! - `HOMEWAY_LADDER_TOKEN`：出口 token（`serve token` 抽 `hmw1…`）
//! - `HOMEWAY_LADDER_EXIT_LOG`：出口 stdout 日志路径
//! - `HOMEWAY_LADDER_PIDFILE`：出口 pidfile（kill -9 用）
//! - `HOMEWAY_LADDER_RESTART`：重启命令（形如 `/bin/zsh <repo>/tools/local-rust-exit.sh start 2`，
//!   以空格切分 ⇒ 路径里不得有空格）
//!
//! **不做**：真机（= S8）、`quic-ab` 门槛（= S8）、判据行登记（= S7）。

use std::net::{Ipv4Addr, SocketAddrV4};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use homeway_core::identity::Identity;
use homeway_core::token::{self, EndpointKind};
use homeway_quic::{
    Candidate, Cmd, Island, IslandConfig, IslandCredential, RpkPublicKey, TokenSecret, Via,
};

/// 判据（§3.3 写死）：出口重新监听（E1）→ 客户端首个回显成功。
const T_RECV_LIMIT: Duration = Duration::from_secs(3_500);
/// 等待上界（只判上界——flake 口径②）。
const WAIT: Duration = Duration::from_secs(30);
/// 轮询粒度（日志行/计数观测；5ms ⇒ 观测误差 ≤5ms 且方向偏晚 = 保守）。
const POLL: Duration = Duration::from_millis(5);

// ---------------------------------------------------------------------------
// 台架小件
// ---------------------------------------------------------------------------

/// 带本地时戳的岛侧日志收集器（`T_detect` 的读数源：C18 行落纸时刻）。
#[derive(Clone)]
struct Trace {
    t0: Instant,
    lines: Arc<Mutex<Vec<(Duration, String)>>>,
}

impl Trace {
    fn new() -> Self {
        Self {
            t0: Instant::now(),
            lines: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn logf(&self) -> homeway_quic::Logf {
        let t0 = self.t0;
        let sink = Arc::clone(&self.lines);
        Arc::new(move |l: &str| {
            let at = t0.elapsed();
            println!("[{:>8.3}s] [island] {l}", at.as_secs_f64());
            sink.lock().unwrap().push((at, l.to_owned()));
        })
    }

    /// 首条含 `needle` 的行（带时戳）。
    fn first(&self, needle: &str) -> Option<(Duration, String)> {
        self.lines
            .lock()
            .unwrap()
            .iter()
            .find(|(_, l)| l.contains(needle))
            .cloned()
    }

    /// 含 `needle` 的行数。
    fn count(&self, needle: &str) -> usize {
        self.lines
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, l)| l.contains(needle))
            .count()
    }

    fn dump(&self, tag: &str) {
        println!("[{tag}] island 判据行（{n} 条）", n = self.lines.lock().unwrap().len());
        for (at, l) in self.lines.lock().unwrap().iter() {
            println!("[{tag}]   {:>8.3}s {l}", at.as_secs_f64());
        }
    }
}

fn env_var(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("须给 {name}（tools/quic-ladder-e2e.sh 设定）"))
}

fn log_lines(path: &PathBuf) -> usize {
    std::fs::read_to_string(path)
        .map(|s| s.lines().count())
        .unwrap_or(0)
}

/// 等待日志文件**新增行**里出现 `needle`；返回首次观测时刻（5ms 粒度）。
fn wait_log_new(path: &PathBuf, skip: usize, needle: &str, wait: Duration) -> Option<Instant> {
    let deadline = Instant::now() + wait;
    loop {
        let text = std::fs::read_to_string(path).unwrap_or_default();
        if text.lines().skip(skip).any(|l| l.contains(needle)) {
            return Some(Instant::now());
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(POLL);
    }
}

/// token 里的 QUIC 端点（回环同端口——同机隔离缝，照 `quic_island_e2e` 先例）。
fn exit_quic_port(token_str: &str) -> u16 {
    let tok = token::decode(token_str).expect("token 可解");
    tok.endpoints
        .iter()
        .find(|e| e.kind == EndpointKind::Quic)
        .map(|e| {
            let raw: SocketAddrV4 = e.addr.parse().expect("QUIC 端点可解析");
            raw.port()
        })
        .expect("token 必带 QUIC 端点")
}

/// 起一枚连到（候选= `cands`）的岛 + 附加 TUN 数据面 + 后台保活线程（保持「在用档」）。
struct Lab {
    island: Island,
    /// 赛跑胜出的候选地址（负向用例的「楔子真在路径上」负判据）。
    winner: SocketAddrV4,
    keep: Arc<AtomicBool>,
    keep_thread: Option<std::thread::JoinHandle<()>>,
}

impl Drop for Lab {
    fn drop(&mut self) {
        self.keep.store(false, Ordering::SeqCst);
        if let Some(h) = self.keep_thread.take() {
            let _ = h.join();
        }
        let _ = self.island.stop_within(Instant::now() + Duration::from_secs(3));
    }
}

impl Lab {
    /// 起岛（候选由用例给）+ 连上 + 附加 TUN（socketpair）+ 保活线程。
    fn start(cands: Vec<Candidate>, trace: Trace) -> Lab {
        let token_str = env_var("HOMEWAY_LADDER_TOKEN");
        let tok = token::decode(&token_str).expect("token 可解");
        let rpk = tok.rpk.expect("token 必带出口 RPK");
        let id = Identity::ephemeral().expect("临时身份");
        let cred = IslandCredential::new(
            TokenSecret::from_bytes(*tok.secret.as_bytes()),
            id.public_key(),
            *id.dev_tag().as_bytes(),
            RpkPublicKey::from_bytes(*rpk.as_bytes()),
        );
        let mut cfg = IslandConfig::new(cred);
        cfg.bind = Some(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0));
        let island = Island::start(trace.logf(), Arc::new(|_r: &str| {}), cfg).expect("岛可起");
        let (tx, rx) = mpsc::channel();
        island
            .tx()
            .send(Cmd::Connect {
                cands,
                budget: Duration::from_secs(5),
                reply: tx,
            })
            .expect("投递 Connect");
        let outcome = rx
            .recv_timeout(Duration::from_secs(10))
            .expect("Connect 回执")
            .expect("赛跑须胜出（本地出口）");
        let winner = outcome.winner;

        // 数据面：TUN socketpair（岛持一端；另一端当「App」写包以维持「在用档」）
        let (tun, peer) = std::os::unix::net::UnixDatagram::pair().expect("socketpair(DGRAM)");
        {
            use std::os::fd::AsRawFd as _;
            let (tx, rx) = mpsc::channel();
            island
                .tx()
                .send(Cmd::TunAttach {
                    fd: tun.as_raw_fd(),
                    mtu: 1280,
                    reply: tx,
                })
                .expect("投递 TunAttach");
            rx.recv_timeout(Duration::from_secs(5))
                .expect("attach 回执")
                .expect("attach 受理");
        }
        // 保活：每 200ms 写一包（= App 出站 ⇒ 岛侧「在用档」判据 = 出站新鲜，§3.2-1）
        let keep = Arc::new(AtomicBool::new(true));
        let k = Arc::clone(&keep);
        let keep_thread = std::thread::spawn(move || {
            let pkt = vec![0u8; 100];
            while k.load(Ordering::SeqCst) {
                let _ = peer.send(&pkt);
                std::thread::sleep(Duration::from_millis(200));
            }
            drop(peer);
        });
        std::mem::forget(tun); // fd 所有权交岛（岛从不 close；用例只读不写）
        Lab {
            island,
            winner,
            keep,
            keep_thread: Some(keep_thread),
        }
    }

    /// 等 `ladder_probe_ok` 增长到 > `baseline`；返回首次观测时刻。
    fn wait_probe_ok_above(&self, baseline: u64, wait: Duration) -> Option<(Instant, u64)> {
        let deadline = Instant::now() + wait;
        loop {
            let n = self.island.snapshot().ladder_probe_ok;
            if n > baseline {
                return Some((Instant::now(), n));
            }
            if Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(POLL);
        }
    }

    fn probe_ok(&self) -> u64 {
        self.island.snapshot().ladder_probe_ok
    }
}

/// 起一枚 UDP 楔子代理（黑洞窗口注入），返回（子进程、控制文件路径、代理端口）。
fn start_wedge(exit_port: u16) -> (std::process::Child, PathBuf, u16) {
    let repo = PathBuf::from(env_var("HOMEWAY_LADDER_REPO"));
    let script = repo.join("tools/quic-wedge-proxy.py");
    // 取一个空闲端口（绑 `:0` 读回后释放——harness 面可容忍极小 TOCTOU）
    let probe = std::net::UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).expect("探端口");
    let port = probe.local_addr().unwrap().port();
    drop(probe);
    let ctrl = std::env::temp_dir().join(format!("hw-wedge-{}-{port}.ctrl", std::process::id()));
    let _ = std::fs::remove_file(&ctrl);
    let mut child = std::process::Command::new("python3")
        .arg(&script)
        .arg(format!("127.0.0.1:{port}"))
        .arg(format!("127.0.0.1:{exit_port}"))
        .arg(&ctrl)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .expect("楔子代理可起");
    // 等就绪：端口被占住（本端再绑同端口必失败）= 代理已 bind
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if std::net::UdpSocket::bind((Ipv4Addr::LOCALHOST, port)).is_err() {
            return (child, ctrl, port);
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    // 起失败：先收尸再报错（不留孤儿进程；clippy::zombie_processes）
    let _ = child.kill();
    let _ = child.wait();
    panic!("楔子代理未在 5s 内绑上端口 {port}");
}

// ---------------------------------------------------------------------------
// 相位 A/B：kill -9 注入（§3.3 的两种相位）
// ---------------------------------------------------------------------------

/// kill -9 本地出口；`restart_delay` 后经 harness 脚本重启，并等 E1 `serve 就绪` 行。
///
/// 返回（重启指令时刻、E1 行首次观测时刻）。
fn kill9_and_restart(exit_log: &PathBuf, restart_delay: Duration) -> (Instant, Instant) {
    let pidfile = PathBuf::from(env_var("HOMEWAY_LADDER_PIDFILE"));
    let pid: i32 = std::fs::read_to_string(&pidfile)
        .expect("pidfile 可读")
        .trim()
        .parse()
        .expect("pidfile 是 pid");
    let skip = log_lines(exit_log);
    let kill_at = Instant::now();
    let st = std::process::Command::new("kill")
        .arg("-9")
        .arg(pid.to_string())
        .status()
        .expect("kill -9 可发");
    assert!(st.success(), "kill -9 必须成功（pid={pid}）");
    println!("[e2e-ladder] kill9 pid={pid} at t=0.000s");
    if !restart_delay.is_zero() {
        std::thread::sleep(restart_delay);
    }
    let cmd_parts: Vec<String> = env_var("HOMEWAY_LADDER_RESTART")
        .split_whitespace()
        .map(str::to_owned)
        .collect();
    // 重启命令是**脚本进程**（起完出口即退）；判据靠 E1 行而非进程退出码 —— 与 harness 的
    // `local-rust-exit.sh start` 同口径。收尸放旁路线程：不阻塞本线程的 E1 观测，也不留孤儿。
    let mut restart = std::process::Command::new(&cmd_parts[0])
        .args(&cmd_parts[1..])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .expect("重启命令可起");
    std::thread::spawn(move || {
        let _ = restart.wait();
    });
    println!(
        "[e2e-ladder] restart 指令 at t={:.3}s（delay={:?}）",
        kill_at.elapsed().as_secs_f64(),
        restart_delay
    );
    // E1 行首次观测（**判据起点**；5ms 粒度、方向偏晚 = 保守）
    let t_ready = wait_log_new(exit_log, skip, "serve 就绪", Duration::from_secs(25))
        .expect("重启后必须见到 E1 `serve 就绪` 行");
    println!(
        "[e2e-ladder] E1 `serve 就绪` 观测 at t={:.3}s（kill 后）",
        kill_at.elapsed().as_secs_f64()
    );
    (kill_at, t_ready)
}

/// 相位 A：停机 < `T_detect`（kill -9 后**立刻**重启）。
#[test]
#[ignore = "端到端（kill -9 故障注入）：需本地出口在跑（tools/quic-ladder-e2e.sh 驱动）"]
fn kill9_then_immediate_restart_recovers_within_t_recv() {
    phase_kill9_restart(Duration::from_millis(0), "A（停机 < T_detect）");
}

/// 相位 B：停机 5s（客户端必然先经历一次完整的失败链动作）。
#[test]
#[ignore = "端到端（kill -9 故障注入）：需本地出口在跑（tools/quic-ladder-e2e.sh 驱动）"]
fn kill9_then_restart_after_five_seconds_recovers_within_t_recv() {
    phase_kill9_restart(Duration::from_secs(5), "B（停机 5s）");
}

fn phase_kill9_restart(restart_delay: Duration, phase: &str) {
    let exit_log = PathBuf::from(env_var("HOMEWAY_LADDER_EXIT_LOG"));
    let port = exit_quic_port(&env_var("HOMEWAY_LADDER_TOKEN"));
    let trace = Trace::new();
    let lab = Lab::start(
        vec![Candidate {
            addr: SocketAddrV4::new(Ipv4Addr::LOCALHOST, port),
            via: Via::Direct,
        }],
        trace.clone(),
    );
    // 基线：先看几拍成功探活（在用档背靠背；同时证明探活流已就位）
    let base = lab.probe_ok();
    let (t_base, n_base) = lab
        .wait_probe_ok_above(base, Duration::from_secs(5))
        .expect("建连后必须立刻有成功快探");
    println!(
        "[e2e-ladder] 相位{phase}：基线快探成功 {n_base} 次（首拍观测 t={:.3}s）",
        t_base.duration_since(trace.t0).as_secs_f64()
    );

    // 注入：kill -9 + 相位规定的停机后重启
    let (kill_at, t_ready) = kill9_and_restart(&exit_log, restart_delay);
    let base_after_kill = lab.probe_ok();
    let (t_echo, n_echo) = lab
        .wait_probe_ok_above(base_after_kill, WAIT)
        .unwrap_or_else(|| panic!("{phase}：重启后 {WAIT:?} 内未见回显（阶梯未恢复）"));
    let t_recv = t_echo.duration_since(t_ready);
    let offline = t_ready.duration_since(kill_at);
    println!(
        "[e2e-ladder] 相位{phase}：回显成功 #{n_echo} at t={:.3}s（kill 后）",
        kill_at.elapsed().as_secs_f64()
    );
    println!(
        "[e2e-ladder] 相位{phase}：T_recv = {:.0}ms（≤ {}ms 判据；观测粒度 {POLL:?}、方向偏晚）",
        t_recv.as_millis(),
        T_RECV_LIMIT.as_millis()
    );
    println!(
        "[e2e-ladder] 相位{phase}：T_kill→E1 = {:.0}ms（出口停机 + 重启耗时，**不计入** T_recv）",
        offline.as_millis()
    );
    trace.dump("e2e-ladder");
    // T_detect 单独登记：C18「链路快探失败」行（= 复探失败定音）落纸时刻 − kill
    if let Some((at, line)) = trace.first("链路快探失败") {
        let t_detect = at.saturating_sub(kill_at.duration_since(trace.t0));
        println!(
            "[e2e-ladder] 相位{phase}：T_detect = {:.0}ms（首条实探失败 ≤2.1s）｜{line}",
            t_detect.as_millis()
        );
    } else {
        println!("[e2e-ladder] 相位{phase}：首探失败行未落纸（复探成功 ⇒ 抖动面：见下方行）");
    }
    if let Some((at, line)) = trace.first("链路动作选") {
        println!(
            "[e2e-ladder] 相位{phase}：动作选择 at t={:.3}s ｜{line}",
            at.as_secs_f64()
        );
    }
    if let Some((at, line)) = trace.first("链路重连中") {
        println!(
            "[e2e-ladder] 相位{phase}：重连起跑 at t={:.3}s ｜{line}",
            at.as_secs_f64()
        );
        let since_kill = at.saturating_sub(kill_at.duration_since(trace.t0));
        println!(
            "[e2e-ladder] 相位{phase}：T_detect(到动作) = {:.0}ms",
            since_kill.as_millis()
        );
    }

    assert!(
        t_recv <= T_RECV_LIMIT,
        "相位{phase}：T_recv={}ms 超判据 {}ms（§3.3 上界）",
        t_recv.as_millis(),
        T_RECV_LIMIT.as_millis()
    );
    // 阶梯的健康面：恢复后不得残留失败链（快照读数）
    let snap = lab.island.snapshot();
    println!(
        "[e2e-ladder] 相位{phase}：snap ladder_action={} fail_streak={} jitter_streak={} probe_ok={}",
        snap.ladder_action, snap.ladder_fail_streak, snap.ladder_jitter_streak, snap.ladder_probe_ok
    );
    assert_eq!(snap.ladder_fail_streak, 0, "恢复后失败链须归零");
    println!("[e2e-ladder] 相位{phase}：done");
}

// ---------------------------------------------------------------------------
// 负向①：瞬时黑洞（1.5s）⇒ 只记抖动、不动作
// ---------------------------------------------------------------------------

/// **判据（设计门 4-1(5) / S4 完成判据的负向用例①）**：瞬时黑洞 1.5s（< 复探总窗
/// 0.7+1.4=2.1s）⇒ 复探成功 ⇒ **只记抖动行**，**不产生** M/R/B 动作
/// （`ladder_action` 保持空、无「链路重连中/世代重建」行）。
#[test]
#[ignore = "端到端（黑洞窗口注入）：需本地出口在跑（tools/quic-ladder-e2e.sh 驱动）"]
fn transient_blackhole_window_only_jitters_and_takes_no_action() {
    let port = exit_quic_port(&env_var("HOMEWAY_LADDER_TOKEN"));
    let (mut proxy, ctrl, pport) = start_wedge(port);
    let trace = Trace::new();
    let lab = Lab::start(
        vec![Candidate {
            addr: SocketAddrV4::new(Ipv4Addr::LOCALHOST, pport),
            via: Via::Direct,
        }],
        trace.clone(),
    );
    assert_eq!(
        lab.winner,
        SocketAddrV4::new(Ipv4Addr::LOCALHOST, pport),
        "赛跑必须胜在**楔子端口**上（否则黑洞注入落不到路径上——假绿面）"
    );
    // 基线：穿代理的成功探活（证明代理在转发）
    let base = lab.probe_ok();
    let (t_base, n) = lab
        .wait_probe_ok_above(base, Duration::from_secs(8))
        .expect("经楔子代理必须能成功探活（代理转发面）");
    println!(
        "[e2e-ladder] 负向①：代理转发正常（快探 #{n} at t={:.3}s；winner={}）",
        t_base.duration_since(trace.t0).as_secs_f64(),
        lab.winner
    );
    // 投放黑洞窗口 1.5s（§3.3 的设计值；< 复探总窗 2.1s）
    let t_drop = Instant::now();
    std::fs::write(&ctrl, b"drop").expect("控制文件可写");
    println!("[e2e-ladder] 负向①：黑洞窗口起（1.5s）");
    std::thread::sleep(Duration::from_millis(1500));
    std::fs::remove_file(&ctrl).expect("控制文件可删");
    println!("[e2e-ladder] 负向①：黑洞窗口止（t={:.3}s）", t_drop.elapsed().as_secs_f64());
    // 窗口结束后必须恢复（复探成功 ⇒ 抖动面）
    let after = lab.probe_ok();
    let ok = lab.wait_probe_ok_above(after, Duration::from_secs(8));
    assert!(ok.is_some(), "窗口结束后须恢复（复探成功）");
    std::thread::sleep(Duration::from_millis(800)); // 留一点拍数（若有动作，早已发生）
    let snap = lab.island.snapshot();
    trace.dump("e2e-ladder");
    println!(
        "[e2e-ladder] 负向①：抖动行 {} 条；fail_streak={} action={:?}",
        trace.count("链路探活抖动"),
        snap.ladder_fail_streak,
        snap.ladder_action
    );
    assert!(trace.count("链路探活抖动") >= 1, "须记抖动行（复探成功）");
    assert!(
        trace.count("链路重连中") == 0 && trace.count("世代重建") == 0,
        "瞬时黑洞不得产生 R/B 动作"
    );
    assert!(
        snap.ladder_action.is_empty(),
        "瞬时黑洞不得留下动作（实得 {:?}）",
        snap.ladder_action
    );
    let _ = proxy.kill();
    let _ = std::fs::remove_file(&ctrl);
    println!("[e2e-ladder] 负向①：done");
}

// ---------------------------------------------------------------------------
// 负向②：回显永不返回但连接不关 ⇒ 必有动作（不得无限静默）
// ---------------------------------------------------------------------------

/// **判据（设计门 4-1(5) / S4 完成判据的负向用例②）**：「服务面卡死」形态——**回显永不
/// 返回、连接不关**（本用例用外向丢包复刻客户端可见的同一谓词：黑障持续 + QUIC 层无
/// CONNECT 帧 ⇒ `close_reason()` 为空 ⇒ 岛侧 `connections` 仍为 1）⇒ 复探两次后**必有动作**
/// （M/R/B 至少一条），不得无限静默。
#[test]
#[ignore = "端到端（持续黑障注入）：需本地出口在跑（tools/quic-ladder-e2e.sh 驱动）"]
fn wedge_without_close_must_escalate_to_an_action() {
    let port = exit_quic_port(&env_var("HOMEWAY_LADDER_TOKEN"));
    let (mut proxy, ctrl, pport) = start_wedge(port);
    let trace = Trace::new();
    let lab = Lab::start(
        vec![Candidate {
            addr: SocketAddrV4::new(Ipv4Addr::LOCALHOST, pport),
            via: Via::Direct,
        }],
        trace.clone(),
    );
    assert_eq!(
        lab.winner,
        SocketAddrV4::new(Ipv4Addr::LOCALHOST, pport),
        "赛跑必须胜在**楔子端口**上（否则黑洞注入落不到路径上——假绿面）"
    );
    let base = lab.probe_ok();
    lab.wait_probe_ok_above(base, Duration::from_secs(8))
        .expect("经楔子代理必须能成功探活（代理转发面）");
    // 持续黑障（窗口**不撤**）：回显永不返回、连接不关
    std::fs::write(&ctrl, b"drop").expect("控制文件可写");
    println!("[e2e-ladder] 负向②：持续黑障起（回显永不返回；连接不关）");
    let t_drop = Instant::now();
    let deadline = Instant::now() + Duration::from_secs(12);
    let mut action_line: Option<(Duration, String)> = None;
    while Instant::now() < deadline {
        if let Some((at, line)) = trace.first("链路动作选") {
            action_line = Some((at, line));
            break;
        }
        std::thread::sleep(POLL);
    }
    let (at, line) = action_line.unwrap_or_else(|| {
        trace.dump("e2e-ladder");
        panic!("持续黑障下必须有动作（复探两次之后），实得零动作行")
    });
    println!(
        "[e2e-ladder] 负向②：动作在 {:.0}ms 落纸（黑障**仍在**）｜{line}",
        t_drop.elapsed().as_millis()
    );
    // 「连接不关」的事实面：动作发生时岛仍持有连接（close_reason 为空）
    let snap = lab.island.snapshot();
    println!(
        "[e2e-ladder] 负向②：动作时 snap.connections={} via={:?} fail_streak={} probe_ok={}",
        snap.connections, snap.via, snap.ladder_fail_streak, snap.ladder_probe_ok
    );
    assert_eq!(snap.connections, 1, "连接须仍在（回显挂起 ≠ 连接关）");
    assert!(snap.ladder_fail_streak >= 1, "须已记失败链");
    let _ = at;
    // 黑障**持续期**内不得自愈（证明动作确实发生在黑障下，而不是「先撤障再动作」）
    let frozen = lab.probe_ok();
    std::thread::sleep(Duration::from_millis(2000));
    assert_eq!(
        lab.probe_ok(),
        frozen,
        "黑障未撤 ⇒ 回显不得恢复（注入有效性负判据）"
    );
    // 撤掉黑障 ⇒ 阶梯须自愈（重连完成 / 回显恢复）
    std::fs::remove_file(&ctrl).expect("控制文件可删");
    println!("[e2e-ladder] 负向②：黑障止 —— 等阶梯自愈");
    let after = lab.probe_ok();
    let ok = lab.wait_probe_ok_above(after, Duration::from_secs(25));
    trace.dump("e2e-ladder");
    println!(
        "[e2e-ladder] 负向②：重连中 {} 条 / 重连完成 {} 条 / 动作 {:?}",
        trace.count("链路重连中"),
        trace.count("链路重连完成"),
        lab.island.snapshot().ladder_action
    );
    assert!(ok.is_some(), "撤掉黑障后阶梯须自愈（回显恢复）");
    let _ = proxy.kill();
    let _ = std::fs::remove_file(&ctrl);
    println!("[e2e-ladder] 负向②：done");
}
