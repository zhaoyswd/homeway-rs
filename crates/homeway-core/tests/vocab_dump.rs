//! R5-5e 词表编译期真源 dump（评审 G-8：提取面 = 编译期打印，不做源码正则解析——
//! 三种声明形态/诱饵字面量天然消除）。`tools/check-vocab.sh` 消费本输出做三方对账。
//!
//! 输出格式（stdout，稳定）：`<family>/<unit>\t<value>` 行。跑法：
//! `cargo test -p homeway-core --test vocab_dump -- --nocapture | grep $'\t'`

/// speedtest-reason/reason：Rust 核的归因码全集（7 个 REASON_* 常量）。
#[test]
fn vocab_dump() {
    let mut out = String::new();
    // speedtest-reason/reason（faces: napi+cp——Rust 核承担 cli 面）
    for v in [
        homeway_core::speedtest::REASON_BUSY,
        homeway_core::speedtest::REASON_LINK_DOWN,
        homeway_core::speedtest::REASON_NOT_SUPPORTED,
        homeway_core::speedtest::REASON_INTERRUPTED,
        homeway_core::speedtest::REASON_TIMEOUT,
        homeway_core::speedtest::REASON_CANCELLED,
        homeway_core::speedtest::REASON_INVALID_ARG,
    ] {
        out.push_str(&format!("speedtest-reason/reason\t{v}\n"));
    }
    // portfwd/err（faces: napi——bind_failed 产出面；dial_failed/invalid_target 为
    // 登记保留值，本核形态不产出：缺席表在 check-vocab.sh）
    out.push_str(&format!(
        "portfwd/err\t{}\n",
        homeway_core::PortfwdErr::BindFailed.as_str()
    ));
    // event-payload/via（faces: napi+cp——Via::as_str 全集）
    for v in [
        homeway_core::wtransport::Via::Direct,
        homeway_core::wtransport::Via::Relay,
        homeway_core::wtransport::Via::None,
    ] {
        out.push_str(&format!("event-payload/via\t{}\n", v.as_str()));
    }
    // event-payload/state（faces: napi+cp——SessState::as_str 全集）
    for v in [
        homeway_core::session::SessState::Starting,
        homeway_core::session::SessState::Ready,
        homeway_core::session::SessState::Failed,
        homeway_core::session::SessState::Stopping,
        homeway_core::session::SessState::Idle,
    ] {
        out.push_str(&format!("event-payload/state\t{}\n", v.as_str()));
    }
    // files-proto/code（faces: direct+napi——wire 错误码全集；tier 侧本就排除该族
    // 的 manifest 存在性检查，值集仍对 ledger 对账）
    for v in [
        homeway_core::files::CODE_STREAM_OPEN,
        homeway_core::files::CODE_ALREADY_EXISTS,
        homeway_core::files::CODE_CANCELED,
        homeway_core::files::CODE_INVALID_ARG,
        homeway_core::files::CODE_INVALID_NAME,
        homeway_core::files::CODE_IS_DIR,
        homeway_core::files::CODE_NOT_FOUND,
        homeway_core::files::CODE_PERMISSION,
        homeway_core::files::CODE_SERVER_BUSY,
        homeway_core::files::CODE_OP_FAILED,
    ] {
        out.push_str(&format!("files-proto/code\t{v}\n"));
    }
    print!("{out}");
    // 自检（第二道门 低-20 整改：dump 测试本身零断言 ⇒ 格式/族数漂移只会静默丢行）：
    // 五族各在、总行数 = 7+1+3+5+10 = 26、行格式恒 `<family>/<unit>\t<value>`。
    const FAMILY_COUNT: usize = 5;
    const TOTAL_VALUES: usize = 26;
    let mut families: Vec<&str> = Vec::new();
    for line in out.lines() {
        let (unit, value) = line
            .split_once('\t')
            .unwrap_or_else(|| panic!("行缺 TAB 分隔（格式漂移）：{line:?}"));
        assert!(!unit.is_empty() && !value.is_empty(), "空字段：{line:?}");
        assert!(unit.contains('/'), "unit 形态应为 family/name：{unit:?}");
        if families.last() != Some(&unit) && !families.contains(&unit) {
            families.push(unit);
        }
    }
    assert_eq!(families.len(), FAMILY_COUNT, "族数漂移：{families:?}");
    let total: usize = out.lines().count();
    assert_eq!(total, TOTAL_VALUES, "值总数漂移（改词表须同批更新此门与 check-vocab）");
}
