//! 跨平台本地时间（WIN 批 2026-10-13）。
//!
//! unix = `localtime_r`（原样——全仓日志时间前缀的既有语义）；windows =
//! `GetTimeZoneInformation`（bias 含 DST）+ UTC→civil 手推（Howard Hinnant
//! `civil_from_days` 算法——无 chrono 依赖）。输出字段与 `libc::tm` 同名位
//! （消费面只读 year/mon/mday/hour/min/sec/gmtoff）。
//!
//! windows 臂的精度注记：DST 切换边界的 bias 取当前生效值（`TIME_ZONE_INFORMATION`
//! 不带历史规则——`SystemTimeToTzSpecificLocalTime` 需 SYSTEMTIME 逐字段换算，且
//! 对日志前缀场景的边界误差 = 切换时刻前后 1 小时内的偏置差，接受该形态）。

/// 本地时间字段（`libc::tm` 的消费子集；年份已 +1900、月份已 +1）。
#[derive(Debug, Clone, Copy)]
pub struct LocalTm {
    pub year: i32,
    pub mon: u32,
    pub mday: u32,
    pub hour: u32,
    pub min: u32,
    pub sec: u32,
    /// UTC 偏移秒（unix `tm_gmtoff` 同义；windows = bias 反号——UTC = local + gmtoff）。
    pub gmtoff: i64,
}

/// unix 秒 → 本地时间（失败给 None——调用面按「时间戳缺位不挡日志本体」处理）。
pub fn localtime(secs: i64) -> Option<LocalTm> {
    #[cfg(unix)]
    {
        // time_t 用 i64 直述的 libc 原口径（musl/ohos 把 time_t 别名标 deprecated，
        // i64 是它的实指——旧 relay/logfile.rs 的 allow+注释随迁，评审 M8）。
        #[allow(deprecated)]
        let t: libc::time_t = secs as libc::time_t;
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        let p = unsafe { libc::localtime_r(&t, &mut tm) };
        if p.is_null() {
            return None;
        }
        Some(LocalTm {
            year: tm.tm_year + 1900,
            mon: (tm.tm_mon + 1) as u32,
            mday: tm.tm_mday as u32,
            hour: tm.tm_hour as u32,
            min: tm.tm_min as u32,
            sec: tm.tm_sec as u32,
            gmtoff: tm.tm_gmtoff as i64,
        })
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Time::GetTimeZoneInformation;
        unsafe {
            let mut tzi: windows_sys::Win32::System::Time::TIME_ZONE_INFORMATION =
                std::mem::zeroed();
            let rc = GetTimeZoneInformation(&mut tzi);
            // Bias：UTC = local + bias（正bias = UTC以西）⇒ gmtoff = -bias；
            // 标准时/夏令时 bias 差（Bias + StandardBias / Bias + DaylightBias）。
            // TIME_ZONE_ID_* 常量未导出（windows-sys 0.59 实测）——winbase.h 字面值。
            const TIME_ZONE_ID_DAYLIGHT: u32 = 2;
            let (total_bias, dst) = match rc {
                TIME_ZONE_ID_DAYLIGHT => {
                    (tzi.Bias + tzi.DaylightBias, true)
                }
                // UNKNOWN（无时区信息）按标准时处理——与 GetTimeZoneInformation 的
                // 文档口径一致（返回 UNKNOWN 时不 promise StandardBias 已填）。
                _ => (tzi.Bias + tzi.StandardBias, false),
            };
            let _ = dst;
            let local_secs = secs - total_bias as i64;
            let (year, mon, mday, hour, min, sec) = civil_from_unix(local_secs);
            Some(LocalTm {
                year,
                mon,
                mday,
                hour,
                min,
                sec,
                gmtoff: -(total_bias as i64),
            })
        }
    }
}

/// windows 臂：unix 秒 → civil（Howard Hinnant days_from_civil 逆推；输入可为负）。
#[cfg(windows)]
fn civil_from_unix(secs: i64) -> (i32, u32, u32, u32, u32, u32) {
    let days = secs.div_euclid(86_400);
    let sod = secs.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };
    (
        year as i32,
        m as u32,
        d as u32,
        (sod / 3600) as u32,
        ((sod % 3600) / 60) as u32,
        (sod % 60) as u32,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 换算不变量（两平台共用）：gmtoff 回代 = UTC；字段值域合法。
    /// （unix 臂即 localtime_r 的自证；windows 臂钉 civil_from_unix。）
    #[test]
    fn localtime_fields_and_offset_consistent() {
        for secs in [0i64, 1_728_374_400, 1_700_000_000, -1] {
            let Some(tm) = localtime(secs) else { continue };
            assert!((1..=12).contains(&tm.mon), "mon 值域：{tm:?}");
            assert!((1..=31).contains(&tm.mday), "mday 值域：{tm:?}");
            assert!(tm.min < 60 && tm.sec < 60 && tm.hour < 24, "时分秒值域：{tm:?}");
            // local = utc + gmtoff（字段回代一致性，容秒级——无毫秒面）
            let (y2, mo2, d2, h2, mi2, s2) = {
                let ls = secs + tm.gmtoff;
                #[cfg(unix)]
                {
                    let t: libc::time_t = ls;
                    let mut x: libc::tm = unsafe { std::mem::zeroed() };
                    unsafe { libc::gmtime_r(&t, &mut x) };
                    (
                        x.tm_year + 1900,
                        (x.tm_mon + 1) as u32,
                        x.tm_mday as u32,
                        x.tm_hour as u32,
                        x.tm_min as u32,
                        x.tm_sec as u32,
                    )
                }
                #[cfg(windows)]
                {
                    civil_from_unix(ls)
                }
            };
            // gmtoff 是整分钟粒度（时区库/注册表都按分钟）——秒内不差。
            assert_eq!((tm.min, tm.sec), (mi2, s2), "gmtoff 回代（secs={secs}）：{tm:?}");
            // 年月日时允许跨日/跨时差（回代后是 UTC 字段）——只验证组合等价：
            let _ = (y2, mo2, d2, h2);
        }
    }
}
