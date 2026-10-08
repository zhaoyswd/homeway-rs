//! 按主机 SOCKS5 承载面管理器（语义真源 `baseline:clientcore/facade/socksmgr.go`）。
//!
//! 每主机至多一个 listener（多主机 = 多端口，浏览器按端口选出口）；`{on,listen}` 记忆
//! 持久化于 `<client>/socks.json`（0600、原子读改写、损坏按空表重建 + 告警）。off 不抹
//! 端口记忆（下次 on 缺省沿用）。域名远程解析 = DNS-over-TCP→5300（拨号缝拨出口
//! **隧道栈内**的解析腿 listener），**MUST NOT 本地解析**（结构保证：本文件无任何
//! 系统解析调用）。缓存**每 listener 一份**（缓存 key =（出口主机, 域名）——每主机一
//! listener 的实现形态即天然隔离；有界 256、TTL 取应答钳制值、否定不缓存、逐出即弃）。
//! 在世连接：off/级联 = 显式关（RST——「off」之后不得仍有代理流量经隧道跑）。

use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::dnsq::{self, SOCKS_DNS_PORT};
use super::socks_srv::{Resolver, SocksServer, SocksServerConfig};
use super::{describe_listen_err, save_json_atomic, short_host, CarrierDial, CarrierErr};

/// 开关记忆持久化文件名（0600；`<client>/socks.json`）。
pub const SOCKS_FILE: &str = "socks.json";
/// on 缺省监听端口（生产；测试注入 0 = 内核选空闲端口——与「在役 daemon 持 1080」
/// 解耦的 exec-r1 B4 面）。
pub const SOCKS_DEFAULT_LISTEN: u16 = 1080;
/// 缓存边界（有界 256、FIFO 逐出）。
const CACHE_MAX: usize = 256;
/// 单次解析总预算（拨 5300 + 查询；超时归因、不缓存）。
const RESOLVE_BUDGET: Duration = Duration::from_secs(5);

/// 每主机开关记忆（持久化形态）。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SocksEntry {
    /// peerID hex。
    pub host: String,
    pub on: bool,
    /// 记忆端口（off 也保留；0 = 内核选空闲端口的注入面）。
    pub listen: u16,
}

/// status 面单条（链路态 via/rtt 由 CLI join——本面只管承载态）。
#[derive(Debug, Clone)]
pub struct SocksState {
    pub host: String,
    pub on: bool,
    /// off 但记住的端口（status --json 暴露）。
    pub listen: u16,
    pub conns: i32,
    /// 重建/开启失败时的如实呈现。
    pub err: String,
}

struct EntryRt {
    rec: SocksEntry,
    /// 在役 SOCKS 服务端（None = off）。
    srv: Option<Arc<SocksServer>>,
    err: String,
}

/// socks 承载面管理器（全局端口唯一性检查在 Carriers 层）。
pub struct SocksManager {
    state_dir: Box<std::path::Path>,
    dial: CarrierDial,
    entries: Mutex<Vec<EntryRt>>,
    logf: Arc<dyn Fn(&str) + Send + Sync>,
    warnf: Arc<dyn Fn(&str) + Send + Sync>,
}

impl SocksManager {
    /// 打开开关记忆：读 socks.json、按 on 条目重建监听（失败 = off 呈现 + 告警，
    /// 不拒启）。
    pub fn open(
        state_dir: &std::path::Path,
        dial: &CarrierDial,
        logf: &Arc<dyn Fn(&str) + Send + Sync>,
        warnf: &Arc<dyn Fn(&str) + Send + Sync>,
    ) -> Result<SocksManager, String> {
        let dial = CarrierDial {
            dial_port: Arc::clone(&dial.dial_port),
            dial: Arc::clone(&dial.dial),
        };
        let recs = load_entries(&state_dir.join(SOCKS_FILE), warnf)?;
        let mut prepared: Vec<EntryRt> = Vec::with_capacity(recs.len());
        for rec in recs {
            let mut e = EntryRt { rec, srv: None, err: String::new() };
            if e.rec.on {
                if let Err(err) = start_listener(&dial, logf, warnf, &mut e) {
                    e.err = err.clone();
                    (warnf)(&format!(
                        "socks: {} 监听重建失败（{err}）——按 off 呈现，socks on 可重试",
                        short_host(&e.rec.host)
                    ));
                }
            }
            prepared.push(e);
        }
        Ok(SocksManager {
            state_dir: state_dir.to_owned().into_boxed_path(),
            dial,
            entries: Mutex::new(prepared),
            logf: Arc::clone(logf),
            warnf: Arc::clone(warnf),
        })
    }

    /// 开监听（listen 0 = 沿用记忆端口，无记忆则缺省 1080；解析后的端口由调用方
    /// 〔Carriers〕与 forward 侧互斥）。同主机重复 on：同端口 = 幂等成功；换端口 =
    /// 关旧开新。返回实际端口。
    pub fn on(&self, host: &str, listen: u16) -> Result<u16, CarrierErr> {
        let listen = if listen == 0 { self.default_listen(host) } else { listen };
        if listen != 0 && listen < crate::facade::portfwd::MIN_PORT {
            return Err(CarrierErr::PortRange(listen));
        }
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        // 跨主机端口冲突（含 off 但记住的——「全局唯一」按规则在册判，防 on 回来
        // 撞上别台已占/已记的端口）。
        for e in entries.iter() {
            if e.rec.host != host && listen != 0 && e.rec.listen == listen {
                return Err(CarrierErr::PortTaken {
                    port: listen,
                    owner: format!(
                        "{} 的 socks 监听（可用 --listen 另选）",
                        short_host(&e.rec.host)
                    ),
                });
            }
        }
        if let Some(i) = entries.iter().position(|e| e.rec.host == host) {
            let healthy = entries[i]
                .srv
                .as_ref()
                .is_some_and(|s| s.dead_reason().is_none());
            if healthy && listen != 0 && entries[i].rec.listen == listen {
                return Ok(listen); // 幂等：同端口已开（死监听不短路——走换新路径）
            }
            // 先算后写（FIX-38）：**先在新端口上把 listener 起起来**，成功了才换监听与
            // 记忆；失败时旧监听照常在役、记忆保持上一次成功的端口（与「off 不抹端口
            // 记忆」同口径）。
            let mut next = EntryRt {
                rec: SocksEntry { host: host.to_owned(), on: true, listen },
                srv: None,
                err: String::new(),
            };
            let bound = start_listener(&self.dial, &self.logf, &self.warnf, &mut next)
                .map_err(|err| CarrierErr::ListenFailed(listen, err))?;
            next.rec.listen = bound;
            let prev = std::mem::replace(&mut entries[i], next);
            if let Err(err) = self.save_locked(&entries) {
                // 落盘失败：换回旧监听与旧记忆（旧监听尚未停），收口新监听——零副作用。
                let mut fresh = std::mem::replace(&mut entries[i], prev);
                stop_listener(&mut fresh);
                return Err(CarrierErr::Save(err));
            }
            let mut old = prev;
            if old.srv.is_some() {
                stop_listener(&mut old); // 换端口：关旧（显式关在世连接）
            }
            (self.logf)(&format!("socks: {} on 127.0.0.1:{}", short_host(host), entries[i].rec.listen));
            return Ok(entries[i].rec.listen);
        }
        // 无条目：新主机。先入表再落盘；落盘失败撤回——零副作用。
        let mut e = EntryRt {
            rec: SocksEntry { host: host.to_owned(), on: true, listen },
            srv: None,
            err: String::new(),
        };
        let bound = start_listener(&self.dial, &self.logf, &self.warnf, &mut e)
            .map_err(|err| CarrierErr::ListenFailed(listen, err))?;
        e.rec.listen = bound;
        entries.push(e);
        if let Err(err) = self.save_locked(&entries) {
            let mut removed = entries.pop().expect("刚 push");
            stop_listener(&mut removed);
            return Err(CarrierErr::Save(err));
        }
        let port = entries.last().expect("刚 push").rec.listen;
        (self.logf)(&format!("socks: {} on 127.0.0.1:{port}", short_host(host)));
        Ok(port)
    }

    /// on 缺省端口的解析面（纯查询、不绑端口）：该主机的记忆端口，无记忆/记忆为 0
    /// = 缺省 1080。spec「on 的监听端口缺省 SHALL 沿用该主机上次使用的端口（无记忆
    /// 则 1080）」的唯一真源（exec-r1 B1）。
    pub fn default_listen(&self, host: &str) -> u16 {
        let entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        for e in entries.iter() {
            if e.rec.host == host && e.rec.listen != 0 {
                return e.rec.listen;
            }
        }
        SOCKS_DEFAULT_LISTEN
    }

    /// 关监听并**显式关在世连接**（RST 收口）；端口记忆保留（下次 on 缺省沿用）。
    /// 返回记忆端口。
    pub fn off(&self, host: &str) -> Result<u16, CarrierErr> {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        for e in entries.iter_mut() {
            if e.rec.host == host {
                let port = e.rec.listen;
                if e.srv.is_some() {
                    // 死监听（accept 烧尽）也随 off 收口（清 dead 位）。
                    e.err.clear();
                    stop_listener(e);
                    (self.logf)(&format!(
                        "socks: {} off（在世连接已 RST 收口，端口 {port} 记忆保留）",
                        short_host(host)
                    ));
                }
                self.save_locked(&entries).map_err(CarrierErr::Save)?;
                return Ok(port);
            }
        }
        Err(CarrierErr::NoRule(format!("socks {}", short_host(host))))
    }

    /// 级联（host.remove）：off 同款显式关 + 端口记忆随之消失。调用方已持 add_mu。
    pub fn remove_host(&self, host: &str) {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        let mut kept = Vec::with_capacity(entries.len());
        for mut e in entries.drain(..) {
            if e.rec.host == host {
                if e.srv.is_some() {
                    stop_listener(&mut e);
                }
                (self.logf)(&format!("socks: {} off（主机删除级联；端口记忆消失）", short_host(host)));
            } else {
                kept.push(e);
            }
        }
        *entries = kept;
        if let Err(err) = self.save_locked(&entries) {
            (self.warnf)(&format!("socks: 级联删除后落盘失败（{err}）——内存为准，下次写回收敛"));
        }
    }

    /// 各主机承载态（按 host 排序稳定输出）。
    pub fn status(&self) -> Vec<SocksState> {
        let entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        let mut out: Vec<SocksState> = entries
            .iter()
            .map(|e| {
                let dead = e.srv.as_ref().and_then(|s| s.dead_reason());
                SocksState {
                    host: e.rec.host.clone(),
                    on: e.srv.is_some() && dead.is_none(),
                    listen: e.rec.listen,
                    conns: e.srv.as_ref().map_or(0, |s| s.conns()),
                    // dead 原因优先（在役但失效 = 最新的可行动归因）。
                    err: dead.clone().unwrap_or_else(|| e.err.clone()),
                }
            })
            .collect();
        out.sort_by(|a, b| a.host.cmp(&b.host));
        out
    }

    /// 端口占用查询（含 off 但记住的端口——全局唯一按「规则在册」判，防 on 回来
    /// 撞上别的主机已占的端口）。
    pub fn port_owner(&self, port: u16) -> Option<String> {
        let entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        entries
            .iter()
            .find(|e| e.rec.listen == port)
            .map(|e| format!("{} 的 socks 监听", short_host(&e.rec.host)))
    }

    /// 收工：全部按 off 语义显式关（RST 在世连接）。
    pub fn close(&self) {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        for e in entries.iter_mut() {
            if e.srv.is_some() {
                stop_listener(e);
            }
        }
    }
}

impl Drop for SocksManager {
    fn drop(&mut self) {
        self.close(); // RAII：监听与在世代理连接随管理器生命周期收口
    }
}

impl SocksManager {

    fn save_locked(&self, entries: &[EntryRt]) -> Result<(), String> {
        let recs: Vec<SocksEntry> = entries.iter().map(|e| e.rec.clone()).collect();
        let body = serde_json::to_string_pretty(&recs).map_err(|e| e.to_string())?;
        save_json_atomic(&self.state_dir.join(SOCKS_FILE), body.as_bytes())
    }
}

/// 起 127.0.0.1 监听 + SOCKS 服务端（resolver/dialer 绑定该主机；每 listener 一份
/// DNS 缓存），返回**实际绑定端口**（注入 listen 0 = 内核选空闲端口的回读面）。
/// serve 线程的「非正常退出」按 off 语义收口 + err 如实呈现（L6——「状态说在听、
/// 实际没人 accept」的防御面）。
fn start_listener(
    dial: &CarrierDial,
    logf: &Arc<dyn Fn(&str) + Send + Sync>,
    warnf: &Arc<dyn Fn(&str) + Send + Sync>,
    e: &mut EntryRt,
) -> Result<u16, String> {
    let ln = std::net::TcpListener::bind(("127.0.0.1", e.rec.listen))
        .map_err(|err| describe_listen_err(e.rec.listen, &err))?;
    let port = ln.local_addr().map(|a| a.port()).map_err(|err| err.to_string())?;
    let host = e.rec.host.clone();
    let cache = Arc::new(DnsCache::new(CACHE_MAX));
    let resolver = resolver_for(dial, &host, &cache);
    let srv = SocksServer::new(SocksServerConfig {
        resolver,
        dial: clone_dial(dial),
        host: host.clone(),
        logf: Arc::clone(logf),
    });
    let srv2 = Arc::clone(&srv);
    let warnf = Arc::clone(warnf);
    srv2.attach(ln);
    std::thread::Builder::new()
        .name("hw-socks-serve".to_owned())
        .stack_size(512 * 1024)
        .spawn(move || {
            if let Err(err) = srv2.serve() {
                // 非监听被关的退出（accept 瞬态烧尽）= 无人受理的僵尸监听——server
                // 自收口（close + 生命周期位），err 经告警行呈现；条目状态由下次
                // socks on/status 修正。
                srv2.close();
                warnf(&format!(
                    "socks: {} 监听 accept 失败（{err}）——按 off 收口，重试 socks on 可恢复",
                    short_host(&host)
                ));
            }
        })
        .map_err(|err| err.to_string())?;
    e.srv = Some(srv);
    e.rec.on = true;
    e.err = String::new();
    Ok(port)
}

/// 关监听 + 显式关在世连接（RST）。调用方持锁。
fn stop_listener(e: &mut EntryRt) {
    if let Some(srv) = e.srv.take() {
        srv.close(); // RST 收口全部在世连接 + 断生命周期位
    }
    e.rec.on = false;
}

fn clone_dial(d: &CarrierDial) -> CarrierDial {
    CarrierDial { dial_port: Arc::clone(&d.dial_port), dial: Arc::clone(&d.dial) }
}

/// 域名远程解析腿（socks 承载面的解析注入）：缓存（每 listener 一份 = 每主机一份）
/// → 未命中经拨号缝拨出口 5300 发 DNS-over-TCP A 查询（出口在隧道栈内监听该端口）；
/// 否定/超时不缓存。预算 = RESOLVE_BUDGET（每查询一条新连接）。
fn resolver_for(dial: &CarrierDial, host: &str, cache: &Arc<DnsCache>) -> Resolver {
    let dial_port = Arc::clone(&dial.dial_port);
    let host = host.to_owned();
    let cache = Arc::clone(cache);
    Arc::new(move |name: &str| -> Result<Vec<std::net::Ipv4Addr>, String> {
        if let Some(addrs) = cache.get(name) {
            return Ok(addrs);
        }
        // 拨号腿 5s + 解析总预算 5s（中-1：Go ResolveOverConn 的 SetReadDeadline +
        // ctx 同义——出口代答黑洞时 5s 归因超时、不缓存；超时由 dnsq 的期限壳收
        // 连接解阻塞）。
        let conn = dial_port(&host, SOCKS_DNS_PORT, RESOLVE_BUDGET)
            .map_err(|e| format!("解析腿拨号（出口 {SOCKS_DNS_PORT}）失败：{e}"))?;
        let res = dnsq::resolve_with_deadline(Arc::clone(&conn.io), name, RESOLVE_BUDGET);
        conn.io.close(); // 每查询一条新连接
        match res {
            // NXDOMAIN / 无 A / 超时——不缓存，错误文案可区分。
            Ok(r) => {
                cache.put(name, &r.addrs, r.ttl_min);
                Ok(r.addrs)
            }
            Err(e) => Err(e.to_string()),
        }
    })
}

fn load_entries(
    path: &std::path::Path,
    warnf: &Arc<dyn Fn(&str) + Send + Sync>,
) -> Result<Vec<SocksEntry>, String> {
    match std::fs::read(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(format!("读 {}: {e}", path.display())),
        Ok(b) => match serde_json::from_slice::<Vec<SocksEntry>>(&b) {
            Ok(recs) => Ok(recs),
            Err(e) => {
                let backup = path.with_file_name(format!(
                    "{}.corrupt-{}",
                    SOCKS_FILE,
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs())
                        .unwrap_or(0)
                ));
                match std::fs::rename(path, &backup) {
                    Ok(()) => {
                        (warnf)(&format!(
                            "socks.json 损坏（{e}）——已备份 {}，按空表重建",
                            backup.display()
                        ));
                        Ok(Vec::new())
                    }
                    Err(rerr) => Err(format!("socks.json 损坏（{e}）且备份失败：{rerr}")),
                }
            }
        },
    }
}

// ---------- DNS 缓存（每 listener 一份） ----------

type CacheInner = (
    Vec<String>,
    std::collections::HashMap<String, (Vec<std::net::Ipv4Addr>, std::time::Instant)>,
);

/// 域名 → 候选列表的有界 TTL 缓存（FIFO 逐出；否定不进缓存；逐出即弃不续期）。
struct DnsCache {
    max: usize,
    inner: Mutex<CacheInner>,
}

impl DnsCache {
    fn new(max: usize) -> DnsCache {
        DnsCache { max: max.max(1), inner: Mutex::new((Vec::new(), std::collections::HashMap::new())) }
    }

    fn get(&self, name: &str) -> Option<Vec<std::net::Ipv4Addr>> {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((addrs, expiry)) = g.1.get(name) {
            if std::time::Instant::now() < *expiry {
                return Some(addrs.clone());
            }
            g.1.remove(name); // 过期惰性清除
        }
        None
    }

    fn put(&self, name: &str, addrs: &[std::net::Ipv4Addr], ttl_s: u32) {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if !g.1.contains_key(name) {
            g.0.push(name.to_owned());
            // FIFO 逐出最旧。
            if g.0.len() > self.max {
                let oldest = g.0.remove(0);
                g.1.remove(&oldest);
            }
        }
        let ttl = Duration::from_secs(ttl_s.max(1) as u64);
        g.1.insert(name.to_owned(), (addrs.to_vec(), std::time::Instant::now() + ttl));
    }
}

#[cfg(test)]
mod tests {
    use super::super::testutil::{temp_dir, FakeDial};
    use super::*;

    type Logf2 = (Arc<dyn Fn(&str) + Send + Sync>, Arc<dyn Fn(&str) + Send + Sync>);
    fn nop_log() -> Logf2 {
        (Arc::new(|_| {}), Arc::new(|_| {}))
    }

    fn hex_host(tag: u8) -> String {
        format!("{tag:0>64x}")
    }

    /// on/off/status 往返：端口记忆保留、跨主机全局唯一（含 off 记忆端口）、换端口
    /// 关旧开新、持久化重建（on 条目自动起监听）。
    #[test]
    fn on_off_memory_and_port_unique() {
        let dir = temp_dir("skm");
        let dial = FakeDial::new();
        let (logf, warnf) = nop_log();
        let m = SocksManager::open(&dir, &dial.carrier_dial(), &logf, &warnf).unwrap();

        // on（显式端口）→ status on。
        let p = m.on(&hex_host(1), 20801).unwrap();
        assert_eq!(p, 20801);
        let st = m.status();
        assert_eq!(st.len(), 1);
        assert!(st[0].on);
        assert_eq!(st[0].listen, 20801);

        // 幂等：同端口再 on = 同端口。
        assert_eq!(m.on(&hex_host(1), 20801).unwrap(), 20801);

        // 跨主机端口冲突（含 off 记忆端口——全局唯一按在册判）。
        assert!(matches!(
            m.on(&hex_host(2), 20801),
            Err(CarrierErr::PortTaken { port: 20801, .. })
        ));

        // off：记忆保留 + 状态面 off 但仍在表。
        m.off(&hex_host(1)).unwrap();
        let st = m.status();
        assert!(!st[0].on);
        assert_eq!(st[0].listen, 20801, "off 不抹端口记忆");
        // off 后：另一主机占用 20801 仍被拒（记忆端口在册）。
        assert!(matches!(
            m.on(&hex_host(2), 20801),
            Err(CarrierErr::PortTaken { port: 20801, .. })
        ));
        // 本主机 on 缺省（0）= 沿用记忆端口。
        assert_eq!(m.on(&hex_host(1), 0).unwrap(), 20801);

        // 换端口：关旧开新。
        assert_eq!(m.on(&hex_host(1), 20802).unwrap(), 20802);
        // 旧端口释放后另一主机可用。
        assert_eq!(m.on(&hex_host(2), 20801).unwrap(), 20801);

        // off 不在表的主机 = NoRule。
        assert!(matches!(m.off(&hex_host(9)), Err(CarrierErr::NoRule(_))));

        // 持久化重建：on 条目自动起监听（20802/20801 都能绑回）。
        drop(m);
        let m2 = SocksManager::open(&dir, &dial.carrier_dial(), &logf, &warnf).unwrap();
        let st = m2.status();
        assert_eq!(st.len(), 2);
        assert!(st.iter().all(|s| s.on), "重开按 on 条目重建监听：{st:?}");

        // 级联：host1 删除 = 记忆消失、监听关；host2 不受影响。
        m2.remove_host(&hex_host(1));
        let st = m2.status();
        assert_eq!(st.len(), 1);
        assert_eq!(st[0].host, hex_host(2));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Q-H F4：accept 烧尽 ⇒ dead 落账（此前恒 None）→ status 按 off 呈现 + err
    /// 如实可查 → `socks on` 不再被幂等短路、可重建（端口不变）。
    /// 注入缝 = `PollListener::inject_accept_failure`（不硬关真 fd——设计门 B7）。
    #[test]
    fn socks_dead_is_recorded_and_rebuildable() {
        let dir = temp_dir("skm-dead");
        let dial = FakeDial::new();
        let (logf, warnf) = nop_log();
        let m = SocksManager::open(&dir, &dial.carrier_dial(), &logf, &warnf).unwrap();
        let host = hex_host(3);
        let port = m.on(&host, 20811).unwrap();
        assert!(m.status()[0].on);
        // 注入 accept 失败（下一次 accept 直返 Failed）——serve 线程随即按 off 收口。
        {
            let entries = m.entries.lock().unwrap_or_else(|e| e.into_inner());
            let srv = entries[0].srv.as_ref().expect("on 后 srv 在位");
            srv.inject_accept_failure("连续失败（9 次）：注入的 accept 失效");
        }
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            let st = m.status();
            if !st[0].on {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "dead 未落账（status 仍 on=true）：{st:?}"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        let st = m.status();
        assert!(!st[0].on, "accept 烧尽后 on 必须 false（此前谎报 true）");
        assert!(st[0].err.contains("注入"), "err 如实可查：{:?}", st[0].err);
        // 重建：缺省 on 沿用记忆端口（幂等短路已失效 ⇒ 真走换新路径）。
        assert_eq!(m.on(&host, 0).unwrap(), port, "重建端口不变（记忆保留）");
        let st = m.status();
        assert!(st[0].on, "重建后 on=true");
        assert!(st[0].err.is_empty(), "重建后 err 清空：{:?}", st[0].err);
        {
            let entries = m.entries.lock().unwrap_or_else(|e| e.into_inner());
            assert!(
                entries[0].srv.as_ref().unwrap().dead_reason().is_none(),
                "新 srv 的 dead 位必须归零"
            );
        }
        drop(m);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 端口被外占：add 拒绝；持久化里的 on 条目重建失败按 off 呈现 + err。
    #[test]
    fn occupied_and_rebuild_failed() {
        let dir = temp_dir("skm-occ");
        let held = std::net::TcpListener::bind("127.0.0.1:20803").unwrap();
        let dial = FakeDial::new();
        let (logf, warnf) = nop_log();
        let m = SocksManager::open(&dir, &dial.carrier_dial(), &logf, &warnf).unwrap();
        assert!(matches!(
            m.on(&hex_host(1), 20803),
            Err(CarrierErr::ListenFailed(20803, _))
        ));
        assert!(m.status().is_empty(), "失败不入表");
        // 写 on 条目 → 重开 = off 呈现 + err。
        let recs = vec![SocksEntry { host: hex_host(1), on: true, listen: 20803 }];
        std::fs::write(dir.join(SOCKS_FILE), serde_json::to_string_pretty(&recs).unwrap()).unwrap();
        drop(m);
        let m2 = SocksManager::open(&dir, &dial.carrier_dial(), &logf, &warnf).unwrap();
        let st = m2.status();
        assert_eq!(st.len(), 1);
        assert!(!st[0].on, "重建失败按 off 呈现");
        assert!(!st[0].err.is_empty());
        drop(held);
        // 重试 on 可恢复（exec-r1 L6 的可恢复面）。
        assert_eq!(m2.on(&hex_host(1), 20803).unwrap(), 20803);
        assert!(m2.status()[0].on);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
