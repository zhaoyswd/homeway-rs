//! 绑卡看护循环（B0-1；语义真源 `baseline:internal/server/bindwatch.go`）。
//!
//! 出口的 WG socket 钉哪张物理网卡：默认**自动挑**，换网自动重挑 + 重钉。
//! - **换网卡指纹变化**（Wi-Fi↔有线、地址漂移）：重新挑卡重钉 socket，并踢公网
//!   端点/udpcap 立即重测（否则接口索引一变，socket 就钉在一个不存在的网卡上、
//!   端点 stale 到下一轮 10 分钟）；
//! - **当前卡健康探针**（每 60s 一次）：连续 2 次探不通才重挑（防抖）；
//! - 兜底：候选全探不通时**不绑**（保留旧 pin 等它回来；从未钉过则走系统默认路由）
//!   ——永远不因为挑不到卡就不启动。
//!
//! 判据行族（E21 后半）：`绑卡看护：网卡 %s 探针失败 %d/%d（%v）` / `…连续探不通，
//! 重新挑卡` / `…%s → %s（指纹变化）` / `…QUIC 端口 socket 钉在 %s（%s）` / `…重钉到 %s
//! 失败（%v）` / `…暂时挑不到可用网卡（%v），先保持现状` / `…挑不到可用物理网卡
//! （%v）—— 本轮不绑，走系统默认路由`。
//!
//! **探针目标注入缝**：`HOMEWAY_BINDWATCH_PROBE=<ip:port[,…]>`（诊断缝，与
//! HOMEWAY_WG_DEBUG 同惯例）把**健康探针**目标临时指向死地址（如 TEST-NET
//! `203.0.113.1:53`）——真机上模拟「当前卡出网退化」采看护判据行（探针失败
//! →连续探不通→重新挑卡）。挑卡（resolve）恒用真 anycast 目标：注入面收窄在
//! 健康探针（挑卡同死会走到「本轮不绑」族——那族行的真网形态另由默认目标下
//! 无网卡的环境给出，语义面单测逐字钉死）。

use std::net::SocketAddrV4;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::time::Duration;

use super::egress::{self, IfaceInfo};
use super::engine::EngineCmd;
use crate::Logf;

/// 轮询节拍（换网是低频事件；5s 足够快，开销可忽略——Go bindInterval）。
const BIND_INTERVAL: Duration = Duration::from_secs(5);
/// 每多少拍做一次「当前卡还通不通」探针（5s × 12 = 60s——Go bindHealthEvery）。
const BIND_HEALTH_EVERY: u32 = 12;
/// 当前卡连续几次探针失败才切换（防抖——Go bindFailsBeforeSwitch）。
const BIND_FAILS_BEFORE_SWITCH: u32 = 2;

/// 一张网卡的指纹（index + up + IPv4 地址集；只看 v4——macOS 的 v6 临时地址会
/// 自行轮换，算进来会频繁误触发。Go `ifaceState` 同义）。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct IfaceFingerprint {
    index: u32,
    up: bool,
    /// 排序后的 `ip/prefix` 列表（`addrs=[…]` 判据行素材）。
    addrs: Vec<String>,
}

impl std::fmt::Display for IfaceFingerprint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "index={} {} addrs=[{}]",
            self.index,
            if self.up { "up" } else { "down" },
            self.addrs.join(",")
        )
    }
}

/// 真实系统的指纹读（Go stateOf）。
fn fingerprint_of(ifi: &IfaceInfo) -> IfaceFingerprint {
    let mut addrs = ifi.cidrs.clone();
    if addrs.is_empty() {
        addrs = ifi.addrs.iter().map(|a| a.to_string()).collect();
    }
    addrs.sort();
    IfaceFingerprint { index: ifi.index, up: ifi.up, addrs }
}

/// 看护循环的可注入面（Go `BindWatchOpts` 的 Resolve/Probe/State 同义——单测不碰
/// 真网卡，脚本化驱动全分支）。
pub(crate) trait WatchDeps {
    /// 挑一张要钉的卡（auto = 候选探针取最快；explicit = 按名重解析）。
    fn resolve(&mut self) -> Result<IfaceInfo, String>;
    /// 健康探针（当前卡还能出网吗）。
    fn probe(&mut self, ifi: &IfaceInfo) -> Result<(), String>;
    /// 读指纹（默认真实系统；单测注入序列）。
    fn state_of(&mut self, ifi: &IfaceInfo) -> IfaceFingerprint;
    /// 重钉 socket（经 EngineCmd::Repin 交驱动线程执行）。
    fn repin(&mut self, index: u32, name: &str) -> Result<(), String>;
    /// 换卡后要做的事（踢公网端点 + udpcap 重测）。
    fn on_change(&mut self);
    fn log(&mut self, line: String);
}

/// 看护循环的滚动状态。
#[derive(Default)]
pub(crate) struct WatchState {
    cur: Option<IfaceInfo>,
    cur_fp: IfaceFingerprint,
    fails: u32,
    tick: u32,
}

/// 一拍看护（Go watchBind 循环体的单拍化——语义逐分支对齐）：
/// 1. 当前卡在、指纹没变且 up：低频健康探针（60s）；成功清 fails；失败计数，
///    连续 2 次打「连续探不通，重新挑卡」落入挑卡；
/// 2. 指纹变化：打「%s → %s」落入挑卡；
/// 3. 挑卡：失败时**保留旧 pin**（从未钉过则「本轮不绑」）；
/// 4. 挑到的还是同一张卡（同 index 同指纹）：不动；
/// 5. 重钉失败打行保持现状；成功打「WG socket 钉在 %s（%s）」+ on_change。
pub(crate) fn watch_tick(st: &mut WatchState, d: &mut impl WatchDeps) {
    st.tick += 1;
    // 1) 当前卡还在、指纹没变：只做低频健康检查
    if let Some(cur) = st.cur.clone() {
        let now = d.state_of(&cur);
        if now == st.cur_fp && now.up {
            if st.tick.is_multiple_of(BIND_HEALTH_EVERY) {
                match d.probe(&cur) {
                    Ok(()) => {
                        st.fails = 0;
                        return;
                    }
                    Err(e) => {
                        st.fails += 1;
                        d.log(format!(
                            "绑卡看护：网卡 {} 探针失败 {}/{}（{e}）",
                            cur.name, st.fails, BIND_FAILS_BEFORE_SWITCH
                        ));
                        if st.fails < BIND_FAILS_BEFORE_SWITCH {
                            return;
                        }
                        d.log(format!("绑卡看护：网卡 {} 连续探不通，重新挑卡", cur.name));
                    }
                }
            } else {
                return;
            }
        } else {
            d.log(format!("绑卡看护：网卡 {} {} → {}", cur.name, st.cur_fp, now));
        }
    }
    // 2) 需要（重新）挑卡
    match d.resolve() {
        Err(e) => {
            if st.cur.is_some() {
                // 暂时挑不到：保留旧 pin（等它回来），不当成「切到不绑」
                d.log(format!("绑卡看护：暂时挑不到可用网卡（{e}），先保持现状"));
            } else {
                d.log(format!("绑卡看护：挑不到可用物理网卡（{e}）—— 本轮不绑，走系统默认路由"));
            }
        }
        Ok(next) => {
            let nfp = d.state_of(&next);
            if let Some(cur) = &st.cur {
                // F5：name 优先的同一张卡判定（index=0 的多卡不互撞）
                if egress::iface_same(cur, &next) && nfp == st.cur_fp {
                    return;
                }
            }
            match d.repin(next.index, &next.name) {
                Err(e) => d.log(format!("绑卡看护：重钉到 {} 失败（{e}）", next.name)),
                Ok(()) => {
                    d.log(format!("绑卡看护：QUIC 端口 socket 钉在 {}（{nfp}）", next.name));
                    st.cur = Some(next);
                    st.cur_fp = nfp;
                    st.fails = 0;
                    d.on_change();
                }
            }
        }
    }
}

/// 生产实现：真网卡探针 + EngineCmd::Repin 重钉 + kick 公网端点/udpcap。
struct RealDeps {
    explicit_name: Option<String>,
    /// 健康探针目标（env 缝 > config > 默认——诊断注入面，见模块头）。
    probe_targets: Vec<SocketAddrV4>,
    /// **挑卡**目标（F2 纪律：恒吃 config/默认，**不吃 env**——与 `bindwatch.rs` 的
    /// 「挑卡恒用真目标」不变量逐字一致；env 只模拟健康探针退化）。
    pick_targets: Vec<SocketAddrV4>,
    cmd_tx: Sender<EngineCmd>,
    pinned_flag: Arc<AtomicBool>,
    pub_kick: std::sync::mpsc::SyncSender<()>,
    udpcap_kick: std::sync::mpsc::SyncSender<()>,
    logf: Logf,
}

impl WatchDeps for RealDeps {
    fn resolve(&mut self) -> Result<IfaceInfo, String> {
        if let Some(name) = &self.explicit_name {
            return egress::interfaces()
                .into_iter()
                .find(|i| &i.name == name)
                .ok_or_else(|| format!("网卡 {name} 当前不可用"));
        }
        // 挑卡恒用 config/默认目标（probe/env 注入只模拟健康探针退化——挑卡同死会误触「不绑」）
        let cands = egress::physical_candidates();
        let logf = Arc::clone(&self.logf);
        egress::select_best(&cands, &self.pick_targets, Duration::from_secs(2), &move |s| {
            (logf)(s);
        })
        .map_err(|e| e.to_string())
    }

    fn probe(&mut self, ifi: &IfaceInfo) -> Result<(), String> {
        egress::probe_iface(ifi, &self.probe_targets, Duration::from_secs(2)).map(|_| ()).map_err(|e| e.to_string())
    }

    fn state_of(&mut self, ifi: &IfaceInfo) -> IfaceFingerprint {
        // live 读（Go stateOf 每拍 ifi.Addrs() 查内核——指纹变化/网卡 down 是
        // 看护的触发面，快照会让分支恒不触发，评审 r1-M2）：按 **name** 回查
        // （F5：index 键面纠偏——`if_nametoindex` 失败的多卡不串键），卡消失 =
        // down + 空地址集（触发重挑）。
        state_of_from(&egress::interfaces(), ifi)
    }

    fn repin(&mut self, index: u32, name: &str) -> Result<(), String> {
        let (tx, rx) = std::sync::mpsc::channel();
        self.cmd_tx
            .send(EngineCmd::Repin { index, name: name.to_owned(), reply: tx })
            .map_err(|e| e.to_string())?;
        match rx.recv_timeout(Duration::from_secs(10)) {
            Ok(Ok(())) => {
                self.pinned_flag.store(true, Ordering::SeqCst);
                Ok(())
            }
            Ok(Err(e)) => Err(e.to_string()),
            Err(_) => Err("重钉回执超时（驱动线程忙）".to_owned()),
        }
    }

    fn on_change(&mut self) {
        // 端点要重测（可能换网/换 IP）；UDP 能力也要重测（换了条路）。pub_kick 直发
        // 即可（EngineCmd::KickPublicEndpoint 是外部/测试面的转传路径，双发=双 kick）
        let _ = self.pub_kick.try_send(());
        let _ = self.udpcap_kick.try_send(());
    }

    fn log(&mut self, line: String) {
        (self.logf)(&line);
    }
}

/// 看护线程装配参数（engine::start 组装）。
pub struct WatcherArgs {
    /// 显式网卡名（--bind-interface <网卡名>）：只按名重解析；None = auto（每轮重挑）。
    pub explicit_name: Option<String>,
    /// 健康探针目标（诊断缝 HOMEWAY_BINDWATCH_PROBE 注入死地址 = 真机采
    /// 「探针失败→重挑卡」判据行；缺省 = config `serve.dns_probe_target`）。
    pub probe_targets: Vec<SocketAddrV4>,
    /// 挑卡目标（F2：恒 = config `serve.dns_probe_target`，**不吃 env**）。
    pub pick_targets: Vec<SocketAddrV4>,
    pub cmd_tx: Sender<EngineCmd>,
    pub pinned_flag: Arc<AtomicBool>,
    pub pub_kick: std::sync::mpsc::SyncSender<()>,
    pub udpcap_kick: std::sync::mpsc::SyncSender<()>,
    pub logf: Logf,
    pub stop: Arc<AtomicBool>,
}

/// 起后台看护（非阻塞；stop 置位即退出）。
pub fn spawn_watcher(args: WatcherArgs) {
    let WatcherArgs {
        explicit_name,
        probe_targets,
        pick_targets,
        cmd_tx,
        pinned_flag,
        pub_kick,
        udpcap_kick,
        logf,
        stop,
    } = args;
    std::thread::Builder::new()
        .name("homeway-bindwatch".into())
        .spawn(move || {
            let mut st = WatchState::default();
            let mut d = RealDeps {
                explicit_name,
                probe_targets,
                pick_targets,
                cmd_tx,
                pinned_flag,
                pub_kick,
                udpcap_kick,
                logf,
            };
            while !stop.load(Ordering::SeqCst) {
                std::thread::sleep(BIND_INTERVAL);
                watch_tick(&mut st, &mut d);
            }
        })
        .ok();
}

/// 健康探针目标：`HOMEWAY_BINDWATCH_PROBE` 诊断缝（注入死地址 TEST-NET = 真机模拟
/// 拔线采「探针失败→重挑卡」判据行）> config/默认（`serve.dns_probe_target`）。
pub fn health_probe_targets_from_env(cfg_targets: &[SocketAddrV4]) -> Vec<SocketAddrV4> {
    if let Some(v) = std::env::var_os("HOMEWAY_BINDWATCH_PROBE") {
        let parsed: Vec<SocketAddrV4> = v
            .to_string_lossy()
            .split(',')
            .filter_map(|s| s.trim().parse().ok())
            .collect();
        if !parsed.is_empty() {
            return parsed;
        }
    }
    pick_targets(cfg_targets)
}

/// **挑卡**目标 = config/默认值（F2 纪律：挑卡不吃 env 缝；缺省 = 修前的
/// `egress::default_probe_targets()` 形态——engine 装配时以 `ServeConfig` 默认填充）。
pub(crate) fn pick_targets(cfg_targets: &[SocketAddrV4]) -> Vec<SocketAddrV4> {
    cfg_targets.to_vec()
}

/// live 网卡表里按 **name** 回查 `want` 的指纹（F5 纯函数注入缝：消除「直连内核无
/// 注入缝」；名字找不到 = down + 空地址集——index 保留 want 值供判据渲染）。
pub(crate) fn state_of_from(live: &[IfaceInfo], want: &IfaceInfo) -> IfaceFingerprint {
    match live.iter().find(|i| i.name == want.name) {
        Some(l) => fingerprint_of(l),
        None => IfaceFingerprint { index: want.index, up: false, addrs: Vec::new() },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 脚本化 WatchDeps：按序回放事件，记录判据行与动作。
    struct Script {
        /// resolve 的回放序列（空 = 恒 Err）。
        resolves: Vec<Result<IfaceInfo, String>>,
        /// probe 的回放序列。
        probes: Vec<Result<(), String>>,
        /// state_of 的指纹（按 index 查表；未登记 = fingerprint_of 真值）。
        fps: std::collections::HashMap<u32, IfaceFingerprint>,
        logs: Vec<String>,
        repins: Vec<(u32, String)>,
        repin_err: Option<String>,
        changes: u32,
    }

    impl Script {
        fn new() -> Self {
            Self {
                resolves: Vec::new(),
                probes: Vec::new(),
                fps: std::collections::HashMap::new(),
                logs: Vec::new(),
                repins: Vec::new(),
                repin_err: None,
                changes: 0,
            }
        }
        fn iface(index: u32, name: &str) -> IfaceInfo {
            IfaceInfo {
                name: name.to_owned(),
                index,
                index_ok: index != 0,
                addrs: vec![std::net::Ipv4Addr::new(192, 0, 2, index as u8)],
                cidrs: vec![format!("192.0.2.{index}/24")],
                up: true,
                loopback: false,
            }
        }
        fn fp(index: u32, up: bool, addr: &str) -> IfaceFingerprint {
            IfaceFingerprint { index, up, addrs: vec![addr.to_owned()] }
        }
    }

    impl WatchDeps for Script {
        fn resolve(&mut self) -> Result<IfaceInfo, String> {
            if self.resolves.is_empty() {
                Err("所有候选网卡都探不通：en0 不通（预算）".to_owned())
            } else {
                self.resolves.remove(0)
            }
        }
        fn probe(&mut self, ifi: &IfaceInfo) -> Result<(), String> {
            if self.probes.is_empty() {
                Ok(())
            } else {
                self.probes.remove(0).map_err(|e| format!("en{}: {e}", ifi.index))
            }
        }
        fn state_of(&mut self, ifi: &IfaceInfo) -> IfaceFingerprint {
            self.fps.get(&ifi.index).cloned().unwrap_or_else(|| fingerprint_of(ifi))
        }
        fn repin(&mut self, index: u32, name: &str) -> Result<(), String> {
            if let Some(e) = &self.repin_err {
                return Err(e.clone());
            }
            self.repins.push((index, name.to_owned()));
            Ok(())
        }
        fn on_change(&mut self) {
            self.changes += 1;
        }
        fn log(&mut self, line: String) {
            self.logs.push(line);
        }
    }

    fn joined(logs: &[String]) -> String {
        logs.join("\n")
    }

    /// 首拍即挑卡钉上（无当前卡 → resolve 成功 → repin + 判据行 + on_change）。
    #[test]
    fn first_pick_pins() {
        let mut st = WatchState::default();
        let mut d = Script::new();
        d.resolves = vec![Ok(Script::iface(6, "en0"))];
        watch_tick(&mut st, &mut d);
        assert_eq!(d.repins, vec![(6, "en0".to_owned())]);
        assert_eq!(d.changes, 1);
        assert!(joined(&d.logs).contains("绑卡看护：QUIC 端口 socket 钉在 en0（index=6 up addrs=[192.0.2.6/24]）"));
    }

    /// 无当前卡 + 挑不到：`本轮不绑` 行、不重钉；有当前卡 + 挑不到（指纹先变触发
    /// 挑卡）：`先保持现状`、保留旧 pin。
    #[test]
    fn resolve_failure_keeps_old_pin() {
        let mut st = WatchState::default();
        let mut d = Script::new();
        watch_tick(&mut st, &mut d); // 挑不到（resolves 空）
        assert!(joined(&d.logs).contains("本轮不绑，走系统默认路由"));
        // 钉上卡后再挑不到（指纹变化触发挑卡，但候选全探不通）：保留旧 pin
        d.resolves = vec![Ok(Script::iface(6, "en0"))];
        watch_tick(&mut st, &mut d);
        d.logs.clear();
        d.fps.insert(6, Script::fp(6, true, "198.51.100.7/24")); // 地址漂移 → 触发挑卡
        watch_tick(&mut st, &mut d); // resolves 空 → 挑不到
        let all = joined(&d.logs);
        assert!(all.contains("暂时挑不到可用网卡"), "有当前卡时保持现状：{all}");
        assert!(all.contains("→ index=6 up addrs=[198.51.100.7/24]"), "指纹变化行在先：{all}");
        assert_eq!(d.repins.len(), 1, "不再重钉");
    }

    /// 同卡同指纹：健康拍前零动作（零重钉、零判据行）。
    #[test]
    fn stable_card_noop() {
        let mut st = WatchState::default();
        let mut d = Script::new();
        d.resolves = vec![Ok(Script::iface(6, "en0"))];
        watch_tick(&mut st, &mut d);
        d.logs.clear();
        let baseline = d.repins.len();
        for _ in 0..3 {
            watch_tick(&mut st, &mut d);
        }
        assert_eq!(d.repins.len(), baseline, "指纹稳定时零重钉");
        assert!(d.logs.is_empty(), "指纹稳定时零判据行：{}", joined(&d.logs));
    }

    /// 健康探针连续失败防抖：第一次失败打行不切；第二次「连续探不通，重新挑卡」
    /// ——重挑回**同卡同指纹**则维持现状（Go `next.Index==cur.Index && State==curState`
    /// continue 同义，不重钉不 on_change）。
    #[test]
    fn probe_fail_debounce_then_repick() {
        let mut st = WatchState::default();
        let mut d = Script::new();
        d.resolves = vec![Ok(Script::iface(6, "en0"))];
        watch_tick(&mut st, &mut d); // 钉上（tick 1）
        // 快进到健康拍前一拍（tick 11；下一拍 tick 12 即健康探针拍）
        for _ in 1..BIND_HEALTH_EVERY - 1 {
            watch_tick(&mut st, &mut d);
        }
        assert_eq!(st.tick, BIND_HEALTH_EVERY - 1, "下一拍即健康探针拍");
        d.probes = vec![Err("预算耗尽".to_owned()), Err("预算耗尽".to_owned())];
        watch_tick(&mut st, &mut d); // tick 12：第一次失败
        assert!(joined(&d.logs).contains("探针失败 1/2"), "{}", joined(&d.logs));
        assert_eq!(d.repins.len(), 1, "第一次失败不切卡");
        // tick 13-23 非健康拍（无动作），tick 24 第二次失败 → 重挑回同卡 ⇒ 维持现状
        for _ in 0..BIND_HEALTH_EVERY {
            watch_tick(&mut st, &mut d);
        }
        let all = joined(&d.logs);
        assert!(all.contains("探针失败 2/2"), "{all}");
        assert!(all.contains("连续探不通，重新挑卡"), "{all}");
        assert_eq!(d.repins.len(), 1, "重挑回同卡同指纹 = 维持现状");
        assert_eq!(d.changes, 1);
    }

    /// 指纹变化（地址漂移）→ `%s → %s` 行 + 重挑重钉。
    #[test]
    fn fingerprint_change_repins() {
        let mut st = WatchState::default();
        let mut d = Script::new();
        let ifi = Script::iface(6, "en0");
        d.resolves = vec![Ok(ifi.clone())];
        watch_tick(&mut st, &mut d);
        d.logs.clear();
        // 地址漂移（同卡换地址）
        d.fps.insert(6, Script::fp(6, true, "198.51.100.7/24"));
        d.resolves = vec![Ok(Script::iface(6, "en0"))];
        watch_tick(&mut st, &mut d);
        let all = joined(&d.logs);
        assert!(all.contains("网卡 en0 index=6 up addrs=[192.0.2.6/24] → index=6 up addrs=[198.51.100.7/24]"), "{}", all);
        assert_eq!(d.repins.len(), 2);
    }

    /// F5：`state_of_from` 纯函数三态——两张 index=0 卡按 **name** 不串键；名字消失
    /// = down + 空地址集（触发重挑）；换 index（同 name）⇒ 指纹变（重钉信号不丢）。
    #[test]
    fn state_of_from_name_keyed() {
        let a = Script::iface(0, "en0");
        let b = Script::iface(0, "en1");
        let live = vec![a.clone(), b.clone()];
        assert_eq!(state_of_from(&live, &a), fingerprint_of(&a), "index=0 的两卡不得互撞");
        assert_eq!(state_of_from(&live, &b), fingerprint_of(&b));
        // 名字消失 ⇒ down + 空地址集（index 保留 want 值供判据渲染）
        let ghost = Script::iface(0, "en9");
        assert_eq!(
            state_of_from(&live, &ghost),
            IfaceFingerprint { index: 0, up: false, addrs: Vec::new() }
        );
        // 换 index（同 name）⇒ 指纹变（「换 index ⇒ 重钉」信号不丢）
        let renamed = Script::iface(7, "en0");
        assert_ne!(state_of_from(&[renamed], &a), fingerprint_of(&a));
    }

    /// F2 纪律（代码门 M3④：**真注入** env）：健康探针吃 `HOMEWAY_BINDWATCH_PROBE`
    /// 、**挑卡目标不吃**。env 是进程级——本仓只有本测试读写该变量（无并发互踩点）。
    #[test]
    fn pick_targets_ignore_env_seam() {
        let cfg = vec![SocketAddrV4::new(std::net::Ipv4Addr::new(223, 5, 5, 5), 53)];
        assert_eq!(pick_targets(&cfg), cfg, "挑卡目标 = config 值原样");
        assert_eq!(health_probe_targets_from_env(&cfg), cfg, "无 env = config/默认");
        std::env::set_var("HOMEWAY_BINDWATCH_PROBE", "203.0.113.1:53,203.0.113.2:53");
        let got = health_probe_targets_from_env(&cfg);
        std::env::remove_var("HOMEWAY_BINDWATCH_PROBE");
        assert_eq!(
            got,
            vec!["203.0.113.1:53".parse::<SocketAddrV4>().unwrap(), "203.0.113.2:53".parse().unwrap()],
            "env 缝真生效于健康探针（死地址注入形态）"
        );
        assert_eq!(pick_targets(&cfg), cfg, "env 注入不得改变挑卡目标");
        assert_eq!(health_probe_targets_from_env(&cfg), cfg, "env 摘除后回落 config");
    }

    /// F5：重挑回**同一张卡**（name 同、index 翻转 0→6）仍算维持现状——`iface_same`
    /// name 优先（修前 `cur.index == next.index` 会把同卡误判成换卡 → 多余重钉）。
    #[test]
    fn repick_same_name_with_index_flap_stays() {
        let mut st = WatchState::default();
        let mut d = Script::new();
        d.resolves = vec![Ok(Script::iface(0, "en0"))];
        watch_tick(&mut st, &mut d);
        assert_eq!(d.repins.len(), 1, "首拍钉上");
        d.logs.clear();
        // 触发挑卡：当前卡地址漂移；重挑回**同名卡**（live index 字段形态不变 = 旧指纹同值）
        d.fps.insert(0, Script::fp(0, true, "198.51.100.7/24"));
        d.fps.insert(6, Script::fp(0, true, "192.0.2.0/24"));
        d.resolves = vec![Ok(Script::iface(6, "en0"))];
        watch_tick(&mut st, &mut d);
        assert_eq!(d.repins.len(), 1, "同名同指纹 ⇒ 维持现状（不重钉）");
        assert!(!joined(&d.logs).contains("QUIC 端口 socket 钉在"), "无重钉行：{}", joined(&d.logs));
        assert_eq!(st.cur.as_ref().map(|i| i.index), Some(0), "当前卡不被同名重挑改写");
    }

    /// 重挑挑到**别的卡**：重钉 + 「QUIC 端口 socket 钉在 %s」 + on_change；重钉失败：打行、
    /// 不更新当前卡、不 on_change。
    #[test]
    fn repin_failure_logged() {
        let mut st = WatchState::default();
        let mut d = Script::new();
        d.resolves = vec![Ok(Script::iface(6, "en0"))];
        watch_tick(&mut st, &mut d);
        assert_eq!(d.changes, 1);
        // 地址漂移触发挑卡，挑到 en1：重钉成功路径
        d.resolves = vec![Ok(Script::iface(7, "en1"))];
        d.fps.insert(6, Script::fp(6, true, "198.51.100.7/24"));
        watch_tick(&mut st, &mut d);
        assert!(joined(&d.logs).contains("绑卡看护：QUIC 端口 socket 钉在 en1"), "{}", joined(&d.logs));
        assert_eq!(d.changes, 2);
        // 再触发挑卡（en1 指纹变）挑回 en0 但重钉失败：打行、保持 en1
        d.repin_err = Some("IP_BOUND_IF/IPV6_BOUND_IF: x / y".to_owned());
        d.resolves = vec![Ok(Script::iface(6, "en0"))];
        d.fps.insert(7, Script::fp(7, true, "203.0.113.9/24"));
        watch_tick(&mut st, &mut d);
        assert!(joined(&d.logs).contains("重钉到 en0 失败"), "{}", joined(&d.logs));
        assert_eq!(d.changes, 2, "失败不算换卡");
        assert_eq!(st.cur.as_ref().map(|i| i.name.as_str()), Some("en1"), "当前卡保持旧值");
    }
}
