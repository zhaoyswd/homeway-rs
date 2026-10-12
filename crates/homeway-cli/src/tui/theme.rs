//! 面板主题 token（r5 UI 批：向 herdr 对齐——Catppuccin 暗色系）。
//!
//! 色值 = herdr `Palette::catppuccin()`（`herdr src/app/state.rs`）同源：
//! 文本层级 text/subtext0/overlay0，表面层级 selection_bg/active_row/surface，
//! 语义色 accent(蓝)/green/yellow/red/mauve。终端无字号，层级用 BOLD/DIM。

#![allow(dead_code)]

use ratatui::style::{Color, Modifier, Style};

pub(crate) const TEXT: Color = Color::Rgb(205, 214, 244);
pub(crate) const SUBTEXT0: Color = Color::Rgb(166, 173, 200);
pub(crate) const OVERLAY0: Color = Color::Rgb(108, 112, 134);
pub(crate) const OVERLAY1: Color = Color::Rgb(127, 132, 156);
pub(crate) const SELECTION_BG: Color = Color::Rgb(49, 50, 68);
pub(crate) const ACTIVE_ROW_BG: Color = Color::Rgb(30, 30, 46);
pub(crate) const SURFACE_DIM: Color = Color::Rgb(30, 30, 46);
pub(crate) const SURFACE1: Color = Color::Rgb(69, 71, 90);
pub(crate) const ACCENT: Color = Color::Rgb(137, 180, 250);
pub(crate) const GREEN: Color = Color::Rgb(166, 227, 161);
pub(crate) const YELLOW: Color = Color::Rgb(249, 226, 175);
pub(crate) const RED: Color = Color::Rgb(243, 139, 168);
pub(crate) const MAUVE: Color = Color::Rgb(203, 166, 247);

/// 区块小标题（` spaces` 风）：overlay0 + BOLD。
pub(crate) fn section_header() -> Style {
    Style::new().fg(OVERLAY0).add_modifier(Modifier::BOLD)
}

/// 次级/装饰文字（副标题、分隔、按钮）。
pub(crate) fn muted() -> Style {
    Style::new().fg(OVERLAY0)
}

/// 空态/占位提示。
pub(crate) fn placeholder() -> Style {
    Style::new().fg(OVERLAY0).add_modifier(Modifier::DIM)
}

/// 条目主名（未选中）：subtext0 + BOLD（herdr agent 行同名次）。
pub(crate) fn entry_name(selected: bool) -> Style {
    Style::new()
        .fg(if selected { TEXT } else { SUBTEXT0 })
        .add_modifier(Modifier::BOLD)
}

/// 选中行整行底色（herdr selection_bg——**非反白**）。
pub(crate) fn selection() -> Style {
    Style::new().bg(SELECTION_BG)
}

/// 分隔线（`─`）：surface_dim。
pub(crate) fn divider() -> Style {
    Style::new().fg(SURFACE_DIM)
}

/// 状态行文字。
pub(crate) fn status_text() -> Style {
    Style::new().fg(SUBTEXT0)
}

/// 终端 pane 标题：聚焦 = accent + BOLD（herdr pane border title 同款）。
pub(crate) fn pane_title(focused: bool) -> Style {
    if focused {
        Style::new().fg(ACCENT).add_modifier(Modifier::BOLD)
    } else {
        Style::new().fg(OVERLAY0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palette_matches_herdr_catppuccin_tokens() {
        // 与 herdr src/app/state.rs Palette::catppuccin() 同源钉。
        assert_eq!(TEXT, Color::Rgb(205, 214, 244));
        assert_eq!(SELECTION_BG, Color::Rgb(49, 50, 68));
        assert_eq!(ACCENT, Color::Rgb(137, 180, 250));
        assert_eq!(GREEN, Color::Rgb(166, 227, 161));
    }
}
