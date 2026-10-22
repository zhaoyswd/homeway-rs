//! 出口质量历史的离线查询，不启动 daemon、不读取凭证。

use std::fmt;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use homeway_core::server::quality::{read_report, QualityQuery};

use crate::cli_flags::{self, Val};

const USAGE: &str = "用法：homeway serve quality [--since 10m | --from <时间> [--to <时间>]] [--device <16位devTag>] [--json] [--state <目录>]\n\
  默认最近10分钟；时间支持 Unix 毫秒或带时区 RFC3339，如 2026-10-22T20:10:00+08:00。\n\
  --to 单独使用时查询此前10分钟；--since 与 --from/--to 不能混用。\n\
  读取本机有界历史；无覆盖不代表网络健康。吞吐不是带宽容量，连接不是 HTTP 请求。";

#[derive(Debug)]
enum ArgError {
    Unknown(String),
    Duplicate(String),
    Value { name: String, reason: String },
    Conflict,
    Range,
}

impl fmt::Display for ArgError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unknown(v) => write!(f, "未知参数：{v}"),
            Self::Duplicate(v) => write!(f, "参数重复：--{v}"),
            Self::Value { name, reason } => write!(f, "--{name}：{reason}"),
            Self::Conflict => f.write_str("--since 与 --from/--to 不能混用"),
            Self::Range => f.write_str("时间范围须满足 0 ≤ from < to，时长不得超过给定终点"),
        }
    }
}

impl std::error::Error for ArgError {}

struct Args {
    state: PathBuf,
    query: QualityQuery,
    json: bool,
}

fn invalid(name: &str, reason: &str) -> ArgError {
    ArgError::Value { name: name.into(), reason: reason.into() }
}

fn parse_args(args: &[String], now_ms: u64) -> Result<Args, ArgError> {
    let mut state = None;
    let mut from = None;
    let mut to = None;
    let mut since = None;
    let mut device = None;
    let mut json = false;
    let mut seen = std::collections::HashSet::new();
    let mut i = 0;
    while i < args.len() {
        let raw = &args[i];
        let (name, inline) = cli_flags::split_flag(raw)
            .ok_or_else(|| ArgError::Unknown(raw.clone()))?;
        if !matches!(name, "state" | "from" | "to" | "since" | "device" | "json") {
            return Err(ArgError::Unknown(raw.clone()));
        }
        if !seen.insert(name) {
            return Err(ArgError::Duplicate(name.into()));
        }
        if name == "json" {
            json = cli_flags::take_bool(name, inline, true)
                .map_err(|e| invalid(name, &e))?;
        } else {
            let value = match cli_flags::take_value(inline, args.get(i + 1).map(String::as_str), false) {
                Val::Ok(v) => v,
                Val::Missing(why) => return Err(invalid(name, &format!("缺值（{why}）"))),
                Val::Empty => return Err(invalid(name, "不能是空值")),
            };
            if inline.is_none() {
                i += 1;
            }
            match name {
                "state" => state = Some(PathBuf::from(value)),
                "from" | "to" => {
                    let ms = parse_time(&value).ok_or_else(|| invalid(name,
                        "需要 Unix 毫秒或带明确时区的 RFC3339 时间"))?;
                    if name == "from" { from = Some(ms); } else { to = Some(ms); }
                }
                "since" => {
                    since = Some(cli_flags::parse_duration(&value)
                        .and_then(|d| u64::try_from(d.as_millis()).ok())
                        .filter(|v| *v > 0)
                        .ok_or_else(|| invalid(name, "需要正时长，如 30s、10m、1h（至少1ms，不满一年）"))?);
                }
                "device" => {
                    if value.len() != 16 || !value.bytes().all(|b| b.is_ascii_hexdigit()) {
                        return Err(invalid(name, "需要完整16位十六进制 devTag"));
                    }
                    device = Some(value.to_ascii_lowercase());
                }
                _ => unreachable!("名称已校验"),
            }
        }
        i += 1;
    }
    if since.is_some() && (from.is_some() || to.is_some()) {
        return Err(ArgError::Conflict);
    }
    let to_ms = to.unwrap_or(now_ms);
    let from_ms = match from {
        Some(v) => v,
        None => to_ms.checked_sub(since.unwrap_or(600_000)).ok_or(ArgError::Range)?,
    };
    if from_ms >= to_ms {
        return Err(ArgError::Range);
    }
    Ok(Args {
        state: state.unwrap_or_else(crate::unified_cli::default_state_dir),
        query: QualityQuery { from_ms, to_ms, device },
        json,
    })
}

/// RFC3339 的显式时区子集；纯整数日历运算保证跨平台同义，无本地时区猜测。
fn parse_time(s: &str) -> Option<u64> {
    if !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) {
        return s.parse().ok();
    }
    let b = s.as_bytes();
    if b.len() < 20 || !s.is_ascii() || b[4] != b'-' || b[7] != b'-'
        || !matches!(b[10], b'T' | b't') || b[13] != b':' || b[16] != b':' {
        return None;
    }
    let num = |a: usize, z: usize| -> Option<i64> {
        let v = b.get(a..z)?;
        if !v.iter().all(u8::is_ascii_digit) { return None; }
        s.get(a..z)?.parse().ok()
    };
    let year = num(0, 4)?;
    let month = num(5, 7)?;
    let day = num(8, 10)?;
    let h = num(11, 13)?;
    let m = num(14, 16)?;
    let sec = num(17, 19)?;
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let max_day = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => if leap { 29 } else { 28 },
        _ => return None,
    };
    if year < 1970 || !(1..=max_day).contains(&day) || h > 23 || m > 59 || sec > 59 {
        return None;
    }
    let mut tz = 19;
    let mut millis = 0;
    if b.get(tz) == Some(&b'.') {
        tz += 1;
        let start = tz;
        while b.get(tz).is_some_and(u8::is_ascii_digit) { tz += 1; }
        if tz == start || tz - start > 9 { return None; }
        for j in 0..3 {
            millis = millis * 10 + if start + j < tz { (b[start + j] - b'0') as i64 } else { 0 };
        }
    }
    let offset = match b.get(tz)? {
        b'Z' | b'z' if tz + 1 == b.len() => 0,
        sign @ (b'+' | b'-') if tz + 6 == b.len() && b[tz + 3] == b':' => {
            let hours = num(tz + 1, tz + 3)?;
            let minutes = num(tz + 4, tz + 6)?;
            // -00:00 表示本地偏移未知，不适合作为明确查询时间。
            if hours > 23 || minutes > 59 || (*sign == b'-' && hours == 0 && minutes == 0) { return None; }
            (hours * 3600 + minutes * 60) * if *sign == b'+' { 1 } else { -1 }
        }
        _ => return None,
    };
    let y = year - i64::from(month <= 2);
    let era = y / 400;
    let yoe = y - era * 400;
    let mp = month + if month > 2 { -3 } else { 9 };
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    u64::try_from((days * 86_400 + h * 3600 + m * 60 + sec - offset) * 1000 + millis).ok()
}

pub(crate) fn cmd_quality(args: &[String]) {
    if args.iter().any(|s| matches!(s.as_str(), "--help" | "-h")) {
        println!("{USAGE}");
        return;
    }
    let now = SystemTime::now().duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis().min(u64::MAX as u128) as u64).unwrap_or(0);
    let args = match parse_args(args, now) {
        Ok(v) => v,
        Err(e) => { eprintln!("homeway serve quality：{e}\n{USAGE}"); std::process::exit(2); }
    };
    let report = match read_report(&args.state, args.query) {
        Ok(v) => v,
        Err(e) => { eprintln!("读取出口质量历史失败：{e}"); std::process::exit(1); }
    };
    if args.json {
        match serde_json::to_string_pretty(&report) {
            Ok(s) => println!("{s}"),
            Err(e) => { eprintln!("质量报告 JSON 编码失败：{e}"); std::process::exit(1); }
        }
    } else {
        println!("{}", report.render_text());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &[&str]) -> Vec<String> { s.iter().map(|s| s.to_string()).collect() }

    #[test]
    fn timezone_and_fraction_are_exact() {
        let utc = parse_time("2026-10-22T12:10:00Z").unwrap();
        assert_eq!(parse_time("2026-10-22T20:10:00+08:00"), Some(utc));
        assert_eq!(parse_time("2026-10-22T07:10:00-05:00"), Some(utc));
        assert_eq!(parse_time("2026-10-22T12:10:00.123456Z"), Some(utc + 123));
        assert_eq!(parse_time("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_time("2000-03-01T00:00:00Z"), Some(951_868_800_000));
        assert_eq!(parse_time("2024-02-29T00:00:00Z"), Some(1_709_164_800_000));
        assert_eq!(parse_time(&utc.to_string()), Some(utc));
    }

    #[test]
    fn rejects_ambiguous_and_invalid_dates_without_panicking() {
        for s in ["", "2026-10-22T12:10:00", "2026-02-29T00:00:00Z", "2100-02-29T00:00:00Z",
            "2026-00-01T00:00:00Z", "2026-01-00T00:00:00Z", "2026-01-01T24:00:00Z",
            "2026-01-01T00:00:60Z", "2026-01-01T00:00:00-00:00", "2026-01-01T00:00:00+24:00",
            "2026-01-01T00:00:00.Z", "2026-01-01T00:00:00.1234567890Z", "2026-01-01T00:00:00Zextra",
            "2026-一月-22T12:00:00Z", "18446744073709551616"] {
            assert_eq!(parse_time(s), None, "{s}");
        }
    }

    #[test]
    fn ranges_filters_and_inline_flags() {
        let a = parse_args(&args(&["--since=2m", "--device", "ABCDEF0123456789", "--json=false", "--state=/tmp/quality-cli"]), 1_000_000).unwrap();
        assert_eq!(a.query.from_ms, 880_000);
        assert_eq!(a.query.to_ms, 1_000_000);
        assert_eq!(a.query.device.as_deref(), Some("abcdef0123456789"));
        assert!(!a.json);
        assert_eq!(a.state, PathBuf::from("/tmp/quality-cli"));
        let a = parse_args(&args(&["--from=10", "--to=20", "--json"]), 1_000_000).unwrap();
        assert_eq!((a.query.from_ms, a.query.to_ms), (10, 20));
        assert!(a.json);
        let a = parse_args(&args(&["--to=900000"]), 1_000_000).unwrap();
        assert_eq!(a.query.from_ms, 300_000);
    }

    #[test]
    fn malformed_flags_never_fall_back_to_production_state() {
        for s in [vec!["--state", "--json"], vec!["--state="], vec!["--from=20", "--to=10"],
            vec!["--since=10m", "--from=1"], vec!["--since=0s"], vec!["--since=0.1ms"],
            vec!["--device=abcd"], vec!["--json=maybe"], vec!["--to=10"], vec!["--json", "--json"],
            vec!["--typo=1"], vec!["stray"]] {
            assert!(parse_args(&args(&s), 1_000_000).is_err(), "{s:?}");
        }
    }
}
