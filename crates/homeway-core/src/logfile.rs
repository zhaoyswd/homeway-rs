//! 带尺寸轮转的日志文件写入器（语义真源 `baseline:internal/logfile/logfile.go`）。
//!
//! events.log（摘要）/ debug.log（细节）/ relay.log（中继）三处共用一个写入器：
//! 超过 `max_bytes` 就轮转（`name → name.1 → name.2 → …`，最旧的删掉），总占用
//! 有上界。追加写（O_APPEND）：重启不清历史，轮转负责有界。
//!
//! 失败语义（Go 同义）：轮转/写失败（目录被删/权限被改）不致命——丢本段并闭句柄，
//! 下次写入重试 open；持久故障不该把出口进程带走。写入并发安全（内部锁）。

use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// 轮转参数（Go `internal/server/logging.go` / `nodestate` / `relay/logging.go` 同源）。
pub const EVENTS_MAX_BYTES: u64 = 2 << 20; // events.log 2MB×3
pub const EVENTS_BACKUPS: usize = 3;
pub const DEBUG_MAX_BYTES: u64 = 8 << 20; // debug.log 8MB×2
pub const DEBUG_BACKUPS: usize = 2;

/// 一个轮转日志文件（`&self` 写入——内部锁；Go `logfile.Writer` 同义）。
pub struct RotatingWriter {
    path: PathBuf,
    max_bytes: u64,
    backups: usize,
    slot: Mutex<Slot>,
}

struct Slot {
    f: Option<File>,
    written: u64,
}

impl RotatingWriter {
    /// 打开（或创建）`dir/name`，超过 `max_bytes` 轮转，保留 `backups` 份历史。
    /// 打开失败返回 error（调用方决定降级——日志开不了不该挡服务）。
    pub fn open(dir: &Path, name: &str, max_bytes: u64, backups: usize) -> std::io::Result<Self> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt as _;
            std::fs::DirBuilder::new().mode(0o700).create(dir).or_else(ignore_exists)?;
        }
        #[cfg(not(unix))]
        std::fs::create_dir_all(dir)?;
        let w = Self {
            path: dir.join(name),
            max_bytes,
            backups,
            slot: Mutex::new(Slot { f: None, written: 0 }),
        };
        {
            let mut slot = w.lock_slot();
            w.open_locked(&mut slot)?;
        }
        Ok(w)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 追加一段（调用方自带换行语义——本方法写 `line + '\n'`）。超限先轮转再写；
    /// 轮转/写失败丢本段（下次重试 open）。
    pub fn write_line(&self, line: &str) {
        let mut slot = self.lock_slot();
        if slot.f.is_none() && self.open_locked(&mut slot).is_err() {
            return; // 重开也没成：丢本段（Go Write 的 openError 路径同义）
        }
        if slot.written + line.len() as u64 + 1 > self.max_bytes {
            self.rotate_locked(&mut slot);
            if slot.f.is_none() {
                return; // 轮转失败（重开没成）：丢本段
            }
        }
        let ok = slot
            .f
            .as_mut()
            .is_some_and(|f| f.write_all(line.as_bytes()).and_then(|_| f.write_all(b"\n")).is_ok());
        if ok {
            slot.written += line.len() as u64 + 1;
        } else {
            // 写了一半/全失败：句柄可能已坏，闭掉走下次重开（Go 同义）
            slot.f = None;
        }
    }

    /// 关闭当前句柄（幂等；收工用）。
    pub fn close(&self) {
        if let Ok(mut slot) = self.slot.lock() {
            slot.f = None;
        }
    }

    fn lock_slot(&self) -> std::sync::MutexGuard<'_, Slot> {
        // 锁中毒 = 持锁线程 panic：日志不该把进程带走，直接按「无句柄」降级重取
        self.slot.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// 打开（或重开）追加句柄；`written` 从文件大小恢复（重启不清历史）。
    fn open_locked(&self, slot: &mut Slot) -> std::io::Result<()> {
        #[cfg(unix)]
        let mut opts = OpenOptions::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            opts.mode(0o600);
        }
        #[cfg(not(unix))]
        let mut opts = OpenOptions::new();
        let f = opts.create(true).append(true).open(&self.path)?;
        slot.written = f.metadata().map(|m| m.len()).unwrap_or(0);
        slot.f = Some(f);
        Ok(())
    }

    /// 关当前文件，历史依次后移（`.2→.3 … .1→.2`），当前改名 `.1`，重开新文件。
    /// 失败（比如目录被删）时 `f` 留 None，`write_line` 的重开路径兜底。
    fn rotate_locked(&self, slot: &mut Slot) {
        slot.f = None;
        for i in (1..=self.backups).rev() {
            let src = rotate_path(&self.path, i);
            if i == self.backups {
                let _ = std::fs::remove_file(&src); // 最旧的直接删
                continue;
            }
            let _ = std::fs::rename(&src, rotate_path(&self.path, i + 1));
        }
        if std::fs::rename(&self.path, rotate_path(&self.path, 1)).is_err() {
            return;
        }
        let _ = self.open_locked(slot);
    }
}

fn rotate_path(base: &Path, i: usize) -> PathBuf {
    let mut s = base.as_os_str().to_owned();
    s.push(format!(".{i}"));
    PathBuf::from(s)
}

#[cfg(unix)]
fn ignore_exists(e: std::io::Error) -> std::io::Result<()> {
    if e.kind() == std::io::ErrorKind::AlreadyExists {
        Ok(())
    } else {
        Err(e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("homeway-logfile-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    /// 超限轮转出 .1/.2/.3，最旧被顶掉（2MB×3 档实测缩参验证）。
    #[test]
    fn rotation_kicks_in_and_caps_backups() {
        let dir = tmpdir("rotate");
        let max = 1024;
        let w = RotatingWriter::open(&dir, "events.log", max, 3).unwrap();
        let line = "x".repeat(256);
        for _ in 0..(max / 256 * 6) {
            w.write_line(&line); // 6 倍上限 → 必然多次轮转
        }
        assert!(dir.join("events.log").exists());
        assert!(dir.join("events.log.1").exists(), "超限应轮转出 .1 档");
        assert!(dir.join("events.log.2").exists());
        assert!(dir.join("events.log.3").exists());
        assert!(!dir.join("events.log.4").exists(), "backups=3 档外不保留");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 追加语义：重开不清历史，written 恢复（跨实例续写同一文件）。
    #[test]
    fn append_across_reopen() {
        let dir = tmpdir("append");
        {
            let w = RotatingWriter::open(&dir, "debug.log", DEBUG_MAX_BYTES, 2).unwrap();
            w.write_line("第一行");
        }
        let w2 = RotatingWriter::open(&dir, "debug.log", DEBUG_MAX_BYTES, 2).unwrap();
        w2.write_line("第二行");
        drop(w2);
        let body = std::fs::read_to_string(dir.join("debug.log")).unwrap();
        assert_eq!(body, "第一行\n第二行\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 目录被删后轮转不致命：丢段；目录回来后自愈续写。（POSIX 语义：已开句柄在
    /// unlink 后仍可写——失败面在轮转 rename/reopen，Go openError 路径同义。）
    #[test]
    fn survives_rotation_failure() {
        let dir = tmpdir("rmdir");
        let w = RotatingWriter::open(&dir, "events.log", 1024, 3).unwrap();
        w.write_line("ok");
        let _ = std::fs::remove_dir_all(&dir);
        // 目录没了 + 超限触发轮转：rename/reopen 都失败 ⇒ 句柄闭掉、本段丢弃（不 panic）
        let big = "y".repeat(2048);
        w.write_line(&big);
        std::fs::create_dir_all(&dir).unwrap();
        w.write_line("恢复行");
        let body = std::fs::read_to_string(dir.join("events.log")).unwrap();
        assert_eq!(body, "恢复行\n", "轮转失败期丢段；目录回来后重开自愈");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
