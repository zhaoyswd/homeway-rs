//! tcc — macOS 文件夹授权（Transparency, Consent, and Control）的探测与预授权。
//!
//! 背景：进程首次访问「文稿 / 桌面 / 下载 / 可移动卷 / 网络卷 / iCloud 云盘」时，
//! `open`/`read_dir` 等系统调用**阻塞到用户在屏幕上点『允许 / 不允许』**（弹窗按
//! 「负责进程」归属——launchd 起的出口守护 = homeway 自己；无图形会话的进程则
//! 直接 EPERM 不弹窗）。远端用户看到的却是「请求不返回」——像卡死。
//!
//! 本模块给两层抓手：
//! - [`probe_read_dir`]：带预算的目录枚举探测——预算内返回即「已授权」，EPERM/EACCES
//!   即「被拒」，预算耗尽即「弹窗未点」（探测线程继续挂到用户点掉为止）；
//! - [`protected_roots`]：出口机上最常见的三个受保护根（`homeway serve grant` 用）。
//!
//! 只在 macOS 有意义；其它平台 `protected_roots` 为空、探测恒 [`GrantStatus::Granted`]。

use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;

/// 单目录探测结论。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrantStatus {
    /// 预算内完成——本进程可访问（TCC 已授权或该路径本就不受保护）。
    Granted,
    /// 返回了权限错误（用户点了『不允许』，或无图形会话被系统直接拒绝）。
    Denied(&'static str),
    /// 预算耗尽——大概率弹窗在等用户（也可能是慢盘；重试即知）。
    Pending,
}

/// macOS 上最常见的三个受保护根（`$HOME` 下；files/term 的日常访问面）。
/// 可移动卷 / 网络卷（`/Volumes`）是另外的授权类别，不在此列——提示文案提及。
pub fn protected_roots() -> Vec<(&'static str, PathBuf)> {
    #[cfg(target_os = "macos")]
    {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default();
        [("文稿", "Documents"), ("桌面", "Desktop"), ("下载", "Downloads")]
            .into_iter()
            .map(|(name, sub)| (name, home.join(sub)))
            .collect()
    }
    #[cfg(not(target_os = "macos"))]
    {
        Vec::new()
    }
}

/// 带预算的 `read_dir` 探测：预算内返回/权限错误即结论；预算耗尽 = [`GrantStatus::Pending`]
/// （探测线程 detach，继续挂到系统调用返回——用户点掉弹窗后自然退出，结论丢弃）。
pub fn probe_read_dir(path: &std::path::Path, wait: Duration) -> GrantStatus {
    // 实现平台无关（read_dir + 探测线程 + 预算）：macOS 上 TCC 未决会让 read_dir
    // 阻塞到用户点弹窗 ⇒ 走 Pending 分支；其它平台 read_dir 立即返回真实结果，
    // Pending 只剩慢盘含义。**不做 `#[cfg]` 特判**——曾因非 macOS 分支恒 Granted
    // 而 unused-import 连挂 CI 三平台（2026-10-15）。
    let p = path.to_path_buf();
    let (tx, rx) = mpsc::channel();
    // 探测线程：成功/失败经 channel 回执；Pending 时它继续阻塞在 read_dir 里
    // （macOS TCC 弹窗）直到用户点掉——daemon 侧调用方须用独立线程跑本函数。
    let spawned = std::thread::Builder::new()
        .name("hw-tcc-probe".into())
        .spawn(move || {
            let r = std::fs::read_dir(&p);
            let _ = tx.send(r.is_ok());
        });
    if spawned.is_err() {
        return GrantStatus::Denied("probe_spawn_failed");
    }
    match rx.recv_timeout(wait) {
        Ok(true) => GrantStatus::Granted,
        Ok(false) => GrantStatus::Denied("permission"), // EPERM/EACCES/NotFound 等（macOS 含用户点『不允许』）
        Err(mpsc::RecvTimeoutError::Timeout) => GrantStatus::Pending,
        Err(mpsc::RecvTimeoutError::Disconnected) => GrantStatus::Denied("probe_panicked"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 已可访问的目录 ⇒ Granted（临时目录不受 TCC 保护，探测即回）。
    #[test]
    fn probe_grants_accessible_dir() {
        let d = std::env::temp_dir().join(format!("hw-tcc-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let st = probe_read_dir(&d, Duration::from_secs(3));
        assert_eq!(st, GrantStatus::Granted);
        let _ = std::fs::remove_dir_all(&d);
    }

    /// 权限错误 ⇒ Denied（对不存在路径 read_dir 返回 NotFound——也归入 Denied 文案面，
    /// 真实 TCC 拒绝是 EPERM；两者对调用方的处置相同：不可用 + 提示去系统设置）。
    #[test]
    fn probe_maps_error_to_denied() {
        let st = probe_read_dir(
            &std::env::temp_dir().join("hw-tcc-no-such-dir-xyz"),
            Duration::from_secs(3),
        );
        assert!(matches!(st, GrantStatus::Denied(_)), "实得 {st:?}");
    }

    /// 受保护根三个、都在 $HOME 下；非 macOS 为空（本测试在 macOS 上跑即为三条）。
    #[test]
    fn protected_roots_shape() {
        let roots = protected_roots();
        #[cfg(target_os = "macos")]
        {
            assert_eq!(roots.len(), 3);
            assert!(roots.iter().all(|(_, p)| p.starts_with(std::path::Path::new(
                &std::env::var_os("HOME").unwrap_or_default()
            ))));
        }
        #[cfg(not(target_os = "macos"))]
        assert!(roots.is_empty());
    }
}
