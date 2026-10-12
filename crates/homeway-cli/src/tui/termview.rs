//! 终端视图渲染（tasks 4.4）：`SessionVt` 网格 → ratatui pane 映射。
//!
//! - **宽字符**：Cell 显式宽度位直读——width=2 渲染符号本体，width=0 的尾占位格
//!   落空格（不重画，避免残影）；
//! - **空格契约**：VT「无文本 = symbol 空串」（vt.rs:171）——空串在真终端是
//!   Print no-op，必须落**真实空格**并先整区铺底（Cell::reset），否则切会话/
//!   清屏时旧字符残留（r5 验收实测：切换会话多会话内容叠加）；
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
    /// 最近一次下发的 RESIZE 尺寸（跟随窗口变化的判据，r5 实录：面板焦点期间
    /// 改窗口 → 回同一会话后 interacted=true 且世代未变 ⇒ 不重发 ⇒ 会话几何冻结）。
    pub last_sent: Option<(u16, u16)>,
    /// 最后注入会话的焦点态（r4 高3 去重：面板切换与外层事件共用；None = 未知/未启用）。
    pub session_focus: Option<bool>,
}

impl TermView {
    pub fn new(cols: u16, rows: u16) -> Result<Self, homeway_core::term::vt::VtError> {
        Ok(TermView {
            leg: TermLeg::new(cols, rows)?,
            interacted: false,
            last_sent: None,
            session_focus: None,
        })
    }

    /// 渲染到 pane 区域（每渲染拍调用；rows_and_commit 同时清脏标）。
    pub fn render(&mut self, f: &mut ratatui::Frame, area: Rect) {
        // 先整区铺真空格（Cell::reset = 空格 + 缺省样式）：VT 契约「无文本 =
        // symbol 空串」，而真终端对空串 cell 的 Print 是 no-op——空串格直接落
        // 缓冲时旧字符（上一会话/上一帧）会原样残留（r5 验收实测：切换会话
        // 多会话内容叠加）。铺底同时清掉网格小于 pane 的右侧/下侧带区。
        let buf = f.buffer_mut();
        for y in area.top()..area.bottom() {
            for x in area.left()..area.right() {
                if let Some(rc) = buf.cell_mut((x, y)) {
                    rc.reset();
                }
            }
        }
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

/// 终端焦点的宿主光标定位：VT 光标 → pane 绝对坐标（出界夹取回 pane 内）。
/// 差分渲染下宿主光标若不显式定位，会停在「最后一个被打印的格子」这种无语义
/// 位置——herdr 的 host cursor 同款实践（r5 验收：光标位置观感混乱）。
pub(crate) fn cursor_in_pane(c: homeway_core::term::vt::Cursor, area: Rect) -> Option<(u16, u16)> {
    if area.width == 0 || area.height == 0 {
        return None;
    }
    Some((
        area.x + c.x.min(area.width - 1),
        area.y + c.y.min(area.height - 1),
    ))
}

fn apply_cell(rc: &mut RCell, cell: &homeway_core::term::vt::Cell) {    if cell.width == 0 {
        // 尾占位格不打印（真终端自持宽字形两列——打印会打碎字形的右半列）。
        rc.set_symbol("");
    } else if cell.symbol.is_empty() {
        // 无文本契约（vt.rs:171）= 空串 ⇒ 必须落**真实空格**：空串在真终端是
        // no-op，清屏/切换会话的空白区都靠它把旧字符盖掉（背景色仍按 cell 带）。
        rc.set_symbol(" ");
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

    #[test]
    fn cursor_in_pane_clamps_to_area() {
        let area = Rect::new(10, 5, 40, 10);
        let mk = |x: u16, y: u16| homeway_core::term::vt::Cursor {
            x,
            y,
            visible: true,
            blinking: false,
            password: false,
            wide_tail: false,
            shape: homeway_core::term::vt::CursorShape::default(),
        };
        assert_eq!(cursor_in_pane(mk(3, 2), area), Some((13, 7)), "界内直映");
        assert_eq!(cursor_in_pane(mk(100, 50), area), Some((49, 14)), "出界夹取回 pane 右下");
        assert_eq!(cursor_in_pane(mk(0, 0), Rect::new(0, 0, 0, 5)), None, "零宽 area 无定位");
    }

    #[test]
    fn session_switch_clears_previous_content() {
        // r5 验收实录：切换会话后旧会话内容透出——空串格在真终端 Print no-op +
        // 网格小于 pane 的带区无人写。缓冲面契约：新会话空白区 = 真实空格，
        // 带区随铺底清空；宽字符尾占位格（width=0）仍落空串（不打印）。
        let mut t = Terminal::new(TestBackend::new(40, 10)).unwrap();
        let area = Rect::new(0, 0, 40, 10);
        // 会话 A：满屏 'O'
        let mut a = TermView::new(40, 10).expect("建视图");
        let mut full = b"\x1b[2J\x1b[H".to_vec();
        full.extend(vec![b'O'; 40 * 10]);
        a.leg.on_frame(&homeway_core::term::frames::Frame {
            op: homeway_core::term::frames::Op::DATA,
            payload: full,
        });
        t.draw(|f| a.render(f, area)).unwrap();
        // 切到会话 B：全新 VT（runtime 换代语义），20×5 网格只有左上 'B'，其余全空
        let mut b = TermView::new(20, 5).expect("建视图");
        b.leg.on_frame(&homeway_core::term::frames::Frame {
            op: homeway_core::term::frames::Op::DATA,
            payload: b"\x1b[2J\x1b[HB".to_vec(),
        });
        t.draw(|f| b.render(f, area)).unwrap();
        let buf = t.backend().buffer();
        assert_eq!(buf[(0, 0)].symbol(), "B", "新会话内容在位");
        for y in 0..10u16 {
            for x in 0..40u16 {
                if (x, y) == (0, 0) {
                    continue;
                }
                let s = buf[(x, y)].symbol();
                assert_eq!(s, " ", "({x},{y}) 残留 {s:?}——旧会话字符或空串 no-op 未清");
            }
        }
    }
}
