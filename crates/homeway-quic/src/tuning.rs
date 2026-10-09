//! 可配常量面（M3 §15-2/§15-3）：**流窗口 / 流并发 / 待发队列 / 快探参数 / 发送面新鲜度窗**。
//!
//! 形态照 `HOMEWAY_QUIC_MTU` 先例（M1 §12-①）：**env 优先 → 显式配置 → 设计缺省**；
//! 非法或越界 ⇒ **不改该项** + 返回一行「按缺省走」说明（调用方按 `Logf` 落行）。
//!
//! 为什么这些是常量而不是「调优」：初值都带设计文档给的账（`M3-design.md` §1.4/§1.7/§7/
//! §8.2-16），改动即偏离设计（须先登记再改——§15-3）；env 消融臂供 S8 门槛/真机标定。
//!
//! **本文件纯 std**（隔离门 ② 条扫描面：不得出现 `tokio::|quinn|rustls|async fn|.await`）
//! ——两端（岛侧客户端面与出口面）与同步面（S7 的配置登记条）读同一份值域。

use std::time::Duration;

// ---------------------------------------------------------------------------
// 流面限制（§1.7 的定值 + §8.2-16 的配置条）
// ---------------------------------------------------------------------------

/// 流窗口 / 并发 / 队列的**设计定值**（§1.7；初值与值域都不许悄悄漂）。
pub mod stream_defaults {
    /// `max_concurrent_bidi_streams`（quinn TP；§1.7：quinn 缺省 100 ⇒ 收窄到 64）。
    pub const MAX_BIDI: u32 = 64;
    /// `max_concurrent_uni_streams`（§1.7-N13：本期限定「反向不开流」⇒ **0**；
    /// 与「`Cmd::Probe` 换 tag=5 真回显」**同切片**落地，否则巡检定音失效）。
    pub const MAX_UNI: u32 = 0;
    /// `stream_receive_window`（每流接收窗；quinn 缺省 1.19 MiB ⇒ 收窄到 256 KiB）。
    pub const RECV_WINDOW: u32 = 256 * 1024;
    /// `send_window`（**连接级**、多流共享；quinn 缺省 10 MB ⇒ 收窄到 2 MiB）。
    pub const SEND_WINDOW: u32 = 2 * 1024 * 1024;
    /// 每流**待发队列**上界（§1.4：有界待发，懒分配）。
    pub const PENDING_BYTES: usize = 64 * 1024;
    /// 自记账域固定占用（§1.4-N14）：控制流 1 + probe 持久流 1。
    pub const RESERVED_STREAMS: u32 = 2;
}

/// 流面限制（两端共用同一份值 ⇒ 写进 [`crate::exit::transport`] 的 `TransportConfig`）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct StreamLimits {
    /// `max_concurrent_bidi_streams`（quinn TP）。
    pub max_bidi: u32,
    /// `max_concurrent_uni_streams`（本期 = 0）。
    pub max_uni: u32,
    /// `stream_receive_window`（字节）。
    pub recv_window: u32,
    /// `send_window`（字节；**连接级**）。
    pub send_window: u32,
    /// 每流待发队列上界（字节）。
    pub pending_bytes: usize,
}

impl Default for StreamLimits {
    fn default() -> Self {
        Self {
            max_bidi: stream_defaults::MAX_BIDI,
            max_uni: stream_defaults::MAX_UNI,
            recv_window: stream_defaults::RECV_WINDOW,
            send_window: stream_defaults::SEND_WINDOW,
            pending_bytes: stream_defaults::PENDING_BYTES,
        }
    }
}

impl StreamLimits {
    /// 设计定值（= [`Default`]；具名入口供「配置条 vs 环境覆盖」两处读同一份）。
    pub fn design() -> Self {
        Self::default()
    }

    /// 有效服务流容量（§1.4-N14：自记账域 = {控制流, probe 持久流, 服务流}）。
    pub const fn service_capacity(&self) -> usize {
        self.max_bidi
            .saturating_sub(stream_defaults::RESERVED_STREAMS) as usize
    }

    /// env 覆盖（key → 值域）：返回生效条数与「非法/越界 ⇒ 按缺省」说明行。
    ///
    /// 值域写在这里（**不是** env 解析处）：越界值会让 quinn 侧行为不可预期
    /// （如 0 并发 = 服务流全开不出来），故必须挡在进 `TransportConfig` 之前。
    pub fn apply_env(&mut self, get: &dyn Fn(&str) -> Option<String>) -> (usize, Vec<String>) {
        let mut applied = 0;
        let mut notes = Vec::new();
        // `max_bidi`：下限 3（= 保留 2 + 至少 1 条服务流），上限 1024（quinn 面充裕）。
        match parse_env_u64(get, ENV_STREAMS, 3, 1024) {
            EnvOutcome::Value(v) => {
                self.max_bidi = v as u32;
                applied += 1;
            }
            EnvOutcome::Rejected(note) => notes.push(note),
            EnvOutcome::Unset => {}
        }
        // 每流接收窗：32 KiB（小于此值 bulk 服务流会被窗口拖死）… 4 MiB（超 quinn 缺省即回到内存账）。
        match parse_env_u64(get, ENV_STREAM_WINDOW, 32 * 1024, 4 * 1024 * 1024) {
            EnvOutcome::Value(v) => {
                self.recv_window = v as u32;
                applied += 1;
            }
            EnvOutcome::Rejected(note) => notes.push(note),
            EnvOutcome::Unset => {}
        }
        // 连接级发送窗（多流共享）：256 KiB…64 MiB。
        match parse_env_u64(get, ENV_SEND_WINDOW, 256 * 1024, 64 * 1024 * 1024) {
            EnvOutcome::Value(v) => {
                self.send_window = v as u32;
                applied += 1;
            }
            EnvOutcome::Rejected(note) => notes.push(note),
            EnvOutcome::Unset => {}
        }
        // 每流待发队列：4 KiB（小于单条 bulk 写 = 恒背压）…4 MiB（超过即单流可吃掉进程序列）。
        match parse_env_u64(get, ENV_STREAM_PENDING, 4096, 4 * 1024 * 1024) {
            EnvOutcome::Value(v) => {
                self.pending_bytes = v as usize;
                applied += 1;
            }
            EnvOutcome::Rejected(note) => notes.push(note),
            EnvOutcome::Unset => {}
        }
        (applied, notes)
    }
}

/// env 名（S7 的配置登记条读它；生产/消融两边不许各写一份）。
pub const ENV_STREAMS: &str = "HOMEWAY_QUIC_STREAMS";
/// env 名：每流接收窗（字节）。
pub const ENV_STREAM_WINDOW: &str = "HOMEWAY_QUIC_STREAM_WINDOW";
/// env 名：连接级发送窗（字节）。
pub const ENV_SEND_WINDOW: &str = "HOMEWAY_QUIC_SEND_WINDOW";
/// env 名：每流待发队列（字节）。
pub const ENV_STREAM_PENDING: &str = "HOMEWAY_QUIC_STREAM_PENDING";

// ---------------------------------------------------------------------------
// 快探参数（§3.2/§15-2；S4 消费——S1 落值域与消融臂）
// ---------------------------------------------------------------------------

/// 快探/恢复的**初值**（§3.2 的三段节拍 + §3.1 的 B 门 + §14.4-1 的「待标定」项）。
pub mod probe_defaults {
    use std::time::Duration;

    /// 在用档首探预算（§13-T2 实测：预算 700ms/间隔 0 ⇒ 706ms 定音）。
    pub const FAST_BUDGET: Duration = Duration::from_millis(700);
    /// 复探倍数（§3.1-2：首探失败 ⇒ 本拍内用加倍预算复探一次）。
    pub const REPROBE_FACTOR: u32 = 2;
    /// 待机档节拍（= `PATROL_INTERVAL`，不变——保电池/CPU）。
    pub const IDLE_INTERVAL: Duration = Duration::from_secs(60);
    /// 抖动连续升格阈值（§3.1-③：防 fail-silent）。
    pub const JITTER_STREAK: u32 = 3;
    /// B 门：连续 R 失败次数（§3.1：连续 2 次且窗 ≥10s）。
    pub const RECONNECT_STREAK: u32 = 2;
    /// B 门：累计失败窗（同上）。
    pub const REBUILD_WINDOW: Duration = Duration::from_secs(10);
    /// 本机发送面错误的**新鲜度窗**（§3.1-N5：照 `demand::OUTBOUND_FRESH` 先例）。
    pub const SEND_ERR_FRESH: Duration = Duration::from_secs(5);
}

/// 快探/恢复参数（S4 的阶梯重写消费；值域与 env 臂在 S1 落地）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ProbeTuning {
    /// 首探预算。
    pub fast_budget: Duration,
    /// 复探倍数（≥1）。
    pub reprobe_factor: u32,
    /// 待机档节拍。
    pub idle_interval: Duration,
    /// 抖动连续升格阈值。
    pub jitter_streak: u32,
    /// B 门：连续 R 失败次数。
    pub reconnect_streak: u32,
    /// B 门：累计失败窗。
    pub rebuild_window: Duration,
    /// 本机发送面错误的新鲜度窗（N5；M/R 判别用）。
    pub send_err_fresh: Duration,
}

impl Default for ProbeTuning {
    fn default() -> Self {
        Self {
            fast_budget: probe_defaults::FAST_BUDGET,
            reprobe_factor: probe_defaults::REPROBE_FACTOR,
            idle_interval: probe_defaults::IDLE_INTERVAL,
            jitter_streak: probe_defaults::JITTER_STREAK,
            reconnect_streak: probe_defaults::RECONNECT_STREAK,
            rebuild_window: probe_defaults::REBUILD_WINDOW,
            send_err_fresh: probe_defaults::SEND_ERR_FRESH,
        }
    }
}

impl ProbeTuning {
    /// 设计定值（= [`Default`]）。
    pub fn design() -> Self {
        Self::default()
    }

    /// env 覆盖（时长类 env 的单位一律 **ms**；照 `HOMEWAY_QUIC_MTU` 的「整数裸值」形态）。
    pub fn apply_env(&mut self, get: &dyn Fn(&str) -> Option<String>) -> (usize, Vec<String>) {
        let mut applied = 0;
        let mut notes = Vec::new();
        env_ms(get, ENV_PROBE_BUDGET, 50, 10_000, &mut self.fast_budget, &mut applied, &mut notes);
        env_u32(
            get,
            ENV_PROBE_REPROBE,
            1,
            4,
            &mut self.reprobe_factor,
            &mut applied,
            &mut notes,
        );
        env_ms(get, ENV_PROBE_IDLE, 5_000, 600_000, &mut self.idle_interval, &mut applied, &mut notes);
        env_u32(get, ENV_JITTER_STREAK, 1, 10, &mut self.jitter_streak, &mut applied, &mut notes);
        env_u32(
            get,
            ENV_RECONNECT_STREAK,
            1,
            10,
            &mut self.reconnect_streak,
            &mut applied,
            &mut notes,
        );
        env_ms(get, ENV_REBUILD_WINDOW, 1_000, 300_000, &mut self.rebuild_window, &mut applied, &mut notes);
        env_ms(
            get,
            ENV_SEND_ERR_FRESH,
            500,
            60_000,
            &mut self.send_err_fresh,
            &mut applied,
            &mut notes,
        );
        (applied, notes)
    }
}

/// env 名：首探预算（ms）。
pub const ENV_PROBE_BUDGET: &str = "HOMEWAY_QUIC_PROBE_BUDGET";
/// env 名：复探倍数。
pub const ENV_PROBE_REPROBE: &str = "HOMEWAY_QUIC_PROBE_REPROBE";
/// env 名：待机档节拍（ms）。
pub const ENV_PROBE_IDLE: &str = "HOMEWAY_QUIC_PROBE_IDLE";
/// env 名：抖动连续升格阈值。
pub const ENV_JITTER_STREAK: &str = "HOMEWAY_QUIC_JITTER_STREAK";
/// env 名：B 门连续失败次数。
pub const ENV_RECONNECT_STREAK: &str = "HOMEWAY_QUIC_RECONNECT_STREAK";
/// env 名：B 门失败窗（ms）。
pub const ENV_REBUILD_WINDOW: &str = "HOMEWAY_QUIC_REBUILD_WINDOW";
/// env 名：发送面错误新鲜度窗（ms）。
pub const ENV_SEND_ERR_FRESH: &str = "HOMEWAY_QUIC_SEND_ERR_FRESH";

// ---------------------------------------------------------------------------
// env 解析小件（纯函数；测试直接喂 `get` 闭包，不碰进程环境）
// ---------------------------------------------------------------------------

/// 一次 env 读取的结论（**不改**调用侧值 ⇒ 未设/非法项按缺省走）。
enum EnvOutcome {
    /// env 未设（常态；不打行、不改值）。
    Unset,
    /// 合法值（在值域内）。
    Value(u64),
    /// 非法或越界：说明行（调用方落 `Logf`）。
    Rejected(String),
}

/// 读一个「非负整数 + 闭区间」env（照 `HOMEWAY_QUIC_MTU` 的记行形态：**不夹取**）。
fn parse_env_u64(get: &dyn Fn(&str) -> Option<String>, name: &str, min: u64, max: u64) -> EnvOutcome {
    let Some(raw) = get(name) else {
        return EnvOutcome::Unset;
    };
    match raw.trim().parse::<u64>() {
        Ok(v) if (min..=max).contains(&v) => EnvOutcome::Value(v),
        _ => EnvOutcome::Rejected(format!(
            "quic: ⚠️ {name}={raw} 非法或越界（有效区间 [{min},{max}]）—— 该项按设计缺省走（照 HOMEWAY_QUIC_MTU 先例）"
        )),
    }
}

/// 生产 env 读取口（`std::env::var` 的唯一封装；测试用闭包替代）。
pub fn env_get(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

/// 一项「毫秒数」env 的施加（未设 ⇒ 静默；非法/越界 ⇒ 不改值 + 一行说明）。
fn env_ms(
    get: &dyn Fn(&str) -> Option<String>,
    name: &str,
    min: u64,
    max: u64,
    slot: &mut Duration,
    applied: &mut usize,
    notes: &mut Vec<String>,
) {
    match parse_env_u64(get, name, min, max) {
        EnvOutcome::Value(v) => {
            *slot = Duration::from_millis(v);
            *applied += 1;
        }
        EnvOutcome::Rejected(note) => notes.push(note),
        EnvOutcome::Unset => {}
    }
}

/// 一项 `u32` env 的施加（同 [`env_ms`]）。
fn env_u32(
    get: &dyn Fn(&str) -> Option<String>,
    name: &str,
    min: u64,
    max: u64,
    slot: &mut u32,
    applied: &mut usize,
    notes: &mut Vec<String>,
) {
    match parse_env_u64(get, name, min, max) {
        EnvOutcome::Value(v) => {
            *slot = v as u32;
            *applied += 1;
        }
        EnvOutcome::Rejected(note) => notes.push(note),
        EnvOutcome::Unset => {}
    }
}

/// 施加 env 覆盖并按 `Logf` 落说明行的便捷口（岛/出口的启动路径共用）。
///
/// 返回生效条数（0 = 全缺省）。**未设**的项不打行；**非法**项打行后按缺省走。
pub fn apply_stream_env(lim: &mut StreamLimits, logf: &crate::cmd::Logf) -> usize {
    let (applied, notes) = lim.apply_env(&env_get);
    for n in &notes {
        (*logf)(n);
    }
    applied
}

/// 施加快探 env 覆盖（同 [`apply_stream_env`]）。
pub fn apply_probe_env(t: &mut ProbeTuning, logf: &crate::cmd::Logf) -> usize {
    let (applied, notes) = t.apply_env(&env_get);
    for n in &notes {
        (*logf)(n);
    }
    applied
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 未设 env 的闭包（生产缺省路径）。
    fn none(_: &str) -> Option<String> {
        None
    }

    /// 固定表（测试用；避免碰进程环境）。
    fn table(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
        move |k: &str| {
            pairs
                .iter()
                .find(|(n, _)| *n == k)
                .map(|(_, v)| (*v).to_owned())
        }
    }

    /// **判据（§15-3 初值 = 设计定值）**：缺省逐值照设计（含 `uni=0` 与 N14 的自记账域）。
    #[test]
    fn stream_limits_default_to_design_values() {
        let d = StreamLimits::design();
        assert_eq!(d.max_bidi, 64, "§1.7 的并发上限");
        assert_eq!(d.max_uni, 0, "§1.7-N13：本期 uni=0");
        assert_eq!(d.recv_window, 256 * 1024, "每流接收窗");
        assert_eq!(d.send_window, 2 * 1024 * 1024, "连接级发送窗");
        assert_eq!(d.pending_bytes, 64 * 1024, "每流待发队列");
        // N14：有效服务流容量 = 上限 − 2（控制流 + probe 持久流）
        assert_eq!(d.service_capacity(), 62);
        assert_eq!(d, StreamLimits::default());
    }

    /// **判据（env 消融臂，§15-2）**：合法值逐项生效；非法/越界 ⇒ 不改该项 + 一行说明。
    #[test]
    fn stream_env_overrides_apply_or_are_rejected_with_note() {
        // ① 合法：四项全生效
        let mut lim = StreamLimits::design();
        let get = table(&[
            ("HOMEWAY_QUIC_STREAMS", "32"),
            ("HOMEWAY_QUIC_STREAM_WINDOW", "131072"),
            ("HOMEWAY_QUIC_SEND_WINDOW", "1048576"),
            ("HOMEWAY_QUIC_STREAM_PENDING", "16384"),
        ]);
        let (applied, notes) = lim.apply_env(&get);
        assert_eq!(applied, 4, "四项都命中：{notes:?}");
        assert!(notes.is_empty(), "合法值不产说明行：{notes:?}");
        assert_eq!(lim.max_bidi, 32);
        assert_eq!(lim.service_capacity(), 30);
        assert_eq!(lim.recv_window, 131_072);
        assert_eq!(lim.send_window, 1_048_576);
        assert_eq!(lim.pending_bytes, 16_384);

        // ② 非法（非数字/负数/越界）⇒ 该项按缺省 + 说明行；其余项不受影响
        let mut lim = StreamLimits::design();
        let get = table(&[
            ("HOMEWAY_QUIC_STREAMS", "2"),         // 越界下限（< 3）
            ("HOMEWAY_QUIC_STREAM_WINDOW", "abc"), // 非数字
            ("HOMEWAY_QUIC_SEND_WINDOW", "-1"),    // 负数
            ("HOMEWAY_QUIC_STREAM_PENDING", "4194305"), // 越界上限（> 4 MiB）
        ]);
        let (applied, notes) = lim.apply_env(&get);
        assert_eq!(applied, 0);
        assert_eq!(notes.len(), 4, "四条各自一行：{notes:?}");
        assert_eq!(lim, StreamLimits::design(), "非法项一律不改值");
        for n in &notes {
            assert!(n.contains("非法或越界"), "说明行形态：{n}");
            assert!(n.contains("按设计缺省走"), "说明行要点：{n}");
        }

        // ③ 未设 ⇒ 既不生效也不产行（常态路径零噪声）
        let mut lim = StreamLimits::design();
        let (applied, notes) = lim.apply_env(&none);
        assert_eq!(applied, 0);
        assert!(notes.is_empty(), "未设不打行：{notes:?}");

        // ④ 边界值收（含 = 上下界）
        let mut lim = StreamLimits::design();
        let get = table(&[
            ("HOMEWAY_QUIC_STREAMS", "3"),
            ("HOMEWAY_QUIC_STREAM_WINDOW", "4194304"),
        ]);
        let (applied, _) = lim.apply_env(&get);
        assert_eq!(applied, 2);
        assert_eq!(lim.max_bidi, 3);
        assert_eq!(lim.service_capacity(), 1, "下限 = 恰一条服务流容量");
    }

    /// **判据（快探参数初值 + 消融臂，§15-2/§3.2）**：缺省 = 设计值；env 命中逐项生效。
    #[test]
    fn probe_tuning_defaults_and_env_overrides() {
        let d = ProbeTuning::design();
        assert_eq!(d.fast_budget, Duration::from_millis(700), "§13-T2 的承重值");
        assert_eq!(d.reprobe_factor, 2);
        assert_eq!(d.idle_interval, Duration::from_secs(60));
        assert_eq!(d.jitter_streak, 3);
        assert_eq!(d.reconnect_streak, 2, "B 门：连续 2");
        assert_eq!(d.rebuild_window, Duration::from_secs(10), "B 门：窗 ≥10s");
        assert_eq!(d.send_err_fresh, Duration::from_secs(5), "N5 新鲜度窗");

        let mut t = ProbeTuning::design();
        let get = table(&[
            ("HOMEWAY_QUIC_PROBE_BUDGET", "300"),
            ("HOMEWAY_QUIC_PROBE_REPROBE", "3"),
            ("HOMEWAY_QUIC_PROBE_IDLE", "5000"),
            ("HOMEWAY_QUIC_JITTER_STREAK", "5"),
            ("HOMEWAY_QUIC_RECONNECT_STREAK", "4"),
            ("HOMEWAY_QUIC_REBUILD_WINDOW", "30000"),
            ("HOMEWAY_QUIC_SEND_ERR_FRESH", "1500"),
        ]);
        let (applied, notes) = t.apply_env(&get);
        assert_eq!(applied, 7, "七项全命中：{notes:?}");
        assert!(notes.is_empty());
        assert_eq!(t.fast_budget, Duration::from_millis(300));
        assert_eq!(t.reprobe_factor, 3);
        assert_eq!(t.idle_interval, Duration::from_secs(5));
        assert_eq!(t.jitter_streak, 5);
        assert_eq!(t.reconnect_streak, 4);
        assert_eq!(t.rebuild_window, Duration::from_secs(30));
        assert_eq!(t.send_err_fresh, Duration::from_millis(1500));

        // 越界（复探倍数 0 / 新鲜度窗 100ms）⇒ 不生效 + 两行说明
        let mut t = ProbeTuning::design();
        let get = table(&[
            ("HOMEWAY_QUIC_PROBE_REPROBE", "0"),
            ("HOMEWAY_QUIC_SEND_ERR_FRESH", "100"),
        ]);
        let (applied, notes) = t.apply_env(&get);
        assert_eq!(applied, 0);
        assert_eq!(notes.len(), 2);
        assert_eq!(t, ProbeTuning::design());
    }

    /// env 名常量是**单源**（生产/消融/登记三处不许各写一份字面量）。
    #[test]
    fn env_names_are_the_single_source() {
        assert_eq!(ENV_STREAMS, "HOMEWAY_QUIC_STREAMS");
        assert_eq!(ENV_STREAM_WINDOW, "HOMEWAY_QUIC_STREAM_WINDOW");
        assert_eq!(ENV_SEND_WINDOW, "HOMEWAY_QUIC_SEND_WINDOW");
        assert_eq!(ENV_STREAM_PENDING, "HOMEWAY_QUIC_STREAM_PENDING");
        assert_eq!(ENV_PROBE_BUDGET, "HOMEWAY_QUIC_PROBE_BUDGET");
        assert_eq!(ENV_PROBE_REPROBE, "HOMEWAY_QUIC_PROBE_REPROBE");
        assert_eq!(ENV_PROBE_IDLE, "HOMEWAY_QUIC_PROBE_IDLE");
        assert_eq!(ENV_JITTER_STREAK, "HOMEWAY_QUIC_JITTER_STREAK");
        assert_eq!(ENV_RECONNECT_STREAK, "HOMEWAY_QUIC_RECONNECT_STREAK");
        assert_eq!(ENV_REBUILD_WINDOW, "HOMEWAY_QUIC_REBUILD_WINDOW");
        assert_eq!(ENV_SEND_ERR_FRESH, "HOMEWAY_QUIC_SEND_ERR_FRESH");
    }
}
