#![no_main]
//! fuzz_term_frames — term 帧协议解码族（Q-D F11 深挖轨）。
//!
//! 口径：帧层载荷小、无分配放大面 ⇒ 不做 dims 预检（对照 `fuzz_term_codec` 的预检注记）。
//! `read_frame` 用 `&[u8]` 当 `Read`（帧头/长度域/尾随块的全部分支）。
//! 种子：`tools/gen-fuzz-seeds.sh` 的 term 区（`fixtures/term/frames.v1.jsonl` 的
//! payloadHex + `fixtures/vectors/term_*.json`）——本目标无独立 README，口径写在这里。
//! 回归轨（同名目标、xorshift 重放）：`crates/homeway-core/tests/fuzz_replay.rs::fuzz_term_frames`。

use homeway_core::term::frames;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = frames::read_frame(&mut &data[..]);
    let _ = frames::dec_greeting(data);
    let _ = frames::dec_hello(data);
    let _ = frames::dec_hello_tail(data);
    let _ = frames::dec_create(data);
    let _ = frames::dec_resize(data);
    let _ = frames::dec_attached(data);
    let _ = frames::dec_replay_done(data);
    let _ = frames::dec_ended(data);
    let _ = frames::dec_state(data);
    let _ = frames::dec_error(data);
    let _ = frames::dec_name(data);
    let _ = frames::dec_input(data);
    // 编码面往返（合法输入手搓帧 → 读回；长度域截断语义同 Go）
    let _ = frames::encode_frame(frames::Op(0x01), data);
});
