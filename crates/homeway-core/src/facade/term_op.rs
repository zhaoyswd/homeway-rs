//! 终端一次性操作面（语义真源 `baseline:clientcore/cmd/clientcore/app_term.go`）。
//!
//! 会话列表/终止会话不需要终端渲染：经终端通道桥（`<filesDir>/bridge/term.sock` →
//! 隧道 → 出口 term 端口）说帧协议——每次操作开一条短连接、发一帧、读回复、关连接。
//! JSON 进 JSON 出（`{"op":…,"auth":…,"sock":…[, "name":…]}`）。
//!
//! 帧操作码与编解码全部经 `crate::term::frames`（FIX-94 同义：升级帧格式只改一处）；
//! 本模块只剩一次性操作（GREETING/LIST/KILL/OK/ERROR）的归因层（termError 码表，
//! contract-ledger 台账族⑦——与 App 的 TermRules.ets 对账、只增不改）。

use std::io::Write;
use std::os::unix::net::UnixStream;
use std::time::Duration;

use serde_json::{Map, Value};

use crate::term::frames::{self, Op, PROTO_VER};

/// IO 超时（Go termDialTimeout/termIOTimeout 同值；拨号经内核 backlog 即成）。
const IO_TIMEOUT: Duration = Duration::from_secs(15);

/// 桥鉴权首包长度（16B 魔数 + 32B 令牌；`app_bridge.go` bridgeAuthLen 同源）。
pub const BRIDGE_AUTH_LEN: usize = 48;

/// term 桥层归一码词表（Go termCode* 同串；与 pkg/term ERROR 帧码〔族③〕是两个独立
/// 冻结空间——族③经 remote 码透传消费）。
pub mod code {
    pub const BAD_JSON: &str = "bad_json";
    pub const INVALID_ARG: &str = "invalid_arg";
    pub const BRIDGE_DOWN: &str = "bridge_down";
    pub const BRIDGE_AUTH: &str = "bridge_auth";
    pub const TERM_UNREACHABLE: &str = "term_unreachable";
    pub const TERM_FOREIGN: &str = "term_foreign_service";
    pub const TERM_VERSION: &str = "term_version";
    pub const IO: &str = "io";
    pub const BAD_REPLY: &str = "bad_reply";
    pub const REMOTE: &str = "remote";
    pub const MARSHAL: &str = "marshal";
}

/// 带稳定错误码的失败（termError 的 Rust 面）。
#[derive(Debug, Clone)]
pub struct TermOpError {
    pub code: &'static str,
    pub msg: String,
    /// 族③透传码（ERROR 帧的动态 code；仅 REMOTE 归层时填——marshal 面 `code` 键
    /// 原样流通，归层静态码只做格式不完整时的兜底）。
    remote_code: Option<String>,
}

impl TermOpError {
    fn new(code: &'static str, msg: impl Into<String>) -> Self {
        TermOpError { code, msg: msg.into(), remote_code: None }
    }
}

/// ClientCoreTermCall：opJson 进、JSON 出（永不 reject 的信封面）。
pub fn term_call(op_json: &str) -> String {
    let op: Value = match serde_json::from_str(op_json) {
        Ok(v) => v,
        Err(e) => return term_marshal(None, Some(TermOpError::new(code::BAD_JSON, format!("参数不是合法 JSON：{e}")))),
    };
    let name = op.get("op").and_then(Value::as_str).unwrap_or("");
    let auth = op.get("auth").and_then(Value::as_str).unwrap_or("");
    let sock = op.get("sock").and_then(Value::as_str).unwrap_or("");
    let (res, terr) = match name {
        "list" => term_list(auth, sock),
        "kill" => {
            let who = op.get("name").and_then(Value::as_str).unwrap_or("");
            if who.is_empty() {
                (None, Some(TermOpError::new(code::INVALID_ARG, "kill 需要 name")))
            } else {
                term_kill(who, auth, sock)
            }
        }
        other => (None, Some(TermOpError::new(code::INVALID_ARG, format!("未知操作 {other:?}")))),
    };
    term_marshal(res, terr)
}

/// 连上桥 + 发鉴权首包 + 校验 GREETING（每操作一条短连接）。
fn term_dial(auth_hex: &str, sock: &str) -> Result<UnixStream, TermOpError> {
    if sock.is_empty() {
        return Err(TermOpError::new(
            code::BRIDGE_DOWN,
            "终端通道暂时不可用（桥未就绪：VPN 未连接且服务会话未就绪，或正在恢复）",
        ));
    }
    // 工单④：UDS 拨号加 connect 预算（本地一般即成；对端 backlog 满时不得无限挂）
    let mut conn = super::bridge_host::connect_budget(
        std::path::Path::new(sock),
        Duration::from_millis(500),
    )
        .map_err(|e| TermOpError::new(code::BRIDGE_DOWN, format!("终端通道暂时不可用（桥未就绪或正在恢复）：{e}")))?;
    conn.set_write_timeout(Some(IO_TIMEOUT)).ok();
    conn.set_read_timeout(Some(IO_TIMEOUT)).ok();
    write_auth(&mut conn, auth_hex)
        .map_err(|e| TermOpError::new(code::BRIDGE_AUTH, format!("终端通道鉴权失败：{e}")))?;
    let frame = frames::read_frame(&mut conn).map_err(|e| {
        // 两种可能合并文案（Go 同款可行动归因）：① 出口没有 term 服务（回落成本机端口的
        // banner/断开）；② 这条隧道的回程地址已失效（出口 no candidates available for
        // endpoint）。先重连 VPN（② 的标准恢复动作）。
        TermOpError::new(
            code::TERM_UNREACHABLE,
            format!(
                "终端服务没有应答：可能是该出口未启用终端（官方版/旧版 tailcat），\
                 也可能是本机隧道的回程地址已失效（出口日志会看到 no candidates available for endpoint）。\
                 先重连一次 VPN；若仍不行，请把出口升级到增强版 tailcat。详情：{e}"
            ),
        )
    })?;
    if frame.op != Op::GREETING {
        // 文案保持**逐字**（Go 真源 `baseline:app_term.go:125` 的
        // `出口 %d 端口上不是终端服务（收到帧 0x%02x）`，端口 = `termServicePort()`）。
        // M3 A13 逐条复核：本串不改——①它是 App 可见文本，Go 对齐面要求同串；
        // ②QUIC 档虽不再拨端口，此处的「7724 端口」是**服务身份**的指代（用户可见的诊断语），
        //   改字反而与基础词表脱钩。**登记在案**（不入判据行；如需去端口 = 跨仓词表批）。
        return Err(TermOpError::new(
            code::TERM_FOREIGN,
            format!("出口 7724 端口上不是终端服务（收到帧 0x{:02x}）", frame.op.0),
        ));
    }
    match frames::dec_greeting(&frame.payload) {
        Ok((ver, _)) if ver == PROTO_VER => Ok(conn),
        _ => Err(TermOpError::new(
            code::TERM_VERSION,
            format!("出口终端服务协议版本不匹配（本端 {PROTO_VER}）"),
        )),
    }
}

/// 桥鉴权首包（客户端面；Go bridgeWriteAuth 同义：hex 解码 + 长度校验 + 原样写出）。
pub fn write_auth<W: Write>(w: &mut W, auth_hex: &str) -> std::io::Result<()> {
    let blob = hex_decode(auth_hex).ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "鉴权 blob 不是合法 hex")
    })?;
    if blob.len() != BRIDGE_AUTH_LEN {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("鉴权 blob 长度不对（{} ≠ {BRIDGE_AUTH_LEN}；App 还没拿到本会话令牌？）", blob.len()),
        ));
    }
    w.write_all(&blob)
}

/// 小写 hex 解码（48B blob 的 96 hex 面；不引依赖——hex 面足够小）。
fn hex_decode(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(s.len() / 2);
    for pair in bytes.chunks(2) {
        let hi = (pair[0] as char).to_digit(16)?;
        let lo = (pair[1] as char).to_digit(16)?;
        out.push((hi * 16 + lo) as u8);
    }
    Some(out)
}

fn term_list(auth_hex: &str, sock: &str) -> (Option<Value>, Option<TermOpError>) {
    let mut conn = match term_dial(auth_hex, sock) {
        Ok(c) => c,
        Err(e) => return (None, Some(e)),
    };
    if let Err(e) = conn.write_all(&frames::encode_frame(Op::LIST, &[])) {
        return (None, Some(TermOpError::new(code::IO, format!("发送 LIST 失败：{e}"))));
    }
    let frame = match frames::read_frame(&mut conn) {
        Ok(f) => f,
        Err(e) => return (None, Some(TermOpError::new(code::IO, format!("读取 LIST 回复失败：{e}")))),
    };
    match frame.op {
        Op::LIST => {
            // 坏 JSON 必须报 bad_reply（评审 r1-F16：旧写法 unwrap_or(Null) 会吞错并
            // 假成功回 {"ok":true,"sessions":[]}）
            let parsed: Result<Value, _> = serde_json::from_slice(&frame.payload);
            match parsed {
                Err(e) => (None, Some(TermOpError::new(code::BAD_REPLY, format!("LIST 回复不是合法 JSON：{e}")))),
                Ok(mut out) => {
                    if !out.is_object() {
                        out = Value::Object(Map::new());
                    }
                    if let Some(obj) = out.as_object_mut() {
                        obj.entry("sessions").or_insert_with(|| Value::Array(vec![]));
                    }
                    (Some(out), None)
                }
            }
        }
        Op::ERROR => (None, Some(decode_error(&frame.payload))),
        other => (None, Some(TermOpError::new(code::BAD_REPLY, format!("LIST 回复帧意外（0x{:02x}）", other.0)))),
    }
}

fn term_kill(name: &str, auth_hex: &str, sock: &str) -> (Option<Value>, Option<TermOpError>) {
    let mut conn = match term_dial(auth_hex, sock) {
        Ok(c) => c,
        Err(e) => return (None, Some(e)),
    };
    if let Err(e) = conn.write_all(&frames::encode_frame(Op::KILL, &frames::enc_name(name))) {
        return (None, Some(TermOpError::new(code::IO, format!("发送 KILL 失败：{e}"))));
    }
    let frame = match frames::read_frame(&mut conn) {
        Ok(f) => f,
        Err(e) => return (None, Some(TermOpError::new(code::IO, format!("读取 KILL 回复失败：{e}")))),
    };
    match frame.op {
        Op::OK => {
            let mut m = Map::new();
            m.insert("killed".into(), Value::String(name.to_owned()));
            (Some(Value::Object(m)), None)
        }
        Op::ERROR => (None, Some(decode_error(&frame.payload))),
        other => (None, Some(TermOpError::new(code::BAD_REPLY, format!("KILL 回复帧意外（0x{:02x}）", other.0)))),
    }
}

/// 解 ERROR 载荷（族③码透传；格式不完整折 remote）。
fn decode_error(p: &[u8]) -> TermOpError {
    match frames::dec_error(p) {
        Ok((c, m)) => TermOpError {
            code: code::REMOTE,
            msg: m,
            remote_code: Some(c),
        },
        Err(_) => TermOpError::new(code::REMOTE, "出口错误帧格式不完整"),
    }
}

/// termMarshalJSON：成功 `{"ok":true,…载荷}`；失败 `{"error":{"code","msg"}}`（键序字典序）。
pub fn term_marshal(res: Option<Value>, err: Option<TermOpError>) -> String {
    let mut out = Map::new();
    if let Some(r) = res {
        if let Some(obj) = r.as_object() {
            for (k, v) in obj {
                out.insert(k.clone(), v.clone());
            }
        }
    }
    match err {
        Some(e) => {
            let mut em = Map::new();
            em.insert("code".into(), Value::String(e.code_owned()));
            em.insert("msg".into(), Value::String(e.msg));
            out.insert("error".into(), Value::Object(em));
        }
        None => {
            out.insert("ok".into(), Value::from(true));
        }
    }
    Value::Object(out).to_string()
}

impl TermOpError {
    /// remote 透传码的动态面：族③码原样流通（`code` 键取透传值；归层静态码兜底）。
    fn code_owned(&self) -> String {
        if self.code == code::REMOTE {
            if let Some(c) = &self.remote_code {
                return c.clone();
            }
        }
        self.code.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::os::unix::net::UnixListener;
    use std::path::PathBuf;

    fn tmp_sock(name: &str) -> (UnixListener, PathBuf) {
        let dir = std::env::temp_dir().join(format!("hwtermop-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("term.sock");
        let _ = std::fs::remove_file(&path);
        (UnixListener::bind(&path).unwrap(), path)
    }

    fn auth_hex_valid() -> String {
        let mut blob = [0u8; BRIDGE_AUTH_LEN];
        blob[..16].copy_from_slice(b"TIERBRIDGEAUTH01");
        blob.iter().map(|b| format!("{b:02x}")).collect()
    }

    /// 端到端：桥桩（auth 校验 + GREETING + LIST 应答）→ term_call 全流程信封。
    #[test]
    fn list_via_fake_bridge() {
        let (ln, path) = tmp_sock("list");
        let auth = auth_hex_valid();
        let path_str = path.to_str().unwrap().to_owned();
        // 桩线程：读 auth 48B → 回 GREETING → 读 LIST → 回 LIST 载荷
        std::thread::spawn(move || {
            let (mut c, _) = ln.accept().unwrap();
            let mut buf = [0u8; BRIDGE_AUTH_LEN];
            c.read_exact(&mut buf).unwrap();
            c.write_all(&frames::encode_frame(Op::GREETING, &[PROTO_VER, 0x01, 0x00, 0x00, 0x00]))
                .unwrap();
            let f = frames::read_frame(&mut c).unwrap();
            assert_eq!(f.op, Op::LIST);
            c.write_all(&frames::encode_frame(Op::LIST, br#"{"sessions":[{"name":"s1"}]}"#))
                .unwrap();
        });
        let out = term_call(&format!(r#"{{"op":"list","auth":"{auth}","sock":"{path_str}"}}"#));
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["ok"], true);
        assert_eq!(v["sessions"][0]["name"], "s1");
    }

    /// 错误信封：bad_json / 未知操作 / kill 缺 name / 空 sock（bridge_down）。
    #[test]
    fn error_envelopes() {
        let v: Value = serde_json::from_str(&term_call("{not json")).unwrap();
        assert_eq!(v["error"]["code"], "bad_json");
        let v: Value = serde_json::from_str(&term_call(r#"{"op":"nope"}"#)).unwrap();
        assert_eq!(v["error"]["code"], "invalid_arg");
        let v: Value = serde_json::from_str(&term_call(r#"{"op":"kill"}"#)).unwrap();
        assert_eq!(v["error"]["code"], "invalid_arg");
        let v: Value =
            serde_json::from_str(&term_call(r#"{"op":"list","auth":"","sock":""}"#)).unwrap();
        assert_eq!(v["error"]["code"], "bridge_down");
    }

    /// 鉴权失败（坏 hex / 长度不对）⇒ bridge_auth。
    #[test]
    fn auth_failures() {
        let (_ln_alive, path) = tmp_sock("auth");
        let path_str = path.to_str().unwrap().to_owned();
        let out = term_call(&format!(r#"{{"op":"list","auth":"zz","sock":"{path_str}"}}"#));
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["error"]["code"], "bridge_auth");
        let short = "ab".repeat(24); // 48B ≠ 48
        let out = term_call(&format!(r#"{{"op":"list","auth":"{short}","sock":"{path_str}"}}"#));
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["error"]["code"], "bridge_auth");
    }

    /// 非 GREETING 首帧 ⇒ term_foreign_service。
    #[test]
    fn foreign_service() {
        let (ln, path) = tmp_sock("foreign");
        let auth = auth_hex_valid();
        let path_str = path.to_str().unwrap().to_owned();
        std::thread::spawn(move || {
            let (mut c, _) = ln.accept().unwrap();
            let mut buf = [0u8; BRIDGE_AUTH_LEN];
            c.read_exact(&mut buf).unwrap();
            c.write_all(&frames::encode_frame(Op::STATE, &[0])).unwrap();
        });
        let out = term_call(&format!(r#"{{"op":"list","auth":"{auth}","sock":"{path_str}"}}"#));
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["error"]["code"], "term_foreign_service");
    }

    #[test]
    fn hex_decode_roundtrip() {
        assert_eq!(hex_decode("00ff10"), Some(vec![0, 255, 16]));
        assert_eq!(hex_decode("0"), None);
        assert_eq!(hex_decode("zz"), None);
    }
}
