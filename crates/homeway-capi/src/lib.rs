//! libclientcore.so 的 C-ABI 壳（R7-7h；语义真源 = Go `clientcore/cmd/clientcore`
//! 的 `//export` 族——**同名符号原位替换**，tier 的 tailcat_napi.cpp/Index.d.ts/
//! Index.ets 契约按符号名闭合、零改动消费）。
//!
//! ABI 契约（两侧必须一致——tier tailcat_napi.cpp 头注释的镜像）：
//! - 入参 `char*`：本壳只读（CStr 即时转 &str），**从不释放**——副本由 NAPI 侧
//!   分配与回收；
//! - 返回 `char*`：`libc::malloc` 分配（**非 Rust 分配器**——NAPI 侧 `std::free`
//!   释放，Go C.CString 同契约）；失败兜底返回空串指针（永不为 NULL——NAPI 侧
//!   MakeString 对 NULL 转空串，但恒非空更稳）；
//! - panic 边界（工单⑥）：每导出 `catch_unwind`——c-shared 宿主是扩展进程，
//!   panic 穿透 = 整个 App 数据面死；兜底返回安全错误值（int = -9；char* = 空）。
//!
//! 版本注入：`HOMEWAY_CORE_VERSION`（构建脚本注入核源 SHA——tier build-core.sh 对接）
//! 与 `HOMEWAY_RUSTC_VERSION`（rustc 版本，等价 Go runtime.Version() 信息位）。

use std::ffi::{CStr, CString};
use std::net::ToSocketAddrs;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, OnceLock};

use homeway_core::facade::demand::DemandSignals;
use homeway_core::facade::speedtest_op::SpeedHost;
use homeway_core::facade::tun_exec::TunnelExec;
use homeway_core::facade::ClientCore;
use homeway_core::{probe, token};

/// 测速轮级状态机（Start/Status/Cancel 三导出的执行面）。
static SPEED: OnceLock<SpeedHost> = OnceLock::new();

/// App 核单例（进程级——扩展进程一个 ClientCore）。
static CORE: OnceLock<ClientCore> = OnceLock::new();

fn speed() -> &'static SpeedHost {
    SPEED.get_or_init(SpeedHost::new)
}

fn core() -> &'static ClientCore {
    CORE.get_or_init(|| {
        let demand = Arc::new(DemandSignals::new());
        let exec = TunnelExec::new(Arc::clone(&demand));
        let c = ClientCore::with_shared(exec.clone(), Arc::clone(&demand));
        // 回前台 kick 接线（false→true 转变 → 巡检立即探测一次）
        c.set_foreground_kick(Some(Box::new(move || exec.kick_probe())));
        c
    })
}

// ---------------------------------------------------------------------------
// ABI 工具（CString 契约 + panic 边界）
// ---------------------------------------------------------------------------

/// 入参转 &str（NULL 安全——按空串处理；Go C.GoString(NULL) 同义崩溃面在 Go 侧
/// 不存在，这里更保守）。
fn arg_str<'a>(p: *const std::os::raw::c_char) -> std::borrow::Cow<'a, str> {
    if p.is_null() {
        return "".into();
    }
    unsafe { CStr::from_ptr(p) }.to_string_lossy()
}

/// 返回串：libc::malloc 分配（NAPI 侧 std::free）；分配失败 = 空串指针（恒非 NULL）。
fn ret_cstring(s: &str) -> *mut std::os::raw::c_char {
    match CString::new(s) {
        Ok(c) => {
            let bytes = c.as_bytes_with_nul();
            let len = bytes.len();
            unsafe {
                let buf = libc::malloc(len) as *mut std::os::raw::c_char;
                if buf.is_null() {
                    return empty_cstr();
                }
                std::ptr::copy_nonoverlapping(
                    bytes.as_ptr() as *const std::os::raw::c_char,
                    buf,
                    len,
                );
                buf
            }
        }
        Err(_) => empty_cstr(), // 内嵌 NUL：截断面（判据串不含 NUL）
    }
}

/// 空串指针（静态分配——只读语义，free() 对静态指针是 UB！改为 malloc 的空串）。
fn empty_cstr() -> *mut std::os::raw::c_char {
    unsafe {
        let buf = libc::malloc(1) as *mut std::os::raw::c_char;
        if buf.is_null() {
            return std::ptr::null_mut(); // 分配失败兜底（NAPI 侧按空串处理）
        }
        *buf = 0;
        buf
    }
}

/// panic 边界（工单⑥）：导出体的 catch_unwind 包装。fallback 是**闭包**（评审
/// r2-我-3：立即求值形态在成功路径也执行 `empty_cstr()` ⇒ 每次成功调用 malloc(1)
/// 一个无主堆块——裸指针无 Drop，10 个导出全中；懒求值后只在真 panic 时兜底）。
fn guard<T>(body: impl FnOnce() -> T + std::panic::UnwindSafe, fallback: impl FnOnce() -> T) -> T {
    match std::panic::catch_unwind(body) {
        Ok(v) => v,
        Err(_) => fallback(),
    }
}

// ---------------------------------------------------------------------------
// 20 个导出（符号名/签名/语义 = Go probe_lib.go 及 app_*.go 的 //export 逐个对齐）
// ---------------------------------------------------------------------------

/// ClientCoreVersion：`tier core %s (%s, c-shared)` 同形。
#[no_mangle]
pub extern "C" fn ClientCoreVersion() -> *mut std::os::raw::c_char {
    guard(|| ret_cstring(&ClientCore::version()), empty_cstr)
}

/// ClientCoreTunPrepare：0 已开始 / -1 忙 / -2 日志打不开 / -3 参数错（含 token 空）。
#[no_mangle]
pub extern "C" fn ClientCoreTunPrepare(
    c_config: *const std::os::raw::c_char,
) -> std::os::raw::c_int {
    guard(|| core().tun_prepare(&arg_str(c_config), true), || -9)
}

/// ClientCoreTunAttach：0 已接管 / -1 无 ready 世代 / -3 fd≤0 / -4 接管失败 / -5 轮询超时。
#[no_mangle]
pub extern "C" fn ClientCoreTunAttach(fd: std::os::raw::c_int) -> std::os::raw::c_int {
    guard(
        || core().tun_attach(fd, 0), // mtu 由世代线程从 cfg 读（fd 通道只传 fd）
        || -9,
    )
}

/// ClientCoreTunStatus：tunStatusJSON（完整键面）。
#[no_mangle]
pub extern "C" fn ClientCoreTunStatus() -> *mut std::os::raw::c_char {
    guard(|| ret_cstring(&core().tun_status()), empty_cstr)
}

/// ClientCoreTunStop：0 已停 / -1 等待超时 / -2 超时后强制放锁。
#[no_mangle]
pub extern "C" fn ClientCoreTunStop() -> std::os::raw::c_int {
    guard(|| core().tun_stop(), || -9)
}

/// ClientCoreTunRecover：恢复阶梯下推（from = 起跑档位 1..=3）。
#[no_mangle]
pub extern "C" fn ClientCoreTunRecover(from: std::os::raw::c_int) -> std::os::raw::c_int {
    guard(|| core().tun_recover(from as i64), || -9)
}

/// ClientCoreTunSetPortForwards：运行中整表热替换（0/-1/-2）。
#[no_mangle]
pub extern "C" fn ClientCoreTunSetPortForwards(
    c_cfg: *const std::os::raw::c_char,
) -> std::os::raw::c_int {
    guard(|| core().tun_set_port_forwards(&arg_str(c_cfg)), || -9)
}

/// ClientCoreTunRunning：1 在跑（attached 且健康）/ 0。
#[no_mangle]
pub extern "C" fn ClientCoreTunRunning() -> std::os::raw::c_int {
    guard(|| core().tun_running(), || -9)
}

/// ClientCoreTunSetForeground：前台位下发；返回上一状态。
#[no_mangle]
pub extern "C" fn ClientCoreTunSetForeground(fg: std::os::raw::c_int) -> std::os::raw::c_int {
    guard(|| core().tun_set_foreground(fg != 0), || -9)
}

/// ClientCoreTunSetActivity：需求信号每拍下发（fg/screen）。
#[no_mangle]
pub extern "C" fn ClientCoreTunSetActivity(fg: std::os::raw::c_int, screen: std::os::raw::c_int) {
    guard(|| core().tun_set_activity(fg != 0, screen != 0), || ())
}

/// ClientCoreProbeAddr：token 本地解析（不联网）。
#[no_mangle]
pub extern "C" fn ClientCoreProbeAddr(
    c_token: *const std::os::raw::c_char,
) -> *mut std::os::raw::c_char {
    guard(
        || ret_cstring(&core().probe_addr(&arg_str(c_token))),
        empty_cstr,
    )
}

/// ClientCoreProbeReach：对 token 端点全集（域名先解析）并发发参照点探测——App
/// 进程旁路（独立临时 socket、无身份明文包，不碰在跑会话），总预算 ~3s。
/// results 只含应答端点（死端点静默；中继应答为尾部能力）。
#[no_mangle]
pub extern "C" fn ClientCoreProbeReach(
    c_token: *const std::os::raw::c_char,
) -> *mut std::os::raw::c_char {
    guard(
        || {
            let report = probe_reach_report(&arg_str(c_token));
            match report {
                Ok(rep) => ret_cstring(&homeway_core::facade::probe_json::probe_reach_json(&rep)),
                Err(msg) => ret_cstring(&homeway_core::facade::probe_json::probe_reach_err(&msg)),
            }
        },
        empty_cstr,
    )
}

/// ProbeReach 的探测编排（App 旁路形态：独立 socket、无身份、并发全端点）。
/// 预算口径（评审 r2-L9 对齐 Go）：**父预算 3.5s**（spec ≤3.5s MUST）——域名解析
/// 并行（1.5s 子预算；挂死的解析线程超时即弃，不拖父预算）+ 去重（同地址多端点
/// 只探一次）+ 探测预算 = 父预算余量与 3s 取小。
fn probe_reach_report(
    token_raw: &str,
) -> Result<homeway_core::facade::probe_json::ReachReport, String> {
    let t0 = std::time::Instant::now();
    const PARENT: std::time::Duration = std::time::Duration::from_millis(3500);
    const RESOLVE: std::time::Duration = std::time::Duration::from_millis(1500);
    const PROBE_MAX: std::time::Duration = std::time::Duration::from_secs(3);
    let raw = token_raw.trim();
    let tok = token::decode(raw).map_err(|e| e.to_string())?;
    // 端点全集（域名先解析——并行、带 1.5s 子预算；解析失败/超时的端点静默跳过）
    let (tx, rx) = std::sync::mpsc::channel::<Vec<(std::net::SocketAddr, bool)>>();
    for ep in &tok.endpoints {
        let addr = ep.addr.clone();
        let relay = ep.kind == token::EndpointKind::Relay;
        let tx2 = tx.clone();
        std::thread::spawn(move || {
            if let Ok(addrs) = addr.to_socket_addrs() {
                let _ = tx2.send(addrs.map(|a| (a, relay)).collect());
            }
        });
    }
    drop(tx);
    let mut targets: Vec<(std::net::SocketAddr, bool)> = Vec::new();
    let resolve_deadline = t0 + RESOLVE;
    loop {
        let left = resolve_deadline.saturating_duration_since(std::time::Instant::now());
        if left.is_zero() {
            break;
        }
        match rx.recv_timeout(left) {
            Ok(batch) => targets.extend(batch),
            Err(_) => break,
        }
    }
    // 去重（同地址只探一次——域名展开成多地址时避免重复打）
    targets.sort_by_key(|(a, _)| *a);
    targets.dedup_by_key(|(a, _)| *a);
    if targets.is_empty() {
        return Err("token 里没有可解析的端点".into());
    }
    // 探测预算 = 父预算余量与 3s 取小（spec：整轮 ≤3.5s MUST）
    let budget = PARENT.saturating_sub(t0.elapsed()).min(PROBE_MAX);
    if budget.is_zero() {
        return Err("端点解析耗尽预算".into());
    }
    let results: Vec<homeway_core::facade::probe_json::ReachEntry> = std::thread::scope(|s| {
        let handles: Vec<_> = targets
            .iter()
            .map(|(addr, relay)| {
                let addr = *addr;
                let relay = *relay;
                s.spawn(move || {
                    probe::ping_ex(addr, probe::PROBE_PAD, budget)
                        .ok()
                        .map(|r| homeway_core::facade::probe_json::ReachEntry {
                            ep: addr.to_string(),
                            rtt_ms: r.rtt.as_millis() as i64,
                            build: r.build,
                            relay,
                        })
                })
            })
            .collect();
        handles
            .into_iter()
            .filter_map(|h| h.join().ok().flatten())
            .collect()
    });
    let mut m = homeway_core::facade::probe_json::ReachReport {
        peer: tok.peer_id.as_bytes()[..6]
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect(),
        endpoints: tok
            .endpoints
            .iter()
            .map(|ep| match ep.kind {
                token::EndpointKind::Relay => format!("relay:{}", ep.addr),
                // M1 起 QUIC 类端点（additive：既有两形态不变；App 只透传字符串）
                token::EndpointKind::Quic => format!("quic:{}", ep.addr),
                token::EndpointKind::Direct => ep.addr.clone(),
            })
            .collect(),
        results,
    };
    m.results.sort_by(|a, b| a.ep.cmp(&b.ep));
    Ok(m)
}

/// ClientCoreServiceStart：服务会话（App 进程内、无 TUN 的 WG 会话 + 三座桥）。
#[no_mangle]
pub extern "C" fn ClientCoreServiceStart(
    c_config: *const std::os::raw::c_char,
) -> std::os::raw::c_int {
    guard(|| core().service_start(&arg_str(c_config)), || -9)
}

/// ClientCoreServiceStop：0 已收工 / -1 等待超时（重试）。
#[no_mangle]
pub extern "C" fn ClientCoreServiceStop() -> std::os::raw::c_int {
    guard(|| core().service_stop(), || -9)
}

/// ClientCoreServiceStatus：状态 JSON + bridge 四键。
#[no_mangle]
pub extern "C" fn ClientCoreServiceStatus() -> *mut std::os::raw::c_char {
    guard(|| ret_cstring(&core().service_status()), empty_cstr)
}

/// ClientCoreFilesCall：文件管理单导出（操作名分发）。
#[no_mangle]
pub extern "C" fn ClientCoreFilesCall(
    c_op: *const std::os::raw::c_char,
) -> *mut std::os::raw::c_char {
    guard(
        || ret_cstring(&core().files_call(&arg_str(c_op))),
        empty_cstr,
    )
}

/// ClientCoreTermCall：终端一次性操作（list/kill）。
#[no_mangle]
pub extern "C" fn ClientCoreTermCall(
    c_op: *const std::os::raw::c_char,
) -> *mut std::os::raw::c_char {
    guard(
        || ret_cstring(&core().term_call(&arg_str(c_op))),
        empty_cstr,
    )
}

/// ClientCoreSpeedTestStart：同步跑完整轮（NAPI 在 async work 线程上调）——busy 门 +
/// Cancel 真取消在 SpeedHost。
#[no_mangle]
pub extern "C" fn ClientCoreSpeedTestStart(
    c_params: *const std::os::raw::c_char,
) -> *mut std::os::raw::c_char {
    guard(
        || ret_cstring(&speed().start(&arg_str(c_params))),
        empty_cstr,
    )
}

/// ClientCoreSpeedTestStatus：轮询快照（250ms 面经 NAPI async 壳）。
#[no_mangle]
pub extern "C" fn ClientCoreSpeedTestStatus() -> *mut std::os::raw::c_char {
    guard(|| ret_cstring(&speed().status()), empty_cstr)
}

/// ClientCoreSpeedTestCancel：取消在途轮（即时返回 ok；轮以 cancelled 收场）。
#[no_mangle]
pub extern "C" fn ClientCoreSpeedTestCancel() -> *mut std::os::raw::c_char {
    guard(|| ret_cstring(&speed().cancel()), empty_cstr)
}

/// 进程内自检位（测试/诊断面——非导出契约；防「未消费」告警的显式标记）。
#[allow(dead_code)]
static CAPI_LOADED: AtomicBool = AtomicBool::new(false);

#[cfg(test)]
mod tests {
    use super::*;

    /// 20 个导出符号的存在性由链接期保证（cdylib 无静态断言面）——这里钉 ABI 行为面：
    /// 返回串 malloc 契约（可 free）、空入参安全、panic 兜底。
    #[test]
    fn ret_cstring_malloc_contract() {
        let p = ret_cstring("tier core x (rust, c-shared)");
        assert!(!p.is_null());
        unsafe {
            let s = CStr::from_ptr(p);
            assert_eq!(s.to_bytes(), b"tier core x (rust, c-shared)");
            libc::free(p as *mut _); // NAPI 侧 std::free 的等价物
        }
    }

    #[test]
    fn null_arg_is_safe() {
        assert_eq!(arg_str(std::ptr::null()), "");
    }

    #[test]
    fn version_cstr_shape() {
        let p = ClientCoreVersion();
        unsafe {
            let s = CStr::from_ptr(p).to_string_lossy().into_owned();
            libc::free(p as *mut _);
            assert!(s.starts_with("tier core "), "{s}");
            assert!(s.ends_with(", c-shared)"), "{s}");
        }
    }

    /// ProbeReach：坏 token → {"error":…} 信封（不 panic、不 NULL）。
    #[test]
    fn probe_reach_error_envelope() {
        let p = ClientCoreProbeReach(ret_cstring("hmw2-garbage"));
        unsafe {
            let s = CStr::from_ptr(p).to_string_lossy().into_owned();
            libc::free(p as *mut _);
            assert!(s.contains("\"error\""), "{s}");
        }
    }
}
