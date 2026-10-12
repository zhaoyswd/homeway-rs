//! 帧模型（移植自 herdr `src/protocol/wire.rs`，Apache-2.0，见 docs/reviews/
//! TUI-terminal-stack-transplant-assessment.md）：把 ratatui `Buffer` 转成独立的
//! 单元格帧，交由 [`super::blit::BlitEncoder`] 自行差分/编码——**输出侧不再经过
//! ratatui 的 crossterm 后端**（其行内游标假设是 r5 渲染缺陷的病灶层）。
//!
//! v1 裁剪：hyperlinks（OSC 8）与 kitty graphics 未移植——我们的流水线不产生这两类
//! 效果；将来需要时按上游补齐。

#![allow(dead_code)]

use ratatui::style::{Color, Modifier};

/// 光标形状（DECSCUSR 参数）：0=默认 1=闪烁块 2=常亮块 3=闪烁下划线 4=常亮下划线
/// 5=闪烁竖线 6=常亮竖线。
pub(crate) type CursorShapeParam = u8;

/// 光标状态（帧内）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct CursorState {
    pub x: u16,
    pub y: u16,
    pub visible: bool,
    pub shape: CursorShapeParam,
}

/// 单个单元格（独立于 ratatui `Cell` 的帧表示）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CellData {
    /// 显示的字素簇（通常 1–2 个字符；宽字符本体两列宽、尾格另占一格）。
    pub symbol: String,
    /// 前景色打包：0xAANNVVVV 族（见 [`color_to_u32`]）。
    pub fg: u32,
    /// 背景色打包。
    pub bg: u32,
    /// ratatui Modifier 位掩码（含赫德自加的下划线样式高位）。
    pub modifier: u16,
    /// 差分跳过位（宽字符尾/占位——不打印）。
    pub skip: bool,
}

impl CellData {
    pub(crate) fn from_ratatui_cell(cell: &ratatui::buffer::Cell) -> Self {
        Self {
            symbol: cell.symbol().to_owned(),
            fg: color_to_u32(cell.fg),
            bg: color_to_u32(cell.bg),
            modifier: modifier_to_u16(cell.modifier),
            skip: matches!(cell.diff_option, ratatui::buffer::CellDiffOption::Skip),
        }
    }
}

/// 一帧（行优先）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FrameData {
    pub cells: Vec<CellData>,
    pub width: u16,
    pub height: u16,
    pub cursor: Option<CursorState>,
}

impl FrameData {
    /// 由 ratatui `Buffer` + 可选光标构造帧。
    pub(crate) fn from_ratatui_buffer(
        buffer: &ratatui::buffer::Buffer,
        cursor: Option<CursorState>,
    ) -> Self {
        let area = buffer.area;
        let mut cells = Vec::with_capacity((area.width as usize) * (area.height as usize));
        for row in 0..area.height {
            for col in 0..area.width {
                let cell = buffer.cell((col, row)).expect("cell within bounds");
                cells.push(CellData::from_ratatui_cell(cell));
            }
        }
        FrameData { cells, width: area.width, height: area.height, cursor }
    }
}

/// ratatui 颜色 → 打包 u32（0x00_00_00_VV 命名 / 0x01_00_00_II 索引 / 0x02_RRGGBB）。
pub(crate) fn color_to_u32(color: Color) -> u32 {
    match color {
        Color::Reset => 0x00_00_00_00,
        Color::Black => 0x00_00_00_01,
        Color::Red => 0x00_00_00_02,
        Color::Green => 0x00_00_00_03,
        Color::Yellow => 0x00_00_00_04,
        Color::Blue => 0x00_00_00_05,
        Color::Magenta => 0x00_00_00_06,
        Color::Cyan => 0x00_00_00_07,
        Color::Gray => 0x00_00_00_08,
        Color::DarkGray => 0x00_00_00_09,
        Color::LightRed => 0x00_00_00_0A,
        Color::LightGreen => 0x00_00_00_0B,
        Color::LightYellow => 0x00_00_00_0C,
        Color::LightBlue => 0x00_00_00_0D,
        Color::LightMagenta => 0x00_00_00_0E,
        Color::LightCyan => 0x00_00_00_0F,
        Color::White => 0x00_00_00_10,
        Color::Indexed(i) => 0x01_00_00_00 | (i as u32),
        Color::Rgb(r, g, b) => 0x02_00_00_00 | ((r as u32) << 16) | ((g as u32) << 8) | (b as u32),
    }
}

pub(crate) const UNDERLINE_STYLE_SHIFT: u16 = 12;
pub(crate) const UNDERLINE_STYLE_MASK: u16 = 0xF000;

/// ratatui Modifier → u16 位掩码。
pub(crate) fn modifier_to_u16(modifier: Modifier) -> u16 {
    modifier.bits()
}

/// 取扩展的下划线样式（0=无 1..5=单/双/波浪/点/虚线——值域与 ghostty SGR 同序）。
pub(crate) fn underline_style_from_modifier(modifier: u16) -> u8 {
    ((modifier & UNDERLINE_STYLE_MASK) >> UNDERLINE_STYLE_SHIFT) as u8
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;

    #[test]
    fn from_buffer_roundtrip_basic() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 4, 2));
        buf.set_string(0, 0, "ab", ratatui::style::Style::new().fg(Color::Rgb(1, 2, 3)));
        buf.set_string(2, 0, "世", ratatui::style::Style::new());
        let cursor = CursorState { x: 3, y: 1, visible: true, shape: 1 };
        let frame = FrameData::from_ratatui_buffer(&buf, Some(cursor.clone()));
        assert_eq!(frame.width, 4);
        assert_eq!(frame.height, 2);
        assert_eq!(frame.cells.len(), 8);
        assert_eq!(frame.cells[0].symbol, "a");
        assert_eq!(frame.cells[0].fg, 0x02010203);
        assert_eq!(frame.cells[2].symbol, "世");
        assert_eq!(frame.cursor, Some(cursor));
    }

    #[test]
    fn color_packing_named_indexed_rgb() {
        assert_eq!(color_to_u32(Color::Reset), 0);
        assert_eq!(color_to_u32(Color::Red), 0x02);
        assert_eq!(color_to_u32(Color::Indexed(200)), 0x01_00_00_00 | 200);
        assert_eq!(color_to_u32(Color::Rgb(10, 20, 30)), 0x020A141E);
    }

    #[test]
    fn underline_style_bits_roundtrip() {
        let with_style = modifier_to_u16(Modifier::BOLD) | (3u16 << UNDERLINE_STYLE_SHIFT);
        assert_eq!(underline_style_from_modifier(with_style), 3);
        assert_eq!(underline_style_from_modifier(modifier_to_u16(Modifier::BOLD)), 0);
    }
}
