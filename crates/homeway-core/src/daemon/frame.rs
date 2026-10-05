//! 帧封装（daemon 控制面 wire 底座；语义真源 `baseline:internal/control/frame.go`，
//! spec = tier 仓 openspec daemon-control-plane「帧封装」）。
//!
//! 单条 UDS 连接多路复用三类流量（请求/响应、事件推送、流式通道数据），统一
//! `[op:1][len:4 大端][body]`。帧长上限（spec 冻结）：控制帧 body ≤ 1MiB、流
//! DATA 帧 body ≤ 256KiB；**长度先验**——声明长度超限时在读 body 之前即报
//! [`ErrBadFrame`]（body 留在流里，连接层随后回 `goodbye(bad_frame)` 断连）。
//!
//! `#[non_exhaustive]` 纪律见仓 AGENTS「工程原则」；op 码位表只增不改（预留段
//! 0x05–0x0F / 0x14–0x1F / 0x22–0x2F，新段自 0x30 起）。

use std::io::{Read, Write};

/// 控制面协议版本（fixtures 按 protoVersion 分目录：v1/…）。
pub const PROTO_VERSION: i32 = 1;

/// op 码位表（spec「帧封装」初始集，只增不改）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Op {
    Hello,
    Welcome,
    Reload,
    Goodbye,
    Req,
    Rsp,
    Evt,
    StreamData,
    StreamEnd,
}

impl Op {
    pub const fn code(self) -> u8 {
        match self {
            Op::Hello => 0x01,
            Op::Welcome => 0x02,
            Op::Reload => 0x03,
            Op::Goodbye => 0x04,
            Op::Req => 0x10,
            Op::Rsp => 0x11,
            Op::Evt => 0x12,
            Op::StreamData => 0x20,
            Op::StreamEnd => 0x21,
        }
    }

    /// 码位表内解出；**预留段与任意其它值都算非法**（bad_frame 断连）。
    pub const fn from_code(code: u8) -> Option<Op> {
        match code {
            0x01 => Some(Op::Hello),
            0x02 => Some(Op::Welcome),
            0x03 => Some(Op::Reload),
            0x04 => Some(Op::Goodbye),
            0x10 => Some(Op::Req),
            0x11 => Some(Op::Rsp),
            0x12 => Some(Op::Evt),
            0x20 => Some(Op::StreamData),
            0x21 => Some(Op::StreamEnd),
            _ => None,
        }
    }

    /// 规范名（fixtures/日志用）。
    pub const fn name(self) -> &'static str {
        match self {
            Op::Hello => "hello",
            Op::Welcome => "welcome",
            Op::Reload => "reload",
            Op::Goodbye => "goodbye",
            Op::Req => "req",
            Op::Rsp => "rsp",
            Op::Evt => "evt",
            Op::StreamData => "stream.data",
            Op::StreamEnd => "stream.end",
        }
    }
}

/// 帧长上限（冻结，spec「帧封装」）。
pub const MAX_CONTROL_BODY: usize = 1 << 20;
/// 流 DATA 帧 body（含 4 字节 streamId 前缀）的上限：256KiB。
pub const MAX_STREAM_BODY: usize = 256 << 10;
/// 流 body 的 streamId 前缀宽度。
pub const STREAM_ID_SIZE: usize = 4;

/// 帧层不可恢复错误（长度超限 / 流 body 短于 streamId 前缀 / 半帧）。
/// 连接层收到 = 回 `goodbye(bad_frame)` 后断连；调用方 MUST NOT 继续在该流上解码。
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum FrameError {
    #[error("bad_frame: 声明长度 {len} 超上限 {max}（op=0x{op:02x}）")]
    Overlong { op: u8, len: u32, max: usize },
    #[error("bad_frame: 流 body 短于 streamId 前缀（{0} 字节）")]
    ShortStreamBody(usize),
    #[error("bad_frame: op 0x{0:02x} 不在码位表（预留段/未知值）")]
    BadOp(u8),
    #[error("IO：{0}")]
    Io(#[from] std::io::Error),
}

/// 已过上限先验的帧头（op + 声明 body 长度）。
#[derive(Debug, Clone, Copy)]
pub struct FrameHead {
    pub op: u8,
    pub n: u32,
}

/// 按 op 选帧长上限（流 DATA 帧 256KiB、其余控制类 1MiB）。读循环 MUST 走这条
/// 路——一律按控制上限读会让流帧的 256KiB 上限在读路径不生效（Go exec-r1 B2
/// 同款教训）。
fn max_body_for(op: u8) -> usize {
    if op == Op::StreamData.code() {
        MAX_STREAM_BODY
    } else {
        MAX_CONTROL_BODY
    }
}

/// 读 5 字节帧头并按 op 选上限做长度先验（超限**不读 body** 返回
/// [`FrameError::Overlong`]）。服务器/客户端读循环用（`read_header` + `read_body`
/// 组合 = 按 op 的两档上限）。IO 错误/EOF 原样透出（= 连接结束）。
pub fn read_header(r: &mut impl Read) -> Result<FrameHead, FrameError> {
    let mut head = [0u8; 5];
    r.read_exact(&mut head)?;
    let fh = FrameHead { op: head[0], n: u32::from_be_bytes([head[1], head[2], head[3], head[4]]) };
    let max = max_body_for(fh.op);
    if fh.n as u64 > max as u64 {
        // 长度先验（host-cli 3b L1 同义）：Rust usize ≥ 32 位下 u32 比较天然无
        // 符号，无 Go 侧 int(h.N) 变负的绕过面，但比较仍按无符号双写钉死口径。
        return Err(FrameError::Overlong { op: fh.op, len: fh.n, max });
    }
    Ok(fh)
}

/// 读 FrameHead 声明的 body（head 已过 read_header 的上限先验）。
pub fn read_body(r: &mut impl Read, head: FrameHead) -> Result<Vec<u8>, FrameError> {
    let mut body = vec![0u8; head.n as usize];
    r.read_exact(&mut body)?;
    Ok(body)
}

/// 写一帧（`[op:1][len:4 大端][body]`）。调用方保证 body 不超所属类的上限。
pub fn write_frame(w: &mut impl Write, op: Op, body: &[u8]) -> std::io::Result<()> {
    let mut head = [0u8; 5];
    head[0] = op.code();
    head[1..5].copy_from_slice(&(body.len() as u32).to_be_bytes());
    w.write_all(&head)?;
    if !body.is_empty() {
        w.write_all(body)?;
    }
    Ok(())
}

/// 帧编码为字节（fixtures 对拍/测试用）。
pub fn encode_frame(op: Op, body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(5 + body.len());
    out.push(op.code());
    out.extend_from_slice(&(body.len() as u32).to_be_bytes());
    out.extend_from_slice(body);
    out
}

// ---------- 流 DATA 帧的 body 封装（spec：[streamId:4 大端][原始字节]） ----------

/// 流 DATA 帧 body = `[streamId:4 大端][bytes]`。
pub fn encode_stream_body(stream_id: u32, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(STREAM_ID_SIZE + payload.len());
    out.extend_from_slice(&stream_id.to_be_bytes());
    out.extend_from_slice(payload);
    out
}

/// 解流 DATA 帧 body；短于 4 字节前缀 = [`FrameError::ShortStreamBody`]（连接层断连）。
pub fn decode_stream_body(body: &[u8]) -> Result<(u32, &[u8]), FrameError> {
    if body.len() < STREAM_ID_SIZE {
        return Err(FrameError::ShortStreamBody(body.len()));
    }
    let id = u32::from_be_bytes([body[0], body[1], body[2], body[3]]);
    Ok((id, &body[STREAM_ID_SIZE..]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_frame() {
        let f = encode_frame(Op::Evt, b"hello-body");
        assert_eq!(&f[..5], &[0x12, 0, 0, 0, 10]);
        let mut cur = std::io::Cursor::new(f.clone());
        let h = read_header(&mut cur).unwrap();
        assert_eq!(h.op, Op::Evt.code());
        assert_eq!(h.n, 10);
        let mut body = vec![0u8; 10];
        cur.read_exact(&mut body).unwrap();
        assert_eq!(body, b"hello-body");
    }

    #[test]
    fn overlong_rejected_before_body() {
        // 控制类上限 1MiB：声明 1MiB+1 拒；body 不被读（流位置停在头后）。
        let mut head = vec![Op::Req.code()];
        head.extend_from_slice(&((MAX_CONTROL_BODY as u32 + 1).to_be_bytes()));
        head.extend_from_slice(b"XXXX");
        let mut cur = std::io::Cursor::new(head);
        match read_header(&mut cur) {
            Err(FrameError::Overlong { len, max, .. }) => {
                assert_eq!(len as usize, MAX_CONTROL_BODY + 1);
                assert_eq!(max, MAX_CONTROL_BODY);
            }
            other => panic!("应报 Overlong，得 {other:?}"),
        }
        assert_eq!(cur.position(), 5, "超限帧 body 必须留在流里（不读）");
    }

    #[test]
    fn stream_frame_gets_stream_limit() {
        // 流 DATA 帧走 256KiB 档：512KiB 声明在流帧上拒（Go exec-r1 B2 的回归钉）。
        let mut head = vec![Op::StreamData.code()];
        head.extend_from_slice(&((MAX_STREAM_BODY as u32 + 1).to_be_bytes()));
        let mut cur = std::io::Cursor::new(head);
        assert!(matches!(read_header(&mut cur), Err(FrameError::Overlong { .. })));
        // 同一声明长度在控制帧（如 rsp）上合法。
        let mut head2 = vec![Op::Rsp.code()];
        head2.extend_from_slice(&((MAX_STREAM_BODY as u32 + 1).to_be_bytes()));
        let mut cur2 = std::io::Cursor::new(head2);
        assert!(read_header(&mut cur2).is_ok());
    }

    #[test]
    fn stream_body_codec() {
        let b = encode_stream_body(7, b"abc");
        assert_eq!(b, [0, 0, 0, 7, b'a', b'b', b'c']);
        let (id, payload) = decode_stream_body(&b).unwrap();
        assert_eq!((id, payload), (7, &b"abc"[..]));
        assert!(matches!(decode_stream_body(&[0, 0]), Err(FrameError::ShortStreamBody(2))));
    }

    #[test]
    fn op_table_bidirectional() {
        for code in [0x01u8, 0x02, 0x03, 0x04, 0x10, 0x11, 0x12, 0x20, 0x21] {
            let op = Op::from_code(code).unwrap();
            assert_eq!(op.code(), code);
            assert!(!op.name().is_empty());
        }
        // 预留段与未知值：全部非法。
        for code in [0x05u8, 0x0F, 0x13, 0x14, 0x1F, 0x22, 0x2F, 0x30, 0x00, 0xFF] {
            assert!(Op::from_code(code).is_none(), "0x{code:02x} 应为非法 op");
        }
    }
}
