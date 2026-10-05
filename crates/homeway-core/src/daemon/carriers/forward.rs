//! 桌面端口转发规则管理器（语义真源 `baseline:clientcore/facade/forward.go`）。
//!
//! daemon 侧承载面：规则持久化于 `<client>/forwards.json`（0600、原子读改写、损坏按
//! 空表重建 + 告警）；监听器在本进程跑（127.0.0.1、仅回环、会话无关长活——规则 add
//! 即起、delete/重启重建随表）；入站连接经拨号缝到目标（target 空 = 出口自己同端口、
//! target IP = 任意目标缝）。**与手机 portfwd 面（App UI + ClientCoreTunSetPortForwards）
//! 零耦合**：两套规则表、两套监听宿主互不相干。
//!
//! 在世连接语义：delete/级联 = **不强关**已建立连接（关监听只拒新连接，pipe 自然
//! 收口——手机面同款实质）；上游拨号失败 = 本地 RST 收口（优雅 FIN 会让客户端静默
//! 挂住）。

use std::path::Path;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Arc, Mutex};

use super::{
    describe_listen_err, pipe_half_close, rst_close_tcp, save_json_atomic, short_host,
    CarrierDial, CarrierErr, PollAccept, PollListener,
};

/// 规则持久化文件名（0600；`<client>/forwards.json`）。
pub const FORWARDS_FILE: &str = "forwards.json";

/// 每主机规则上限（Go forwardMaxPerHost）。
pub const MAX_PER_HOST: usize = 8;
/// 每监听并发连接上限（与 socks MaxConns 同值 256——任一本机进程打满 fd 的防御面）。
pub const MAX_CONNS: i32 = 256;

/// 一条转发规则（持久化形态）。target_ip 空 = 出口自己；target_port 0 = 同监听端口。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ForwardRule {
    /// peerID hex。
    pub host: String,
    pub listen: u16,
    #[serde(rename = "targetIp", default, skip_serializing_if = "String::is_empty")]
    pub target_ip: String,
    #[serde(rename = "targetPort", default, skip_serializing_if = "is_zero_u16")]
    pub target_port: u16,
}

fn is_zero_u16(v: &u16) -> bool {
    *v == 0
}

/// 规则运行态（forward.list 面）。
#[derive(Debug, Clone)]
pub struct ForwardState {
    pub rule: ForwardRule,
    /// listening | failed。
    pub state: String,
    pub err: String,
    pub conns: i32,
    /// 超并发上限被拒的连接计数。
    pub rejected: i32,
}

/// 目标呈现文案（语义唯一源 `pkg/portfwd.DescribeTarget` 的桌面口径；port 0 = 同
/// 监听端口 ⇒ 落成 listen——FIX-46：新增呈现面一律走这里）。
pub fn describe_target(ip: &str, port: u16, listen: u16) -> String {
    if ip.is_empty() {
        if port == 0 {
            return "出口自己（同端口）".to_owned();
        }
        return format!("出口自己:{port}");
    }
    let port = if port == 0 { listen } else { port };
    format!("{ip}:{port}")
}

fn describe_rule_target(r: &ForwardRule) -> String {
    if r.target_ip.is_empty() {
        if r.target_port == 0 {
            return "出口自己（同端口）".to_owned();
        }
        return format!("出口自己:{}", r.target_port);
    }
    format!("{}:{}", r.target_ip, r.target_port)
}

/// 管理器级装配件（accept/serve 线程与 Manager 解耦的携带面）。
struct Wire {
    dial: CarrierDial,
    logf: Arc<dyn Fn(&str) + Send + Sync>,
    warnf: Arc<dyn Fn(&str) + Send + Sync>,
}

struct FwdEntry {
    rule: ForwardRule,
    wire: Arc<Wire>,
    /// 运行态（listening/failed + 原因；重启重建的软失败面）。
    live: Mutex<(String, String)>,
    /// accept 轮询体（None = 监听已收口/从未建立；取走 = accept 线程退出）。
    listener: Mutex<Option<PollListener>>,
    conns: AtomicI32,
    rejected: AtomicI32,
}

/// forward 规则管理器（全局端口唯一性检查在 Carriers 层——本管理器只管自己名下
/// 的端口）。
pub struct ForwardManager {
    state_dir: Box<Path>,
    wire: Arc<Wire>,
    rules: Mutex<Vec<Arc<FwdEntry>>>,
}

impl ForwardManager {
    /// 打开规则表：读 forwards.json（缺失 = 空表；损坏 = 备份后空表 + 告警不拒启）、
    /// 按表重建监听器（端口被占 = 该条 failed 如实呈现，不阻断其余——手机
    /// 「软失败不回滚」哲学）。
    pub fn open(
        state_dir: &Path,
        dial: &CarrierDial,
        logf: &Arc<dyn Fn(&str) + Send + Sync>,
        warnf: &Arc<dyn Fn(&str) + Send + Sync>,
    ) -> Result<ForwardManager, String> {
        let wire = Arc::new(Wire {
            dial: CarrierDial {
                dial_port: Arc::clone(&dial.dial_port),
                dial: Arc::clone(&dial.dial),
            },
            logf: Arc::clone(logf),
            warnf: Arc::clone(warnf),
        });
        let recs = load_rules(&state_dir.join(FORWARDS_FILE), warnf)?;
        let mut prepared: Vec<Arc<FwdEntry>> = Vec::with_capacity(recs.len());
        for r in recs {
            let e = Arc::new(FwdEntry {
                rule: r.clone(),
                wire: Arc::clone(&wire),
                live: Mutex::new(("listening".to_owned(), String::new())),
                listener: Mutex::new(None),
                conns: AtomicI32::new(0),
                rejected: AtomicI32::new(0),
            });
            match start_listener(&e) {
                Ok(()) => {}
                Err(err) => {
                    *e.live.lock().unwrap_or_else(|er| er.into_inner()) =
                        ("failed".to_owned(), err.clone());
                    (wire.warnf)(&format!(
                        "forward: 规则 {}:{} 重建监听失败（{err}）——按 failed 呈现，其余规则不受影响",
                        short_host(&r.host),
                        r.listen
                    ));
                }
            }
            prepared.push(e);
        }
        Ok(ForwardManager {
            state_dir: state_dir.to_owned().into_boxed_path(),
            wire,
            rules: Mutex::new(prepared),
        })
    }

    /// 建规则并起监听：校验（值域/每主机上限/本管理器内端口唯一）→ **当场监听**
    /// （失败 = 错误返回、不入表——「failed 软状态」只留重启重建路径）→ 落盘 →
    /// 入表。调用方（Carriers）已做跨 socks 的全局端口检查与成员检查。
    pub fn add(&self, rule: ForwardRule) -> Result<(), CarrierErr> {
        validate_rule(&rule)?;
        let mut rules = self.rules.lock().unwrap_or_else(|e| e.into_inner());
        let mut per_host = 0usize;
        for e in rules.iter() {
            if e.rule.host == rule.host {
                per_host += 1;
            }
            if e.rule.listen == rule.listen {
                return Err(CarrierErr::PortTaken {
                    port: rule.listen,
                    owner: format!("{} 的 forward 规则", short_host(&e.rule.host)),
                });
            }
        }
        if per_host >= MAX_PER_HOST {
            return Err(CarrierErr::TooManyRules(short_host(&rule.host), per_host));
        }
        let e = Arc::new(FwdEntry {
            rule: rule.clone(),
            wire: Arc::clone(&self.wire),
            live: Mutex::new((String::new(), String::new())),
            listener: Mutex::new(None),
            conns: AtomicI32::new(0),
            rejected: AtomicI32::new(0),
        });
        start_listener(&e).map_err(|err| CarrierErr::ListenFailed(rule.listen, err))?;
        // 先入表再落盘（落盘内容 = 内存表快照）；落盘失败撤回——零副作用。
        rules.push(Arc::clone(&e));
        if let Err(err) = self.save_locked(&rules) {
            if let Some(last) = rules.pop() {
                if let Some(mut l) = last.listener.lock().unwrap_or_else(|er| er.into_inner()).take() {
                    l.close();
                }
            }
            return Err(CarrierErr::Save(err));
        }
        (self.wire.logf)(&format!(
            "forward: + {} 127.0.0.1:{} → {}",
            short_host(&rule.host),
            rule.listen,
            describe_rule_target(&rule)
        ));
        Ok(())
    }

    /// 删规则并关监听（**不强关**在世连接——自然收口；conns 可观察此过程）。
    pub fn remove(&self, host: &str, listen: u16) -> Result<(), CarrierErr> {
        let mut rules = self.rules.lock().unwrap_or_else(|e| e.into_inner());
        let idx = rules
            .iter()
            .position(|e| e.rule.host == host && e.rule.listen == listen)
            .ok_or_else(|| CarrierErr::NoRule(format!("{}/{}", short_host(host), listen)))?;
        let e = rules.remove(idx);
        if let Some(mut l) = e.listener.lock().unwrap_or_else(|er| er.into_inner()).take() {
            l.close(); // 只关监听：已 accept 的连接由 pipe 自然收口
        }
        self.save_locked(&rules).map_err(CarrierErr::Save)?;
        (self.wire.logf)(&format!(
            "forward: - {} 127.0.0.1:{listen}（在世连接不强关）",
            short_host(host)
        ));
        Ok(())
    }

    /// 级联删该主机全部规则（host.remove；同 delete 语义——不强关在世连接）。
    /// 调用方（Carriers）已持 add_mu。
    pub fn remove_host(&self, host: &str) {
        let mut rules = self.rules.lock().unwrap_or_else(|e| e.into_inner());
        let mut kept = Vec::with_capacity(rules.len());
        for e in rules.drain(..) {
            if e.rule.host == host {
                if let Some(mut l) = e.listener.lock().unwrap_or_else(|er| er.into_inner()).take() {
                    l.close();
                }
                (self.wire.logf)(&format!(
                    "forward: - {} 127.0.0.1:{}（主机删除级联；在世连接不强关）",
                    short_host(host),
                    e.rule.listen
                ));
            } else {
                kept.push(e);
            }
        }
        *rules = kept;
        if let Err(err) = self.save_locked(&rules) {
            (self.wire.warnf)(&format!(
                "forward: 级联删除后落盘失败（{err}）——内存为准，下次写回收敛"
            ));
        }
    }

    /// 规则表快照（host 空 = 全部；按 host/listen 稳定排序）。
    pub fn list(&self, host: &str) -> Vec<ForwardState> {
        let rules = self.rules.lock().unwrap_or_else(|e| e.into_inner());
        let mut out: Vec<ForwardState> = rules
            .iter()
            .filter(|e| host.is_empty() || e.rule.host == host)
            .map(|e| {
                let (state, err) = e.live.lock().unwrap_or_else(|er| er.into_inner()).clone();
                ForwardState {
                    rule: e.rule.clone(),
                    state,
                    err,
                    conns: e.conns.load(Ordering::Relaxed),
                    rejected: e.rejected.load(Ordering::Relaxed),
                }
            })
            .collect();
        out.sort_by(|a, b| a.rule.host.cmp(&b.rule.host).then(a.rule.listen.cmp(&b.rule.listen)));
        out
    }

    /// 端口占用查询（全局唯一检查用）：占用返回占用方描述。
    pub fn port_owner(&self, port: u16) -> Option<String> {
        let rules = self.rules.lock().unwrap_or_else(|e| e.into_inner());
        rules
            .iter()
            .find(|e| e.rule.listen == port)
            .map(|e| format!("{} 的 forward 规则", short_host(&e.rule.host)))
    }

    /// 收工：关全部监听（在世连接不强关；随进程/角色收口）。
    pub fn close(&self) {
        let rules = self.rules.lock().unwrap_or_else(|e| e.into_inner());
        for e in rules.iter() {
            if let Some(mut l) = e.listener.lock().unwrap_or_else(|er| er.into_inner()).take() {
                l.close();
            }
        }
    }
}

impl Drop for ForwardManager {
    fn drop(&mut self) {
        self.close(); // RAII：监听随管理器生命周期收口（在世连接仍不强关）
    }
}

impl ForwardManager {

    fn save_locked(&self, rules: &[Arc<FwdEntry>]) -> Result<(), String> {
        let recs: Vec<ForwardRule> = rules.iter().map(|e| e.rule.clone()).collect();
        let body = serde_json::to_string_pretty(&recs).map_err(|e| e.to_string())?;
        save_json_atomic(&self.state_dir.join(FORWARDS_FILE), body.as_bytes())
    }
}

/// 校验（Go Add 前置：监听值域 / 目标形态；目标端口不限下界——出口去拨不 bind）。
fn validate_rule(rule: &ForwardRule) -> Result<(), CarrierErr> {
    if !(crate::facade::portfwd::MIN_PORT..=u16::MAX).contains(&rule.listen) {
        return Err(CarrierErr::PortRange(rule.listen));
    }
    if !rule.target_ip.is_empty() && rule.target_ip.parse::<std::net::Ipv4Addr>().is_err() {
        return Err(CarrierErr::BadTarget(rule.target_ip.clone()));
    }
    Ok(())
}

/// 起 127.0.0.1 回环监听 + accept 轮询线程。
fn start_listener(e: &Arc<FwdEntry>) -> Result<(), String> {
    let ln = std::net::TcpListener::bind(("127.0.0.1", e.rule.listen))
        .map_err(|err| describe_listen_err(e.rule.listen, &err))?;
    let port = ln.local_addr().map(|a| a.port()).unwrap_or(e.rule.listen);
    *e.live.lock().unwrap_or_else(|er| er.into_inner()) = ("listening".to_owned(), String::new());
    *e.listener.lock().unwrap_or_else(|er| er.into_inner()) = Some(PollListener::new(ln));
    let entry = Arc::clone(e);
    std::thread::Builder::new()
        .name("hw-fwd-accept".to_owned())
        .stack_size(256 * 1024)
        .spawn(move || accept_loop(entry, port))
        .map_err(|err| err.to_string())?;
    Ok(())
}

/// accept 轮询（50ms 节拍；监听收口 = listener 被取走 → Closed 退出）。
fn accept_loop(e: Arc<FwdEntry>, port_for_log: u16) {
    loop {
        let step = {
            let mut guard = e.listener.lock().unwrap_or_else(|er| er.into_inner());
            match guard.as_mut() {
                Some(pl) => pl.accept(),
                None => return,
            }
        };
        match step {
            PollAccept::Conn(conn) => {
                // 每监听并发上限（256）：超限拒绝并计数（单线程串行判-增，不会超上限）。
                if e.conns.load(Ordering::Relaxed) >= MAX_CONNS {
                    e.rejected.fetch_add(1, Ordering::Relaxed);
                    (e.wire.logf)(&format!(
                        "forward: {}:{} 连接拒绝（并发上限 {MAX_CONNS}）",
                        short_host(&e.rule.host),
                        port_for_log
                    ));
                    rst_close_tcp(&conn);
                    drop(conn);
                    continue;
                }
                e.conns.fetch_add(1, Ordering::Relaxed);
                let entry = Arc::clone(&e);
                std::thread::Builder::new()
                    .name("hw-fwd-conn".to_owned())
                    .stack_size(512 * 1024)
                    .spawn(move || serve_conn(entry, conn))
                    .expect("线程创建不可失败");
            }
            PollAccept::Idle => {}
            PollAccept::Closed => return, // delete/级联/收工——正常收口
            PollAccept::Failed(err) => {
                // 瞬态错误烧尽：置 failed + 原因（与重启重建的软失败同款）——防
                // 「状态说在听、实际没人 accept」。
                let mut guard = e.listener.lock().unwrap_or_else(|er| er.into_inner());
                if let Some(mut l) = guard.take() {
                    l.close();
                    *e.live.lock().unwrap_or_else(|er| er.into_inner()) =
                        ("failed".to_owned(), err.clone());
                    drop(guard);
                    (e.wire.warnf)(&format!(
                        "forward: {}:{} {err}",
                        short_host(&e.rule.host),
                        e.rule.listen
                    ));
                }
                return;
            }
        }
    }
}

/// 单条转发：拨号 → 双向透传。上游失败 = RST 收口（MUST NOT 优雅 FIN 静默挂住
/// 客户端）；半关闭透传（FIX-35：任一向 EOF 只收该向写端）。
fn serve_conn(e: Arc<FwdEntry>, conn: std::net::TcpStream) {
    let _dec = ConnGuard(&e);
    let r = &e.rule;
    let port = if r.target_port == 0 { r.listen } else { r.target_port };
    let upstream = if r.target_ip.is_empty() {
        (e.wire.dial.dial_port)(&r.host, port).map(|c| c.io)
    } else {
        let ip: std::net::Ipv4Addr = r.target_ip.parse().expect("校验已挡非 v4 字面量");
        (e.wire.dial.dial)(&r.host, std::net::SocketAddrV4::new(ip, port)).map(|c| c.io)
    };
    let upstream = match upstream {
        Ok(u) => u,
        Err(err) => {
            (e.wire.logf)(&format!(
                "forward: {}:{} 拨上游失败（RST 收口）：{err}",
                short_host(&r.host),
                r.listen
            ));
            rst_close_tcp(&conn);
            return;
        }
    };
    pipe_half_close(&e.wire.logf, conn, upstream);
}

/// 在世连接计数守卫（serve_conn 全出口恰一次递减）。
struct ConnGuard<'a>(&'a FwdEntry);

impl Drop for ConnGuard<'_> {
    fn drop(&mut self) {
        self.0.conns.fetch_sub(1, Ordering::Relaxed);
    }
}

fn load_rules(
    path: &Path,
    warnf: &Arc<dyn Fn(&str) + Send + Sync>,
) -> Result<Vec<ForwardRule>, String> {
    match std::fs::read(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(format!("读 {}: {e}", path.display())),
        Ok(b) => match serde_json::from_slice::<Vec<ForwardRule>>(&b) {
            Ok(recs) => Ok(recs),
            Err(e) => {
                let backup = path.with_file_name(format!(
                    "{}.corrupt-{}",
                    FORWARDS_FILE,
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs())
                        .unwrap_or(0)
                ));
                match std::fs::rename(path, &backup) {
                    Ok(()) => {
                        (warnf)(&format!(
                            "forwards.json 损坏（{e}）——已备份 {}，按空表重建（原件保留，可修复后重启恢复）",
                            backup.display()
                        ));
                        Ok(Vec::new())
                    }
                    Err(rerr) => Err(format!("forwards.json 损坏（{e}）且备份失败：{rerr}")),
                }
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::super::testutil::{echo_listener, eventually, temp_dir, FakeDial};
    use super::*;
    use std::time::Duration;

    fn nop_log() -> (Arc<dyn Fn(&str) + Send + Sync>, Arc<dyn Fn(&str) + Send + Sync>) {
        (Arc::new(|_| {}), Arc::new(|_| {}))
    }

    fn hex_host(tag: u8) -> String {
        format!("{tag:0>64x}")
    }

    /// 建规则 → 回环往返（echo 目标）→ 删规则（在世连接不强关）→ 持久化重建。
    #[test]
    fn add_roundtrip_remove_and_rebuild() {
        let dir = temp_dir("fwd");
        let dial = FakeDial::new();
        let (echo_addr, _echo_h) = echo_listener();
        dial.port_map.lock().unwrap().insert(19990, echo_addr);
        let (logf, warnf) = nop_log();
        let m = ForwardManager::open(&dir, &dial.carrier_dial(), &logf, &warnf).unwrap();

        // add：值域/唯一/上限的负例。
        assert!(matches!(
            m.add(ForwardRule { host: hex_host(1), listen: 80, target_ip: String::new(), target_port: 0 }),
            Err(CarrierErr::PortRange(80))
        ));
        assert!(matches!(
            m.add(ForwardRule { host: hex_host(1), listen: 19990, target_ip: "example.com".into(), target_port: 0 }),
            Err(CarrierErr::BadTarget(_))
        ));
        m.add(ForwardRule { host: hex_host(1), listen: 19990, target_ip: String::new(), target_port: 0 })
            .unwrap();
        assert!(matches!(
            m.add(ForwardRule { host: hex_host(2), listen: 19990, target_ip: String::new(), target_port: 0 }),
            Err(CarrierErr::PortTaken { port: 19990, .. })
        ));

        // 数据面：拨 127.0.0.1:19990 → 经隧道 echo → 回读同载荷。
        let mut c = std::net::TcpStream::connect("127.0.0.1:19990").unwrap();
        use std::io::{Read, Write};
        c.write_all(b"ping-forward").unwrap();
        let mut buf = [0u8; 64];
        let mut got = Vec::new();
        eventually(Duration::from_secs(5), "echo 回读", || {
            match c.read(&mut buf) {
                Ok(0) => false,
                Ok(n) => {
                    got.extend_from_slice(&buf[..n]);
                    got == b"ping-forward"
                }
                Err(_) => false,
            }
        });
        assert_eq!(got, b"ping-forward");
        // 拨号缝确实经了 host:port 形态。
        assert!(dial.calls.lock().unwrap().contains(&format!("{}:19990", hex_host(1))));

        // list：listening + conns 快照面。
        let states = m.list("");
        assert_eq!(states.len(), 1);
        assert_eq!(states[0].state, "listening");
        assert_eq!(states[0].rule.listen, 19990);
        assert_eq!(states[0].rule.host, hex_host(1));

        // remove 后再删 = NoRule。
        m.remove(&hex_host(1), 19990).unwrap();
        assert!(matches!(m.remove(&hex_host(1), 19990), Err(CarrierErr::NoRule(_))));

        // 持久化重建：文件里已是空表（删后落盘）；重开拿到空表。
        drop(m);
        let m2 = ForwardManager::open(&dir, &dial.carrier_dial(), &logf, &warnf).unwrap();
        assert!(m2.list("").is_empty());

        // 重建路径：写一条规则 → 重开 → 监听重建（19991 未映射 = dial 失败仍 listening——
        // 监听与拨号缝无关）。
        m2.add(ForwardRule { host: hex_host(3), listen: 19991, target_ip: "127.0.0.9".into(), target_port: 8080 })
            .unwrap();
        drop(m2);
        let m3 = ForwardManager::open(&dir, &dial.carrier_dial(), &logf, &warnf).unwrap();
        let states = m3.list("");
        assert_eq!(states.len(), 1, "重开按表重建");
        assert_eq!(states[0].state, "listening", "监听重建与拨号缝无关");
        m3.close();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 端口被外部进程占用：add 拒绝不入表；重建路径按 failed 呈现不拒启。
    #[test]
    fn occupied_port_add_rejected_and_rebuild_failed() {
        let dir = temp_dir("fwd-occ");
        // 外占 listener（一直持有）。
        let held = std::net::TcpListener::bind("127.0.0.1:19992").unwrap();
        let dial = FakeDial::new();
        let (logf, warnf) = nop_log();
        let m = ForwardManager::open(&dir, &dial.carrier_dial(), &logf, &warnf).unwrap();
        // add 当场监听失败 = 错误返回、不入表。
        assert!(matches!(
            m.add(ForwardRule { host: hex_host(1), listen: 19992, target_ip: String::new(), target_port: 0 }),
            Err(CarrierErr::ListenFailed(19992, _))
        ));
        assert!(m.list("").is_empty());
        // 把外占端口写进持久化 → 重开 = failed 软状态。
        let recs = vec![ForwardRule { host: hex_host(1), listen: 19992, target_ip: String::new(), target_port: 0 }];
        std::fs::write(
            dir.join(FORWARDS_FILE),
            serde_json::to_string_pretty(&recs).unwrap(),
        )
        .unwrap();
        drop(m);
        let m2 = ForwardManager::open(&dir, &dial.carrier_dial(), &logf, &warnf).unwrap();
        let states = m2.list("");
        assert_eq!(states[0].state, "failed");
        assert!(!states[0].err.is_empty());
        drop(held);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 每主机 8 条上限 + 级联删除 + 损坏文件按空表重建。
    #[test]
    fn per_host_cap_cascade_and_corrupt_file() {
        let dir = temp_dir("fwd-cap");
        let dial = FakeDial::new();
        let (logf, warnf) = nop_log();
        let m = ForwardManager::open(&dir, &dial.carrier_dial(), &logf, &warnf).unwrap();
        for p in 20001..=20008u16 {
            m.add(ForwardRule { host: hex_host(1), listen: p, target_ip: String::new(), target_port: 0 })
                .unwrap();
        }
        assert!(matches!(
            m.add(ForwardRule { host: hex_host(1), listen: 20009, target_ip: String::new(), target_port: 0 }),
            Err(CarrierErr::TooManyRules(_, 8))
        ));
        // 级联：另一主机不受影响。
        m.add(ForwardRule { host: hex_host(2), listen: 20010, target_ip: String::new(), target_port: 0 })
            .unwrap();
        m.remove_host(&hex_host(1));
        let states = m.list("");
        assert_eq!(states.len(), 1);
        assert_eq!(states[0].rule.host, hex_host(2));
        drop(m);
        // 损坏文件：备份 + 空表。
        std::fs::write(dir.join(FORWARDS_FILE), b"{not json").unwrap();
        let m2 = ForwardManager::open(&dir, &dial.carrier_dial(), &logf, &warnf).unwrap();
        assert!(m2.list("").is_empty());
        assert!(std::fs::read_dir(&dir).unwrap().any(|e| {
            e.unwrap().file_name().to_string_lossy().starts_with("forwards.json.corrupt-")
        }));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
