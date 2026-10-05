//! relay 两级日志 + relay.log 轮转（2MB × 3）。
//!
//! 语义真源 `baseline:internal/relay/logging.go`：
//! - **ulogf**（用户流，终端）：只放 token/端点公告，同时抄进 relay.log；
//! - **logf**（运行日志）：中继全部运行日志（注册腿/会话/回收/分钟统计/告警）只进文件；
//! - 打不开不致命（中继没有必须落盘的状态）：终端提示一句，服务继续。
//!
//! 时间戳前缀与 Go 同形：`2006-01-02 15:04:05.000 [relay] `。
//! 轮转写文件本体在 `crate::logfile`（events/debug/relay 三处共用——P1-1 收拢）。

use std::path::Path;

use crate::logfile::{RotatingWriter, EVENTS_BACKUPS, EVENTS_MAX_BYTES};

/// 时间戳（Go 布局 `2006-01-02 15:04:05.000 [relay] ` 的**本地时区**等价形态：
/// localtime_r 取本地偏移——epoch civil 换算仍是 UTC 基）。
fn stamp() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs_utc = now.as_secs() as i64;
    // 本地时区偏移（libc::localtime_r；失败按 UTC）。time_t 用 i64 直述——
    // libc 对部分目标（musl/ohos）把 time_t 别名标 deprecated，i64 是它的实指。
    #[allow(deprecated)]
    let local_offset: i64 = unsafe {
        let t: libc::time_t = secs_utc as libc::time_t;
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&t, &mut tm).is_null() {
            0
        } else {
            tm.tm_gmtoff as i64
        }
    };
    let secs = secs_utc + local_offset;
    let millis = now.subsec_millis();
    // days since epoch → y/m/d（civil_from_days 算法）
    let days = secs.div_euclid(86400);
    let tod = secs.rem_euclid(86400);
    let (h, m, s) = (tod / 3600, (tod % 3600) / 60, tod % 60);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mth = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if mth <= 2 { y + 1 } else { y };
    format!("{y:04}-{mth:02}-{d:02} {h:02}:{m:02}:{s:02}.{millis:03} ")
}

/// 两级日志句柄：ulogf（终端 + 抄文件）/ logf（只文件）。
/// 终端打印经 `term` 注入（CLI 给 println，测试给收集器）。
pub struct RelayLog {
    file: Option<RotatingWriter>,
    term: crate::Logf,
}

impl RelayLog {
    pub fn open(state_cache_dir: &Path, term: crate::Logf) -> Self {
        let file = match RotatingWriter::open(state_cache_dir, "relay.log", EVENTS_MAX_BYTES, EVENTS_BACKUPS) {
            Ok(f) => Some(f),
            Err(e) => {
                term(&format!(
                    "homeway relay: ⚠️ 文件日志打开失败（ {e} ）—— 本轮日志缺失，服务继续"
                ));
                None
            }
        };
        Self { file, term }
    }

    pub fn log_path(&self) -> Option<String> {
        self.file.as_ref().map(|f| f.path().display().to_string())
    }

    /// 用户流：终端 + 抄文件。
    pub fn ulogf(&self, s: &str) {
        let line = format!("{}[relay] {}", stamp(), s);
        (self.term)(&line);
        if let Some(f) = &self.file {
            f.write_line(&line);
        }
    }

    /// 运行日志：只进文件。
    pub fn logf(&self, s: &str) {
        let line = format!("{}[relay] {}", stamp(), s);
        if let Some(f) = &self.file {
            f.write_line(&line);
        }
    }

    /// 转成 relay Config 的 logf 面。
    pub fn logf_fn(self: &std::sync::Arc<Self>) -> crate::Logf {
        let me = std::sync::Arc::clone(self);
        std::sync::Arc::new(move |s: &str| me.logf(s))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_level_logging() {
        let dir = std::env::temp_dir().join(format!("rllog2-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let seen2 = std::sync::Arc::clone(&seen);
        let term: crate::Logf = std::sync::Arc::new(move |s: &str| {
            seen2.lock().unwrap().push(s.to_owned());
        });
        let log = std::sync::Arc::new(RelayLog::open(&dir, term));
        log.ulogf("中继 token：rl1ABC");
        log.logf("中继：后端 abcd 注册成功（腿 1.2.3.4:9）");
        let lines = std::fs::read_to_string(dir.join("relay.log")).unwrap();
        assert!(lines.contains("中继 token：rl1ABC"), "ulogf 应抄进文件");
        assert!(lines.contains("注册成功"), "logf 应进文件");
        let term_lines = seen.lock().unwrap();
        assert_eq!(term_lines.len(), 1, "终端只见 ulogf 行");
        assert!(term_lines[0].contains("[relay] "), "时间戳前缀形态");
        assert!(term_lines[0].contains("中继 token"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
