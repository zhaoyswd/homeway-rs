//! Go `time.Duration` 的字符串格式（判据行 C5 的 `耗时 %v` 是 Go `%v` 打 Duration——
//! `{:?}` 在 Rust 里输出 `0ns`/`90s`，与 Go 的 `0s`/`1m30s` 必不匹配）。
//!
//! 覆盖判据面会出现的区间（亚毫秒 ~ 小时级）；实现对照 Go `time.Duration.String()`
//! （src/time/time.go Format）与 `Round(Millisecond)`：
//! - 0 → `0s`；500ms → `500ms`；5.025s → `5.025s`；90.5s → `1m30.5s`；61s → `1m1s`；
//! - 分/时段仅在 >0 时出现；秒的小数部分 9 位、去尾零、全零则无小数点。

use std::time::Duration;

/// Go `d.Round(time.Millisecond)` + `String()` 的组合（判据 C5 的实际口径）。
pub fn fmt_duration_go_ms(d: Duration) -> String {
    // Round half away from zero（Go Round 语义）；负数不出现于计时面。
    let ns = d.as_nanos();
    let rounded = ((ns + 500_000) / 1_000_000) * 1_000_000;
    fmt_duration_go_ns(rounded as u64)
}

/// Go `d.Round(time.Second)` + `String()`（巡检空窗行 `巡检空窗 %v` 的口径——
/// service.go:604 `gap.Round(time.Second)`）。
pub fn fmt_duration_go_secs(d: Duration) -> String {
    let ns = d.as_nanos();
    let rounded = ((ns + 500_000_000) / 1_000_000_000) * 1_000_000_000;
    fmt_duration_go_ns(rounded as u64)
}

/// Go `Duration::String()`（输入已取整的纳秒数）。
fn fmt_duration_go_ns(ns: u64) -> String {
    if ns == 0 {
        return "0s".to_owned();
    }
    if ns < 1_000_000_000 {
        // 亚秒：ms/µs 选最大整单位（判据面经 ms 取整后只剩 ms/0s 两形）
        return format!("{}ms", ns / 1_000_000);
    }
    let secs = ns / 1_000_000_000;
    let frac_ns = (ns % 1_000_000_000) as u32;
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    let mut frac = format!("{frac_ns:09}");
    while frac.ends_with('0') {
        frac.pop();
    }
    let mut out = String::new();
    if h > 0 {
        out.push_str(&format!("{h}h"));
    }
    if h > 0 || m > 0 {
        out.push_str(&format!("{m}m"));
    }
    out.push_str(&s.to_string());
    if !frac.is_empty() {
        out.push('.');
        out.push_str(&frac);
    }
    out.push('s');
    out
}

/// 日志时间前缀（Go `logTimePrefix` 同形：`2006-01-02 15:04:05.000 `，本地时区
/// 带毫秒；nodestate events.log / serve 文件日志共用）。libc 的 localtime_r 取本地
/// 时区（无 chrono 依赖；失败回空串——时间戳缺位不挡日志本体）。
///
/// 返回串形如 `2026-10-05 12:34:56.789 `（尾随空格——拼 `[homeway]` 前的 Go 形态）。
pub fn now_log_prefix() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    unsafe {
        let t: libc::time_t = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&t, &mut tm).is_null() {
            return String::new();
        }
        let ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.subsec_millis())
            .unwrap_or(0);
        format!(
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03} ",
            tm.tm_year + 1900,
            tm.tm_mon + 1,
            tm.tm_mday,
            tm.tm_hour,
            tm.tm_min,
            tm.tm_sec,
            ms
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Go 行为锚定用例（go playground 对照：time.Duration(d).Round(ms).String()）。
    #[test]
    fn matches_go_duration_string() {
        for (ns, want) in [
            (0, "0s"),
            (400_000, "0s"),        // 0.4ms → round 到 0
            (600_000, "1ms"),       // 0.6ms → 1ms
            (500_000_000, "500ms"), // 500ms 不进位
            (1_000_000_000, "1s"),
            (5_025_000_000, "5.025s"),
            (5_020_000_000, "5.02s"), // 尾零去除
            (5_000_000_000, "5s"),    // 全零无小数点
            (60_500_000_000, "1m0.5s"),
            (90_000_000_000, "1m30s"),
            (3_661_000_000_000, "1h1m1s"),
        ] {
            assert_eq!(
                fmt_duration_go_ms(Duration::from_nanos(ns)),
                want,
                "ns={ns}"
            );
        }
    }
}
