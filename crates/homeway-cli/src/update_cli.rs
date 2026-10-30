//! `homeway update` —— 从 GitHub Release 自更新（2026-10-23 批）。
//!
//! 流程：读取最新 Release 的 SHA256SUMS 确定 tag →（**同 tag = 没有更新，收工**）
//! → 固定 tag 下载本平台 tar.gz → 按首次清单哈希校验 → 解包单文件 `homeway` → 烟囱 `--version`（不过 = 中止，不动现役）→
//! 备份现役 → 同目录 rename 原子替换 → 按托管形态重启统一进程：
//! - 守护在跑：SIGTERM 优雅收工（unified 的按序收工路径）；launchd/KeepAlive 主机
//!   由系统拉起新二进制（锁被新 pid 持有即证）；裸进程主机给 3s 宽限确认无人拉起后
//!   **自拉起**（复用 daemon_cli 的按需拉起件——setsid 脱离 + spawn.log，CLI 退出
//!   不带走它）；20s 无人接手 = 如实报告 + 手动指引。
//! - 守护未跑：只换文件（下次手动起 / 任意 CLI 命令按需拉起时即新版本）。
//!
//! 角色恢复**零额外状态**：统一进程零参启动按 config 期望态装配 serve/relay
//! （`unified_cli::run_unified_state`），config 即模式真源，重启自动回原形态。
//!
//! 边界：阿里云等 GitHub 不可达主机在 fetch 阶段如实报错（部署纪律 2026-10-21：
//! 该类主机经本机下载校验后 scp 上传，不走直连 GitHub）；windows 运行中的 exe
//! 无法 rename 盖写——明确报不支持。
//!
//! HTTPS 面在 `homeway_core::minhttp`（同步 rustls 白名单件；本模块不碰 TLS）。

// update 面 windows 不支持（运行中的 exe 无法原位替换）——整个模块只在非 windows
// 编译（main.rs 分派层收口，windows 臂给明确报错）；模块内零 cfg 碎片。
use std::io::Read as _;
use std::path::Path;
use std::time::{Duration, Instant};

use homeway_core::minhttp;
use homeway_core::nodestate::probe_lock_holder;

use crate::cli_flags;

/// GitHub 仓库坐标（公开仓）。
const RELEASE_HOST: &str = "github.com";
const REPO_SLUG: &str = "zhaoyswd/homeway-rs";
/// 产物体上限（64 MiB——现役出口二进制 ≈9 MiB，gzip 后 ≈4 MiB，双倍余量防坏包）。
const ARTIFACT_CAP: usize = 64 << 20;
/// SHA256SUMS 上限（纯文本几十行）。
const SUMS_CAP: usize = 64 << 10;

pub fn cmd_update(args: &[String]) {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        eprintln!("用法：homeway update [--state DIR]     从 GitHub Release 自更新（查最新 tag → 下载校验 → 替换 → 重启守护）");
        eprintln!("  最新 tag 与当前版本相同 = 打印「没有更新」直接收工；");
        eprintln!(
            "  替换前备份现役到 <state>/cache/homeway.prev，并先跑新二进制的 --version 烟囱；"
        );
        eprintln!("  守护进程按托管形态重启（launchd 主机由 KeepAlive 拉起；裸进程主机自拉起）。");
        std::process::exit(0);
    }
    // windows：运行中的 exe 被内核锁住，rename 盖写不可行（MoveFileEx 延迟替换是
    // 另一套形态）——分派层（main.rs）已给明确报错，本模块只在非 windows 编译。
    let mut state = crate::unified_cli::default_state_dir();
    let mut i = 0;
    while i < args.len() {
        let raw = args[i].as_str();
        let Some((name, inline)) = cli_flags::split_flag(raw) else {
            eprintln!("未知参数：{raw}");
            std::process::exit(2);
        };
        let next = args.get(i + 1).map(String::as_str);
        let mut adv = 1usize;
        match name {
            "state" => {
                state = cli_flags::take_state_or_exit("state", inline, next);
                if inline.is_none() {
                    adv = 2;
                }
            }
            other => {
                eprintln!("未知参数：--{other}（update 只认 --state）");
                std::process::exit(2);
            }
        }
        i += adv;
    }
    run(&state);
}

fn run(state: &Path) {
    let current = crate::cli_version();

    // ---- ①–③ 最新清单 → 固定 tag 下载 → 首次清单哈希校验 ----
    println!("update: 查询 GitHub 最新 release（{REPO_SLUG}）…");
    let platform = match asset_name_for(std::env::consts::OS, std::env::consts::ARCH) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("update: 失败：{e}");
            std::process::exit(1);
        }
    };
    let release = match prepare_release(current, platform, |host, path, cap, budget| {
        minhttp::get(host, path, None, Instant::now() + budget, cap)
    }) {
        Ok(Some(release)) => release,
        Ok(None) => return,
        Err(e) => {
            eprintln!("update: 失败：{e}");
            if matches!(e, UpdateError::Fetch { .. }) {
                eprintln!("  GitHub 不可达的主机（如阿里云）不走本命令——按部署纪律由本机下载校验后 scp 换装。");
            }
            std::process::exit(1);
        }
    };
    let tag = release.tag;

    // ---- ④ 解包 + 烟囱（不过 = 中止，不动现役）----
    let bin = match extract_homeway(&release.archive) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("update: 失败：解包不了（{e}）——中止，现役未动");
            std::process::exit(1);
        }
    };
    let exe = std::env::current_exe()
        .and_then(|p| p.canonicalize().or(Ok(p)))
        .unwrap_or_else(|e| {
            eprintln!("update: 失败：定位当前二进制不了（{e}）");
            std::process::exit(1);
        });
    let Some(exe_dir) = exe.parent().map(Path::to_path_buf) else {
        eprintln!("update: 失败：当前二进制没有父目录（{:?}）", exe);
        std::process::exit(1);
    };
    let staged = exe_dir.join(format!(".homeway-update-{}", std::process::id()));
    if let Err(e) = write_staged(&staged, &bin) {
        eprintln!("update: 失败：暂存新二进制到 {} 失败（{e}）", staged.display());
        let _ = std::fs::remove_file(&staged);
        std::process::exit(1);
    }
    let smoke = std::process::Command::new(&staged).arg("--version").output();
    let smoke_ok = matches!(&smoke, Ok(o) if o.status.success()
        && String::from_utf8_lossy(&o.stdout).trim() == format!("homeway {tag}"));
    if !smoke_ok {
        eprintln!("update: 失败：新二进制烟囱不过（--version 应答 homeway {tag}）——中止，现役未动");
        let _ = std::fs::remove_file(&staged);
        std::process::exit(1);
    }

    // ---- ⑤ 备份 + 原子替换 ----
    let backup = state.join("cache").join("homeway.prev");
    if let Err(e) = std::fs::create_dir_all(backup.parent().unwrap_or(state)) {
        eprintln!("update: 失败：建备份目录失败（{e}）");
        let _ = std::fs::remove_file(&staged);
        std::process::exit(1);
    }
    match std::fs::copy(&exe, &backup) {
        Ok(_) => println!("update: 已备份现役 → {}（回退 = 放回 {}）", backup.display(), exe.display()),
        Err(e) => {
            eprintln!("update: 失败：备份现役失败（{e}）——中止，现役未动");
            let _ = std::fs::remove_file(&staged);
            std::process::exit(1);
        }
    }
    if let Err(e) = std::fs::rename(&staged, &exe) {
        eprintln!("update: 失败：替换 {} 失败（{e}）——中止，现役未动", exe.display());
        let _ = std::fs::remove_file(&staged);
        std::process::exit(1);
    }
    println!("update: 已替换 {}（{current} → {tag}）", exe.display());

    // ---- ⑥ 重启统一进程（形态感知）----
    restart_unified(state);
}

/// 守护重启：SIGTERM 旧 pid → 等「新 pid 持锁」（托管拉起）或「锁空 + 进程已退」
/// （3s 宽限后自拉起）；最后等 control.sock 就绪作收工判据。
fn restart_unified(state: &Path) {
    let (held, pid, form) = probe_lock_holder(state);
    if !held {
        println!("update: 统一进程未在运行——替换完成，下次启动（手动或任意命令按需拉起）即新版本");
        return;
    }
    if pid <= 0 || pid as u32 == std::process::id() {
        eprintln!("update: ⚠️ 锁持有者 pid 异常（{pid}）——二进制已替换，请手动重启统一进程");
        return;
    }
    println!("update: 统一进程在跑（pid={pid}，{form}）——发停止信号…");
    if unsafe { libc::kill(pid, libc::SIGTERM) } != 0 {
        let e = std::io::Error::last_os_error();
        eprintln!("update: ⚠️ 向 pid={pid} 发 SIGTERM 失败（{e}）——二进制已替换，请手动重启统一进程");
        return;
    }
    let t0 = Instant::now();
    let mut up = false;
    loop {
        std::thread::sleep(Duration::from_millis(150));
        let (h, p2, _) = probe_lock_holder(state);
        if h && p2 != pid {
            println!("update: 统一进程已由托管方拉起（旧 pid={pid} → 新 pid={p2}——launchd/KeepAlive 形态）");
            up = true;
            break;
        }
        if !h && process_gone(pid) && t0.elapsed() >= Duration::from_secs(3) {
            // 3s 内无人拉起 = 无托管形态（nohup/手动/按需拉起宿主）——自拉起。
            match crate::daemon_cli::start_spawned_process(state) {
                Ok(np) => println!(
                    "update: 守护已退出且无托管拉起——已自拉起（新 pid={np}；日志 {}）",
                    crate::daemon_cli::spawn_log_path(state).display()
                ),
                Err(e) => {
                    eprintln!("update: ⚠️ 自拉起失败（{e}）——请手动启动：homeway --state {}", state.display());
                }
            }
            up = true;
            break;
        }
        if t0.elapsed() >= Duration::from_secs(20) {
            eprintln!(
                "update: ⚠️ 20s 内旧守护未退出 / 未见新进程——二进制已替换，请手动重启（macOS：launchctl kickstart -k gui/$UID/me.zhaozhe.homeway-exit；其它：homeway --state {}）",
                state.display()
            );
            break;
        }
    }
    if !up {
        return;
    }
    // 收工判据：control.sock 可连（统一进程 control 角色恒开——可连即用户面在）。
    let sock = state.join("control.sock");
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        if std::os::unix::net::UnixStream::connect(&sock).is_ok() {
            println!("update: 控制面就绪（{}）——升级完成", sock.display());
            return;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    eprintln!(
        "update: ⚠️ 控制面 15s 内未就绪——查 {} 与 events.log；二进制已替换成功",
        crate::daemon_cli::spawn_log_path(state).display()
    );
}

/// pid 是否已不存在（unix `kill(pid,0)` 的 ESRCH 语义；EPERM = 在但非我们进程）。
fn process_gone(pid: i32) -> bool {
    if unsafe { libc::kill(pid, 0) } != 0 {
        matches!(std::io::Error::last_os_error().raw_os_error(), Some(libc::ESRCH))
    } else {
        false
    }
}

/// 平台 → Release 产物名段（release.yml 的 matrix.name）。
fn asset_name_for(os: &str, arch: &str) -> Result<&'static str, String> {
    match (os, arch) {
        ("macos", "aarch64") => Ok("darwin-arm64"),
        ("macos", "x86_64") => Ok("darwin-amd64"),
        ("linux", "x86_64") => Ok("linux-amd64"),
        ("linux", "aarch64") => Ok("linux-arm64"),
        (os, arch) => Err(format!("平台 {os}/{arch} 没有对应的 Release 产物（现役五目标外的平台请自行构建）")),
    }
}

/// 清单中的目标产物；字段借用首次清单，不重新查询 latest。
#[derive(Debug)]
struct Artifact<'a> {
    name: &'a str,
    sha256: [u8; 32],
}

#[derive(Debug)]
struct Manifest<'a> {
    tag: &'a str,
    artifact: Option<Artifact<'a>>,
}

#[derive(Debug, thiserror::Error)]
enum ManifestError {
    #[error("SHA256SUMS 不是 UTF-8：{0}")]
    Utf8(#[from] std::str::Utf8Error),
    #[error("SHA256SUMS 第 {0} 行格式非法（应为 64 位十六进制哈希及官方产物名）")]
    InvalidLine(usize),
    #[error("SHA256SUMS 第 {0} 行产物名或 tag 非法")]
    InvalidAsset(usize),
    #[error("SHA256SUMS 包含重复产物：{0}")]
    DuplicateAsset(String),
    #[error("SHA256SUMS 包含多个版本")]
    MixedVersions,
    #[error("SHA256SUMS 没有产物")]
    Empty,
}

impl<'a> Manifest<'a> {
    fn parse(bytes: &'a [u8], platform: &str) -> Result<Self, ManifestError> {
        let sums = std::str::from_utf8(bytes)?;
        let mut tag = None;
        let mut artifact = None;
        let mut names = std::collections::HashSet::new();
        for (index, line) in sums.lines().enumerate() {
            let line_number = index + 1;
            if line.trim().is_empty() {
                continue;
            }
            let mut fields = line.split_whitespace();
            let (Some(hash), Some(name)) = (fields.next(), fields.next()) else {
                return Err(ManifestError::InvalidLine(line_number));
            };
            if fields.next().is_some()
                || hash.len() != 64
                || !hash.bytes().all(|b| b.is_ascii_hexdigit())
            {
                return Err(ManifestError::InvalidLine(line_number));
            }
            let name = name.strip_prefix('*').unwrap_or(name);
            let (version, asset_platform) =
                release_asset_parts(name).ok_or(ManifestError::InvalidAsset(line_number))?;
            if tag.is_some_and(|prior| prior != version) {
                return Err(ManifestError::MixedVersions);
            }
            tag = Some(version);
            if !names.insert(name) {
                return Err(ManifestError::DuplicateAsset(name.to_owned()));
            }
            if asset_platform == platform {
                let mut sha256 = [0; 32];
                for (byte, pair) in sha256.iter_mut().zip(hash.as_bytes().as_chunks::<2>().0) {
                    // 上面的 ASCII hex 检查已保证两位均可转换。
                    *byte = ((pair[0] as char).to_digit(16).unwrap() * 16
                        + (pair[1] as char).to_digit(16).unwrap())
                        as u8;
                }
                artifact = Some(Artifact { name, sha256 });
            }
        }
        Ok(Self {
            tag: tag.ok_or(ManifestError::Empty)?,
            artifact,
        })
    }
}

/// release.yml 的五种归档名。tag 是单个安全 URL 路径段，不接受转义/路径/查询片段。
fn release_asset_parts(name: &str) -> Option<(&str, &str)> {
    let name = name.strip_prefix("homeway-")?;
    let (tag, platform) = [
        ("-darwin-arm64.tar.gz", "darwin-arm64"),
        ("-darwin-amd64.tar.gz", "darwin-amd64"),
        ("-linux-amd64.tar.gz", "linux-amd64"),
        ("-linux-arm64.tar.gz", "linux-arm64"),
        ("-windows-amd64.zip", "windows-amd64"),
    ]
    .into_iter()
    .find_map(|(suffix, platform)| name.strip_suffix(suffix).map(|tag| (tag, platform)))?;
    let version = tag.strip_prefix('v')?;
    if tag.len() > 128
        || !version.as_bytes().first()?.is_ascii_digit()
        || !version
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_' | b'+'))
    {
        return None;
    }
    Some((tag, platform))
}

#[derive(Debug, thiserror::Error)]
enum UpdateError {
    #[error("{stage}不通（{source}）")]
    Fetch {
        stage: &'static str,
        source: minhttp::HttpError,
    },
    #[error("{stage} HTTP 状态 {status}（{detail}）")]
    Status {
        stage: &'static str,
        status: u16,
        detail: &'static str,
    },
    #[error(transparent)]
    Manifest(#[from] ManifestError),
    #[error("最新 Release {tag} 的 SHA256SUMS 缺本平台产物（{platform}）")]
    MissingArtifact { tag: String, platform: String },
    #[error("哈希不齐（sha256 期望 {expected}，实得 {actual}）——中止，现役未动")]
    HashMismatch { expected: String, actual: String },
}

struct PreparedRelease {
    tag: String,
    archive: Vec<u8>,
}

/// 下载准备阶段不读写本机文件/进程；GET 注入缝用于离线验证路径、次序和失败边界。
fn prepare_release(
    current: &str,
    platform: &str,
    mut get: impl FnMut(&str, &str, usize, Duration) -> Result<minhttp::Response, minhttp::HttpError>,
) -> Result<Option<PreparedRelease>, UpdateError> {
    let mut fetch = |path: &str, cap, budget, stage| {
        let response = get(RELEASE_HOST, path, cap, budget)
            .map_err(|source| UpdateError::Fetch { stage, source })?;
        if response.status != 200 {
            let detail = match response.status {
                403 => "GitHub 拒绝访问；仅凭状态码不能确定原因",
                404 => "未找到对应的 Release 文件",
                429 => "GitHub 请求过多，请稍后重试",
                _ => "下载未成功",
            };
            return Err(UpdateError::Status {
                stage,
                status: response.status,
                detail,
            });
        }
        Ok(response)
    };
    let sums = fetch(
        &format!("/{REPO_SLUG}/releases/latest/download/SHA256SUMS"),
        SUMS_CAP,
        Duration::from_secs(30),
        "读取最新 Release 的 SHA256SUMS",
    )?;
    let manifest = Manifest::parse(&sums.body, platform)?;
    let tag = manifest.tag;
    if tag == current {
        println!("update: 已是最新（{tag}）——没有更新");
        return Ok(None);
    }
    let artifact = manifest
        .artifact
        .ok_or_else(|| UpdateError::MissingArtifact {
            tag: tag.to_owned(),
            platform: platform.to_owned(),
        })?;
    if current == "(devel)" {
        println!("update: 当前是本地 devel 构建（无版本号）——按最新 Release {tag} 继续");
    } else {
        println!("update: 有新版本：{current} → {tag}");
    }
    println!("update: 下载 {} …", artifact.name);
    let archive = fetch(
        &format!("/{REPO_SLUG}/releases/download/{tag}/{}", artifact.name),
        ARTIFACT_CAP,
        Duration::from_secs(180),
        "下载产物",
    )?
    .body;
    use sha2::{Digest as _, Sha256};
    let actual: [u8; 32] = Sha256::digest(&archive).into();
    if actual != artifact.sha256 {
        return Err(UpdateError::HashMismatch {
            expected: hex_lower(&artifact.sha256),
            actual: hex_lower(&actual),
        });
    }
    println!(
        "update: sha256 校验通过（{}…，{} 字节）",
        &hex_lower(&actual)[..12],
        archive.len()
    );
    Ok(Some(PreparedRelease {
        tag: tag.to_owned(),
        archive,
    }))
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0xf) as usize] as char);
    }
    s
}

/// 暂存新二进制（同目录 = 与目标同文件系统，rename 原子性前提）+ 0755 + 落盘。
fn write_staged(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;
    use std::os::unix::fs::PermissionsExt as _;
    let mut f = std::fs::File::create(path)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    f.set_permissions(std::fs::Permissions::from_mode(0o755))?;
    Ok(())
}

/// 从 tar.gz 里解出**单文件 `homeway`**（release.yml 的产物形态：`tar czf … homeway`，
/// 单条目无目录前缀）。最小 ustar 读取面：512B 头 + 八进制尺寸 + 类型位；跳过
/// 目录/pax 扩展头（`x`/`g`）与 GNU 长名（`L`——产物名 `homeway` 用不到，若未来
/// 真出现由烟囱 `--version` 兜底拦下）。非 ustar / 截断 / 条目超限 = 显式错误。
fn extract_homeway(tar_gz: &[u8]) -> Result<Vec<u8>, String> {
    let mut dec = flate2::read::GzDecoder::new(tar_gz);
    let mut tar = Vec::new();
    dec.read_to_end(&mut tar).map_err(|e| format!("gzip 解压失败：{e}"))?;
    // 解压后体量护栏（正常 ≈9 MiB；256 MiB 防恶意/坏包撑内存）。
    if tar.len() > 256 << 20 {
        return Err("解压后超 256 MiB 上限".into());
    }
    let mut off = 0usize;
    let mut entries = 0usize;
    while off + 512 <= tar.len() {
        let hdr = &tar[off..off + 512];
        if hdr.iter().all(|&b| b == 0) {
            break; // 结束块（两块全零；首块即止）
        }
        entries += 1;
        if entries > 64 {
            return Err("tar 条目超 64（不是预期的单文件产物形态）".into());
        }
        if &hdr[257..262] != b"ustar" {
            return Err(format!("tar 头缺 ustar 魔数（off={off}）——不是预期产物格式"));
        }
        let name = cstr(&hdr[0..100]);
        let size = octal(&hdr[124..136])? as usize;
        let typeflag = hdr[156];
        let data_off = off + 512;
        let Some(data) = tar.get(data_off..data_off.checked_add(size).ok_or("tar 尺寸溢出")?) else {
            return Err("tar 条目数据截断".into());
        };
        match typeflag {
            b'0' | 0 => {
                let base = name.rsplit('/').next().unwrap_or(&name);
                if base == "homeway" {
                    return Ok(data.to_vec());
                }
            }
            // 目录 / pax 扩展（x/g）/ GNU 长名（L）/ 硬软链等：跳过（产物不该有，
            // 出现也不致命——目标条目未命中时在下方显式报错）。
            b'1'..=b'7' | b'x' | b'g' | b'L' => {}
            other => return Err(format!("tar 条目类型不认识（{other:?}）")),
        }
        off = data_off + size.div_ceil(512) * 512;
    }
    Err("tar 里没有名为 homeway 的常规文件条目".into())
}

/// ustar 头字段：NUL 截断的 ASCII。
fn cstr(field: &[u8]) -> String {
    let end = field.iter().position(|&b| b == 0).unwrap_or(field.len());
    String::from_utf8_lossy(&field[..end]).into_owned()
}

/// ustar 尺寸字段：八进制 ASCII（空格/NUL 填充；全空 = 0）。
fn octal(field: &[u8]) -> Result<u64, String> {
    let s: String = field
        .iter()
        .filter(|b| **b != b' ' && **b != 0)
        .map(|b| *b as char)
        .collect();
    if s.is_empty() {
        return Ok(0);
    }
    u64::from_str_radix(&s, 8).map_err(|_| format!("tar 八进制尺寸非法（{s:?}）"))
}

// ---------- 单测（离线、确定性） ----------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asset_name_mapping_and_rejections() {
        assert_eq!(asset_name_for("macos", "aarch64").unwrap(), "darwin-arm64");
        assert_eq!(asset_name_for("macos", "x86_64").unwrap(), "darwin-amd64");
        assert_eq!(asset_name_for("linux", "x86_64").unwrap(), "linux-amd64");
        assert_eq!(asset_name_for("linux", "aarch64").unwrap(), "linux-arm64");
        assert!(asset_name_for("windows", "x86_64").is_err());
        assert!(asset_name_for("freebsd", "x86_64").is_err());
    }

    const HASH: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn manifest(tag: &str) -> Vec<u8> {
        [
            "darwin-arm64.tar.gz",
            "darwin-amd64.tar.gz",
            "linux-amd64.tar.gz",
            "linux-arm64.tar.gz",
            "windows-amd64.zip",
        ]
        .map(|suffix| format!("{HASH}  homeway-{tag}-{suffix}\n"))
        .concat()
        .into_bytes()
    }

    #[test]
    fn manifest_accepts_five_official_assets_and_checksum_forms() {
        for platform in [
            "darwin-arm64",
            "darwin-amd64",
            "linux-amd64",
            "linux-arm64",
            "windows-amd64",
        ] {
            let bytes = manifest("v0.3.24");
            let parsed = Manifest::parse(&bytes, platform).unwrap();
            assert_eq!(parsed.tag, "v0.3.24");
            assert_eq!(hex_lower(&parsed.artifact.unwrap().sha256), HASH);
        }
        let sums = format!(
            "\r\n{} *homeway-v0.3.25-rc.1+build-linux-amd64.tar.gz\r\n\r\n",
            HASH.to_uppercase()
        );
        let parsed = Manifest::parse(sums.as_bytes(), "linux-amd64").unwrap();
        assert_eq!(parsed.tag, "v0.3.25-rc.1+build");
        assert_eq!(hex_lower(&parsed.artifact.unwrap().sha256), HASH);
    }

    #[test]
    fn manifest_rejects_ambiguous_or_malformed_input() {
        assert!(matches!(
            Manifest::parse(b"\xff", "linux-amd64"),
            Err(ManifestError::Utf8(_))
        ));
        assert!(matches!(
            Manifest::parse(b"\n", "linux-amd64"),
            Err(ManifestError::Empty)
        ));
        for line in [
            format!("{}g  homeway-v0.3.24-linux-amd64.tar.gz", &HASH[..63]),
            format!("{}  homeway-v0.3.24-linux-amd64.tar.gz", &HASH[..63]),
            format!("{HASH}  homeway-v0.3.24-linux-amd64.tar.gz extra"),
            "missing-name".to_owned(),
        ] {
            assert!(
                matches!(
                    Manifest::parse(line.as_bytes(), "linux-amd64"),
                    Err(ManifestError::InvalidLine(_))
                ),
                "{line}"
            );
        }
        for name in [
            "../homeway-v0.3.24-linux-amd64.tar.gz",
            "homeway-v1/../../evil-linux-amd64.tar.gz",
            "homeway-v1%2fescape-linux-amd64.tar.gz",
            "homeway-v1?query-linux-amd64.tar.gz",
            "homeway-v1#fragment-linux-amd64.tar.gz",
            "homeway-v1\\escape-linux-amd64.tar.gz",
            "homeway-v-linux-amd64.tar.gz",
            "homeway-main-linux-amd64.tar.gz",
            "homeway-v0.3.24-windows-amd64.tar.gz",
            "homeway-v0.3.24-linux-amd64.zip",
            "homeway-v0.3.24-unknown.tar.gz",
        ] {
            let sums = format!("{HASH} {name}");
            assert!(
                matches!(
                    Manifest::parse(sums.as_bytes(), "linux-amd64"),
                    Err(ManifestError::InvalidAsset(_))
                ),
                "{name}"
            );
        }
        let duplicate = format!(
            "{HASH} homeway-v0.3.24-linux-amd64.tar.gz\n{HASH} *homeway-v0.3.24-linux-amd64.tar.gz"
        );
        assert!(matches!(
            Manifest::parse(duplicate.as_bytes(), "linux-amd64"),
            Err(ManifestError::DuplicateAsset(_))
        ));
        let conflict = format!(
            "{HASH} homeway-v0.3.24-linux-amd64.tar.gz\n{} homeway-v0.3.24-linux-amd64.tar.gz",
            "0".repeat(64)
        );
        assert!(matches!(
            Manifest::parse(conflict.as_bytes(), "linux-amd64"),
            Err(ManifestError::DuplicateAsset(_))
        ));
        let mixed = format!(
            "{HASH} homeway-v0.3.24-darwin-arm64.tar.gz\n{HASH} homeway-v0.3.25-linux-amd64.tar.gz"
        );
        assert!(matches!(
            Manifest::parse(mixed.as_bytes(), "linux-amd64"),
            Err(ManifestError::MixedVersions)
        ));
    }

    fn response(status: u16, body: Vec<u8>) -> minhttp::Response {
        minhttp::Response { status, body }
    }

    #[test]
    fn update_pins_download_to_first_manifest_without_api_or_second_latest_request() {
        use sha2::{Digest as _, Sha256};
        let archive = b"verified-release-archive";
        let sums = format!(
            "{} homeway-v0.3.24-linux-amd64.tar.gz",
            hex_lower(&Sha256::digest(archive))
        );
        let mut requests = Vec::new();
        let release = prepare_release("v0.3.23", "linux-amd64", |host, path, cap, budget| {
            requests.push((host.to_owned(), path.to_owned(), cap, budget));
            // 若 latest 此时前移，第二次 latest 请求应直接失败；只允许固定原 tag。
            match requests.len() {
                1 => Ok(response(200, sums.as_bytes().to_vec())),
                2 if path == "/zhaoyswd/homeway-rs/releases/download/v0.3.24/homeway-v0.3.24-linux-amd64.tar.gz" => Ok(response(200, archive.to_vec())),
                _ => panic!("意外请求 {host}{path}"),
            }
        }).unwrap().unwrap();
        assert_eq!(release.tag, "v0.3.24");
        assert_eq!(release.archive, archive);
        assert_eq!(requests.len(), 2);
        assert!(requests.iter().all(|(host, _, _, _)| host == "github.com"));
        assert_eq!(
            requests[0].1,
            "/zhaoyswd/homeway-rs/releases/latest/download/SHA256SUMS"
        );
        assert_eq!(
            (requests[0].2, requests[0].3),
            (SUMS_CAP, Duration::from_secs(30))
        );
        assert_eq!(
            (requests[1].2, requests[1].3),
            (ARTIFACT_CAP, Duration::from_secs(180))
        );
    }

    #[test]
    fn update_current_version_stops_after_validating_entire_manifest() {
        let mut requests = 0;
        // 当前平台缺包但 tag 相同：保持既有“没有更新”语义。
        assert!(prepare_release("v0.3.24", "linux-amd64", |_, _, _, _| {
            requests += 1;
            Ok(response(
                200,
                format!("{HASH} homeway-v0.3.24-darwin-arm64.tar.gz").into_bytes(),
            ))
        })
        .unwrap()
        .is_none());
        assert_eq!(requests, 1);
        let mixed = format!(
            "{HASH} homeway-v0.3.24-darwin-arm64.tar.gz\n{HASH} homeway-v0.3.25-linux-amd64.tar.gz"
        );
        assert!(matches!(
            prepare_release("v0.3.24", "linux-amd64", |_, _, _, _| Ok(response(
                200,
                mixed.as_bytes().to_vec()
            ))),
            Err(UpdateError::Manifest(ManifestError::MixedVersions))
        ));
    }

    #[test]
    fn update_rejects_missing_platform_and_bad_manifest_before_archive_request() {
        for sums in [
            b"broken".to_vec(),
            format!("{HASH} homeway-v0.3.24-darwin-arm64.tar.gz").into_bytes(),
        ] {
            let mut requests = 0;
            let result = prepare_release("v0.3.23", "linux-amd64", |_, _, _, _| {
                requests += 1;
                Ok(response(200, sums.clone()))
            });
            assert!(matches!(
                result,
                Err(UpdateError::Manifest(_)) | Err(UpdateError::MissingArtifact { .. })
            ));
            assert_eq!(requests, 1);
        }
    }

    #[test]
    fn update_reports_http_and_transport_errors_in_both_stages() {
        for status in [403, 404, 429, 500] {
            for failed_request in [1, 2] {
                let mut requests = 0;
                let result = prepare_release("v0.3.23", "linux-amd64", |_, _, _, _| {
                    requests += 1;
                    Ok(if requests == failed_request {
                        response(status, vec![])
                    } else {
                        response(200, manifest("v0.3.24"))
                    })
                });
                assert!(
                    matches!(result, Err(UpdateError::Status { status: actual, .. }) if actual == status)
                );
                assert_eq!(requests, failed_request);
            }
        }
        for failed_request in [1, 2] {
            let mut requests = 0;
            let result = prepare_release("v0.3.23", "linux-amd64", |_, _, _, _| {
                requests += 1;
                if requests == failed_request {
                    Err(minhttp::HttpError::Deadline)
                } else {
                    Ok(response(200, manifest("v0.3.24")))
                }
            });
            assert!(matches!(
                result,
                Err(UpdateError::Fetch {
                    source: minhttp::HttpError::Deadline,
                    ..
                })
            ));
            assert_eq!(requests, failed_request);
        }
    }

    #[test]
    fn update_hash_mismatch_returns_no_prepared_archive() {
        let mut requests = 0;
        let result = prepare_release("(devel)", "linux-amd64", |_, _, _, _| {
            requests += 1;
            Ok(response(
                200,
                if requests == 1 {
                    manifest("v0.3.24")
                } else {
                    b"corrupt".to_vec()
                },
            ))
        });
        assert!(matches!(result, Err(UpdateError::HashMismatch { .. })));
        assert_eq!(requests, 2);
    }

    /// 只下载与校验，不暂存/换装/重启；真网不纳入默认测试门。
    #[test]
    #[ignore = "真网依赖：手动跑，官方清单及本平台归档只读校验"]
    fn github_release_preparation_over_real_tls() {
        let platform = asset_name_for(std::env::consts::OS, std::env::consts::ARCH).unwrap();
        let get = |host: &str, path: &str, cap, budget| {
            minhttp::get(host, path, None, Instant::now() + budget, cap)
        };
        let release = prepare_release("(devel)", platform, get).unwrap().unwrap();
        assert!(!release.archive.is_empty());
        assert!(prepare_release(&release.tag, platform, get).unwrap().is_none());
    }

    #[test]
    fn hex_lower_known_vector() {
        assert_eq!(hex_lower(&[0x00, 0x0f, 0xff]), "000fff");
    }

    // ---- tar.gz 往返（测试面自建 ustar 写入器——只服务本测，不进生产面） ----

    fn ustar_entry(name: &str, typeflag: u8, data: &[u8]) -> Vec<u8> {
        let mut hdr = vec![0u8; 512];
        let n = name.as_bytes();
        assert!(n.len() < 100);
        hdr[..n.len()].copy_from_slice(n);
        let size = format!("{:011o}\0", data.len());
        hdr[124..124 + size.len()].copy_from_slice(size.as_bytes());
        hdr[156] = typeflag;
        hdr[257..262].copy_from_slice(b"ustar");
        hdr[262] = 0;
        hdr[263] = b'0';
        let mut out = hdr;
        out.extend_from_slice(data);
        out.extend(std::iter::repeat_n(0, data.len().div_ceil(512) * 512 - data.len()));
        out
    }

    fn tar_gz(entries: &[(&str, u8, &[u8])]) -> Vec<u8> {
        let mut tar = Vec::new();
        for (n, t, d) in entries {
            tar.extend_from_slice(&ustar_entry(n, *t, d));
        }
        tar.extend_from_slice(&[0u8; 1024]); // 结束双零块
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        use std::io::Write as _;
        enc.write_all(&tar).unwrap();
        enc.finish().unwrap()
    }

    #[test]
    fn extract_roundtrip_single_file() {
        let payload = b"#!/bin/sh\necho fake\n".repeat(64);
        let gz = tar_gz(&[("homeway", b'0', &payload)]);
        let got = extract_homeway(&gz).unwrap();
        assert_eq!(got, payload);
    }

    #[test]
    fn extract_skips_dir_and_pax_then_finds_homeway() {
        let payload = b"bin-bytes";
        let gz = tar_gz(&[
            ("./", b'5', b""),
            ("pax头占位", b'x', b"20 path=whatever\n"),
            ("./homeway", b'0', payload),
        ]);
        assert_eq!(extract_homeway(&gz).unwrap(), payload);
    }

    #[test]
    fn extract_rejects_missing_or_bogus() {
        // 没有 homeway 条目。
        let gz = tar_gz(&[("./", b'5', b"")]);
        assert!(extract_homeway(&gz).is_err());
        // 非 ustar 头（魔数位被抹）。
        let mut bad = tar_gz(&[("homeway", b'0', b"x")]);
        let mut dec = flate2::read::GzDecoder::new(&bad[..]);
        let mut tar = Vec::new();
        use std::io::Read as _;
        dec.read_to_end(&mut tar).unwrap();
        tar[257] = b'X';
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        use std::io::Write as _;
        enc.write_all(&tar).unwrap();
        bad = enc.finish().unwrap();
        assert!(extract_homeway(&bad).is_err());
        // 非 gzip。
        assert!(extract_homeway(b"not-gzip").is_err());
    }

    #[test]
    fn octal_field_forms() {
        assert_eq!(octal(b"00000001000\0").unwrap(), 512);
        assert_eq!(octal(b"          12").unwrap(), 10);
        assert_eq!(octal(b"            ").unwrap(), 0);
        assert!(octal(b"0000000000z\0").is_err());
    }
}
