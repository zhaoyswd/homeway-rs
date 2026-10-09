//! tunStatusJSON 契约产出（语义真源 `baseline:clientcore/cmd/clientcore/tunmode.go:564`
//! 的 `tunStatusJSON`；消费契约 = `tier:tailcat/src/main/cpp/types/libtailcat/Index.d.ts`）。
//!
//! **完整键面**（Go map 经 json.Marshal 按字典序输出——serde_json `Map`（BTreeMap）同序，
//! 对照测试断言键序不只键集合）：
//! - 恒有：`state`（idle|preparing|ready|attached|failed）、`code`（机器可读原因码：
//!   core/attach/stopped/attach-timeout/空）、`reason`、`meowed`、`readyBy`、`elapsedMs`、
//!   `running`（0|1——等价 ClientCoreTunRunning）、`demand{active,reason,at,fg}`；
//! - demand 内可选：`outboundAt`（unix 毫秒，最近出站包时刻）、`localErrAdopted`、
//!   `localErrTotal`（传输面在场才有）；
//! - 可选：`unhealthyReason`（不健康原因分类：patrol/fd/panic/stop）；
//! - runner 在场：`stats{fdReadBytes,fdWriteBytes,pfAccepted,pfFails}`、`exitIp`（出口隧道
//!   IP——dns-host-resolver：VpnConfig 的 dnsAddresses 消费）、`link{via,ep,rttMs,at}`、
//!   `portForwards[{listen,target,state,err,code,conns}]`、桥在场时
//!   `bridgeAuth`+`bridgeFilesSock`+`bridgeTermSock`+`bridgeSpeedSock`；
//! - 传输在场：`identity{dev,pub}`（**顶层键**——与 serviceSnapshotJSON 的嵌在 link 内
//!   不同）、`tunIp`（应用面第二派生地址，有效才有）。
//!
//! 纯函数面：不触任何全局——输入 `TunStatusInput` 由 facade 持有方组装（对照测试直接
//! 喂构造输入，同 Go `TestServiceSnapshotJSONReadyWithBridgeKeys` 的守卫形态）。

use serde_json::{Map, Value};

use super::demand::DemandState;
use super::stage::StageSnapshot;

/// 一条端口映射的运行状态（Go pfState；conns = 当前活跃转发连接数）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PfStateIn {
    pub listen: u16,
    pub target: String,
    /// listening | failed
    pub state: String,
    pub err: String,
    /// 失败映射的稳定错误码（portfwd/err 词表；成功/listening 为空串）。
    pub code: String,
    pub conns: i64,
}

/// 桥状态（Go bridgeHost 的 sockJSON/authHex 面；桥未起 = None）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BridgeIn {
    /// hex(魔数+令牌)，96 hex 字符；只经状态通道分发，不落盘不进日志。
    pub auth_hex: String,
    pub files_sock: String,
    pub term_sock: String,
    pub speed_sock: String,
}

/// 链路快照（link 段；via = direct|relay|none）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkIn {
    pub via: String,
    pub ep: String,
    pub rtt_ms: i64,
    /// unix 毫秒。
    pub at_ms: i64,
}

/// runner 在场的整块输入（stats/exitIp/link/portForwards/bridge）。
#[derive(Debug, Clone)]
pub struct RunnerIn {
    pub fd_read_bytes: u64,
    pub fd_write_bytes: u64,
    pub pf_accepted: u64,
    pub pf_fails: u64,
    pub exit_ip: String,
    pub link: LinkIn,
    pub port_forwards: Vec<PfStateIn>,
    pub bridge: Option<BridgeIn>,
}

/// 传输在场的身份/应用面地址输入。
#[derive(Debug, Clone)]
pub struct TransportIn {
    /// (dev 短指纹, pub 短指纹)——只公钥/devTag，私钥不外出。
    pub identity: Option<(String, String)>,
    /// 应用面（VpnConfig/TUN）地址：l3-exit-intercept D4 的第二派生地址；
    /// prepare 就绪后扩展读它建 VPN 接口（单一来源，不与隧道 IP 混用）。
    pub tun_ip: Option<String>,
    /// 最近出站包时刻（unix 毫秒；None = 本世代还没发过）。
    pub outbound_at_ms: Option<i64>,
    /// （采纳的本地错误数, 本地发送错误总数）。
    pub local_err: Option<(u64, u64)>,
}

/// tunStatusJSON 的完整输入。
#[derive(Debug, Clone, Default)]
pub struct TunStatusInput {
    pub stage: StageSnapshot,
    /// running 判据（单飞锁 + 健康位 + attached 的合成；调用方持有这三个信号）。
    pub running: bool,
    pub demand: DemandState,
    /// fg 诊断位（不参与需求合成——仅给诊断面一个出口）。
    pub demand_fg: bool,
    pub unhealthy_reason: Option<String>,
    pub runner: Option<RunnerIn>,
    pub transport: Option<TransportIn>,
    /// QUIC 岛快照段（M1 S3-2；**additive 平级段**——`None` = 本世代非 quic 档/岛不在）。
    pub quic: Option<QuicIn>,
}

/// 四类丢弃计数（M1 S3-3：N-c 行与 JSON **同源**——同一份 `IslandSnapshot::drops`）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct QuicDropsIn {
    pub too_large: u64,
    pub send_buffer_full: u64,
    pub return_queue_full: u64,
    pub unregistered: u64,
}

/// QUIC 岛快照（M1 S3-2 的 `quic` 段；字段清单 = S2b 交下的 `IslandSnapshot` 全量，
/// **键名照抄**——本段是 M1 新增面（无 Go 对照），键名即契约，改动须登记）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct QuicIn {
    /// `max_datagram_size()` 现值（0 = 未建连）。
    pub mtu: u32,
    /// `current_mtu`（DPLPMTUD/黑障后的现值）。
    pub current_mtu: u16,
    pub lost_packets: u64,
    pub congestion_events: u64,
    pub migrations: u64,
    pub migration_unconfirmed: bool,
    pub drops: QuicDropsIn,
    /// `direct|relay|none`（与 link 段同一词表）。
    pub via: String,
    pub ep: String,
    pub rtt_ms: u64,
    pub packets_in: u64,
    pub packets_out: u64,
    /// 当前本地地址（UDP 源端口；空串 = 未就绪）——detach 后仍可读（M0 §8.1 残余面）。
    pub local: String,
    pub connections: u64,
    pub relay_tx: u64,
    pub rx_ignored: u64,
    pub candidates: u64,
    pub mirrors: u64,
    /// 已占用（未确认）的 DATAGRAM 发送缓冲字节数（M1 交下项 N8① / M2 S2-5；
    /// **瞬时量**：无连接 = 0，满 = 1 MiB）。黑洞期「已入缓冲的 1 MiB」的可观测面。
    pub send_buffer_used: u64,
}

/// 快照 → tunStatusJSON（Go tunStatusJSON 逐键对齐；键序 = 字典序）。
pub fn tun_status_json(input: &TunStatusInput) -> String {
    let st = &input.stage;
    let elapsed_ms = st.since.elapsed().as_millis() as i64;

    let mut m = Map::new();
    m.insert("state".into(), Value::String(st.stage.as_str().into()));
    m.insert("code".into(), Value::String(st.code.clone()));
    m.insert("reason".into(), Value::String(st.reason.clone()));
    m.insert("meowed".into(), Value::from(st.meowed));
    m.insert("readyBy".into(), Value::String(st.ready_by.clone()));
    m.insert("elapsedMs".into(), Value::from(elapsed_ms));
    m.insert("running".into(), Value::from(i32::from(input.running)));

    // demand 段：reason 空 = 从未判定（Go 兜「未判定」）
    let mut dm = Map::new();
    dm.insert("active".into(), Value::from(input.demand.active));
    dm.insert(
        "reason".into(),
        Value::String(if input.demand.reason.is_empty() {
            "未判定".to_owned()
        } else {
            input.demand.reason.clone()
        }),
    );
    dm.insert("at".into(), Value::from(input.demand.at_ms));
    dm.insert("fg".into(), Value::from(input.demand_fg));
    if let Some(t) = &input.transport {
        if let Some(at) = t.outbound_at_ms {
            dm.insert("outboundAt".into(), Value::from(at));
        }
        if let Some((adopted, total)) = t.local_err {
            dm.insert("localErrAdopted".into(), Value::from(adopted));
            dm.insert("localErrTotal".into(), Value::from(total));
        }
    }
    m.insert("demand".into(), Value::Object(dm));

    if let Some(why) = &input.unhealthy_reason {
        if !why.is_empty() {
            m.insert("unhealthyReason".into(), Value::String(why.clone()));
        }
    }

    if let Some(r) = &input.runner {
        let mut stats = Map::new();
        stats.insert("fdReadBytes".into(), Value::from(r.fd_read_bytes));
        stats.insert("fdWriteBytes".into(), Value::from(r.fd_write_bytes));
        stats.insert("pfAccepted".into(), Value::from(r.pf_accepted));
        stats.insert("pfFails".into(), Value::from(r.pf_fails));
        m.insert("stats".into(), Value::Object(stats));
        m.insert("exitIp".into(), Value::String(r.exit_ip.clone()));
        let mut link = Map::new();
        link.insert("via".into(), Value::String(r.link.via.clone()));
        link.insert("ep".into(), Value::String(r.link.ep.clone()));
        link.insert("rttMs".into(), Value::from(r.link.rtt_ms));
        link.insert("at".into(), Value::from(r.link.at_ms));
        m.insert("link".into(), Value::Object(link));
        m.insert(
            "portForwards".into(),
            Value::Array(
                r.port_forwards
                    .iter()
                    .map(|p| {
                        let mut e = Map::new();
                        e.insert("listen".into(), Value::from(p.listen));
                        e.insert("target".into(), Value::String(p.target.clone()));
                        e.insert("state".into(), Value::String(p.state.clone()));
                        e.insert("err".into(), Value::String(p.err.clone()));
                        e.insert("code".into(), Value::String(p.code.clone()));
                        e.insert("conns".into(), Value::from(p.conns));
                        Value::Object(e)
                    })
                    .collect(),
            ),
        );
        if let Some(b) = &r.bridge {
            m.insert("bridgeAuth".into(), Value::String(b.auth_hex.clone()));
            m.insert("bridgeFilesSock".into(), Value::String(b.files_sock.clone()));
            m.insert("bridgeTermSock".into(), Value::String(b.term_sock.clone()));
            m.insert("bridgeSpeedSock".into(), Value::String(b.speed_sock.clone()));
        }
    }

    if let Some(t) = &input.transport {
        // identity = 顶层键（与 service 形态嵌在 link 内不同——对照 Go tunmode.go:642）
        if let Some((dev, pubk)) = &t.identity {
            let mut id = Map::new();
            id.insert("dev".into(), Value::String(dev.clone()));
            id.insert("pub".into(), Value::String(pubk.clone()));
            m.insert("identity".into(), Value::Object(id));
        }
        if let Some(ip) = &t.tun_ip {
            m.insert("tunIp".into(), Value::String(ip.clone()));
        }
    }

    // M1 S3-2：`quic` 段（**additive 平级段**；岛不在 = 整段缺席 ⇒ 旧读者零影响）
    if let Some(q) = &input.quic {
        let mut qm = Map::new();
        qm.insert("mtu".into(), Value::from(q.mtu));
        qm.insert("current_mtu".into(), Value::from(q.current_mtu));
        qm.insert("lost_packets".into(), Value::from(q.lost_packets));
        qm.insert("congestion_events".into(), Value::from(q.congestion_events));
        qm.insert("migrations".into(), Value::from(q.migrations));
        qm.insert(
            "migration_unconfirmed".into(),
            Value::from(q.migration_unconfirmed),
        );
        let mut d = Map::new();
        d.insert("too_large".into(), Value::from(q.drops.too_large));
        d.insert(
            "send_buffer_full".into(),
            Value::from(q.drops.send_buffer_full),
        );
        d.insert(
            "return_queue_full".into(),
            Value::from(q.drops.return_queue_full),
        );
        d.insert("unregistered".into(), Value::from(q.drops.unregistered));
        qm.insert("drops".into(), Value::Object(d));
        qm.insert("via".into(), Value::String(q.via.clone()));
        qm.insert("ep".into(), Value::String(q.ep.clone()));
        qm.insert("rtt_ms".into(), Value::from(q.rtt_ms));
        qm.insert("packets_in".into(), Value::from(q.packets_in));
        qm.insert("packets_out".into(), Value::from(q.packets_out));
        qm.insert("local".into(), Value::String(q.local.clone()));
        qm.insert("connections".into(), Value::from(q.connections));
        qm.insert("relay_tx".into(), Value::from(q.relay_tx));
        qm.insert("rx_ignored".into(), Value::from(q.rx_ignored));
        qm.insert("candidates".into(), Value::from(q.candidates));
        qm.insert("mirrors".into(), Value::from(q.mirrors));
        qm.insert("send_buffer_used".into(), Value::from(q.send_buffer_used));
        m.insert("quic".into(), Value::Object(qm));
    }

    Value::Object(m).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::facade::stage::TunStage;
    use std::time::Instant;

    fn stage_in(stage: TunStage, code: &str, reason: &str, meowed: bool, ready_by: &str) -> StageSnapshot {
        StageSnapshot {
            stage,
            code: code.to_owned(),
            reason: reason.to_owned(),
            meowed,
            ready_by: ready_by.to_owned(),
            since: Instant::now(),
        }
    }

    /// **Go 快照对照**（fixtures/vectors/tun_status.jsonl——baseline 克隆内
    /// TestVecgenTunStatus 产的真 Go 字节；无 runner 期的全部可达形态）。逐案构造
    /// 等价输入，断言字节相等（elapsedMs 随墙钟——两侧统一归一 0）。
    #[test]
    fn go_vectors_stage_face() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/vectors/tun_status.jsonl");
        let raw = std::fs::read_to_string(path).expect("tun_status.jsonl 应在（tools/gen-vectors.sh 产）");
        let mut seen = 0;
        for line in raw.lines().filter(|l| !l.trim().is_empty()) {
            let rec: Value = serde_json::from_str(line).unwrap();
            let name = rec["name"].as_str().unwrap();
            let want = rec["json"].as_str().unwrap().to_owned();
            let got_raw = tun_status_json(&vec_case_input(name));
            // elapsedMs 归一（Go 产端已归 0；Rust 侧 since=now → 0~1ms 段替 0）
            let got = match got_raw.split_once("\"elapsedMs\":") {
                Some((pre, rest)) => {
                    let tail = rest.find(',').unwrap_or(rest.len());
                    format!("{pre}\"elapsedMs\":0{}", &rest[tail..])
                }
                None => got_raw,
            };
            assert_eq!(got, want, "case {name}: Go 向量对账失败");
            seen += 1;
        }
        assert_eq!(seen, 10, "向量应有 10 案（生成器加案时同步本断言）");
    }

    /// 向量案名 → 等价 TunStatusInput（与 vecgen_tunstatus_test.go 的驱动逐案对照）。
    /// 注意 ⑤–⑩ 案的 readyBy="wg"：**Go 的 setStage 不清 readyBy**（只在世代起点清），
    /// 向量实证——ready_softfail 之后的全部阶段都带着上一世代的判据。
    fn vec_case_input(name: &str) -> TunStatusInput {
        let demand = |active: bool, reason: &str, at: i64| DemandState {
            active,
            reason: reason.to_owned(),
            at_ms: at,
        };
        let base = |stage, demand, fg| TunStatusInput {
            stage,
            running: false,
            demand,
            demand_fg: fg,
            ..Default::default()
        };
        match name {
            "idle" => base(stage_in(TunStage::Idle, "", "", false, ""), demand(false, "", 0), false),
            "preparing" => base(stage_in(TunStage::Preparing, "", "", false, ""), demand(false, "", 0), false),
            "ready_meowed" => base(stage_in(TunStage::Ready, "", "", true, "wg"), demand(false, "", 0), false),
            "ready_softfail" => base(stage_in(TunStage::Ready, "", "暖机窗口内未收到注册确认", false, "wg"), demand(false, "", 0), false),
            "failed_core" => base(stage_in(TunStage::Failed, "core", "新栈启动失败：token 解析失败", false, "wg"), demand(false, "", 0), false),
            "attach_timeout_idle" => base(stage_in(TunStage::Idle, "attach-timeout", "就绪后无人 attach，已自行收工放锁", false, "wg"), demand(false, "", 0), false),
            "demand_unset" => base(stage_in(TunStage::Preparing, "", "", false, "wg"), demand(false, "", 0), false),
            "demand_screen_on" => base(stage_in(TunStage::Preparing, "", "", false, "wg"), demand(true, "亮屏", 1696000000000), false),
            "demand_stale_screen_fg" => base(stage_in(TunStage::Preparing, "", "", false, "wg"), demand(false, "熄屏（位陈旧）", 1696000000000), true),
            "unhealthy_patrol" => TunStatusInput {
                unhealthy_reason: Some("patrol".into()),
                ..base(stage_in(TunStage::Preparing, "", "", false, "wg"), demand(false, "熄屏（位陈旧）", 1696000000000), true)
            },
            other => panic!("未知向量案名 {other}"),
        }
    }

    /// 恒有键 + 字典序（Go json.Marshal(map) 同序）。
    #[test]
    fn base_key_face_and_order() {
        let input = TunStatusInput {
            stage: stage_in(TunStage::Preparing, "", "", false, ""),
            running: false,
            demand: DemandState { active: false, reason: String::new(), at_ms: 0 },
            demand_fg: false,
            ..Default::default()
        };
        let json = tun_status_json(&input);
        let v: Value = serde_json::from_str(&json).unwrap();
        let keys: Vec<&str> = v.as_object().unwrap().keys().map(|k| k.as_str()).collect();
        assert_eq!(
            keys,
            vec!["code", "demand", "elapsedMs", "meowed", "readyBy", "reason", "running", "state"]
        );
        // demand 段键面与「未判定」兜底
        let d = &v["demand"];
        let dk: Vec<&str> = d.as_object().unwrap().keys().map(|k| k.as_str()).collect();
        assert_eq!(dk, vec!["active", "at", "fg", "reason"]);
        assert_eq!(d["reason"], "未判定");
        assert_eq!(d["at"], 0);
    }

    /// 完整键面（attached + runner + transport + bridge + portForwards + unhealthy）。
    /// 对照形态 = Go TestServiceSnapshotJSONReadyWithBridgeKeys 的键集合守卫（构造输入直喂）。
    #[test]
    fn full_key_face_attached() {
        let input = TunStatusInput {
            stage: stage_in(TunStage::Attached, "", "", true, "wg"),
            running: true,
            demand: DemandState { active: true, reason: "亮屏".into(), at_ms: 1696000000000 },
            demand_fg: true,
            unhealthy_reason: Some("patrol".into()),
            runner: Some(RunnerIn {
                fd_read_bytes: 111,
                fd_write_bytes: 222,
                pf_accepted: 3,
                pf_fails: 1,
                exit_ip: "100.64.255.1".into(),
                link: LinkIn {
                    via: "direct".into(),
                    ep: "192.168.3.12:41641".into(),
                    rtt_ms: 12,
                    at_ms: 1696000005000,
                },
                port_forwards: vec![PfStateIn {
                    listen: 18080,
                    target: "主机:8080".into(),
                    state: "listening".into(),
                    err: String::new(),
                    code: String::new(),
                    conns: 2,
                }],
                bridge: Some(BridgeIn {
                    auth_hex: "ab".repeat(48),
                    files_sock: "/b/files.sock".into(),
                    term_sock: "/b/term.sock".into(),
                    speed_sock: "/b/speedtest.sock".into(),
                }),
            }),
            transport: Some(TransportIn {
                identity: Some(("7dc61647".into(), "6e18bb87".into())),
                tun_ip: Some("100.64.73.151".into()),
                outbound_at_ms: Some(1696000009000),
                local_err: Some((0, 0)),
            }),
            quic: None,
        };
        let json = tun_status_json(&input);
        let v: Value = serde_json::from_str(&json).unwrap();
        let keys: Vec<&str> = v.as_object().unwrap().keys().map(|k| k.as_str()).collect();
        assert_eq!(
            keys,
            vec![
                "bridgeAuth", "bridgeFilesSock", "bridgeSpeedSock", "bridgeTermSock", "code",
                "demand", "elapsedMs", "exitIp", "identity", "link", "meowed", "portForwards",
                "readyBy", "reason", "running", "state", "stats", "tunIp", "unhealthyReason",
            ]
        );
        assert_eq!(v["running"], 1);
        assert_eq!(v["readyBy"], "wg");
        assert_eq!(v["exitIp"], "100.64.255.1");
        assert_eq!(v["identity"]["dev"], "7dc61647");
        assert_eq!(v["tunIp"], "100.64.73.151");
        assert_eq!(v["stats"]["fdReadBytes"], 111);
        assert_eq!(v["portForwards"][0]["listen"], 18080);
        assert_eq!(v["portForwards"][0]["conns"], 2);
        assert_eq!(v["unhealthyReason"], "patrol");
        // demand 内的可选三键随 transport 出现
        assert_eq!(v["demand"]["outboundAt"], 1696000009000i64);
        assert_eq!(v["demand"]["localErrAdopted"], 0);
        let dk: Vec<&str> = v["demand"].as_object().unwrap().keys().map(|k| k.as_str()).collect();
        assert_eq!(dk, vec!["active", "at", "fg", "localErrAdopted", "localErrTotal", "outboundAt", "reason"]);
    }

    /// **判据（M1 S3-2）**：`quic` 段 = **additive 平级段**——①字段齐（S2b 交下的
    /// 全量清单）；②岛不在（`None`）= 整段缺席，既有键面/键序逐字不变（旧读者零影响）。
    #[test]
    fn quic_section_is_additive_and_complete() {
        // ② 缺席形态：既有键面不变（对照 full_key_face_attached 的键表——无 `quic`）
        let bare = TunStatusInput {
            stage: stage_in(TunStage::Attached, "", "", true, "wg"),
            running: true,
            demand: DemandState { active: false, reason: String::new(), at_ms: 0 },
            demand_fg: false,
            runner: None,
            transport: None,
            unhealthy_reason: None,
            quic: None,
        };
        let v: Value = serde_json::from_str(&tun_status_json(&bare)).unwrap();
        assert!(v.get("quic").is_none(), "岛不在 ⇒ 整段缺席");

        // ① 在场形态：字段齐 + 键序（serde_json Map = 字典序）
        let input = TunStatusInput {
            quic: Some(QuicIn {
                mtu: 1362,
                current_mtu: 1400,
                lost_packets: 3,
                congestion_events: 1,
                migrations: 2,
                migration_unconfirmed: true,
                drops: QuicDropsIn {
                    too_large: 1,
                    send_buffer_full: 2,
                    return_queue_full: 3,
                    unregistered: 4,
                },
                via: "relay".into(),
                ep: "192.168.3.12:42652".into(),
                rtt_ms: 12,
                packets_in: 5,
                packets_out: 6,
                local: "192.168.3.12:54123".into(),
                connections: 1,
                relay_tx: 7,
                rx_ignored: 8,
                candidates: 4,
                mirrors: 9,
                send_buffer_used: 4096,
            }),
            ..bare
        };
        let v: Value = serde_json::from_str(&tun_status_json(&input)).unwrap();
        let qk: Vec<&str> = v["quic"].as_object().unwrap().keys().map(|k| k.as_str()).collect();
        assert_eq!(
            qk,
            vec![
                "candidates", "congestion_events", "connections", "current_mtu", "drops", "ep",
                "local", "lost_packets", "migration_unconfirmed", "migrations", "mirrors", "mtu",
                "packets_in", "packets_out", "relay_tx", "rtt_ms", "rx_ignored",
                "send_buffer_used", "via",
            ]
        );
        let dk: Vec<&str> = v["quic"]["drops"].as_object().unwrap().keys().map(|k| k.as_str()).collect();
        assert_eq!(
            dk,
            vec!["return_queue_full", "send_buffer_full", "too_large", "unregistered"]
        );
        assert_eq!(v["quic"]["drops"]["too_large"], 1);
        assert_eq!(v["quic"]["migration_unconfirmed"], true);
        assert_eq!(v["quic"]["mtu"], 1362);
        assert_eq!(v["quic"]["via"], "relay");
        assert_eq!(v["quic"]["send_buffer_used"], 4096, "S2-5 的读数位进 JSON");
    }

    /// runner 缺席 ⇒ stats/exitIp/link/portForwards/bridge 全缺（failed/prepare 期形态）。
    #[test]
    fn no_runner_keys_absent() {
        let input = TunStatusInput {
            stage: stage_in(TunStage::Failed, "core", "新栈启动失败：token 解析失败", false, ""),
            running: false,
            demand: DemandState { active: false, reason: String::new(), at_ms: 0 },
            demand_fg: false,
            ..Default::default()
        };
        let v: Value = serde_json::from_str(&tun_status_json(&input)).unwrap();
        for k in ["stats", "exitIp", "link", "portForwards", "bridgeAuth", "identity", "tunIp", "unhealthyReason"] {
            assert!(v.get(k).is_none(), "failed 态不应含 {k}");
        }
        assert_eq!(v["code"], "core");
        assert_eq!(v["state"], "failed");
    }

    /// bridge 缺席（桥未起/identityDir 未配置）⇒ 桥四键缺，runner 其余键在。
    #[test]
    fn runner_without_bridge() {
        let input = TunStatusInput {
            stage: stage_in(TunStage::Attached, "", "", true, "wg"),
            running: true,
            demand: DemandState { active: false, reason: String::new(), at_ms: 0 },
            demand_fg: false,
            runner: Some(RunnerIn {
                fd_read_bytes: 0,
                fd_write_bytes: 0,
                pf_accepted: 0,
                pf_fails: 0,
                exit_ip: "100.64.255.1".into(),
                link: LinkIn { via: "none".into(), ep: String::new(), rtt_ms: 0, at_ms: 0 },
                port_forwards: vec![],
                bridge: None,
            }),
            transport: None,
            ..Default::default()
        };
        let v: Value = serde_json::from_str(&tun_status_json(&input)).unwrap();
        for k in ["bridgeAuth", "bridgeFilesSock", "bridgeTermSock", "bridgeSpeedSock"] {
            assert!(v.get(k).is_none(), "无桥不应含 {k}");
        }
        assert!(v.get("link").is_some());
        // 空映射表 = 空数组（Go make([]map,0,0) → []）
        assert_eq!(v["portForwards"], Value::Array(vec![]));
    }
}
