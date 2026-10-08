//! 三臂共用件：内层包构造 + 每包 CPU 采样 + JSON 行输出。
//!
//! 转入自 `/tmp/quic-lab`（口径不变；M0 设计 §4.3 的逐项映射表见 `tools/quic-ab/README.md`）。
//! 主指标 = **每包 CPU**（`getrusage` user+sys 微秒 / 成功往返数），不是墙钟吞吐：
//! 本机 loadavg 常态 2-3（PERF-AB §9.15.1：同二进制墙钟可漂 30×）。

/// 合法内层 IPv4 包构造（boringtun 的 `validate_decapsulated_packet` 会按 IP 头解析：
/// version/IHL 与 total length 必须自洽，否则 decap 报 InvalidPacket）。
pub fn ipv4(src: [u8; 4], dst: [u8; 4], len: usize) -> Vec<u8> {
    assert!(len >= 20, "IPv4 头 20B 起步");
    let mut p = vec![0u8; len];
    p[0] = 0x45; // version=4, IHL=5
    p[1] = 0x00; // DSCP/ECN
    p[2..4].copy_from_slice(&(len as u16).to_be_bytes()); // total_length
    p[4..6].copy_from_slice(&0x1234u16.to_be_bytes()); // identification
    p[6..8].copy_from_slice(&0x4000u16.to_be_bytes()); // DF
    p[8] = 64; // TTL
    p[9] = 17; // UDP（载荷内容与本实验无关）
    p[10..12].copy_from_slice(&0u16.to_be_bytes()); // checksum（本实验不校验）
    p[12..16].copy_from_slice(&src);
    p[16..20].copy_from_slice(&dst);
    for (i, b) in p[20..].iter_mut().enumerate() {
        *b = (i % 251) as u8;
    }
    p
}

/// 进程 user+sys CPU 微秒（macOS/Linux：`getrusage(RUSAGE_SELF)`）。
pub fn cpu_now_us() -> (u64, u64) {
    unsafe {
        let mut ru: libc::rusage = std::mem::zeroed();
        if libc::getrusage(libc::RUSAGE_SELF, &mut ru) != 0 {
            return (0, 0);
        }
        let u = ru.ru_utime.tv_sec as u64 * 1_000_000 + ru.ru_utime.tv_usec as u64;
        let s = ru.ru_stime.tv_sec as u64 * 1_000_000 + ru.ru_stime.tv_usec as u64;
        (u, s)
    }
}

/// 环境变量读数（臂的公共开关：`PAYLOAD` / `MTU` / `IDLE_SECS`）。
pub fn env_num<T: std::str::FromStr>(key: &str, default: T) -> T {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// JSON 行输出（三臂共用形态：单行、字段序固定——`cpu-<arm>-r<N>.json` 的内容）。
pub struct JsonLine {
    fields: Vec<(&'static str, String)>,
}

impl JsonLine {
    pub fn new(arm: &str) -> Self {
        Self {
            fields: vec![("arm", format!("\"{arm}\""))],
        }
    }

    pub fn num(mut self, k: &'static str, v: u64) -> Self {
        self.fields.push((k, v.to_string()));
        self
    }

    pub fn int(mut self, k: &'static str, v: i64) -> Self {
        self.fields.push((k, v.to_string()));
        self
    }

    pub fn f(mut self, k: &'static str, v: f64, prec: usize) -> Self {
        self.fields.push((k, format!("{v:.prec$}")));
        self
    }

    pub fn render(&self) -> String {
        let body: Vec<String> = self
            .fields
            .iter()
            .map(|(k, v)| format!("\"{k}\":{v}"))
            .collect();
        format!("{{{}}}", body.join(","))
    }
}
