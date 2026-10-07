//! size — 终端网格尺寸的值对象（Q-D F1a/F1b）。
//!
//! 背景（`docs/reviews/QD-design.md` F1）：尺寸入径此前是裸 `u16`，`RESIZE 65535x65535`
//! 会被 alacritty 原样接受并按 `rows × cols` 即时分配两屏（实测 ≈96 GiB 起；分配失败走
//! `handle_alloc_error` ⇒ **abort，不是 unwind**——`catch_unwind` 救不了）。本模块把
//! 「已归一（非 0 且 ≤ 上限）」编码进类型：**未归一的尺寸在类型上不可表达**，
//! 会话几何只能由 [`Size`] 写入。
//!
//! 两个构造器对应两条入径，语义不同（对齐 Go `pkg/term`）：
//! - [`Size::normalized`]——HELLO/建会话面（Go `spawnLocked`）：`0 → 缺省`、超限 → 夹取；
//! - [`Size::from_report`]——RESIZE 上报面（Go `applySizeLocked` 的 0 门语义）：
//!   `0 → None`（**忽略本次上报**，不改腿/不改会话/不 apply）、超限 → 夹取。

/// 网格尺寸（宽 × 高，单位 = 字符格）。
///
/// 不变量：`1 ≤ cols ≤ MAX_COLS`、`1 ≤ rows ≤ MAX_ROWS`。字段私有——构造器是唯一入口。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Size {
    cols: u16,
    rows: u16,
}

impl Size {
    /// 列上限。
    ///
    /// 依据（F1a 上限论证）：两屏即时分配 `2 × 1000 × 500 × 24B ≈ 24 MiB`；
    /// 客户端余量——手机 surface 网格 ≈100×40（≥10×）、桌面 CLI 可读字号下
    /// 8K 全屏 ≈850-960 列 × 250-270 行在限内。只有不可读的极小字号（≤6 px）
    /// 才越过限值，这类形态会被夹取且**无 env 逃逸口**（安全边界不由环境放大，
    /// 残余登记见 `docs/reviews/QD.md`）。
    pub const MAX_COLS: u16 = 1000;
    /// 行上限（同上）。
    pub const MAX_ROWS: u16 = 500;
    /// 缺省几何（Go `spawnLocked` 的 80×24）。
    pub const DEFAULT: Size = Size { cols: 80, rows: 24 };

    pub const fn cols(self) -> u16 {
        self.cols
    }

    pub const fn rows(self) -> u16 {
        self.rows
    }

    /// HELLO/建会话面归一：`0 → 缺省`、超限 → 夹取（幂等）。
    pub fn normalized(cols: u16, rows: u16) -> Size {
        Size {
            cols: if cols == 0 { Self::DEFAULT.cols } else { cols.min(Self::MAX_COLS) },
            rows: if rows == 0 { Self::DEFAULT.rows } else { rows.min(Self::MAX_ROWS) },
        }
    }

    /// RESIZE 上报面归一：`0 → None`（忽略本次上报——对齐 Go 在 0 门后写会话尺寸）；
    /// 其余 → 按上限夹取（幂等）。
    pub fn from_report(cols: u16, rows: u16) -> Option<Size> {
        if cols == 0 || rows == 0 {
            return None;
        }
        Some(Self::normalized(cols, rows))
    }

    /// 是否恰为已归一形态（`normalized(cols, rows)` 不夹取任何分量）。
    /// 组件层硬拒（[`super::vt::SessionVt`]）与夹取日志判定用。
    pub fn is_exact(cols: u16, rows: u16) -> bool {
        let s = Self::normalized(cols, rows);
        s.cols == cols && s.rows == rows
    }
}

impl std::fmt::Display for Size {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}x{}", self.cols, self.rows)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 边界表（0/1/合法/等于上限/超限/u16::MAX）。
    #[test]
    fn normalized_boundaries() {
        assert_eq!(Size::normalized(0, 0), Size::DEFAULT, "双 0 ⇒ 缺省 80x24");
        assert_eq!(Size::normalized(0, 40), Size { cols: 80, rows: 40 }, "单 0 分量各自缺省");
        assert_eq!(Size::normalized(1, 1), Size { cols: 1, rows: 1 }, "最小合法值原样");
        assert_eq!(Size::normalized(120, 40), Size { cols: 120, rows: 40 }, "常见值原样");
        assert_eq!(
            Size::normalized(Size::MAX_COLS, Size::MAX_ROWS),
            Size { cols: 1000, rows: 500 },
            "等于上限原样"
        );
        assert_eq!(Size::normalized(1001, 501), Size { cols: 1000, rows: 500 }, "越界夹到上限");
        assert_eq!(
            Size::normalized(u16::MAX, u16::MAX),
            Size { cols: 1000, rows: 500 },
            "u16::MAX 夹到上限（绝不发起巨型分配）"
        );
        // 幂等
        let once = Size::normalized(65535, 3);
        assert_eq!(Size::normalized(once.cols(), once.rows()), once, "夹取幂等");
    }

    /// 上报面：0 值 ⇒ None（忽略上报），其余同归一。
    #[test]
    fn report_gate_zero_and_clamp() {
        assert_eq!(Size::from_report(0, 24), None, "cols=0 忽略");
        assert_eq!(Size::from_report(80, 0), None, "rows=0 忽略");
        assert_eq!(Size::from_report(0, 0), None, "0x0 忽略");
        assert_eq!(Size::from_report(80, 24), Some(Size::DEFAULT));
        assert_eq!(
            Size::from_report(u16::MAX, u16::MAX),
            Some(Size { cols: 1000, rows: 500 }),
            "极端值夹取而非忽略"
        );
    }

    /// is_exact：夹取/缺省替换的输入不算「恰归一」。
    #[test]
    fn exact_gate() {
        assert!(Size::is_exact(80, 24));
        assert!(Size::is_exact(1000, 500));
        assert!(!Size::is_exact(0, 24), "0 会被替换成缺省 ⇒ 非恰归一");
        assert!(!Size::is_exact(1001, 24));
        assert!(!Size::is_exact(80, 501));
        assert!(!Size::is_exact(u16::MAX, u16::MAX));
    }

    /// 展示形态（日志用）。
    #[test]
    fn display_shape() {
        assert_eq!(Size::normalized(100, 32).to_string(), "100x32");
    }
}
