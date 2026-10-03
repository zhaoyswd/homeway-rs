//! R5 增族向量对照（M20）：STUN/SPED golden——Go 生产真源产出
//! （StunRequest/ParseStunResponse/WriteControl/ReadFrameLoose），Rust 侧解析面
//! 对拍（fixtures/vectors/stun_sped.json）。

fn load(name: &str) -> serde_json::Value {
    let p = format!("{}/../../fixtures/vectors/{name}", env!("CARGO_MANIFEST_DIR"));
    match std::fs::read_to_string(&p) {
        Ok(s) => serde_json::from_str(&s).unwrap(),
        Err(e) => {
            panic!("读取 {p} 失败：{e}（cwd={:?}）", std::env::current_dir());
        }
    }
}

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

/// STUN 请求生成侧逐字节（Rust `stun_request` vs Go `StunRequest`——四 padding 边界）。
#[test]
fn stun_request_matches_go() {
    let v = load("stun_sped.json");
    for c in v["stun"]["requests"].as_array().unwrap() {
        let mut tx = [0u8; 12];
        tx.copy_from_slice(&unhex(c["txid"].as_str().unwrap()));
        let got = homeway_core::server::egress::stun_request(&tx, c["software"].as_str().unwrap());
        let want = unhex(c["wire"].as_str().unwrap());
        assert_eq!(got, want, "请求形态 {} 不一致", c["name"].as_str().unwrap());
    }
}

/// STUN 应答解析侧（Rust `parse_stun_response` 吃 Go 手搓应答样本——XOR/plain/
/// 双属性取 XOR/截断报错/第二 txID）。
#[test]
fn stun_response_parse_matches_go() {
    let v = load("stun_sped.json");
    for c in v["stun"]["responses"].as_array().unwrap() {
        let wire = unhex(c["wire"].as_str().unwrap());
        let name = c["name"].as_str().unwrap();
        match homeway_core::server::egress::parse_stun_response(&wire) {
            Some((tx, mapped)) => {
                assert!(
                    c["want_err"].is_null(),
                    "{name}：Go 报错但 Rust 解析成功"
                );
                let mut want_tx = [0u8; 12];
                want_tx.copy_from_slice(&unhex(c["txid"].as_str().unwrap()));
                assert_eq!(tx, want_tx, "{name}：txID 不一致");
                assert_eq!(
                    mapped.to_string(),
                    c["mapped"].as_str().unwrap(),
                    "{name}：映射地址不一致"
                );
            }
            None => {
                assert!(
                    !c["want_err"].is_null(),
                    "{name}：Go 解析成功但 Rust 报错（mapped={:?}）",
                    c["mapped"]
                );
            }
        }
    }
}

/// SPED 控制帧 + data 帧解析（Rust `decode_frame` 吃 Go 真源字节——类型/seq/len 逐字段；
/// data 帧全零载荷 crc 同算法）。
#[test]
fn sped_frames_decode_go_wire() {
    let v = load("stun_sped.json");
    for c in v["sped"]["controls"].as_array().unwrap() {
        let wire = unhex(c["wire"].as_str().unwrap());
        let (head, payload) = homeway_core::speedtest::decode_frame(&wire)
            .expect("控制帧应完整")
            .expect("帧应到齐");
        assert_eq!(head.typ, c["frame_type"].as_u64().unwrap() as u8, "{}", c["name"]);
        assert_eq!(head.seq, 0, "控制帧 seq 恒 0");
        assert_eq!(payload, c["payload"].as_str().unwrap().as_bytes(), "{}", c["name"]);
    }
    for c in v["sped"]["datas"].as_array().unwrap() {
        let wire = unhex(c["wire"].as_str().unwrap());
        let (head, payload) = homeway_core::speedtest::decode_frame(&wire)
            .expect("data 帧应完整")
            .expect("帧应到齐");
        assert_eq!(head.typ, 4, "{}：data 类型", c["name"]);
        assert_eq!(head.seq as u64, c["seq"].as_u64().unwrap(), "{}", c["name"]);
        assert_eq!(head.len, c["len"].as_u64().unwrap() as usize, "{}", c["name"]);
        assert!(payload.iter().all(|&b| b == 0), "{}：data 载荷全零", c["name"]);
    }
}

/// 生成侧往返自洽（Rust 构造的帧经自家解析器回读一致——fuzz 往返断言的向量锚）。
#[test]
fn sped_frame_roundtrip_stable() {
    // 手搓最小 control 帧（与 Frame::control 同构的字节形态）
    let payload = br#"{"role":"send","warmup_ms":2000,"window_ms":10000}"#;
    let mut wire = Vec::with_capacity(15 + payload.len());
    wire.extend_from_slice(b"SPED");
    wire.push(1); // TypeRequest
    wire.extend_from_slice(&0u32.to_le_bytes());
    wire.extend_from_slice(&(payload.len() as u16).to_le_bytes());
    let crc = crc32_ieee_local(payload);
    wire.extend_from_slice(&crc.to_le_bytes());
    wire.extend_from_slice(payload);
    let (head, back) = homeway_core::speedtest::decode_frame(&wire)
        .expect("完整")
        .expect("到齐");
    assert_eq!((head.typ, head.seq, head.len), (1, 0, payload.len()));
    assert_eq!(back, payload);
}

fn crc32_ieee_local(data: &[u8]) -> u32 {
    // IEEE crc32——**同源副本**（与 core 内 speedtest.rs 的 crc32_ieee 是同一算法的
    // 逐行重写，非真正独立实现；编码器私有 ⇒ 本轨是 decode-only 往返，抓不到
    // 「编码器与解码器口径互不一致」形态——R5 第二道门 低-17 注记）
    let mut crc = !0u32;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
        }
    }
    !crc
}
