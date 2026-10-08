//! Q-H 批 E2E 回归（设计 §3.2）：非法配置 **不再打崩统一进程**（fail-fast + 零副作用）
//! + `--state` 形态族 fail-fast + `--help` 短路（CA13）。
//!
//! 全部用独立 `mktemp` state（`/tmp` 隔离），跑完杀进程 + 删目录；config 一律
//! **hermetic**（显式空闲 UDP 端口 / `bind_interface="none"` / `upnp=false` /
//! `stun=""` / `dns_port=0` / `files_root=<tmp>`——绝不绑 41641、绝不打真网）。

use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_homeway-cli");

fn mktemp(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "hw-qh-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// 探测一个空闲 UDP 端口（绑 127.0.0.1:0 读回后释放；小竞争面可接受）。
fn free_udp_port() -> u16 {
    let s = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    s.local_addr().unwrap().port()
}

/// hermetic 合法 config（serve 停用起步；`serve start` 时才真装配）。
fn hermetic_config(state: &Path, enabled: bool, listen: u16) -> String {
    format!(
        "[serve]\nenabled = {enabled}\nlisten = {listen}\nbind_interface = \"none\"\nupnp = false\n\
         stun = \"\"\nstun6 = \"\"\ndns_port = 0\nfiles_root = \"{}\"\n[relay]\nenabled = false\n",
        state.display()
    )
}

struct Unified {
    child: std::process::Child,
    state: PathBuf,
}

impl Unified {
    fn spawn(state: &Path) -> Unified {
        let log = state.join("unified.log");
        let out = std::fs::File::create(&log).unwrap();
        let err = out.try_clone().unwrap();
        let child = std::process::Command::new(BIN)
            .arg("--state")
            .arg(state)
            .stdin(std::process::Stdio::null())
            .stdout(out)
            .stderr(err)
            .spawn()
            .unwrap();
        Unified { child, state: state.to_owned() }
    }

    fn alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    fn log(&self) -> String {
        std::fs::read_to_string(self.state.join("unified.log")).unwrap_or_default()
    }
}

impl Drop for Unified {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn wait_sock(state: &Path, d: Duration) -> bool {
    let sock = state.join("control.sock");
    let deadline = Instant::now() + d;
    while Instant::now() < deadline {
        if std::os::unix::net::UnixStream::connect(&sock).is_ok() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

/// 不给 `--state` 的原样调用（speedtest 直连/portfwd 不吃该 flag）。
fn raw_cli(cwd: &Path, args: &[&str]) -> std::process::Output {
    std::process::Command::new(BIN).current_dir(cwd).args(args).output().unwrap()
}

fn cli(state: &Path, args: &[&str]) -> std::process::Output {
    std::process::Command::new(BIN)
        .args(args)
        .arg("--state")
        .arg(state)
        .output()
        .unwrap()
}

fn text(out: &std::process::Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// 形态①/②（设计 §3.1）：坏 config 的 `serve start` = 拒绝（rc=1 + 可行动文案）
/// **且进程存活** **且内存/文件一致**；修好 config 后 `serve start` 成功、终态 running。
#[test]
fn config_failfast_does_not_kill_unified() {
    let cases: &[(&str, &str)] = &[
        ("[serve]\nenabled = false\npeer_ttl = \"abc\"\n", "peer_ttl"),
        (
            "[serve]\nenabled = false\n\n[serve.tx_shape]\nrate_mbps = \"200\"\n",
            "rate_mbps",
        ),
        // Q-J F2（代码门 M3③）：新键值域同走 fail-fast + 零副作用 + 进程存活
        ("[serve]\nenabled = false\ndns_fallback = \"\"\n", "dns_fallback"),
        ("[serve]\nenabled = false\ndns_probe_target = [\"1.2.3.4:0\"]\n", "dns_probe_target"),
    ];
    for (bad, marker) in cases {
        let state = mktemp("failfast");
        std::fs::write(state.join("config.toml"), hermetic_config(&state, false, free_udp_port())).unwrap();
        let mut uni = Unified::spawn(&state);
        assert!(wait_sock(&state, Duration::from_secs(10)), "统一进程未就绪：{}", uni.log());

        // 注入坏 config → serve start 必须被拒绝。
        std::fs::write(state.join("config.toml"), bad).unwrap();
        let out = cli(&state, &["serve", "start"]);
        let t = text(&out);
        assert_eq!(out.status.code(), Some(1), "坏 config 的 serve start 应 rc=1：{t}");
        assert!(
            t.contains("config.toml") && t.contains(marker),
            "文案必须可行动（config.toml 路径 + 字段）：{t}"
        );
        // ① 进程存活（修前形态 = 统一进程 DEAD）。
        assert!(uni.alive(), "统一进程必须存活；log：{}", uni.log());
        // ② 文件逐字节未变（零副作用）。
        assert_eq!(std::fs::read_to_string(state.join("config.toml")).unwrap(), *bad);
        // ③ 状态面如实（enabled=false / stopped，不谎报）。
        let st = cli(&state, &["serve", "status"]);
        let s = text(&st);
        assert_eq!(st.status.code(), Some(0), "{s}");
        assert!(s.contains("enabled=false"), "状态面 enabled 必须为 false：{s}");
        assert!(s.contains("state=stopped"), "状态面必须 stopped：{s}");

        // 修好 config → serve start ∈ {started, already}；终态 running。
        let listen = free_udp_port();
        std::fs::write(state.join("config.toml"), hermetic_config(&state, false, listen)).unwrap();
        let out = cli(&state, &["serve", "start"]);
        let t = text(&out);
        assert_eq!(out.status.code(), Some(0), "修好后 serve start 应成功：{t}");
        assert!(
            t.contains("started") || t.contains("already"),
            "action ∈ {{started, already}}：{t}"
        );
        let st = cli(&state, &["serve", "status"]);
        let s = text(&st);
        assert!(s.contains("state=running"), "终局必须 running：{s}");
        // 收尾（避免残留引擎线程拖住进程退出）：stop 后即杀。
        let _ = cli(&state, &["serve", "stop"]);
        drop(uni);
        let _ = std::fs::remove_dir_all(&state);
    }
}

/// 形态③（设计 §3.1）：启动期非法 = **拒启**（exit 1 + 可行动文案）。
#[test]
fn bad_config_at_startup_refuses_to_start() {
    let state = mktemp("startup");
    std::fs::write(state.join("config.toml"), "[serve]\nenabled = true\npeer_ttl = \"abc\"\n").unwrap();
    let mut child = std::process::Command::new(BIN)
        .arg("--state")
        .arg(&state)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    // 有界等待（回归时不许挂死测试）。
    let deadline = Instant::now() + Duration::from_secs(15);
    let status = loop {
        if let Some(s) = child.try_wait().unwrap() {
            break s;
        }
        if Instant::now() >= deadline {
            // 代码门 L2：断言失败前先收尸（Child::drop 不杀进程——回归时会留一个
            // 持锁的统一进程 + /tmp 目录）。
            let _ = child.kill();
            let _ = child.wait();
            panic!("坏 config 未拒启（进程仍活着）");
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let mut so = String::new();
    let mut se = String::new();
    child.stdout.take().unwrap().read_to_string(&mut so).unwrap();
    child.stderr.take().unwrap().read_to_string(&mut se).unwrap();
    let all = format!("{so}{se}");
    assert_eq!(status.code(), Some(1), "拒启 exit 1：{all}");
    assert!(
        all.contains("config.toml") && all.contains("peer_ttl"),
        "拒启文案必须带路径 + 字段：{all}"
    );
    let _ = std::fs::remove_dir_all(&state);
}

/// F8/CA13：`serve stop --help` 只打用法（rc=0）且**不写 config**（修前 = 真停）。
#[test]
fn serve_stop_help_does_not_touch_config() {
    let state = mktemp("help");
    let body = hermetic_config(&state, true, free_udp_port());
    std::fs::write(state.join("config.toml"), &body).unwrap();
    for args in [
        vec!["serve", "stop", "--help"],
        vec!["serve", "--help"],
        vec!["relay", "stop", "-h"],
        vec!["host", "--help"],
        vec!["host", "add", "--help"],
        vec!["status", "--help"],
    ] {
        let out = cli(&state, &args);
        let t = text(&out);
        assert_eq!(out.status.code(), Some(0), "{args:?} 应 rc=0：{t}");
        assert!(t.contains("用法"), "{args:?} 必须打用法：{t}");
    }
    assert_eq!(
        std::fs::read_to_string(state.join("config.toml")).unwrap(),
        body,
        "--help 短路不得改 config"
    );
    let _ = std::fs::remove_dir_all(&state);
}

/// F2：`--state=DIR` 等号形全站点生效（不落 CWD）；缺值/吞 flag/空值 fail-fast。
#[test]
fn state_flag_forms_failfast_and_eq_form_accepted() {
    let cwd = mktemp("cwd");
    let state = mktemp("state");
    // ① 等号形被收下（token list；台账为空 = rc 0）——且**不落 CWD**。
    let out = std::process::Command::new(BIN)
        .current_dir(&cwd)
        .args(["serve", "token", "list"])
        .arg(format!("--state={}", state.display()))
        .output()
        .unwrap();
    let t = text(&out);
    assert_eq!(out.status.code(), Some(0), "{t}");
    assert!(state.join("serve").exists(), "等号形必须真的指到 --state 目录：{t}");
    assert!(!cwd.join("serve").exists(), "不得落 CWD（修前形态）：{t}");
    // ② 吞 flag：`status --state --json` ⇒ 缺值 exit 2（不静默把 --json 当目录）。
    let out = std::process::Command::new(BIN)
        .current_dir(&cwd)
        .args(["status", "--state", "--json"])
        .output()
        .unwrap();
    let t = text(&out);
    assert_eq!(out.status.code(), Some(2), "{t}");
    assert!(t.contains("缺值") && t.contains("--json"), "缺值文案须指名下一个 flag：{t}");
    // ③ 空值：`--state=` ⇒ exit 2。
    let out = std::process::Command::new(BIN)
        .current_dir(&cwd)
        .args(["status", "--state="])
        .output()
        .unwrap();
    let t = text(&out);
    assert_eq!(out.status.code(), Some(2), "{t}");
    assert!(t.contains("空值"), "{t}");
    let _ = std::fs::remove_dir_all(&cwd);
    let _ = std::fs::remove_dir_all(&state);
}

/// F7：数值/布尔 flag 非法值 fail-fast（不静默回退）。
#[test]
fn numeric_and_bool_flags_failfast() {
    let state = mktemp("flags");
    // 数值：`--rounds abc`（此前静默回落 3）。
    let out = cli(&state, &["speedtest", "--token", "hmw1xxx", "--rounds", "abc"]);
    let t = text(&out);
    assert_eq!(out.status.code(), Some(2), "{t}");
    assert!(t.contains("--rounds"), "{t}");
    // 布尔：`--json=maybe`（此前静默 true）。
    let out = cli(&state, &["status", "--json=maybe"]);
    let t = text(&out);
    assert_eq!(out.status.code(), Some(2), "{t}");
    assert!(t.contains("--json") && t.contains("非法值"), "{t}");
    // 代码门 M4：等号形真的落到直连形态（此前 `speedtest --token=X` 判成守护托管 →
    // 「未知参数：--token」exit 2；现在应走 token 解码 → rc=1「token 解析失败」）。
    // 注：speedtest 直连/portfwd 不吃 `--state`（身份面是 --identity-dir）——按原样调用。
    let out = raw_cli(&state, &["speedtest", "--token=hmw1xxx"]);
    let t = text(&out);
    assert_eq!(out.status.code(), Some(1), "等号形应走直连形态：{t}");
    assert!(t.contains("token 解析失败"), "{t}");
    // 代码门 M4：portfwd 等号形同样生效（此前死代码 + 未知参数）。
    let out = raw_cli(&state, &["portfwd", "--token=hmw1xxx", "--map", "15432:5432"]);
    let t = text(&out);
    assert_eq!(out.status.code(), Some(1), "{t}");
    assert!(t.contains("token 解析失败"), "{t}");
    // 代码门 M4：verb 级布尔显式值被接受（`--force=false` 不再「未知参数」；
    // 走到 token 解码失败 = 证明 get 解析面收下了该形态）。
    let out = cli(&state, &["files", "get", "/tmp/qh-none", "--force=false", "--token=hmw1xxx"]);
    let t = text(&out);
    assert!(!t.contains("未知参数"), "--force=false 必须被收下：{t}");
    assert!(t.contains("token 解析失败"), "{t}");
    let _ = std::fs::remove_dir_all(&state);
}
