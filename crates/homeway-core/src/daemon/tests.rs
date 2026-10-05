//! daemon 模块测试：①冻结契约夹具对拍（fixtures/control-cp-v1/frames.jsonl——
//! 43 帧逐条：帧封装/JSON 语义等价/流 body 逐字节；部分控制帧加**字节级**编码
//! 对拍钉键序）；②服务器全协议集成（真实 UDS + 帧 + JSON，走 client.rs 真实
//! 消费者路径——握手/host 面/快照/订阅回放与在线/stream 双向/close/未知 op/
//! bad_request/非法帧 goodbye/在途 overrun）。

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::json;

use super::client::{ClientErr, ControlClient};
use super::frame::{self, Op};
use super::hosts::HostRecord;
use super::proto::*;
use super::server::{ControlServer, ServerConfig};
use super::vocab;
use super::{Backend, RoleOp, RoleOpOut, StreamConn};

// ---------- fixtures 对拍 ----------

fn fixtures_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/control-cp-v1").join(name)
}

#[derive(serde::Deserialize)]
struct Fixture {
    name: String,
    op: String,
    hex: String,
    #[serde(default)]
    expect: serde_json::Value,
}

/// 全部 43 帧向量：帧封装解码 + expect 语义对拍（decode_check.py 同款口径）。
#[test]
fn control_frames_fixture_decode() {
    let raw = std::fs::read_to_string(fixtures_path("frames.jsonl")).unwrap();
    let lines: Vec<&str> = raw.lines().filter(|l| !l.trim().is_empty()).collect();
    assert_eq!(lines.len(), 43, "冻结契约 = 43 帧（变多/变少都是契约漂移——先核对 spec）");
    let mut n = 0;
    for line in lines {
        let fx: Fixture = serde_json::from_str(line).unwrap();
        let bytes = hex_to_bytes(&fx.hex);
        assert!(bytes.len() >= 5, "{}: 帧短于 5 字节头", fx.name);
        let op_code = bytes[0];
        let n_len = u32::from_be_bytes([bytes[1], bytes[2], bytes[3], bytes[4]]);
        let body = &bytes[5..];
        assert_eq!(n_len as usize, body.len(), "{}: 声明长度 ≠ body 实长", fx.name);
        let declared_op = u8::from_str_radix(fx.op.trim_start_matches("0x"), 16).unwrap();
        assert_eq!(op_code, declared_op, "{}: 帧内 op 与声明不符", fx.name);
        let op = Op::from_code(op_code).expect("fixtures 内 op 恒在码位表");
        // 流帧：body = [streamId:4][bytes] 逐字节比对。
        if op == Op::StreamData {
            let (id, payload) = frame::decode_stream_body(body).expect("流前缀");
            let want = fx.expect.get("stream").cloned().unwrap_or(fx.expect.clone());
            assert_eq!(
                id,
                want.get("streamId").and_then(|v| v.as_u64()).unwrap_or(0) as u32,
                "{}: streamId",
                fx.name
            );
            let want_hex = want.get("bytesHex").and_then(|v| v.as_str()).unwrap_or("");
            assert_eq!(hex_str(payload), want_hex.to_lowercase(), "{}: 流字节", fx.name);
        } else {
            // 控制类：JSON 语义等价（键序不敏感）。
            let decoded: serde_json::Value = serde_json::from_slice(body).unwrap();
            let want = fx.expect.get("json").cloned().unwrap_or(fx.expect.clone());
            assert_eq!(decoded, want, "{}: JSON 语义不等价", fx.name);
        }
        n += 1;
    }
    assert_eq!(n, 43);
}

/// 字节级编码对拍（键序钉死）：fixtures 里结构最简单的五类帧用本仓编码器重产，
/// 逐字节必须与冻结向量一致（serde 声明序 = Go struct 序）。
#[test]
fn control_frames_fixture_encode_bytes() {
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("hello.cli", encode_json_frame(
            Op::Hello,
            &HelloBody {
                proto_version: 1,
                frontend: FrontendInfo {
                    kind: "cli".into(),
                    name: "homeway".into(),
                    version: "0.5.0".into(),
                },
            },
        )),
        ("welcome.ok", encode_json_frame(
            Op::Welcome,
            &WelcomeBody {
                server_version: "0.5.0".into(),
                generation: "00112233445566778899aabbccddeeff".into(),
                server_seq: 42,
            },
        )),
        ("reload.proto_mismatch", encode_json_frame(
            Op::Reload,
            &ReloadBody { reason: "proto_mismatch".into() },
        )),
        ("goodbye.overrun", encode_json_frame(
            Op::Goodbye,
            &GoodbyeBody { reason: "overrun".into() },
        )),
        ("rsp.error.unknown_op", encode_json_frame(
            Op::Rsp,
            &response_body(9, None, Some(&OpError::code(vocab::CODE_UNKNOWN_OP))),
        )),
    ];
    let raw = std::fs::read_to_string(fixtures_path("frames.jsonl")).unwrap();
    for (want_name, produced) in cases {
        let line = raw
            .lines()
            .find(|l| l.contains(&format!("\"name\":\"{want_name}\"")))
            .unwrap_or_else(|| panic!("fixture {want_name} 不在文件里"));
        let fx: Fixture = serde_json::from_str(line).unwrap();
        assert_eq!(hex_str(&produced), fx.hex.to_lowercase(), "{want_name}: 字节级不一致（键序/omitempty 漂移）");
    }
}

fn hex_to_bytes(s: &str) -> Vec<u8> {
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
}

fn hex_str(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

// ---------- 服务器集成（真实 UDS + client.rs 消费路径） ----------

/// 回声流腿：写什么回什么；`eof_after` 次读后 EOF（默认立即回声不 EOF——测试
/// 主动 close 流收 end）。
struct EchoConn {
    inbox: Mutex<VecDeque<Vec<u8>>>,
    closed: AtomicBool,
    written: AtomicU64,
}

impl EchoConn {
    fn new() -> Arc<EchoConn> {
        Arc::new(EchoConn { inbox: Mutex::new(VecDeque::new()), closed: AtomicBool::new(false), written: AtomicU64::new(0) })
    }
}

impl StreamConn for EchoConn {
    fn read_chunk(&self) -> std::io::Result<Vec<u8>> {
        if self.closed.load(Ordering::SeqCst) {
            return Err(std::io::Error::other("closed"));
        }
        loop {
            if let Some(v) = self.inbox.lock().unwrap().pop_front() {
                return Ok(v);
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn write_chunk(&self, data: &[u8]) -> std::io::Result<usize> {
        if self.closed.load(Ordering::SeqCst) {
            return Err(std::io::Error::other("closed"));
        }
        self.written.fetch_add(data.len() as u64, Ordering::SeqCst);
        // 回声：写进读队列（分块 ≤4B 便于观察交错——term 键盘量级）。
        for chunk in data.chunks(4) {
            self.inbox.lock().unwrap().push_back(chunk.to_vec());
        }
        Ok(data.len())
    }

    fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
    }

    fn shutdown_write(&self) {
        // 测试桩无半关面：退化为全关（trait 契约允许）。
        self.close();
    }
}

struct MockBackend {
    hosts: Mutex<Vec<HostRecord>>,
    echo: Mutex<Option<Arc<EchoConn>>>,
    slow_add: Duration,
    not_ready: AtomicBool,
    /// 承载面内存版（低-11：9 op 的 server 级用例——内存语义足以钉 wire 面：
    /// op 名/载荷字段/错误码映射；真语义面归 carriers 单测 + CA 实采）。
    forwards: Mutex<Vec<super::ForwardRule>>,
    socks_mem: Mutex<Vec<(String, bool, u16)>>, // (host, on, port)
    speed_mem: Mutex<std::collections::HashMap<String, bool>>, // host -> running
}

impl MockBackend {
    fn new() -> Arc<MockBackend> {
        Arc::new(MockBackend {
            hosts: Mutex::new(Vec::new()),
            echo: Mutex::new(None),
            slow_add: Duration::ZERO,
            not_ready: AtomicBool::new(false),
            forwards: Mutex::new(Vec::new()),
            socks_mem: Mutex::new(Vec::new()),
            speed_mem: Mutex::new(std::collections::HashMap::new()),
        })
    }

    fn host_exists(&self, host_hex: &str) -> bool {
        self.hosts.lock().unwrap().iter().any(|r| r.id == host_hex)
    }
}

impl Backend for MockBackend {
    fn server_version(&self) -> String {
        "test-0.1".to_owned()
    }

    fn roles_status(&self) -> Vec<RoleBrief> {
        vec![RoleBrief { name: "control".into(), state: "running".into(), restarts: 0, reason: None }]
    }

    fn host_briefs(&self) -> Vec<HostBrief> {
        self.hosts
            .lock()
            .unwrap()
            .iter()
            .map(|r| HostBrief { id: r.id.clone(), name: r.name.clone(), added_at: r.added_at })
            .collect()
    }

    fn add_host(&self, name: &str, token: &str, _force: bool) -> Result<HostAddResult, BackendErr> {
        if self.slow_add > Duration::ZERO {
            std::thread::sleep(self.slow_add);
        }
        if token == "bad" {
            return Err(BackendErr::BadToken("mock".into()));
        }
        let id = format!("{token:0>64}");
        let mut hs = self.hosts.lock().unwrap();
        if hs.iter().any(|r| r.token == token) {
            return Err(BackendErr::HostExists);
        }
        let rec = HostRecord { id: id.clone(), name: (!name.is_empty()).then(|| name.to_owned()), token: token.to_owned(), added_at: 1 };
        hs.push(rec);
        Ok(HostAddResult {
            id,
            name: (!name.is_empty()).then(|| name.to_owned()),
            added_at: 1,
            reach: HostReach { tier: vocab::REACH_TIER_SKIPPED.to_owned(), best_ep: None, rtt_ms: None, tested: Vec::new() },
        })
    }

    fn remove_host(&self, host_hex: &str) -> Result<(), BackendErr> {
        let mut hs = self.hosts.lock().unwrap();
        let n = hs.len();
        hs.retain(|r| r.id != host_hex);
        if hs.len() == n {
            return Err(BackendErr::NoHost);
        }
        Ok(())
    }

    fn host_states(&self) -> Vec<HostState> {
        self.hosts
            .lock()
            .unwrap()
            .iter()
            .map(|r| HostState {
                id: r.id.clone(),
                name: r.name.clone(),
                state: "ready".into(),
                reason: None,
                link: Some(HostLink { via: "direct".into(), ep: "127.0.0.1:1".into(), rtt_ms: 3, at: 7 }),
                stats: Some(HostRxTx { rx_bytes: 10, tx_bytes: 20 }),
                added_at: Some(r.added_at),
            })
            .collect()
    }

    fn dial_stream(&self, kind: &str, host_hex: &str) -> Result<Arc<dyn StreamConn>, BackendErr> {
        if kind != vocab::STREAM_KIND_TERM && kind != vocab::STREAM_KIND_FILES {
            return Err(BackendErr::BadStreamKind(kind.to_owned()));
        }
        if !self.hosts.lock().unwrap().iter().any(|r| r.id == host_hex) {
            return Err(BackendErr::NoHost);
        }
        let e = EchoConn::new();
        *self.echo.lock().unwrap() = Some(Arc::clone(&e));
        Ok(e)
    }

    fn not_ready(&self) -> bool {
        self.not_ready.load(Ordering::SeqCst)
    }

    fn role_op(&self, _op: RoleOp) -> Result<RoleOpOut, BackendErr> {
        Err(BackendErr::RoleStopped)
    }

    // 承载面 9 方法（低-11：内存版实装——端口全局唯一 / no_host / 幂等 off /
    // speedtest busy 相位的最小语义）。

    fn forward_add(&self, rule: super::ForwardRule) -> Result<super::carriers::forward::ForwardState, BackendErr> {
        if !self.host_exists(&rule.host) {
            return Err(BackendErr::NoHost);
        }
        let mut fs = self.forwards.lock().unwrap();
        if fs.iter().any(|r| r.listen == rule.listen) {
            return Err(BackendErr::Other(format!("本地端口 {} 已被其他转发或 socks 占用", rule.listen)));
        }
        fs.push(rule.clone());
        Ok(super::carriers::forward::ForwardState {
            state: "listening".to_owned(),
            err: String::new(),
            conns: 0,
            rejected: 0,
            rule,
        })
    }

    fn forward_remove(&self, host_hex: &str, listen: u16) -> Result<(), BackendErr> {
        let mut fs = self.forwards.lock().unwrap();
        let n = fs.len();
        fs.retain(|r| !(r.host == host_hex && r.listen == listen));
        if fs.len() == n {
            return Err(BackendErr::Other(format!("转发不存在（{host_hex} :{listen}）")));
        }
        Ok(())
    }

    fn forward_list(&self, host_hex: &str) -> Vec<super::carriers::forward::ForwardState> {
        self.forwards
            .lock()
            .unwrap()
            .iter()
            .filter(|r| host_hex.is_empty() || r.host == host_hex)
            .map(|r| super::carriers::forward::ForwardState {
                state: "listening".to_owned(),
                err: String::new(),
                conns: 1,
                rejected: 0,
                rule: r.clone(),
            })
            .collect()
    }

    fn socks_on(&self, host_hex: &str, listen: u16) -> Result<u16, BackendErr> {
        if !self.host_exists(host_hex) {
            return Err(BackendErr::NoHost);
        }
        let mut m = self.socks_mem.lock().unwrap();
        let port = if listen == 0 {
            m.iter().find(|(h, _, _)| h == host_hex).map(|(_, _, p)| *p).unwrap_or(super::carriers::SOCKS_DEFAULT_LISTEN)
        } else {
            listen
        };
        if let Some(e) = m.iter_mut().find(|(h, _, _)| h == host_hex) {
            e.1 = true;
            e.2 = port;
        } else {
            m.push((host_hex.to_owned(), true, port));
        }
        Ok(port)
    }

    fn socks_off(&self, host_hex: &str) -> Result<u16, BackendErr> {
        let mut m = self.socks_mem.lock().unwrap();
        let Some(e) = m.iter_mut().find(|(h, _, _)| h == host_hex) else {
            return Err(BackendErr::Other("socks 未开过（无记忆端口）".into()));
        };
        e.1 = false;
        Ok(e.2)
    }

    fn socks_states(&self) -> Vec<super::carriers::SocksState> {
        self.socks_mem
            .lock()
            .unwrap()
            .iter()
            .map(|(h, on, p)| super::carriers::SocksState {
                host: h.clone(),
                on: *on,
                listen: *p,
                conns: 0,
                err: String::new(),
            })
            .collect()
    }

    fn speedtest_start(
        &self,
        host_hex: &str,
        _p: super::SpeedtestParams,
    ) -> Result<super::carriers::speedrun::SpeedtestAck, BackendErr> {
        if !self.host_exists(host_hex) {
            return Err(BackendErr::NoHost);
        }
        let mut m = self.speed_mem.lock().unwrap();
        if m.get(host_hex).copied().unwrap_or(false) {
            return Ok(super::carriers::speedrun::SpeedtestAck { phase: "busy", reason: Some("上一轮还在跑") });
        }
        m.insert(host_hex.to_owned(), true);
        Ok(super::carriers::speedrun::SpeedtestAck { phase: "waiting", reason: None })
    }

    fn speedtest_status(&self, host_hex: &str) -> Result<Option<super::SpeedtestStatus>, BackendErr> {
        if !self.host_exists(host_hex) {
            return Err(BackendErr::NoHost);
        }
        Ok(if self.speed_mem.lock().unwrap().get(host_hex).copied().unwrap_or(false) {
            Some(super::SpeedtestStatus {
                waiting: false,
                wait_remain_ms: 0,
                phase: "down".to_owned(),
                bytes: 42,
                elapsed_ms: 7,
                inst_bps: 1.0,
                usage_down: 0,
                usage_up: 0,
                result: None,
            })
        } else {
            None
        })
    }

    fn speedtest_cancel(&self, host_hex: &str) -> Result<(), BackendErr> {
        if !self.host_exists(host_hex) {
            return Err(BackendErr::NoHost);
        }
        self.speed_mem.lock().unwrap().insert(host_hex.to_owned(), false);
        Ok(())
    }
}

fn start_server(tag: &str, backend: Arc<dyn Backend>) -> (Arc<ControlServer>, PathBuf, Arc<super::bus::Bus>, std::thread::JoinHandle<()>) {
    let dir = std::env::temp_dir().join(format!("hw-ctl-srv-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let (sock, ln) = super::listen::listen_control(&dir).unwrap();
    let bus = Arc::new(super::bus::Bus::new());
    let srv = ControlServer::new(ServerConfig {
        server_version: "test-0.1".to_owned(),
        bus: Arc::clone(&bus),
        backend,
        logf: Arc::new(|_| {}),
    });
    let s2 = Arc::clone(&srv);
    let h = std::thread::spawn(move || {
        let _ = s2.serve(ln);
    });
    (srv, sock, bus, h)
}

fn short() -> Duration {
    Duration::from_secs(5)
}

#[test]
fn server_full_roundtrip() {
    let backend = MockBackend::new();
    let (srv, sock, bus, serve_h) = start_server("full", backend as Arc<dyn Backend>);

    // ---- 握手 ----
    let (c, welcome) = ControlClient::dial(&sock, "cli", "test").unwrap();
    assert_eq!(welcome.server_version, "test-0.1");
    assert_eq!(welcome.generation, bus.generation());
    assert_eq!(welcome.server_seq, 0);

    // ---- host.add / host.list / host.add 重复 / host.remove ----
    let v = c.request("host.add", Some(json!({"token": "aaaa", "name": "mbp"})), short()).unwrap();
    let id = v["id"].as_str().unwrap().to_owned();
    assert_eq!(v["reach"]["tier"], "skipped");
    let v = c.request("host.list", None, short()).unwrap();
    assert_eq!(v["hosts"][0]["id"].as_str().unwrap(), id);
    assert_eq!(v["hosts"][0]["name"].as_str().unwrap(), "mbp");
    let e = c.request("host.add", Some(json!({"token": "aaaa"})), short()).unwrap_err();
    assert_eq!(e.code, "host_exists");
    let e = c.request("host.add", Some(json!({"token": "bad"})), short()).unwrap_err();
    assert_eq!(e.code, "bad_token");
    // 缺 token → bad_request。
    let e = c.request("host.add", Some(json!({"name": "x"})), short()).unwrap_err();
    assert_eq!(e.code, "bad_request");
    // 未知 op → unknown_op（不断连）。
    let e = c.request("host.nuke", None, short()).unwrap_err();
    assert_eq!(e.code, "unknown_op");
    // unknown_op 后连接仍活（同连接后续请求成功）。
    c.request("host.list", None, short()).unwrap();

    // ---- snapshot.get（seq 先读）----
    let v = c.request("snapshot.get", None, short()).unwrap();
    assert_eq!(v["hosts"][0]["state"], "ready");
    assert_eq!(v["hosts"][0]["link"]["via"], "direct");
    assert_eq!(v["generation"], bus.generation());

    // ---- 订阅：确认 → 回放 → 在线 ----
    bus.publish(vocab::EventPayload::SessionAdded { host: "h1".into(), name: "n".into(), added_at: 1 });
    let seq0 = bus.current_seq();
    // cursor=0 = 从头续播（纯在线订阅 cursor=None 无回放——Go 同语义）。
    let sub = c.subscribe(&["session".to_owned()], Some(0), "", bus.generation(), short()).unwrap();
    assert_eq!(sub.cursor, seq0);
    let events = c.take_events().unwrap();
    // 回放段（订阅前发的事件）应先到。
    let ev = events.recv_timeout(short()).unwrap();
    assert_eq!((ev.seq, ev.kind.as_str()), (1, "session.added"));
    // 在线事件随后。
    bus.publish(vocab::EventPayload::SessionRemoved { host: "h1".into(), reason: "user".into() });
    let ev = events.recv_timeout(short()).unwrap();
    assert_eq!(ev.kind, "session.removed");
    // 代际失配 → cursor_stale。
    let e = c.subscribe(&["session".to_owned()], None, "", "wrong-gen", short()).unwrap_err();
    assert_eq!(e.code, "cursor_stale");
    // 超前游标 → bad_request。
    let e = c
        .subscribe(&["session".to_owned()], Some(9999), "", bus.generation(), short())
        .unwrap_err();
    assert_eq!(e.code, "bad_request");

    // ---- 流：open → 双向 → close ----
    let st = match c.open_stream(vocab::STREAM_KIND_TERM, &id, short()) {
        Ok(st) => st,
        Err(e) => panic!("stream.open 应成功：{e}"),
    };
    assert!(st.stream_id() > 0);
    c.stream_send(&st, b"hello").unwrap();
    // 回声（EchoConn 4B 分块——收齐）。
    let mut got = Vec::new();
    while got.len() < 5 {
        let Some(chunk) = st.recv_timeout(short()) else { break };
        got.extend_from_slice(&chunk);
    }
    assert_eq!(got, b"hello");
    c.stream_close(&st, short()).unwrap();
    // end 帧应到达（closed）；此后 recv 恒 None、send 报终结。
    let deadline = std::time::Instant::now() + short();
    while st.end_reason().is_none() && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(st.end_reason().as_deref(), Some("closed"));
    assert!(matches!(c.stream_send(&st, b"x"), Err(ClientErr::StreamEnded(_))));
    // 已关流再 close → no_stream。
    let e = c
        .request("stream.close", Some(json!({"streamId": st.stream_id()})), short())
        .unwrap_err();
    assert_eq!(e.code, "no_stream");

    // ---- stream.open 对不在表主机 → no_host ----
    match c.open_stream(vocab::STREAM_KIND_TERM, "ff", short()) {
        Err(e) => assert_eq!(e.code, "no_host"),
        Ok(_) => panic!("不在表主机应 no_host"),
    }
    // kind 值域外 → bad_request。
    match c.open_stream("socks", &id, short()) {
        Err(e) => assert_eq!(e.code, "bad_request"),
        Ok(_) => panic!("kind 值域外应 bad_request"),
    }

    // ---- host.remove ----
    let v = c.request("host.remove", Some(json!({"host": id})), short()).unwrap();
    assert_eq!(v["removed"], json!(true));
    let e = c.request("host.remove", Some(json!({"host": id})), short()).unwrap_err();
    assert_eq!(e.code, "no_host");

    // ---- daemon.status（骨架面 + pid）----
    let v = c.request("daemon.status", None, short()).unwrap();
    assert_eq!(v["serverVersion"], "test-0.1");
    assert_eq!(v["roles"][0]["name"], "control");
    assert!(v["pid"].as_i64().unwrap() > 0);

    // ---- 优雅收工：客户端 close → 服务端 shutdown → goodbye 尽力面 ----
    c.close();
    srv.shutdown();
    // drop listener 令 serve 返回。
    // （listener 已在 serve 线程持有——shutdown 后 accept 返回，线程退出。）
    let _ = serve_h.join();
    let _ = std::fs::remove_dir_all(sock.parent().unwrap());
}

/// recv_wait（无预算阻塞收）必须被**数据到达**唤醒——不是只靠流终结/超时逃生。
/// 回归背景（B0-2b 第 2 棒实测）：blocking_push 漏 notify_all 时，远程 term 腿的
/// 首帧要睡到流终结（15s HELLO 超时）才醒——recv_timeout 因自带超时幸免，本测试
/// 用 recv_wait 钉住唤醒链。
#[test]
fn stream_recv_wait_wakes_on_data() {
    let backend = MockBackend::new();
    let (srv, sock, _bus, serve_h) = start_server("recvwake", backend as Arc<dyn Backend>);
    let (c, _) = ControlClient::dial(&sock, "cli", "test").unwrap();
    c.request("host.add", Some(json!({"token": "aaaa", "name": "mbp"})), short()).unwrap();
    let id: String = c.request("host.list", None, short()).unwrap()["hosts"][0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let st = c.open_stream(vocab::STREAM_KIND_TERM, &id, short()).unwrap();

    // 先挂 recv_wait（此刻队列空——线程必须在 recv_cv 上睡），再发数据。
    let st2 = Arc::clone(&st);
    let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
    let waiter = std::thread::spawn(move || {
        if let Some(v) = st2.recv_wait() {
            let _ = tx.send(v);
        }
    });
    std::thread::sleep(Duration::from_millis(100)); // 确保消费面已挂起
    c.stream_send(&st, b"ping").unwrap(); // EchoConn 回声 → 下行 stream.data → blocking_push
    let got = rx.recv_timeout(Duration::from_secs(3));
    assert!(
        matches!(got.as_deref(), Ok(b"ping")),
        "recv_wait 应被数据到达唤醒（≤3s），得 {got:?}"
    );
    // 收尾解挂线程（流终结路径 notify——此处显式 close）。
    c.stream_close(&st, short()).unwrap();
    let _ = waiter.join();
    c.close();
    srv.shutdown();
    let _ = serve_h.join();
    let _ = std::fs::remove_dir_all(sock.parent().unwrap());
}

#[test]
fn server_bad_frame_gets_goodbye_and_disconnect() {
    let (srv, sock, _bus, serve_h) = start_server("badframe", MockBackend::new());
    // 裸 socket：先发合法 hello 握手，再发声明超限的帧头 → goodbye(bad_frame) 断连。
    use std::io::{Read, Write};
    let mut s = std::os::unix::net::UnixStream::connect(&sock).unwrap();
    let _ = s.set_read_timeout(Some(Duration::from_millis(200)));
    let hello = encode_json_frame(
        Op::Hello,
        &HelloBody {
            proto_version: 1,
            frontend: FrontendInfo { kind: "cli".into(), name: "t".into(), version: "0".into() },
        },
    );
    s.write_all(&hello).unwrap();
    // 超限：控制帧声明 2MiB（> 1MiB 上限）。
    let mut bad = vec![Op::Rsp.code()];
    bad.extend_from_slice(&(2u32 << 20).to_be_bytes());
    s.write_all(&bad).unwrap();
    // 读到 goodbye(bad_frame) 后 EOF。
    let mut buf = [0u8; 128];
    let mut got = Vec::new();
    let deadline = std::time::Instant::now() + short();
    while got.len() < 64 && std::time::Instant::now() < deadline {
        match s.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => got.extend_from_slice(&buf[..n]),
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut =>
            {
                std::thread::sleep(Duration::from_millis(5))
            }
            Err(_) => break,
        }
    }
    // 第一帧 = welcome；随后 goodbye(bad_frame)。
    let head = &got[..5];
    assert_eq!(head[0], Op::Welcome.code());
    let wlen = u32::from_be_bytes([head[1], head[2], head[3], head[4]]) as usize;
    let rest = &got[5 + wlen..];
    assert_eq!(rest[0], Op::Goodbye.code(), "超限帧应回 goodbye（bad_frame）");
    let blen = u32::from_be_bytes([rest[1], rest[2], rest[3], rest[4]]) as usize;
    let body: GoodbyeBody = serde_json::from_slice(&rest[5..5 + blen]).unwrap();
    assert_eq!(body.reason, "bad_frame");
    srv.shutdown();
    let _ = serve_h.join();
    let _ = std::fs::remove_dir_all(sock.parent().unwrap());
}

#[test]
fn server_request_overrun_disconnects() {
    // 慢 host.add（800ms/条）+ 背靠背 40 条请求（不读应答）→ 在途超 32 → goodbye(overrun)。
    let dir = std::env::temp_dir().join(format!("hw-ctl-srv-ovr-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let (sock, ln) = super::listen::listen_control(&dir).unwrap();
    let bus = Arc::new(super::bus::Bus::new());
    let slow_backend = SlowBackend { delay: Duration::from_millis(800) };
    let srv = ControlServer::new(ServerConfig {
        server_version: "t".into(),
        bus,
        backend: Arc::new(slow_backend),
        logf: Arc::new(|_| {}),
    });
    let s2 = Arc::clone(&srv);
    let h = std::thread::spawn(move || {
        let _ = s2.serve(ln);
    });
    use std::io::{Read, Write};
    let mut s = std::os::unix::net::UnixStream::connect(&sock).unwrap();
    let _ = s.set_read_timeout(Some(Duration::from_millis(200))); // deadline 复检可触达
    let hello = encode_json_frame(
        Op::Hello,
        &HelloBody {
            proto_version: 1,
            frontend: FrontendInfo { kind: "cli".into(), name: "t".into(), version: "0".into() },
        },
    );
    s.write_all(&hello).unwrap();
    std::thread::sleep(Duration::from_millis(200)); // 等 welcome 写出
    // 40 条 host.add 背靠背（慢工位 → 在途堆栈；token 必须是字符串——数字会被
    // 载荷解析秒拒，在途堆不起来）。
    let mut burst = Vec::new();
    for i in 0..40 {
        let req = RequestBody { corr: i + 1, op: "host.add".into(), args: Some(json!({"token": i.to_string()})) };
        burst.extend_from_slice(&encode_json_frame(Op::Req, &req));
    }
    s.write_all(&burst).unwrap();
    // 应收到 goodbye(overrun) 且连接断开。
    let mut buf = [0u8; 2048];
    let mut got = Vec::new();
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while std::time::Instant::now() < deadline {
        match s.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => got.extend_from_slice(&buf[..n]),
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut =>
            {
                std::thread::sleep(Duration::from_millis(10))
            }
            Err(_) => break,
        }
        if got.windows(9).any(|w| w == b"\"overrun\"") {
            break;
        }
    }
    assert!(
        got.windows(9).any(|w| w == b"\"overrun\""),
        "在途超界应回 goodbye(overrun)，实收 {} 字节",
        got.len()
    );
    srv.shutdown();
    let _ = h.join();
    let _ = std::fs::remove_dir_all(&dir);
}

struct SlowBackend {
    delay: Duration,
}

impl Backend for SlowBackend {
    fn server_version(&self) -> String {
        "t".into()
    }
    fn roles_status(&self) -> Vec<RoleBrief> {
        Vec::new()
    }
    fn host_briefs(&self) -> Vec<HostBrief> {
        Vec::new()
    }
    fn add_host(&self, _name: &str, _token: &str, _force: bool) -> Result<HostAddResult, BackendErr> {
        std::thread::sleep(self.delay);
        Err(BackendErr::Other("slow-fail".into()))
    }
    fn remove_host(&self, _host: &str) -> Result<(), BackendErr> {
        Ok(())
    }
    fn host_states(&self) -> Vec<HostState> {
        Vec::new()
    }
    fn dial_stream(&self, _kind: &str, _host: &str) -> Result<Arc<dyn StreamConn>, BackendErr> {
        Err(BackendErr::NoHost)
    }
    fn not_ready(&self) -> bool {
        false
    }
    fn role_op(&self, _op: RoleOp) -> Result<RoleOpOut, BackendErr> {
        Err(BackendErr::RoleStopped)
    }
}

// ---------- 宽松解码（wire-2）：缺键帧不断连 ----------

#[test]
fn missing_fields_get_stable_errors_not_disconnect() {
    // Go encoding/json 零值语义：缺 corr = 0、缺 op = 空串（→ unknown_op 应答），
// 缺 version 的 hello 正常握手——**只有非法 JSON 才 bad_json 断连**。
    let (srv, sock, _bus, serve_h) = start_server("lenient", MockBackend::new());
    let (c, _w) = ControlClient::dial(&sock, "cli", "t").unwrap();
    // ① {} 请求：零值 op="" → unknown_op（连接仍活）。
    let e = c.request("", Some(json!({})), short()).unwrap_err();
    assert_eq!(e.code, "unknown_op");
    // ② 同连接仍活。
    c.request("host.list", None, short()).unwrap();
    c.close();
    // ③ 缺 version 的 hello：裸 socket 握手应成功拿 welcome。
    use std::io::{Read, Write};
    let mut s = std::os::unix::net::UnixStream::connect(&sock).unwrap();
    let _ = s.set_read_timeout(Some(Duration::from_millis(200)));
    let hello = encode_json_frame(
        Op::Hello,
        &serde_json::json!({"protoVersion": 1, "frontend": {"kind": "cli", "name": "t"}}),
    );
    s.write_all(&hello).unwrap();
    let mut buf = [0u8; 256];
    let mut got = Vec::new();
    let deadline = std::time::Instant::now() + short();
    while got.len() < 32 && std::time::Instant::now() < deadline {
        match s.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => got.extend_from_slice(&buf[..n]),
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut =>
            {
                std::thread::sleep(Duration::from_millis(5))
            }
            Err(_) => break,
        }
    }
    assert_eq!(got[0], Op::Welcome.code(), "缺 version 的 hello 应正常握手（零值容忍）");
    srv.shutdown();
    let _ = serve_h.join();
    let _ = std::fs::remove_dir_all(sock.parent().unwrap());
}

// ---------- 并发-2：stream.open 的 30s 级拨号计入在途 ----------

#[test]
fn slow_dial_counts_inflight_and_overruns() {
    // 慢 dial_stream（2s/次）+ 背靠背 40 条 stream.open：拨号全程占在途（Go defer
    // 同义）——第 33 条在途触发 goodbye(overrun)。
    struct SlowDial(#[allow(dead_code)] ());
    impl Backend for SlowDial {
        fn server_version(&self) -> String {
            "t".into()
        }
        fn roles_status(&self) -> Vec<RoleBrief> {
            Vec::new()
        }
        fn host_briefs(&self) -> Vec<HostBrief> {
            Vec::new()
        }
        fn add_host(&self, _n: &str, _t: &str, _f: bool) -> Result<HostAddResult, BackendErr> {
            Ok(HostAddResult {
                id: "x".into(),
                name: None,
                added_at: 0,
                reach: HostReach { tier: "skipped".into(), best_ep: None, rtt_ms: None, tested: Vec::new() },
            })
        }
        fn remove_host(&self, _h: &str) -> Result<(), BackendErr> {
            Ok(())
        }
        fn host_states(&self) -> Vec<HostState> {
            Vec::new()
        }
        fn dial_stream(&self, _k: &str, _h: &str) -> Result<Arc<dyn StreamConn>, BackendErr> {
            std::thread::sleep(Duration::from_secs(2));
            Err(BackendErr::NoHost)
        }
        fn not_ready(&self) -> bool {
            false
        }
        fn role_op(&self, _op: RoleOp) -> Result<RoleOpOut, BackendErr> {
            Err(BackendErr::RoleStopped)
        }
    }
    let dir = std::env::temp_dir().join(format!("hw-ctl-slowdial-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let (sock, ln) = super::listen::listen_control(&dir).unwrap();
    let srv = ControlServer::new(ServerConfig {
        server_version: "t".into(),
        bus: Arc::new(super::bus::Bus::new()),
        backend: Arc::new(SlowDial(())),
        logf: Arc::new(|_| {}),
    });
    let s2 = Arc::clone(&srv);
    let h = std::thread::spawn(move || {
        let _ = s2.serve(ln);
    });
    use std::io::{Read, Write};
    let mut s = std::os::unix::net::UnixStream::connect(&sock).unwrap();
    let _ = s.set_read_timeout(Some(Duration::from_millis(200)));
    s.write_all(&encode_json_frame(
        Op::Hello,
        &HelloBody {
            proto_version: 1,
            frontend: FrontendInfo { kind: "cli".into(), name: "t".into(), version: "0".into() },
        },
    ))
    .unwrap();
    std::thread::sleep(Duration::from_millis(200));
    let mut burst = Vec::new();
    for i in 0..40 {
        let req = RequestBody {
            corr: i + 1,
            op: "stream.open".into(),
            args: Some(json!({"kind": "term", "host": "aa"})),
        };
        burst.extend_from_slice(&encode_json_frame(Op::Req, &req));
    }
    s.write_all(&burst).unwrap();
    let mut buf = [0u8; 2048];
    let mut got = Vec::new();
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        if std::time::Instant::now() > deadline {
            break;
        }
        match s.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => got.extend_from_slice(&buf[..n]),
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut =>
            {
                std::thread::sleep(Duration::from_millis(10))
            }
            Err(_) => break,
        }
        if got.windows(9).any(|w| w == b"\"overrun\"") {
            break;
        }
    }
    assert!(
        got.windows(9).any(|w| w == b"\"overrun\""),
        "慢拨号应计入在途并触发 overrun，实收 {} 字节",
        got.len()
    );
    srv.shutdown();
    let _ = h.join();
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------- 握手协议次序（版本不匹配 → reload） ----------

#[test]
fn handshake_version_mismatch_reloads() {
    let (srv, sock, _bus, serve_h) = start_server("reload", MockBackend::new());
    use std::io::{Read, Write};
    let mut s = std::os::unix::net::UnixStream::connect(&sock).unwrap();
    let _ = s.set_read_timeout(Some(Duration::from_millis(200)));
    let hello = encode_json_frame(
        Op::Hello,
        &HelloBody {
            proto_version: 99,
            frontend: FrontendInfo { kind: "cli".into(), name: "t".into(), version: "0".into() },
        },
    );
    s.write_all(&hello).unwrap();
    let mut buf = [0u8; 256];
    let mut got = Vec::new();
    let deadline = std::time::Instant::now() + short();
    while got.len() < 32 && std::time::Instant::now() < deadline {
        match s.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => got.extend_from_slice(&buf[..n]),
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut =>
            {
                std::thread::sleep(Duration::from_millis(5))
            }
            Err(_) => break,
        }
    }
    assert_eq!(got[0], Op::Reload.code(), "版本不匹配应回 reload（proto_mismatch）");
    let blen = u32::from_be_bytes([got[1], got[2], got[3], got[4]]) as usize;
    let body: ReloadBody = serde_json::from_slice(&got[5..5 + blen]).unwrap();
    assert_eq!(body.reason, "proto_mismatch");
    srv.shutdown();
    let _ = serve_h.join();
    let _ = std::fs::remove_dir_all(sock.parent().unwrap());
}

/// 低-11（D-1 评审登记 → D-2 收口）：承载面 9 op 的 **server 级用例**——真实 UDS
/// 控制面 client 发请求，MockBackend 内存版承接。钉三件事：① op 名/载荷字段进
/// backend 方法；② 成功载荷形状（wire 面）；③ 错误码映射（no_host / bad_request /
/// 端口冲突 detail）。语义面（真监听/RST/状态机）归 carriers 单测 + CA 实采。
#[test]
fn carrier_ops_server_roundtrip() {
    let backend = MockBackend::new();
    let (srv, sock, _bus, serve_h) = start_server("carops", Arc::clone(&backend) as Arc<dyn Backend>);
    let (c, _w) = ControlClient::dial(&sock, "cli", "test").unwrap();
    let v = c.request("host.add", Some(json!({"token": "aaaa"})), short()).unwrap();
    let id = v["id"].as_str().unwrap().to_owned();

    // ---- forward.add：成功（listening 态）/ 端口冲突 / 缺字段 / no_host ----
    let v = c
        .request("forward.add", Some(json!({"host": id, "listen": 18081, "targetIp": "10.1.2.3", "targetPort": 80})), short())
        .unwrap();
    assert_eq!(v["rule"]["state"], "listening");
    assert_eq!(v["rule"]["listen"], 18081);
    assert_eq!(v["rule"]["targetIp"], "10.1.2.3");
    let e = c
        .request("forward.add", Some(json!({"host": id, "listen": 18081, "targetPort": 80})), short())
        .unwrap_err();
    assert_eq!(e.code, "bad_request", "端口冲突经 Other 映射 bad_request + detail（实收 {} {}）", e.code, e.detail());
    let e = c
        .request("forward.add", Some(json!({"host": id, "targetPort": 80})), short())
        .unwrap_err();
    assert_eq!(e.code, "bad_request", "缺 listen 字段");
    let e = c
        .request("forward.add", Some(json!({"host": "f".repeat(64), "listen": 18082, "targetPort": 80})), short())
        .unwrap_err();
    assert_eq!(e.code, "no_host", "host 不在表");

    // ---- forward.list：全部（host 空）与按 host 过滤 ----
    c.request("forward.add", Some(json!({"host": id, "listen": 18082, "targetPort": 443})), short()).unwrap();
    let v = c.request("forward.list", Some(json!({"host": ""})), short()).unwrap();
    assert_eq!(v["forwards"].as_array().unwrap().len(), 2);
    let v = c.request("forward.list", Some(json!({"host": "f".repeat(64)})), short()).unwrap();
    assert_eq!(v["forwards"].as_array().unwrap().len(), 0, "不在表 host = 空形状");

    // ---- forward.remove：成功幂等差（不存在 → bad_request detail）----
    let v = c
        .request("forward.remove", Some(json!({"host": id, "listen": 18082})), short())
        .unwrap();
    assert_eq!(v["removed"], true);
    let e = c
        .request("forward.remove", Some(json!({"host": id, "listen": 18082})), short())
        .unwrap_err();
    assert_eq!(e.code, "bad_request", "重复 remove 报错（内存版语义）");

    // ---- socks.on / off / status：记忆端口 + 幂等 off + 状态面 ----
    let v = c.request("socks.on", Some(json!({"host": id, "listen": 18099})), short()).unwrap();
    assert_eq!(v["listen"], 18099);
    let e = c.request("socks.on", Some(json!({"host": "f".repeat(64)})), short()).unwrap_err();
    assert_eq!(e.code, "no_host");
    let v = c.request("socks.off", Some(json!({"host": id})), short()).unwrap();
    assert_eq!(v["listen"], 18099, "off 返回记忆端口");
    let v = c.request("socks.status", None, short()).unwrap();
    assert_eq!(v["socks"][0]["host"].as_str().unwrap(), id);
    assert_eq!(v["socks"][0]["on"], false);
    assert_eq!(v["socks"][0]["listen"], 18099, "off 后端口记忆保留");
    // 再 on（listen 0 = 用记忆端口）。
    let v = c.request("socks.on", Some(json!({"host": id, "listen": 0})), short()).unwrap();
    assert_eq!(v["listen"], 18099, "listen 0 = 记忆/缺省端口");

    // ---- speedtest.start / status / cancel：waiting → down 相位 → cancel ----
    let v = c
        .request("speedtest.start", Some(json!({"host": id, "downMs": 100, "upMs": 0, "warmupMs": 0, "streams": 2, "waitMs": 0})), short())
        .unwrap();
    assert_eq!(v["phase"], "waiting");
    // 单飞：再 start → busy。
    let v = c
        .request("speedtest.start", Some(json!({"host": id, "downMs": 100})), short())
        .unwrap();
    assert_eq!(v["phase"], "busy");
    assert_eq!(v["reason"], "上一轮还在跑");
    let v = c.request("speedtest.status", Some(json!({"host": id})), short()).unwrap();
    assert_eq!(v["phase"], "down", "引擎快照相位透出");
    assert_eq!(v["bytes"], 42);
    let e = c.request("speedtest.status", Some(json!({"host": "f".repeat(64)})), short()).unwrap_err();
    assert_eq!(e.code, "no_host");
    let v = c.request("speedtest.cancel", Some(json!({"host": id})), short()).unwrap();
    assert_eq!(v["cancelled"], true);
    // cancel 后 status = 运行面丢失（None → idle 形态——「运行面丢失」判据 =
    // idle + 非 waiting + 无终态，Go 同义）。
    let v = c.request("speedtest.status", Some(json!({"host": id})), short()).unwrap();
    assert_eq!(v["phase"], "idle", "无运行面 = idle 形态（实收 {v}）");
    assert!(v["waiting"].is_null() || v["waiting"] == false, "waiting=false 省略（omitempty）");

    // ---- not_ready 门：承载面同样被挡 ----
    backend.not_ready.store(true, Ordering::SeqCst);
    let e = c.request("forward.list", Some(json!({"host": ""})), short()).unwrap_err();
    assert_eq!(e.code, "not_ready");
    backend.not_ready.store(false, Ordering::SeqCst);
    c.request("forward.list", Some(json!({"host": ""})), short()).unwrap();

    let _ = c.goodbye();
    srv.shutdown();
    let _ = serve_h.join();
}
