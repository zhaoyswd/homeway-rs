//! 同步与线程卫生的单源小件（Q-F F6/F7 收敛面）。
//!
//! 为什么独立成模块：`lock_unpoison` 此前在 `facade::tun_shared`（facade 域内）与
//! `facade::service_op` 的私有 trait（`.lup()`）两处并存，`session`/`wgcore` 面则
//! 裸用 `expect("…锁中毒")`——持锁线程 panic 后，c-shared 宿主进程里任何一处
//! `expect` 都等于把「一次锁中毒」升级成「扩展进程死」。本模块把三件收敛为单源：
//!
//! - [`lock_unpoison`]：锁中毒不 panic（数据仍可用，into_inner 取出继续）；
//! - [`join_bounded`]：JoinHandle 的分片有界等待（收工预算面，Q-F F6-4）；
//! - [`log_spawn_failed`]：派生线程 spawn 失败的统一记行（Q-F F7——失败不可注入，
//!   故抽成可直接单测的纯函数）。
//!
//! carve-out（Q-F F6-3 明示）：**分配失败 = abort**（`handle_alloc_error`）不在本
//! 模块可处置面——进程级 abort 无栈可退，任何「不 panic」纪律都覆盖不到它。

use std::sync::{Mutex, MutexGuard, RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::Logf;

/// 锁中毒不 panic（持锁线程 panic 后锁数据仍可用——`into_inner` 取出继续；一致性
/// 问题交给该锁的语义面自愈）。
///
/// **单源范围（如实口径，代码门 ⑥-1）**：本批收敛的是 `expect("…锁中毒")` 一族与
/// facade 内三份私有 `LockUnpoison` trait（`.lup()`）——session / recover /
/// `facade::tun_exec` / `wgcore::Client` 面 / 本模块调用点全走本件。**等价内联副本
/// 仍在**（`unwrap_or_else(|e| e.into_inner())` 直写，约 5 个文件 ~39 处；语义完全
/// 一致），未纳入本批。
pub(crate) fn lock_unpoison<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// [`RwLock`] 读侧的同义件（session 的世代/域名重解析锁）。
pub(crate) fn read_unpoison<T>(m: &RwLock<T>) -> RwLockReadGuard<'_, T> {
    m.read().unwrap_or_else(|e| e.into_inner())
}

/// [`RwLock`] 写侧的同义件。
pub(crate) fn write_unpoison<T>(m: &RwLock<T>) -> RwLockWriteGuard<'_, T> {
    m.write().unwrap_or_else(|e| e.into_inner())
}

/// JoinHandle 的有界等待：到点返回 false 并**放弃 join**（句柄析构即分离，线程自行
/// 退出；调用方按放行自退记行）。真等到则 join 收口返回 true。
pub(crate) fn join_bounded(h: JoinHandle<()>, deadline: Instant) -> bool {
    if wait_finished(&h, deadline) {
        let _ = h.join();
        true
    } else {
        false
    }
}

/// 只等待不消费句柄（调用方要在到点后另作处置——例：交收割线程）。
pub(crate) fn wait_finished(h: &JoinHandle<()>, deadline: Instant) -> bool {
    while !h.is_finished() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    h.is_finished()
}

/// 派生线程 spawn 失败的统一记行（Q-F F7）：`<线程名> 启动失败（{e}）—— <后果>`。
/// 域前缀由调用方的 logf 自带（隧道域 `tier-core: `／服务会话 `服务会话: `／桥宿主
/// `<桥名>: `）——故此处不再拼域。
pub(crate) fn log_spawn_failed(logf: &Logf, thread_name: &str, e: &std::io::Error, consequence: &str) {
    (logf)(&format!("{thread_name} 启动失败（{e}）—— {consequence}"));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    /// 毒锁：持锁线程 panic 后取锁不得 panic，且数据仍可读。
    #[test]
    fn lock_unpoison_survives_poisoned_mutex() {
        let m = std::sync::Arc::new(Mutex::new(7u32));
        let m2 = std::sync::Arc::clone(&m);
        let _ = std::thread::spawn(move || {
            let _g = m2.lock().unwrap();
            panic!("持锁线程炸了");
        })
        .join();
        assert!(m.is_poisoned());
        assert_eq!(*lock_unpoison(&m), 7, "毒锁仍可取（into_inner）");
    }

    /// join_bounded 两形态：已结束 ⇒ true 且真 join；卡死 ⇒ 到点 false 放行。
    #[test]
    fn join_bounded_both_forms() {
        let h = std::thread::spawn(|| {});
        assert!(join_bounded(h, Instant::now() + Duration::from_secs(2)));
        let (tx, rx) = mpsc::channel::<()>();
        let h = std::thread::spawn(move || {
            let _ = rx.recv();
        });
        let t0 = Instant::now();
        assert!(
            !join_bounded(h, Instant::now() + Duration::from_millis(120)),
            "卡死线程到点放行"
        );
        assert!(t0.elapsed() < Duration::from_secs(2), "不得越过期限久等");
        drop(tx); // 放行线程自行退出
    }

    /// spawn 失败记行行文（F7-5 可测面）：`<名> 启动失败（{e}）—— <后果>`。
    #[test]
    fn spawn_failed_line_shape() {
        let (logf, rx) = sink();
        let e = std::io::Error::other("Resource temporarily unavailable");
        log_spawn_failed(&logf, "巡检线程", &e, "本会话失去自愈巡检");
        let line = rx.recv().unwrap();
        assert!(line.starts_with("巡检线程 启动失败（"), "{line}");
        assert!(line.ends_with("—— 本会话失去自愈巡检"), "{line}");
        assert!(line.contains("Resource temporarily unavailable"), "{line}");
    }

    fn sink() -> (Logf, mpsc::Receiver<String>) {
        let (tx, rx) = mpsc::channel();
        (std::sync::Arc::new(move |s: &str| {
            let _ = tx.send(s.to_owned());
        }), rx)
    }
}
