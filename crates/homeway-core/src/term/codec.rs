//! codec — surface v4 体编码（R6 6e）。行为真源 = baseline 克隆
//! `pkg/term/term_surface.go` 与 `pkg/term/vt/cellcodec.go`；判据 =
//! `fixtures/vectors/surface_codec.json` 逐字节 + `fixtures/surface-golden/` 夹具两向对拍。
//!
//! 分层：帧（`[op][len:2LE]`）在 [`super::frames`]，本文件管 surface 大帧**解压后的体**：
//!
//! - **cell 行编码**（cellcodec）：`[ver:1][cols:2][rows:2]` + 每行 `[y:2][格流]`；
//!   格流三标记 `0x00 空白游程+varint / 0x01 完整格 / 0x02 重复游程+varint`。
//!   宽度不编（Go appendCell 同款）——「解码语义宽度」由 symbol 是否为空派生
//!   （空 ⇒ 0，客户端不画字形）。
//! - **SNAPSHOT / DIFF / FETCH-ROWS 体**（v4）：光标块 6B（x,y,flags,shape）与回滚条
//!   （total u64 + offset u64 + len u16）两处同形；DIFF 的 rowCount=0 是合法帧。
//! - **分片层**：payload = `[flags:1][gzip]`，片 ≤ 60KiB，bit0 = more；空数据也发一片。
//!   gzip 字节与 Go 不必逐位同（deflate 实现差异，设计 D-12）——契约 = 解压后字节。
//! - **上行小件**：THEME / CLIPBOARD / NOTIFY 载荷。
//!
//! 单一映射纪律（照搬 Go）：wire 光标 = [`surface_cursor_of`]、wire 模式位 =
//! [`surface_modes_of`]——快照/差分/golden 样例共用一处，两处各按一套位映射 golden 就白做。

use std::io::Read as _;
use std::io::Write as _;

use super::frames;
use super::vt;

/// surface 载荷版本（体头一字节；布局变了就必须升版本——Go surfaceVer 同款纪律）。
pub const SURFACE_VER: u8 = 4;
/// 单个分片里 gzip 数据的字节上限（+1 分片头 < 65535 帧长上限）。
pub const FRAG_CHUNK: usize = 60 << 10;
/// 分片头「还有后续片」标志。
pub const FRAG_MORE_BIT: u8 = 1 << 0;
/// 单组分片片数上限（防病态对端无限分片）。
pub const MAX_FRAG_PARTS: usize = 512;
/// SNAPSHOT 附带的回滚镜像窗口大小（视口数的倍数）。
pub const MIRROR_VIEWPORTS: usize = 10;

/// 光标 flags 位。
pub mod cursor_flags {
    pub const VISIBLE: u8 = 1 << 0;
    pub const BLINKING: u8 = 1 << 1;
    pub const WIDE_TAIL: u8 = 1 << 2;
    pub const PASSWORD: u8 = 1 << 3;
}

/// misc 字节位（bit0 = xterm modifyOtherKeys mode 2）。
pub const MISC_MODIFY_OTHER_KEYS: u8 = 1 << 0;

/// SNAPSHOT/DIFF/STATE 共用的 legacy 模式位布局（客户端已有这套位；只增不改）。
pub mod mode_bits {
    pub const DECCKM: u32 = 1 << 0;
    pub const MOUSE_1000: u32 = 1 << 1;
    pub const MOUSE_1002: u32 = 1 << 2;
    pub const MOUSE_1003: u32 = 1 << 3;
    pub const MOUSE_1006: u32 = 1 << 4;
    pub const FOCUS: u32 = 1 << 5;
    pub const BRACKETED: u32 = 1 << 6;
    pub const ALT_SCREEN: u32 = 1 << 7;
}

// ---------------------------------------------------------------------------
// 错误
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CodecError {
    #[error("term surface: 网格编码非法：{0}")]
    BadGrid(&'static str),
    #[error("term surface: 载荷截断（需要 {need}，剩 {left}）")]
    Truncated { need: usize, left: usize },
    #[error("term surface: 版本 {0} ≠ {1}")]
    BadVersion(u8, u8),
    #[error("term surface: 分片数超过 {0}")]
    TooManyFragParts(usize),
    #[error("term surface: 分片头缺失")]
    FragHeaderMissing,
    #[error("term surface: gzip: {0}")]
    Gzip(#[from] std::io::Error),
}

// ---------------------------------------------------------------------------
// wire 光标 / 模式位（单一映射）
// ---------------------------------------------------------------------------

/// wire 光标块（x,y,flags,shape——SNAPSHOT 与 DIFF 同形 6B）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SurfaceCursor {
    pub x: u16,
    pub y: u16,
    pub flags: u8,
    pub shape: u8,
}

/// vt 光标 → wire 光标（单一映射：会话帧生成与 golden 样例共用）。
pub fn surface_cursor_of(c: &vt::Cursor) -> SurfaceCursor {
    let mut flags = 0u8;
    if c.visible {
        flags |= cursor_flags::VISIBLE;
    }
    if c.blinking {
        flags |= cursor_flags::BLINKING;
    }
    if c.wide_tail {
        flags |= cursor_flags::WIDE_TAIL;
    }
    if c.password {
        flags |= cursor_flags::PASSWORD;
    }
    SurfaceCursor { x: c.x, y: c.y, flags, shape: c.shape as u8 }
}

/// 回滚条 wire 形态（total/offset 绝对行号空间，len = 视口行数）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ScrollbarWire {
    pub total: u64,
    pub offset: u64,
    pub len: u16,
}

impl From<vt::Scrollbar> for ScrollbarWire {
    fn from(sb: vt::Scrollbar) -> Self {
        ScrollbarWire { total: sb.total, offset: sb.offset, len: sb.len as u16 }
    }
}

/// vt 模式位 → wire 形态（`(modes, kitty, misc)`；单一映射，Go surfaceModesOf 同款）。
pub fn surface_modes_of(m: &vt::Modes) -> (u32, u8, u8) {
    let mut modes = 0u32;
    if m.cursor_keys_app {
        modes |= mode_bits::DECCKM;
    }
    if m.mouse_x10 || m.mouse_normal {
        modes |= mode_bits::MOUSE_1000;
    }
    if m.mouse_button {
        modes |= mode_bits::MOUSE_1002;
    }
    if m.mouse_any {
        modes |= mode_bits::MOUSE_1003;
    }
    if m.mouse_sgr {
        modes |= mode_bits::MOUSE_1006;
    }
    if m.focus_events {
        modes |= mode_bits::FOCUS;
    }
    if m.bracketed_paste {
        modes |= mode_bits::BRACKETED;
    }
    if m.screen == vt::Screen::Alternate {
        modes |= mode_bits::ALT_SCREEN;
    }
    let mut misc = 0u8;
    if m.modify_other_keys {
        misc |= MISC_MODIFY_OTHER_KEYS;
    }
    (modes, m.kitty_flags, misc)
}

// ---------------------------------------------------------------------------
// cell 行编码（cellcodec）
// ---------------------------------------------------------------------------

/// 格流标记字节。
mod marker {
    pub const BLANK_RUN: u8 = 0x00;
    pub const CELL: u8 = 0x01;
    pub const REPEAT: u8 = 0x02;
    pub const SYM_LEN_MASK: u8 = 0x7f;
    pub const CELL_SKIP_BIT: u8 = 0x80;
}

/// 颜色 kind（与 vt::Color 的 wire 形态一一对应）。
mod color_kind {
    pub const NONE: u8 = 0;
    pub const PALETTE: u8 = 1;
    pub const RGB: u8 = 2;
}

/// cellcodec 版本（载荷头一字节）。
pub const CELL_CODEC_VERSION: u8 = 1;

/// 把视口行编成整块字节（`[ver][cols:2LE][rows:2LE]` + 逐行 `[y:2LE][格流]`）。
pub fn encode_grid(cols: u16, rows: u16, rs: &[vt::Row]) -> Vec<u8> {
    let mut out = Vec::with_capacity(5 + rs.len() * 8);
    out.push(CELL_CODEC_VERSION);
    out.extend_from_slice(&cols.to_le_bytes());
    out.extend_from_slice(&rows.to_le_bytes());
    for r in rs {
        out.extend_from_slice(&r.y.to_le_bytes());
        append_row_cells(&mut out, &r.cells);
    }
    out
}

/// 只编行序列（差分帧与 FETCH-ROWS 应答；每行 `[y:2LE][格流]`，无头）。
pub fn encode_rows(rows: &[vt::Row]) -> Vec<u8> {
    let mut out = Vec::with_capacity(rows.len() * 8);
    for r in rows {
        out.extend_from_slice(&r.y.to_le_bytes());
        append_row_cells(&mut out, &r.cells);
    }
    out
}

/// 「空白格」判定：无字素、无占位、无背景、无修饰、无前景（游程编码的对象）。
fn is_blank(c: &vt::Cell) -> bool {
    c.symbol.is_empty() && !c.skip && c.bg == vt::Color::None && c.attr == 0 && c.fg == vt::Color::None
}

fn append_row_cells(out: &mut Vec<u8>, cells: &[vt::Cell]) {
    let mut blank_run = 0usize;
    let mut repeat_run = 0usize;
    let mut prev: Option<&vt::Cell> = None;
    let flush_blank = |out: &mut Vec<u8>, run: &mut usize| {
        if *run > 0 {
            out.push(marker::BLANK_RUN);
            append_varint(out, *run as u64);
            *run = 0;
        }
    };
    let flush_repeat = |out: &mut Vec<u8>, run: &mut usize| {
        if *run > 0 {
            out.push(marker::REPEAT);
            append_varint(out, *run as u64);
            *run = 0;
        }
    };
    for c in cells {
        if is_blank(c) {
            flush_repeat(out, &mut repeat_run);
            blank_run += 1;
            prev = Some(c);
            continue;
        }
        if let Some(p) = prev {
            if c == p {
                flush_blank(out, &mut blank_run);
                repeat_run += 1;
                continue;
            }
        }
        flush_blank(out, &mut blank_run);
        flush_repeat(out, &mut repeat_run);
        out.push(marker::CELL);
        append_cell(out, c);
        prev = Some(c);
    }
    flush_blank(out, &mut blank_run);
    flush_repeat(out, &mut repeat_run);
}

fn append_cell(out: &mut Vec<u8>, c: &vt::Cell) {
    let sym = c.symbol.as_bytes();
    let mut hdr = (sym.len() & 0x7f) as u8;
    if c.skip {
        hdr |= marker::CELL_SKIP_BIT;
    }
    out.push(hdr);
    out.extend_from_slice(sym);
    append_color(out, c.fg);
    append_color(out, c.bg);
    out.extend_from_slice(&c.attr.to_le_bytes());
}

fn append_color(out: &mut Vec<u8>, c: vt::Color) {
    match c {
        vt::Color::None => out.push(color_kind::NONE),
        vt::Color::Palette(idx) => {
            out.push(color_kind::PALETTE);
            out.push(idx);
        }
        vt::Color::Rgb(r, g, b) => {
            out.push(color_kind::RGB);
            out.extend_from_slice(&[r, g, b]);
        }
    }
}

fn append_varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push(v as u8 | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

/// 解整块网格（与 [`encode_grid`] 严格互逆）。返回 `(cols, rows, 行)`。
pub fn decode_grid(b: &[u8]) -> Result<(u16, u16, Vec<vt::Row>), CodecError> {
    if b.len() < 5 {
        return Err(CodecError::BadGrid("头截断"));
    }
    if b[0] != CELL_CODEC_VERSION {
        return Err(CodecError::BadGrid("版本字节不符"));
    }
    let cols = u16::from_le_bytes([b[1], b[2]]);
    let rows = u16::from_le_bytes([b[3], b[4]]);
    let mut off = 5usize;
    let mut out = Vec::with_capacity(rows as usize);
    for _ in 0..rows {
        if off + 2 > b.len() {
            return Err(CodecError::BadGrid("行号截断"));
        }
        let y = u16::from_le_bytes([b[off], b[off + 1]]);
        off += 2;
        let (cells, n) = decode_row_cells(&b[off..], cols as usize)?;
        off += n;
        out.push(vt::Row { y, dirty: false, wraps: false, cells });
    }
    Ok((cols, rows, out))
}

/// 解行序列（count = 行数；cols = 每格列数）。
pub fn decode_rows(b: &[u8], count: usize, cols: usize) -> Result<Vec<vt::Row>, CodecError> {
    let mut out = Vec::with_capacity(count);
    let mut off = 0usize;
    for _ in 0..count {
        if off + 2 > b.len() {
            return Err(CodecError::BadGrid("行号截断"));
        }
        let y = u16::from_le_bytes([b[off], b[off + 1]]);
        off += 2;
        let (cells, n) = decode_row_cells(&b[off..], cols)?;
        off += n;
        out.push(vt::Row { y, dirty: false, wraps: false, cells });
    }
    Ok(out)
}

/// 解一段格流。返回 `(cells, 消费字节数)`。
fn decode_row_cells(b: &[u8], cols: usize) -> Result<(Vec<vt::Cell>, usize), CodecError> {
    let mut cells: Vec<vt::Cell> = Vec::with_capacity(cols);
    let mut off = 0usize;
    while cells.len() < cols {
        if off >= b.len() {
            return Err(CodecError::BadGrid("格流截断"));
        }
        match b[off] {
            marker::BLANK_RUN => {
                off += 1;
                let (n, adv) = read_varint(&b[off..])?;
                off += adv;
                for _ in 0..n.min((cols - cells.len()) as u64) {
                    cells.push(vt::Cell { width: 1, ..vt::Cell::default() });
                }
            }
            marker::REPEAT => {
                off += 1;
                let (n, adv) = read_varint(&b[off..])?;
                off += adv;
                let last = cells.last().cloned().ok_or(CodecError::BadGrid("行首 repeat"))?;
                for _ in 0..n.min((cols - cells.len()) as u64) {
                    cells.push(last.clone());
                }
            }
            marker::CELL => {
                off += 1;
                let (c, adv) = decode_cell(&b[off..])?;
                off += adv;
                cells.push(c);
            }
            _ => return Err(CodecError::BadGrid("表外标记")),
        }
    }
    Ok((cells, off))
}

fn decode_cell(b: &[u8]) -> Result<(vt::Cell, usize), CodecError> {
    if b.is_empty() {
        return Err(CodecError::BadGrid("格头缺失"));
    }
    let n = (b[0] & marker::SYM_LEN_MASK) as usize;
    let skip = b[0] & marker::CELL_SKIP_BIT != 0;
    let mut off = 1usize;
    if off + n > b.len() {
        return Err(CodecError::BadGrid("symbol 截断"));
    }
    let symbol = String::from_utf8_lossy(&b[off..off + n]).into_owned();
    off += n;
    let (fg, adv) = decode_color(&b[off..])?;
    off += adv;
    let (bg, adv) = decode_color(&b[off..])?;
    off += adv;
    if off + 2 > b.len() {
        return Err(CodecError::BadGrid("attr 截断"));
    }
    let attr = u16::from_le_bytes([b[off], b[off + 1]]);
    off += 2;
    // 无字素的格宽度记 0（客户端据此不画字形；宽度不编——解码侧派生）。
    let width = if symbol.is_empty() { 0 } else { 1 };
    Ok((vt::Cell { symbol, width, skip, fg, bg, attr }, off))
}

fn decode_color(b: &[u8]) -> Result<(vt::Color, usize), CodecError> {
    match b.first().copied().ok_or(CodecError::BadGrid("颜色缺失"))? {
        color_kind::NONE => Ok((vt::Color::None, 1)),
        color_kind::PALETTE => {
            let idx = *b.get(1).ok_or(CodecError::BadGrid("palette 截断"))?;
            Ok((vt::Color::Palette(idx), 2))
        }
        color_kind::RGB => {
            if b.len() < 4 {
                return Err(CodecError::BadGrid("rgb 截断"));
            }
            Ok((vt::Color::Rgb(b[1], b[2], b[3]), 4))
        }
        _ => Err(CodecError::BadGrid("颜色 kind 表外")),
    }
}

fn read_varint(b: &[u8]) -> Result<(u64, usize), CodecError> {
    let mut v = 0u64;
    for (i, byte) in b.iter().take(10).enumerate() {
        v |= u64::from(byte & 0x7f) << (7 * i);
        if byte & 0x80 == 0 {
            return Ok((v, i + 1));
        }
    }
    Err(CodecError::BadGrid("varint 未终止"))
}

// ---------------------------------------------------------------------------
// SNAPSHOT / DIFF / FETCH-ROWS 体
// ---------------------------------------------------------------------------

/// SNAPSHOT 解压后的体（design D2：全网格 + 光标/形状 + 模式位 + 标题 + 镜像窗口）。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SnapshotBody {
    pub revision: u32,
    pub cols: u16,
    pub rows: u16,
    pub cursor: SurfaceCursor,
    pub modes: u32,
    pub kitty: u8,
    pub misc: u8,
    pub title: String,
    pub scroll: ScrollbarWire,
    /// [`encode_grid`] 的整块字节。
    pub grid: Vec<u8>,
    /// 镜像窗口（[`encode_grid`] 整块字节；备用屏/无回滚为空）。
    pub mirror: Vec<u8>,
}

/// 组 SNAPSHOT 体。
pub fn enc_snapshot_body(s: &SnapshotBody) -> Vec<u8> {
    let mut out = Vec::with_capacity(32 + s.title.len() + s.grid.len() + s.mirror.len());
    out.push(SURFACE_VER);
    out.extend_from_slice(&s.revision.to_le_bytes());
    out.extend_from_slice(&s.cols.to_le_bytes());
    out.extend_from_slice(&s.rows.to_le_bytes());
    out.extend_from_slice(&s.cursor.x.to_le_bytes());
    out.extend_from_slice(&s.cursor.y.to_le_bytes());
    out.push(s.cursor.flags);
    out.push(s.cursor.shape);
    out.extend_from_slice(&s.modes.to_le_bytes());
    out.push(s.kitty);
    out.push(s.misc);
    out.extend_from_slice(&s.scroll.total.to_le_bytes());
    out.extend_from_slice(&s.scroll.offset.to_le_bytes());
    out.extend_from_slice(&s.scroll.len.to_le_bytes());
    out.extend_from_slice(&(s.title.len() as u16).to_le_bytes());
    out.extend_from_slice(s.title.as_bytes());
    out.extend_from_slice(&(s.grid.len() as u32).to_le_bytes());
    out.extend_from_slice(&s.grid);
    out.extend_from_slice(&(s.mirror.len() as u32).to_le_bytes());
    out.extend_from_slice(&s.mirror);
    out
}

/// 解 SNAPSHOT 体（恰好耗尽；残留 = 畸形）。
pub fn dec_snapshot_body(p: &[u8]) -> Result<SnapshotBody, CodecError> {
    let mut r = Reader::from(p);
    let ver = r.u8()?;
    if ver != SURFACE_VER {
        return Err(CodecError::BadVersion(ver, SURFACE_VER));
    }
    let revision = r.u32()?;
    let cols = r.u16()?;
    let rows = r.u16()?;
    let cursor = read_cursor(&mut r)?;
    let modes = r.u32()?;
    let kitty = r.u8()?;
    let misc = r.u8()?;
    let total = r.u64()?;
    let offset = r.u64()?;
    let len = r.u16()?;
    let title_len = r.u16()? as usize;
    let title_bytes = r.bytes(title_len)?;
    let grid_len = r.u32()? as usize;
    let grid = r.bytes(grid_len)?.to_vec();
    let mirror_len = r.u32()? as usize;
    let mirror = r.bytes(mirror_len)?.to_vec();
    r.expect_exhausted()?;
    Ok(SnapshotBody {
        revision,
        cols,
        rows,
        cursor,
        modes,
        kitty,
        misc,
        title: String::from_utf8_lossy(title_bytes).into_owned(),
        scroll: ScrollbarWire { total, offset, len },
        grid,
        mirror,
    })
}

/// SURFACE-DIFF 解压后的体：脏行 patch + revision + 本拍光标/模式位/回滚条。
/// `row_count` 可以为 0（「只有状态变了」的合法更新）。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DiffBody {
    pub revision: u32,
    pub cols: u16,
    pub rows: u16,
    pub cursor: SurfaceCursor,
    pub modes: u32,
    pub scroll: ScrollbarWire,
    /// 行编码（[`encode_rows`]；RowCount=0 时为空）。
    pub row_bytes: Vec<u8>,
    pub row_count: u16,
}

/// 组 DIFF 体。
pub fn enc_diff_body(d: &DiffBody) -> Vec<u8> {
    let mut out = Vec::with_capacity(44 + d.row_bytes.len());
    out.push(SURFACE_VER);
    out.extend_from_slice(&d.revision.to_le_bytes());
    out.extend_from_slice(&d.cols.to_le_bytes());
    out.extend_from_slice(&d.rows.to_le_bytes());
    out.extend_from_slice(&d.row_count.to_le_bytes());
    // 光标块与快照同位序（x,y,flags,shape）
    out.extend_from_slice(&d.cursor.x.to_le_bytes());
    out.extend_from_slice(&d.cursor.y.to_le_bytes());
    out.push(d.cursor.flags);
    out.push(d.cursor.shape);
    out.extend_from_slice(&d.modes.to_le_bytes());
    out.extend_from_slice(&d.scroll.total.to_le_bytes());
    out.extend_from_slice(&d.scroll.offset.to_le_bytes());
    out.extend_from_slice(&d.scroll.len.to_le_bytes());
    out.extend_from_slice(&d.row_bytes);
    out
}

/// 解 DIFF 体。
pub fn dec_diff_body(p: &[u8]) -> Result<DiffBody, CodecError> {
    let mut r = Reader::from(p);
    let ver = r.u8()?;
    if ver != SURFACE_VER {
        return Err(CodecError::BadVersion(ver, SURFACE_VER));
    }
    let revision = r.u32()?;
    let cols = r.u16()?;
    let rows = r.u16()?;
    let row_count = r.u16()?;
    let cursor = read_cursor(&mut r)?;
    let modes = r.u32()?;
    let total = r.u64()?;
    let offset = r.u64()?;
    let len = r.u16()?;
    let row_bytes = r.rest().to_vec();
    Ok(DiffBody {
        revision,
        cols,
        rows,
        cursor,
        modes,
        scroll: ScrollbarWire { total, offset, len },
        row_bytes,
        row_count,
    })
}

/// FETCH-ROWS 请求（绝对行号区间，与回滚条同一套行号空间）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FetchRowsReq {
    pub from: u64,
    pub count: u16,
}

/// 单次 FETCH-ROWS 行数上限。
pub const FETCH_ROWS_MAX: u16 = 512;

pub fn enc_fetch_rows_req(r: FetchRowsReq) -> Vec<u8> {
    let mut out = Vec::with_capacity(10);
    out.extend_from_slice(&r.from.to_le_bytes());
    out.extend_from_slice(&r.count.to_le_bytes());
    out
}

pub fn dec_fetch_rows_req(p: &[u8]) -> Result<FetchRowsReq, CodecError> {
    if p.len() < 10 {
        return Err(CodecError::Truncated { need: 10, left: p.len() });
    }
    Ok(FetchRowsReq {
        from: u64::from_le_bytes(p[0..8].try_into().expect("8B")),
        count: u16::from_le_bytes(p[8..10].try_into().expect("2B")),
    })
}

/// FETCH-ROWS 应答（带几何与 revision——客户端据此丢弃过期应答）。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FetchRowsReply {
    pub revision: u32,
    pub cols: u16,
    pub rows: u16,
    pub from: u64,
    pub count: u16,
    pub row_bytes: Vec<u8>,
}

pub fn enc_fetch_rows_reply(r: &FetchRowsReply) -> Vec<u8> {
    let mut out = Vec::with_capacity(24 + r.row_bytes.len());
    out.push(SURFACE_VER);
    out.extend_from_slice(&r.revision.to_le_bytes());
    out.extend_from_slice(&r.cols.to_le_bytes());
    out.extend_from_slice(&r.rows.to_le_bytes());
    out.extend_from_slice(&r.from.to_le_bytes());
    out.extend_from_slice(&r.count.to_le_bytes());
    out.extend_from_slice(&r.row_bytes);
    out
}

pub fn dec_fetch_rows_reply(p: &[u8]) -> Result<FetchRowsReply, CodecError> {
    let mut r = Reader::from(p);
    let ver = r.u8()?;
    if ver != SURFACE_VER {
        return Err(CodecError::BadVersion(ver, SURFACE_VER));
    }
    let revision = r.u32()?;
    let cols = r.u16()?;
    let rows = r.u16()?;
    let from = r.u64()?;
    let count = r.u16()?;
    let row_bytes = r.rest().to_vec();
    Ok(FetchRowsReply { revision, cols, rows, from, count, row_bytes })
}

/// 光标块 6B（快照与差分同形）。
fn read_cursor(r: &mut Reader) -> Result<SurfaceCursor, CodecError> {
    let x = r.u16()?;
    let y = r.u16()?;
    let flags = r.u8()?;
    let shape = r.u8()?;
    Ok(SurfaceCursor { x, y, flags, shape })
}

/// 带边界检查的小端读取器。
struct Reader<'a> {
    p: &'a [u8],
    off: usize,
}

impl<'a> Reader<'a> {
    fn from(p: &'a [u8]) -> Self {
        Reader { p, off: 0 }
    }

    fn need(&self, n: usize) -> Result<(), CodecError> {
        if self.off + n > self.p.len() {
            return Err(CodecError::Truncated { need: n, left: self.p.len() - self.off });
        }
        Ok(())
    }

    fn u8(&mut self) -> Result<u8, CodecError> {
        self.need(1)?;
        let v = self.p[self.off];
        self.off += 1;
        Ok(v)
    }

    fn u16(&mut self) -> Result<u16, CodecError> {
        self.need(2)?;
        let v = u16::from_le_bytes(self.p[self.off..self.off + 2].try_into().expect("2B"));
        self.off += 2;
        Ok(v)
    }

    fn u32(&mut self) -> Result<u32, CodecError> {
        self.need(4)?;
        let v = u32::from_le_bytes(self.p[self.off..self.off + 4].try_into().expect("4B"));
        self.off += 4;
        Ok(v)
    }

    fn u64(&mut self) -> Result<u64, CodecError> {
        self.need(8)?;
        let v = u64::from_le_bytes(self.p[self.off..self.off + 8].try_into().expect("8B"));
        self.off += 8;
        Ok(v)
    }

    fn bytes(&mut self, n: usize) -> Result<&'a [u8], CodecError> {
        self.need(n)?;
        let v = &self.p[self.off..self.off + n];
        self.off += n;
        Ok(v)
    }

    fn rest(&mut self) -> &'a [u8] {
        let v = &self.p[self.off..];
        self.off = self.p.len();
        v
    }

    fn expect_exhausted(&self) -> Result<(), CodecError> {
        if self.off != self.p.len() {
            return Err(CodecError::Truncated { need: 0, left: self.p.len() - self.off });
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// 分片层 + gzip
// ---------------------------------------------------------------------------

/// 把「压缩后的整块数据」切成帧载荷序列（每个元素 = `[flags:1][chunk]`）。
/// 空数据也发一片（flags=0、chunk 空）——让对端明确看到「这一组到齐了」。
pub fn fragment_payload(data: &[u8]) -> Vec<Vec<u8>> {
    if data.len() <= FRAG_CHUNK {
        let mut out = Vec::with_capacity(1 + data.len());
        out.push(0);
        out.extend_from_slice(data);
        return vec![out];
    }
    let mut out = Vec::new();
    let mut off = 0usize;
    while off < data.len() {
        let end = (off + FRAG_CHUNK).min(data.len());
        let flags = if end < data.len() { FRAG_MORE_BIT } else { 0 };
        let mut payload = Vec::with_capacity(1 + end - off);
        payload.push(flags);
        payload.extend_from_slice(&data[off..end]);
        out.push(payload);
        off = end;
    }
    out
}

/// 解一个分片载荷（`[flags][chunk]`）。
pub fn dec_fragment(p: &[u8]) -> Result<(u8, &[u8]), CodecError> {
    match p.split_first() {
        Some((flags, chunk)) => Ok((*flags, chunk)),
        None => Err(CodecError::FragHeaderMissing),
    }
}

/// 客户端侧攒片器（服务端自测也用它做往返验证）。集齐返回整块 gzip 数据；
/// 中途出错（片数超上限）复位并报错。
#[derive(Default)]
pub struct FragAssembler {
    buf: Vec<u8>,
    parts: usize,
}

impl FragAssembler {
    /// 喂一片帧载荷；`Ok(Some(data))` = 本组集齐（data = 整块 gzip 字节）。
    pub fn push(&mut self, payload: &[u8]) -> Result<Option<Vec<u8>>, CodecError> {
        let (flags, chunk) = dec_fragment(payload)?;
        self.parts += 1;
        if self.parts > MAX_FRAG_PARTS {
            self.reset();
            return Err(CodecError::TooManyFragParts(MAX_FRAG_PARTS));
        }
        self.buf.extend_from_slice(chunk);
        if flags & FRAG_MORE_BIT != 0 {
            return Ok(None);
        }
        let out = std::mem::take(&mut self.buf);
        self.reset();
        Ok(Some(out))
    }

    pub fn reset(&mut self) {
        self.buf = Vec::new();
        self.parts = 0;
    }
}

/// 压一段数据（surface 大载荷全部压缩后分片）。
///
/// 头域口径尽力与 Go `gzip.NewWriter` 一致：mtime=0、OS=255、level 6（xfl=0）。
/// deflate 字节本身不强求逐位同（实现差异，设计 D-12）——契约 = 解压后字节。
pub fn gzip_bytes(p: &[u8]) -> Vec<u8> {
    let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::new(6));
    enc.write_all(p).expect("in-memory gzip");
    enc.finish().expect("in-memory gzip")
}

/// 解压。
pub fn gunzip_bytes(p: &[u8]) -> Result<Vec<u8>, CodecError> {
    let mut dec = flate2::read::GzDecoder::new(p);
    let mut out = Vec::new();
    dec.read_to_end(&mut out)?;
    Ok(out)
}

// ---------------------------------------------------------------------------
// 上行小件（THEME / CLIPBOARD / NOTIFY）
// ---------------------------------------------------------------------------

/// THEME 载荷的深浅色标志位。
pub const THEME_DARK: u8 = 1 << 0;

/// 组 THEME 载荷：`[flags:1][fgR fgG fgB][bgR bgG bgB]`。
pub fn enc_theme(fg: [u8; 3], bg: [u8; 3], dark: bool) -> Vec<u8> {
    let flags = if dark { THEME_DARK } else { 0 };
    vec![flags, fg[0], fg[1], fg[2], bg[0], bg[1], bg[2]]
}

/// 解 THEME 载荷。
pub fn dec_theme(p: &[u8]) -> Result<([u8; 3], [u8; 3], bool), CodecError> {
    if p.len() < 7 {
        return Err(CodecError::Truncated { need: 7, left: p.len() });
    }
    let dark = p[0] & THEME_DARK != 0;
    Ok(([p[1], p[2], p[3]], [p[4], p[5], p[6]], dark))
}

/// CLIPBOARD 载荷的种类（首字节）。
pub mod clip_kind {
    /// S→C：程序要写剪贴板。
    pub const WRITE: u8 = 0;
    /// S→C：程序要读剪贴板（客户端回 [`READ_ANSWER`]）。
    pub const READ_REQ: u8 = 1;
    /// C→S：客户端对读请求的应答。
    pub const READ_ANSWER: u8 = 2;
}

/// 剪贴板内容长度上限（FIX-27：载荷 `[kind][len:2][text]` 的 len 是 u16，取满帧上限
/// 减 3 字节头 ⇒ 长度字段恒精确，64KiB–256KiB 区间不静默损坏）。
pub const CLIP_MAX_BYTES: usize = frames::MAX_PAYLOAD - 3;

/// 组 CLIPBOARD 载荷：`[kind:1][len:2LE][text]`（超限按字节截断——同 Go）。
pub fn enc_clipboard(kind: u8, text: &[u8]) -> Vec<u8> {
    let text = &text[..text.len().min(CLIP_MAX_BYTES)];
    let mut out = Vec::with_capacity(3 + text.len());
    out.push(kind);
    out.extend_from_slice(&(text.len() as u16).to_le_bytes());
    out.extend_from_slice(text);
    out
}

/// 解任意方向的 CLIPBOARD 帧（服务端自测与客户端参考实现用）。
pub fn dec_clipboard(p: &[u8]) -> Result<(u8, Vec<u8>), CodecError> {
    let (kind, rest) = p.split_first().ok_or(CodecError::Truncated { need: 3, left: p.len() })?;
    if rest.len() < 2 {
        return Err(CodecError::Truncated { need: 3, left: p.len() });
    }
    let n = u16::from_le_bytes(rest[0..2].try_into().expect("2B")) as usize;
    if rest.len() < 2 + n {
        return Err(CodecError::BadGrid("剪贴板载荷截断"));
    }
    Ok((*kind, rest[2..2 + n].to_vec()))
}

/// 解客户端对读请求的应答（非读应答载荷 = 错）。
pub fn dec_clipboard_answer(p: &[u8]) -> Result<Vec<u8>, CodecError> {
    let (kind, text) = dec_clipboard(p)?;
    if kind != clip_kind::READ_ANSWER {
        return Err(CodecError::BadGrid("非读应答"));
    }
    Ok(text)
}

/// NOTIFY 载荷的文本上限。
pub const NOTIFY_MAX_BYTES: usize = 4096;

/// 组 NOTIFY 载荷：`[len:2LE][text]`（超限按字节截断——同 Go）。
pub fn enc_notify(text: &[u8]) -> Vec<u8> {
    let text = &text[..text.len().min(NOTIFY_MAX_BYTES)];
    let mut out = Vec::with_capacity(2 + text.len());
    out.extend_from_slice(&(text.len() as u16).to_le_bytes());
    out.extend_from_slice(text);
    out
}

/// 解 NOTIFY 载荷。
pub fn dec_notify(p: &[u8]) -> Result<Vec<u8>, CodecError> {
    if p.len() < 2 {
        return Err(CodecError::Truncated { need: 2, left: p.len() });
    }
    let n = u16::from_le_bytes(p[0..2].try_into().expect("2B")) as usize;
    if p.len() < 2 + n {
        return Err(CodecError::BadGrid("notify 载荷截断"));
    }
    Ok(p[2..2 + n].to_vec())
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::term::testutil;
    use crate::term::vt::{Row, SessionVt};
    use serde_json::Value;

    fn vtcell(v: &Value) -> vt::Cell {
        // 颜色 JSON 形态：[]/缺 = none；[1, idx] = palette；[2, r, g, b] = rgb（kind 值同 wire）
        let color = |v: &Value| -> vt::Color {
            let a = v.as_array().map(Vec::as_slice).unwrap_or(&[]);
            let num = |i: usize| a.get(i).and_then(Value::as_u64).unwrap_or(0) as u8;
            match (a.first().and_then(Value::as_u64).unwrap_or(0), a.len()) {
                (1, 2..) => vt::Color::Palette(num(1)),
                (2, 4..) => vt::Color::Rgb(num(1), num(2), num(3)),
                _ => vt::Color::None,
            }
        };
        let attr = v.get("attr").and_then(Value::as_u64).unwrap_or(0) as u16;
        vt::Cell {
            symbol: v.get("sym").and_then(Value::as_str).unwrap_or("").to_string(),
            width: v.get("width").and_then(Value::as_u64).unwrap_or(0) as u8,
            skip: v.get("skip").and_then(Value::as_bool).unwrap_or(false),
            fg: color(v.get("fg").unwrap_or(&Value::Null)),
            bg: color(v.get("bg").unwrap_or(&Value::Null)),
            attr,
        }
    }

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex"))
            .collect()
    }

    /// 6e 判据①：cellcodec 向量逐字节对拍（encode_grid/encode_rows）+ 解码往返 +
    /// 解码格值（除 width——解码语义宽度由 symbol 派生，不参与往返）。
    #[test]
    fn surface_cellcodec_parity_with_go_vectors() {
        let v: Value = serde_json::from_str(testutil::VECTOR_SURFACE_CODEC)
            .expect("surface_codec.json invalid");
        let cases = v.get("cells").and_then(Value::as_array).expect("cells array");
        assert!(cases.len() >= 15, "cellcodec 案数 {}", cases.len());
        for c in cases {
            let name = c.get("name").and_then(Value::as_str).expect("name");
            let cols = c.get("cols").and_then(Value::as_u64).expect("cols") as u16;
            let rows: Vec<vt::Row> = c
                .get("rows_in")
                .and_then(Value::as_array)
                .expect("rows_in")
                .iter()
                .map(|r| vt::Row {
                    y: r.get("y").and_then(Value::as_u64).expect("y") as u16,
                    dirty: false,
                    wraps: false,
                    cells: r
                        .get("cells")
                        .and_then(Value::as_array)
                        .map(|a| a.iter().map(vtcell).collect())
                        .unwrap_or_default(),
                })
                .collect();
            let grid = encode_grid(cols, rows.len() as u16, &rows);
            let rows_enc = encode_rows(&rows);
            assert_eq!(
                hex::encode(&grid),
                c.get("grid_hex").and_then(Value::as_str).expect("grid_hex"),
                "{name}: encode_grid 与 Go 不一致"
            );
            assert_eq!(
                hex::encode(&rows_enc),
                c.get("rows_hex").and_then(Value::as_str).expect("rows_hex"),
                "{name}: encode_rows 与 Go 不一致"
            );
            // 解码往返：格值逐字段（除 width 派生语义）+ 重编字节恒等
            let (dcols, drows_n, decoded) = decode_grid(&grid).expect("decode_grid");
            assert_eq!((dcols, drows_n as usize), (cols, rows.len()), "{name}: 几何");
            for (want, got) in rows.iter().zip(&decoded) {
                assert_eq!(want.y, got.y, "{name}: 行 y");
                assert_eq!(want.cells.len(), got.cells.len(), "{name}: 格数");
                for (wc, gc) in want.cells.iter().zip(&got.cells) {
                    assert_eq!(wc.symbol, gc.symbol, "{name}: symbol");
                    assert_eq!(wc.skip, gc.skip, "{name}: skip");
                    assert_eq!(wc.fg, gc.fg, "{name}: fg");
                    assert_eq!(wc.bg, gc.bg, "{name}: bg");
                    assert_eq!(wc.attr, gc.attr, "{name}: attr");
                    // width 不编（解码侧派生），不参与往返对拍
                }
            }
            assert_eq!(
                encode_grid(dcols, drows_n, &decoded),
                grid,
                "{name}: 重编字节恒等"
            );
            // 行序列解码（差分/FETCH-ROWS 面；cols=0 时格流为空也合法）
            let decoded_rows = decode_rows(&rows_enc, rows.len(), cols as usize)
                .unwrap_or_else(|e| panic!("{name}: decode_rows: {e}"));
            assert_eq!(decoded_rows.len(), rows.len(), "{name}: decode_rows 行数");
        }
    }

    /// 6e 判据②：DecodeGrid/DecodeRows 负例（篡改字节全走报错路径）。
    #[test]
    fn surface_cellcodec_negative_cases() {
        let v: Value = serde_json::from_str(testutil::VECTOR_SURFACE_CODEC)
            .expect("surface_codec.json invalid");
        let cases = v.get("cells_bad").and_then(Value::as_array).expect("cells_bad");
        assert!(cases.len() >= 7, "负例案数 {}", cases.len());
        for c in cases {
            let name = c.get("name").and_then(Value::as_str).expect("name");
            let input = hex(c.get("in_hex").and_then(Value::as_str).expect("in_hex"));
            let count = c.get("count").and_then(Value::as_u64).unwrap_or(0) as usize;
            let cols = c.get("cols").and_then(Value::as_u64).unwrap_or(0) as usize;
            if count > 0 {
                assert!(decode_rows(&input, count, cols).is_err(), "{name}: 应报错");
            } else {
                assert!(decode_grid(&input).is_err(), "{name}: 应报错");
            }
        }
    }

    /// 6e 判据③：SNAPSHOT/DIFF/FETCH-ROWS 体向量逐字节对拍 + 解码往返字段一致。
    #[test]
    fn surface_bodies_parity_with_go_vectors() {
        let v: Value = serde_json::from_str(testutil::VECTOR_SURFACE_CODEC)
            .expect("surface_codec.json invalid");
        // SNAPSHOT：解码 → 重编 == Go 字节（位序/字段序的往返锚；手工重排体只会造
        // 出第二份会漂移的「真源」，这里按向量本身对拍）
        for c in v.get("snapshot").and_then(Value::as_array).expect("snapshot") {
            let name = c.get("name").and_then(Value::as_str).expect("name");
            let want = c.get("body_hex").and_then(Value::as_str).expect("body_hex");
            let bytes = hex(want);
            let body = dec_snapshot_body(&bytes).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(hex::encode(enc_snapshot_body(&body)), want, "{name}: 往返");
            if name == "full" {
                // 体头字段序抽查：ver / rev u32 / cols / rows / 光标块 6B（x,y,flags,shape）
                assert_eq!(bytes[0], SURFACE_VER);
                assert_eq!(bytes[1..5], 0x01020304u32.to_le_bytes());
                assert_eq!(bytes[5..7], 100u16.to_le_bytes());
                assert_eq!(bytes[7..9], 32u16.to_le_bytes());
                assert_eq!(bytes[9..11], 9u16.to_le_bytes());
                assert_eq!(bytes[11..13], 30u16.to_le_bytes());
                assert_eq!((bytes[13], bytes[14], bytes[15]), (0x0f, 3, 0xff));
                assert_eq!(body.revision, 0x01020304);
                assert_eq!(
                    body.cursor,
                    SurfaceCursor { x: 9, y: 30, flags: 0x0f, shape: 3 }
                );
                assert_eq!(body.title, "标题-title");
                assert_eq!(
                    body.scroll,
                    ScrollbarWire { total: 1234567890123, offset: 1234567890101, len: 32 }
                );
                assert!(!body.grid.is_empty() && !body.mirror.is_empty());
            }
        }
        for c in v.get("diff").and_then(Value::as_array).expect("diff") {
            let name = c.get("name").and_then(Value::as_str).expect("name");
            let want = c.get("body_hex").and_then(Value::as_str).expect("body_hex");
            let bytes = hex(want);
            let body = dec_diff_body(&bytes).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(hex::encode(enc_diff_body(&body)), want, "{name}: 往返");
        }
        for c in v.get("fetch_req").and_then(Value::as_array).expect("fetch_req") {
            let name = c.get("name").and_then(Value::as_str).expect("name");
            let want = c.get("body_hex").and_then(Value::as_str).expect("body_hex");
            let bytes = hex(want);
            let req = dec_fetch_rows_req(&bytes).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(hex::encode(enc_fetch_rows_req(req)), want, "{name}: 往返");
        }
        for c in v.get("fetch_reply").and_then(Value::as_array).expect("fetch_reply") {
            let name = c.get("name").and_then(Value::as_str).expect("name");
            let want = c.get("body_hex").and_then(Value::as_str).expect("body_hex");
            let bytes = hex(want);
            let rep = dec_fetch_rows_reply(&bytes).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(hex::encode(enc_fetch_rows_reply(&rep)), want, "{name}: 往返");
        }
    }

    /// 6e 判据④：分片向量（空/单片）+ 多片边界与攒片器（合成数据——60KiB hex 不进向量）。
    #[test]
    fn surface_fragment_and_assembler() {
        let v: Value = serde_json::from_str(testutil::VECTOR_SURFACE_CODEC)
            .expect("surface_codec.json invalid");
        for c in v.get("fragment").and_then(Value::as_array).expect("fragment") {
            let name = c.get("name").and_then(Value::as_str).expect("name");
            let data = hex(c.get("in_hex").and_then(Value::as_str).expect("in_hex"));
            let want: Vec<(u8, Vec<u8>)> = c
                .get("frags")
                .and_then(Value::as_array)
                .expect("frags")
                .iter()
                .map(|f| {
                    (
                        f.get("flags").and_then(Value::as_u64).expect("flags") as u8,
                        hex(f.get("hex").and_then(Value::as_str).expect("hex")),
                    )
                })
                .collect();
            let got = fragment_payload(&data);
            assert_eq!(got.len(), want.len(), "{name}: 片数");
            for (g, (wf, wd)) in got.iter().zip(&want) {
                let (gf, gd) = dec_fragment(g).expect("frag");
                assert_eq!((gf, gd), (*wf, wd.as_slice()), "{name}: 片内容");
            }
            // 攒片器吃完全部分片 → 整块数据
            let mut asm = FragAssembler::default();
            let mut done = None;
            for g in &got {
                if let Some(d) = asm.push(g).expect("push") {
                    done = Some(d);
                }
            }
            assert_eq!(done.as_deref(), Some(data.as_slice()), "{name}: 攒片重组");
        }
        // 多片边界（合成）：恰 60KiB 单片；60KiB+1 两片（more 位 + 尺寸切分）；超大件片数上限
        let chunk = FRAG_CHUNK;
        let one = vec![0xabu8; chunk];
        assert_eq!(fragment_payload(&one).len(), 1, "恰 fragChunk 单片");
        let two = vec![0xabu8; chunk + 1];
        let frags = fragment_payload(&two);
        assert_eq!(frags.len(), 2, "fragChunk+1 两片");
        assert_eq!(dec_fragment(&frags[0]).unwrap().0, FRAG_MORE_BIT, "首片 more 位");
        assert_eq!(frags[1].len(), 2, "末片 = 头 + 1 字节");
        // 攒片上限 512（513 片 × 最小片 2B 触发；重置后可再用）
        let mut asm = FragAssembler::default();
        let one_frag = vec![FRAG_MORE_BIT];
        for _ in 0..MAX_FRAG_PARTS {
            assert!(asm.push(&one_frag).unwrap().is_none());
        }
        assert!(matches!(asm.push(&one_frag), Err(CodecError::TooManyFragParts(_))));
        assert_eq!(asm.push(&[0]).unwrap().unwrap(), Vec::<u8>::new(), "复位后可用");
    }

    /// gzip 往返 + 头域口径（mtime=0、OS=255）+ 能解 Go 的 gzip（golden .bin 里的真分片）。
    #[test]
    fn surface_gzip_roundtrip_and_header() {
        let body = b"surface-gzip-\xe4\xbd\xa0\xe5\xa5\xbd".repeat(100);
        let gz = gzip_bytes(&body);
        // gzip 头：1f 8b 08 / mtime=0 / xfl / OS=255
        assert_eq!(&gz[..3], &[0x1f, 0x8b, 0x08]);
        assert_eq!(&gz[4..8], &[0, 0, 0, 0], "mtime=0");
        assert_eq!(gz[9], 255, "OS=255");
        assert_eq!(gunzip_bytes(&gz).unwrap(), body);
        // Go 产的 gzip（golden 样例 = fragmentPayload(gzipBytes(body)) 的真分片）
        for key in ["session-cjk", "session-git-log", "session-styles"] {
            let frames = testutil::read_golden_bin(key);
            let mut asm = FragAssembler::default();
            let mut done = None;
            for (_, payload) in &frames {
                if let Some(d) = asm.push(payload).expect("golden frag") {
                    done = Some(d);
                }
            }
            let gz = done.expect("golden 集齐");
            assert!(gunzip_bytes(&gz).is_ok(), "{key}: 解 Go gzip");
        }
    }

    /// 上行小件：THEME/CLIPBOARD/NOTIFY 的 golden 字节表（surface_input_cases.tsv）。
    #[test]
    fn surface_uplink_payloads_parity() {
        let cases = testutil::read_input_cases();
        assert!(cases.len() >= 19, "input cases {}", cases.len());
        let mut seen_theme = false;
        let mut seen_clip = false;
        for (name, kind, fields, want_hex) in &cases {
            let want = hex(want_hex);
            match kind.as_str() {
                "theme" => {
                    seen_theme = true;
                    let dark = fields[0].trim() == "1";
                    let c = |s: &str| -> [u8; 3] {
                        let s = s.trim();
                        [
                            u8::from_str_radix(&s[0..2], 16).expect("r"),
                            u8::from_str_radix(&s[2..4], 16).expect("g"),
                            u8::from_str_radix(&s[4..6], 16).expect("b"),
                        ]
                    };
                    let fg = c(&fields[1]);
                    let bg = c(&fields[2]);
                    assert_eq!(enc_theme(fg, bg, dark), want, "{name}");
                    assert_eq!(dec_theme(&want).unwrap(), (fg, bg, dark), "{name}");
                }
                "clipboard" => {
                    seen_clip = true;
                    let text = fields.first().map(String::as_str).unwrap_or("").as_bytes();
                    assert_eq!(enc_clipboard(clip_kind::READ_ANSWER, text), want, "{name}");
                    assert_eq!(dec_clipboard_answer(&want).unwrap(), text, "{name}");
                }
                _ => {} // key/text/mouse/focus/caps/hello-tail 面 = frames.rs 的既有判据
            }
        }
        assert!(seen_theme && seen_clip, "theme/clipboard 案须被消费");
        // NOTIFY：Go encNotify/decNotify（golden 表无此行——Go 侧同函数族，补齐往返）
        let n = enc_notify(b"hello-\xe4\xb8\x96");
        assert_eq!(dec_notify(&n).unwrap(), b"hello-\xe4\xb8\x96".to_vec());
        assert!(dec_notify(&[0]).is_err());
        // 超限截断
        let big = vec![b'x'; NOTIFY_MAX_BYTES + 10];
        assert_eq!(enc_notify(&big).len(), 2 + NOTIFY_MAX_BYTES);
        let big_clip = vec![b'x'; CLIP_MAX_BYTES + 10];
        assert_eq!(enc_clipboard(clip_kind::WRITE, &big_clip).len(), 3 + CLIP_MAX_BYTES);
    }


    /// 客户端参考网格（消费侧最小模型：快照建网格 + 差分行覆盖）。
    struct ClientGrid {
        cols: u16,
        rows: Vec<Row>,
    }

    impl ClientGrid {
        fn apply_snapshot(body: &SnapshotBody) -> Self {
            let (cols, _, rows) = decode_grid(&body.grid).expect("snapshot grid");
            let mut rows = rows;
            // 行数补齐到视口（blankRun 语义：缺行按全空白行——Go 客户端同款）
            while rows.len() < body.rows as usize {
                rows.push(Row { y: rows.len() as u16, dirty: false, wraps: false, cells: vec![] });
            }
            ClientGrid { cols, rows }
        }

        fn apply_diff(&mut self, body: &DiffBody) {
            let patch =
                decode_rows(&body.row_bytes, body.row_count as usize, self.cols as usize)
                    .expect("diff rows");
            for r in patch {
                if (r.y as usize) < self.rows.len() {
                    self.rows[r.y as usize].cells = r.cells;
                }
            }
        }
    }

    /// 6e 判据⑤（消费向）：Rust 的攒片器 + gzip 解压 + 体解码吃得了 **Go 产的** golden 字节，
    /// 帧序应用（快照 → 差分）后的网格 digest 与 manifest.tsv 逐行一致。
    #[test]
    fn surface_golden_consume_go_bins() {
        let manifest = testutil::golden_manifest();
        assert!(manifest.len() >= 8, "golden manifest 行数 {}", manifest.len());
        for row in &manifest {
            let name = &row[0];
            let frames = testutil::read_golden_bin(name);
            assert!(!frames.is_empty(), "{name}: golden 帧序列空");
            let mut grid: Option<ClientGrid> = None;
            let mut cursor = SurfaceCursor::default();
            let mut scroll = ScrollbarWire::default();
            let mut modes = 0u32;
            for (op, payload) in &frames {
                match *op {
                    0x0d => {
                        let mut asm = FragAssembler::default();
                        let gz = asm.push(payload).expect("frag").expect("快照集齐");
                        let body =
                            dec_snapshot_body(&gunzip_bytes(&gz).expect("gunzip")).expect("body");
                        grid = Some(ClientGrid::apply_snapshot(&body));
                        cursor = body.cursor;
                        scroll = body.scroll;
                        modes = body.modes;
                    }
                    0x0f => {
                        let mut asm = FragAssembler::default();
                        let gz = asm.push(payload).expect("frag").expect("差分集齐");
                        let body =
                            dec_diff_body(&gunzip_bytes(&gz).expect("gunzip")).expect("body");
                        grid.as_mut().expect("差分前必有快照").apply_diff(&body);
                        cursor = body.cursor;
                        scroll = body.scroll;
                        modes = body.modes;
                    }
                    other => panic!("{name}: golden 含未预期 op 0x{other:02x}"),
                }
            }
            let g = grid.expect("{name}: 无快照帧");
            let text = testutil::fnv_hex(testutil::text_of(&g.rows).as_bytes());
            let style = testutil::fnv_hex(testutil::style_of(&g.rows, g.cols as usize).as_bytes());
            assert_eq!(text, row[5], "{name}: 文本 digest（消费 Go 字节）");
            assert_eq!(style, row[15], "{name}: 样式向量 digest（消费 Go 字节）");
            // 光标 4 列 / 回滚条 3 列 / 模式位列
            let want_cursor =
                format!("{},{},{},{}", cursor.x, cursor.y, cursor.flags, cursor.shape);
            assert_eq!(format!("{},{},{},{}", row[8], row[9], row[10], row[11]), want_cursor,
                "{name}: 光标列");
            assert_eq!(
                format!("{},{},{}", scroll.total, scroll.offset, scroll.len),
                format!("{},{},{}", row[12], row[13], row[14]),
                "{name}: 回滚条列"
            );
            assert_eq!(row[16], format!("{modes}"), "{name}: 模式位列");
        }
    }

    /// 6e 判据⑥（产出向）：真实会话夹具喂 SessionVt → 我们自己产 SNAPSHOT/DIFF 体
    /// （gzip + 分片走我们的编码器）→ 回流解码 → digest/光标/回滚条/模式位 == manifest。
    #[test]
    fn surface_golden_produce_from_vt() {
        let manifest = testutil::golden_manifest();
        let find = |name: &str| -> Vec<String> {
            manifest
                .iter()
                .find(|r| r[0] == name)
                .unwrap_or_else(|| panic!("manifest 缺 {name}"))
                .clone()
        };
        for key in ["session-cjk", "session-git-log", "session-hexdump", "session-styles"] {
            let data = if key == "session-styles" {
                testutil::golden_styles_session()
            } else {
                std::fs::read(format!(
                    "{}/../../fixtures/term-vt/{key}.bin",
                    env!("CARGO_MANIFEST_DIR")
                ))
                .unwrap_or_else(|e| panic!("{key}.bin: {e}"))
            };
            let mut vt = SessionVt::new(100, 32, 5000).expect("vt");
            vt.write(&data);
            // 快照
            let rows = vt.rows();
            let snap_cursor = surface_cursor_of(&vt.cursor());
            let snap_scroll: ScrollbarWire = vt.scrollbar().into();
            let (snap_modes, kitty, misc) = surface_modes_of(&vt.modes());
            let grid = encode_grid(100, 32, &rows);
            let mirror = vt.mirror_rows(32 * MIRROR_VIEWPORTS);
            let mirror_enc = if mirror.is_empty() {
                Vec::new()
            } else {
                encode_grid(100, mirror.len() as u16, &mirror)
            };
            let body = SnapshotBody {
                revision: 1,
                cols: 100,
                rows: 32,
                cursor: snap_cursor,
                modes: snap_modes,
                kitty,
                misc,
                title: String::new(),
                scroll: snap_scroll,
                grid,
                mirror: mirror_enc,
            };
            let wire = pipeline_snapshot(&body);
            let row = find(key);
            assert_grid_digests(&wire, &row, "快照（产出向）");
            assert_eq!((wire.cursor, wire.scroll, wire.modes), (snap_cursor, snap_scroll, snap_modes));
            // 差分：追加「GOLDEN-DIFF-追加」至脏行非空且光标变化止（Go golden 生成器同款）
            let mut client = ClientGrid::apply_snapshot(&wire);
            let mut diff_row = None;
            for _ in 0..4 {
                vt.write("GOLDEN-DIFF-追加".as_bytes());
                vt.update();
                let dirty = vt.dirty_rows();
                let c = surface_cursor_of(&vt.cursor());
                if !dirty.is_empty() && c != snap_cursor {
                    let scroll: ScrollbarWire = vt.scrollbar().into();
                    let (modes, _, _) = surface_modes_of(&vt.modes());
                    let d = DiffBody {
                        revision: 1,
                        cols: 100,
                        rows: 32,
                        cursor: c,
                        modes,
                        scroll,
                        row_bytes: encode_rows(&dirty),
                        row_count: dirty.len() as u16,
                    };
                    diff_row = Some(d);
                    break;
                }
            }
            let d = diff_row.unwrap_or_else(|| panic!("{key}: 4 次追加无差分"));
            let wire = pipeline_diff(&d);
            let row = find(&format!("{key}-diff"));
            client.apply_diff(&wire);
            let text = testutil::fnv_hex(testutil::text_of(&client.rows).as_bytes());
            let style =
                testutil::fnv_hex(testutil::style_of(&client.rows, client.cols as usize).as_bytes());
            assert_eq!(text, row[5], "{key}-diff: 文本 digest（产出向 + 应用差分）");
            assert_eq!(style, row[15], "{key}-diff: 样式 digest（产出向 + 应用差分）");
            assert_eq!(
                format!("{},{},{},{}", wire.cursor.x, wire.cursor.y, wire.cursor.flags, wire.cursor.shape),
                format!("{},{},{},{}", row[8], row[9], row[10], row[11]),
                "{key}-diff: 光标列"
            );
            assert_eq!(
                format!("{},{},{}", wire.scroll.total, wire.scroll.offset, wire.scroll.len),
                format!("{},{},{}", row[12], row[13], row[14]),
                "{key}-diff: 回滚条列"
            );
            assert_eq!(row[16], format!("{}", wire.modes), "{key}-diff: 模式位列");
        }
    }

    /// 产出向的回流管道（快照形态）：体 → gzip → 分片 → 攒片 → 解压 → 解体。
    fn pipeline_snapshot(body: &SnapshotBody) -> SnapshotBody {
        let raw = enc_snapshot_body(body);
        let gz = gzip_bytes(&raw);
        let frags = fragment_payload(&gz);
        let mut asm = FragAssembler::default();
        let mut done = None;
        for f in &frags {
            if let Some(d) = asm.push(f).expect("push") {
                done = Some(d);
            }
        }
        dec_snapshot_body(&gunzip_bytes(&done.expect("集齐")).expect("gunzip")).expect("dec")
    }

    /// 产出向的回流管道（差分形态）。
    fn pipeline_diff(body: &DiffBody) -> DiffBody {
        let raw = enc_diff_body(body);
        let gz = gzip_bytes(&raw);
        let frags = fragment_payload(&gz);
        let mut asm = FragAssembler::default();
        let mut done = None;
        for f in &frags {
            if let Some(d) = asm.push(f).expect("push") {
                done = Some(d);
            }
        }
        dec_diff_body(&gunzip_bytes(&done.expect("集齐")).expect("gunzip")).expect("dec")
    }

    fn assert_grid_digests(body: &SnapshotBody, row: &[String], what: &str) {
        let (cols, _, rows) = decode_grid(&body.grid).expect("grid");
        let text = testutil::fnv_hex(testutil::text_of(&rows).as_bytes());
        let style = testutil::fnv_hex(testutil::style_of(&rows, cols as usize).as_bytes());
        assert_eq!(text, row[5], "{}: 文本 digest", what);
        assert_eq!(style, row[15], "{}: 样式向量 digest", what);
    }

    /// 模式位/光标单一映射：全字段形态（位不重叠 + 形状枚举数值）。
    #[test]
    fn surface_mapping_bits() {
        let m = vt::Modes {
            cursor_keys_app: true,
            mouse_x10: false,
            mouse_normal: true,
            mouse_button: true,
            mouse_any: true,
            mouse_sgr: true,
            focus_events: true,
            bracketed_paste: true,
            screen: vt::Screen::Alternate,
            kitty_flags: 0x0f,
            modify_other_keys: true,
            ..vt::Modes::default()
        };
        let (modes, kitty, misc) = surface_modes_of(&m);
        assert_eq!(modes, 0xff);
        assert_eq!((kitty, misc), (0x0f, 1));
        // X10 单独也置 MOUSE_1000 位（Go MouseX10||MouseNormal 同置）
        let (m2, _, _) = surface_modes_of(&vt::Modes { mouse_x10: true, ..vt::Modes::default() });
        assert_eq!(m2, mode_bits::MOUSE_1000);
        let c = vt::Cursor {
            x: 9,
            y: 30,
            visible: true,
            blinking: true,
            password: true,
            wide_tail: true,
            shape: vt::CursorShape::BlockHollow,
        };
        assert_eq!(
            surface_cursor_of(&c),
            SurfaceCursor { x: 9, y: 30, flags: 0x0f, shape: 3 }
        );
    }
}
