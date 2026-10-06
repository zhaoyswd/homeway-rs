//! 内层 MTU 升档门（P2；设计真源 docs/reviews/P2.md §3.2/§3.3）。
//!
//! 坑 4/23 防线的手机侧执行面：`cfg.mtu > 1280`（App 注入的请求值）时，核在
//! warmup link 落定后、stage Ready 发布前做**一次**判定，结果进
//! `GenRun.mtu_eff` → tunStatusJSON 的 `mtuEff`（runner 块可选键）→ 扩展用它
//! 建 VpnConfig（接口 MTU 建立时定型——运行中不可变，见设计 §3.5）。
//!
//! 三条件硬与（任一不满足 = 维持 1280）：
//! 1. `link.via == "direct"`——中继腿一概不升（坑 23 的历史形态就是中继+分片丢；
//!    且中继腿外层多 9B 路由头，封账最差）；
//! 2. 采纳端点为 IPv4——v6 外层无路由器分片（RFC 8200），是唯一没有
//!    「分片可达」兜底的族（评审 P2-r1-①：整体排除）；
//! 3. DF 探针通过——按直连 v4 真实封账（20+8+2〔腿帧〕+32〔WG〕= inner+62）的
//!    精确模型发一个 DF 探针，本地路由缓存即拒（EMSGSIZE）或发送失败 = 不升。
//!    盲区如实登记：探针是**本地路由口径**，下游瓶颈不可见——由 v4 无 DF 分片
//!    可达兜底（降速不黑洞）+ 配置文档「仅已知 ≥1500 路径开」收口。
//!
//! 本门只管 **TUN 应用流量面**（VpnConfig.mtu）。手机核 stack B（自连）恒 1280
//! （P2 不升——升它需要 Interface 重建形态，P3 候选；见设计 §3.2 收窄说明）。

use std::net::{SocketAddr, SocketAddrV4, UdpSocket};

/// 探针载荷头（8B 可识别魔数——出口日志里能看出这条「入站新源」是 MTU 探针，
/// 后续出口侧也可据此静默；评审 P2-r1-6）。
const PROBE_MAGIC: &[u8; 8] = b"HMPROBE1";

/// 直连 v4 一跳的固定封账（IP20 + UDP8 + 腿帧2 + WG32——boringtun `PacketData`
/// 4+4+8+16 = 32B，与 `wgcore::WG_BUF` 注释同源）。
const DIRECT_V4_OVERHEAD: usize = 62;

/// DF 探针结果（可注入——单测不碰真 socket）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeOutcome {
    /// 发出成功（本地路由承载得住满尺寸外层包）。
    Pass,
    /// 本地路由即拒（EMSGSIZE——路由缓存 MTU 不足）。
    TooLarge,
    /// 其它错误（网络不可达等——保守按不升处理）。
    Error(String),
}

/// 发 DF 探针：独立 v4 UDP socket + DF + 单发单弃。载荷总长 = `inner + 34`
/// （2B 腿帧 + 32B WG 开销的精确模型——IP 包总长 = inner + 62，与真实直连 v4
/// 满包同尺寸）。**不要求回声**：verdict 只取本地 sendto 的即时结果（EMSGSIZE
/// 在内核路由查表时即返回，无需 ICMP 往返）。
pub fn df_probe(ep: SocketAddrV4, inner_mtu: usize) -> ProbeOutcome {
    let total = inner_mtu + DIRECT_V4_OVERHEAD - 28; // −(20 IP + 8 UDP) = UDP 载荷长
    let mut payload = vec![0u8; total];
    payload[..PROBE_MAGIC.len()].copy_from_slice(PROBE_MAGIC);
    let sock = match UdpSocket::bind("0.0.0.0:0") {
        Ok(s) => s,
        Err(e) => return ProbeOutcome::Error(format!("探针 socket 绑定失败：{e}")),
    };
    if !set_df_v4(&sock) {
        // DF 设不上（理论不至——Linux/macOS 都有对应选项）：保守按不升。
        return ProbeOutcome::Error("DF 选项设置失败（平台不支持——保守不升）".into());
    }
    match sock.send_to(&payload, ep) {
        Ok(_) => ProbeOutcome::Pass,
        // EMSGSIZE 判 raw_os_error（ErrorKind::MessageTooLarge 在 stable 不可用）
        Err(e) if e.raw_os_error() == Some(libc::EMSGSIZE) => ProbeOutcome::TooLarge,
        Err(e) => ProbeOutcome::Error(format!("探针发送失败：{e}")),
    }
}

/// v4 socket 设 DF（发送侧不分片）：Linux/OHOS = `IP_MTU_DISCOVER =
/// IP_PMTUDISC_DO`；macOS = `IP_DONTFRAG = 1`（libc 0.2.189 两族常量都有导出）。
fn set_df_v4(sock: &UdpSocket) -> bool {
    use std::os::fd::AsRawFd as _;
    let fd = sock.as_raw_fd();
    let rc = unsafe {
        #[cfg(target_os = "macos")]
        {
            let on: libc::c_int = 1;
            libc::setsockopt(
                fd,
                libc::IPPROTO_IP,
                libc::IP_DONTFRAG,
                &on as *const _ as *const libc::c_void,
                std::mem::size_of::<libc::c_int>() as libc::socklen_t,
            )
        }
        #[cfg(not(target_os = "macos"))]
        {
            let val: libc::c_int = libc::IP_PMTUDISC_DO;
            libc::setsockopt(
                fd,
                libc::IPPROTO_IP,
                libc::IP_MTU_DISCOVER,
                &val as *const _ as *const libc::c_void,
                std::mem::size_of::<libc::c_int>() as libc::socklen_t,
            )
        }
    };
    rc == 0
}

/// 升档门裁决（每世代一次；`outcome` 由调用方先用 [`df_probe`] 取或单测注入；
/// `direct` = 采纳链路是否直连——评审 P2-r2-L3：enum/bool 代替字符串模式匹配，
/// 「direct」词面的真源在 `wtransport::Via::as_str`，调用点比较后传布尔）。
/// 返回生效内层 MTU（1280 或请求值），并打出判据行（仅请求 ≠1280 时）。
pub fn decide(
    requested: u32,
    direct: bool,
    ep: Option<SocketAddr>,
    outcome: &ProbeOutcome,
    logf: &dyn Fn(&str),
) -> u32 {
    let req = crate::wgcore::stackb::clamp_inner_mtu(requested as usize) as u32;
    if req <= crate::wgcore::stackb::MTU as u32 {
        return crate::wgcore::stackb::MTU as u32; // 默认档：零日志零行为
    }
    let keep = |why: &str| {
        logf(&format!("mtu: 维持 1280（请求 {req}；{why}）"));
        crate::wgcore::stackb::MTU as u32
    };
    if !direct {
        return keep("非直连腿不升档（坑 23 防线）");
    }
    let Some(SocketAddr::V4(_)) = ep.filter(|a| a.is_ipv4()) else {
        return keep("采纳端点非 IPv4（v6 外层无分片兜底，不升档）");
    };
    match outcome {
        ProbeOutcome::Pass => {
            logf(&format!("mtu: 档位 {req}（直连 v4 + DF 探针通过；出口侧需 inner_mtu 同档才有下行收益）"));
            req
        }
        ProbeOutcome::TooLarge => keep("DF 探针 EMSGSIZE——本地路由承载不住满尺寸外层包"),
        ProbeOutcome::Error(e) => keep(&format!("DF 探针失败：{e}")),
    }
}

/// 世代接线助手（评审 P2-r2-M2：把「是否进门/是否探针」的分支收进可测函数，
/// `gen_loop` 只剩一行存储）。返回 `Some(生效 MTU)`（仅请求 ≠1280 时 Some——
/// 默认档保持 None ⇒ status 无 mtuEff 键）。`probe` 注入缝 = 单测替身。
pub fn generation_mtu_eff(
    cfg_mtu: u32,
    direct: bool,
    ep: Option<SocketAddr>,
    probe: impl FnOnce(SocketAddrV4, usize) -> ProbeOutcome,
    logf: &dyn Fn(&str),
) -> Option<u32> {
    if crate::wgcore::stackb::clamp_inner_mtu(cfg_mtu as usize) <= crate::wgcore::stackb::MTU {
        return None; // 默认档：不进门、不探针、不记行
    }
    // 探针只在「直连 + v4」形态才有意义——其余形态 decide 自会拒并记行。
    let outcome = match ep {
        Some(SocketAddr::V4(ep4)) if direct => probe(ep4, crate::wgcore::stackb::clamp_inner_mtu(cfg_mtu as usize)),
        _ => ProbeOutcome::Error("未探（非直连 v4 形态）".into()),
    };
    Some(decide(cfg_mtu, direct, ep, &outcome, logf))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_log(_: &str) {}

    fn counter() -> (std::sync::Arc<std::sync::atomic::AtomicUsize>, impl Fn(&str)) {
        let n = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let n2 = std::sync::Arc::clone(&n);
        (n, move |_| {
            n2.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        })
    }

    #[test]
    fn default_request_is_zero_touch() {
        // 1280/0/超下界请求：恒 1280 且**零日志**（P2-r2-L4：计数断言，不是空实现）。
        for req in [1280u32, 0, 600] {
            let (n, log) = counter();
            assert_eq!(decide(req, true, Some("1.2.3.4:1".parse().unwrap()), &ProbeOutcome::Pass, &log), 1280);
            assert_eq!(n.load(std::sync::atomic::Ordering::Relaxed), 0, "默认档不得打行（req={req}）");
            // 世代助手同口径：None ⇒ status 无 mtuEff 键
            assert_eq!(generation_mtu_eff(req, true, None, |_, _| ProbeOutcome::Pass, &log), None);
        }
    }

    #[test]
    fn relay_leg_never_raises() {
        let (n, log) = counter();
        let v = decide(1380, false, Some("1.2.3.4:1".parse().unwrap()), &ProbeOutcome::Pass, &log);
        assert_eq!(v, 1280);
        assert_eq!(n.load(std::sync::atomic::Ordering::Relaxed), 1);
    }

    #[test]
    fn v6_endpoint_never_raises() {
        let v = decide(1380, true, Some("[2001:db8::1]:1".parse().unwrap()), &ProbeOutcome::Pass, &no_log);
        assert_eq!(v, 1280);
        // 无端点（软失败暖机形态）同样不升
        assert_eq!(decide(1380, true, None, &ProbeOutcome::Pass, &no_log), 1280);
    }

    #[test]
    fn probe_verdict_decides_direct_v4() {
        let ep = Some(SocketAddr::from(([114, 242, 60, 128], 41641)));
        assert_eq!(decide(1380, true, ep, &ProbeOutcome::Pass, &no_log), 1380);
        assert_eq!(decide(1380, true, ep, &ProbeOutcome::TooLarge, &no_log), 1280);
        assert_eq!(decide(1380, true, ep, &ProbeOutcome::Error("x".into()), &no_log), 1280);
    }

    #[test]
    fn request_clamped_to_domain() {
        // 超上界请求 → clamp 到 1400 域内再判（探针不过仍 1280）。
        let ep = Some(SocketAddr::from(([1, 2, 3, 4], 9)));
        let n = std::sync::atomic::AtomicUsize::new(0);
        let log = |s: &str| {
            if s.contains("档位") {
                assert!(s.contains("1400"), "日志应打 clamp 后值：{s}");
                n.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
        };
        assert_eq!(decide(2000, true, ep, &ProbeOutcome::Pass, &log), 1400);
        assert_eq!(n.load(std::sync::atomic::Ordering::Relaxed), 1);
    }

    /// P2-r2-M2：世代接线（gen_loop 那一行的逻辑面）——不进门/进门各态。
    #[test]
    fn generation_wiring_states() {
        let ep = Some(SocketAddr::from(([10, 1, 2, 3], 41641)));
        // 软失败暖机（无链路信息）→ Some(1280) + 一行「维持」
        let (n, log) = counter();
        assert_eq!(generation_mtu_eff(1380, false, None, |_, _| ProbeOutcome::Pass, &log), Some(1280));
        assert_eq!(n.load(std::sync::atomic::Ordering::Relaxed), 1);
        // 中继（direct=false，有端点）→ 不调探针、Some(1280)
        let probed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let p2 = std::sync::Arc::clone(&probed);
        assert_eq!(
            generation_mtu_eff(1380, false, ep, move |_, _| {
                p2.store(true, std::sync::atomic::Ordering::Relaxed);
                ProbeOutcome::Pass
            }, &|_| {}),
            Some(1280)
        );
        assert!(!probed.load(std::sync::atomic::Ordering::Relaxed), "中继腿不得发探针");
        // 直连 v4 + 探针过 → Some(1380)
        assert_eq!(
            generation_mtu_eff(1380, true, ep, |ep4, inner| {
                assert_eq!(ep4.port(), 41641);
                assert_eq!(inner, 1380);
                ProbeOutcome::Pass
            }, &no_log),
            Some(1380)
        );
        // 直连 v6 端点 → 不调探针（族门先拒）
        assert_eq!(
            generation_mtu_eff(1380, true, Some("[2001:db8::1]:1".parse().unwrap()), |_, _| ProbeOutcome::Pass, &no_log),
            Some(1280)
        );
    }

    #[test]
    fn real_probe_against_loopback() {
        // 真 socket 冒烟：回环路由 MTU ≥16K，inner 1380 的探针必过（不依赖外部网络）。
        // 目的 = 本机一个无人端口（载荷对出口语义 = 坏包丢弃；这里只验本地路由口径）。
        let probe_port = {
            let s = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
            let p = s.local_addr().unwrap().port();
            drop(s);
            p
        };
        let out = df_probe(SocketAddrV4::new(std::net::Ipv4Addr::LOCALHOST, probe_port), 1380);
        assert_eq!(out, ProbeOutcome::Pass, "回环上 1380 探针应通过");
    }
}
