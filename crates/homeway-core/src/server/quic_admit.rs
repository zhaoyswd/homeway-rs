//! `serve.quic_admit` 段的解析与装配（M2 设计 §3.2 六行七键 + env `HOMEWAY_QUIC_ADMIT_RETRY`）。
//!
//! **两套纪律并存**（§3.2 表末的显式注记）：
//!
//! - **config 段**（config.toml 的 `[serve.quic_admit]`）：值域非法 ⇒ **拒启**（照 Q-H 的
//!   严格表纪律，执行点 = `homeway-cli` 的 `load_config_strict` / `validate_file`）；
//! - **env**（`HOMEWAY_QUIC_ADMIT_RETRY`）：非法值 ⇒ **记行 + 缺省**（**不 fail-fast**——
//!   与 M1 的 `HOMEWAY_TRANSPORT` 同纪律：排障开关不该把出口打进死路）。
//!
//! 值域与缺省的**唯一真源** = `homeway_quic::AdmitLimits`（纯 std；本模块只做「env 叠加 +
//! 记行」这一步，故不复制任何数值）。

use crate::Logf;

pub use homeway_quic::{AdmitLimits, RetryPolicy};

/// 排障开关名（§3.2 表末；取值 = `RetryPolicy::VALUES`）。
pub const ENV_RETRY_POLICY: &str = "HOMEWAY_QUIC_ADMIT_RETRY";

/// env 叠加（**env > config**，只覆盖 `retry_policy`）并记行——装配点（`ServeEngine::start`）
/// 在构造 `ExitQuicConfig` 之前调一次。
///
/// 记行口径：
/// - env 缺失 ⇒ 静默（config/缺省照旧）；
/// - env 合法 ⇒ 生效并记一行（覆盖了 config 值时看得见「谁覆盖谁」）；
/// - env 非法 ⇒ 记行 + **保持 config 值**（不 fail-fast）。
pub fn resolve_retry_policy(base: AdmitLimits, raw: Option<&str>, logf: &Logf) -> AdmitLimits {
    let Some(raw) = raw else { return base };
    let mut out = base;
    match RetryPolicy::parse(raw) {
        Some(p) => {
            out.retry_policy = p;
            if p != base.retry_policy {
                (logf)(&format!(
                    "抗放大策略被 env 覆盖（{ENV_RETRY_POLICY}={raw} 覆盖配置 {}）",
                    base.retry_policy.text()
                ));
            }
        }
        None => (logf)(&format!(
            "⚠️ {ENV_RETRY_POLICY}={raw:?} 非法（合法取值 {}）—— 记行后按 {} 走（不 fail-fast）",
            RetryPolicy::VALUES.join("|"),
            base.retry_policy.text()
        )),
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn sink() -> (Logf, std::sync::Arc<Mutex<Vec<String>>>) {
        let lines = std::sync::Arc::new(Mutex::new(Vec::new()));
        let l = std::sync::Arc::clone(&lines);
        (
            std::sync::Arc::new(move |s: &str| l.lock().unwrap().push(s.to_owned())),
            lines,
        )
    }

    /// **判据（S3-4）**：env 非法 ⇒ 记行 + 缺省（**不 fail-fast**）——返回值稳定为 config 值。
    #[test]
    fn invalid_env_logs_and_keeps_config_value() {
        let (logf, lines) = sink();
        let base = AdmitLimits {
            retry_policy: RetryPolicy::Never,
            ..AdmitLimits::default()
        };
        let got = resolve_retry_policy(base, Some("恒开"), &logf);
        assert_eq!(got, base, "非法 env 不得改动生效值");
        let ls = lines.lock().unwrap();
        assert_eq!(ls.len(), 1, "恰好一行记行：{ls:?}");
        assert!(ls[0].contains("非法"), "行文含「非法」：{}", ls[0]);
        assert!(ls[0].contains("不 fail-fast"), "行文标注不 fail-fast：{}", ls[0]);
    }

    /// **判据（S3-4）**：env 合法 ⇒ 覆盖配置值并记行；与配置同值 ⇒ 静默（不噪声）。
    #[test]
    fn valid_env_overrides_and_logs_only_when_effective() {
        let (logf, lines) = sink();
        let base = AdmitLimits::default();
        let got = resolve_retry_policy(base, Some("always"), &logf);
        assert_eq!(got.retry_policy, RetryPolicy::Always, "env 覆盖生效");
        assert_eq!(got.per_src_fails, base.per_src_fails, "只覆盖 retry_policy");
        assert_eq!(lines.lock().unwrap().len(), 1, "覆盖记一行");
        // 同值：静默
        let (logf2, lines2) = sink();
        let got2 = resolve_retry_policy(base, Some(base.retry_policy.text()), &logf2);
        assert_eq!(got2, base);
        assert!(lines2.lock().unwrap().is_empty(), "同值不记行");
        // 缺省：静默
        let (logf3, lines3) = sink();
        assert_eq!(resolve_retry_policy(base, None, &logf3), base);
        assert!(lines3.lock().unwrap().is_empty(), "env 缺失不记行");
    }
}
