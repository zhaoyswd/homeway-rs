//! A local selection owns one displayed pane; it never sends terminal input.
use std::borrow::Cow;

use homeway_core::term::vt;

use super::view::TerminalView;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Identity {
    pub revision: u32,
    pub epoch: u64,
    pub base: u64,
    pub cols: u16,
    pub rows: u16,
    pub alternate: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Point {
    pub column: u16,
    pub row: u16,
}

/// Pane-local physical cells; the column interval is half-open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Span {
    pub row: u16,
    pub start: u16,
    pub end: u16,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub(super) enum Error {
    #[error("当前画面没有可选择的内容")]
    Unavailable,
    #[error("选区经过尚未加载的历史行，请等待画面加载后重试")]
    MissingRow,
    #[error("选区超过剪贴板的 {limit} 字节上限，未复制")]
    TooLarge { limit: usize },
}

/// The same clipped material is displayed and copied until the gesture ends.
pub(super) struct Snapshot {
    identity: Identity,
    rows: Vec<vt::Row>,
    width: u16,
    top: u64,
    scrollbar: vt::Scrollbar,
    mouse_reporting: bool,
}

impl Snapshot {
    pub(super) fn capture(
        identity: Identity,
        view: TerminalView<'_>,
        width: u16,
        height: u16,
    ) -> Option<Self> {
        let width = width.min(identity.cols);
        if width == 0 || height == 0 || view.rows.is_empty() {
            return None;
        }
        let mut rows = Vec::with_capacity(view.rows.len().min(usize::from(height)));
        for row in view.rows.iter().take(usize::from(height)) {
            // Empty cells identify unavailable FETCH material, not a blank line.
            if !row.cells.is_empty() && row.cells.len() < usize::from(width) {
                return None;
            }
            let mut row = vt::Row {
                y: row.y,
                dirty: row.dirty,
                wraps: row.wraps,
                cells: row.cells.iter().take(usize::from(width)).cloned().collect(),
            };
            // The renderer projects a wide head cropped by the pane as a space.
            // Capture that visible space so copying cannot reveal its hidden half.
            if let Some(last) = row.cells.last_mut() {
                if last.width == 2 && !last.skip {
                    last.symbol.clear();
                    last.width = 1;
                }
            }
            rows.push(row);
        }
        Some(Self {
            identity,
            rows,
            width,
            top: view.top,
            scrollbar: view.scrollbar,
            mouse_reporting: view.mouse_reporting,
        })
    }

    pub(super) fn identity(&self) -> Identity {
        self.identity
    }

    pub(super) fn view(&self) -> TerminalView<'_> {
        TerminalView {
            rows: &self.rows,
            cursor: None,
            mouse_reporting: self.mouse_reporting,
            scrollbar: self.scrollbar,
            top: self.top,
        }
    }

    fn point(&self, point: Point) -> Result<Point, Error> {
        let row = usize::from(point.row).min(self.rows.len().saturating_sub(1));
        let material = self.rows.get(row).ok_or(Error::Unavailable)?;
        if material.cells.is_empty() {
            return Err(Error::MissingRow);
        }
        Ok(Point {
            column: point.column.min(self.width - 1),
            row: row as u16,
        })
    }

    fn cell_bounds(&self, point: Point) -> (u16, u16) {
        let cells = &self.rows[usize::from(point.row)].cells;
        let column = usize::from(point.column);
        let cell = &cells[column];
        if (cell.skip || cell.width == 0)
            && column > 0
            && cells[column - 1].width == 2
            && !cells[column - 1].skip
        {
            (point.column - 1, point.column + 1)
        } else {
            (
                point.column,
                point.column + if cell.width == 2 { 2 } else { 1 },
            )
        }
    }
}

pub(super) struct Selection {
    snapshot: Snapshot,
    anchor: Point,
    end: Point,
    moved: bool,
}

impl Selection {
    pub(super) fn begin(snapshot: Snapshot, point: Point) -> Result<Self, Error> {
        let point = snapshot.point(point)?;
        Ok(Self {
            snapshot,
            anchor: point,
            end: point,
            moved: false,
        })
    }

    pub(super) fn snapshot(&self) -> &Snapshot {
        &self.snapshot
    }

    pub(super) fn update(&mut self, point: Point) -> Result<(), Error> {
        let point = self.snapshot.point(point)?;
        let start = self.anchor.row.min(point.row);
        let end = self.anchor.row.max(point.row);
        if self.snapshot.rows[usize::from(start)..=usize::from(end)]
            .iter()
            .any(|row| row.cells.is_empty())
        {
            return Err(Error::MissingRow);
        }
        self.moved |= point != self.anchor;
        self.end = point;
        Ok(())
    }

    pub(super) fn spans(&self) -> Vec<Span> {
        if !self.moved {
            return Vec::new();
        }
        let (first, last) =
            if (self.anchor.row, self.anchor.column) <= (self.end.row, self.end.column) {
                (self.anchor, self.end)
            } else {
                (self.end, self.anchor)
            };
        let start = self.snapshot.cell_bounds(first).0;
        let end = self.snapshot.cell_bounds(last).1;
        (first.row..=last.row)
            .map(|row| Span {
                row,
                start: if row == first.row { start } else { 0 },
                end: if row == last.row {
                    end
                } else {
                    self.snapshot.width
                },
            })
            .collect()
    }

    pub(super) fn text(&self, max_bytes: usize) -> Result<Option<String>, Error> {
        let spans = self.spans();
        if spans.is_empty() {
            return Ok(None);
        }
        let mut result = String::new();
        for (index, span) in spans.iter().enumerate() {
            let row = &self.snapshot.rows[usize::from(span.row)];
            if row.cells.is_empty() {
                return Err(Error::MissingRow);
            }
            let mut line = String::new();
            for cell in &row.cells[usize::from(span.start)..usize::from(span.end)] {
                if cell.skip || cell.width == 0 {
                    continue;
                }
                if cell.attr & vt::attr::INVISIBLE != 0 {
                    for _ in 0..cell.width {
                        line.push(' ');
                    }
                } else {
                    line.push_str(projected_symbol(&cell.symbol).as_ref());
                }
            }
            // Joining a cropped row would silently omit its invisible right side.
            let soft = row.wraps && self.snapshot.width == self.snapshot.identity.cols;
            let line = if soft {
                line.as_str()
            } else {
                line.trim_end_matches(' ')
            };
            append(&mut result, line, max_bytes)?;
            if index + 1 < spans.len() && !soft {
                append(&mut result, "\n", max_bytes)?;
            }
        }
        Ok((!result.is_empty()).then_some(result))
    }
}

fn append(output: &mut String, value: &str, limit: usize) -> Result<(), Error> {
    if output.len().saturating_add(value.len()) > limit {
        return Err(Error::TooLarge { limit });
    }
    output.push_str(value);
    Ok(())
}

/// VT TAB already occupies physical columns. Project controls as visible spaces
/// while retaining combining marks; source cells and terminal bytes are untouched.
pub(super) fn projected_symbol(symbol: &str) -> Cow<'_, str> {
    if symbol.is_empty() || symbol.chars().all(char::is_control) {
        return Cow::Borrowed(" ");
    }
    if !symbol.chars().any(char::is_control) {
        return Cow::Borrowed(symbol);
    }
    let mut visible: String = symbol.chars().filter(|c| !c.is_control()).collect();
    if symbol.chars().next().is_some_and(char::is_control) {
        visible.insert(0, ' ');
    }
    Cow::Owned(visible)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn capture(rows: &[vt::Row], cols: u16, width: u16) -> Snapshot {
        Snapshot::capture(
            Identity {
                revision: 1,
                epoch: 2,
                base: 0,
                cols,
                rows: rows.len() as u16,
                alternate: false,
            },
            TerminalView {
                rows,
                cursor: None,
                mouse_reporting: false,
                scrollbar: vt::Scrollbar {
                    total: rows.len() as u64,
                    offset: 0,
                    len: rows.len() as u64,
                },
                top: 0,
            },
            width,
            rows.len() as u16,
        )
        .unwrap()
    }

    fn row(text: &str, wraps: bool) -> vt::Row {
        vt::Row {
            wraps,
            cells: text
                .chars()
                .map(|ch| vt::Cell {
                    symbol: if ch == ' ' {
                        String::new()
                    } else {
                        ch.to_string()
                    },
                    width: 1,
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        }
    }

    #[test]
    fn click_is_empty_and_reverse_drag_is_linear_with_hard_and_soft_breaks() {
        let rows = [row("ab  ", true), row("cd  ", false), row("efgh", false)];
        let mut selection =
            Selection::begin(capture(&rows, 4, 4), Point { column: 1, row: 2 }).unwrap();
        assert!(selection.spans().is_empty());
        assert_eq!(selection.text(1024).unwrap(), None);
        selection.update(Point { column: 1, row: 0 }).unwrap();
        assert_eq!(
            selection.spans(),
            vec![
                Span {
                    row: 0,
                    start: 1,
                    end: 4
                },
                Span {
                    row: 1,
                    start: 0,
                    end: 4
                },
                Span {
                    row: 2,
                    start: 0,
                    end: 2
                },
            ]
        );
        assert_eq!(selection.text(1024).unwrap().as_deref(), Some("b  cd\nef"));
        assert_eq!(selection.text(3), Err(Error::TooLarge { limit: 3 }));
    }

    #[test]
    fn wide_tail_combining_controls_hidden_and_right_crop_use_visible_material() {
        let mut vt = vt::SessionVt::new(12, 3, 20).unwrap();
        vt.write("中e\u{301}\tZ\r\n\x1b[8mSECRET\x1b[0m ok".as_bytes());
        let rows = vt.rows();
        let mut selection =
            Selection::begin(capture(&rows, 12, 12), Point { column: 1, row: 0 }).unwrap();
        selection.update(Point { column: 8, row: 0 }).unwrap();
        assert_eq!(
            selection.spans()[0],
            Span {
                row: 0,
                start: 0,
                end: 9
            }
        );
        assert_eq!(
            selection.text(1024).unwrap().as_deref(),
            Some("中e\u{301}     Z")
        );
        let mut hidden =
            Selection::begin(capture(&rows, 12, 12), Point { column: 0, row: 1 }).unwrap();
        hidden.update(Point { column: 8, row: 1 }).unwrap();
        assert_eq!(hidden.text(1024).unwrap().as_deref(), Some("       ok"));
        let mut clipped =
            Selection::begin(capture(&rows, 12, 1), Point { column: 0, row: 0 }).unwrap();
        clipped.update(Point { column: 0, row: 1 }).unwrap();
        assert_eq!(clipped.text(1024).unwrap().as_deref(), Some("\n"));
        assert_eq!(clipped.snapshot().view().rows[0].cells[0].symbol, "");
        assert_eq!(
            rows[0].cells[0].symbol, "中",
            "capture never modifies authoritative cells"
        );
    }

    #[test]
    fn cropped_soft_rows_are_separated_and_missing_history_cannot_be_copied() {
        let rows = [row("abcdef", true), row("ghijkl", false)];
        let mut cropped =
            Selection::begin(capture(&rows, 6, 3), Point { column: 0, row: 0 }).unwrap();
        cropped
            .update(Point {
                column: 99,
                row: 99,
            })
            .unwrap();
        assert_eq!(cropped.text(1024).unwrap().as_deref(), Some("abc\nghi"));
        let rows = [row("abcd", false), vt::Row::default(), row("efgh", false)];
        assert!(matches!(
            Selection::begin(capture(&rows, 4, 4), Point { column: 0, row: 1 }),
            Err(Error::MissingRow)
        ));
        let mut selection =
            Selection::begin(capture(&rows, 4, 4), Point { column: 0, row: 0 }).unwrap();
        assert_eq!(
            selection.update(Point { column: 0, row: 2 }),
            Err(Error::MissingRow)
        );
        assert_eq!(selection.text(1024).unwrap(), None);
    }
}
