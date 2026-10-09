//! ProbeAddr / ProbeReach 的 JSON 呈现面（语义真源
//! `baseline:clientcore/cmd/clientcore/app_probe.go` / `app_probe_reach.go`）。
//!
//! 两者都是「JSON 进、JSON 出、永不 reject」的纯呈现层；解析本体在 `crate::token`，
//! 探测本体在 `crate::probe`（编排由调用方装配——App 进程旁路、独立临时 socket、
//! 总预算 ~3s）。成功/失败信封逐字段对齐：
//! - ProbeAddr 成功 `{"ok":true,"peer":"<公钥前 6 字节 hex>","endpoints":[…]}`
//!   （relay 端点加 `relay:` 前缀）；失败 `{"error":"…"}`。
//! - ProbeReach 成功 `{"ok":true,"peer":…,"endpoints":[…],"results":[{ep,rtt_ms,build,relay}…]}`
//!   （results 只含应答端点，死端点静默）；解析失败 `{"error":"…"}`。

use serde_json::{Map, Value};

use crate::token::{EndpointKind, Token, TokenError};

/// errJSON（两导出共用；`{"error":"…"}` 单键）。
fn err_json(msg: &str) -> String {
    let mut m = Map::new();
    m.insert("error".into(), Value::String(msg.to_owned()));
    Value::Object(m).to_string()
}

/// 端点呈现（relay 加前缀；ProbeAddr/ProbeReach 共用）。
///
/// M1 起 QUIC 类端点加 `quic:` 前缀（**additive**：`relay:` 与裸地址两种既有形态不变；
/// App 侧只透传字符串——登记条见 M1 设计 §3.6 的 token 端点表条）。
fn endpoints_of(token: &Token) -> Vec<String> {
    token
        .endpoints
        .iter()
        .map(|ep| match ep.kind {
            EndpointKind::Relay => format!("relay:{}", ep.addr),
            EndpointKind::Quic => format!("quic:{}", ep.addr),
            EndpointKind::Direct => ep.addr.clone(),
        })
        .collect()
}

/// peer 短指纹（公钥前 6 字节 hex——`fmt.Sprintf("%x", tok.PeerID[:6])` 同形）。
fn peer_hex(token: &Token) -> String {
    token.peer_id.as_bytes()[..6].iter().map(|b| format!("{b:02x}")).collect()
}

/// ClientCoreProbeAddr：token → `{"ok":true,"peer":…,"endpoints":[…]}`；解析失败 `{"error":…}`。
/// 入参做 TrimSpace（Go strings.TrimSpace 同语义）。
pub fn probe_addr_json(token_raw: &str) -> String {
    let raw = token_raw.trim();
    match crate::token::decode(raw) {
        Ok(tok) => {
            let mut m = Map::new();
            m.insert("ok".into(), Value::from(true));
            m.insert("peer".into(), Value::String(peer_hex(&tok)));
            m.insert(
                "endpoints".into(),
                Value::Array(endpoints_of(&tok).into_iter().map(Value::String).collect()),
            );
            Value::Object(m).to_string()
        }
        Err(e) => err_json(&token_error_text(&e)),
    }
}

/// TokenError 的对外文案（Go errJSON 消费 err.Error()；Rust 面把类型化错误折成展示文本）。
fn token_error_text(e: &TokenError) -> String {
    e.to_string()
}

/// 单端点结论（只回报活端点；JSON 字段序 = Go reachResult 声明序——struct 序非 map 序，
/// serde derive 同声明序对齐）。
#[derive(serde::Serialize)]
pub struct ReachEntry {
    pub ep: String,
    pub rtt_ms: i64,
    pub build: String,
    pub relay: bool,
}

/// ProbeReach 的探测报告（`pkg/probe.Reach` 的呈现子集——编排本体由调用方装配）。
pub struct ReachReport {
    pub peer: String,
    pub endpoints: Vec<String>,
    /// 只含应答端点（死端点静默；中继应答为尾部能力——旧版中继不回探测 ⇒ 无应答
    /// 不断言中继故障，App 侧只做正面确认）。
    pub results: Vec<ReachEntry>,
}

/// ClientCoreProbeReach：token → 探测 → `{"ok":true,"peer":…,"endpoints":[…],
/// "results":[…]}`。外层 = Go map 字典序（字段按字母序声明），results 元素 =
/// Go reachResult struct 声明序（ep,rtt_ms,build,relay）——serde_json 的 `to_value`
/// 会经 BTreeMap 重排，故整报告走 `to_string`（声明序直出）。
pub fn probe_reach_json(report: &ReachReport) -> String {
    #[derive(serde::Serialize)]
    struct Wire<'a> {
        endpoints: &'a [String],
        ok: bool,
        peer: &'a str,
        results: &'a [ReachEntry],
    }
    serde_json::to_string(&Wire {
        endpoints: &report.endpoints,
        ok: true,
        peer: &report.peer,
        results: &report.results,
    })
    .expect("报告序列化不会失败")
}

/// 解析失败信封（探测编排里的 parse-error 出口）。
pub fn probe_reach_err(msg: &str) -> String {
    err_json(msg)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::token::{self, PeerId, Secret, TokenSpec};

    /// 现编一枚正例 token（peer/端点定值）：键面/取值/键序对照（成功向）。
    fn sample_token() -> String {
        let spec = TokenSpec {
            peer_id: &PeerId::from([7u8; 32]),
            secret: &Secret::from([9u8; 32]),
            endpoints: &[token::EndpointRef {
                addr: "192.168.3.12:41641",
                kind: EndpointKind::Direct,
            }],
            rpk: None,
        };
        token::encode(&spec).unwrap()
    }

    #[test]
    fn probe_addr_ok_shape() {
        let json = probe_addr_json(&format!("  {}  ", sample_token())); // TrimSpace 面
        let v: Value = serde_json::from_str(&json).unwrap();
        let keys: Vec<&str> = v.as_object().unwrap().keys().map(|k| k.as_str()).collect();
        assert_eq!(keys, vec!["endpoints", "ok", "peer"]);
        assert!(v["ok"].as_bool().unwrap());
        assert_eq!(v["peer"], "070707070707"); // 6 字节 hex
        assert_eq!(v["endpoints"][0], "192.168.3.12:41641");
    }

    /// relay 端点前缀呈现。
    #[test]
    fn probe_addr_relay_prefix() {
        let spec = TokenSpec {
            peer_id: &PeerId::from([1u8; 32]),
            secret: &Secret::from([2u8; 32]),
            endpoints: &[token::EndpointRef { addr: "r.example:41741", kind: EndpointKind::Relay }],
            rpk: None,
        };
        let raw = token::encode(&spec).unwrap();
        let v: Value = serde_json::from_str(&probe_addr_json(&raw)).unwrap();
        assert_eq!(v["endpoints"][0], "relay:r.example:41741");
    }

    #[test]
    fn probe_addr_err_shape() {
        let v: Value = serde_json::from_str(&probe_addr_json("hmw2-garbage")).unwrap();
        assert!(v.get("error").is_some() && v.get("ok").is_none());
    }

    /// results 字段序 = 声明序（Go struct 序，非字典序）。
    #[test]
    fn probe_reach_results_field_order() {
        let report = ReachReport {
            peer: "abc123".into(),
            endpoints: vec!["1.2.3.4:41641".into()],
            results: vec![ReachEntry {
                ep: "1.2.3.4:41641".into(),
                rtt_ms: 21,
                build: "homeway-rs-dev".into(),
                relay: false,
            }],
        };
        let json = probe_reach_json(&report);
        assert!(
            json.contains(
                "\"results\":[{\"ep\":\"1.2.3.4:41641\",\"rtt_ms\":21,\"build\":\"homeway-rs-dev\",\"relay\":false}]"
            ),
            "results 元素字段序 ep→rtt_ms→build→relay：{json}"
        );
    }
}
