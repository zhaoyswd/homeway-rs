//! **单承载（QUIC-only）端点面判据**（M5 C4 改写；脚本 = `tools/quic-wg-e2e.sh`）。
//!
//! 四条用例：
//! 1. `wg_only_token_generation_fails_visibly_without_fallback`（**常跑**，无需出口）：
//!    **设计 §2.6-G9 的负例实测**——只带 WG 类端点（`Direct`）的旧 token ⇒ 岛候选为空
//!    （且无 `rpk` 尾字段 ⇒ 落 RPK 面归因）⇒ 世代必须**可见失败**（`岛未就用（…）` 归因行 +
//!    `failed` 终态 + 单飞锁释放），**且不得有任何回落/兜底话术**。token **在本用例内铸造**
//!    （M5 起出口不再能产出该形态——`serve.quic` 键已删，「WG-only token」只作为存量形态存在）。
//! 2. `exit_token_is_single_bearer_quic_only`（`#[ignore]`，需外部本地 Rust 出口在跑）：
//!    **E3 改写后的正向面**——铸出的 token 带 `rpk`、端点**全是 Quic 类**（`Direct` 零命中）。
//! 3. `removed_transport_key_is_ignored`（**常跑**）：M5 删掉的承载键 `tunConfig.transport`
//!    属**未知键**（`TunConfigJson` 未加 `deny_unknown_fields`）⇒ App 继续传不得导致配置被拒
//!    （设计 §4.3 的 R9 核查点，用测试钉住）。
//! 4. `legacy_hmw1_token_surfaces_actionable_changeover_attribution`（**常跑**，M7 S1 新增）：
//!    存量 `hmw1` token（设备 App 主机卡里的旧条目）⇒ status reason 必须带**换代归因**
//!    （哨兵原文 + 「存量 token 已失效」/取证入口/重新粘贴），且不得说成串损坏。
//!
//! 环境契约（用例 2）：`HOMEWAY_WG_E2E_TOKEN`（出口 token）/ `HOMEWAY_WG_E2E_EXIT_LOG`
//! （出口 stdout 日志）。

use std::path::PathBuf;
use std::time::{Duration, Instant};

use homeway_core::facade::demand::DemandSignals;
use homeway_core::facade::tun_exec::TunnelExec;
use homeway_core::facade::ClientCore;

/// 轮询上界（只判上界——flake 口径②）。
const WAIT: Duration = Duration::from_secs(25);

/// 等待判据（有界轮询）。
fn wait_until(mut f: impl FnMut() -> bool, wait: Duration) -> bool {
    let deadline = Instant::now() + wait;
    while Instant::now() < deadline {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

fn log_lines(path: &PathBuf) -> usize {
    std::fs::read_to_string(path)
        .map(|s| s.lines().count())
        .unwrap_or(0)
}

/// 有界等待日志文件**第 `skip` 行之后**出现 `needle`（返回命中行）。
fn wait_log_from(path: &PathBuf, skip: usize, needle: &str, wait: Duration) -> Option<String> {
    let deadline = Instant::now() + wait;
    while Instant::now() < deadline {
        if let Ok(s) = std::fs::read_to_string(path) {
            if let Some(l) = s.lines().skip(skip).find(|l| l.contains(needle)) {
                return Some(l.to_owned());
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    None
}

/// **判据（设计 §2.6-G9）**：只带 WG 类端点（`Direct`）且无 `rpk` 的 token ⇒ 岛候选为空
/// ⇒ **可见失败**：
/// - 世代日志：`岛未就用（…）` 归因行在场（无 `rpk` 尾字段 ⇒ 落 RPK 面归因；带 RPK 但
///   纯 WG 端点的形态落「候选为空」面——两条都使 `tun_exec` 单测与一条 e2e 用例）；
///   **零**回落/兜底话术；**零** `transport: 本世代 L3 承载 =`（A/B 开关行已删）；
/// - 状态面：`state=failed` 且 `reason` 带同一归因（用户/排障可见）；
/// - 生命周期：failed 终态后 `tun_stop` 即收 0（单飞锁已放，无孤儿世代）。
#[test]
fn wg_only_token_generation_fails_visibly_without_fallback() {
    use homeway_core::token::{self, EndpointRef, PeerId, Secret, TokenSpec};
    let peer = PeerId::from([0x11u8; 32]);
    let secret = Secret::from([0x12u8; 32]);
    // 旧 token 形态：只有 WG 类（Direct）端点、无 rpk 尾字段
    let eps = [EndpointRef::new("203.0.113.9:41641", token::EndpointKind::Direct)];
    let token = token::encode(&TokenSpec {
        peer_id: &peer,
        secret: &secret,
        endpoints: &eps,
        rpk: None,
    })
    .expect("token 可编码");
    let tok = homeway_core::token::decode(&token).expect("token 可解");
    assert!(tok.rpk.is_none(), "旧形态：token 不带 rpk 尾字段");
    assert!(
        !tok.endpoints
            .iter()
            .any(|e| e.kind == homeway_core::token::EndpointKind::Quic),
        "旧形态：token 不带 QUIC 类端点：{:?}",
        tok.endpoints
    );

    let dir = std::env::temp_dir().join(format!("hw-m5c4-wgonly-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("临时目录可建");
    let out = dir.join("gen.log");
    let ident_dir = dir.join("identity");
    let _ = std::fs::remove_file(&out);
    // **旧键回传**：App 侧仍传 `transport`（M5 已删的键）⇒ 未知键忽略，不得拒启（R9）
    let cfg = format!(
        r#"{{"token":"{token}","out":"{}","identityDir":"{}","transport":"wg"}}"#,
        out.display(),
        ident_dir.display()
    );
    let demand = std::sync::Arc::new(DemandSignals::new());
    let exec = TunnelExec::new(std::sync::Arc::clone(&demand));
    let core = ClientCore::with_shared(exec, demand);
    assert_eq!(core.tun_prepare(&cfg, true), 0, "prepare 受理（未知键不拒启）");

    let hit = wait_log_from(&out, 0, "岛未就用", WAIT).expect("失败归因行必须可见");
    println!("[g9] attribution={hit}");
    assert!(
        hit.contains("未携带出口 RPK") || hit.contains("候选为空"),
        "归因须指向「无岛可用」（RPK 或候选）：{hit}"
    );
    let all = std::fs::read_to_string(&out).unwrap_or_default();
    for banned in ["回落 WG", "尝试 WG 兜底", "本世代 L3 承载", "按承载分档"] {
        assert!(!all.contains(banned), "单承载后不得出现 `{banned}`：{all}");
    }

    assert!(
        wait_until(|| core.tun_status().contains("\"state\":\"failed\""), WAIT),
        "空候选 ⇒ failed 终态：{}",
        core.tun_status()
    );
    let st = core.tun_status();
    assert!(st.contains("岛未就用"), "失败归因须进 status reason：{st}");
    assert!(
        !st.contains("\"quic\""),
        "岛未构造 ⇒ quic 段缺席（无岛可报）：{st}"
    );
    assert_eq!(core.tun_stop(), 0, "failed 终态后 stop 即收 0（放锁）");
    println!("[g9] state=failed, no fallback, lock released");
}

/// **判据（M7 S1；设计 §10-5 = M6 `§12-1` 交下）**：用户手里**上一代**的 `hmw1` token
/// 撞本代核 ⇒ 失败面必须**点名归因**（不是一句「解析失败」）：
/// - 哨兵原文（Go 对齐字节，App 面可达）：`homeway/token: 不支持的 token 版本: hmw1`
/// - **换代归因**（`TokenError::changeover_attribution`）：点明「存量 token 已失效」+ 取证入口
///   （`serve token`）+ 重新粘贴的动作
/// - `failed` 终态 + 归因进 status reason（**App 用户可见面**，U2 真机实录点）
/// - 不得把换代的必然结果说成「串坏了」（禁 `校验失败` 话术）
///
/// 这条覆盖**设备核换代后设备仍存旧 token** 的主路径（App 主机卡里的存量条目）。
#[test]
fn legacy_hmw1_token_surfaces_actionable_changeover_attribution() {
    // 真 `hmw1` 形态（版本面在 base64 解析之前判死 ⇒ 前缀即足够；不构造旧布局载荷）
    let legacy = "hmw1iu50C3IgUoXEPLRuz7_lQAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
    let err = homeway_core::token::decode(legacy).expect_err("hmw1 串必拒（版本面）");
    let dir = std::env::temp_dir().join(format!("hw-m7s1-legacy-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("临时目录可建");
    let out = dir.join("gen.log");
    let ident_dir = dir.join("identity");
    let _ = std::fs::remove_file(&out);
    let cfg = format!(
        r#"{{"token":"{legacy}","out":"{}","identityDir":"{}"}}"#,
        out.display(),
        ident_dir.display()
    );
    let demand = std::sync::Arc::new(DemandSignals::new());
    let exec = TunnelExec::new(std::sync::Arc::clone(&demand));
    let core = ClientCore::with_shared(exec, demand);
    assert_eq!(
        core.tun_prepare(&cfg, true),
        0,
        "坏 token 是**世代内**失败（装配受理 ⇒ 失败进 status），不是参数面拒启"
    );
    let deadline = Instant::now() + WAIT;
    while core.tun_status().contains("\"state\":\"preparing\"") {
        assert!(Instant::now() < deadline, "版本拒应即时落 failed：{}", core.tun_status());
        std::thread::sleep(Duration::from_millis(20));
    }
    let st = core.tun_status();
    assert!(st.contains("\"state\":\"failed\""), "旧 token ⇒ failed 终态：{st}");
    assert!(
        st.contains("不支持的 token 版本: hmw1"),
        "哨兵原文必须保留（Go 对齐字节 / App 按类归因）：{st}"
    );
    let note = err
        .changeover_attribution()
        .expect("UnsupportedVersion ⇒ 换代归因");
    for needle in ["存量 token 已失效", "hmw2", "serve token"] {
        assert!(note.contains(needle), "换代归因须含 {needle}：{note}");
        assert!(
            st.contains(needle),
            "换代归因必须进 status reason（用户可见）：缺 {needle} —— {st}"
        );
    }
    assert!(
        !st.contains("校验失败"),
        "换代（版本不符）不得被说成串损坏：{st}"
    );
    // 锁已放：下一次 prepare 仍可受理（失败不粘单飞锁）
    assert_eq!(core.tun_prepare(&format!(r#"{{"token":"{legacy}","out":"{}"}}"#, out.display()), true), 0);
    let _ = core.tun_stop();
    println!("[s1] legacy-attribution={note}");
}

/// **判据（E3 改写后的正向面）**：真出口铸出的 token = **单承载 QUIC**——`rpk` 在场、
/// 端点全为 `Quic` 类、`Direct` 类零命中（出口不再有 WG 端口；`tools/quic-wg-e2e.sh` 驱动）。
#[test]
#[ignore = "端到端：需本地 Rust 出口在跑（tools/quic-wg-e2e.sh 驱动）"]
fn exit_token_is_single_bearer_quic_only() {
    let token = std::env::var("HOMEWAY_WG_E2E_TOKEN").expect("须给 HOMEWAY_WG_E2E_TOKEN");
    let tok = homeway_core::token::decode(&token).expect("token 可解");
    assert!(tok.rpk.is_some(), "QUIC 单承载 ⇒ token 必带 rpk 尾字段");
    let kinds: Vec<_> = tok.endpoints.iter().map(|e| e.kind).collect();
    assert!(
        tok.endpoints
            .iter()
            .any(|e| e.kind == homeway_core::token::EndpointKind::Quic),
        "必有 QUIC 类端点：{kinds:?}"
    );
    assert!(
        !tok.endpoints
            .iter()
            .any(|e| e.kind == homeway_core::token::EndpointKind::Direct),
        "WG 类（Direct）端点已退役（单承载）：{kinds:?}"
    );
    // 出口侧对照：日志在场（公共端口/QUIC 面的判据行由驱动脚本断言）
    if let Ok(p) = std::env::var("HOMEWAY_WG_E2E_EXIT_LOG") {
        let exit_log = PathBuf::from(p);
        assert!(log_lines(&exit_log) > 0, "出口日志须在场：{exit_log:?}");
    }
    println!("[token] quic-only 单承载：{} 个端点", tok.endpoints.len());
}

/// **判据（设计 §4.3 的 R9 核查点）**：删掉的承载键 `tunConfig.transport` 是**未知键**
/// （`TunConfigJson` 无 `deny_unknown_fields`）⇒ App 继续传不报错、不拒启。
///
/// 本用例**不需要出口**：只钉「JSON 面接受该键且照常装配」——用一个只带 WG 端点的 token
/// （无 QUIC 候选 ⇒ 世代落 failed）足以证明 prepare 受理（未知键忽略）而非参数面拒启。
#[test]
fn removed_transport_key_is_ignored() {
    use homeway_core::token::{self, EndpointRef, PeerId, RpkPubKey, Secret, TokenSpec};
    let peer = PeerId::from([1u8; 32]);
    let secret = Secret::from([2u8; 32]);
    let rpk = RpkPubKey::from([7u8; 32]);
    let eps = [EndpointRef::new(
        "203.0.113.1:41641",
        token::EndpointKind::Direct,
    )];
    let tok = token::encode(&TokenSpec {
        peer_id: &peer,
        secret: &secret,
        endpoints: &eps,
        rpk: Some(&rpk),
    })
    .expect("token 可编码");

    let dir = std::env::temp_dir().join(format!("hw-m5c4-ukey-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("临时目录可建");
    let out = dir.join("gen.log");
    let ident_dir = dir.join("identity");
    let _ = std::fs::remove_file(&out);
    let cfg = format!(
        r#"{{"token":"{tok}","out":"{}","identityDir":"{}","transport":"quic","quicMtuCap":1400}}"#,
        out.display(),
        ident_dir.display()
    );
    let demand = std::sync::Arc::new(DemandSignals::new());
    let exec = TunnelExec::new(std::sync::Arc::clone(&demand));
    let core = ClientCore::with_shared(exec, demand);
    assert_eq!(
        core.tun_prepare(&cfg, true),
        0,
        "未知键（transport）不得拒启（serde 未 deny_unknown_fields）"
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    while core.tun_status().contains("\"state\":\"preparing\"") && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = core.tun_stop();
}
