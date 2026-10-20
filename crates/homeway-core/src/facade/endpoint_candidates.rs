//! VPN/服务会话共用的端点线索表。线索绝不是身份凭据：每次连接仍必须完成 RPK/准入。
use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::token::{EndpointKind, Token};
use homeway_quic::{Candidate, Via};
use serde::{Deserialize, Serialize};

const CAP: usize = 64;
const FILE_CAP: u64 = 64 * 1024;
const HINT_TTL: u64 = 300;
const DNS_TTL: u64 = 600;
const VERIFIED_TTL: u64 = 7 * 86400;
const WORKERS: usize = 8;
static ACTIVE: AtomicUsize = AtomicUsize::new(0);
static SERIAL: AtomicUsize = AtomicUsize::new(0);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Source {
    Token,
    Hint,
    Probe,
    Inband,
    VerifiedPath,
}

#[derive(Clone, Serialize, Deserialize)]
struct Entry {
    addr: SocketAddr,
    relay: bool,
    source: Source,
    /// DNS来源的token原条目；字面量与动态线索为空。
    domain: Option<String>,
    expires: u64,
    verified: u64,
    failures: u8,
    retry: u64,
}
impl Entry {
    fn candidate(&self, label: [u8; 8]) -> Candidate {
        Candidate {
            addr: self.addr,
            via: if self.relay {
                Via::Relay { label }
            } else {
                Via::Direct
            },
        }
    }
    fn same(&self, c: Candidate) -> bool {
        self.addr == c.addr && self.relay == relay(c)
    }
}
#[derive(Clone, Serialize, Deserialize)]
struct Cooldown {
    addr: SocketAddr,
    relay: bool,
    failures: u8,
    retry: u64,
}
#[derive(Default, Serialize, Deserialize)]
struct Cache {
    entries: Vec<Entry>,
    #[serde(default)]
    cooldowns: Vec<Cooldown>,
}

pub(crate) struct EndpointCandidates {
    entries: Mutex<Vec<Entry>>,
    restored_failures: Vec<Cooldown>,
    domains: Mutex<Vec<(String, bool)>>,
    literals: Mutex<Vec<(SocketAddr, bool)>>,
    path: Option<PathBuf>,
    sinks: Mutex<Vec<homeway_quic::IslandTx>>,
    label: [u8; 8],
    busy: AtomicBool,
    dirty: AtomicBool,
    /// refresh(true)撞上写盘/DNS单飞时合并一轮请求，不能静默丢失恢复解析。
    pending_dns: AtomicBool,
    /// 首次纯域名启动沿使用Critical解析池，不能被后台刷新占满的池饿死。
    pending_critical: AtomicBool,
    /// 最近真实采纳路径优先于旧验证缓存；候选洪水不能挤掉现任。
    current: Mutex<Option<Candidate>>,
    probe_cursor: AtomicUsize,
    #[cfg(test)]
    worker_denials: AtomicUsize,
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
fn relay(c: Candidate) -> bool {
    matches!(c.via, Via::Relay { .. })
}
fn canonical(a: SocketAddr) -> SocketAddr {
    if let IpAddr::V6(ip) = a.ip() {
        if let Some(v4) = ip.to_ipv4_mapped() {
            return SocketAddr::new(IpAddr::V4(v4), a.port());
        }
    }
    a
}
fn valid(a: SocketAddr) -> bool {
    let a = canonical(a);
    a.port() != 0
        && !a.ip().is_unspecified()
        && !a.ip().is_multicast()
        && !matches!(a.ip(), IpAddr::V4(v) if v.is_broadcast())
}

impl EndpointCandidates {
    pub(crate) fn open(tok: &Token, dir: Option<&Path>) -> Arc<Self> {
        static TABLES: OnceLock<Mutex<HashMap<PathBuf, Weak<EndpointCandidates>>>> =
            OnceLock::new();
        let path = dir.map(|d| {
            d.join(format!(
                "{}.json",
                tok.peer_id
                    .as_bytes()
                    .iter()
                    .map(|x| format!("{x:02x}"))
                    .collect::<String>()
            ))
        });
        let mut registry = TABLES
            .get_or_init(Default::default)
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        registry.retain(|_, v| v.strong_count() != 0);
        if let Some(existing) = path
            .as_ref()
            .and_then(|p| registry.get(p))
            .and_then(Weak::upgrade)
        {
            existing.add_token(tok);
            return existing;
        }
        let cache = path
            .as_ref()
            .and_then(|p| read_cache(p).ok())
            .unwrap_or_default();
        let mut entries = cache.entries;
        for e in &mut entries {
            if let Some(c) = cache
                .cooldowns
                .iter()
                .find(|c| c.addr == e.addr && c.relay == e.relay)
            {
                e.failures = c.failures;
                e.retry = c.retry;
            }
        }
        let this = Arc::new(Self {
            entries: Mutex::new(entries),
            restored_failures: cache.cooldowns,
            sinks: Mutex::new(Vec::new()),
            domains: Mutex::new(Vec::new()),
            literals: Mutex::new(Vec::new()),
            path: path.clone(),
            label: crate::legframe::relay_id(tok.peer_id.as_bytes()),
            busy: AtomicBool::new(false),
            dirty: AtomicBool::new(false),
            pending_dns: AtomicBool::new(false),
            pending_critical: AtomicBool::new(false),
            current: Mutex::new(None),
            probe_cursor: AtomicUsize::new(0),
            #[cfg(test)]
            worker_denials: AtomicUsize::new(0),
        });
        this.add_token(tok);
        if let Some(p) = path {
            registry.insert(p, Arc::downgrade(&this));
        }
        this
    }

    fn add_token(&self, tok: &Token) {
        let mut literals = Vec::new();
        let mut domains = Vec::new();
        for e in tok.endpoints.iter().take(CAP) {
            let is_relay = match e.kind {
                EndpointKind::Quic => false,
                EndpointKind::Relay => true,
                _ => continue,
            };
            if let Ok(addr) = e.addr.parse::<SocketAddr>() {
                let addr = canonical(addr);
                if valid(addr) && !literals.contains(&(addr, is_relay)) {
                    literals.push((addr, is_relay));
                }
            } else if crate::hostdns::split_host_port(&e.addr).is_some()
                && !domains.contains(&(e.addr.clone(), is_relay)) {
                domains.push((e.addr.clone(), is_relay));
            }
        }
        // 同peer换token替换授权目录；旧token的未验证地址不能永久占槽。
        {
            let mut configured = self.domains.lock().unwrap_or_else(|e| e.into_inner());
            *configured = domains.clone();
            *self.literals.lock().unwrap_or_else(|e| e.into_inner()) = literals.clone();
        }
        self.entries.lock().unwrap_or_else(|e| e.into_inner()).retain_mut(|e| {
            if e.source != Source::Token { return true; }
            let retained = literals.contains(&(e.addr, e.relay))
                || e.domain.as_ref().is_some_and(|d| domains.contains(&(d.clone(), e.relay)));
            if retained { return true; }
            if e.verified == 0 { return false; }
            e.source = Source::VerifiedPath;
            e.domain = None;
            e.expires = e.verified.saturating_add(VERIFIED_TTL);
            true
        });
        for (addr, is_relay) in literals {
            self.insert(Candidate { addr, via: if is_relay { Via::Relay { label: self.label } } else { Via::Direct } },
                Source::Token, None, u64::MAX);
        }
    }

    fn insert(&self, c: Candidate, source: Source, domain: Option<String>, expires: u64) {
        let c = Candidate {
            addr: canonical(c.addr),
            ..c
        };
        if !valid(c.addr) {
            return;
        }
        let mut list = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        let n = now();
        list.retain(|e| e.expires > n);
        if let Some(e) = list.iter_mut().find(|e| e.same(c)) {
            // 低信任线索不能续命已认证记录，也不能覆盖token来源。
            if source == Source::Token || (e.source != Source::Token && e.verified == 0) {
                e.source = source;
                e.domain = domain;
            }
            if source == Source::Token || e.verified == 0 {
                e.expires = e.expires.max(expires);
            }
            return;
        }
        if list.len() >= CAP {
            let pinned = *self.current.lock().unwrap_or_else(|e| e.into_inner());
            let victim = list.iter().enumerate()
                .filter(|(_, e)| e.source != Source::Token && (source == Source::Token || e.verified == 0))
                // 当前token优先于旧缓存；优先换掉未验证项，其次最旧认证项，现任最后。
                .min_by_key(|(_, e)| (pinned.is_some_and(|c| e.same(c)), e.verified, e.expires))
                .map(|(i, _)| i);
            if let Some(i) = victim {
                list.remove(i);
            } else {
                return;
            }
        }
        let cooldown = self
            .restored_failures
            .iter()
            .find(|e| e.addr == c.addr && e.relay == relay(c) && e.retry > n);
        list.push(Entry {
            addr: c.addr,
            relay: relay(c),
            source,
            domain,
            expires,
            verified: 0,
            failures: cooldown.map(|e| e.failures).unwrap_or_default(),
            retry: cooldown.map(|e| e.retry).unwrap_or_default(),
        });
    }

    pub(crate) fn learn(&self, source: Source, cands: &[Candidate]) {
        for &c in cands.iter().take(CAP) {
            if matches!(source, Source::Probe | Source::Hint)
                && (!valid(c.addr) || !crate::probe::probe_addr_acceptable(&c.addr)) { continue; }
            self.insert(c, source, None, now() + HINT_TTL);
        }
    }

    /// bool=true的唯一调用点必须是TLS钉定+准入完成的采纳沿。
    pub(crate) fn outcome(self: &Arc<Self>, cand: Candidate, authenticated: bool) {
        let cand = Candidate {
            addr: canonical(cand.addr),
            ..cand
        };
        if !valid(cand.addr) {
            return;
        }
        let mut list = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        // 在途候选可能已被TTL/新线索淘汰；真实认证成功仍必须记回，不能让线索洪水抹掉胜者。
        if authenticated && !list.iter().any(|e| e.same(cand)) {
            if list.len() >= CAP {
                let victim = list
                    .iter()
                    .enumerate()
                    .filter(|(_, e)| e.source != Source::Token)
                    .min_by_key(|(_, e)| e.verified)
                    .map(|(i, _)| i);
                if let Some(i) = victim {
                    list.remove(i);
                }
            }
            if list.len() < CAP {
                list.push(Entry {
                    addr: cand.addr,
                    relay: relay(cand),
                    source: Source::VerifiedPath,
                    domain: None,
                    expires: now() + VERIFIED_TTL,
                    verified: 0,
                    failures: 0,
                    retry: 0,
                });
            }
        }
        if let Some(e) = list.iter_mut().find(|e| e.same(cand)) {
            if authenticated {
                e.verified = now();
                e.failures = 0;
                e.retry = 0;
                e.expires = e.expires.max(now() + VERIFIED_TTL);
                *self.current.lock().unwrap_or_else(|e| e.into_inner()) = Some(cand);
                self.dirty.store(true, Ordering::Release);
            } else {
                e.failures = e.failures.saturating_add(1);
                e.retry = now() + (1u64 << e.failures.min(6));
                self.dirty.store(true, Ordering::Release);
            }
        }
        drop(list);
        self.refresh(false);
    }

    pub(crate) fn candidates(&self) -> Vec<Candidate> {
        let n = now();
        let mut list = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        list.retain(|e| e.expires > n);
        // 退避必须实际减少拨号：冷却项不投；全表冷却时只保留最早到期的一项，避免删空。
        let mut refs: Vec<_> = list.iter().filter(|e| e.retry <= n).collect();
        if refs.is_empty() {
            if let Some(first) = list.iter().min_by_key(|e| (e.retry, e.relay)) {
                refs.push(first);
            }
        }
        refs.sort_by_key(|e| (e.relay, std::cmp::Reverse(e.verified), e.failures));
        refs.into_iter().map(|e| e.candidate(self.label)).collect()
    }

    pub(crate) fn bind(&self, island: &homeway_quic::Island) {
        let cands = self.candidates();
        let mut sinks = self.sinks.lock().unwrap_or_else(|e| e.into_inner());
        sinks.retain(|tx| {
            tx.send(homeway_quic::Cmd::SetCandidates {
                cands: cands.clone(),
            })
            .is_ok()
        });
        if sinks.len() == 8 {
            sinks.remove(0);
        }
        sinks.push(island.tx());
    }

    pub(crate) fn initial(self: &Arc<Self>) -> Vec<Candidate> {
        self.initial_with_budget(crate::hostdns::PROBE_SYNC_BUDGET)
    }

    fn initial_with_budget(self: &Arc<Self>, budget: Duration) -> Vec<Candidate> {
        let deadline = Instant::now() + budget;
        if self.candidates().is_empty() { self.pending_critical.store(true, Ordering::Release); }
        self.refresh(true);
        // 首次无候选时，额度暂满不是解析完成；在同一总预算内短拍重试单飞槽。
        // pending_dns只置一次，重试不制造新的刷新请求；已有候选/恢复绝不等待。
        loop {
            let cands = self.candidates();
            if !cands.is_empty() || Instant::now() >= deadline { return cands; }
            if !self.busy.load(Ordering::Acquire) {
                if !self.pending_dns.load(Ordering::Acquire) { return cands; }
                self.refresh(false);
            }
            std::thread::sleep(Duration::from_millis(5).min(deadline.saturating_duration_since(Instant::now())));
        }
    }

    pub(crate) fn hooks(self: &Arc<Self>) -> homeway_quic::CandidateHooks {
        let r = Arc::clone(self);
        let s = Arc::clone(self);
        let l = Arc::clone(self);
        let o = Arc::clone(self);
        homeway_quic::CandidateHooks {
            snapshot: Arc::new(move || s.candidates()),
            refresh: Arc::new(move || {
                r.refresh(true);
                r.candidates()
            }),
            learn: Arc::new(move |c, source| {
                let source = match source {
                    homeway_quic::CandidateSource::Hint => Source::Hint,
                    homeway_quic::CandidateSource::Probe => Source::Probe,
                    homeway_quic::CandidateSource::Inband => Source::Inband,
                };
                l.learn(source, &[c]);
            }),
            outcome: Arc::new(move |c, ok| o.outcome(c, ok)),
        }
    }

    /// 单飞短命后台任务；DNS总预算固定，不阻塞调用方，更不延长恢复动作预算。
    pub(crate) fn refresh(self: &Arc<Self>, dns: bool) {
        if dns { self.pending_dns.store(true, Ordering::Release); }
        if !self.dirty.load(Ordering::Acquire) && !self.pending_dns.load(Ordering::Acquire) {
            return;
        }
        if self.busy.swap(true, Ordering::AcqRel) {
            return;
        }
        #[cfg(test)]
        let unavailable = self.worker_denials.try_update(Ordering::AcqRel, Ordering::Acquire,
            |n| n.checked_sub(1)).is_ok();
        #[cfg(not(test))]
        let unavailable = false;
        if unavailable || ACTIVE
            .try_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                (n < WORKERS).then_some(n + 1)
            })
            .is_err()
        {
            self.busy.store(false, Ordering::Release);
            return;
        }
        let this = Arc::clone(self);
        let spawn = std::thread::Builder::new()
            .name("homeway-endpoint-refresh".into())
            .spawn(move || {
                struct Done(Arc<EndpointCandidates>);
                impl Drop for Done {
                    fn drop(&mut self) {
                        self.0.busy.store(false, Ordering::Release);
                        ACTIVE.fetch_sub(1, Ordering::AcqRel);
                        // 成功沿可恰在save交换dirty之后到达；归还单飞槽后承接，避免丢最终落盘。
                        if self.0.dirty.load(Ordering::Acquire) || self.0.pending_dns.load(Ordering::Acquire) {
                            self.0.refresh(false);
                        }
                    }
                }
                let _done = Done(Arc::clone(&this));
                let dns = this.pending_dns.swap(false, Ordering::AcqRel);
                if !dns {
                    std::thread::sleep(Duration::from_millis(50));
                } // 合并一轮成功/失败沿，避免每候选刷盘。
                if dns {
                    let lane = if this.pending_critical.swap(false, Ordering::AcqRel) {
                        crate::hostdns::ResolveLane::Critical
                    } else { crate::hostdns::ResolveLane::Background };
                    let deadline = Instant::now() + crate::hostdns::PROBE_SYNC_BUDGET;
                    let domains = this
                        .domains
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .clone();
                    for (domain, is_relay) in domains {
                        let left = deadline.saturating_duration_since(Instant::now());
                        if left.is_zero() {
                            break;
                        }
                        let Some((host, port)) = crate::hostdns::split_host_port(&domain) else {
                            continue;
                        };
                        if let Ok(ips) = crate::hostdns::lookup_host(
                            &host,
                            left,
                            lane,
                        ) {
                            this.apply_dns(&domain, is_relay, port, ips);
                        }
                    }
                }
                if dns {
                    let deadline = Instant::now() + Duration::from_millis(250);
                    let targets = this.probe_targets();
                    for (index, &addr) in targets.iter().enumerate() {
                        let left = deadline.saturating_duration_since(Instant::now());
                        if left.is_zero() { break; }
                        // 两个目标公平分享本轮剩余预算，首个黑洞不能永久饿死第二个。
                        let left = left / (targets.len() - index) as u32;
                        if let Ok(reply) =
                            crate::probe::ping_ex(addr, crate::probe::PROBE_PAD, left)
                        {
                            let cands: Vec<_> = reply
                                .endpoints
                                .into_iter()
                                .map(|addr| Candidate {
                                    addr,
                                    via: Via::Direct,
                                })
                                .collect();
                            this.learn(Source::Probe, &cands);
                        }
                    }
                }
                let cands = this.candidates();
                this.sinks
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .retain(|tx| {
                        tx.send(homeway_quic::Cmd::SetCandidates {
                            cands: cands.clone(),
                        })
                        .is_ok()
                    });
                // 不持内存锁做IO；同一表只有这一个写者，原子替换不暴露半文件。
                if this.dirty.swap(false, Ordering::AcqRel) {
                    let _ = this.save();
                }
            });
        if spawn.is_err() {
            self.busy.store(false, Ordering::Release);
            ACTIVE.fetch_sub(1, Ordering::AcqRel);
        }
    }

    /// 有效直连候选公平轮转，单轮最多两个，总探测预算250ms；中继不是出口探测目标。
    fn probe_targets(&self) -> Vec<SocketAddr> {
        let roster: Vec<_> = self.candidates().into_iter().filter(|c| !relay(*c)).map(|c| c.addr).collect();
        if roster.is_empty() { return Vec::new(); }
        let count = roster.len().min(2);
        let start = self.probe_cursor.fetch_add(count, Ordering::Relaxed) % roster.len();
        (0..count).map(|i| roster[(start + i) % roster.len()]).collect()
    }

    fn apply_dns(&self, domain: &str, is_relay: bool, port: u16, ips: Vec<IpAddr>) {
        let domains = self.domains.lock().unwrap_or_else(|e| e.into_inner());
        if !domains.iter().any(|(d, r)| d == domain && *r == is_relay) { return; }
        let literals = self.literals.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let addrs: Vec<_> = ips
            .into_iter()
            .take(CAP)
            .map(|ip| canonical(SocketAddr::new(ip, port)))
            .filter(|a| valid(*a))
            .collect();
        if addrs.is_empty() {
            return;
        } // 解析失败/空答不能清掉最后可用的旧结果。
          // 成功解析的新快照取代未验证旧结果；曾认证地址留至其TTL（DNS漂移可回落）。
        self.entries.lock().unwrap_or_else(|e| e.into_inner()).retain_mut(|e| {
            if e.domain.as_deref() != Some(domain) || e.relay != is_relay || addrs.contains(&e.addr) { return true; }
            if literals.contains(&(e.addr, e.relay)) {
                e.source = Source::Token;
                e.domain = None;
                e.expires = u64::MAX;
                return true;
            }
            if e.verified == 0 { return false; }
            // 旧DNS快照是历史回落缓存，不再占当前Token来源的不可淘汰槽。
            e.source = Source::VerifiedPath;
            e.domain = None;
            e.expires = e.verified.saturating_add(VERIFIED_TTL);
            true
        });
        for addr in addrs {
            let literal = literals.contains(&(addr, is_relay));
            self.insert(
                Candidate { addr, via: if is_relay { Via::Relay { label: self.label } } else { Via::Direct } },
                Source::Token,
                (!literal).then(|| domain.to_owned()),
                if literal { u64::MAX } else { now() + DNS_TTL },
            );
        }
    }

    fn save(&self) -> std::io::Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let Some(parent) = path.parent() else {
            return Ok(());
        };
        fs::create_dir_all(parent)?;
        // token字面量不是学习缓存；避免旧token地址永久留存。
        let snapshot = self
            .entries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let entries = snapshot
            .iter()
            .filter(|e| e.verified != 0 && e.expires > now())
            .cloned()
            .map(|mut e| {
                e.expires = e.verified.saturating_add(VERIFIED_TTL);
                e
            })
            .collect();
        let cooldowns = snapshot
            .iter()
            .filter(|e| e.retry > now())
            .map(|e| Cooldown {
                addr: e.addr,
                relay: e.relay,
                failures: e.failures,
                retry: e.retry,
            })
            .collect();
        let data = serde_json::to_vec(&Cache { entries, cooldowns })?;
        let tmp = path.with_extension(format!(
            "tmp-{}-{}",
            std::process::id(),
            SERIAL.fetch_add(1, Ordering::Relaxed)
        ));
        let mut opts = OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let result = (|| {
            let mut f = opts.open(&tmp)?;
            f.write_all(&data)?;
            f.sync_all()?;
            fs::rename(&tmp, path)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&tmp);
        }
        result
    }
}

fn read_cache(path: &Path) -> std::io::Result<Cache> {
    let f = fs::File::open(path)?;
    if f.metadata()?.len() > FILE_CAP {
        return Ok(Cache::default());
    }
    let mut bytes = Vec::new();
    f.take(FILE_CAP + 1).read_to_end(&mut bytes)?;
    let cache: Cache = serde_json::from_slice(&bytes)?;
    let entries = cache
        .entries
        .into_iter()
        .take(CAP)
        .map(|mut e| {
            e.addr = canonical(e.addr);
            if e.retry > now().saturating_add(64) {
                e.retry = 0;
                e.failures = 0;
            }
            e
        })
        .filter(|e| {
            valid(e.addr)
                && e.verified != 0
                && e.verified <= now()
                && e.expires > now()
                && e.expires <= e.verified.saturating_add(VERIFIED_TTL)
        })
        .collect();
    // 冷却仅是元数据，绝不据此复活未验证hint地址；最大64s，拒绝文件伪造永久封禁。
    let cooldowns = cache
        .cooldowns
        .into_iter()
        .take(CAP)
        .map(|mut e| {
            e.addr = canonical(e.addr);
            e
        })
        .filter(|e| {
            valid(e.addr)
                && e.failures != 0
                && e.retry > now()
                && e.retry <= now().saturating_add(64)
        })
        .collect();
    Ok(Cache { entries, cooldowns })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::token::{EndpointRef, PeerId, Secret, TokenSpec};

    fn token(endpoints: &[EndpointRef<'_>]) -> Token {
        let peer_id = PeerId::from([41; 32]);
        let secret = Secret::from([42; 32]);
        crate::token::decode(
            &crate::token::encode(&TokenSpec {
                peer_id: &peer_id,
                secret: &secret,
                endpoints,
                rpk: None,
            })
            .unwrap(),
        )
        .unwrap()
    }
    fn cand(port: u16) -> Candidate {
        Candidate {
            addr: SocketAddr::from(([8, 8, 8, 8], port)),
            via: Via::Direct,
        }
    }
    fn empty() -> Arc<EndpointCandidates> {
        EndpointCandidates::open(&token(&[]), None)
    }
    fn idle(c: &EndpointCandidates) {
        let deadline = Instant::now() + Duration::from_secs(4);
        while c.busy.load(Ordering::Acquire) {
            assert!(Instant::now() < deadline, "后台任务应有界退出");
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    struct Temp(PathBuf);
    impl Temp {
        fn new() -> Self {
            Self(std::env::temp_dir().join(format!(
                "hw-candidates-{}-{}",
                std::process::id(),
                SERIAL.fetch_add(1, Ordering::Relaxed)
            )))
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn untrusted_hints_do_not_become_verified_or_survive_restart() {
        let dir = Temp::new();
        let t = token(&[]);
        let c = EndpointCandidates::open(&t, Some(&dir.0));
        c.learn(Source::Hint, &[cand(48001)]);
        c.save().unwrap();
        assert_eq!(c.entries.lock().unwrap()[0].verified, 0);
        drop(c);
        assert!(EndpointCandidates::open(&t, Some(&dir.0))
            .candidates()
            .is_empty());
    }

    #[test]
    fn authenticated_success_persists_and_both_modes_share_peer_table() {
        let dir = Temp::new();
        let t = token(&[]);
        let c = EndpointCandidates::open(&t, Some(&dir.0));
        let service = EndpointCandidates::open(&t, Some(&dir.0));
        assert!(Arc::ptr_eq(&c, &service));
        c.learn(Source::Probe, &[cand(48002)]);
        c.outcome(cand(48002), true);
        idle(&c);
        let path = c.path.clone().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        drop(service);
        drop(c);
        let restored = EndpointCandidates::open(&t, Some(&dir.0));
        assert_eq!(restored.candidates(), vec![cand(48002)]);
        assert!(restored.entries.lock().unwrap()[0].verified > 0);
        let data = fs::read_to_string(path).unwrap();
        assert!(!data.contains("secret"));
    }

    #[test]
    fn failures_cool_down_actual_attempts_but_never_delete_all_candidates() {
        let c = empty();
        c.learn(Source::Hint, &[cand(48003), cand(48004)]);
        c.outcome(cand(48003), false);
        assert_eq!(c.candidates(), vec![cand(48004)], "实际投出候选数由2减至1");
        for _ in 0..100 {
            c.outcome(cand(48004), false);
        }
        assert_eq!(
            c.candidates(),
            vec![cand(48003)],
            "全表冷却保留最早到期一项"
        );
        for e in c.entries.lock().unwrap().iter_mut() {
            e.retry = now().saturating_sub(1);
        }
        assert_eq!(c.candidates().len(), 2, "冷却到期候选重入赛跑");
        let list = c.entries.lock().unwrap();
        assert!(list
            .iter()
            .all(|e| e.verified == 0 && e.retry <= now() + 64));
    }

    #[test]
    fn failed_token_candidate_remains_cooling_after_drop_and_reopen() {
        let dir = Temp::new();
        let t = token(&[
            EndpointRef {
                kind: EndpointKind::Quic,
                addr: "8.8.8.8:48013",
            },
            EndpointRef {
                kind: EndpointKind::Quic,
                addr: "8.8.8.8:48014",
            },
        ]);
        let c = EndpointCandidates::open(&t, Some(&dir.0));
        for _ in 0..6 {
            c.outcome(cand(48013), false);
        }
        c.learn(Source::Hint, &[cand(48015)]);
        c.outcome(cand(48015), false);
        idle(&c);
        drop(c);
        let reopened = EndpointCandidates::open(&t, Some(&dir.0));
        assert_eq!(
            reopened.candidates(),
            vec![cand(48014)],
            "失败token重开后仍冷却，未验证hint不因元数据复活"
        );
        let list = reopened.entries.lock().unwrap();
        assert_eq!(
            list.iter().find(|e| e.same(cand(48013))).unwrap().failures,
            6
        );
    }

    #[test]
    fn admitted_candidate_is_recovered_even_if_hint_flood_evicted_it_inflight() {
        let c = empty();
        c.learn(Source::Hint, &[cand(48010)]);
        for port in 49000..49200 {
            c.learn(Source::Hint, &[cand(port)]);
        }
        assert!(!c.candidates().contains(&cand(48010)));
        c.outcome(cand(48010), true);
        idle(&c);
        assert_eq!(
            c.entries
                .lock()
                .unwrap()
                .iter()
                .find(|e| e.same(cand(48010)))
                .unwrap()
                .source,
            Source::VerifiedPath
        );
        assert!(c.candidates().contains(&cand(48010)));
        assert!(c
            .entries
            .lock()
            .unwrap()
            .iter()
            .any(|e| e.same(cand(48010)) && e.verified > 0));
    }

    #[test]
    fn low_trust_hint_cannot_downgrade_or_extend_the_accepted_path() {
        let c = empty();
        c.learn(Source::Probe, &[cand(48011)]);
        c.outcome(cand(48011), true);
        idle(&c);
        let before = c.entries.lock().unwrap()[0].clone();
        c.learn(Source::Hint, &[cand(48011)]);
        for port in 49000..49200 {
            c.learn(Source::Hint, &[cand(port)]);
        }
        let list = c.entries.lock().unwrap();
        let accepted = list
            .iter()
            .find(|e| e.same(cand(48011)))
            .expect("采纳地址不可被hint容量洪水挤掉");
        assert_eq!(accepted.source, Source::Probe);
        assert_eq!(accepted.expires, before.expires);
        assert_eq!(accepted.verified, before.verified);
    }

    #[test]
    fn ttl_and_address_validation_bound_pollution_without_evicting_verified() {
        let c = empty();
        c.learn(Source::Hint, &[cand(48005)]);
        c.outcome(cand(48005), true);
        idle(&c);
        for p in 49000..49200 {
            c.learn(Source::Hint, &[cand(p)]);
        }
        for a in [
            "0.0.0.0:1",
            "[::]:1",
            "224.0.0.1:1",
            "255.255.255.255:1",
            "[ff02::1]:1",
            "[::ffff:224.0.0.1]:1",
            "[::ffff:0.0.0.0]:1",
            "127.0.0.1:0",
        ] {
            c.learn(
                Source::Probe,
                &[Candidate {
                    addr: a.parse().unwrap(),
                    via: Via::Direct,
                }],
            );
        }
        assert_eq!(c.candidates().len(), CAP);
        assert!(c.candidates().contains(&cand(48005)));
        for e in c.entries.lock().unwrap().iter_mut() {
            if e.verified == 0 {
                e.expires = now() - 1;
            }
        }
        assert_eq!(c.candidates(), vec![cand(48005)]);
        assert!(valid("192.168.1.2:41641".parse().unwrap()), "LAN合法");
    }

    #[test]
    fn dns_drift_replaces_unverified_and_failure_preserves_last_result() {
        let c = empty();
        c.domains.lock().unwrap().push(("exit.test:41641".to_owned(), false));
        c.apply_dns(
            "exit.test:41641",
            false,
            41641,
            vec!["192.0.2.1".parse().unwrap()],
        );
        c.apply_dns("exit.test:41641", false, 41641, Vec::new());
        assert_eq!(
            c.candidates()[0].addr.ip(),
            "192.0.2.1".parse::<IpAddr>().unwrap()
        );
        c.apply_dns(
            "exit.test:41641",
            false,
            41641,
            vec!["2001:db8::2".parse().unwrap()],
        );
        assert_eq!(c.candidates().len(), 1);
        let old = c.candidates()[0];
        c.outcome(old, true);
        idle(&c);
        c.apply_dns(
            "exit.test:41641",
            false,
            41641,
            vec!["192.0.2.3".parse().unwrap()],
        );
        assert!(c.candidates().contains(&old), "已认证旧地址保留至TTL供回落");
        assert_eq!(c.candidates().len(), 2);
    }

    #[test]
    fn startup_resolves_domains_and_refresh_never_blocks_existing_candidates() {
        let t = token(&[EndpointRef {
            kind: EndpointKind::Quic,
            addr: "localhost:48006",
        }]);
        let c = EndpointCandidates::open(&t, None);
        let initial = c.initial();
        assert!(!initial.is_empty(), "域名token启动可用");
        assert!(initial.iter().all(|e| e.addr.ip().is_loopback()));
        let before = Instant::now();
        for _ in 0..100 {
            let _ = (c.hooks().refresh)();
        }
        assert!(
            before.elapsed() < Duration::from_millis(100),
            "恢复轮取候选不等待DNS"
        );
        idle(&c);
    }

    #[test]
    fn production_probe_endpoint_list_is_learned_but_not_authenticated() {
        let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(4)))
            .unwrap();
        let addr = socket.local_addr().unwrap().to_string();
        let t = token(&[EndpointRef {
            kind: EndpointKind::Quic,
            addr: &addr,
        }]);
        let responder = std::thread::spawn(move || {
            let mut b = [0u8; 1024];
            let (n, peer) = socket.recv_from(&mut b).unwrap();
            let response =
                crate::probe::respond_ex(&b[..n], "test", 0, &[cand(48007).addr]).unwrap();
            socket.send_to(&response, peer).unwrap();
        });
        let c = EndpointCandidates::open(&t, None);
        // 全局worker额度可被其它并行测试合法占满；模拟后续观测事件重投，
        // 不能把“本次未取得槽位”当成“探测已完成”。不改产品额度/单飞语义。
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            c.refresh(true);
            idle(&c);
            if c.entries.lock().unwrap().iter().any(|e| e.addr == cand(48007).addr) {
                break;
            }
            assert!(Instant::now() < deadline, "后续观测应在有空闲worker后完成取样");
            std::thread::sleep(Duration::from_millis(5));
        }
        responder.join().unwrap();
        let entries = c.entries.lock().unwrap();
        let learned = entries
            .iter()
            .find(|e| e.addr == cand(48007).addr)
            .expect("真实probe回复列表进入共享表");
        assert_eq!(learned.source, Source::Probe);
        assert_eq!(learned.verified, 0);
    }

    #[test]
    fn malformed_oversized_and_expired_cache_never_override_token() {
        let dir = Temp::new();
        fs::create_dir_all(&dir.0).unwrap();
        let t = token(&[EndpointRef {
            kind: EndpointKind::Quic,
            addr: "8.8.8.8:48008",
        }]);
        let c = EndpointCandidates::open(&t, Some(&dir.0));
        let path = c.path.clone().unwrap();
        drop(c);
        for bytes in [b"not-json".to_vec(), vec![b' '; FILE_CAP as usize + 1]] {
            fs::write(&path, bytes).unwrap();
            let restored = EndpointCandidates::open(&t, Some(&dir.0));
            assert_eq!(restored.candidates(), vec![cand(48008)]);
            drop(restored);
        }
        let mut expired = Entry {
            addr: cand(48009).addr,
            relay: false,
            source: Source::Hint,
            domain: None,
            expires: now() - 1,
            verified: now() - 2,
            failures: 0,
            retry: 0,
        };
        fs::write(
            &path,
            serde_json::to_vec(&Cache {
                entries: vec![expired.clone()],
                ..Cache::default()
            })
            .unwrap(),
        )
        .unwrap();
        assert!(read_cache(&path).unwrap().entries.is_empty());
        expired.expires = u64::MAX;
        fs::write(
            &path,
            serde_json::to_vec(&Cache {
                entries: vec![expired],
                ..Cache::default()
            })
            .unwrap(),
        )
        .unwrap();
        assert!(
            read_cache(&path).unwrap().entries.is_empty(),
            "缓存不能自称永久已验证"
        );
    }
    #[test]
    fn current_token_replaces_stale_directory_and_beats_full_verified_cache() {
        let c = empty();
        for port in 50000..50064 {
            c.insert(cand(port), Source::VerifiedPath, None, now() + VERIFIED_TTL);
            c.entries.lock().unwrap().last_mut().unwrap().verified = now();
        }
        *c.current.lock().unwrap() = Some(cand(50063));
        let fresh = token(&[EndpointRef { kind: EndpointKind::Quic, addr: "8.8.8.8:59999" }]);
        c.add_token(&fresh);
        assert!(c.candidates().contains(&cand(59999)), "当前token优先于旧验证缓存");
        assert!(c.candidates().contains(&cand(50063)), "非极限情况下保留现任");
        let newer = token(&[EndpointRef { kind: EndpointKind::Quic, addr: "8.8.8.8:59998" }]);
        c.add_token(&newer);
        assert!(!c.candidates().contains(&cand(59999)), "未验证旧token目录必须被替换");
        assert!(c.candidates().contains(&cand(59998)));
    }

    #[test]
    fn dns_requested_while_writer_busy_is_served_once_after_writer_finishes() {
        let t = token(&[EndpointRef { kind: EndpointKind::Quic, addr: "localhost:59997" }]);
        let c = EndpointCandidates::open(&t, None);
        c.learn(Source::Hint, &[cand(48001)]);
        c.outcome(cand(48001), false);
        assert!(c.busy.load(Ordering::Acquire));
        c.refresh(true);
        let deadline = Instant::now() + Duration::from_secs(4);
        while !c.candidates().iter().any(|c| c.addr.port() == 59997) {
            assert!(Instant::now() < deadline, "写盘worker结束后必须承接DNS请求");
            std::thread::sleep(Duration::from_millis(5));
        }
        idle(&c);
        assert!(!c.pending_dns.load(Ordering::Acquire));
        assert!(!c.busy.load(Ordering::Acquire));
    }

    #[test]
    fn delayed_dns_from_old_token_cannot_reintroduce_removed_domain() {
        let old = token(&[EndpointRef { kind: EndpointKind::Quic, addr: "old.test:48020" }]);
        let c = EndpointCandidates::open(&old, None);
        c.add_token(&token(&[EndpointRef { kind: EndpointKind::Quic, addr: "8.8.8.8:48021" }]));
        c.apply_dns("old.test:48020", false, 48020, vec!["192.0.2.20".parse().unwrap()]);
        assert_eq!(c.candidates(), vec![cand(48021)]);
    }

    #[test]
    fn full_persisted_old_token_cache_cannot_hide_new_token() {
        let dir = Temp::new();
        let c = EndpointCandidates::open(&token(&[]), Some(&dir.0));
        for port in 51000..51064 {
            c.insert(cand(port), Source::Token, None, u64::MAX);
            c.entries.lock().unwrap().last_mut().unwrap().verified = now();
        }
        c.save().unwrap();
        drop(c);
        let fresh = token(&[EndpointRef { kind: EndpointKind::Quic, addr: "8.8.8.8:59996" }]);
        let restored = EndpointCandidates::open(&fresh, Some(&dir.0));
        assert!(restored.candidates().contains(&cand(59996)));
        assert_eq!(restored.entries.lock().unwrap().len(), CAP);
    }

    #[test]
    fn persistent_save_error_does_not_restart_workers_or_retain_table_forever() {
        let dir = Temp::new();
        fs::write(&dir.0, b"parent is a file").unwrap();
        let c = EndpointCandidates::open(&token(&[]), Some(&dir.0));
        c.learn(Source::Hint, &[cand(48022)]);
        c.outcome(cand(48022), true);
        let deadline = Instant::now() + Duration::from_secs(2);
        while c.busy.load(Ordering::Acquire) || c.dirty.load(Ordering::Acquire) {
            assert!(Instant::now() < deadline, "写失败不得自发无限重启");
            c.refresh(false); // 若本轮进程worker额度已满，允许后续外部触发一次。
            std::thread::sleep(Duration::from_millis(5));
        }
        let weak = Arc::downgrade(&c);
        drop(c);
        while weak.strong_count() != 0 {
            assert!(Instant::now() < deadline, "短命worker必须释放table/sink持有");
            std::thread::sleep(Duration::from_millis(5));
        }
        fs::remove_file(&dir.0).unwrap();
    }

    #[test]
    fn domain_only_start_retries_transient_worker_saturation_with_one_pending_request() {
        let t = token(&[EndpointRef { kind: EndpointKind::Quic, addr: "localhost:59995" }]);
        let c = EndpointCandidates::open(&t, None);
        c.worker_denials.store(3, Ordering::Release);
        let start = Instant::now();
        let initial = c.initial();
        assert!(initial.iter().any(|c| c.addr.port() == 59995));
        assert!(start.elapsed() >= Duration::from_millis(5), "必须真正经历额度暂满后的重试");
        idle(&c);
        assert!(!c.pending_dns.load(Ordering::Acquire));
        assert_eq!(c.worker_denials.load(Ordering::Acquire), 0);
    }

    #[test]
    fn persistent_worker_saturation_keeps_original_startup_deadline() {
        let t = token(&[EndpointRef { kind: EndpointKind::Quic, addr: "localhost:59994" }]);
        let c = EndpointCandidates::open(&t, None);
        c.worker_denials.store(usize::MAX, Ordering::Release);
        let start = Instant::now();
        assert!(c.initial_with_budget(Duration::from_millis(50)).is_empty());
        assert!(start.elapsed() < Duration::from_millis(200));
        assert!(!c.busy.load(Ordering::Acquire));
        assert!(c.pending_dns.load(Ordering::Acquire));
    }

    #[test]
    fn cold_domain_start_uses_critical_lane_when_background_pool_is_full() {
        let limiter = crate::hostdns::process_limiter();
        let mut held = Vec::new();
        for _ in 0..4 { held.push(limiter.acquire(crate::hostdns::ResolveLane::Background, Duration::from_secs(2)).unwrap()); }
        let t = token(&[EndpointRef { kind: EndpointKind::Quic, addr: "localhost:59993" }]);
        let c = EndpointCandidates::open(&t, None);
        let initial = c.initial();
        assert!(initial.iter().any(|c| c.addr.port() == 59993), "后台池占满不能使冷启动解析失败");
        drop(held);
        idle(&c);
    }

    #[test]
    fn current_dns_snapshot_beats_old_verified_dns_without_downgrading_literal_token() {
        let t = token(&[
            EndpointRef { kind: EndpointKind::Quic, addr: "exit.test:41641" },
            EndpointRef { kind: EndpointKind::Quic, addr: "192.0.2.1:41641" },
        ]);
        let c = EndpointCandidates::open(&t, None);
        for i in 1..=64 {
            let addr = SocketAddr::new(IpAddr::V4(std::net::Ipv4Addr::new(192, 0, 2, i)), 41641);
            c.insert(Candidate { addr, via: Via::Direct }, Source::Token, Some("exit.test:41641".to_owned()), now() + VERIFIED_TTL);
            c.entries.lock().unwrap().iter_mut().find(|e| e.addr == addr).unwrap().verified = now();
        }
        let fresh: IpAddr = "192.0.2.65".parse().unwrap();
        c.apply_dns("exit.test:41641", false, 41641, vec![fresh]);
        assert!(c.candidates().iter().any(|c| c.addr.ip() == fresh), "新DNS快照不能被旧已验证快照堵住");
        let list = c.entries.lock().unwrap();
        let literal = list.iter().find(|e| e.addr == "192.0.2.1:41641".parse().unwrap()).unwrap();
        assert_eq!(literal.source, Source::Token);
        assert!(literal.domain.is_none());
        assert_eq!(literal.expires, u64::MAX);
        assert!(list.iter().any(|e| e.source == Source::VerifiedPath));
    }

    #[test]
    fn unauthenticated_probe_and_hint_reject_private_mapped_and_fake_addresses() {
        let c = empty();
        for source in [Source::Hint, Source::Probe] {
            for raw in ["10.0.0.1:41641", "172.16.0.1:41641", "192.168.1.2:41641",
                "127.0.0.1:41641", "[::1]:41641", "169.254.1.1:41641", "198.18.1.1:41641",
                "100.64.0.1:41641", "0.1.2.3:41641", "240.1.2.3:41641", "255.255.255.255:41641", "[::]:41641", "[fc00::1]:41641", "[fe80::1]:41641",
                "[::ffff:127.0.0.1]:41641", "[::ffff:192.168.1.2]:41641",
                "[::ffff:198.18.1.1]:41641", "[::ffff:100.64.0.1]:41641"] {
                c.learn(source, &[Candidate { addr: raw.parse().unwrap(), via: Via::Direct }]);
            }
        }
        assert!(c.candidates().is_empty());
        c.learn(Source::Probe, &[cand(48023)]);
        assert_eq!(c.candidates(), vec![cand(48023)]);
        c.add_token(&token(&[EndpointRef { kind: EndpointKind::Quic, addr: "192.168.1.2:41641" }]));
        assert!(c.candidates().iter().any(|c| c.addr.ip() == "192.168.1.2".parse::<IpAddr>().unwrap()), "显式token仍允许LAN");
    }

    #[test]
    fn bounded_probe_round_robin_covers_valid_non_token_candidates() {
        let c = empty();
        c.add_token(&token(&[EndpointRef { kind: EndpointKind::Quic, addr: "8.8.8.8:48024" }]));
        c.learn(Source::Hint, &[cand(48025), cand(48026), cand(48027), cand(48028)]);
        c.learn(Source::Probe, &[Candidate { addr: cand(48029).addr, via: Via::Relay { label: c.label } }]);
        let mut seen = Vec::new();
        for _ in 0..3 {
            let round = c.probe_targets();
            assert_eq!(round.len(), 2);
            for addr in round { if !seen.contains(&addr) { seen.push(addr); } }
        }
        assert_eq!(seen.len(), 5, "非Token有效直连候选也必须被公平采样");
        assert!(!seen.contains(&cand(48029).addr), "不能把中继地址当出口probe目标");
    }

}
