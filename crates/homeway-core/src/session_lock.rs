//! identity 会话锁（R7-7e：CLI 双会话治理——R6 前置批 rekey stall 根因的转正防线）。
//!
//! **根因回顾**（R6 前置批 ①）：wireguard 单 peer 一条 keypair 链（current/previous/
//! next），同 identity 的两个并发会话（常驻 connect × 独立 files/speedtest CLI）后到
//! 握手经 ReceivedWithKeypair 顶掉 current ⇒ 双向黑洞 ⇒ 15s 自愈 rekey 反踢——
//! 「写通道长时间无进展」。产品形态（App：服务会话/隧道会话各唯一、files/term 走
//! 常驻会话自己的桥）无此面；**CLI 是唯一能踩进来的入口**。
//!
//! 治理：`<identity_dir>/session.lock` + `flock(2)`（内核语义——持有进程死亡即自动
//! 释放，无陈旧文件问题；锁内容 = 持有者自述 pid/动词/起时，供错误信息直指现场）：
//! - `connect`（常驻）：锁被活持有者占 ⇒ 明确错误退出（双常驻同 identity = 互踢形态）；
//! - `files` / `speedtest`（短命动词）：锁被活持有者占 ⇒ 明确错误（这正是 R6 根因
//!   形态——常驻会话在场时短命动词必踢掉常驻的 keypair）；
//! - 死持有者（进程已退）⇒ flock 已被内核释放，直接拿锁（内容覆写）；
//! - 临时身份（identity_dir = None）⇒ 无共享钥料，无锁面；
//! - `serve`/`relay` 不参与（出口/中继身份与客户端身份是两个空间）；
//! - `--no-session-lock` 逃生口（矩阵脚本/刻意并发测试用）。
//!
//! App 侧（第 2 棒）复用本面落实「隧道会话与服务会话不得并发」（tier Index.d.ts 的
//! 同一契约——两类会话共用设备 WG 身份）。

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// 锁文件名（落在 identity 目录——身份生命周期 = 锁生命周期）。
const LOCK_NAME: &str = "session.lock";

/// 拿锁失败：锁被活持有者占着（错误信息直指现场）。
#[derive(Debug, thiserror::Error)]
#[error("identity 已有会话在跑（pid={pid} 动词={verb}，起于 {since_ms}）——同 identity 并发会话会互踢 WG keypair（R6 前置批 ① 根因形态）：先停掉它，或换 --identity-dir，或确知无害用 --no-session-lock")]
pub struct LockHeld {
    pub pid: u32,
    pub verb: String,
    pub since_ms: u64,
}

/// 其它锁错误（IO 类——按本地环境问题报）。
#[derive(Debug, thiserror::Error)]
#[error("会话锁 {path} 操作失败：{err}")]
pub struct LockIo {
    path: PathBuf,
    #[source]
    err: std::io::Error,
}

/// 持有的会话锁（Drop 释放 flock；文件保留复用——内容只在持有期内有效）。
#[derive(Debug)]
pub struct SessionLock {
    _file: File,
    path: PathBuf,
}

impl Drop for SessionLock {
    fn drop(&mut self) {
        // flock 随 fd 关闭释放（Drop of File）；文件不删——下个持有者覆写。
        // 留一条已释放的痕迹文件比「删文件 + 创建竞态」干净（flock 内核语义保证
        // 内容只在持有期内被信任）。
        let _ = &self.path;
    }
}

/// 为一个会话动词拿 identity 锁。`verb` = 自述动词（connect/files/speedtest——
/// 错误信息消费）。identity 目录不存在则创建（与 master.key 的目录契约一致）。
pub fn acquire(identity_dir: &Path, verb: &str) -> Result<SessionLock, LockError> {
    // 密钥目录权限契约（identity.rs 同款 0700——评审 r1-C6：create_dir_all 的 0755
    // 会把新目录的权限放宽）
    use std::os::unix::fs::DirBuilderExt;
    let mut builder = std::fs::DirBuilder::new();
    builder.mode(0o700);
    builder
        .recursive(true)
        .create(identity_dir)
        .map_err(|e| LockError::Io(LockIo { path: identity_dir.to_owned(), err: e }))?;
    let path = identity_dir.join(LOCK_NAME);
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&path)
        .map_err(|e| LockError::Io(LockIo { path: path.clone(), err: e }))?;
    // flock EX|NB：内核级持有者判活（进程死 = 自动释放）——不依赖 pid 探测
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc != 0 {
        let err = std::io::Error::last_os_error();
        // EWOULDBLOCK 与 EAGAIN 在 POSIX 上同值（11）——只匹配一个（同值双匹配在
        // 部分目标报 unreachable pattern）
        let eagain = libc::EWOULDBLOCK;
        let held = matches!(err.raw_os_error(), Some(e) if e == eagain || e == libc::EACCES);
        if held {
            // 读持有者自述（拿锁失败 ⇒ 内容是有效持有者写的）
            let (pid, v, since_ms) = read_holder(&file);
            return Err(LockError::Held(LockHeld { pid, verb: v, since_ms }));
        }
        return Err(LockError::Io(LockIo { path, err }));
    }
    // 拿到锁：覆写自述（std 对 &File 有 Read/Write/Seek 三 impl——借引用即可）
    let since_ms = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0);
    use std::io::{Seek, SeekFrom};
    let mut f = &file;
    let _ = f.set_len(0);
    let _ = f.seek(SeekFrom::Start(0));
    let _ = writeln!(f, "{} {} {}", std::process::id(), verb, since_ms);
    let _ = f.flush();
    Ok(SessionLock { _file: file, path })
}

/// 拿锁错误两类。
#[derive(Debug, thiserror::Error)]
pub enum LockError {
    #[error("{0}")]
    Held(#[from] LockHeld),
    #[error("{0}")]
    Io(#[from] LockIo),
}

/// 读持有者自述（"pid verb since_ms"；解析失败给保守占位——错误信息仍可行动）。
fn read_holder(f: &File) -> (u32, String, u64) {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = f;
    let _ = f.seek(SeekFrom::Start(0));
    let mut buf = String::new();
    if f.read_to_string(&mut buf).is_err() {
        return (0, "未知".to_owned(), 0);
    }
    let mut it = buf.split_whitespace();
    let pid = it.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let verb = it.next().unwrap_or("未知").to_owned();
    let since = it.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    (pid, verb, since)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("hwlock-{name}-{:?}-{}", std::process::id(), name));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// 拿锁/放锁/复拿（同进程同 identity：放锁后应能复拿）。
    #[test]
    fn acquire_release_reacquire() {
        let dir = tmp_dir("cycle");
        {
            let lock = acquire(&dir, "connect").unwrap();
            // 同进程第二次开（新 fd = 新 open file description）⇒ 应 Held
            match acquire(&dir, "files") {
                Err(LockError::Held(h)) => {
                    assert_eq!(h.pid, std::process::id());
                    assert_eq!(h.verb, "connect");
                }
                other => panic!("应 Held，实得 {other:?}"),
            }
            drop(lock);
        }
        // 放锁后复拿：成功且自述更新
        let lock = acquire(&dir, "files").unwrap();
        drop(lock);
    }

    /// 死持有者（陈旧内容文件、flock 已释放）⇒ 直接拿锁并覆写。
    #[test]
    fn stale_holder_reclaimed() {
        let dir = tmp_dir("stale");
        std::fs::write(dir.join(LOCK_NAME), format!("{} {} {}", 999999, "connect", 12345)).unwrap();
        let lock = acquire(&dir, "speedtest").unwrap();
        // 覆写后内容 = 本进程
        let f = File::open(dir.join(LOCK_NAME)).unwrap();
        let (pid, verb, _) = read_holder(&f);
        assert_eq!(pid, std::process::id());
        assert_eq!(verb, "speedtest");
        drop(lock);
    }

    /// 目录自动创建（identity 目录可不存在）。
    #[test]
    fn dir_created() {
        let dir = tmp_dir("mk").join("nested").join("identity");
        let lock = acquire(&dir, "connect").unwrap();
        assert!(dir.join(LOCK_NAME).exists());
        drop(lock);
    }
}
