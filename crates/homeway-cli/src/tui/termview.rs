//! 终端视图渲染（tasks 4.4）：`SessionVt` 网格 → ratatui pane 映射。
//!
//! - **宽字符**：Cell 显式宽度位直读——width=2 渲染符号本体，width=0 的尾占位格
//!   落空格（不重画，避免残影）；
//! - **裁剪**：pane 小于会话网格 ⇒ 左上对齐裁剪（会话尺寸跟随交互实例的 RESIZE
//!   选举，旁观态下裁剪是预期形态，design D2）；
//! - **颜色/属性**：Palette→Indexed、Rgb→Rgb；attr 位 → Modifier（BOLD/ITALIC/
//!   INVERSE/FAINT/INVISIBLE/STRIKETHROUGH/UNDERLINE）；
//! - **回看**：SessionVt 无滚动偏移面（display_offset 恒 0，`vt.rs:49` 注记）——
//!   滚轮回看需要 core 增量（tasks 4.8 已登记，含 design D7 探针先例的公开面
//!   路径）；本批渲染可见视口。
//!
//! 消费方 = app 主循环（runtime，任务组 4b）——期间豁免 dead_code。

#![allow(dead_code)]

use ratatui::buffer::Cell as RCell;
use ratatui::layout::Rect;
use ratatui::style::{Color as RColor, Modifier as RModifier, Style};

use homeway_core::term::vt::Color as HColor;

use super::leg::TermLeg;

/// 终端视图句柄：UI 持有腿处理器（字节经通道由 UI 侧喂入——合帧渲染）。
pub(crate) struct TermView {
    pub leg: TermLeg,
    /// 进入终端焦点后是否已发生用户交互（交互实例的 RESIZE 门，r2 N-4）。
    pub interacted: bool,
}

impl TermView {
    pub fn new(cols: u16, rows: u16) -> Result<Self, homeway_core::term::vt::VtError> {
        Ok(TermView { leg: TermLeg::new(cols, rows)?, interacted: false })
    }

    /// 渲染到 pane 区域（每渲染拍调用；rows_and_commit 同时清脏标）。
    pub fn render(&mut self, f: &mut ratatui::Frame, area: Rect) {
        let rows = if self.leg.display_offset() == 0 {
            self.leg.rows_and_commit()
        } else {
            self.leg.rows_at_offset()
        };
        let buf = f.buffer_mut();
        for (ry, row) in rows.iter().enumerate() {
            if ry as u16 >= area.height {
                break;
            }
            let y = area.y + ry as u16;
            let mut x = area.x;
            for cell in &row.cells {
                if x >= area.x + area.width {
                    break;
                }
                let rc = buf.cell_mut((x, y)).expect("pane 内坐标");
                apply_cell(rc, cell);
                if cell.width == 2 {
                    // 宽字符占两格：下一格由 width=0 占位落空格（此处直接推进）。
                    x += 1;
                    if x < area.x + area.width {
                        if let Some(rc) = buf.cell_mut((x, y)) {
                            // 尾占位格置空串：拼接文本不受残影污染（显示面为空白）。
                            rc.set_symbol("");
                            rc.set_style(Style::new().bg(cell_bg(&cell.bg)));
                        }
                    }
                }
                x += 1;
            }
        }
    }
}

fn apply_cell(rc: &mut RCell, cell: &homeway_core::term::vt::Cell) {
    if cell.symbol.is_empty() || cell.width == 0 {
        rc.set_symbol("");
    } else {
        rc.set_symbol(cell.symbol.as_str());
    }
    rc.set_style(Style::new().fg(cell_fg(&cell.fg)).bg(cell_bg(&cell.bg)).add_modifier(attr_mod(cell.attr)));
}

fn cell_fg(c: &HColor) -> RColor {
    match c {
        HColor::None => RColor::Reset,
        HColor::Palette(i) => RColor::Indexed(*i),
        HColor::Rgb(r, g, b) => RColor::Rgb(*r, *g, *b),
    }
}

fn cell_bg(c: &HColor) -> RColor {
    cell_fg(c)
}

fn attr_mod(attr: u16) -> RModifier {
    use homeway_core::term::vt::attr as ha;
    use homeway_core::term::vt::attr::UNDERLINE_MASK;
    let mut m = RModifier::empty();
    if attr & ha::BOLD != 0 {
        m |= RModifier::BOLD;
    }
    if attr & ha::ITALIC != 0 {
        m |= RModifier::ITALIC;
    }
    if attr & ha::INVERSE != 0 {
        m |= RModifier::REVERSED;
    }
    if attr & ha::FAINT != 0 {
        m |= RModifier::DIM;
    }
    if attr & ha::INVISIBLE != 0 {
        m |= RModifier::HIDDEN;
    }
    if attr & ha::STRIKETHROUGH != 0 {
        m |= RModifier::CROSSED_OUT;
    }
    if attr & UNDERLINE_MASK != 0 {
        m |= RModifier::UNDERLINED;
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn line_text(t: &mut Terminal<TestBackend>, y: u16, width: u16) -> String {
        let b = t.backend().buffer();
        (0..width)
            .map(|x| b[(x, y)].symbol().to_owned())
            .collect::<Vec<_>>()
            .concat()
            .trim_end()
            .to_owned()
    }

    #[test]
    fn grid_maps_text_and_wide_chars() {
        let mut tv = TermView::new(40, 10).expect("建视图");
        // 窄字符 + 宽字符 + 颜色（红字）
        tv.leg.on_frame(&homeway_core::term::frames::Frame {
            op: homeway_core::term::frames::Op::DATA,
            payload: b"\x1b[2J\x1b[Hhi \x1b[31m\xe4\xb8\x96\x1b[0m\xe7\x95\x8c".to_vec(),
        });
        let mut t = Terminal::new(TestBackend::new(40, 10)).unwrap();
        let area = Rect::new(0, 0, 40, 10);
        t.draw(|f| tv.render(f, area)).unwrap();
        let b = t.backend().buffer();
        assert_eq!(b[(3, 0)].symbol(), "世", "宽字形头格在原网格列");
        let flat: String = (0..40)
            .map(|x| b[(x, 0)].symbol().to_owned())
            .collect::<Vec<_>>()
            .concat()
            .replace(' ', "");
        assert!(flat.contains("hi世界"), "窄+宽字符映射（宽尾列由终端持有，拼接按列）：{flat}");
    }

    #[test]
    fn pane_smaller_than_grid_clips() {
        let mut tv = TermView::new(40, 10).expect("建视图");
        tv.leg.on_frame(&homeway_core::term::frames::Frame {
            op: homeway_core::term::frames::Op::DATA,
            payload: b"\x1b[2J\x1b[HABCDEFGH".to_vec(),
        });
        let mut t = Terminal::new(TestBackend::new(40, 10)).unwrap();
        let area = Rect::new(0, 0, 4, 1); // 只看得到 4 列
        t.draw(|f| tv.render(f, area)).unwrap();
        assert_eq!(line_text(&mut t, 0, 40), "ABCD", "pane 小于网格 = 左上裁剪");
    }
}
