//! term — 出口侧终端服务（R6）。
//!
//! 分层（设计见 `docs/reviews/R6-design.md`）：
//! - [`vt`]：会话屏态的仿真底座（alacritty_terminal 0.26 适配层——坐标/模式位/脏行
//!   读取与 cell 归一化都在这一处换算，上层只见 Go `pkg/term/vt` 同形的视口模型）；
//! - [`responder`]：自建应答器（DA/DSR/DECRQM/kitty/OSC 颜色查询的 ghostty 同形应答；
//!   行为真源 = fixtures/vectors/term_responder.json）；
//! - [`keyenc`]：自建键/鼠标/焦点/粘贴编码器（行为真源 =
//!   fixtures/vectors/term_{keyenc,mouseenc}.json）；
//! - [`frames`]：term 帧协议编解码（op 表/词表面/HELLO 尾随块/INPUT 上行；
//!   判据 = fixtures/term/frames.v1.jsonl 冻结契约）；
//! - [`codec`]：surface v5 / cell v2 体编码（cell 三标记流/SNAPSHOT/DIFF/FETCH-ROWS/分片/gzip；
//!   判据 = fixtures/surface-current/codec.json + fixtures/surface-current/ 夹具）；
//! - [`manifest`]：agent 状态识别规则的数据驱动引擎（region 切片/谓词求值/优先级
//!   仲裁 + 内嵌 23 文件与本地覆盖；判据 = fixtures/vectors/term_manifest_eval.json）；
//! - [`scan`]：旁路扫描器（只读不消费字节）——标题单一来源/OSC 9 双语义/OSC 21337
//!   直报/legacy 模式位，能吃跨 read 切断的序列；
//! - [`agent`]：检测融合（身份/输出/CPU/屏幕/直报五路权威序 + 状态机卫生 +
//!   平台进程表）；
//! - [`pty`]：会话子进程装配（登录 shell 解析链/环境白名单/spawn/resize/尺寸哨兵/
//!   SIGHUP→SIGKILL）；
//! - [`ring`]：有界输出环（定长环/绝对偏移读/回放起点对齐/epoch 表）；
//! - [`size`]：网格尺寸值对象（非 0 + 上限 1000×500 的不变量进类型——P0-3 夹取面）；
//! - [`session`]：会话注册表与腿接入语义（多腿注册序/ENDED 词表应用/活动选举；
//!   纯状态机——PTY/泵/写者接线在 6f）；
//! - 其余模块（service/leg/surface——会话装配与投递编排）按拆步 6f-3b 陆续就位。
//!
//! 终端 wire 仅支持当前版本；冻结 baseline 只作历史资料，不参与旧协议解码。

pub mod agent;
pub mod codec;
pub(crate) mod cwd;
pub mod frames;
pub mod legout;
pub mod manifest;
pub mod session;
pub mod keyenc;
pub mod pty;
pub mod responder;
pub mod ring;
pub mod scan;
pub mod service;
pub mod size;
pub mod vt;
pub mod wire;

/// 跨模块共享的测试件：golden 夹具读取与 digest 口径（只在测试构建编译）。
///
/// 当前 golden 摘要集中一处，避免 vt/codec 两份实现漂移。
#[cfg(test)]
pub(crate) mod testutil {
    use crate::term::vt::{Cell, Color, Row};

    /// 当前 surface v5 / cell v2 定值向量。
    pub const VECTOR_SURFACE_CODEC: &str =
        include_str!("../../../../fixtures/surface-current/codec.json");

    fn fixtures_dir() -> String {
        format!("{}/../../fixtures", env!("CARGO_MANIFEST_DIR"))
    }

    pub fn read_current_frames(key: &str) -> Vec<(u8, Vec<u8>)> {
        let frames: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../fixtures/surface-current/frames.json"
        )).expect("current surface frames");
        frames[key].as_array().expect("current frame case").iter().map(|frame| {
            let op = frame["op"].as_u64().expect("frame op") as u8;
            let payload = hex::decode(frame["hex"].as_str().expect("frame hex")).expect("frame bytes");
            (op, payload)
        }).collect()
    }

    /// 读 golden manifest（name → 各列）。
    pub fn golden_manifest() -> Vec<Vec<String>> {
        let raw =
            std::fs::read_to_string(format!("{}/surface-current/manifest.tsv", fixtures_dir()))
                .expect("golden manifest");
        raw.lines()
            .filter(|l| !l.is_empty())
            .map(|l| l.split('\t').map(|s| s.to_string()).collect())
            .collect()
    }

    /// 读上行字节表 `surface-current/input.tsv`（theme/clipboard 四案）：
    /// `name <TAB> kind <TAB> fields(逗号分隔) <TAB> hex`。
    #[allow(clippy::type_complexity)]
    pub fn read_input_cases() -> Vec<(String, String, Vec<String>, String)> {
        let raw = std::fs::read_to_string(format!(
            "{}/surface-current/input.tsv",
            fixtures_dir()
        ))
        .expect("surface_input_cases.tsv");
        raw.lines()
            .filter(|l| !l.is_empty())
            .map(|l| {
                let mut parts = l.split('\t');
                let name = parts.next().unwrap_or_default().to_string();
                let kind = parts.next().unwrap_or_default().to_string();
                let fields = parts
                    .next()
                    .unwrap_or_default()
                    .split(',')
                    .map(|s| s.to_string())
                    .collect();
                let hexcol = parts.next().unwrap_or_default().to_string();
                (name, kind, fields, hexcol)
            })
            .collect()
    }

    /// Go goldenDigest 同款：FNV-1a 64 → 16 位小写 hex。
    pub fn fnv_hex(data: &[u8]) -> String {
        use std::hash::Hasher;
        let mut h = fnv::FnvHasher::default();
        h.write(data);
        format!("{:016x}", h.finish())
    }

    /// 样式专项夹具（Go goldenStylesSession 同款内联序列）：SGR 组合属性/256 色/真彩/
    /// 基础 16 色/宽字符/鼠标上报/括号粘贴/光标隐藏——录制夹具碰不齐的面全点亮。
    pub fn golden_styles_session() -> Vec<u8> {
        "\u{1b}[2J\u{1b}[H\u{1b}[1;3;4;7mBOLD\u{1b}[0m\u{1b}[2;9mDIM-STRIKE\u{1b}[0m\u{1b}[38;5;196mPAL256\u{1b}[0m \u{1b}[48;2;10;20;30mRGBBG\u{1b}[0m \u{1b}[31;44mRED-BLUE\u{1b}[0m \u{5bbd}\u{5b57}\r\n\u{1b}[?1000h\u{1b}[?1002h\u{1b}[?1006h\u{1b}[?2004h\u{1b}[?1049lSTYLES-OK"
            .as_bytes()
            .to_vec()
    }

    /// Go gridTextLines 口径：占位格跳过、空符号补空格、行尾 TrimRight(" ")、\n 连接。
    pub fn text_of(rows: &[Row]) -> String {
        rows.iter()
            .map(|r| {
                let mut s = String::new();
                for c in &r.cells {
                    if c.skip {
                        continue;
                    }
                    if c.symbol.is_empty() {
                        s.push(' ');
                    } else {
                        s.push_str(&c.symbol);
                    }
                }
                s.trim_end_matches(' ').to_string()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// 当前样式摘要口径：逐行逐格 26 个 hex（fg 5B · bg 5B · attr 2B · flags 1B）。
    /// 缺格按空白格（Width=1），显式记录真实 0/1/2 宽度。
    pub fn style_of(rows: &[Row], cols: usize) -> String {
        fn color_hex(c: Color) -> [u8; 5] {
            match c {
                Color::None => [0, 0, 0, 0, 0],
                Color::Palette(i) => [1, i, 0, 0, 0],
                Color::Rgb(r, g, b) => [2, 0, r, g, b],
            }
        }
        let mut out = String::new();
        for r in rows {
            for x in 0..cols {
                let blank = Cell { width: 1, ..Cell::default() };
                let c = r.cells.get(x).unwrap_or(&blank);
                let mut flags = 0u8;
                if c.skip {
                    flags |= 1;
                }
                flags |= c.width << 1;
                let fg = color_hex(c.fg);
                let bg = color_hex(c.bg);
                for b in fg.iter().chain(bg.iter()) {
                    out.push_str(&format!("{b:02x}"));
                }
                out.push_str(&format!("{:04x}{:02x}", c.attr, flags));
            }
            out.push('\n');
        }
        out
    }
}
