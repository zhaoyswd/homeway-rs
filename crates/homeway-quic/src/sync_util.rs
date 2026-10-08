//! 岛侧线程卫生小件（自持单源——不引第三个 crate；设计 §1.2 代价①）。
//!
//! 与 `homeway-core` 的 `syncutil` 语义**同源同文**（`lock_unpoison` 不 panic、
//! `log_spawn_failed` 行文 `<名> 启动失败（{e}）—— <后果>`），但此处自持一份：
//! 岛是叶子 crate，不得依赖 `homeway-core`（隔离门层 0）。
//!
//! carve-out（与仓内同口径）：**分配失败 = abort**，不在本模块可处置面。

use std::sync::{Mutex, MutexGuard};

use crate::cmd::Logf;

/// 锁中毒不 panic（持锁线程 panic 后锁数据仍可用——`into_inner` 取出继续）。
///
/// 岛内一切取锁（含 `Drop → stop` 链）都走本件：c-shared 宿主进程里任何一处
/// `expect("…锁中毒")` 都等于把「一次锁中毒」升级成「扩展进程死」。
pub(crate) fn lock_unpoison<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// 派生线程 spawn 失败的统一记行（Q-F F7 同文）：`<线程名> 启动失败（{e}）—— <后果>`。
pub(crate) fn log_spawn_failed(logf: &Logf, thread_name: &str, e: &std::io::Error, consequence: &str) {
    (logf)(&format!("{thread_name} 启动失败（{e}）—— {consequence}"));
}
