//! ring — 会话历史的有界输出环（R6 6f-3a；行为真源 = baseline 克隆
//! `pkg/term/service.go` 的 appendLocked/readLocked/replayStartLocked/noteSizeLocked）。
//!
//! 定长环：容量即历史上限（默认 1MiB）；`written` 是累计字节（绝对偏移），
//! `start = written - min(written, cap)` 是最旧可用字节。回放起点选择：
//! 尾部优先（回放上限）→ 行边界 → ESC 起点 → 原样；epoch 表（≤64）记尺寸变化点。

use super::size::Size;

/// 起点对齐时最多前看的字节数。
pub const REPLAY_TRIM: usize = 4096;

/// 一次尺寸变化点（epoch）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Epoch {
    pub off: u64,
    /// 变化后的尺寸（已归一——类型保证非 0 且在限内）。
    pub size: Size,
}

/// 回放起点的「尺寸变化策略」。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplayEpoch {
    /// 从历史起点起（默认；跨尺寸回放由 REPLAY-DONE 的 flags 标记）。
    Whole,
    /// 只回放最后一个 epoch 之后的输出。
    Last,
}

/// 定长字节环。
pub struct OutputRing {
    buf: Vec<u8>,
    /// 累计写入字节数（绝对偏移空间）。
    written: u64,
    /// 最旧可用字节的绝对偏移。
    start: u64,
    /// 尺寸变化点表（≤64）。
    pub epochs: Vec<Epoch>,
}

impl OutputRing {
    pub fn new(cap: usize) -> Self {
        OutputRing { buf: vec![0; cap], written: 0, start: 0, epochs: Vec::new() }
    }

    pub fn cap(&self) -> usize {
        self.buf.len()
    }

    pub fn written(&self) -> u64 {
        self.written
    }

    pub fn start(&self) -> u64 {
        self.start
    }

    /// 把输出写进环（环长 0 = 不记）。
    pub fn append(&mut self, mut p: &[u8]) {
        let cap = self.buf.len();
        if cap == 0 {
            return;
        }
        while !p.is_empty() {
            let pos = (self.written % cap as u64) as usize;
            let n = (cap - pos).min(p.len());
            self.buf[pos..pos + n].copy_from_slice(&p[..n]);
            self.written += n as u64;
            p = &p[n..];
        }
        let start = self.written.saturating_sub(cap as u64);
        if start > self.start {
            self.start = start;
        }
    }

    /// 取 [off, off+max) 的输出字节（超出历史区间就从 start 开始）。
    pub fn read(&self, mut off: u64, mut max: usize) -> Vec<u8> {
        if off < self.start {
            off = self.start;
        }
        if off >= self.written || max == 0 {
            return Vec::new();
        }
        if max as u64 > self.written - off {
            max = (self.written - off) as usize;
        }
        let cap = self.buf.len();
        if cap == 0 {
            return Vec::new();
        }
        let mut out = Vec::with_capacity(max);
        while out.len() < max {
            let pos = (off % cap as u64) as usize;
            let avail = (cap - pos).min(max - out.len());
            out.extend_from_slice(&self.buf[pos..pos + avail]);
            off += avail as u64;
        }
        out
    }

    /// 记一次尺寸变化（同时进 epoch 表；超 64 截旧）。入参已归一（[`Size`]）。
    pub fn note_size(&mut self, size: Size) {
        self.epochs.push(Epoch { off: self.written, size });
        if self.epochs.len() > 64 {
            let drop = self.epochs.len() - 64;
            self.epochs.drain(..drop);
        }
    }

    /// 回放窗口是否跨过尺寸变化点（REPLAY-DONE 的 flags bit1）。
    pub fn window_crosses_epoch(&self, start: u64) -> bool {
        self.epochs.iter().any(|e| e.off > start && e.off < self.written)
    }

    /// 选回放起点：尾部优先（replay 上限）→ 行边界 → ESC 起点 → 原样。
    /// 返回 `(起点, 头部截断)`。
    pub fn replay_start(&self, replay_max: usize, epoch_policy: ReplayEpoch) -> (u64, bool) {
        let mut start = self.start;
        let mut truncated = false;
        if epoch_policy == ReplayEpoch::Last {
            if let Some(last) = self.epochs.last() {
                if last.off > start {
                    start = last.off;
                    truncated = true;
                }
            }
        }
        if replay_max > 0 && self.written - start > replay_max as u64 {
            start = self.written - replay_max as u64;
            truncated = true;
        }
        let head = self.read(start, REPLAY_TRIM);
        if head.is_empty() {
            return (start, truncated);
        }
        if let Some(i) = head.iter().position(|&b| b == b'\n') {
            return (start + i as u64 + 1, truncated);
        }
        if let Some(i) = head.iter().position(|&b| b == 0x1b) {
            if i > 0 {
                return (start + i as u64, truncated);
            }
        }
        (start, truncated)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sz(cols: u16, rows: u16) -> Size {
        Size::normalized(cols, rows)
    }

    #[test]
    fn ring_append_read_wraparound() {
        let mut r = OutputRing::new(16);
        r.append(b"0123456789abcdefghij"); // 20 字节进 16 环：最旧 4 字节被顶掉
        assert_eq!(r.written(), 20);
        assert_eq!(r.start(), 4);
        assert_eq!(r.read(0, 100), b"456789abcdefghij");
        // 环回读
        let mut r2 = OutputRing::new(8);
        r2.append(b"AAAABBBBCCCC");
        assert_eq!(r2.read(0, 100), b"BBBBCCCC");
        assert_eq!(r2.read(8, 4), b"CCCC");
        // 部分窗口（环回：abs6/7 = pos6/7 的 B，abs8 = pos0 的 C）
        assert_eq!(r2.read(6, 3), b"BBC");
        // 越界起点抬升到 start
        assert_eq!(r2.read(0, 2), b"BB");
        // max=0 / off >= written
        assert!(r2.read(8, 0).is_empty());
        assert!(r2.read(100, 5).is_empty());
    }

    #[test]
    fn ring_zero_cap_is_noop() {
        let mut r = OutputRing::new(0);
        r.append(b"xyz");
        assert_eq!((r.written(), r.start()), (0, 0));
        assert!(r.read(0, 10).is_empty());
    }

    #[test]
    fn replay_start_line_then_esc_then_raw() {
        // 行边界优先：起点前看窗内首个 \n 之后
        let mut r = OutputRing::new(1024);
        r.append(b"partial-line\x1b[2Kthen more\nsecond\n");
        let (start, trunc) = r.replay_start(0, ReplayEpoch::Whole);
        assert!(!trunc);
        assert_eq!(r.read(start, 1024), b"second\n", "首 \\n 之后");
        // 无 \\n 有 ESC：从 ESC 起（i > 0）
        let mut r2 = OutputRing::new(1024);
        r2.append(b"plain\x1b[31mred");
        let (start2, _) = r2.replay_start(0, ReplayEpoch::Whole);
        assert_eq!(r2.read(start2, 1024), b"\x1b[31mred");
        // ESC 在首位（i == 0）不算：原样
        let mut r3 = OutputRing::new(1024);
        r3.append(b"\x1b[31mno-newline");
        let (start3, _) = r3.replay_start(0, ReplayEpoch::Whole);
        assert_eq!(start3, 0);
        // 全裸文本：原样
        let mut r4 = OutputRing::new(1024);
        r4.append(b"no markers at all");
        assert_eq!(r4.replay_start(0, ReplayEpoch::Whole).0, 0);
    }

    #[test]
    fn replay_start_tail_window_and_epoch() {
        let mut r = OutputRing::new(1024);
        r.append(&[b'x'; 600]);
        r.note_size(sz(100, 32)); // epoch @600
        r.append(&[b'y'; 100]);
        // 尾部窗口 50：起点 = 700-50，truncated
        let (start, trunc) = r.replay_start(50, ReplayEpoch::Whole);
        assert!(trunc);
        assert_eq!(start, 650);
        // 窗口谓词：epoch 点严格落在 (start, written) 开区间内才算跨过
        assert!(!r.window_crosses_epoch(start), "600 在窗口之前");
        assert!(r.window_crosses_epoch(599));
        assert!(!r.window_crosses_epoch(600), "严格大于：恰在边界不算");
        // epoch=last：起点 = 600（最后的尺寸变化点）
        let (start2, trunc2) = r.replay_start(0, ReplayEpoch::Last);
        assert!(trunc2);
        assert_eq!(start2, 600);
        assert_eq!(r.read(start2, 1000), [b'y'; 100]);
    }

    #[test]
    fn epochs_table_caps_at_64() {
        let mut r = OutputRing::new(1024);
        for i in 0..80 {
            r.note_size(sz(80 + i as u16, 24));
            r.append(b"z");
        }
        assert_eq!(r.epochs.len(), 64);
        // 最旧的被截掉：首条 epoch 的 off = 第 16 次变化（=16 字节处）
        assert_eq!(r.epochs[0].off, 16);
        assert_eq!(r.epochs[0].size.cols(), 96);
    }
}
