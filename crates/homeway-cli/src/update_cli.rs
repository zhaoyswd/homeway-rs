//! `homeway update` —— 从 GitHub Release 自更新（2026-10-23 批）。
//!
//! 流程：查最新 tag →（**同 tag = 没有更新，收工**）→ 下载本平台 tar.gz + SHA256SUMS
//! → 哈希校验 → 解包单文件 `homeway` → 烟囱 `--version`（不过 = 中止，不动现役）→
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
const API_HOST: &str = "api.github.com";
const REPO_SLUG: &str = "zhaoyswd/homeway-rs";
/// 产物体上限（64 MiB——现役出口二进制 ≈9 MiB，gzip 后 ≈4 MiB，双倍余量防坏包）。
const ARTIFACT_CAP: usize = 64 << 20;
/// API JSON 上限（2 MiB——releases/latest 的 assets 清单远小于此）。
const JSON_CAP: usize = 2 << 20;
/// SHA256SUMS 上限（纯文本几十行）。
const SUMS_CAP: usize = 64 << 10;

pub fn cmd_update(args: &[String]) {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        eprintln!("用法：homeway update [--state DIR]     从 GitHub Release 自更新（查最新 tag → 下载校验 → 替换 → 重启守护）");
        eprintln!("  最新 tag 与当前版本相同 = 打印「没有更新」直接收工；");
        eprintln!("  替换前备份现役到 <state>/cache/homeway.prev，并先跑新二进制的 --version 烟囱；");
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

    // ---- ① 查最新 release ----
    println!("update: 查询 GitHub 最新 release（{REPO_SLUG}）…");
    let dl = Instant::now() + Duration::from_secs(30);
    let resp = match minhttp::get(
        API_HOST,
        &format!("/repos/{REPO_SLUG}/releases/latest"),
        Some("application/vnd.github+json"),
        dl,
        JSON_CAP,
    ) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("update: 失败：查最新 release 不通（{e}）");
            eprintln!("  GitHub 不可达的主机（如阿里云）不走本命令——按部署纪律由本机下载校验后 scp 换装。");
            std::process::exit(1);
        }
    };
    match resp.status {
        200 => {}
        404 => {
            eprintln!("update: 失败：仓库还没有 Release（404）");
            std::process::exit(1);
        }
        403 => {
            eprintln!("update: 失败：API 拒绝（403——未认证限流 60 次/小时/IP，稍后再试）");
            std::process::exit(1);
        }
        other => {
            eprintln!("update: 失败：API 状态 {other}");
            std::process::exit(1);
        }
    }
    let meta: serde_json::Value = match serde_json::from_slice(&resp.body) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("update: 失败：release 元数据解析不了（{e}）");
            std::process::exit(1);
        }
    };
    let Some(tag) = meta["tag_name"].as_str().map(str::to_owned) else {
        eprintln!("update: 失败：release 元数据缺 tag_name");
        std::process::exit(1);
    };
    if tag == current {
        println!("update: 已是最新（{tag}）——没有更新");
        return;
    }
    if current == "(devel)" {
        println!("update: 当前是本地 devel 构建（无版本号）——按最新 Release {tag} 继续");
    } else {
        println!("update: 有新版本：{current} → {tag}");
    }

    // ---- ② 选本平台产物 ----
    let platform = match asset_name_for(std::env::consts::OS, std::env::consts::ARCH) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("update: 失败：{e}");
            std::process::exit(1);
        }
    };
    let asset_tar = format!("homeway-{tag}-{platform}.tar.gz");
    let mut tar_url = None;
    let mut sums_url = None;
    if let Some(assets) = meta["assets"].as_array() {
        for a in assets {
            let name = a["name"].as_str().unwrap_or("");
            let url = a["browser_download_url"].as_str().unwrap_or("");
            if name == asset_tar {
                tar_url = Some(url.to_owned());
            } else if name == "SHA256SUMS" {
                sums_url = Some(url.to_owned());
            }
        }
    }
    let (Some(tar_url), Some(sums_url)) = (tar_url, sums_url) else {
        eprintln!("update: 失败：最新 release 缺产物（要 {asset_tar} 与 SHA256SUMS）");
        eprintln!("  本机平台产物名 = {asset_tar}");
        std::process::exit(1);
    };

    // ---- ③ 下载 + 哈希校验 ----
    let fetch = |url: &str, cap: usize, budget: Duration| -> minhttp::Response {
        let (host, path) = minhttp::https_parts(url).unwrap_or_else(|| {
            eprintln!("update: 失败：下载地址不是 https：{url}");
            std::process::exit(1);
        });
        match minhttp::get(&host, &path, None, Instant::now() + budget, cap) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("update: 失败：下载 {url} 不通（{e}）");
                std::process::exit(1);
            }
        }
    };
    println!("update: 下载 {asset_tar} …");
    let dl_tar = fetch(&tar_url, ARTIFACT_CAP, Duration::from_secs(180));
    if dl_tar.status != 200 {
        eprintln!("update: 失败：产物下载状态 {}", dl_tar.status);
        std::process::exit(1);
    }
    let dl_sums = fetch(&sums_url, SUMS_CAP, Duration::from_secs(30));
    if dl_sums.status != 200 {
        eprintln!("update: 失败：SHA256SUMS 下载状态 {}", dl_sums.status);
        std::process::exit(1);
    }
    let sums = String::from_utf8_lossy(&dl_sums.body).into_owned();
    let Some(want) = sha256_for(&sums, &asset_tar) else {
        eprintln!("update: 失败：SHA256SUMS 里找不到 {asset_tar} 的行");
        std::process::exit(1);
    };
    let got = {
        use sha2::{Digest as _, Sha256};
        let mut h = Sha256::new();
        h.update(&dl_tar.body);
        hex_lower(&h.finalize())
    };
    if got != want {
        eprintln!("update: 失败：哈希不齐（sha256 期望 {want}，实得 {got}）——中止，现役未动");
        std::process::exit(1);
    }
    println!("update: sha256 校验通过（{}…，{} 字节）", &want[..12], dl_tar.body.len());

    // ---- ④ 解包 + 烟囱（不过 = 中止，不动现役）----
    let bin = match extract_homeway(&dl_tar.body) {
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

/// SHA256SUMS 里找 `<asset>` 行的哈希（shasum 形态：`<hex>  <name>` / `<hex> *<name>`）。
fn sha256_for(sums: &str, asset: &str) -> Option<String> {
    for line in sums.lines() {
        let mut it = line.split_whitespace();
        let (Some(h), Some(n)) = (it.next(), it.next()) else { continue };
        if h.len() == 64 && n.trim_start_matches('*') == asset {
            return Some(h.to_ascii_lowercase());
        }
    }
    None
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

    #[test]
    fn sha256sums_parsing_forms() {
        let sums = "abc0000000000000000000000000000000000000000000000000000000000def  homeway-v0.3.21-linux-amd64.tar.gz\n\
                    1111111111111111111111111111111111111111111111111111111111111111 *homeway-v0.3.21-darwin-arm64.tar.gz\n\
                    短行\n";
        assert_eq!(
            sha256_for(sums, "homeway-v0.3.21-darwin-arm64.tar.gz").as_deref(),
            Some("1111111111111111111111111111111111111111111111111111111111111111")
        );
        assert_eq!(
            sha256_for(sums, "homeway-v0.3.21-linux-amd64.tar.gz").as_deref(),
            Some("abc0000000000000000000000000000000000000000000000000000000000def")
        );
        assert_eq!(sha256_for(sums, "不存在.tar.gz"), None);
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
