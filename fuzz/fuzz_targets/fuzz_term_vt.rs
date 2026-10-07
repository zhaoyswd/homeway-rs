#![no_main]
//! fuzz_term_vt — term 屏态仿真底座 + 键/鼠标编码器（Q-D F11 深挖轨）。
//!
//! oracle：① 不 panic（VT 流/应答/读取面全链）；② `dirty_rows()` 每格
//! `symbol.len() <= 127`——F5 的**运行时哨兵**（`debug_assert` 在 release/fuzz 构建下
//! 关闭，超长字素簇的截断不变量由本断言兜）。
//! 种子：`fixtures/term-vt/*.bin`（真会话字节流）——本目标无独立 README，口径写在这里。
//! 回归轨（同名目标、xorshift 重放）：`crates/homeway-core/tests/fuzz_replay.rs::fuzz_term_vt`。

use homeway_core::term::keyenc;
use homeway_core::term::vt::SessionVt;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(mut vt) = SessionVt::new(20, 5, 100) else { return };
    // 喂入（收集应答分片；上限防病态输入把应答数组撑爆——不是库面不变量）
    let mut responses = 0usize;
    vt.write_collecting(data, &mut |_r| {
        responses += 1;
    });
    let _ = responses;
    let _ = vt.take_clip_events();
    let _ = vt.update();
    // F5 运行时哨兵：每格 symbol ≤ 127 B（长度域 7 位）
    for row in vt.dirty_rows() {
        for c in &row.cells {
            assert!(c.symbol.len() <= 127, "cell symbol 超上限（F5 哨兵）：{} B", c.symbol.len());
        }
    }
    vt.clean();
    // 读取面（快照/采样/explain/FETCH-ROWS/镜像）
    let _ = vt.rows();
    let _ = vt.screen_text();
    let _ = vt.plain_text();
    let _ = vt.cursor();
    let _ = vt.modes();
    let _ = vt.scrollbar();
    let _ = vt.rows_at(0, 64);
    let _ = vt.mirror_rows(32);
    // 键/鼠标/焦点编码面（模式位驱动的编码器；事件从输入字节派生）
    let b = |i: usize| data.get(i).copied().unwrap_or(0);
    let _ = vt.encode_key(&keyenc::KeyEvent {
        key: keyenc::Key(u16::from_le_bytes([b(0), b(1)])),
        action: keyenc::KeyAction::from_wire(b(2) % 3).unwrap_or_default(),
        mods: keyenc::Mods(u16::from_le_bytes([b(3), b(4)])),
        text: "",
        composing: b(5) & 1 != 0,
    });
    let _ = vt.encode_mouse(&keyenc::MouseEvent {
        action: keyenc::MouseAction::from_wire(b(6) % 3).unwrap_or_default(),
        button: keyenc::MouseButton(b(7)),
        mods: keyenc::Mods(u16::from_le_bytes([b(8), b(9)])),
        x: u16::from_le_bytes([b(10), b(11)]),
        y: u16::from_le_bytes([b(12), b(13)]),
    });
    let _ = keyenc::encode_focus(b(14) & 1 != 0);
    let _ = keyenc::encode_paste_part(data, true, b(15) & 1 != 0, b(16) & 1 != 0);
});
