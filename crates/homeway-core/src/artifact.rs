//! 状态工件（export / import / reset cache；B0-2b P1-6）。
//!
//! 语义真源 `baseline:internal/nodestate/artifact.go`（role-management 1.3，D7）+
//! `internal/daemon/artifact_cli.go`（CLI 接线）：
//!
//! - **export**：不变量四件（config.toml + serve/ + relay/ + client/）→ 单文件
//!   未压缩 tar（0600、固定布局 `homeway-export/`；不含 cache/ 与任何 socket/lock
//!   ——瞬态不参与恢复）。定序输出（顶层 → config → relay → serve → client/，
//!   子项按名排序）——人可 diff。
//! - **import**：布局校验（缺件/多件/异物拒）+ 逐条目路径安全校验（前缀 =
//!   `homeway-export/`、无 ../ 绝对路径/符号链接；条目数/总量/单文件三上限——
//!   半可信输入）→ 临时目录解包 → 落位序（旧四件改名 `.import-old-<ts>/` →
//!   rename 入位 → 成功删旧、任一步失败回滚）。目标 state 的统一进程 MUST 在停
//!   （锁试探，在跑 = 可行动错误拒绝）。
//! - **reset cache**：清空 `<state>/cache/`（在跑拒绝——日志文件被进程持有）；
//!   L1/L2 与 `migration-backup-<ts>/` 不碰。
//!
//! tar 面：**自管 ustar 读写出最小面**（无第三方依赖——工件是我们自己的固定布局，
//! 名字 ≤100 字节、无 pax 扩展需求；Go archive/tar 对同布局也产 ustar 头，两侧
//! 工件互通）。pax 扩展头（'x'/'g'）与本布局不会出现，读到达即拒（可行动文案）。

use std::io::{Read, Write};
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::Path;

/// 工件顶层目录名（Go exportTop）。
pub const EXPORT_TOP: &str = "homeway-export";

/// client/ 点名四件（r1 中-12）；serve/ 与 relay/ 走白名单全目录。
const CLIENT_PIECES: [&str; 4] = ["identity", "hosts.json", "forwards.json", "socks.json"];
/// serve/ 白名单（revoked.jsonl 随工件进出：漏了它 = 备份后已吊销凭证复活）。
const SERVE_ALLOWED: [&str; 3] = ["key.bin", "tokens.jsonl", "revoked.jsonl"];
/// relay/ 白名单。
const RELAY_ALLOWED: [&str; 1] = ["relay.key"];

/// 工件规模上限（半可信输入的第一道界；配合单文件 64MB 上限——FIX-55 同义）。
const MAX_ENTRIES: usize = 4096;
const MAX_TOTAL_BYTES: u64 = 256 << 20;
const MAX_FILE_BYTES: u64 = 64 << 20;

// ---------------------------------------------------------------------------
// export
// ---------------------------------------------------------------------------

/// 不变量四件打包为未压缩 tar（0600）。config.toml 缺失 = 三层布局未就位，可行动
/// 错误（先跑统一进程完成迁移）。白名单外条目**告警跳过**（FIX-51：静默丢 = 用户
/// 以为备份了整份 state）。
pub fn export(state_dir: &Path, dest: &Path) -> Result<(), String> {
    let config_path = state_dir.join("config.toml");
    let config_data = match std::fs::read(&config_path) {
        Ok(d) => d,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(format!(
                "export: {} 里没有 config.toml——三层布局未就位（先启动统一进程完成迁移）",
                state_dir.display()
            ));
        }
        Err(e) => return Err(e.to_string()),
    };
    let mut tar = TarWriter::new();
    tar.dir(EXPORT_TOP)?;
    tar.file(&format!("{EXPORT_TOP}/config.toml"), &config_data)?;
    // 定序：relay → serve → client（Go 同序——确定性输出，人可 diff）。
    for sub in [
        ("relay", state_dir.join("relay"), Whitelist::Fixed(&RELAY_ALLOWED)),
        ("serve", state_dir.join("serve"), Whitelist::Fixed(&SERVE_ALLOWED)),
        ("client", state_dir.join("client"), Whitelist::Pieces(&CLIENT_PIECES)),
    ] {
        tar.dir(&format!("{EXPORT_TOP}/{}", sub.0))?;
        let (names, skipped) = match collect_names(&sub.1, sub.2) {
            Ok(v) => v,
            Err(NotReady::Absent) => continue, // 该角色从未跑过：空目录照样进工件
            Err(NotReady::Io(e)) => return Err(e),
        };
        if !skipped.is_empty() {
            eprintln!(
                "homeway: ⚠️ export 跳过 {}/ 下 {} 个白名单外条目（不进工件，也不影响原 state）：{}",
                sub.0,
                skipped.len(),
                trunc_list(&skipped, 5)
            );
        }
        for n in names {
            let p = sub.1.join(&n);
            let Ok(meta) = std::fs::symlink_metadata(&p) else {
                continue; // 点名件缺席（该角色未用过）：跳过
            };
            if meta.is_dir() {
                tar_dir(&mut tar, &format!("{EXPORT_TOP}/{}/{}", sub.0, n), &p)?;
            } else {
                let data = std::fs::read(&p).map_err(|e| e.to_string())?;
                tar.file(&format!("{EXPORT_TOP}/{}/{}", sub.0, n), &data)?;
            }
        }
    }
    let bytes = tar.finish();
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .mode(0o600)
        .open(dest)
        .map_err(|e| e.to_string())?;
    f.write_all(&bytes).map_err(|e| e.to_string())?;
    Ok(())
}

enum Whitelist<'a> {
    /// 白名单全目录（serve/relay）：目录内白名单件全取。
    Fixed(&'a [&'a str]),
    /// 点名件（client 四件）。
    Pieces(&'a [&'a str]),
}

enum NotReady {
    Absent,
    Io(String),
}

fn collect_names(dir: &Path, wl: Whitelist<'_>) -> Result<(Vec<String>, Vec<String>), NotReady> {
    let (names, skipped): (Vec<String>, Vec<String>) = match wl {
        Whitelist::Fixed(allowed) => {
            let entries = match std::fs::read_dir(dir) {
                Ok(e) => e,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(NotReady::Absent),
                Err(e) => return Err(NotReady::Io(e.to_string())),
            };
            let (mut keep, mut skip) = (Vec::new(), Vec::new());
            for e in entries.flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                if allowed.contains(&name.as_str()) {
                    keep.push(name);
                } else {
                    skip.push(name);
                }
            }
            keep.sort();
            skip.sort();
            (keep, skip)
        }
        Whitelist::Pieces(pieces) => {
            // 点名件集合之外的条目也要告警（FIX-51）。
            let mut skip = Vec::new();
            if let Ok(entries) = std::fs::read_dir(dir) {
                for e in entries.flatten() {
                    let name = e.file_name().to_string_lossy().into_owned();
                    if !pieces.contains(&name.as_str()) {
                        skip.push(name);
                    }
                }
            }
            skip.sort();
            (pieces.iter().map(|s| s.to_string()).collect(), skip)
        }
    };
    Ok((names, skipped))
}

/// 递归打包目录（子项按名排序；0600/0700 权限语义）。
fn tar_dir(tar: &mut TarWriter, name: &str, src: &Path) -> Result<(), String> {
    tar.dir(&format!("{name}/"))?;
    let mut names: Vec<String> = std::fs::read_dir(src)
        .map_err(|e| e.to_string())?
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    for n in names {
        let p = src.join(&n);
        let meta = std::fs::symlink_metadata(&p).map_err(|e| e.to_string())?;
        if meta.is_dir() {
            tar_dir(tar, &format!("{name}/{n}"), &p)?;
        } else {
            let data = std::fs::read(&p).map_err(|e| e.to_string())?;
            tar.file(&format!("{name}/{n}"), &data)?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// import
// ---------------------------------------------------------------------------

/// 工件条目（内存形态——工件小，key/token/identity 都是 KB 级，整读校验 + 解包两用）。
struct Entry {
    name: String,
    dir: bool,
    data: Vec<u8>,
}

/// 工件落位（校验 → 解包 → 落位序 → 回滚）。成功后旧四件备份目录一并清理。
pub fn import(state_dir: &Path, artifact: &Path) -> Result<(), String> {
    std::fs::create_dir_all(state_dir).map_err(|e| e.to_string())?;
    // 目标进程在停（锁试探；在跑 = 可行动错误拒绝）。
    if lock_held(state_dir)? {
        return Err(format!(
            "import: 目标 state 的统一进程在跑（{}/lock 被持有）——先停进程再导入",
            state_dir.display()
        ));
    }
    let entries = read_artifact(artifact)?;
    validate_artifact(&entries)?;
    let ts = timestamp();
    let tmp_dir = state_dir.join(format!(".import-tmp-{ts}"));
    let old_dir = state_dir.join(format!(".import-old-{ts}"));
    if let Err(e) = extract_artifact(&entries, &tmp_dir) {
        let _ = std::fs::remove_dir_all(&tmp_dir);
        return Err(e);
    }
    if let Err(e) = place_pieces(state_dir, &tmp_dir.join(EXPORT_TOP), &old_dir) {
        // place_pieces 内部已回滚（新件撤掉、旧四件归位）；这里清场临时目录。
        let _ = std::fs::remove_dir_all(&tmp_dir);
        let empty = std::fs::read_dir(&old_dir).map(|mut d| d.next().is_none()).unwrap_or(true);
        if empty {
            let _ = std::fs::remove_dir_all(&old_dir);
            return Err(e);
        }
        return Err(format!("{e}（旧件已回滚归位；{} 残留可手工检查删除）", old_dir.display()));
    }
    let _ = std::fs::remove_dir_all(&tmp_dir);
    // Go os.RemoveAll 语义：不存在 = 成功（全新 state 无旧件 ⇒ 目录从未建）。
    match std::fs::remove_dir_all(&old_dir) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            return Err(format!("import: 落位成功但清理 {} 失败（{e}）——可手工删除", old_dir.display()));
        }
    }
    Ok(())
}

/// 读 tar 并做逐条目安全校验（前缀/路径段/类型/规模四界——工件来自用户，半可信）。
fn read_artifact(path: &Path) -> Result<Vec<Entry>, String> {
    let mut f = std::fs::File::open(path).map_err(|e| format!("import: 打开工件失败：{e}"))?;
    let mut raw = Vec::new();
    f.read_to_end(&mut raw).map_err(|e| format!("import: 读工件失败：{e}"))?;
    let mut tr = TarReader::new(&raw).map_err(|e| format!("import: 工件不是合法 tar：{e}"))?;
    let mut entries = Vec::new();
    let mut total_bytes: u64 = 0;
    while let Some((name, dir, data)) = tr.next().map_err(|e| format!("import: 工件不是合法 tar：{e}"))? {
        if entries.len() >= MAX_ENTRIES {
            return Err(format!("import: 工件条目数超上限（{MAX_ENTRIES}）——拒绝"));
        }
        if !dir {
            total_bytes += data.len() as u64;
            if total_bytes > MAX_TOTAL_BYTES {
                return Err(format!("import: 工件总字节超上限（{} MB）——拒绝", MAX_TOTAL_BYTES >> 20));
            }
            if data.len() as u64 > MAX_FILE_BYTES {
                return Err(format!("import: 违规条目 {name:?}（单文件 {} MB 过大）", data.len() >> 20));
            }
        }
        let name = name.trim_end_matches('/').to_owned();
        if name.starts_with('/') {
            return Err(format!("import: 违规条目 {name:?}（绝对路径）"));
        }
        if !name.starts_with(&format!("{EXPORT_TOP}/")) && name != EXPORT_TOP {
            return Err(format!("import: 违规条目 {name:?}（前缀必须 = {EXPORT_TOP}/）"));
        }
        for part in name.split('/') {
            if part == ".." || part == "." || part.is_empty() {
                return Err(format!("import: 违规条目 {name:?}（.. / 空路径段）"));
            }
        }
        entries.push(Entry { name, dir, data });
    }
    Ok(entries)
}

/// 布局校验：必件齐、无多件、无异物（目录当文件/文件当目录）。
fn validate_artifact(entries: &[Entry]) -> Result<(), String> {
    let find = |name: &str| entries.iter().find(|e| e.name == name);
    if find(EXPORT_TOP).is_none() {
        return Err(format!("import: 缺件 {EXPORT_TOP}/（工件顶层目录）"));
    }
    match find(&format!("{EXPORT_TOP}/config.toml")) {
        Some(e) if !e.dir => {}
        _ => return Err(format!("import: 缺件 {EXPORT_TOP}/config.toml")),
    }
    for sub in ["serve", "relay", "client"] {
        match find(&format!("{EXPORT_TOP}/{sub}")) {
            Some(e) if e.dir => {}
            _ => return Err(format!("import: 缺件 {EXPORT_TOP}/{sub}/")),
        }
    }
    for e in entries {
        if e.name == EXPORT_TOP {
            continue;
        }
        let rel = e.name.strip_prefix(&format!("{EXPORT_TOP}/")).unwrap_or("");
        let parts: Vec<&str> = rel.split('/').collect();
        match parts[0] {
            "config.toml" => {
                if e.dir || parts.len() != 1 {
                    return Err(format!("import: 多件/异物 {:?}", e.name));
                }
            }
            "serve" => {
                if e.dir && parts.len() == 1 {
                    continue;
                }
                if parts.len() != 2 || e.dir || !SERVE_ALLOWED.contains(&parts[1]) {
                    return Err(format!(
                        "import: 多件/异物 {:?}（serve/ 只接受 key.bin / tokens.jsonl / revoked.jsonl）",
                        e.name
                    ));
                }
            }
            "relay" => {
                if e.dir && parts.len() == 1 {
                    continue;
                }
                if parts.len() != 2 || e.dir || !RELAY_ALLOWED.contains(&parts[1]) {
                    return Err(format!("import: 多件/异物 {:?}（relay/ 只接受 relay.key）", e.name));
                }
            }
            "client" => {
                if e.dir && parts.len() == 1 {
                    continue;
                }
                if parts.len() < 2 {
                    return Err(format!("import: 多件/异物 {:?}", e.name));
                }
                if !CLIENT_PIECES.contains(&parts[1]) {
                    return Err(format!(
                        "import: 多件/异物 {:?}（client/ 只接受 identity/ hosts.json forwards.json socks.json）",
                        e.name
                    ));
                }
                if parts[1] != "identity" && (parts.len() != 2 || e.dir) {
                    return Err(format!("import: 多件/异物 {:?}", e.name));
                }
            }
            _ => {
                return Err(format!(
                    "import: 多件/异物 {:?}（固定布局只含 config.toml 与 serve/ relay/ client/）",
                    e.name
                ));
            }
        }
    }
    Ok(())
}

/// 解包到临时目录（文件 0600、目录 0700）。
fn extract_artifact(entries: &[Entry], tmp_dir: &Path) -> Result<(), String> {
    for e in entries {
        if e.name == EXPORT_TOP {
            continue;
        }
        let dst = tmp_dir.join(&e.name);
        if e.dir {
            std::fs::create_dir_all(&dst).map_err(|err| err.to_string())?;
            continue;
        }
        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent).map_err(|err| err.to_string())?;
        }
        std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .mode(0o600)
            .open(&dst)
            .and_then(|mut f| f.write_all(&e.data))
            .map_err(|err| err.to_string())?;
    }
    Ok(())
}

/// 落位序（r1 低-4）：旧四件改名进 old_dir → 新件 rename 入位 → 成功后由调用方
/// 删旧。rename(2) 不能原子替换非空目录，非原子窗口由「先搬走旧件 + 失败回滚」
/// 收口。工件里缺席的件维持「工件即全量」语义。
fn place_pieces(state_dir: &Path, src_dir: &Path, old_dir: &Path) -> Result<(), String> {
    const PIECES: [&str; 4] = ["config.toml", "serve", "relay", "client"];
    let mut moved_old: Vec<String> = Vec::new();
    let mut placed: Vec<String> = Vec::new();
    let result = place_pieces_inner(state_dir, src_dir, old_dir, &PIECES, &mut moved_old, &mut placed);
    if let Err(e) = &result {
        // 失败回滚：先撤新入位件，再把旧四件归位（顺序不可反——rename 不能盖非空目录）。
        for p in &placed {
            let _ = std::fs::remove_file(state_dir.join(p)).or_else(|_| std::fs::remove_dir_all(state_dir.join(p)));
        }
        for p in &moved_old {
            let _ = std::fs::rename(old_dir.join(p), state_dir.join(p));
        }
        return Err(e.clone());
    }
    Ok(())
}

fn place_pieces_inner(
    state_dir: &Path,
    src_dir: &Path,
    old_dir: &Path,
    pieces: &[&str],
    moved_old: &mut Vec<String>,
    placed: &mut Vec<String>,
) -> Result<(), String> {
    for p in pieces {
        let src = state_dir.join(p);
        if !src.exists() {
            continue;
        }
        std::fs::create_dir_all(old_dir).map_err(|e| format!("import: 建旧件暂存目录失败：{e}"))?;
        std::fs::rename(&src, old_dir.join(p))
            .map_err(|e| format!("import: 旧件 {p} 搬走失败：{e}"))?;
        moved_old.push((*p).to_owned());
    }
    for p in pieces {
        let src = src_dir.join(p);
        if !src.exists() {
            continue;
        }
        std::fs::rename(&src, state_dir.join(p))
            .map_err(|e| format!("import: 新件 {p} 入位失败：{e}"))?;
        placed.push((*p).to_owned());
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// reset cache
// ---------------------------------------------------------------------------

/// 清空 `<state>/cache/`（在跑拒绝）；L1/L2 与 migration-backup-* 不碰。
pub fn reset_cache(state_dir: &Path) -> Result<(), String> {
    if lock_held(state_dir)? {
        return Err(format!(
            "reset cache: 统一进程在跑（{}/lock 被持有）——先停进程（日志文件被进程持有）",
            state_dir.display()
        ));
    }
    std::fs::remove_dir_all(state_dir.join("cache")).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(state_dir.join("cache")).map_err(|e| e.to_string())?;
    Ok(())
}

// ---------------------------------------------------------------------------
// 锁试探（Go probe_unix.go lockHeld 同义：不带 create——只读探测不造 lock 文件）
// ---------------------------------------------------------------------------

fn lock_held(state_dir: &Path) -> Result<bool, String> {
    let path = state_dir.join("lock");
    #[allow(clippy::suspicious_open_options)]
    let f = match std::fs::OpenOptions::new().read(true).write(true).open(&path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(format!("锁试探失败（{}）：{e}", path.display())),
    };
    let fd = std::os::unix::io::AsRawFd::as_raw_fd(&f);
    let r = unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) };
    if r != 0 {
        let e = std::io::Error::last_os_error();
        if e.kind() == std::io::ErrorKind::WouldBlock {
            return Ok(true); // 有人持有
        }
        return Err(format!("锁试探失败（{}）：{e}", path.display()));
    }
    unsafe { libc::flock(fd, libc::LOCK_UN) };
    Ok(false)
}

fn timestamp() -> String {
    // Go time.Now().Format("20060102-150405") 同形（本地时区）。
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    // 本地时区偏移：靠 libc localtime_r 拿（无 chrono 依赖）。
    unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&secs, &mut tm);
        format!(
            "{:04}{:02}{:02}-{:02}{:02}{:02}",
            tm.tm_year as i64 + 1900,
            tm.tm_mon + 1,
            tm.tm_mday,
            tm.tm_hour,
            tm.tm_min,
            tm.tm_sec
        )
    }
}

/// 列表截断（告警用；超出以「…(+N)」示意）。
fn trunc_list(items: &[String], max: usize) -> String {
    if items.len() <= max {
        return format!("[{}]", items.join(", "));
    }
    let tail = format!("…(+{})", items.len() - max);
    let mut out: Vec<&str> = items[..max].iter().map(|s| s.as_str()).collect();
    out.push(tail.as_str());
    format!("[{}]", out.join(", "))
}

// ---------------------------------------------------------------------------
// ustar 最小读出面（无第三方依赖；固定布局的名字 ≤100B，Go archive/tar 对同布局
// 也产 ustar 头——两侧工件互通）
// ---------------------------------------------------------------------------

const BLOCK: usize = 512;

struct TarWriter {
    out: Vec<u8>,
}

impl TarWriter {
    fn new() -> Self {
        TarWriter { out: Vec::new() }
    }

    fn dir(&mut self, name: &str) -> Result<(), String> {
        self.header(name, 0, b'5')?;
        Ok(())
    }

    fn file(&mut self, name: &str, data: &[u8]) -> Result<(), String> {
        self.header(name, data.len() as u64, b'0')?;
        self.out.extend_from_slice(data);
        let pad = (BLOCK - data.len() % BLOCK) % BLOCK;
        self.out.resize(self.out.len() + pad, 0);
        Ok(())
    }

    fn header(&mut self, name: &str, size: u64, typeflag: u8) -> Result<(), String> {
        let name_b = name.as_bytes();
        if name_b.len() > 100 {
            return Err(format!("tar 条目名超 100 字节（{name}）——固定布局不会出现，疑似异物"));
        }
        let mut h = [0u8; BLOCK];
        h[..name_b.len()].copy_from_slice(name_b);
        h[100..108].copy_from_slice(octal(0o700, 8).as_bytes()); // mode
        h[108..116].copy_from_slice(octal(0, 8).as_bytes()); // uid
        h[116..124].copy_from_slice(octal(0, 8).as_bytes()); // gid
        h[124..136].copy_from_slice(octal(size, 12).as_bytes()); // size
        h[136..148].copy_from_slice(octal(0, 12).as_bytes()); // mtime（0 = 确定性输出）
        h[148..156].fill(b' '); // chksum 位先置空格再算
        h[156] = typeflag;
        h[257..263].copy_from_slice(b"ustar\0");
        h[263..265].copy_from_slice(b"00");
        let sum: u32 = h.iter().map(|&b| b as u32).sum();
        h[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
        self.out.extend_from_slice(&h);
        Ok(())
    }

    fn finish(mut self) -> Vec<u8> {
        self.out.resize(self.out.len() + BLOCK * 2, 0);
        self.out
    }
}

fn octal(v: u64, width: usize) -> String {
    format!("{v:0width$o}", width = width - 1) + "\0"
}

struct TarReader<'a> {
    data: &'a [u8],
    off: usize,
}

impl<'a> TarReader<'a> {
    fn new(data: &'a [u8]) -> Result<Self, String> {
        if data.len() < BLOCK {
            return Err("长度不足一个头块".to_owned());
        }
        Ok(TarReader { data, off: 0 })
    }

    /// 下一条目（None = 结束）；返回 (名字, 是否目录, 数据)。
    fn next(&mut self) -> Result<Option<(String, bool, Vec<u8>)>, String> {
        if self.off + BLOCK > self.data.len() {
            return Ok(None);
        }
        let h = &self.data[self.off..self.off + BLOCK];
        if h.iter().all(|&b| b == 0) {
            return Ok(None); // 结尾块
        }
        // 校验和核对（字段为空格形态的和）。
        let stored = parse_octal(&h[148..156]).ok_or("校验和字段非法")? as u32;
        let sum: u32 = h
            .iter()
            .enumerate()
            .map(|(i, &b)| if (148..156).contains(&i) { b' ' as u32 } else { b as u32 })
            .sum();
        if sum != stored {
            return Err(format!("头块校验和不符（{sum} ≠ {stored}）"));
        }
        let size = parse_octal(&h[124..136]).ok_or("size 字段非法")? as usize;
        let typeflag = h[156];
        let mut name = String::from_utf8_lossy(&h[..100])
            .trim_end_matches('\0')
            .to_owned();
        // ustar prefix（名字 ≤100 时为空——固定布局不触发，触发也能拼）。
        let prefix = String::from_utf8_lossy(&h[345..500]).trim_end_matches('\0').to_owned();
        if !prefix.is_empty() {
            name = format!("{prefix}/{name}");
        }
        self.off += BLOCK;
        let end = self.off + size;
        if end > self.data.len() {
            return Err("条目数据不足（截断的工件）".to_owned());
        }
        let data = self.data[self.off..end].to_vec();
        self.off = end + (BLOCK - size % BLOCK) % BLOCK;
        match typeflag {
            b'5' => Ok(Some((name, true, Vec::new()))),
            b'0' | 0 => Ok(Some((name, false, data))),
            // pax 扩展头（'x'/'g'）与其它类型 = 拒（可行动文案；本布局不会出现）。
            other => Err(format!("条目 {name:?} 的类型 0x{other:02x} 不受支持（只接受普通文件与目录）")),
        }
    }
}

fn parse_octal(field: &[u8]) -> Option<u64> {
    let s: String = String::from_utf8_lossy(field)
        .trim_end_matches(['\0', ' '])
        .trim()
        .chars()
        .filter(|&c| c != ' ')
        .collect();
    if s.is_empty() {
        return Some(0);
    }
    u64::from_str_radix(&s, 8).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!(
            "hw-artifact-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn seed_state(d: &Path) {
        std::fs::create_dir_all(d.join("serve")).unwrap();
        std::fs::create_dir_all(d.join("relay")).unwrap();
        std::fs::create_dir_all(d.join("client/identity")).unwrap();
        std::fs::create_dir_all(d.join("cache")).unwrap();
        std::fs::write(d.join("config.toml"), "[serve]\nenabled = true\n").unwrap();
        std::fs::write(d.join("serve/key.bin"), b"k").unwrap();
        std::fs::write(d.join("serve/tokens.jsonl"), b"t1\n").unwrap();
        std::fs::write(d.join("serve/revoked.jsonl"), b"r1\n").unwrap();
        std::fs::write(d.join("serve/term.sock"), b"transient").unwrap(); // 白名单外 → 告警跳过
        std::fs::write(d.join("relay/relay.key"), b"rk").unwrap();
        std::fs::write(d.join("client/hosts.json"), b"[]").unwrap();
        std::fs::write(d.join("client/identity/master.key"), b"mk").unwrap();
        std::fs::write(d.join("cache/events.log"), "可弃层".as_bytes()).unwrap();
    }

    #[test]
    fn export_import_roundtrip() {
        let src = tmpdir("rt-src");
        seed_state(&src);
        let dest = src.join("out.tar");
        export(&src, &dest).unwrap();
        // 工件权限 0600。
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(&dest).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        // 工件内容 = 白名单四件（term.sock 不在）。
        let entries = read_artifact(&dest).unwrap();
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert!(names.contains(&"homeway-export/serve/key.bin"));
        assert!(names.contains(&"homeway-export/serve/revoked.jsonl"));
        assert!(names.contains(&"homeway-export/relay/relay.key"));
        assert!(names.contains(&"homeway-export/client/identity/master.key"));
        assert!(names.contains(&"homeway-export/client/hosts.json"));
        assert!(!names.iter().any(|n| n.contains("term.sock")));
        assert!(!names.iter().any(|n| n.contains("cache")));
        // import 到全新 state：四件落位、cache 不参与。
        let dst = tmpdir("rt-dst");
        import(&dst, &dest).unwrap();
        assert_eq!(std::fs::read_to_string(dst.join("config.toml")).unwrap(), "[serve]\nenabled = true\n");
        assert_eq!(std::fs::read(dst.join("serve/key.bin")).unwrap(), b"k");
        assert_eq!(std::fs::read(dst.join("client/identity/master.key")).unwrap(), b"mk");
        assert!(!dst.join("cache").exists());
        assert!(!dst.join(".import-old-").exists());
        let residue: Vec<String> = std::fs::read_dir(&dst)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with(".import-"))
            .collect();
        assert!(residue.is_empty(), "临时/备份目录应清理：{residue:?}");
        // 覆盖导入（旧件退场路径）。
        std::fs::write(src.join("serve/tokens.jsonl"), b"t2\n").unwrap();
        let dest2 = src.join("out2.tar");
        export(&src, &dest2).unwrap();
        import(&dst, &dest2).unwrap();
        assert_eq!(std::fs::read_to_string(dst.join("serve/tokens.jsonl")).unwrap(), "t2\n");
        let _ = std::fs::remove_dir_all(&src);
        let _ = std::fs::remove_dir_all(&dst);
    }

    #[test]
    fn import_rejects_bad_layouts() {
        let d = tmpdir("bad");
        let mk = |name: &str, entries: &[(&str, bool, &[u8])]| {
            let mut t = TarWriter::new();
            t.dir(EXPORT_TOP).unwrap();
            for (n, is_dir, data) in entries {
                if *is_dir {
                    t.dir(n).unwrap();
                } else {
                    t.file(n, data).unwrap();
                }
            }
            let p = d.join(name);
            std::fs::write(&p, t.finish()).unwrap();
            p
        };
        // 缺 config.toml。
        let p = mk("no-config.tar", &[]);
        assert!(import(&d.join("t1"), &p).unwrap_err().contains("缺件"));
        // 白名单外条目（serve/ 下异物）。
        let p = mk(
            "stray.tar",
            &[
                (&format!("{EXPORT_TOP}/config.toml"), false, b"[serve]".as_slice()),
                (&format!("{EXPORT_TOP}/serve"), true, b""),
                (&format!("{EXPORT_TOP}/relay"), true, b""),
                (&format!("{EXPORT_TOP}/client"), true, b""),
                (&format!("{EXPORT_TOP}/serve/evil.bin"), false, b"x"),
            ],
        );
        let err = import(&d.join("t2"), &p).unwrap_err();
        assert!(err.contains("多件/异物"), "{err}");
        // 路径违规（../ 逃逸）。
        let mut t = TarWriter::new();
        t.dir(EXPORT_TOP).unwrap();
        t.file(&format!("{EXPORT_TOP}/../evil"), b"x").unwrap();
        let p = d.join("escape.tar");
        std::fs::write(&p, t.finish()).unwrap();
        let err = import(&d.join("t3"), &p).unwrap_err();
        assert!(err.contains("违规条目"), "{err}");
        // 非 tar。
        let p = d.join("garbage.tar");
        std::fs::write(&p, b"not a tar at all........").unwrap();
        assert!(import(&d.join("t4"), &p).unwrap_err().contains("不是合法 tar"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn import_rejects_when_locked() {
        let d = tmpdir("locked");
        seed_state(&d);
        let dest = d.join("out.tar");
        export(&d, &dest).unwrap();
        let _lock = crate::nodestate::InstanceLock::acquire(&d, "unified").unwrap();
        let err = import(&d, &dest).unwrap_err();
        assert!(err.contains("在跑"), "{err}");
        drop(_lock);
        import(&d, &dest).unwrap(); // 解锁后可导
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn reset_cache_clears_only_cache() {
        let d = tmpdir("reset");
        seed_state(&d);
        let _lock = crate::nodestate::InstanceLock::acquire(&d, "unified").unwrap();
        assert!(reset_cache(&d).unwrap_err().contains("在跑"));
        drop(_lock);
        reset_cache(&d).unwrap();
        assert!(d.join("cache").exists()); // 重建空目录
        assert!(!d.join("cache/events.log").exists());
        assert!(d.join("serve/key.bin").exists()); // L2 不动
        assert!(d.join("config.toml").exists()); // L1 不动
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Go archive/tar 产出的 ustar 头（同布局）必须可读——两侧工件互通的钉子。
    /// 字节串 = Go tar.Writer 对 name="homeway-export/config.toml"、size=7、
    /// mode 0600 的输出形态（手工构造同语义头，读回校验）。
    #[test]
    fn ustar_reader_parses_go_shaped_header() {
        let mut t = TarWriter::new();
        t.dir(EXPORT_TOP).unwrap();
        t.file(&format!("{EXPORT_TOP}/config.toml"), b"[serve]").unwrap();
        let bytes = t.finish();
        let mut r = TarReader::new(&bytes).unwrap();
        let (n1, d1, _) = r.next().unwrap().unwrap();
        assert!(d1 && n1 == EXPORT_TOP);
        let (n2, d2, data) = r.next().unwrap().unwrap();
        assert!(!d2 && n2 == "homeway-export/config.toml" && data == b"[serve]");
        assert!(r.next().unwrap().is_none());
    }
}
