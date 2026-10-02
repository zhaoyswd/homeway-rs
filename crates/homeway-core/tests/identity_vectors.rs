//! Go ↔ Rust 对照测试：`fixtures/vectors/{identity,tunnel_addr,psk}.json`（Go 侧生产
//! 代码真源产出，见 `tools/gen-vectors.sh`）。逐字节断言——任一不一致即「没对齐」。
//!
//! identity 族经**store 全路径**复算：把向量的 master.key 落盘到临时目录，
//! `load_or_create` 应产出与 Go `LoadOrCreateIdentity` 一致的私钥/公钥
//! （Go 向量生成时同路径交叉验证；devtag 文件不落盘 ⇒ 标签走创建随机路径，
//! 不在字节断言面内——TagDerived 派生路径由 `identity::tests` 内联向量钉住）。

use homeway_core::identity;
use homeway_core::psk::Psk;
use homeway_core::token::{PeerId, Secret};
use homeway_core::tunnel_addr;
use serde::Deserialize;

fn vectors_path(name: &str) -> std::path::PathBuf {
    let mut p = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("../../fixtures/vectors");
    p.push(name);
    p
}

fn h32(s: &str) -> [u8; 32] {
    let mut v = [0u8; 32];
    for (i, pair) in s.as_bytes().chunks(2).enumerate() {
        v[i] = u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap();
    }
    v
}

fn to_hex(b: &[u8]) -> String {
    b.iter().map(|v| format!("{v:02x}")).collect()
}

fn temp_dir(tag: &str) -> std::path::PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .subsec_nanos();
    let dir = std::env::temp_dir().join(format!(
        "homeway-rs-vec-{tag}-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

// ---- identity 族 ----

#[derive(Deserialize)]
struct IdentityVectors {
    cases: Vec<IdentityCase>,
}

#[derive(Deserialize)]
struct IdentityCase {
    name: String,
    master: String,
    peer_id: String,
    private_key: String,
    public_key: String,
}

#[test]
fn identity_derivation_matches_go_via_store_path() {
    let vs: IdentityVectors =
        serde_json::from_reader(std::fs::File::open(vectors_path("identity.json")).unwrap())
            .unwrap();
    assert!(vs.cases.len() >= 4, "向量集不完整");
    for c in &vs.cases {
        let dir = temp_dir("identity");
        std::fs::write(dir.join("master.key"), h32(&c.master)).unwrap();
        let (id, src, warn) =
            identity::load_or_create(Some(&dir), &PeerId::from(h32(&c.peer_id))).unwrap();
        assert_eq!(src, identity::IdentitySource::Reused, "{}", c.name);
        assert!(warn.is_none(), "{}", c.name);
        assert_eq!(to_hex(&id.private_key_bytes()), c.private_key, "{}", c.name);
        assert_eq!(to_hex(&id.public_key()), c.public_key, "{}", c.name);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

// ---- tunnel_addr 族 ----

#[derive(Deserialize)]
struct AddrVectors {
    cases: Vec<AddrCase>,
}

#[derive(Deserialize)]
struct AddrCase {
    name: String,
    secret: String,
    pubkey: String,
    tunnel_ip: String,
    tun_ip: String,
}

#[test]
fn tunnel_addr_matches_go() {
    let vs: AddrVectors =
        serde_json::from_reader(std::fs::File::open(vectors_path("tunnel_addr.json")).unwrap())
            .unwrap();
    assert!(vs.cases.len() >= 3, "向量集应含守卫命中样本");
    for c in &vs.cases {
        let sec = Secret::from(h32(&c.secret));
        let pub_ = h32(&c.pubkey);
        assert_eq!(
            tunnel_addr::derive_tunnel_ip(&sec, &pub_).to_string(),
            c.tunnel_ip,
            "{}",
            c.name
        );
        assert_eq!(
            tunnel_addr::derive_tun_ip(&sec, &pub_).to_string(),
            c.tun_ip,
            "{}",
            c.name
        );
    }
}

// ---- psk 族 ----

#[derive(Deserialize)]
struct PskVectors {
    cases: Vec<PskCase>,
}

#[derive(Deserialize)]
struct PskCase {
    name: String,
    secret: String,
    psk: String,
}

#[test]
fn psk_matches_go() {
    let vs: PskVectors =
        serde_json::from_reader(std::fs::File::open(vectors_path("psk.json")).unwrap()).unwrap();
    assert!(!vs.cases.is_empty());
    for c in &vs.cases {
        assert_eq!(
            to_hex(Psk::from(Secret::from(h32(&c.secret))).as_bytes()),
            c.psk,
            "{}",
            c.name
        );
    }
}
