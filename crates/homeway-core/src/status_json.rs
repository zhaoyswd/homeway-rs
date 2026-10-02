//! 状态 JSON 契约（语义真源 `baseline:clientcore/cmd/clientcore/app_service.go::serviceSnapshotJSON`）。
//!
//! 形状逐键逐缺省对齐：state/reason 恒有；elapsedMs 随 Since；bridge* 随桥（CLI 形态
//! 无桥 = 恒缺省）；link 在时 identity 嵌其内出现；stats 随快照指针（`if snap.Stats != nil`，
//! 不预设必在）。键序 = Go `json.Marshal(map)` 的字典序——serde_json 的 `Map` 默认
//! BTreeMap 同序（对照测试**断言键序**，不只键集合；R2 评审低-18）。
//!
//! 隧道形态的 tunStatusJSON（meowed/readyBy/portForwards/demand/exitIp/tunIp…）需要
//! TUN 面，归 R7（facade.rs 登记契约）。

use serde_json::{Map, Value};

use crate::session::SessionSnapshot;

/// 无实例形态（Go `serviceCur == nil` 短路——逐字节）。
pub fn idle_json() -> &'static str {
    "{\"state\":\"idle\"}"
}

/// 快照 → 状态 JSON（服务形态；Go serviceSnapshotJSON 逐键对齐）。
pub fn snapshot_json(s: &SessionSnapshot) -> String {
    let mut m = Map::new();
    m.insert("state".into(), Value::String(s.state.as_str().into()));
    m.insert("reason".into(), Value::String(s.reason.clone()));
    m.insert(
        "elapsedMs".into(),
        Value::from(s.since.elapsed().as_millis() as u64),
    );
    if let Some(link) = &s.link {
        let mut l = Map::new();
        l.insert("via".into(), Value::String(link.via.clone()));
        l.insert("ep".into(), Value::String(link.ep.clone()));
        l.insert("at".into(), Value::from(link.at_ms));
        l.insert("rttMs".into(), Value::from(link.rtt_ms));
        m.insert("link".into(), Value::Object(l));
        // Go：identity 嵌在 `if snap.Link != nil` 内
        if let Some((dev, pubk)) = &s.identity {
            let mut i = Map::new();
            i.insert("dev".into(), Value::String(dev.clone()));
            i.insert("pub".into(), Value::String(pubk.clone()));
            m.insert("identity".into(), Value::Object(i));
        }
    }
    if let Some((rx, tx)) = s.stats {
        let mut st = Map::new();
        st.insert("rxBytes".into(), Value::from(rx));
        st.insert("txBytes".into(), Value::from(tx));
        m.insert("stats".into(), Value::Object(st));
    }
    Value::Object(m).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{LinkSnapshot, SessState};
    use std::time::Instant;

    /// 键序 = 字典序（Go json.Marshal(map) 同序）；identity 随 link 出现。
    #[test]
    fn key_order_and_conditional_presence() {
        let s = SessionSnapshot {
            state: SessState::Ready,
            reason: String::new(),
            since: Instant::now(),
            link: Some(LinkSnapshot {
                via: "direct".into(),
                ep: "127.0.0.1:42641".into(),
                rtt_ms: 0,
                at_ms: 1_700_000_000_000,
            }),
            identity: Some(("dfc1b0e2".into(), "60115c03".into())),
            stats: Some((1024, 2048)),
        };
        let json = snapshot_json(&s);
        // 键序断言：elapsedMs < identity < link < reason < state < stats
        let keys: Vec<&str> = ["elapsedMs", "identity", "link", "reason", "state", "stats"].to_vec();
        let mut last = 0usize;
        for k in keys {
            let pos = json.find(&format!("\"{k}\":")).expect(k);
            assert!(pos > last, "键序非字典序：{json}");
            last = pos;
        }
        assert!(json.contains("\"link\":{\"at\":1700000000000,\"ep\":\"127.0.0.1:42641\",\"rttMs\":0,\"via\":\"direct\"}"));
        assert!(json.contains("\"stats\":{\"rxBytes\":1024,\"txBytes\":2048}"));
    }

    /// 无 link ⇒ identity/stats 缺省面（stats 随快照指针）。
    #[test]
    fn no_link_means_no_identity() {
        let s = SessionSnapshot {
            state: SessState::Starting,
            reason: String::new(),
            since: Instant::now(),
            link: None,
            identity: Some(("a".into(), "b".into())),
            stats: None,
        };
        let json = snapshot_json(&s);
        assert!(!json.contains("identity"));
        assert!(!json.contains("stats"));
        assert!(!json.contains("link"));
    }

    #[test]
    fn idle_literal() {
        assert_eq!(idle_json(), "{\"state\":\"idle\"}");
    }
}
