//! 多主机表（语义真源 `baseline:clientcore/facade/table.go`——4a 自 daemon registry
//! 迁入的表语义真源）：hosts.json 原子落盘、同键刷新（同后端重签发）、carried
//! 保真、先落盘再起会话（落盘失败零副作用）、每记录自持会话。
//!
//! 会话 = 仓内 `facade::host_session::HostSession`（M5 C2 换源：QUIC 岛承接的
//! 「无 TUN 服务会话」——暖机/巡检/恢复阶梯/整会话重建都在会话内，「每记录自持」
//! 天然成立，无 Go 侧 recGate 镜像需求；WG 档 `session::Session` 已于 M5 C3 删除）。
//!
//! **B0-2b 第 1 棒注记**：Go hostsession 的 Observer/LinkChanged 钩子在 Rust
//! Session 无对应面——事件面（session.state_changed / link.changed）由 1s 差分
//! 轮询线程生产（`spawn_event_pump`；延迟 ≤1s，词表/载荷与 Go 同源）。换真钩子
//! 登记为后续棒（Session 加 observer 缝），见 docs/reviews/B0-2b.md。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::bus::Bus;
use super::proto::{BackendErr, HostAddResult, HostBrief, HostReach, HostState, ReachTested};
use super::vocab::{self, EventPayload};
use crate::facade::host_session::{HostSession, HostSessionConfig};
use crate::token::{self, EndpointKind};

/// 主机表持久化文件（0600）。
pub const HOSTS_FILE_NAME: &str = "hosts.json";

/// reach 探测预算常量（Go pkg/probe/reach.go 同源；spec host-management ≤3.5s MUST）。
const REACH_PARENT_BUDGET: Duration = Duration::from_millis(3500);
const REACH_PROBE_BUDGET: Duration = Duration::from_secs(3);
/// 请求填充长度（与 wgcore probePad 同源同值——要拿端点列表段就得 pad 够长）。
const REACH_PAD: usize = 200;

/// hosts.json 的一条：一台后端主机的登记（键 = ID = token 里的后端公钥 hex）。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct HostRecord {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub token: String,
    /// Unix 毫秒（Go time.Time JSON 形态的 Rust 收敛——写 JSON 消费侧只有本表，
    /// 整数毫秒无需 Go 的 RFC3339 解析面）。
    #[serde(rename = "addedAt")]
    pub added_at: i64,
}

struct HostEntry {
    rec: HostRecord,
    sess: Option<Arc<HostSession>>,
}

struct Inner {
    hosts: BTreeMap<[u8; 32], HostEntry>,
    /// 装载时 id 非法条目的原样携带：不启动会话、不进寻址面，但落盘时随有效记录
    /// 一并写回（「不丢数据」优先于「表自洁」）。
    carried: Vec<HostRecord>,
}

/// 状态差分的一拍快照项（host 记录 + 其会话）。
type HostSnap = (HostRecord, Option<Arc<HostSession>>);

/// 事件差分线程的「上一拍」状态指纹（state 值 + link 四元组）。
type LastSeen = (String, Option<(String, String, i64, i64)>);

/// 多主机会话注册表（键 = peerID；桌面直拨隧道端口，桥为零）。
pub struct HostTable {
    client_dir: PathBuf,
    identity_dir: PathBuf,
    logf: Arc<dyn Fn(&str) + Send + Sync>,
    bus: Arc<Bus>,
    /// 表变更（Add/Remove/Close）的串行化（Go opMu）：临界区覆盖锁外的会话停止。
    change_mu: Mutex<()>,
    inner: Mutex<Inner>,
    /// 事件差分线程的上一拍状态（host → (state, link 指纹)）。
    last_seen: Mutex<BTreeMap<[u8; 32], LastSeen>>,
}

impl HostTable {
    /// 打开主机表（读 hosts.json——缺失 = 空表；损坏 = 备份后空表 + 告警）并按表
    /// 逐后端拉会话（「重启按表恢复」）。半途失败返回错误（表不挂载）。
    /// `endpoint_cache_dir`：**WG 档装配参数，M5 C2 起本模块零消费**（设计
    /// §1.6-G-1：岛候选来自 token，无学习缓存面）——保留形参只为零改装配面。
    pub fn open(
        client_dir: &Path,
        identity_dir: &Path,
        _endpoint_cache_dir: &Path,
        logf: Arc<dyn Fn(&str) + Send + Sync>,
        bus: Arc<Bus>,
    ) -> Result<Arc<HostTable>, String> {
        let table = HostTable {
            client_dir: client_dir.to_owned(),
            identity_dir: identity_dir.to_owned(),
            logf,
            bus,
            change_mu: Mutex::new(()),
            inner: Mutex::new(Inner { hosts: BTreeMap::new(), carried: Vec::new() }),
            last_seen: Mutex::new(BTreeMap::new()),
        };
        let recs = load_hosts(&client_dir.join(HOSTS_FILE_NAME), &table.logf)?;
        {
            let mut in_ = table.inner.lock().unwrap_or_else(|e| e.into_inner());
            for rec in recs {
                match decode_peer_id(&rec.id) {
                    Some(id) => {
                        in_.hosts.insert(id, HostEntry { rec, sess: None });
                    }
                    None => {
                        (table.logf)(&format!(
                            "hosts: 记录 {:?} 的 id 非法——保留在表、不启动会话、不参与寻址；可手工修正或删除该条目",
                            rec.id
                        ));
                        in_.carried.push(rec);
                    }
                }
            }
        }
        // 按表起会话（装载段）。
        let ids: Vec<[u8; 32]> =
            table.inner.lock().unwrap_or_else(|e| e.into_inner()).hosts.keys().copied().collect();
        for id in ids {
            table.start_session_for(id);
        }
        Ok(Arc::new(table))
    }

    /// 装载完成的每台主机补发 session.added（Go 契约③——client 角色重建时控制面
    /// 连接不断，在途订阅者靠这批 added 恢复视图，不静默空表）。
    pub fn publish_loaded(&self) {
        let recs: Vec<HostRecord> = {
            self.inner
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .hosts
                .values()
                .map(|e| e.rec.clone())
                .collect()
        };
        for rec in recs {
            self.bus.publish(EventPayload::SessionAdded {
                host: rec.id,
                name: rec.name.unwrap_or_default(),
                added_at: rec.added_at,
            });
        }
    }

    /// 事件差分线程：1s 拍逐主机快照差分 → session.state_changed / link.changed。
    /// 词表与载荷同 Go；延迟 ≤1s（Session 无 observer 钩子的第 1 棒折衷，头注释）。
    pub fn spawn_event_pump(self: &Arc<Self>) {
        let t = Arc::clone(self);
        std::thread::Builder::new()
            .name("hw-hosts-ev".to_owned())
            .spawn(move || loop {
                std::thread::sleep(Duration::from_secs(1));
                t.pump_once();
            })
            .expect("线程创建不可失败");
    }

    fn pump_once(&self) {
        let entries: Vec<([u8; 32], HostSnap)> = {
            self.inner
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .hosts
                .iter()
                .map(|(id, e)| (*id, (e.rec.clone(), e.sess.clone())))
                .collect()
        };
        for (id, (rec, sess)) in entries {
            let Some(sess) = sess else { continue };
            let snap = sess.snapshot();
            let state = snap.state.as_str().to_owned();
            // link.changed 触发条件 = (via, ep) 变化（Go hostsession setLink 语义：
            // 「rtt 每拍都不同，不作为变化判据——否则事件流退化成 60s 节拍器」；
            // rtt/at 只进载荷——hosts-1 整改）。
            let link = snap.link.map(|l| (l.via.clone(), l.ep.clone(), l.rtt_ms, l.at_ms));
            let changed = {
                let mut seen = self.last_seen.lock().unwrap_or_else(|e| e.into_inner());
                let prev = seen.get(&id);
                let state_changed = prev.is_none_or(|(s, _)| *s != state);
                let link_changed = match (prev.and_then(|(_, l)| l.clone()), &link) {
                    (None, Some(_)) => true,
                    (Some(_), None) => true,
                    (Some(a), Some(b)) => (&a.0, &a.1) != (&b.0, &b.1),
                    (None, None) => false,
                };
                seen.insert(id, (state.clone(), link.clone()));
                (state_changed, link_changed)
            };
            if changed.0 {
                self.bus.publish(EventPayload::SessionStateChanged {
                    host: rec.id.clone(),
                    state,
                    reason: snap.reason.clone(),
                });
            }
            if changed.1 {
                if let Some((via, ep, rtt_ms, at_ms)) = link {
                    self.bus.publish(EventPayload::LinkChanged {
                        host: rec.id.clone(),
                        via,
                        ep,
                        rtt_ms,
                        at: at_ms,
                    });
                }
            }
        }
    }

    // ---------- 表操作（host.add / host.remove 的宿主面） ----------

    /// 添加/刷新一台主机：decode（bad_token 前置——force 不绕过）→ 有界旁路探测
    /// （纯旁路不碰会话状态）→ 入表。全不可达且未带 force → HostUnreachable
    /// （不入表、无任何表副作用）；force = 跳过探测直接入表（tier=skipped）。
    pub fn add_host(&self, name: &str, token_raw: &str, force: bool) -> Result<HostAddResult, BackendErr> {
        let tok = token::decode(token_raw.trim()).map_err(|e| BackendErr::BadToken(e.to_string()))?;
        let mut res = HostAddResult {
            id: hex(tok.peer_id.as_bytes()),
            name: (!name.is_empty()).then(|| name.to_owned()),
            added_at: 0,
            reach: HostReach { tier: vocab::REACH_TIER_SKIPPED.to_owned(), best_ep: None, rtt_ms: None, tested: Vec::new() },
        };
        if !force {
            let rep = reach(token_raw.trim());
            match rep.tier.as_str() {
                t @ (vocab::REACH_TIER_DIRECT | vocab::REACH_TIER_RELAY) => {
                    res.reach.tier = t.to_owned()
                }
                _ => return Err(BackendErr::HostUnreachable), // none：全不可达且未带 force
            }
            res.reach.tested = rep.tested;
            res.reach.best_ep = rep.best_ep;
            res.reach.rtt_ms = rep.best_rtt_ms;
        }
        let rec = self.add_record(name, token_raw.trim(), tok)?;
        res.name = rec.name.clone();
        res.added_at = rec.added_at;
        Ok(res)
    }

    /// 入表（B5：**先落盘再起会话/换会话**——落盘失败零副作用；同键刷新 = 先落盘
    /// 新 token → 停旧 → 起新、内存记录换新值）。
    fn add_record(
        &self,
        name: &str,
        token_raw: &str,
        tok: token::Token,
    ) -> Result<HostRecord, BackendErr> {
        let _chg = self.change_mu.lock().unwrap_or_else(|e| e.into_inner());
        {
            let mut in_ = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            let existing = in_.hosts.get(tok.peer_id.as_bytes()).map(|e| e.rec.clone());
            if let Some(old_rec) = existing {
                if old_rec.token == token_raw {
                    return Err(BackendErr::HostExists); // 同 token 重复添加
                }
                // 同后端重签发：同键刷新。
                let mut new_rec = old_rec.clone();
                new_rec.token = token_raw.to_owned();
                if !name.is_empty() {
                    new_rec.name = Some(name.to_owned());
                }
                let next = replace_record(records_locked(&in_), &new_rec);
                save_records(&self.client_dir, &next).map_err(|er| BackendErr::Other(format!("hosts.json 落盘失败：{er}")))?;
                let old_sess = in_.hosts.get_mut(tok.peer_id.as_bytes()).and_then(|e| e.sess.take());
                if let Some(e) = in_.hosts.get_mut(tok.peer_id.as_bytes()) {
                    e.rec = new_rec.clone();
                }
                drop(in_);
                // 停旧会话（锁外——Rust Session::stop 有界同步收口，无 Go 的 -1 形态）。
                if let Some(old) = old_sess {
                    old.stop();
                }
                // 起新会话装回原条目。
                self.start_session_for(*tok.peer_id.as_bytes());
                (self.logf)(&format!("hosts: {}（{}）token 已刷新（同后端重签发）", new_rec.id, new_rec.name.as_deref().unwrap_or("")));
                return Ok(new_rec);
            }
        }
        let rec = HostRecord {
            id: hex(tok.peer_id.as_bytes()),
            name: (!name.is_empty()).then(|| name.to_owned()),
            token: token_raw.to_owned(),
            added_at: crate::daemon::now_ms(),
        };
        {
            let mut in_ = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            let next = {
                let mut v = records_locked(&in_);
                v.push(rec.clone());
                v
            };
            save_records(&self.client_dir, &next)
                .map_err(|er| BackendErr::Other(format!("hosts.json 落盘失败：{er}")))?;
            in_.hosts.insert(*tok.peer_id.as_bytes(), HostEntry { rec: rec.clone(), sess: None });
        }
        self.start_session_for(*tok.peer_id.as_bytes());
        (self.logf)(&format!("hosts: + {}（{}）", rec.id, rec.name.as_deref().unwrap_or("")));
        // 锁外 emit。
        self.bus.publish(EventPayload::SessionAdded {
            host: rec.id.clone(),
            name: rec.name.clone().unwrap_or_default(),
            added_at: rec.added_at,
        });
        Ok(rec)
    }

    /// 摘除一台主机（先落盘、停会话、删条目——落盘失败 = 内存与会话均未动）。
    pub fn remove_host(&self, id: &[u8; 32]) -> Result<(), BackendErr> {
        let _chg = self.change_mu.lock().unwrap_or_else(|e| e.into_inner());
        let (rec, sess) = {
            let mut in_ = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            let Some(rec) = in_.hosts.get(id).map(|e| e.rec.clone()) else {
                return Err(BackendErr::NoHost);
            };
            let next = without_record(records_locked(&in_), &rec.id);
            save_records(&self.client_dir, &next)
                .map_err(|er| BackendErr::Other(format!("hosts.json 落盘失败：{er}")))?;
            let sess = in_.hosts.get_mut(id).and_then(|e| e.sess.take());
            in_.hosts.remove(id);
            self.last_seen.lock().unwrap_or_else(|e| e.into_inner()).remove(id);
            (rec, sess)
        };
        if let Some(sess) = sess {
            sess.stop();
        }
        (self.logf)(&format!("hosts: - {}（{}）", rec.id, rec.name.as_deref().unwrap_or("")));
        self.bus.publish(EventPayload::SessionRemoved {
            host: rec.id,
            reason: vocab::SESSION_REMOVED_USER.to_owned(),
        });
        Ok(())
    }

    /// 收工：停全部会话 + 逐台 session.removed 照发（reason=detach——在途订阅前端
    /// 经事件面看到完整收工，不静默消失）。
    pub fn close(&self) {
        let _chg = self.change_mu.lock().unwrap_or_else(|e| e.into_inner());
        let drained: Vec<HostRecord> = {
            let mut in_ = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            let recs: Vec<HostRecord> = in_.hosts.values().map(|e| e.rec.clone()).collect();
            let sesses: Vec<Option<Arc<HostSession>>> =
                in_.hosts.values_mut().map(|e| e.sess.take()).collect();
            in_.hosts.clear();
            drop(in_);
            for s in sesses.into_iter().flatten() {
                s.stop();
            }
            recs
        };
        for rec in drained {
            self.bus.publish(EventPayload::SessionRemoved {
                host: rec.id,
                reason: vocab::SESSION_REMOVED_DETACH.to_owned(),
            });
        }
    }

    // ---------- 读面 ----------

    /// 表快照（登记记录；BTreeMap 按 ID 排序稳定输出）。
    pub fn hosts(&self) -> Vec<HostRecord> {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .hosts
            .values()
            .map(|e| e.rec.clone())
            .collect()
    }

    /// 主机登记面（host.list 的静态部分；按 ID 排序）。
    pub fn briefs(&self) -> Vec<HostBrief> {
        self.hosts()
            .into_iter()
            .map(|r| HostBrief { id: r.id, name: r.name, added_at: r.added_at })
            .collect()
    }

    /// 各主机动态面（state/reason/link/stats——无锁快照汇成）。
    pub fn states(&self) -> Vec<HostState> {
        let entries: Vec<HostSnap> = {
            self.inner
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .hosts
                .values()
                .map(|e| (e.rec.clone(), e.sess.clone()))
                .collect()
        };
        let mut out = Vec::with_capacity(entries.len());
        for (rec, sess) in entries {
            out.push(host_state_of(&rec, sess.as_deref()));
        }
        out
    }

    /// 取一台主机的会话（流腿/状态面用；不在表 = None）。
    pub fn session(&self, id: &[u8; 32]) -> Option<Arc<HostSession>> {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .hosts
            .get(id)
            .and_then(|e| e.sess.clone())
    }

    /// 登记在册谓词（承载面成员检查用——与 `session` 不同：会话对象不在〔构造期
    /// 失败/重建窗口〕仍在册）。
    pub fn has_record(&self, id: &[u8; 32]) -> bool {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).hosts.contains_key(id)
    }

    pub fn session_by_hex(&self, id_hex: &str) -> Option<Arc<HostSession>> {
        decode_peer_id(id_hex).and_then(|id| self.session(&id))
    }

    // ---------- 会话构造 ----------

    fn start_session_for(&self, id: [u8; 32]) {
        let (token_raw, host_log) = {
            let in_ = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            let Some(e) = in_.hosts.get(&id) else { return };
            (e.rec.token.clone(), e.rec.id.clone())
        };
        let Some(tok) = token::decode(&token_raw).ok() else {
            // Q-H F12：token 解码失败不再静默 return（Rust 独有缺口——Go 无此分支）；
            // 状态面仍如实记为 failed/session_not_built（不加重建环：构造期错误源
            // 非瞬态，重建走 token 刷新/进程重启——Go `facade/table.go` 同形）。
            (self.logf)(&format!(
                "hosts: {host_log} 会话构造失败：台账 token 解码失败（重新 host add 刷 token 可修复）"
            ));
            return;
        };
        let logf = {
            let host = host_log.clone();
            let base = Arc::clone(&self.logf);
            Arc::new(move |s: &str| base(&format!("hosts: {host} {s}"))) as Arc<dyn Fn(&str) + Send + Sync>
        };
        // M5 C2 换源：宿主会话（QUIC 岛）。端点缓存目录（装配参数）随 WG 档退役
        // （设计 §1.6-G-1：岛候选来自 token，无学习缓存）——本模块不再消费它。
        match HostSession::start(HostSessionConfig {
            token: tok,
            identity_dir: Some(self.identity_dir.clone()),
            logf,
        }) {
            Ok(sess) => {
                let mut in_ = self.inner.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(e) = in_.hosts.get_mut(&id) {
                    e.sess = Some(Arc::new(sess));
                }
            }
            Err(e) => {
                // 构造期失败：条目仍在表（登记面可见 failed/session_not_built），不因
                // 构造问题丢主机登记。
                (self.logf)(&format!("hosts: {host_log} 会话构造失败：{e}"));
            }
        }
    }
}

/// 状态面单条（Go hostStateOf 同义：会话对象不在 = failed/session_not_built）。
fn host_state_of(rec: &HostRecord, sess: Option<&HostSession>) -> HostState {
    let Some(sess) = sess else {
        return HostState {
            id: rec.id.clone(),
            name: rec.name.clone(),
            state: "failed".to_owned(),
            reason: Some("session_not_built".to_owned()),
            link: None,
            stats: None,
            added_at: Some(rec.added_at),
        };
    };
    let snap = sess.snapshot();
    HostState {
        id: rec.id.clone(),
        name: rec.name.clone(),
        state: snap.state.as_str().to_owned(),
        reason: (!snap.reason.is_empty()).then(|| snap.reason.clone()),
        link: snap.link.map(|l| super::proto::HostLink { via: l.via, ep: l.ep, rtt_ms: l.rtt_ms, at: l.at_ms }),
        stats: snap.stats.map(|(rx, tx)| super::proto::HostRxTx { rx_bytes: rx as i64, tx_bytes: tx as i64 }),
        added_at: Some(rec.added_at),
    }
}

// ---------- 落盘 ----------

fn records_locked(in_: &Inner) -> Vec<HostRecord> {
    // **含 carried**：非法 id 条目随每次落盘原样写回（有效记录在前、carried 原序
    // 恒在集尾——与 Go 的「同集落盘，相对顺序不承诺」等价；寻址面不含 carried）。
    let mut v: Vec<HostRecord> = in_.hosts.values().map(|e| e.rec.clone()).collect();
    v.extend(in_.carried.iter().cloned());
    v
}

fn replace_record(mut recs: Vec<HostRecord>, rec: &HostRecord) -> Vec<HostRecord> {
    for r in recs.iter_mut() {
        if r.id == rec.id {
            *r = rec.clone();
            return recs;
        }
    }
    recs.push(rec.clone());
    recs
}

fn without_record(recs: Vec<HostRecord>, id: &str) -> Vec<HostRecord> {
    recs.into_iter().filter(|r| r.id != id).collect()
}

/// temp + rename 原子替换（0600）：kill -9 落在写窗口也只会留下完整旧表或完整新表。
fn save_records(client_dir: &Path, recs: &[HostRecord]) -> std::io::Result<()> {
    std::fs::create_dir_all(client_dir)?;
    let path = client_dir.join(HOSTS_FILE_NAME);
    let mut b = serde_json::to_string_pretty(recs)
        .map_err(|e| std::io::Error::other(e.to_string()))?
        .into_bytes();
    b.push(b'\n');
    // 创建即 0600（Go os.WriteFile(0o600) 同形——hosts-2 整改：先写后 chmod 存在
    // 短窗口，token 凭据对同机其他用户可读）。
    #[cfg(unix)]
    let tmp = {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path.with_extension("json.tmp"))?;
        f.write_all(&b)?;
        f.sync_all().ok();
        path.with_extension("json.tmp")
    };
    #[cfg(not(unix))]
    let tmp = {
        std::fs::write(path.with_extension("json.tmp"), &b)?;
        path.with_extension("json.tmp")
    };
    std::fs::rename(tmp, path)
}

/// 读主机表：缺失 = 空表；损坏 = 备份 `hosts.json.corrupt-<ts>` 后按空表启动 +
/// 告警（损坏即拒启会让 client 角色无限退避；备份保住「不静默清空」，空表启动
/// 保住不被一份坏文件锁死）。备份本身失败仍报错拒启。
fn load_hosts(path: &Path, logf: &Arc<dyn Fn(&str) + Send + Sync>) -> Result<Vec<HostRecord>, String> {
    let b = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("读 hosts.json：{e}")),
    };
    match serde_json::from_slice::<Vec<HostRecord>>(&b) {
        Ok(recs) => Ok(recs),
        Err(e) => {
            let backup = path.with_file_name(format!(
                "{}.corrupt-{}",
                HOSTS_FILE_NAME,
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0)
            ));
            std::fs::rename(path, &backup).map_err(|er| format!("hosts.json 损坏（{e}）且备份失败：{er}"))?;
            (logf)(&format!(
                "hosts.json 损坏（{e}）——已备份 {}，按空表启动（原件保留，可修复后重启恢复）",
                backup.display()
            ));
            Ok(Vec::new())
        }
    }
}

/// hex peerID 解码（Backend 装配与表内部共用；非法 = None）。
pub fn decode_peer_id_pub(id_hex: &str) -> Option<[u8; 32]> {
    decode_peer_id(id_hex)
}

fn decode_peer_id(id_hex: &str) -> Option<[u8; 32]> {
    let v = hex_decode(id_hex)?;
    v.try_into().ok()
}

fn hex(b: &[u8; 32]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn hex_decode(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) || !s.bytes().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let mut out = Vec::with_capacity(s.len() / 2);
    let b = s.as_bytes();
    for i in (0..b.len()).step_by(2) {
        let hi = (b[i] as char).to_digit(16)?;
        let lo = (b[i + 1] as char).to_digit(16)?;
        out.push((hi * 16 + lo) as u8);
    }
    Some(out)
}

// ---------- reach 探测（纯旁路：明文一问一答，不进 WG、无身份、不碰会话状态） ----------

struct ReachReport {
    tier: String,
    tested: Vec<ReachTested>,
    best_ep: Option<String>,
    best_rtt_ms: Option<i64>,
}

/// token 端点的**字面量**展开（Go `resolveReachTarget` 的 IP 字面量分支同义）。
///
/// 地址归一（M2 代码门 G3，同类 D1；Go 同分支 `netip.AddrPortFrom(ip.Unmap(), port)`
/// ——reach.go:215）：token 字面量允许写 `[::ffff:a.b.c.d]`，不归一时①与域名解析
/// 产物/纯 v4 字面量**同址去重失配**（同一地址探两次）②回执 `ep` 串（用户可见）
/// 是 mapped 形态。域名路径的 IP 已由 `lookup_host` 契约归一（v4-mapped → v4）。
fn expand_literal(addr: &str) -> Option<std::net::SocketAddr> {
    addr.parse::<std::net::SocketAddr>().ok().map(crate::udpbatch::unmap_v4_in6)
}

/// 对 token 端点全集做参照点探测：每端点独立线程、总预算 3.5s（父预算封顶——
/// spec host-management 的 ≤3.5s MUST 由此成立）。只回报活端点；死端点静默。
/// Tier：有直连应答 = direct；直连全无且中继有应答 = relay；全无 = none。
fn reach(token_raw: &str) -> ReachReport {
    let tok = match token::decode(token_raw) {
        Ok(t) => t,
        Err(_) => {
            return ReachReport { tier: "none".to_owned(), tested: Vec::new(), best_ep: None, best_rtt_ms: None }
        }
    };
    let t0 = Instant::now();
    let (tx, rx) = std::sync::mpsc::channel::<ReachTested>();
    // P0-4：域名端点解析（IP 字面量直用；域名解析一次——每端点独立线程语义下
    // 域名解析在分发前做，预算共享父预算 3.5s）。**全量地址**入探测（评审 4.9：
    // 只取首个 A 会让多记录域名的活路径被误判不可达）；跨端点同址去重。
    let mut eps: Vec<(std::net::SocketAddr, bool)> = Vec::new();
    for ep in &tok.endpoints {
        // **M5 §2.6-G5（订正）**：过滤键 = `Quic | Relay`（与 `quic_candidates` 同源）——
        // 旧键「只吃 `Quic`」（WG 档的 `is_wg` 反过滤；M5 C3 已随 WG 面删除）在两处都错：①WG 端点（`Direct`）不再是承载，
        // ②**`Relay` 也是 QUIC 的合法承载**（relay-only token 会被整条漏掉 ⇒ reach 恒
        // `none` ⇒ `DC3` 三档结论不可达）。reach 的复绿还依赖出口侧「参照点探测明文
        // 应答」在新落点可用（设计 §1.2-M4 的 S3a 迁址面）。
        if !matches!(ep.kind, EndpointKind::Quic | EndpointKind::Relay) {
            continue;
        }
        let relay = ep.kind == EndpointKind::Relay;
        let expanded: Vec<std::net::SocketAddr> = if let Some(addr) = expand_literal(&ep.addr) {
            vec![addr]
        } else {
            let Some((host, port)) = crate::hostdns::split_host_port(&ep.addr)
            else {
                continue;
            };
            let budget = REACH_PARENT_BUDGET.saturating_sub(t0.elapsed());
            crate::hostdns::lookup_host(
                &host,
                budget,
                // Q-F F8e：daemon `host reach` 是用户可见结论面 ⇒ 关键档（不被后台刷新饿死）
                crate::hostdns::ResolveLane::Critical,
            )
                .unwrap_or_default()
                .into_iter()
                .map(|ip| std::net::SocketAddr::new(ip, port))
                .collect()
        };
        for ap in expanded {
            if !eps.iter().any(|(a, _)| *a == ap) {
                eps.push((ap, relay));
            }
        }
    }
    for (addr, is_relay) in eps {
        let tx = tx.clone();
        let budget = REACH_PROBE_BUDGET.min(REACH_PARENT_BUDGET.saturating_sub(t0.elapsed()));
        std::thread::spawn(move || {
            if let Ok(res) = crate::probe::ping_ex(addr, REACH_PAD, budget) {
                let _ = tx.send(ReachTested {
                    ep: addr.to_string(),
                    relay: is_relay,
                    rtt_ms: res.rtt.as_millis() as i64,
                });
            }
        });
    }
    drop(tx);
    let deadline = t0 + REACH_PARENT_BUDGET;
    let mut tested = Vec::new();
    while let Ok(t) = rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
        tested.push(t);
    }
    let has_direct = tested.iter().any(|t| !t.relay);
    let has_relay = tested.iter().any(|t| t.relay);
    let tier = if has_direct {
        vocab::REACH_TIER_DIRECT
    } else if has_relay {
        vocab::REACH_TIER_RELAY
    } else {
        "none"
    };
    // 最优端点 = 命中档（relay 档取中继应答、direct 档取直连应答）里 RTT 最小者。
    let want_relay = tier == vocab::REACH_TIER_RELAY;
    let best = tested
        .iter()
        .filter(|t| t.relay == want_relay)
        .min_by_key(|t| t.rtt_ms)
        .cloned();
    ReachReport {
        tier: tier.to_owned(),
        tested,
        best_ep: best.as_ref().map(|b| b.ep.clone()),
        best_rtt_ms: best.as_ref().map(|b| b.rtt_ms),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("hw-hosts-{tag}-{}-{}", std::process::id(), tag));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn logf() -> Arc<dyn Fn(&str) + Send + Sync> {
        Arc::new(|_: &str| {})
    }

    /// M2 代码门 G3（D1 同类）：`host reach` 的字面量展开归一——mapped 与纯 v4
    /// 同址（去重键/回执 `ep` 串恒为规范形态）；真 v6 原样；非 IP 走域名分支。
    #[test]
    fn reach_literal_normalizes_v4_mapped() {
        let want: std::net::SocketAddr = "127.0.0.1:41641".parse().unwrap();
        assert_eq!(expand_literal("[::ffff:127.0.0.1]:41641"), Some(want));
        assert_eq!(expand_literal("127.0.0.1:41641"), Some(want));
        assert_eq!(
            expand_literal("[2001:db8::1]:41641"),
            Some("[2001:db8::1]:41641".parse().unwrap())
        );
        assert_eq!(expand_literal("home.example.com:41641"), None);
        assert_eq!(expand_literal("nonsense"), None);
    }

    #[test]
    fn carried_and_roundtrip_persist() {
        let dir = tmp_dir("persist");
        let bus = Arc::new(Bus::new());
        let t = HostTable::open(&dir, &dir.join("identity"), &dir.join("eps"), logf(), bus).unwrap();
        // 空表。
        assert!(t.hosts().is_empty());
        // 手写一份含非法 id 的表 → carried 保真写回。
        let recs = vec![HostRecord {
            id: "zz-not-hex".to_owned(),
            name: Some("broken".to_owned()),
            token: "hmw2whatever".to_owned(),
            added_at: 123,
        }];
        std::fs::write(dir.join(HOSTS_FILE_NAME), serde_json::to_string(&recs).unwrap()).unwrap();
        let t2 = HostTable::open(&dir, &dir.join("identity"), &dir.join("eps"), logf(), Arc::new(Bus::new())).unwrap();
        assert!(t2.hosts().is_empty(), "非法 id 不进寻址面");
        // 重新装载后落盘（remove 的 save 路径）不改 carried——直接核对文件仍在。
        let raw = std::fs::read_to_string(dir.join(HOSTS_FILE_NAME)).unwrap();
        let back: Vec<HostRecord> = serde_json::from_str(&raw).unwrap();
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].id, "zz-not-hex");
        let _ = t;
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn corrupt_hosts_backed_up() {
        let dir = tmp_dir("corrupt");
        std::fs::write(dir.join(HOSTS_FILE_NAME), "{ not json").unwrap();
        let t = HostTable::open(&dir, &dir.join("identity"), &dir.join("eps"), logf(), Arc::new(Bus::new()));
        assert!(t.is_ok(), "损坏 = 备份后空表启动（不拒启）");
        let backups: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains("corrupt-"))
            .collect();
        assert_eq!(backups.len(), 1, "原件应被备份");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn add_rejects_bad_token_and_missing() {
        let dir = tmp_dir("add");
        let bus = Arc::new(Bus::new());
        let t = HostTable::open(&dir, &dir.join("identity"), &dir.join("eps"), logf(), bus).unwrap();
        // wire-1 整改：真实路径返回 BadToken（带 detail）——server 层映射 bad_token 码。
        let e = t.add_host("x", "definitely-not-a-token", false).unwrap_err();
        assert!(matches!(e, BackendErr::BadToken(_)), "真实 HostTable 路径应 BadToken（原 Other 假绿）");
        // 死端点 token（合法结构、探测全失败）→ host_unreachable。
        let dead = make_dead_token();
        let e = t.add_host("dead", &dead, false).unwrap_err();
        assert!(matches!(e, BackendErr::HostUnreachable), "全不可达且未 force");
        // force = 跳过探测入表（tier=skipped）。
        let r = t.add_host("dead", &dead, true).unwrap();
        assert_eq!(r.reach.tier, vocab::REACH_TIER_SKIPPED);
        assert!(t.hosts().len() == 1);
        // 同 token 重复添加 → host_exists。
        assert!(matches!(t.add_host("dead", &dead, true), Err(BackendErr::HostExists)));
        // remove 后不在表。
        let id = decode_peer_id(&r.id).unwrap();
        t.remove_host(&id).unwrap();
        assert!(t.hosts().is_empty());
        assert!(matches!(t.remove_host(&id), Err(BackendErr::NoHost)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 构造「结构合法但端点必死」的 token：Direct 端点指向 127.0.0.1:1（恒 RST/无
    /// probe 应答——probe 是 UDP，:1 无服务即无应答）。
    fn make_dead_token() -> String {
        let spec = token::TokenSpec {
            peer_id: &token::PeerId::from([0u8; 32]),
            secret: &token::Secret::from([7u8; 32]),
            endpoints: &[token::EndpointRef::new("127.0.0.1:1", token::EndpointKind::Direct)],
            rpk: None,
        };
        token::encode(&spec).unwrap()
    }
/// Q-H F12（代码门 M5 补测）：坏 token 记录不再静默 return——补日志行 + 状态面
/// `failed/session_not_built`（装载面可查、可行动）。
#[test]
fn bad_token_record_logs_and_reports_failed() {
    use std::sync::Mutex;
    let dir = tmp_dir("f12-badtoken");
    let id = "ab".repeat(32);
    let recs = serde_json::json!([{"id": id, "name": "bad", "token": "不是 token", "addedAt": 1}]);
    std::fs::write(dir.join(HOSTS_FILE_NAME), serde_json::to_string_pretty(&recs).unwrap()).unwrap();
    let logs: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let l2 = Arc::clone(&logs);
    let logf: Arc<dyn Fn(&str) + Send + Sync> =
        Arc::new(move |s: &str| l2.lock().unwrap_or_else(|e| e.into_inner()).push(s.to_owned()));
    let t = HostTable::open(&dir, &dir.join("identity"), &dir.join("eps"), logf, Arc::new(Bus::new()))
        .unwrap();
    let joined = logs.lock().unwrap_or_else(|e| e.into_inner()).join("\n");
    assert!(
        joined.contains("会话构造失败：台账 token 解码失败"),
        "坏 token 必须记行（修前静默 return）：{joined}"
    );
    let st = t.states();
    assert_eq!(st.len(), 1, "记录仍在表（不因构造失败丢登记）");
    assert_eq!(st[0].state, "failed");
    assert_eq!(st[0].reason.as_deref(), Some("session_not_built"));
    let _ = std::fs::remove_dir_all(&dir);
}

}
