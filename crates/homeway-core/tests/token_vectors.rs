//! Go ↔ Rust 对照测试：`fixtures/vectors/token.json`（Go 侧生产代码真源产出，见
//! `tools/gen-vectors.sh`）。逐字节断言——任一不一致即「没对齐」。

use homeway_core::token::{self, TokenError};
use serde::Deserialize;

#[derive(Deserialize)]
struct Vectors {
    cases: Vec<Case>,
    errors: Vec<ErrorCase>,
}

#[derive(Deserialize)]
struct Case {
    name: String,
    #[allow(dead_code)]
    input: RawInput,
    token: String,
    decoded: RawInput,
    body_b64: String,
    #[allow(dead_code)]
    body_hex: String,
    #[allow(dead_code)]
    crc_hex: String,
}

#[derive(Deserialize)]
struct RawInput {
    peer_id: String,
    secret: String,
    endpoints: Vec<RawEndpoint>,
}

#[derive(Deserialize)]
struct RawEndpoint {
    addr: String,
    relay: bool,
}

#[derive(Deserialize)]
struct ErrorCase {
    name: String,
    input: String,
    error: String,
}

fn vectors() -> Vectors {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fixtures/vectors/token.json"
    );
    let raw = std::fs::read_to_string(path).expect("向量文件应在（tools/gen-vectors.sh 生成）");
    serde_json::from_str(&raw).expect("向量 JSON 结构应可反序列化")
}

fn err_kind(err: &TokenError) -> &'static str {
    match err {
        TokenError::Corrupted => "corrupted",
        TokenError::UnsupportedVersion => "unsupported_version",
        TokenError::Malformed { .. } => "malformed",
        _ => "other",
    }
}

/// 正例：整串解析逐字段对齐 + 载荷借用面分步断言 + 再编码逐字节一致。
#[test]
fn go_vectors_decode_byte_exact() {
    let v = vectors();
    assert!(v.cases.len() >= 3, "正例向量应 ≥3 案");
    for c in &v.cases {
        let tok = token::decode(&c.token).unwrap_or_else(|e| panic!("{}: 解析失败 {e:?}", c.name));
        let want_peer = hex::decode(&c.decoded.peer_id).unwrap();
        let want_secret = hex::decode(&c.decoded.secret).unwrap();
        assert_eq!(
            tok.peer_id.as_bytes().as_slice(),
            want_peer.as_slice(),
            "{}: peer_id",
            c.name
        );
        assert_eq!(
            tok.secret.as_bytes().as_slice(),
            want_secret.as_slice(),
            "{}: secret",
            c.name
        );
        assert_eq!(
            tok.endpoints.len(),
            c.decoded.endpoints.len(),
            "{}: 端点数",
            c.name
        );
        for (got, want) in tok.endpoints.iter().zip(&c.decoded.endpoints) {
            assert_eq!(got.addr, want.addr, "{}: 端点地址", c.name);
            assert_eq!(got.relay, want.relay, "{}: 端点 relay 位", c.name);
        }

        // 载荷分步：body_b64 解码 → parse_body（借用零拷贝面）
        let engine = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        use base64::Engine;
        let raw = engine.decode(c.body_b64.as_bytes()).unwrap();
        let parsed = token::parse_body(&raw).unwrap();
        assert_eq!(parsed.peer_id().as_bytes(), tok.peer_id.as_bytes());
        assert_eq!(parsed.secret().as_bytes(), tok.secret.as_bytes());

        // 再编码：Go 串 ↔ Rust 编码逐字节一致
        let spec = token::TokenSpec {
            peer_id: &parsed.peer_id(),
            secret: &parsed.secret(),
            endpoints: parsed.endpoints(),
        };
        let re = token::encode(&spec).unwrap();
        assert_eq!(re, c.token.trim(), "{}: 再编码应与 Go 串逐字节一致", c.name);
    }
}

/// 负例：错误类别与 Go 三哨兵一一对应。
#[test]
fn go_vectors_error_taxonomy() {
    let v = vectors();
    for c in &v.errors {
        let err = token::decode(&c.input)
            .err()
            .unwrap_or_else(|| panic!("{}: 应报错却解析成功", c.name));
        assert_eq!(err_kind(&err), c.error, "{}: 错误类别", c.name);
    }
}
