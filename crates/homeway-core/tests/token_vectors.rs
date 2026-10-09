//! **本仓自产**对照向量：`fixtures/vectors/token_hmw2.json`（M5 S5t 起）。
//!
//! 沿革：旧 `fixtures/vectors/token.json` 是 **Go 冻结向量**（基线 `d4148f6` 的
//! `pkg/proto/token.go` 产出，逐字节 oracle）。M5 S5t 换 `hmw2` 段容器 ⇒ 该向量
//! **退役为历史参照**（「无兼容包袱」口径；设计 §5.2 的连锁 2），本文件改为**自产**：
//! 向量由 [`bless_token_hmw2_vectors`]（`#[ignore]`）从 `token::encode` 生成，普通用例
//! [`token_hmw2_vectors_roundtrip_and_byte_anchor`] 断言「编码器 reproduces 该文件的每个
//! 串 + 解析逐字段对齐 + 负例哨兵码」——**自产不等于无锚**：锚 = 已入库的文件字节，任何
//! 编码器改动都会让它变红（改了就同批重跑 bless 与登记）。
//!
//! 重生成：`cargo test -p homeway-core --test token_vectors bless_token_hmw2_vectors -- --ignored --nocapture`
//! （写回 `fixtures/vectors/token_hmw2.json`；换文件后须同批 `fixtures/SHA256SUMS` + `MANIFEST.md`）。

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use homeway_core::token::{self, EndpointKind, TokenError};
use serde::{Deserialize, Serialize};

#[derive(Deserialize, Serialize)]
struct Vectors {
    /// 向量来源标记（自产 / 生成器）——仅供人读。
    generated_by: String,
    sentinels: Sentinels,
    cases: Vec<Case>,
    errors: Vec<ErrorCase>,
}

#[derive(Deserialize, Serialize)]
struct Sentinels {
    corrupted: String,
    unsupported_version: String,
    unsupported_wrapped: String,
    malformed: String,
    malformed_no_prefix: String,
}

#[derive(Deserialize, Serialize)]
struct Case {
    name: String,
    input: RawInput,
    token: String,
    decoded: RawInput,
    body_b64: String,
    body_hex: String,
    crc_hex: String,
}

#[derive(Deserialize, Serialize, Clone)]
struct RawInput {
    peer_id: String,
    secret: String,
    endpoints: Vec<RawEndpoint>,
    /// 出口 RPK 裸公钥（32B hex；空串 = 不带 `rpk` 段）。
    #[serde(default)]
    rpk: String,
}

#[derive(Deserialize, Serialize, Clone)]
struct RawEndpoint {
    addr: String,
    /// 0=direct 1=relay 2=quic（wire 值）。
    kind: u8,
}

#[derive(Deserialize, Serialize)]
struct ErrorCase {
    name: String,
    input: String,
    error: String,
}

fn vectors_path() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/vectors/token_hmw2.json")
}

fn vectors() -> Vectors {
    let raw = std::fs::read_to_string(vectors_path())
        .expect("向量文件应在（crates/homeway-core/tests/token_vectors.rs 的 bless 用例生成）");
    serde_json::from_str(&raw).expect("向量 JSON 结构应可反序列化")
}

fn err_kind(err: &TokenError) -> &'static str {
    match err {
        TokenError::Corrupted => "corrupted",
        TokenError::UnsupportedVersion { .. } => "unsupported_version",
        TokenError::Malformed { .. } => "malformed",
        _ => "other",
    }
}

fn hex_str(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn hex_bytes(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

type InputParts = (
    token::PeerId,
    token::Secret,
    Vec<token::EndpointRef<'static>>,
    Option<token::RpkPubKey>,
);

fn parse_input(raw: &RawInput) -> InputParts {
    let peer = token::PeerId::from(<[u8; 32]>::try_from(hex_bytes(&raw.peer_id)).unwrap());
    let secret = token::Secret::from(<[u8; 32]>::try_from(hex_bytes(&raw.secret)).unwrap());
    // 借用面：向量条目字符串是 host:port 结构合法的常量串，泄漏成 'static 只服务本用例
    let eps: Vec<token::EndpointRef<'static>> = raw
        .endpoints
        .iter()
        .map(|e| {
            let addr: &'static str = Box::leak(e.addr.clone().into_boxed_str());
            token::EndpointRef::new(addr, EndpointKind::from_wire(e.kind))
        })
        .collect();
    let rpk = if raw.rpk.is_empty() {
        None
    } else {
        Some(token::RpkPubKey::from(
            <[u8; 32]>::try_from(hex_bytes(&raw.rpk)).unwrap(),
        ))
    };
    (peer, secret, eps, rpk)
}

/// 正例：整串解析逐字段对齐 + 载荷借用面分步断言 + **再编码逐字节等于向量串**。
#[test]
fn token_hmw2_vectors_roundtrip_and_byte_anchor() {
    let v = vectors();
    assert!(v.cases.len() >= 3, "正例向量应 ≥3 案");
    for c in &v.cases {
        let tok = token::decode(&c.token).unwrap_or_else(|e| panic!("{}: 解析失败 {e:?}", c.name));
        assert_eq!(hex_str(tok.peer_id.as_bytes()), c.decoded.peer_id, "{}: peer_id", c.name);
        assert_eq!(hex_str(tok.secret.as_bytes()), c.decoded.secret, "{}: secret", c.name);
        assert_eq!(
            tok.endpoints.len(),
            c.decoded.endpoints.len(),
            "{}: 端点数",
            c.name
        );
        for (got, want) in tok.endpoints.iter().zip(&c.decoded.endpoints) {
            assert_eq!(got.addr, want.addr, "{}: 端点地址", c.name);
            assert_eq!(got.kind, EndpointKind::from_wire(want.kind), "{}: 端点类别", c.name);
        }
        assert_eq!(
            tok.rpk.as_ref().map(|k| hex_str(k.as_bytes())).unwrap_or_default(),
            c.decoded.rpk,
            "{}: rpk 段",
            c.name
        );

        // 载荷分步：body_b64 解码 → parse_body（借用零拷贝面）+ CRC 段逐字节
        let raw = URL_SAFE_NO_PAD
            .decode(c.body_b64.as_bytes())
            .unwrap_or_else(|e| panic!("{}: body_b64 非法：{e}", c.name));
        assert_eq!(hex_str(&raw), c.body_hex, "{}: body 字节", c.name);
        assert_eq!(hex_str(&raw[raw.len() - 4..]), c.crc_hex, "{}: crc 段", c.name);
        let parsed = token::parse_body(&raw).unwrap();
        assert_eq!(parsed.peer_id().as_bytes(), tok.peer_id.as_bytes());
        assert_eq!(parsed.secret().as_bytes(), tok.secret.as_bytes());

        // 再编码：向量串 ↔ 本仓编码器逐字节一致（bless 的镜子）
        let (peer, secret, eps, rpk) = parse_input(&c.input);
        let re = token::encode(&token::TokenSpec {
            peer_id: &peer,
            secret: &secret,
            endpoints: &eps,
            rpk: rpk.as_ref(),
        })
        .unwrap();
        assert_eq!(re, c.token, "{}: 再编码应与向量串逐字节一致", c.name);
    }
}

/// 负例：错误类别与三哨兵一一对应。
#[test]
fn token_hmw2_vectors_error_taxonomy() {
    let v = vectors();
    assert!(v.errors.len() >= 5, "负例向量应 ≥5 案");
    for c in &v.errors {
        let err = token::decode(&c.input)
            .err()
            .unwrap_or_else(|| panic!("{}: 应报错却解析成功", c.name));
        assert_eq!(err_kind(&err), c.error, "{}: 错误类别", c.name);
    }
}

/// 哨兵文案：Display 前缀段（经 NAPI 直达 App）。
#[test]
fn token_sentinel_display_texts_match() {
    let v = vectors();
    assert_eq!(TokenError::Corrupted.to_string(), v.sentinels.corrupted);
    assert_eq!(
        TokenError::UnsupportedVersion {
            seen: "hmw2".into()
        }
        .to_string(),
        v.sentinels.unsupported_wrapped
    );
    let m = TokenError::Malformed {
        reason: "缺少 hmw2 前缀",
    }
    .to_string();
    assert_eq!(m, v.sentinels.malformed_no_prefix);
    assert!(v.sentinels.malformed.starts_with("homeway/token: 格式非法"));
    // 存量 `hmw1` 串 ⇒ 明确的版本拒（可行动文案）
    assert_eq!(
        TokenError::UnsupportedVersion {
            seen: "hmw1".into()
        }
        .to_string(),
        v.sentinels.unsupported_version
    );
    assert_eq!(
        TokenError::Malformed {
            reason: "端点缺端口"
        }
        .to_string(),
        v.sentinels.malformed
    );
}

fn crc4_tail(body: &mut Vec<u8>) {
    use sha2::{Digest, Sha256};
    let sum = Sha256::digest(&body[..]);
    body.extend_from_slice(&sum[..4]);
}

/// **生成器**（`#[ignore]`；自产向量的唯一写点）：正例串全部由本仓编码器产出，负例按
/// **构造规则**就地生成（截断 / CRC 翻转 / 未知 critical 段 / 段体越界 / base64 尾缀）。
#[test]
#[ignore = "生成器：显式写 fixtures/vectors/token_hmw2.json（--ignored 才跑）"]
fn bless_token_hmw2_vectors() {
    let mk = |seed: u8| -> (String, String) {
        (
            hex_str(&[seed; 32]),
            (0..32u8).map(|i| seed.wrapping_add(i)).collect::<Vec<_>>().iter().map(|b| format!("{b:02x}")).collect(),
        )
    };
    let spec = |seed: u8, endpoints: Vec<RawEndpoint>, rpk: u8| -> RawInput {
        let (peer_id, secret) = mk(seed);
        RawInput {
            peer_id,
            secret,
            endpoints,
            rpk: if rpk == 0 { String::new() } else { hex_str(&[rpk; 32]) },
        }
    };
    let ep = |addr: &str, kind: u8| RawEndpoint { addr: addr.to_string(), kind };

    let cases_spec: Vec<(&str, RawInput)> = vec![
        ("min-zero-endpoint", spec(0x11, vec![], 0)),
        ("single-direct", spec(0x22, vec![ep("127.0.0.1:42641", 0)], 0)),
        (
            "mixed-direct-domain-relay-quic",
            spec(
                0x33,
                vec![
                    ep("192.168.3.12:42651", 0),
                    ep("exit.example.com:42651", 0),
                    ep("198.51.100.212:41741", 1),
                    ep("[2408:8207:2518:2550::1]:42680", 2),
                ],
                0,
            ),
        ),
        (
            "addr-255-upper-bound",
            spec(0x44, vec![ep(&format!("127.0.0.1:{}", "7".repeat(245)), 2)], 0),
        ),
        (
            "with-rpk-info-segment",
            spec(0x55, vec![ep("198.51.100.7:42652", 2)], 0x77),
        ),
        (
            "quic-multi-endpoint-with-rpk",
            spec(
                0x66,
                vec![ep("192.168.3.12:42652", 2), ep("203.0.113.7:42652", 2)],
                0x88,
            ),
        ),
        (
            "legacy-wg-only-endpoints",
            spec(
                0x99,
                vec![ep("192.168.3.12:41641", 0), ep("203.0.113.9:41741", 1)],
                0,
            ),
        ),
    ];

    let mut cases = Vec::new();
    for (name, input) in &cases_spec {
        let (peer, secret, eps, rpk) = parse_input(input);
        let tok = token::encode(&token::TokenSpec {
            peer_id: &peer,
            secret: &secret,
            endpoints: &eps,
            rpk: rpk.as_ref(),
        })
        .unwrap();
        let b64 = tok.strip_prefix(token::PREFIX).unwrap().to_string();
        let body = URL_SAFE_NO_PAD.decode(b64.as_bytes()).unwrap();
        cases.push(Case {
            name: name.to_string(),
            input: input.clone(),
            token: tok,
            decoded: input.clone(),
            body_b64: b64,
            body_hex: hex_str(&body),
            crc_hex: hex_str(&body[body.len() - 4..]),
        });
    }

    // 负例（构造规则）
    let good = cases[1].token.clone();
    let decode_body = |t: &str| -> Vec<u8> {
        URL_SAFE_NO_PAD.decode(t.strip_prefix(token::PREFIX).unwrap()).unwrap()
    };
    let flip = {
        let mut b = decode_body(&good);
        let n = b.len() - 1;
        b[n] ^= 0xFF;
        format!("{}{}", token::PREFIX, URL_SAFE_NO_PAD.encode(&b))
    };
    let truncated = format!(
        "{}{}",
        token::PREFIX,
        &good[token::PREFIX.len()..good.len() - 6]
    );
    let unknown_critical = {
        let mut b = decode_body(&good);
        b.truncate(b.len() - 4);
        b[0] += 1;
        b.push(0x85);
        b.extend_from_slice(&1u16.to_be_bytes());
        b.push(0x00);
        crc4_tail(&mut b);
        format!("{}{}", token::PREFIX, URL_SAFE_NO_PAD.encode(&b))
    };
    let overrun = {
        let mut b = decode_body(&good);
        b.truncate(b.len() - 4);
        b[3] = 0xFF; // 首段声明的 len 高字节（段头 = [type, len_hi, len_lo]）
        crc4_tail(&mut b);
        format!("{}{}", token::PREFIX, URL_SAFE_NO_PAD.encode(&b))
    };

    let errors = vec![
        ErrorCase {
            // 存量旧版串（M5 前铸造形态）⇒ 明确拒（无兼容包袱）
            name: "legacy-hmw1-token".into(),
            input: "hmw1EREREREREREREREREREREREREREREREREREREREREREBAgMEBQYHCAkKCwwNDg8QERITFBUWFxgZGhscHR4fIADcQl7l".into(),
            error: "unsupported_version".into(),
        },
        ErrorCase {
            name: "no-prefix".into(),
            input: "AAAA".into(),
            error: "malformed".into(),
        },
        ErrorCase {
            name: "crc-flipped".into(),
            input: flip,
            error: "corrupted".into(),
        },
        ErrorCase {
            name: "truncated".into(),
            input: truncated,
            error: "corrupted".into(),
        },
        ErrorCase {
            name: "base64-padding-suffix".into(),
            input: format!("{good}="),
            error: "malformed".into(),
        },
        ErrorCase {
            name: "unknown-critical-segment".into(),
            input: unknown_critical,
            error: "malformed".into(),
        },
        ErrorCase {
            name: "segment-body-overruns-payload".into(),
            input: overrun,
            error: "malformed".into(),
        },
    ];

    let out = Vectors {
        generated_by: "crates/homeway-core/tests/token_vectors.rs::bless_token_hmw2_vectors（M5 S5t 自产；旧 Go 冻结向量 token.json 已退役为历史参照）".into(),
        sentinels: Sentinels {
            corrupted: TokenError::Corrupted.to_string(),
            unsupported_version: format!("{}", TokenError::UnsupportedVersion { seen: "hmw1".into() }),
            unsupported_wrapped: format!("{}", TokenError::UnsupportedVersion { seen: "hmw2".into() }),
            malformed: format!("{}", TokenError::Malformed { reason: "端点缺端口" }),
            malformed_no_prefix: format!("{}", TokenError::Malformed { reason: "缺少 hmw2 前缀" }),
        },
        cases,
        errors,
    };
    let json = serde_json::to_string_pretty(&out).unwrap() + "\n";
    std::fs::write(vectors_path(), json).unwrap();
    println!("[bless] 已写 {}", vectors_path().display());
}
