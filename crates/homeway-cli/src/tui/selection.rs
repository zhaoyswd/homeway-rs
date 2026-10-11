//! 选中状态文件（design D6 / spec「选中状态与回落」）。
//!
//! `<state>/cache/tui-selection.json`：revision 单调 + 主机 id + 会话名，多实例
//! 镜像的真源。纪律：
//! - **原子写**——临时文件 + `rename`，读方永远看到完整旧内容或完整新内容；
//! - **按内容指纹去重**——watch 拍不靠 revision 单调（无锁跨进程读改写会撞号），
//!   内容没变就不通知；
//! - **运行期坏读忽略**——解析失败/文件消失在运行期一律「忽略本次读、保持当前
//!   选中」（指纹照样前移，避免对同一份坏内容反复重读），只有**启动期**的失败
//!   才走回落链（spec「选中状态与回落」）；
//! - **回落不写回**——本模块只提供读写原语；「仅用户发起的切换持久化」由调用方
//!   纪律保证（app 层回落路径不得调 [`SelectionFile::store`]）。
//!
//! 落 `cache/` 的理由：该层可清理（reset cache 整目录删）且不进 export/import
//! 白名单面——选中是设备本地偏好，不随状态迁移本就合理（r2 D-9/D-1 定案）。

// 消费方（app 层的全局唯一接线，tasks 4.6）随下一批落地——期间豁免 dead_code。
#![allow(dead_code)]

use std::fs;
use std::hash::Hasher;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

/// 文件名（`<state>/cache/` 下）。
const FILE_NAME: &str = "tui-selection.json";

/// 选中状态（wire 形态 = JSON；字段只增不改）。
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug, Default)]
pub struct SelectionState {
    /// 单调递增（写方各自 +1；镜像判定不依赖它的全局单调性——last-writer-wins）。
    #[serde(default)]
    pub revision: u64,
    /// 主机标识：远端 = daemon 主机表 id；本机条目 = 固定哨兵 [`LOCAL_HOST_ID`]。
    #[serde(default)]
    pub host_id: Option<String>,
    /// 会话名（spec：选中以会话名为键）。
    #[serde(default)]
    pub session: Option<String>,
}

/// 选中文件句柄。`poll` 的指纹簿记在句柄内（每实例一份）。
pub struct SelectionFile {
    path: PathBuf,
    seen: Mutex<Option<u64>>,
}

impl SelectionFile {
    /// `state_dir` = 统一 state 根（文件落 `<state>/cache/`）。
    pub fn new(state_dir: &Path) -> Self {
        SelectionFile {
            path: state_dir.join("cache").join(FILE_NAME),
            seen: Mutex::new(None),
        }
    }

    /// 文件路径（错误文案用）。
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// **启动期**读取：缺失/解析失败 ⇒ `None`（调用方走回落链）。
    pub fn load(&self) -> Option<SelectionState> {
        read_state(&self.path)
    }

    /// **运行期** watch 拍：内容指纹与上次不同才返回 `Some`。
    /// 解析失败（坏内容）⇒ 指纹照样前移、返回 `None`（保持当前选中，
    /// 不重读同一坏内容）；文件消失 ⇒ 指纹归零、返回 `None`。
    pub fn poll(&self) -> Option<SelectionState> {
        let bytes = fs::read(&self.path).ok();
        let fp = bytes.as_deref().map(hash_bytes);
        let state = bytes.and_then(|b| serde_json::from_slice(&b).ok());
        let mut seen = self.seen.lock().unwrap_or_else(|e| e.into_inner());
        if *seen == fp {
            return None;
        }
        *seen = fp;
        state
    }

    /// 原子写：临时文件 + `rename`（同目录 ⇒ 同文件系统，rename 原子）。
    pub fn store(&self, sel: &SelectionState) -> io::Result<()> {
        let dir = self.path.parent().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "选中文件路径无父目录")
        })?;
        fs::create_dir_all(dir)?;
        let body = serde_json::to_vec_pretty(sel)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
        let tmp = dir.join(format!(
            "{}.tmp.{}",
            FILE_NAME,
            unique_suffix()
        ));
        fs::write(&tmp, &body)?;
        fs::rename(&tmp, &self.path)?;
        // 写方自己的指纹簿记同步前移：下一次 poll 不会把自己写的内容当镜像推回。
        *self.seen.lock().unwrap_or_else(|e| e.into_inner()) = Some(hash_bytes(&body));
        Ok(())
    }
}

/// 读 + 解析（缺失/坏内容 ⇒ `None`；原子写下不会出现半截内容）。
fn read_state(path: &Path) -> Option<SelectionState> {
    let bytes = fs::read(path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn hash_bytes(bytes: &[u8]) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    h.write(bytes);
    h.finish()
}

/// 临时文件后缀（进程内唯一即可：rename 同目录竞争由文件系统原子性兜底）。
fn unique_suffix() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    (std::process::id() as u64) << 32 | N.fetch_add(1, Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::local::LOCAL_HOST_ID;

    /// 每用例独立临时目录（仓内无 tempfile 依赖；进程 id + 计数器足够隔离）。
    fn temp_state_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "hwtui-sel-{}-{}-{}",
            std::process::id(),
            tag,
            unique_suffix()
        ));
        fs::create_dir_all(d.join("cache")).expect("建临时 cache 目录");
        d
    }

    #[test]
    fn store_load_roundtrip() {
        let dir = temp_state_dir("roundtrip");
        let f = SelectionFile::new(&dir);
        let sel = SelectionState {
            revision: 7,
            host_id: Some("peer-abcd1234".into()),
            session: Some("main".into()),
        };
        f.store(&sel).expect("store");
        assert_eq!(f.load(), Some(sel), "store→load 往返一致");
    }

    #[test]
    fn missing_file_is_none() {
        let dir = temp_state_dir("missing");
        let f = SelectionFile::new(&dir);
        assert_eq!(f.load(), None, "缺失 = 启动期回落");
        assert_eq!(f.poll(), None, "缺失 = watch 静默");
        assert_eq!(f.poll(), None, "缺失态下重复 poll 不抖动");
    }

    #[test]
    fn corrupted_runtime_read_is_ignored_once_then_recovered() {
        let dir = temp_state_dir("corrupt");
        let reader = SelectionFile::new(&dir); // 镜像方句柄（与写方分开——各自有指纹簿记）
        fs::write(reader.path(), b"{not json").expect("写坏内容");
        // 运行期：坏内容 = 忽略本次读（None），且指纹按坏内容前移（第二次不再报）。
        assert_eq!(reader.poll(), None, "坏内容运行期忽略");
        assert_eq!(reader.poll(), None, "同一坏内容不反复上报");
        // 内容恢复成合法态后照常拾取（另一实例的写方句柄写入）。
        let writer = SelectionFile::new(&dir);
        let sel = SelectionState { revision: 3, host_id: Some(LOCAL_HOST_ID.into()), session: None };
        writer.store(&sel).expect("恢复写入");
        assert_eq!(reader.poll(), Some(sel), "内容变化后照常镜像");
    }

    #[test]
    fn startup_corrupt_falls_back() {
        let dir = temp_state_dir("startup-corrupt");
        let f = SelectionFile::new(&dir);
        fs::write(f.path(), b"{{{nope").expect("写坏内容");
        assert_eq!(f.load(), None, "启动期坏内容 = 回落链");
    }

    #[test]
    fn poll_dedups_by_content() {
        let dir = temp_state_dir("dedup");
        let writer = SelectionFile::new(&dir);
        let reader = SelectionFile::new(&dir);
        let sel = SelectionState {
            revision: 1,
            host_id: Some("peer-1".into()),
            session: Some("s".into()),
        };
        writer.store(&sel).expect("首次写");
        assert_eq!(reader.poll(), Some(sel.clone()), "首拍上报");
        assert_eq!(reader.poll(), None, "内容未变 = 去重");
        // 逐字节相同的内容重写（同 revision 同字段）⇒ 内容指纹未变 ⇒ 不上报。
        writer.store(&sel.clone()).expect("同内容重写");
        assert_eq!(reader.poll(), None, "同内容重写不上报（指纹锚是内容）");
        // 内容真变（换了主机）⇒ 上报。两个写方即便撞出相同 revision，
        // 内容不同 ⇒ 指纹不同 ⇒ 都会上报（r2 N-5：不以 revision 为去重锚）。
        let changed = SelectionState { revision: 1, host_id: Some("peer-2".into()), session: sel.session };
        writer.store(&changed).expect("切换写");
        assert_eq!(reader.poll(), Some(changed), "内容变化 = 上报");
    }

    #[test]
    fn writer_does_not_echo_own_write() {
        let dir = temp_state_dir("noecho");
        let f = SelectionFile::new(&dir);
        f.store(&SelectionState {
            revision: 1,
            host_id: Some(LOCAL_HOST_ID.into()),
            session: Some("s".into()),
        })
        .expect("写");
        assert_eq!(f.poll(), None, "写方句柄不把自己的写入当镜像推回（防自回声）");
    }

    #[test]
    fn store_leaves_no_temp_files() {
        let dir = temp_state_dir("notemp");
        let f = SelectionFile::new(&dir);
        f.store(&SelectionState::default()).expect("store");
        let leftovers: Vec<_> = fs::read_dir(dir.join("cache"))
            .expect("列 cache")
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains(".tmp."))
            .collect();
        assert!(leftovers.is_empty(), "原子写不留临时文件：{leftovers:?}");
    }
}
