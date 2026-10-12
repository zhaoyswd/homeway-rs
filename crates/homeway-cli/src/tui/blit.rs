//! 帧差分编码器（移植自 herdr `src/protocol/render_ansi.rs` 的 BlitEncoder，
//! Apache-2.0，动机与边界见 docs/reviews/TUI-terminal-stack-transplant-assessment.md）：
//! **输出侧自有差分**——列推进按 unicode 宽度自算、跳位/失效位显式传播、每帧
//! `CSI ?2026` 同步输出包裹、光标先隐藏后定位、DECSCUSR 随帧下发。这是把 r5 期间
//! 反复出现的「ratatui 后端行内游标假设」整类问题从结构上移除的层。
//!
//! v1 裁剪（相对上游）：patch 通道（本端整帧渲染，无远端补丁）、render_prof、
//! OSC 8 超链接、kitty graphics、Windows 专属路径（保留跨平台 cfg 语义）。

#![allow(dead_code)]

use std::cmp;
use std::io::Write;

use unicode_width::UnicodeWidthStr;

use super::frame::{CellData, CursorState, FrameData, underline_style_from_modifier};

const REVERSED_MODIFIER: u16 = 1 << 6;

/// 一帧编码结果。
pub(crate) struct EncodedBlit {
    /// 直接写往宿主终端的字节。
    pub bytes: Vec<u8>,
    /// 是否整屏重画（首帧/尺寸变化）。
    pub full: bool,
    next_last_visible_cursor: Option<(u16, u16)>,
    next_last_cursor_shape: u8,
}

/// 帧→ANSI 的有状态差分编码器。
#[derive(Default)]
pub(crate) struct BlitEncoder {
    last_frame: Option<FrameData>,
    last_visible_cursor: Option<(u16, u16)>,
    last_cursor_shape: u8,
}

impl BlitEncoder {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn encode(&self, frame: &FrameData, repaint: bool) -> EncodedBlit {
        let previous_frame = self.last_frame.as_ref();
        let prev = if repaint { None } else { previous_frame };
        let full = repaint
            || prev.is_none()
            || prev.is_some_and(|p| p.width != frame.width || p.height != frame.height);
        let clear_before_full_redraw = previous_frame.is_none();
        let mut bytes = Vec::new();
        let mut next_last_visible_cursor = self.last_visible_cursor;
        let mut next_last_cursor_shape = self.last_cursor_shape;
        blit_frame_to_with_cursor_memory_and_clear_policy(
            &mut bytes,
            frame,
            prev,
            &mut next_last_visible_cursor,
            &mut next_last_cursor_shape,
            repeat_ime_anchor_after_sync(),
            clear_before_full_redraw,
        );
        EncodedBlit {
            bytes,
            full,
            next_last_visible_cursor,
            next_last_cursor_shape,
        }
    }

    pub(crate) fn commit(&mut self, frame: FrameData, encoded: EncodedBlit) {
        self.last_visible_cursor = encoded.next_last_visible_cursor;
        self.last_cursor_shape = encoded.next_last_cursor_shape;
        self.last_frame = Some(frame);
    }

    pub(crate) fn is_current(&self, frame: &FrameData) -> bool {
        self.last_frame.as_ref() == Some(frame)
    }
}

// ---------------------------------------------------------------------------
// Color / modifier → SGR
// ---------------------------------------------------------------------------

/// 打包 u32 颜色 → 前景 SGR 片段（`38;5;N` / `38;2;R;G;B` / `39`，不含前缀与 m）。
fn color_to_sgr_fg(val: u32) -> String {
    match val >> 24 {
        0x00 => match val & 0xFF {
            0x00 => "39".to_owned(),
            0x01 => "30".to_owned(),
            0x02 => "31".to_owned(),
            0x03 => "32".to_owned(),
            0x04 => "33".to_owned(),
            0x05 => "34".to_owned(),
            0x06 => "35".to_owned(),
            0x07 => "36".to_owned(),
            0x08 => "37".to_owned(),
            0x09 => "90".to_owned(),
            0x0A => "91".to_owned(),
            0x0B => "92".to_owned(),
            0x0C => "93".to_owned(),
            0x0D => "94".to_owned(),
            0x0E => "95".to_owned(),
            0x0F => "96".to_owned(),
            0x10 => "97".to_owned(),
            _ => "39".to_owned(),
        },
        0x01 => format!("38;5;{}", val & 0xFF),
        0x02 => {
            let r = (val >> 16) & 0xFF;
            let g = (val >> 8) & 0xFF;
            let b = val & 0xFF;
            format!("38;2;{r};{g};{b}")
        }
        _ => "39".to_owned(),
    }
}

/// 打包 u32 颜色 → 背景 SGR 片段。
fn color_to_sgr_bg(val: u32) -> String {
    match val >> 24 {
        0x00 => match val & 0xFF {
            0x00 => "49".to_owned(),
            0x01 => "40".to_owned(),
            0x02 => "41".to_owned(),
            0x03 => "42".to_owned(),
            0x04 => "43".to_owned(),
            0x05 => "44".to_owned(),
            0x06 => "45".to_owned(),
            0x07 => "46".to_owned(),
            0x08 => "47".to_owned(),
            0x09 => "100".to_owned(),
            0x0A => "101".to_owned(),
            0x0B => "102".to_owned(),
            0x0C => "103".to_owned(),
            0x0D => "104".to_owned(),
            0x0E => "105".to_owned(),
            0x0F => "106".to_owned(),
            0x10 => "107".to_owned(),
            _ => "49".to_owned(),
        },
        0x01 => format!("48;5;{}", val & 0xFF),
        0x02 => {
            let r = (val >> 16) & 0xFF;
            let g = (val >> 8) & 0xFF;
            let b = val & 0xFF;
            format!("48;2;{r};{g};{b}")
        }
        _ => "49".to_owned(),
    }
}

/// u16 修饰位 → SGR 片段（下划线样式走扩展位：4:2/4:3/4:4/4:5）。
fn modifier_to_sgr_parts(val: u16) -> Vec<&'static str> {
    let mut parts = Vec::new();
    const BOLD: u16 = 1 << 0;
    const DIM: u16 = 1 << 1;
    const ITALIC: u16 = 1 << 2;
    const UNDERLINED: u16 = 1 << 3;
    const SLOW_BLINK: u16 = 1 << 4;
    const RAPID_BLINK: u16 = 1 << 5;
    const HIDDEN: u16 = 1 << 7;
    const CROSSED_OUT: u16 = 1 << 8;

    if val & BOLD != 0 {
        parts.push("1");
    }
    if val & DIM != 0 {
        parts.push("2");
    }
    if val & ITALIC != 0 {
        parts.push("3");
    }
    if val & UNDERLINED != 0 {
        parts.push(match underline_style_from_modifier(val) {
            2 => "4:2",
            3 => "4:3",
            4 => "4:4",
            5 => "4:5",
            _ => "4",
        });
    }
    if val & SLOW_BLINK != 0 {
        parts.push("5");
    }
    if val & RAPID_BLINK != 0 {
        parts.push("6");
    }
    if val & REVERSED_MODIFIER != 0 {
        parts.push("7");
    }
    if val & HIDDEN != 0 {
        parts.push("8");
    }
    if val & CROSSED_OUT != 0 {
        parts.push("9");
    }
    parts
}

/// 构造完整 SGR（恒以 0 复位开头——样式状态自包含，避免跨格污染）。
fn build_sgr(fg: u32, bg: u32, modifier: u16) -> String {
    let mut parts = vec!["0".to_owned()];
    parts.extend(modifier_to_sgr_parts(modifier).into_iter().map(str::to_owned));
    parts.push(color_to_sgr_fg(fg));
    parts.push(color_to_sgr_bg(bg));
    format!("\x1b[{}m", parts.join(";"))
}

// ---------------------------------------------------------------------------
// 单元格宽度与比较
// ---------------------------------------------------------------------------

/// 单元格显示宽度（半角浊音假名合成簇按 2 列——上游特例，ratatui 同口径）。
fn cell_width(cell: &CellData) -> usize {
    if is_halfwidth_katakana_voiced_grapheme(&cell.symbol) {
        return 2;
    }
    cell.symbol.width()
}

fn is_halfwidth_katakana_voiced_grapheme(symbol: &str) -> bool {
    let mut chars = symbol.chars();
    let Some(base) = chars.next() else {
        return false;
    };
    let Some(mark) = chars.next() else {
        return false;
    };
    chars.next().is_none()
        && ('\u{ff66}'..='\u{ff9d}').contains(&base)
        && matches!(mark, '\u{ff9e}' | '\u{ff9f}')
}

/// 视觉相等（skip 位不参与——仅内部差分语义）。
fn cells_visually_equal(a: &CellData, b: &CellData) -> bool {
    a.symbol == b.symbol && a.fg == b.fg && a.bg == b.bg && a.modifier == b.modifier
}

// ---------------------------------------------------------------------------
// 光标
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
struct HostCursorState {
    position: (u16, u16),
    visible: bool,
    /// DECSCUSR 参数（0–6）。0 = 终端默认。
    shape: u8,
}

fn resolve_host_cursor_state(
    frame: &FrameData,
    last_visible_cursor: &mut Option<(u16, u16)>,
) -> HostCursorState {
    if let Some(cursor) = &frame.cursor {
        let position = clamp_cursor_position(frame, cursor.x, cursor.y);
        if cursor.visible {
            *last_visible_cursor = Some(position);
        }
        return HostCursorState {
            position,
            visible: cursor.visible,
            shape: normalize_cursor_shape(cursor.shape),
        };
    }
    // 本帧无光标（面板焦点/回看中）：停在最后已知可见位置（IME 锚点不漂）。
    let position = (*last_visible_cursor)
        .map(|(x, y)| clamp_cursor_position(frame, x, y))
        .unwrap_or_else(|| default_hidden_cursor_position(frame));
    HostCursorState { position, visible: false, shape: 0 }
}

fn normalize_cursor_shape(shape: u8) -> u8 {
    if shape <= 6 { shape } else { 0 }
}

fn default_hidden_cursor_position(frame: &FrameData) -> (u16, u16) {
    (frame.width.saturating_sub(1), frame.height.saturating_sub(1))
}

fn clamp_cursor_position(frame: &FrameData, x: u16, y: u16) -> (u16, u16) {
    (
        x.min(frame.width.saturating_sub(1)),
        y.min(frame.height.saturating_sub(1)),
    )
}

fn write_cursor_position(writer: &mut impl Write, (x, y): (u16, u16)) {
    // CUP 1 基。
    let _ = write!(writer, "\x1b[{};{}H", y + 1, x + 1);
}

fn write_host_cursor_state(writer: &mut impl Write, cursor: HostCursorState, last_shape: &mut u8) {
    write_cursor_position(writer, cursor.position);
    if cursor.shape != *last_shape {
        let _ = write!(writer, "\x1b[{} q", cursor.shape);
        *last_shape = cursor.shape;
    }
    if cursor.visible {
        let _ = writer.write_all(b"\x1b[?25h");
    } else {
        let _ = writer.write_all(b"\x1b[?25l");
    }
}

fn write_ime_anchor_cursor_state(writer: &mut impl Write, cursor: HostCursorState) {
    write_cursor_position(writer, cursor.position);
    if cursor.visible {
        let _ = writer.write_all(b"\x1b[?25h");
    } else {
        let _ = writer.write_all(b"\x1b[?25l");
    }
}

#[cfg(windows)]
fn repeat_ime_anchor_after_sync() -> bool {
    false
}

#[cfg(not(windows))]
fn repeat_ime_anchor_after_sync() -> bool {
    true
}

// ---------------------------------------------------------------------------
// 写入器
// ---------------------------------------------------------------------------

/// 帧→字节主路径。`prev=None` 或尺寸变化 = 整屏；否则仅写变化格。
/// 每帧：`?2026h`（原子呈现）→ `?25l`（先藏光标）→ 格子 → 光标定位+显隐+DECSCUSR
/// → `?2026l`（非 Windows 复读 IME 锚点）。
fn blit_frame_to_with_cursor_memory_and_clear_policy(
    mut writer: impl Write,
    frame: &FrameData,
    prev: Option<&FrameData>,
    last_visible_cursor: &mut Option<(u16, u16)>,
    last_cursor_shape: &mut u8,
    repeat_ime_anchor: bool,
    clear_before_full_redraw: bool,
) {
    let full_redraw =
        prev.is_none() || prev.is_some_and(|p| p.width != frame.width || p.height != frame.height);

    let _ = writer.write_all(b"\x1b[?2026h");
    let _ = writer.write_all(b"\x1b[?25l");
    // 每帧从已知 OSC 8 状态开局（防宿主残留超链接）。
    let _ = writer.write_all(b"\x1b]8;;\x1b\\");

    if full_redraw {
        if clear_before_full_redraw {
            let _ = writer.write_all(b"\x1b[2J");
        }
        write_all_cells(&mut writer, frame);
    } else {
        write_changed_cells(&mut writer, frame, prev.unwrap());
    }

    let host_cursor = resolve_host_cursor_state(frame, last_visible_cursor);
    write_host_cursor_state(&mut writer, host_cursor, last_cursor_shape);
    let _ = writer.write_all(b"\x1b[?2026l");
    if repeat_ime_anchor {
        write_ime_anchor_cursor_state(&mut writer, host_cursor);
    }
    let _ = writer.flush();
}

/// 整屏写入所有非 skip 格。
fn write_all_cells(writer: &mut impl Write, frame: &FrameData) {
    let mut last_sgr = String::new();
    for row in 0..frame.height {
        let mut to_skip = 0usize;
        let mut next_inline_col = None;
        for col in 0..frame.width {
            if to_skip > 0 {
                to_skip -= 1;
                continue;
            }
            let idx = (row as usize) * (frame.width as usize) + (col as usize);
            let cell = &frame.cells[idx];
            if cell.skip {
                next_inline_col = None;
                continue;
            }
            let cursor_position = (next_inline_col != Some(col)).then_some((col, row));
            write_cell(writer, cursor_position, cell, &mut last_sgr);
            let width = cell_width(cell);
            next_inline_col =
                (cell.symbol.is_ascii() && width == 1).then_some(col.saturating_add(1));
            to_skip = width.saturating_sub(1);
        }
    }
    let _ = writer.write_all(b"\x1b[0m");
}

/// 仅写变化格。`to_skip`：宽格后跳过其尾格；`invalidated`：宽度变化波及的后续格
/// 一并重画（宽→窄/窄→宽都会失效相邻列）。
fn write_changed_cells(writer: &mut impl Write, frame: &FrameData, prev: &FrameData) {
    let mut last_sgr = String::new();
    for row in 0..frame.height {
        let mut invalidated = 0usize;
        let mut to_skip = 0usize;
        // 宿主关闭自动折行（DisableLineWrap 未启用——本实现不依赖宿主状态，
        // inline 前进只用于已 ASCII 单宽格，不会溢出到相邻行）。
        let mut next_inline_col = None;
        for col in 0..frame.width {
            let idx = (row as usize) * (frame.width as usize) + (col as usize);
            let cell = &frame.cells[idx];
            let prev_cell = &prev.cells[idx];
            if !cell.skip
                && (!cells_visually_equal(cell, prev_cell) || invalidated > 0)
                && to_skip == 0
            {
                let cursor_position =
                    (next_inline_col != Some(col) || invalidated > 0).then_some((col, row));
                write_cell(writer, cursor_position, cell, &mut last_sgr);
                next_inline_col =
                    (cell.symbol.is_ascii() && cell_width(cell) == 1).then_some(col.saturating_add(1));
            }
            to_skip = cell_width(cell).saturating_sub(1);
            let affected_width = cmp::max(cell_width(cell), cell_width(prev_cell));
            invalidated = cmp::max(affected_width, invalidated).saturating_sub(1);
        }
    }
    if !last_sgr.is_empty() {
        let _ = writer.write_all(b"\x1b[0m");
    }
}

/// 单格写出（CUP 可选 + SGR 去重 + 符号本体）。
fn write_cell(
    writer: &mut impl Write,
    cursor_position: Option<(u16, u16)>,
    cell: &CellData,
    last_sgr: &mut String,
) {
    if cell.skip {
        return;
    }
    if let Some(position) = cursor_position {
        write_cursor_position(writer, position);
    }
    let sgr = build_sgr(cell.fg, cell.bg, cell.modifier);
    if sgr != *last_sgr {
        let _ = writer.write_all(sgr.as_bytes());
        *last_sgr = sgr;
    }
    let _ = writer.write_all(cell.symbol.as_bytes());
}

// ---------------------------------------------------------------------------
// BlitBackend：把 ratatui Terminal 的输出面接到本编码器
// ---------------------------------------------------------------------------

use ratatui::backend::{Backend, ClearType, WindowSize};
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect, Size};
use std::io;
use std::sync::{Arc, Mutex};

/// Buffer-backed 后端：维持整屏 Buffer，flush 时转帧 → 差分编码 → 写宿主终端。
/// **输出侧不再经过 ratatui 的 crossterm 后端**（其逐格 Print 的行内游标假设是
/// r5 渲染缺陷的病灶层）；ratatui 只用于离屏合成（面板/终端视图照旧渲染进 Buffer）。
pub(crate) struct BlitBackend {
    buffer: Buffer,
    /// 帧光标槽（渲染闭包内写、flush 读——ratatui 自身的光标回调在本后端为 no-op）。
    cursor: Arc<Mutex<Option<CursorState>>>,
    encoder: BlitEncoder,
    out: io::Stdout,
    repaint: bool,
}

impl BlitBackend {
    pub(crate) fn new() -> Self {
        let (cols, rows) = crossterm::terminal::size().unwrap_or((80, 24));
        BlitBackend {
            buffer: Buffer::empty(Rect::new(0, 0, cols, rows)),
            cursor: Arc::new(Mutex::new(None)),
            encoder: BlitEncoder::new(),
            out: io::stdout(),
            repaint: true,
        }
    }

    /// 与渲染闭包共享的光标槽。
    pub(crate) fn cursor_slot(&self) -> Arc<Mutex<Option<CursorState>>> {
        Arc::clone(&self.cursor)
    }

    fn term_size(&self) -> (u16, u16) {
        crossterm::terminal::size().unwrap_or((80, 24))
    }
}

impl Backend for BlitBackend {
    type Error = io::Error;

    fn draw<'a, I>(&mut self, content: I) -> io::Result<()>
    where
        I: Iterator<Item = (u16, u16, &'a ratatui::buffer::Cell)>,
    {
        for (x, y, cell) in content {
            if let Some(target) = self.buffer.cell_mut((x, y)) {
                *target = cell.clone();
            }
        }
        Ok(())
    }

    fn hide_cursor(&mut self) -> io::Result<()> {
        Ok(())
    }

    fn show_cursor(&mut self) -> io::Result<()> {
        Ok(())
    }

    fn get_cursor_position(&mut self) -> io::Result<Position> {
        let slot = *self.cursor.lock().unwrap_or_else(|e| e.into_inner());
        Ok(slot
            .filter(|c| c.visible)
            .map(|c| Position { x: c.x, y: c.y })
            .unwrap_or(Position { x: 0, y: 0 }))
    }

    fn set_cursor_position<P: Into<Position>>(&mut self, _position: P) -> io::Result<()> {
        // 光标以渲染闭包写入的共享槽为准（见 ui_tick 接线）。
        Ok(())
    }

    fn clear(&mut self) -> io::Result<()> {
        self.buffer.reset();
        self.repaint = true;
        Ok(())
    }

    fn clear_region(&mut self, _clear_type: ClearType) -> io::Result<()> {
        self.buffer.reset();
        Ok(())
    }

    fn append_lines(&mut self, _n: u16) -> io::Result<()> {
        Ok(())
    }

    fn size(&self) -> io::Result<Size> {
        let (cols, rows) = self.term_size();
        Ok(Size::new(cols, rows))
    }

    fn window_size(&mut self) -> io::Result<WindowSize> {
        let (cols, rows) = self.term_size();
        Ok(WindowSize {
            columns_rows: Size::new(cols, rows),
            pixels: Size::new(cols.saturating_mul(8), rows.saturating_mul(16)),
        })
    }

    fn flush(&mut self) -> io::Result<()> {
        // 尺寸变化兜底（Terminal 理应已走 clear，这里再保险一次）。
        let (cols, rows) = self.term_size();
        if cols != self.buffer.area.width || rows != self.buffer.area.height {
            self.buffer = Buffer::empty(Rect::new(0, 0, cols, rows));
            self.repaint = true;
        }
        let cursor = *self.cursor.lock().unwrap_or_else(|e| e.into_inner());
        let frame = FrameData::from_ratatui_buffer(&self.buffer, cursor);
        let encoded = self.encoder.encode(&frame, self.repaint);
        self.repaint = false;
        self.out.write_all(&encoded.bytes)?;
        self.out.flush()?;
        self.encoder.commit(frame, encoded);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// 测试（移植自上游同文件测试组，聚焦行推进/跳位/光标语义）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::frame::{CellData, CursorState, FrameData};

    const WIDE_GRAPHEME: &str = "💡";
    const HALFWIDTH_VOICED_KANA: &str = "ｶ\u{ff9e}";

    fn make_cell(symbol: &str, fg: u32, bg: u32, modifier: u16) -> CellData {
        CellData { symbol: symbol.to_owned(), fg, bg, modifier, skip: false }
    }

    fn make_skip_cell(symbol: &str, fg: u32, bg: u32, modifier: u16) -> CellData {
        let mut cell = make_cell(symbol, fg, bg, modifier);
        cell.skip = true;
        cell
    }

    fn make_frame(width: u16, height: u16, cells: Vec<CellData>) -> FrameData {
        FrameData { cells, width, height, cursor: None }
    }

    fn text_cell(ch: char) -> CellData {
        make_cell(&ch.to_string(), 0, 0, 0)
    }

    fn blit_full(frame: &FrameData) -> String {
        String::from_utf8(BlitEncoder::new().encode(frame, true).bytes).unwrap()
    }

    #[test]
    fn full_frame_writes_wide_glyph_once_and_skips_its_tail() {
        // 💡 宽字符：头格写符号；尾格 skip 不写；后续 ASCII 以 CUP 精确落位。
        let mut cells = vec![text_cell('a')];
        cells.push(make_cell(WIDE_GRAPHEME, 0, 0, 0));
        cells.push(make_skip_cell("", 0, 0, 0));
        cells.push(text_cell('b'));
        let frame = make_frame(4, 1, cells);
        let out = blit_full(&frame);
        assert!(out.contains(WIDE_GRAPHEME), "宽字形本体写出：{out:?}");
        // 头格 +1 列后尾格被 to_skip 吞掉，b 应落在列 3（CUP 4）。
        let b_pos = out.find('b').expect("b 写出");
        let cup = out[..b_pos].rfind("\x1b[").expect("b 前有 CUP");
        assert!(out[cup..b_pos].contains(";4H"), "b 精确落在列 3（1 基 4）：{out:?}");
    }

    #[test]
    fn halfwidth_katakana_voiced_takes_two_columns() {
        assert_eq!(cell_width(&make_cell(HALFWIDTH_VOICED_KANA, 0, 0, 0)), 2);
        assert_eq!(cell_width(&make_cell("a", 0, 0, 0)), 1);
        assert_eq!(cell_width(&make_cell(WIDE_GRAPHEME, 0, 0, 0)), 2);
    }

    #[test]
    fn diff_only_repaints_changed_cells() {
        let frame1 = make_frame(4, 1, vec![text_cell('a'), text_cell('b'), text_cell('c'), text_cell('d')]);
        let mut frame2 = frame1.clone();
        frame2.cells[2] = text_cell('X');
        let mut enc = BlitEncoder::new();
        let first = enc.encode(&frame1, false);
        assert!(first.full, "首帧整屏");
        enc.commit(frame1.clone(), first);
        let second = enc.encode(&frame2, false);
        assert!(!second.full, "差分帧非整屏");
        let out = String::from_utf8(second.bytes.clone()).unwrap();
        assert!(out.contains('X'), "变化格写出");
        assert!(!out.contains('b'), "未变格不重写：{out:?}");
        enc.commit(frame2.clone(), second);
        assert!(enc.is_current(&frame2));
    }

    #[test]
    fn sgr_reset_prefix_and_underline_style_bits() {
        let mut cell = make_cell("x", 0x00_00_00_02, 0x01_00_00_00 | 244, 1 << 3);
        cell.modifier |= 3 << super::super::frame::UNDERLINE_STYLE_SHIFT; // 波浪线
        let sgr = build_sgr(cell.fg, cell.bg, cell.modifier);
        assert!(sgr.starts_with("\x1b[0;"), "SGR 恒以 0 复位开头：{sgr:?}");
        assert!(sgr.contains("4:3"), "波浪下划线：{sgr:?}");
        assert!(sgr.contains("31"), "红色前景：{sgr:?}");
    }

    #[test]
    fn cursor_hidden_parks_at_last_visible_position() {
        let frame = make_frame(10, 3, vec![text_cell(' '); 30]);
        let mut last = None;
        let visible = resolve_host_cursor_state(
            &FrameData { cursor: Some(CursorState { x: 2, y: 1, visible: true, shape: 1 }), ..frame.clone() },
            &mut last,
        );
        assert!(visible.visible && visible.position == (2, 1));
        let hidden = resolve_host_cursor_state(&frame, &mut last);
        assert!(!hidden.visible, "无光标帧应隐藏");
        assert_eq!(hidden.position, (2, 1), "停在最后可见位（IME 锚点）");
        assert_eq!(hidden.shape, 0, "隐藏帧形状回默认");
    }

    #[test]
    fn size_change_forces_full_redraw_without_clear() {
        let f1 = make_frame(2, 1, vec![text_cell('a'), text_cell('b')]);
        let f2 = make_frame(3, 1, vec![text_cell('x'), text_cell('y'), text_cell('z')]);
        let mut enc = BlitEncoder::new();
        let e1 = enc.encode(&f1, false);
        enc.commit(f1, e1);
        let e2 = enc.encode(&f2, false);
        assert!(e2.full, "尺寸变化 = 整屏");
        let out = String::from_utf8(e2.bytes).unwrap();
        assert!(out.contains('x') && out.contains('y') && out.contains('z'));
        assert!(!out.contains("\x1b[2J"), "非首帧不带清屏（尺寸变化平铺覆盖即可）");
    }

    #[test]
    fn encoder_syncs_output_and_hides_cursor_first() {
        let frame = make_frame(1, 1, vec![text_cell('q')]);
        let out = blit_full(&frame);
        let sync_on = out.find("\x1b[?2026h").expect("同步输出开启");
        let hide = out.find("\x1b[?25l").expect("先隐藏光标");
        let glyph = out.find('q').expect("字形");
        assert!(sync_on < hide && hide < glyph, "帧序：2026h → 25l → 内容：{out:?}");
        assert!(out.contains("\x1b[?2026l"), "同步输出关闭");
    }
}
