use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
mod server {
    pub mod doh {
        use std::net::IpAddr;
        // Both harness addresses are public; production doh test also explicitly
        // asserts ::ffff:<public-v4> is not poisoned.
        pub fn poisoned_ip(_: &IpAddr) -> bool { false }
    }
    pub mod engine {
        use super::super::*;
        struct Resolver;
        impl Resolver {
            fn resolve4(&self, _: &str) -> Option<Ipv4Addr> { panic!("literal-only case") }
            fn resolve6(&self, _: &str) -> Option<Ipv6Addr> { panic!("literal-only case") }
        }
        struct TokenCtx { resolver: Resolver, logf: fn(&str) }
struct StunCandidates<'a> {
    ctx: &'a TokenCtx,
    specs: std::str::Split<'a, char>,
    want_v6: bool,
}

impl<'a> StunCandidates<'a> {
    fn new(ctx: &'a TokenCtx, spec: &'a str, want_v6: bool) -> Self {
        Self { ctx, specs: spec.split(','), want_v6 }
    }
}

impl Iterator for StunCandidates<'_> {
    type Item = (String, SocketAddr);
    /// 惰性：调用方要一个才解析一个（r1#3——预解析全部候选会拖慢整轮）。
    fn next(&mut self) -> Option<Self::Item> {
        let ctx = self.ctx;
        let want_v6 = self.want_v6;
        for cand in self.specs.by_ref().map(str::trim).filter(|c| !c.is_empty()) {
            let Some((host, port)) =
                cand.rsplit_once(':').and_then(|(h, p)| p.parse::<u16>().ok().map(|p| (h, p)))
            else {
                (ctx.logf)(&format!("STUN：候选 {cand} 形态非法（应为 host:port）——跳过"));
                continue;
            };
            let host = host.trim_matches(|c| c == '[' || c == ']');
            if let Ok(ip) = host.parse::<IpAddr>() {
                if ip.is_ipv6() == want_v6 && !super::doh::poisoned_ip(&ip) {
                    return Some((cand.to_owned(), SocketAddr::new(ip, port)));
                }
                continue;
            }
            let ip = if want_v6 {
                ctx.resolver.resolve6(host).map(IpAddr::V6)
            } else {
                ctx.resolver.resolve4(host).map(IpAddr::V4)
            };
            match ip {
                Some(ip) => return Some((cand.to_owned(), SocketAddr::new(ip, port))),
                None => (ctx.logf)(&format!(
                    "STUN：{cand} 解析失败（{}）——跳过",
                    if want_v6 { "无可用 AAAA / DoH 候选已尽 / 应答落污染段" } else { "无可用 A / DoH 候选已尽 / 应答落污染段" }
                )),
            }
        }
        None
    }
}

        pub fn run() {
            let ctx = TokenCtx { resolver: Resolver, logf: |_| {} };
            let mut candidates = StunCandidates::new(&ctx,
                "[::ffff:8.8.8.8]:3478,[2001:4860:4860::8888]:3478", true);
            let first = candidates.next().unwrap();
            assert!(matches!(first.1.ip(), IpAddr::V6(v) if v.to_ipv4_mapped().is_some()));
            // Reproduce the production STUN6 success branch with a scripted valid
            // v4 mapped-address response. It accepts and breaks before second.
            let response: SocketAddr = "8.8.4.4:41641".parse().unwrap();
            let chosen = Some(response); // production engine.rs:2213-2215
            let global_v6 = matches!(chosen.unwrap().ip(), IpAddr::V6(v)
                if v.to_ipv4_mapped().is_none() && (v.segments()[0] & 0xffc0) != 0xfe80);
            assert!(!global_v6);
            let second = candidates.next().unwrap();
            assert_eq!(second.1.to_string(), "[2001:4860:4860::8888]:3478");
            println!("production iterator selected mapped-v4 for want_v6=true: {}", first.1);
            println!("clean v4 STUN response is accepted then rejected only after break; untried real-v6 backup: {}", second.1);
        }
    }
}
fn main() { server::engine::run(); }
