#![no_main]
//! fuzz_term_codec — surface v4 体解码族 + 往返字节相等（Q-D F11 深挖轨）。
//!
//! harness 预检（评审 6.2）：**按目标分别从输入派生维度**再限 `cols×rows ≤ 2e5` 才调用
//! 解码——`decode_grid` 读头 1+2+2；`decode_rows` 的 cols/count 由前 4B 派生；
//! `dec_snapshot_body`/`dec_diff_body` 的 cols/rows 在第 5-8 字节（0-based 5..9）。
//! **预检 ≠ 库守卫**：库侧守卫（F10 的 `guard_dims` + `MAX_GUNZIP_OUT`）由
//! `codec.rs` 单测钉住；这里只是让 fuzz 迭代在合理内存/时间预算内跑。
//!
//! oracle：① 不 panic；② **decode → 再 encode 字节相等**（decode 派生 `width`/`wraps`，
//! 结构体相等会假红——评审 6.5）。
//! `-max_len=8192`（与 files 目标的 262160 不同）：term 面最大合法形态 = 小网格 + 帧载荷，
//! 无需 70KB 跨 u16 样本；深挖档命令见 `tools/ci-local.sh` 注释。
//! 种子：`fixtures/surface-golden/*.bin` + `fixtures/vectors/surface_codec.json`。
//! 回归轨：`crates/homeway-core/tests/fuzz_replay.rs::fuzz_term_codec`。

use homeway_core::term::codec;
use homeway_core::term::vt;
use libfuzzer_sys::fuzz_target;

/// harness 预检上限（≠ 库守卫）。
const DIM_CAP: u64 = 200_000;

/// 输入派生的小网格（每格 1..=4 字节 ASCII symbol——覆盖 CELL/BLANK_RUN/REPEAT 三标记）。
fn synth_rows(data: &[u8], cols: usize, rows: usize) -> Vec<vt::Row> {
    let mut chunks = data.chunks(4);
    let mut out = Vec::with_capacity(rows);
    for y in 0..rows {
        let mut cells = Vec::with_capacity(cols);
        for x in 0..cols {
            let sym: String = chunks
                .next()
                .unwrap_or(&[])
                .iter()
                .filter(|b| b.is_ascii_graphic())
                .map(|b| *b as char)
                .collect();
            let (fg, bg) = match (x + y) % 3 {
                0 => (vt::Color::None, vt::Color::None),
                1 => (vt::Color::Palette((x % 256) as u8), vt::Color::None),
                _ => (vt::Color::Rgb(x as u8, y as u8, 7), vt::Color::Palette(9)),
            };
            cells.push(vt::Cell {
                symbol: if sym.is_empty() { "x".to_string() } else { sym },
                width: 1,
                skip: (x + y) % 5 == 0,
                fg,
                bg,
                attr: ((x * 7 + y * 13) & 0x0fff) as u16,
            });
        }
        out.push(vt::Row { y: y as u16, dirty: false, wraps: false, cells });
    }
    out
}

fuzz_target!(|data: &[u8]| {
    // ---- ① 自产网格往返（字节相等）----
    let cols = 1 + (data.first().copied().unwrap_or(0) as usize % 40);
    let rows = 1 + (data.get(1).copied().unwrap_or(0) as usize % 8);
    let synth = synth_rows(data, cols, rows);
    let grid = codec::encode_grid(cols as u16, rows as u16, &synth);
    let (c, r, decoded) = codec::decode_grid(&grid).expect("自产网格必可解");
    assert_eq!(codec::encode_grid(c, r, &decoded), grid, "encode→decode→encode 字节相等");
    let rows_enc = codec::encode_rows(&synth);
    let decoded_rows = codec::decode_rows(&rows_enc, rows, cols).expect("自产行序列必可解");
    assert_eq!(codec::encode_rows(&decoded_rows), rows_enc, "行序列往返字节相等");

    // ---- ② 原始输入的解析面（按布局派生维度做 harness 预检）----
    if data.len() >= 5 {
        let gc = u16::from_le_bytes([data[1], data[2]]) as u64;
        let gr = u16::from_le_bytes([data[3], data[4]]) as u64;
        if gc.saturating_mul(gr) <= DIM_CAP {
            let _ = codec::decode_grid(data);
        }
    }
    if data.len() >= 4 {
        let rc = u16::from_le_bytes([data[0], data[1]]) as u64;
        let rn = u16::from_le_bytes([data[2], data[3]]) as u64;
        if rc.saturating_mul(rn) <= DIM_CAP {
            let _ = codec::decode_rows(&data[4..], rn as usize, rc as usize);
        }
    }
    // 体解码：cols/rows 在第 5-8 字节（ver 1 + revision 4 之后）
    if data.len() >= 9 {
        let bc = u16::from_le_bytes([data[5], data[6]]) as u64;
        let br = u16::from_le_bytes([data[7], data[8]]) as u64;
        if bc.saturating_mul(br) <= DIM_CAP {
            let _ = codec::dec_snapshot_body(data);
            let _ = codec::dec_diff_body(data);
        }
    }
    // 其余小件（无分配放大面）
    let _ = codec::gunzip_bytes(data);
    let _ = codec::dec_fetch_rows_req(data);
    let _ = codec::dec_fetch_rows_reply(data);
    let _ = codec::dec_fragment(data);
    let _ = codec::dec_theme(data);
    let _ = codec::dec_clipboard(data);
    let _ = codec::dec_notify(data);
    let mut asm = codec::FragAssembler::default();
    for frag in data.chunks(64) {
        let _ = asm.push(frag);
    }
    let _ = codec::fragment_payload(data);
});
