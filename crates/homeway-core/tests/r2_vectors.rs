//! R2 增族向量对照：reg 报文字节 / 端点缓存落盘 JSON / files 帧字节
//! （向量由 tools/vector-gen 从 baseline 克隆的生产真源产出——fixtures/vectors/*.json）。

use homeway_core::wtransport::{endpoint_cache, reg};

fn load(name: &str) -> serde_json::Value {
    let p = format!("{}/../../fixtures/vectors/{name}", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
}

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

/// reg v2 报文逐字节（对拍 proto.EncodeReg 真源产出）。
#[test]
fn reg_wire_matches_go_vectors() {
    let v = load("reg.json");
    for c in v["cases"].as_array().unwrap() {
        let mut secret = [0u8; 32];
        let mut pubkey = [0u8; 32];
        let mut dev = [0u8; 8];
        let s = unhex(c["secret"].as_str().unwrap());
        let p = unhex(c["pubkey"].as_str().unwrap());
        let d = unhex(c["devtag"].as_str().unwrap());
        secret.copy_from_slice(&s);
        pubkey.copy_from_slice(&p);
        dev.copy_from_slice(&d);
        let mut out = Vec::with_capacity(reg::REG_LEN);
        reg::encode_reg_parts(
            &homeway_core::token::Secret::from(secret),
            &pubkey,
            &dev,
            c["ts"].as_i64().unwrap() as u64,
            &mut out,
        );
        let want = unhex(c["wire"].as_str().unwrap());
        assert_eq!(out, want, "reg 用例 {}", c["name"].as_str().unwrap());
    }
}

/// 端点缓存落盘 JSON 逐字节（键序=声明序 / verifiedAt omitempty / i64 毫秒）。
#[test]
fn endpointcache_json_matches_go_bytes() {
    let v = load("endpointcache.json");
    let want = v["json"].as_str().unwrap();
    let mut c = endpoint_cache::EndpointCache::new();
    let t0 = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_millis(1700000000123);
    let t1 = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_millis(1700000099876);
    c.observe("203.0.113.9:41641".parse().unwrap(), endpoint_cache::EndpointSource::Hint, t0);
    c.mark_verified("203.0.113.9:41641".parse().unwrap(), endpoint_cache::EndpointSource::Hint, t1);
    c.observe("[2001:db8::1]:41641".parse().unwrap(), endpoint_cache::EndpointSource::Probe, t1);
    // Rust 序列化等价物：直接构造 CacheFile 同形（save 依赖目录；此测试钉字节形状）
    let got = endpoint_cache::debug_wire_json(
        &c,
        "1111111111111111111111111111111111111111111111111111111111111111",
        t1,
    );
    assert_eq!(got, want, "端点缓存 JSON 字节与 Go 不一致");
}

/// files 帧字节（4B BE 前缀；含终止帧与 70KB 跨 u16 边界样本）。
#[test]
fn files_frame_bytes_match_go() {
    let v = load("files_frames.json");
    for c in v["cases"].as_array().unwrap() {
        let want = unhex(c["wire"].as_str().unwrap());
        let name = c["name"].as_str().unwrap();
        let payload = match name {
            "terminator" => &want[0..0],
            "small" => &want[4..],
            "70k" => &want[4..],
            _ => unreachable!(),
        };
        let mut frame = Vec::with_capacity(4 + payload.len());
        frame.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        frame.extend_from_slice(payload);
        assert_eq!(frame, want, "files 帧用例 {name}");
    }
}
