//! Client-side material from the server's authoritative surface; never reflow PTY bytes.
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use homeway_core::term::{codec, size::Size, vt};

use super::selection::{Identity, Snapshot};
use super::view::{HistoryAction, TerminalView};

const CACHE_CELLS: usize = 128 * 1024;
const CACHE_ROWS: usize = 2048;
const FETCH_WAIT: Duration = Duration::from_secs(2);

#[derive(Debug, thiserror::Error)]
pub(super) enum Error {
    #[error("{0}")]
    Codec(#[from] codec::CodecError),
    #[error("终端 surface 几何或行号无效")]
    Geometry,
}

struct PendingFetch {
    request: codec::FetchRowsReq,
    sent: Instant,
}

#[derive(Default)]
pub(super) struct Surface {
    pub(super) ready: bool,
    cols: u16,
    rows: u16,
    revision: u32,
    seq: u64,
    epoch: u64,
    base: u64,
    pub(super) modes: u32,
    cursor: codec::SurfaceCursor,
    scroll: codec::ScrollbarWire,
    live: Vec<vt::Row>,
    cache: BTreeMap<u64, vt::Row>,
    /// None follows the current screen; Some holds a stable absolute history row.
    anchor: Option<u64>,
    pending: Option<PendingFetch>,
    fetch_retry: Option<Instant>,
    visible: Vec<vt::Row>,
    top: u64,
}

impl Surface {
    pub(super) fn selection_identity(&self) -> Option<Identity> {
        self.ready.then_some(Identity {
            revision: self.revision,
            epoch: self.epoch,
            base: self.base,
            cols: self.cols,
            rows: self.rows,
            alternate: self.alternate(),
        })
    }

    pub(super) fn selection_snapshot(&self, width: u16, height: u16) -> Option<Snapshot> {
        Snapshot::capture(self.selection_identity()?, self.view(), width, height)
    }

    pub(super) fn geometry(&self) -> (u16, u16) {
        (self.cols, self.rows)
    }
    /// Keep display material while accepting a new leg's sequence from one.
    pub(super) fn reconnect(&mut self) {
        self.ready = false;
        self.seq = 0;
        self.pending = None;
        self.fetch_retry = None;
    }

    pub(super) fn snapshot(&mut self, body: codec::SnapshotBody) -> Result<bool, Error> {
        if self.seq != 0 && body.seq <= self.seq {
            return Ok(false);
        }
        validate(body.cols, body.rows, body.scroll, body.history_base)?;
        let (cols, rows, grid) = codec::decode_grid(&body.grid)?;
        if (cols, rows) != (body.cols, body.rows) || !ordered(&grid, rows) {
            return Err(Error::Geometry);
        }
        let changed_identity =
            self.cols != cols || self.rows != rows || self.epoch != body.history_epoch;
        let mirror = if body.mirror.is_empty() {
            Vec::new()
        } else {
            let (mirror_cols, mirror_rows, material) = codec::decode_grid(&body.mirror)?;
            if mirror_cols != cols
                || u64::from(mirror_rows) > body.scroll.offset.saturating_sub(body.history_base)
            {
                return Err(Error::Geometry);
            }
            material
        };
        if changed_identity {
            self.cache.clear();
            self.anchor = None;
        }
        self.pending = None;
        self.fetch_retry = None;
        self.cols = cols;
        self.rows = rows;
        self.revision = body.revision;
        self.seq = body.seq;
        self.epoch = body.history_epoch;
        self.base = body.history_base;
        self.cursor = body.cursor;
        self.modes = body.modes;
        self.scroll = body.scroll;
        self.live = grid;
        let from = self.scroll.offset.saturating_sub(mirror.len() as u64);
        self.cache.extend(
            mirror
                .into_iter()
                .enumerate()
                .map(|(i, row)| (from + i as u64, row)),
        );
        self.ready = true;
        self.clamp();
        self.prune();
        Ok(true)
    }

    /// A gap or changed identity requires a fresh baseline before input is accepted.
    pub(super) fn diff(&mut self, body: codec::DiffBody) -> Result<bool, Error> {
        if body.seq <= self.seq {
            return Ok(true);
        }
        if !self.ready
            || body.seq != self.seq.saturating_add(1)
            || body.revision != self.revision
            || (body.cols, body.rows) != (self.cols, self.rows)
            || body.history_epoch != self.epoch
        {
            self.ready = false;
            self.pending = None;
            return Ok(false);
        }
        validate(body.cols, body.rows, body.scroll, body.history_base)?;
        let patch = codec::decode_rows(
            &body.row_bytes,
            usize::from(body.row_count),
            usize::from(body.cols),
        )?;
        let mut seen = vec![false; usize::from(self.rows)];
        for row in &patch {
            let Some(slot) = seen.get_mut(usize::from(row.y)) else {
                return Err(Error::Geometry);
            };
            if *slot {
                return Err(Error::Geometry);
            }
            *slot = true;
        }
        for row in patch {
            let y = usize::from(row.y);
            self.live[y] = row;
        }
        self.seq = body.seq;
        self.base = body.history_base;
        self.cursor = body.cursor;
        self.modes = body.modes;
        self.scroll = body.scroll;
        self.clamp();
        self.prune();
        Ok(true)
    }

    pub(super) fn fetch_reply(&mut self, body: codec::FetchRowsReply) -> Result<bool, Error> {
        let Some(pending) = self.pending.as_ref() else {
            return Ok(false);
        };
        if !self.ready
            || body.history_epoch != self.epoch
            || body.revision != self.revision
            || (body.cols, body.rows) != (self.cols, self.rows)
            || body.seq > self.seq
            || body.from != pending.request.from
            || body.count > pending.request.count
        {
            return Ok(false);
        }
        self.pending = None;
        self.base = self.base.max(body.history_base.min(self.scroll.offset));
        self.clamp();
        if body.count == 0 {
            // Budget/trim misses are replies, not outstanding requests. Back off
            // explicitly rather than flood the same empty range every UI tick.
            self.fetch_retry = Some(Instant::now() + FETCH_WAIT);
            return Ok(false);
        }
        self.fetch_retry = None;
        let rows = codec::decode_rows(
            &body.row_bytes,
            usize::from(body.count),
            usize::from(body.cols),
        )?;
        for (index, row) in rows.into_iter().enumerate() {
            let line = body.from + index as u64;
            // The mutable live screen always comes from the latest surface grid.
            if line >= self.base && line < self.scroll.offset {
                self.cache.insert(line, row);
            }
        }
        self.prune();
        Ok(true)
    }

    pub(super) fn fetch_failed(&mut self) {
        self.pending = None;
    }

    pub(super) fn scroll(&mut self, action: HistoryAction, height: u16) {
        if !self.ready || self.alternate() {
            return;
        }
        let latest = self.latest_top(height);
        let top = self.anchor.unwrap_or(latest);
        let page = u64::from(height.max(1).saturating_sub(1).max(1));
        let next = match action {
            HistoryAction::Lines(lines) if lines < 0 => {
                top.saturating_sub(u64::from(lines.unsigned_abs()))
            }
            HistoryAction::Lines(lines) => top.saturating_add(lines as u64),
            HistoryAction::PageUp => top.saturating_sub(page),
            HistoryAction::PageDown => top.saturating_add(page),
            HistoryAction::Start => self.base,
            HistoryAction::End => latest,
        }
        .clamp(self.base, latest);
        self.anchor = (next < latest).then_some(next);
        self.fetch_retry = None;
    }

    pub(super) fn follow(&mut self) {
        self.anchor = None;
    }

    pub(super) fn prepare(&mut self, height: u16) {
        let latest = self.latest_top(height);
        if self.anchor.is_some_and(|top| top >= latest) {
            self.anchor = None;
        }
        self.top = self.anchor.unwrap_or(latest).max(self.base);
        self.visible.clear();
        for line in self.top
            ..self
                .top
                .saturating_add(u64::from(height))
                .min(self.scroll.total)
        {
            let row = if line >= self.scroll.offset {
                self.live.get((line - self.scroll.offset) as usize)
            } else {
                self.cache.get(&line)
            };
            self.visible.push(row.cloned().unwrap_or_default());
        }
    }

    pub(super) fn fetch_request(
        &mut self,
        height: u16,
        now: Instant,
    ) -> Option<codec::FetchRowsReq> {
        if !self.ready || self.alternate() {
            return None;
        }
        if self.fetch_retry.is_some_and(|until| now < until) {
            return None;
        }
        self.fetch_retry = None;
        if self
            .pending
            .as_ref()
            .is_some_and(|pending| now.duration_since(pending.sent) < FETCH_WAIT)
        {
            return None;
        }
        self.pending = None;
        let end = self
            .top
            .saturating_add(u64::from(height))
            .min(self.scroll.offset);
        let missing = (self.top..end).find(|line| !self.cache.contains_key(line))?;
        let count = usize::from(height.max(1))
            .saturating_mul(3)
            .min(usize::from(codec::FETCH_ROWS_MAX))
            .min(CACHE_CELLS / usize::from(self.cols.max(1)));
        let from = missing.saturating_sub(u64::from(height)).max(self.base);
        let request = codec::FetchRowsReq {
            history_epoch: self.epoch,
            from,
            count: count.min(self.scroll.offset.saturating_sub(from) as usize) as u16,
        };
        self.pending = Some(PendingFetch { request, sent: now });
        Some(request)
    }

    pub(super) fn view(&self) -> TerminalView<'_> {
        let absolute_cursor = self.scroll.offset + u64::from(self.cursor.y);
        let cursor = (self.ready && self.anchor.is_none() && absolute_cursor >= self.top)
            .then_some(vt::Cursor {
                x: self.cursor.x,
                y: absolute_cursor
                    .saturating_sub(self.top)
                    .min(u64::from(u16::MAX)) as u16,
                visible: self.cursor.flags & codec::cursor_flags::VISIBLE != 0,
                blinking: self.cursor.flags & codec::cursor_flags::BLINKING != 0,
                wide_tail: self.cursor.flags & codec::cursor_flags::WIDE_TAIL != 0,
                password: self.cursor.flags & codec::cursor_flags::PASSWORD != 0,
                shape: vt::CursorShape::Block,
            });
        TerminalView {
            rows: &self.visible,
            cursor,
            mouse_reporting: self.modes
                & (codec::mode_bits::MOUSE_1000
                    | codec::mode_bits::MOUSE_1002
                    | codec::mode_bits::MOUSE_1003)
                != 0,
            scrollbar: vt::Scrollbar {
                total: self.scroll.total,
                offset: self.scroll.offset,
                len: u64::from(self.scroll.len),
            },
            top: self.top,
        }
    }

    pub(super) fn focus_events(&self) -> bool {
        self.modes & codec::mode_bits::FOCUS != 0
    }

    pub(super) fn mouse_position(
        &self,
        column: u16,
        row: u16,
        activating: bool,
    ) -> Option<(u16, u16)> {
        let y = self
            .top
            .saturating_sub(self.scroll.offset)
            .saturating_add(u64::from(row));
        (self.ready && self.anchor.is_none() && column < self.cols && y < u64::from(self.rows))
            // A real press first takes the observer's pane geometry on the server.
            // Passive movement/releases still address the current authoritative grid.
            .then_some((column, if activating { row } else { y as u16 }))
    }

    fn alternate(&self) -> bool {
        self.modes & codec::mode_bits::ALT_SCREEN != 0
    }

    fn latest_top(&self, height: u16) -> u64 {
        // If another active leg is taller, retain the authoritative viewport and keep
        // its cursor in the cropped pane instead of assuming the cursor is on the last row.
        self.scroll.offset
            + u64::from(
                self.cursor
                    .y
                    .min(self.rows.saturating_sub(1))
                    .saturating_add(1)
                    .saturating_sub(height.max(1)),
            )
    }

    fn clamp(&mut self) {
        if self.alternate() {
            self.anchor = None;
            self.cache.clear();
        }
        if let Some(top) = &mut self.anchor {
            *top = (*top).max(self.base);
        }
        self.cache
            .retain(|line, _| *line >= self.base && *line < self.scroll.offset);
    }

    fn prune(&mut self) {
        let maximum = CACHE_ROWS.min(CACHE_CELLS / usize::from(self.cols.max(1)));
        let center = self.anchor.unwrap_or(self.scroll.offset);
        while self.cache.len() > maximum {
            let first = *self.cache.first_key_value().expect("nonempty cache").0;
            let last = *self.cache.last_key_value().expect("nonempty cache").0;
            let farthest = if first.abs_diff(center) > last.abs_diff(center) {
                first
            } else {
                last
            };
            self.cache.remove(&farthest);
        }
    }
}

fn ordered(rows: &[vt::Row], count: u16) -> bool {
    rows.len() == usize::from(count)
        && rows
            .iter()
            .enumerate()
            .all(|(y, row)| usize::from(row.y) == y)
}

fn validate(cols: u16, rows: u16, scroll: codec::ScrollbarWire, base: u64) -> Result<(), Error> {
    if !Size::is_exact(cols, rows)
        || scroll.len != rows
        || base > scroll.offset
        || scroll.offset.checked_add(u64::from(rows)) != Some(scroll.total)
    {
        return Err(Error::Geometry);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reconnect_preserves_display_and_accepts_new_leg_sequence_one() {
        let mut vt = vt::SessionVt::new(30, 5, 10).unwrap();
        vt.write(b"PRESERVED_SCREEN");
        let mut surface = Surface::default();
        surface.snapshot(snapshot(&mut vt, 100, 2)).unwrap();
        surface.prepare(5);
        let before = surface.view().rows.to_vec();
        surface.reconnect();
        surface.prepare(5);
        assert_eq!(surface.view().rows, before);
        assert!(!surface.ready);
        assert!(surface.selection_identity().is_none());
        assert!(surface.snapshot(snapshot(&mut vt, 1, 2)).unwrap());
        assert!(surface.ready);
    }

    fn snapshot(vt: &mut vt::SessionVt, seq: u64, revision: u32) -> codec::SnapshotBody {
        let (cols, rows) = vt.size();
        let mirror = vt.mirror_rows(3);
        codec::SnapshotBody {
            cols,
            rows,
            seq,
            revision,
            history_epoch: vt.history_epoch(),
            history_base: vt.history_base(),
            cursor: codec::surface_cursor_of(&vt.cursor()),
            modes: codec::surface_modes_of(&vt.modes()).0,
            scroll: vt.scrollbar().into(),
            grid: codec::encode_grid(cols, rows, &vt.rows()),
            mirror: if mirror.is_empty() {
                Vec::new()
            } else {
                codec::encode_grid(cols, mirror.len() as u16, &mirror)
            },
            ..Default::default()
        }
    }

    fn diff(vt: &mut vt::SessionVt, seq: u64, revision: u32) -> codec::DiffBody {
        let (cols, rows) = vt.size();
        codec::DiffBody {
            cols,
            rows,
            seq,
            revision,
            history_epoch: vt.history_epoch(),
            history_base: vt.history_base(),
            cursor: codec::surface_cursor_of(&vt.cursor()),
            modes: codec::surface_modes_of(&vt.modes()).0,
            scroll: vt.scrollbar().into(),
            row_count: rows,
            row_bytes: codec::encode_rows(&vt.rows()),
        }
    }

    fn fetch(
        vt: &vt::SessionVt,
        request: codec::FetchRowsReq,
        seq: u64,
        revision: u32,
    ) -> codec::FetchRowsReply {
        let rows = vt.rows_at(request.from, usize::from(request.count));
        let (cols, height) = vt.size();
        codec::FetchRowsReply {
            cols,
            rows: height,
            seq,
            revision,
            history_epoch: vt.history_epoch(),
            history_base: vt.history_base(),
            from: request.from,
            count: rows.len() as u16,
            row_bytes: codec::encode_rows(&rows),
        }
    }

    fn text(row: &vt::Row) -> String {
        row.cells.iter().map(|cell| cell.symbol.as_str()).collect()
    }

    #[test]
    fn selection_snapshot_freezes_only_the_displayed_pane_and_exposes_identity() {
        let mut surface = Surface::default();
        assert!(surface.selection_identity().is_none());
        assert!(surface.selection_snapshot(3, 2).is_none());
        let mut vt = vt::SessionVt::new(6, 3, 20).unwrap();
        vt.write(b"abcdef");
        surface.snapshot(snapshot(&mut vt, 1, 1)).unwrap();
        surface.prepare(2);
        let captured = surface.selection_snapshot(3, 2).unwrap();
        let identity = captured.identity();
        assert_eq!(captured.view().rows.len(), 2);
        assert_eq!(captured.view().rows[0].cells.len(), 3);
        assert_eq!(text(&captured.view().rows[0]), "abc");
        assert!(captured.view().cursor.is_none());
        vt.write(b"\rXYZ");
        assert!(surface.diff(diff(&mut vt, 2, 1)).unwrap());
        surface.prepare(2);
        assert_eq!(surface.selection_identity(), Some(identity));
        assert_eq!(text(&captured.view().rows[0]), "abc");
        assert!(text(&surface.view().rows[0]).starts_with("XYZ"));
        vt.resize(8, 3).unwrap();
        surface.snapshot(snapshot(&mut vt, 3, 2)).unwrap();
        assert_ne!(surface.selection_identity(), Some(identity));
        let primary = surface.selection_identity().unwrap();
        vt.write(b"\x1b[?1049h");
        surface.snapshot(snapshot(&mut vt, 4, 2)).unwrap();
        assert!(surface.selection_identity().unwrap().alternate);
        assert_ne!(surface.selection_identity(), Some(primary));
        assert!(!surface.diff(diff(&mut vt, 6, 2)).unwrap());
        assert!(surface.selection_identity().is_none());
    }

    #[test]
    fn fetch_absolute_history_keeps_anchor_during_new_output_then_follows_end() {
        let mut vt = vt::SessionVt::new(120, 6, 100).unwrap();
        for i in 0..40 {
            vt.write(format!("ROW_{i:03}_{}\r\n", "x".repeat(100)).as_bytes());
        }
        let mut surface = Surface::default();
        surface.snapshot(snapshot(&mut vt, 1, 1)).unwrap();
        surface.scroll(HistoryAction::Start, 3);
        surface.prepare(3);
        let request = surface.fetch_request(3, Instant::now()).unwrap();
        assert_eq!(request.from, 0);
        let reply = fetch(&vt, request, 1, 1);
        assert!(
            codec::decode_rows(&reply.row_bytes, usize::from(reply.count), 120)
                .unwrap()
                .iter()
                .all(|row| row.y == 0)
        );
        assert!(surface.fetch_reply(reply).unwrap());
        surface.prepare(3);
        assert!(text(&surface.view().rows[0]).starts_with("ROW_000_"));
        assert_eq!(surface.view().rows[0].cells.len(), 120);
        for i in 40..44 {
            vt.write(format!("ROW_{i:03}\r\n").as_bytes());
        }
        assert!(surface.diff(diff(&mut vt, 2, 1)).unwrap());
        surface.prepare(3);
        assert_eq!(
            surface.view().top,
            0,
            "new output does not move an absolute history anchor"
        );
        assert!(text(&surface.view().rows[0]).starts_with("ROW_000_"));
        assert!(surface.view().cursor.is_none());
        surface.scroll(HistoryAction::End, 3);
        surface.prepare(3);
        assert!(surface.view().top >= vt.scrollbar().offset);
        assert!(surface.view().cursor.is_some());
    }

    #[test]
    fn sequence_gap_and_geometry_change_reject_old_material_and_recover() {
        let mut vt = vt::SessionVt::new(120, 6, 100).unwrap();
        for i in 0..40 {
            vt.write(format!("ROW_{i}\r\n").as_bytes());
        }
        let mut surface = Surface::default();
        surface.snapshot(snapshot(&mut vt, 1, 1)).unwrap();
        surface.scroll(HistoryAction::Start, 3);
        surface.prepare(3);
        let old_request = surface.fetch_request(3, Instant::now()).unwrap();
        let old_reply = fetch(&vt, old_request, 1, 1);
        assert!(!surface.diff(diff(&mut vt, 3, 1)).unwrap());
        assert!(!surface.ready);
        assert!(
            !surface.snapshot(snapshot(&mut vt, 1, 1)).unwrap(),
            "stale snapshot must not reopen input after a gap"
        );
        vt.resize(60, 6).unwrap();
        surface.snapshot(snapshot(&mut vt, 4, 2)).unwrap();
        assert!(!surface.fetch_reply(old_reply).unwrap());
        assert!(surface.ready);
        surface.prepare(3);
        assert!(surface.view().rows.iter().all(|row| row.cells.len() == 60));
    }

    #[test]
    fn history_trim_clamps_anchor_and_alternate_screen_has_no_primary_history() {
        let mut vt = vt::SessionVt::new(80, 6, 20).unwrap();
        for i in 0..20 {
            vt.write(format!("ROW_{i}\r\n").as_bytes());
        }
        let mut surface = Surface::default();
        surface.snapshot(snapshot(&mut vt, 1, 1)).unwrap();
        surface.scroll(HistoryAction::Start, 3);
        for i in 20..60 {
            vt.write(format!("ROW_{i}\r\n").as_bytes());
        }
        surface.diff(diff(&mut vt, 2, 1)).unwrap();
        surface.prepare(3);
        assert!(surface.view().top >= vt.history_base());
        vt.write(b"\x1b[?1049h\x1b[HALT_ONLY");
        surface.snapshot(snapshot(&mut vt, 3, 2)).unwrap();
        surface.scroll(HistoryAction::Start, 3);
        surface.prepare(3);
        assert_eq!(surface.view().top, 0);
        assert!(surface.fetch_request(3, Instant::now()).is_none());
        assert!(text(&surface.view().rows[0]).starts_with("ALT_ONLY"));
    }

    #[test]
    fn cropped_authoritative_view_keeps_cursor_and_history_cache_has_cell_budget() {
        let mut vt = vt::SessionVt::new(120, 40, 5000).unwrap();
        vt.write(b"AT_TOP");
        let mut surface = Surface::default();
        surface.snapshot(snapshot(&mut vt, 1, 1)).unwrap();
        surface.prepare(10);
        assert_eq!(surface.view().top, 0);
        assert!(text(&surface.view().rows[0]).starts_with("AT_TOP"));
        assert_eq!(surface.view().cursor.unwrap().y, 0);
        vt.write(b"\x1b[35;1HLOW_CURSOR");
        surface.diff(diff(&mut vt, 2, 1)).unwrap();
        surface.prepare(10);
        assert_eq!(surface.view().top, 25);
        assert_eq!(surface.view().cursor.unwrap().y, 9);
        assert_eq!(surface.mouse_position(2, 3, false), Some((2, 28)));
        assert_eq!(
            surface.mouse_position(2, 3, true),
            Some((2, 3)),
            "press is encoded after taking pane geometry"
        );
        assert_eq!(surface.mouse_position(120, 3, false), None);
        assert_eq!(surface.mouse_position(2, 20, false), None);
        surface.scroll(HistoryAction::PageUp, 10);
        surface.prepare(10);
        assert_eq!(
            surface.mouse_position(2, 3, true),
            None,
            "history cannot send clicks to an unrelated live row"
        );
        surface.scroll = codec::ScrollbarWire {
            total: 5040,
            offset: 5000,
            len: 40,
        };
        surface.cache = (0..5000).map(|line| (line, vt::Row::default())).collect();
        surface.prune();
        assert!(surface.cache.len() * 120 <= CACHE_CELLS);
        assert!(surface.cache.len() <= CACHE_ROWS);
    }

    #[test]
    fn empty_fetch_is_completed_and_trimmed_range_retries_with_backoff() {
        let mut vt = vt::SessionVt::new(120, 6, 100).unwrap();
        for i in 0..40 {
            vt.write(format!("ROW_{i}\r\n").as_bytes());
        }
        let mut surface = Surface::default();
        surface.snapshot(snapshot(&mut vt, 1, 1)).unwrap();
        surface.scroll(HistoryAction::Start, 3);
        surface.prepare(3);
        let now = Instant::now();
        let request = surface.fetch_request(3, now).unwrap();
        assert!(!surface
            .fetch_reply(codec::FetchRowsReply {
                revision: 1,
                seq: 1,
                cols: 120,
                rows: 6,
                history_epoch: vt.history_epoch(),
                history_base: 2,
                from: request.from,
                count: 0,
                row_bytes: Vec::new()
            })
            .unwrap());
        assert!(
            surface.pending.is_none(),
            "an empty response has completed its request"
        );
        surface.prepare(3);
        assert_eq!(surface.view().top, 2);
        assert!(
            surface.fetch_request(3, Instant::now()).is_none(),
            "budget misses cannot trigger a 40Hz request loop"
        );
        let retry = surface
            .fetch_request(3, now + FETCH_WAIT + Duration::from_secs(1))
            .unwrap();
        assert!(retry.from >= 2, "retry uses the authoritative trimmed base");
    }
}
