//! 端口转发整表热替换面（语义真源
//! `baseline:clientcore/cmd/clientcore/app_portfwd.go` + `pkg/portfwd`）。
//!
//! 三消费方共一条路：attach 首启（tunConfig）/ 运行中热替换（NAPI）/ 世代收工（nil 表）。
//! NAPI 面（`tunSetPortForwardsJSON`）的返回码契约：
//! `0` 已应用 / `-1` 当前没有**已接管数据面**的世代（改动随下次连接的 tunConfig 自然
//! 生效，可稍后重试）/ `-2` JSON 非法或校验不过（listen=0、targetPort=0、listen 值域外、
//! 目标非 IPv4 字面量、同表内 listen 重复）。
//!
//! 目标语义（`pfTargetText`——**主机**措辞是 NAPI 面口径，与 pkg/portfwd.DescribeTarget
//! 的「出口自己」是两处文案）：targetIp 空 ⇒ 拨出口自己；port 0 ⇒ 同监听端口。

use serde::Deserialize;

use crate::PortfwdErr;

/// 监听端口值域下限（`pkg/portfwd` 同源）。
pub const MIN_PORT: u16 = 1024;

/// 一条映射（tunConfig.portForwards 元素 / NAPI 热替换单元的同形状）。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct PortForwardRule {
    pub listen: u16,
    #[serde(rename = "targetIp", default)]
    pub target_ip: String,
    #[serde(rename = "targetPort", default)]
    pub target_port: u16,
}

/// 整表校验错误（全部折为 NAPI `-2`；类型化以便测试断言归因）。
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum TableErr {
    #[error("非法 JSON：{0}")]
    BadJson(String),
    #[error("listen/targetPort 为 0（listen={listen} targetPort={target_port}）")]
    ZeroPort { listen: u16, target_port: u16 },
    #[error("监听端口 {0} 不在 1024–65535")]
    ListenRange(u16),
    #[error("目标地址须为空（出口自己）或 IPv4 字面量：{0:?}")]
    BadTarget(String),
    #[error("同表内监听端口重复：{0}")]
    DupListen(u16),
}

/// 整表校验（NAPI 热替换前置；Go tunSetPortForwardsJSON 的循环体逐条对齐）。
pub fn validate_table(rules: &[PortForwardRule]) -> Result<(), TableErr> {
    let mut seen = std::collections::BTreeSet::new();
    for f in rules {
        if f.listen == 0 || f.target_port == 0 {
            return Err(TableErr::ZeroPort { listen: f.listen, target_port: f.target_port });
        }
        // 值域与目标语义走共享面：App 与桌面同一条规则同一个答案（FIX-43）
        if !(MIN_PORT..=u16::MAX).contains(&f.listen) {
            return Err(TableErr::ListenRange(f.listen));
        }
        if !f.target_ip.is_empty() && f.target_ip.parse::<std::net::Ipv4Addr>().is_err() {
            return Err(TableErr::BadTarget(f.target_ip.clone()));
        }
        if !seen.insert(f.listen) {
            // 同表内重复监听端口：第二条注定 EADDRINUSE，界面会显示一条莫名其妙的
            // 「失败」（表单本来拦得住，这里兜住被绕过/损坏的 JSON）。
            return Err(TableErr::DupListen(f.listen));
        }
    }
    Ok(())
}

/// 目标呈现文案（NAPI 面口径；port 0 = 同监听端口——语义真源 facade.DescribeTarget/FIX-46，
/// 措辞按 pfTargetText 的「主机」）。
pub fn pf_target_text(f: &PortForwardRule) -> String {
    if f.target_ip.is_empty() {
        if f.target_port == 0 {
            return "主机（同端口）".to_owned();
        }
        return format!("主机:{}", f.target_port);
    }
    let port = if f.target_port == 0 { f.listen } else { f.target_port };
    format!("{}:{}", f.target_ip, port)
}

/// 拨号目标（target_ip 空 = 出口自己〔拨出口本机端口〕；否则经出口拨任意目标）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PfDialTarget {
    /// 拨出口自己的端口。
    ExitPort(u16),
    /// 经出口拨任意可达目标。
    Remote(std::net::SocketAddrV4),
}

impl PortForwardRule {
    pub fn dial_target(&self) -> Result<PfDialTarget, TableErr> {
        if self.target_ip.is_empty() {
            return Ok(PfDialTarget::ExitPort(self.target_port));
        }
        let ip: std::net::Ipv4Addr = self
            .target_ip
            .parse()
            .map_err(|_| TableErr::BadTarget(self.target_ip.clone()))?;
        let port = if self.target_port == 0 { self.listen } else { self.target_port };
        Ok(PfDialTarget::Remote(std::net::SocketAddrV4::new(ip, port)))
    }
}

/// 一条映射的运行态（pfState；tunStatusJSON.portForwards[] 的数据源）。
/// Clone/Eq 不含 conns（原子计数非快照面——比较走 `snapshot()`）。
#[derive(Debug)]
pub struct PfState {
    pub listen: u16,
    pub target: String,
    /// listening | failed。
    pub state: &'static str,
    pub err: String,
    /// 失败映射的稳定错误码（portfwd/err 词表；成功/listening 为空串）。
    pub code: &'static str,
    /// 当前活跃转发连接数（accept 成功 +1、连接结束 -1）。
    pub conns: std::sync::atomic::AtomicI64,
}

impl PfState {
    /// 成功监听态。
    pub fn listening(listen: u16, target: String) -> Self {
        PfState {
            listen,
            target,
            state: "listening",
            err: String::new(),
            code: "",
            conns: std::sync::atomic::AtomicI64::new(0),
        }
    }

    /// 监听失败态（单条失败只记状态、不阻断隧道——软失败不回滚哲学）。
    pub fn failed(listen: u16, target: String, err: String) -> Self {
        PfState {
            listen,
            target,
            state: "failed",
            err,
            code: PortfwdErr::BindFailed.as_str(),
            conns: std::sync::atomic::AtomicI64::new(0),
        }
    }

    /// tunStatusJSON 的 portForwards 元素（快照面；conns 现读现给）。
    pub fn snapshot(&self) -> super::tun_status::PfStateIn {
        super::tun_status::PfStateIn {
            listen: self.listen,
            target: self.target.clone(),
            state: self.state.to_owned(),
            err: self.err.clone(),
            code: self.code.to_owned(),
            conns: self.conns.load(std::sync::atomic::Ordering::Relaxed),
        }
    }
}

/// `{"portForwards":[…]}` 的信封解析。
pub fn parse_rules_json(cfg: &str) -> Result<Vec<PortForwardRule>, TableErr> {
    #[derive(Deserialize)]
    struct Envelope {
        #[serde(rename = "portForwards", default)]
        port_forwards: Vec<PortForwardRule>,
    }
    let v: Envelope =
        serde_json::from_str(cfg).map_err(|e| TableErr::BadJson(e.to_string()))?;
    Ok(v.port_forwards)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(listen: u16, ip: &str, port: u16) -> PortForwardRule {
        PortForwardRule { listen, target_ip: ip.to_owned(), target_port: port }
    }

    /// NAPI 整表校验的五条拒绝 + 通过形态（Go 循环体逐条对齐）。
    #[test]
    fn validate_table_rejections() {
        assert_eq!(validate_table(&[rule(0, "", 80)]), Err(TableErr::ZeroPort { listen: 0, target_port: 80 }));
        assert_eq!(validate_table(&[rule(18080, "", 0)]), Err(TableErr::ZeroPort { listen: 18080, target_port: 0 }));
        assert_eq!(validate_table(&[rule(80, "", 8080)]), Err(TableErr::ListenRange(80)));
        assert_eq!(
            validate_table(&[rule(18080, "fe80::1", 80)]),
            Err(TableErr::BadTarget("fe80::1".into()))
        );
        assert_eq!(
            validate_table(&[rule(18080, "example.com", 80)]),
            Err(TableErr::BadTarget("example.com".into()))
        );
        assert_eq!(
            validate_table(&[rule(18080, "", 80), rule(18080, "", 81)]),
            Err(TableErr::DupListen(18080))
        );
        // 通过：出口自己 / IPv4 字面量 / 目标 <1024 合法（拨号不 bind——spec 只约束监听端口）
        assert!(validate_table(&[rule(18080, "", 22)]).is_ok());
        assert!(validate_table(&[rule(18080, "10.1.2.3", 80)]).is_ok());
        assert!(validate_table(&[]).is_ok());
    }

    /// 目标文案三形态（FIX-46：port 0 不再原样呈现）。
    #[test]
    fn target_text_forms() {
        assert_eq!(pf_target_text(&rule(18080, "", 0)), "主机（同端口）");
        assert_eq!(pf_target_text(&rule(18080, "", 8080)), "主机:8080");
        assert_eq!(pf_target_text(&rule(18080, "1.2.3.4", 0)), "1.2.3.4:18080");
        assert_eq!(pf_target_text(&rule(18080, "1.2.3.4", 9999)), "1.2.3.4:9999");
    }

    /// 拨号目标派生（空 IP = 出口自己；port 0 落成 listen）。
    #[test]
    fn dial_target_derivation() {
        assert_eq!(rule(18080, "", 8080).dial_target().unwrap(), PfDialTarget::ExitPort(8080));
        assert_eq!(
            rule(18080, "1.2.3.4", 0).dial_target().unwrap(),
            PfDialTarget::Remote("1.2.3.4:18080".parse().unwrap())
        );
    }

    /// 失败态的 code = bind_failed（portfwd/err 词面）；成功态空串。
    #[test]
    fn state_codes() {
        let ok = PfState::listening(18080, "主机:8080".into());
        assert_eq!(ok.snapshot().code, "");
        assert_eq!(ok.snapshot().state, "listening");
        let fail = PfState::failed(18081, "主机:80".into(), "bind: address in use".into());
        let snap = fail.snapshot();
        assert_eq!(snap.code, "bind_failed");
        assert_eq!(snap.state, "failed");
        assert_eq!(snap.err, "bind: address in use");
    }
}
