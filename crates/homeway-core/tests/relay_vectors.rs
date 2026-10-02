//! R4 中继子协议向量对照：`fixtures/vectors/relay.json`
//! （向量由 tools/vector-gen 从 baseline 克隆的生产真源 `pkg/proto/{relay,relayctl}.go`
//! 产出——Hello/Challenge/Proof50 v2/OK17/SESSION27/RELEASE9/LEGUP37 + 腿帧封装 +
//! TCP 分帧边界 + MAC 四族定值）。

use homeway_core::relaywire as rw;
use homeway_core::wtransport::frame as wf;

fn load() -> serde_json::Value {
    let p = format!("{}/../../fixtures/vectors/relay.json", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
}

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

fn h32(v: &serde_json::Value, key: &str) -> [u8; 32] {
    unhex(v[key].as_str().unwrap()).try_into().unwrap()
}

fn h16(v: &serde_json::Value, key: &str) -> [u8; 16] {
    unhex(v[key].as_str().unwrap()).try_into().unwrap()
}

/// 子协议编码逐字节（对拍 Go 真源产出；含 MAC 族与 DH 输入的重算）。
#[test]
fn relay_wire_matches_go_vectors() {
    let v = load();
    // 材料从 cases 重取（hello 案例里嵌着 pubkey；其余按顶层字段）
    let nonce = h16(&v, "nonce");
    let cookie = h16(&v, "cookie");
    let secret = {
        // secret1 向量 = 0102..20（token.json 同源材料；本文件只带其派生 MAC——
        // pskMac/okMac/legupMac 直接比对，无需重导材料）
        [0u8; 32]
    };
    let _ = secret;

    // DH 输入重算：X25519(backendPriv, ephPub)（后端侧视角）。
    // hello/proof 案例里的 pubkey = vecPeerID1（0x11×32）——与 DH 私钥是两组材料。
    let backend_priv = x25519_dalek::StaticSecret::from(h32(&v, "backendPriv"));
    let eph_pub = x25519_dalek::PublicKey::from(h32(&v, "ephPub"));
    let pub_: [u8; 32] = {
        let hello_case = v["cases"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["name"] == "hello")
            .map(|c| unhex(c["wire"].as_str().unwrap()))
            .unwrap();
        hello_case[1..33].try_into().unwrap()
    };
    let dh = backend_priv.diffie_hellman(&eph_pub);

    for c in v["cases"].as_array().unwrap() {
        let name = c["name"].as_str().unwrap();
        let want = unhex(c["wire"].as_str().unwrap());
        let got: Vec<u8> = match name {
            "hello" => rw::encode_hello(&pub_),
            "challenge" => rw::encode_challenge(eph_pub.as_bytes(), &nonce),
            "proof-v2" => {
                let psk = rw::auth_mac(&h32_psk(&v), &nonce, &pub_);
                rw::encode_proof(&nonce, dh.as_bytes(), &pub_, Some(&psk))
            }
            "proof-open-psk-zero" => {
                rw::encode_proof(&nonce, dh.as_bytes(), &pub_, None)
            }
            "ok-udp" => rw::ok_bytes(),
            "again" => rw::again_bytes(),
            "keepalive" => rw::keepalive_bytes(),
            "ok-tcp17" => rw::encode_ok_auth(&rw::ok_auth_mac(&h32_psk(&v), &nonce)),
            "session27" => {
                rw::encode_session(&rw::CtlSession { id: 42, data_port: 51000, cookie })
            }
            "release9" => rw::encode_release(99),
            "legup37" => rw::legup_payload(7, &cookie, &h32_psk(&v)),
            "relay-reg-frame" => {
                let mut f = rw::relay_reg_frame(&rw::encode_hello(&pub_));
                // Go EncodeFrame = [0xBB][3][…]；Rust relay_reg_frame 同构，直接比对
                let _ = &mut f;
                rw::relay_reg_frame(&rw::encode_hello(&pub_))
            }
            "ctl-stream-keepalive+256" => {
                let mut wire = Vec::new();
                rw::ctl_frame_into(&rw::keepalive_bytes(), &mut wire);
                let mut pad256 = (0..256u16).map(|i| i as u8).collect::<Vec<u8>>();
                pad256[0] = rw::sub::KEEPALIVE;
                rw::ctl_frame_into(&pad256, &mut wire);
                wire
            }
            other => panic!("未知向量用例 {other}"),
        };
        assert_eq!(got, want, "用例 {name} 字节不符");
    }

    // MAC 四族定值
    let s = h32_psk(&v);
    assert_eq!(unhex(v["pskMac"].as_str().unwrap()), rw::auth_mac(&s, &nonce, &pub_).to_vec(), "pskMac");
    assert_eq!(unhex(v["okMac"].as_str().unwrap()), rw::ok_auth_mac(&s, &nonce).to_vec(), "okMac");
    assert_eq!(unhex(v["proofMac"].as_str().unwrap()), rw::proof_mac(dh.as_bytes(), &nonce, &pub_).to_vec(), "proofMac");
    assert_eq!(unhex(v["legupMac"].as_str().unwrap()), rw::legup_mac(7, &cookie, &s).to_vec(), "legupMac");

    // 分帧字节回读：半帧状态机吃回完整消息
    let wire_case = v["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "ctl-stream-keepalive+256")
        .map(|c| unhex(c["wire"].as_str().unwrap()))
        .unwrap();
    let mut dec = rw::CtlDecoder::new();
    let mut msgs = Vec::new();
    dec.feed(&wire_case, &mut msgs).unwrap();
    assert_eq!(msgs.len(), 2);
    assert_eq!(msgs[0], (rw::sub::KEEPALIVE, vec![]));
    assert_eq!(msgs[1].0, rw::sub::KEEPALIVE);
    assert_eq!(msgs[1].1.len(), 255);
    // pad256 = [0..255]（首字节换 KEEPALIVE）⇒ 载荷 = 1..=255
    assert_eq!(&msgs[1].1[..5], &[1u8, 2, 3, 4, 5][..]);
}

/// secret1 材料（vecgen 的 vecSecret1 = 0102…20）——psk/ok/legup 三族的密钥输入。
fn h32_psk(_v: &serde_json::Value) -> [u8; 32] {
    let mut k = [0u8; 32];
    for (i, b) in k.iter_mut().enumerate() {
        *b = (i + 1) as u8;
    }
    k
}

/// 腿帧封装形态（[0xBB][3]）的独立断言（relay-reg-frame 案例已含；此处钉 decode 侧）。
#[test]
fn relay_reg_frame_decode_shape() {
    let v = load();
    let case = v["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "relay-reg-frame")
        .map(|c| unhex(c["wire"].as_str().unwrap()))
        .unwrap();
    assert_eq!(&case[..2], &[0xBB, 3][..]);
    let (sub, body) = rw::decode_relay_reg_frame(&case).unwrap();
    assert_eq!(sub, rw::sub::HELLO);
    assert_eq!(body.len(), 32);
    // 非 type=3 帧不认
    assert!(rw::decode_relay_reg_frame(&wf::frame_bytes(wf::FrameKind::Data, b"x")).is_none());
}
