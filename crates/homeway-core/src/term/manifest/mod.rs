//! manifest — agent 状态识别规则的数据驱动引擎（R6 6f；行为真源 = baseline 克隆
//! `pkg/term/manifest/`，判据 = `fixtures/vectors/term_manifest_eval.json` 逐规则对拍）。
//!
//! 引擎三件事：**解析 + 校验**（本文件）、**region 切片**（[`region`]）、**谓词求值与
//! priority 仲裁**（本文件）。规则 TOML = `fixtures/term-manifests/`（23 文件单真源，
//! include_str! 直嵌），本地覆盖 `<state>/agent-detection/<id>.toml` 永远优先。
//!
//! 校验上限照 herdr 复杂度门（防病态/恶意覆盖文件拖死出口）：规则 ≤128、门深 ≤8、
//! 门总数 ≤512、单门匹配器 ≤32、匹配器总数 ≤1024、单模式 ≤512 字符、正则必须可编译。
//! 任一失败 ⇒ 整份忽略 + 告警，绝不因此退出进程。
//!
//! 正则方言：manifest 按 Rust regex 方言写，本引擎把语义收窄到 **Go 出口的实跑行为**
//! （`\p{Alphabetic}`→`\p{L}` 近似 + Perl 类转义 ASCII 展开 + `\b` ASCII 词界），
//! 见 [`dialect`] 模块头——与 Go 的 dialect.go 垫片配对成「同文件、同行为」。

pub mod dialect;
pub mod region;

use std::collections::HashMap;
use std::path::Path;

/// 引擎能力版本（manifest 声明更高的 `min_engine_version` 时拒载）。
/// 3 = 支持 `top_non_empty_lines(N)`。
pub const ENGINE_VERSION: u32 = 3;

/// 复杂度上限（与 herdr 同值）。
pub const MAX_RULES_PER_MANIFEST: usize = 128;
pub const MAX_GATE_DEPTH: usize = 8;
pub const MAX_TOTAL_GATES: usize = 512;
pub const MAX_MATCHERS_PER_GATE: usize = 32;
pub const MAX_TOTAL_MATCHERS: usize = 1024;
pub const MAX_MATCHER_CHARS: usize = 512; // 字符数（rune 数）口径

/// 无命中且 agent 已知时的回落标签。
pub const DEFAULT_KNOWN_AGENT_IDLE_FALLBACK: &str = "default_known_agent_idle_fallback";

/// 覆盖目录名（相对出口 state 目录）。
pub const OVERRIDE_DIR_NAME: &str = "agent-detection";

/// 规则声明的状态（字节值 = 词表面 wire 形态）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum State {
    #[default]
    Unknown = 0,
    Idle = 1,
    Working = 2,
    Blocked = 3,
}

impl State {
    /// 日志/explain 用词面（与 TOML 取值同名）。
    pub fn as_str(self) -> &'static str {
        match self {
            State::Idle => "idle",
            State::Working => "working",
            State::Blocked => "blocked",
            State::Unknown => "unknown",
        }
    }

    fn parse(v: &str) -> Result<State, String> {
        match v {
            "idle" => Ok(State::Idle),
            "working" => Ok(State::Working),
            "blocked" => Ok(State::Blocked),
            "unknown" => Ok(State::Unknown),
            other => Err(format!("未知 state {other:?}")),
        }
    }
}

/// manifest 的来源（日志与 explain 标注）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Embedded,
    Override,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Source::Embedded => "embedded",
            Source::Override => "override",
        }
    }
}

/// 一份 agent 规则集。
#[derive(Debug, Clone)]
pub struct Manifest {
    pub id: String,
    pub version: String,
    pub min_engine_version: u32,
    pub aliases: Vec<String>,
    pub rules: Vec<Rule>,
    pub source: Source,
}

/// 一条规则。
#[derive(Debug, Clone)]
pub struct Rule {
    pub id: String,
    pub state: State,
    pub priority: i32,
    pub region: String,
    pub visible_idle: bool,
    pub visible_blocker: bool,
    pub visible_working: bool,
    pub skip_state_update: bool,
    pub gate: Gate,
}

/// 一个谓词门。求值语义（照 herdr）：
/// - contains：**全部**子串出现（大小写不敏感）
/// - regex：**全部**模式命中整段 region 文本
/// - line_regex：**全部**模式命中**至少一行**
/// - all：全部子门命中；any：非空时至少一个命中；not：任一子门命中即整体不命中
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Gate {
    pub all: Vec<Gate>,
    pub any: Vec<Gate>,
    pub not: Vec<Gate>,
    pub contains: Vec<String>,
    pub regex: Vec<String>,
    pub line_regex: Vec<String>,
}

impl Gate {
    /// 是否有「正向」匹配器（规则必须有；只有 not 不算）。
    fn has_positive_matcher(&self) -> bool {
        !self.contains.is_empty()
            || !self.regex.is_empty()
            || !self.line_regex.is_empty()
            || !self.all.is_empty()
            || !self.any.is_empty()
    }

    /// 本门（含 not）是否有任何匹配器。
    fn has_any_matcher(&self) -> bool {
        self.has_positive_matcher() || !self.not.is_empty()
    }
}

/// manifest 不合法（加载方应整份忽略 + 告警，不退出）。
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ManifestError {
    #[error("manifest: TOML 解析失败：{0}")]
    Toml(String),
    #[error("manifest: 非法：{0}")]
    Invalid(String),
}

// ---------------------------------------------------------------------------
// TOML 形状（deny_unknown_fields = Go 的 Undecoded() 检查：拼错的键静默忽略会让
// 规则「看起来生效但永远不命中」，必须拒载）
// ---------------------------------------------------------------------------

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RawManifest {
    id: String,
    #[serde(default)]
    version: String,
    #[serde(default)]
    min_engine_version: u32,
    #[serde(default)]
    #[allow(dead_code)] // 与 Go rawManifest.updated_at 同：认这个键但不进引擎
    updated_at: String,
    #[serde(default)]
    aliases: Vec<String>,
    #[serde(default)]
    rules: Vec<RawRule>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRule {
    #[serde(default)]
    id: String,
    #[serde(default)]
    state: String,
    #[serde(default)]
    priority: i32,
    #[serde(default)]
    region: String,
    #[serde(default)]
    visible_idle: bool,
    #[serde(default)]
    visible_blocker: bool,
    #[serde(default)]
    visible_working: bool,
    #[serde(default)]
    skip_state_update: bool,
    #[serde(default)]
    all: Vec<RawGate>,
    #[serde(default)]
    any: Vec<RawGate>,
    #[serde(default)]
    not: Vec<RawGate>,
    #[serde(default)]
    contains: Vec<String>,
    #[serde(default)]
    regex: Vec<String>,
    #[serde(default)]
    line_regex: Vec<String>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RawGate {
    #[serde(default)]
    all: Vec<RawGate>,
    #[serde(default)]
    any: Vec<RawGate>,
    #[serde(default)]
    not: Vec<RawGate>,
    #[serde(default)]
    contains: Vec<String>,
    #[serde(default)]
    regex: Vec<String>,
    #[serde(default)]
    line_regex: Vec<String>,
}

impl RawGate {
    fn into_gate(self) -> Gate {
        Gate {
            all: self.all.into_iter().map(Self::into_gate).collect(),
            any: self.any.into_iter().map(Self::into_gate).collect(),
            not: self.not.into_iter().map(Self::into_gate).collect(),
            contains: self.contains,
            regex: self.regex,
            line_regex: self.line_regex,
        }
    }
}

/// 解析 TOML 并校验（含复杂度上限与正则编译）。
pub fn parse(data: &str, src: Source) -> Result<Manifest, ManifestError> {
    let raw: RawManifest =
        toml::from_str(data).map_err(|e| ManifestError::Toml(e.to_string()))?;
    let m = raw_to_manifest(raw, src)?;
    m.validate()?;
    Ok(m)
}

fn raw_to_manifest(raw: RawManifest, src: Source) -> Result<Manifest, ManifestError> {
    let mut rules = Vec::with_capacity(raw.rules.len());
    for rr in raw.rules {
        let state = if rr.state.is_empty() {
            State::Unknown
        } else {
            State::parse(&rr.state).map_err(|e| {
                ManifestError::Invalid(format!("规则 {}：{e}", rr.id))
            })?
        };
        let region = if rr.region.trim().is_empty() {
            region::DEFAULT_REGION.to_string()
        } else {
            rr.region
        };
        rules.push(Rule {
            id: rr.id,
            state,
            priority: rr.priority,
            region,
            visible_idle: rr.visible_idle,
            visible_blocker: rr.visible_blocker,
            visible_working: rr.visible_working,
            skip_state_update: rr.skip_state_update,
            gate: Gate {
                all: rr.all.into_iter().map(RawGate::into_gate).collect(),
                any: rr.any.into_iter().map(RawGate::into_gate).collect(),
                not: rr.not.into_iter().map(RawGate::into_gate).collect(),
                contains: rr.contains,
                regex: rr.regex,
                line_regex: rr.line_regex,
            },
        });
    }
    Ok(Manifest {
        id: raw.id,
        version: raw.version,
        min_engine_version: raw.min_engine_version,
        aliases: raw.aliases,
        rules,
        source: src,
    })
}

#[derive(Default)]
struct Complexity {
    gates: usize,
    matchers: usize,
}

impl Manifest {
    /// 校验整份 manifest（规则数、region 名、门复杂度、正则可编译）。
    pub fn validate(&self) -> Result<(), ManifestError> {
        if self.id.is_empty() {
            return Err(ManifestError::Invalid("缺少 id".into()));
        }
        if self.rules.is_empty() {
            return Err(ManifestError::Invalid("至少要有一条规则".into()));
        }
        if self.rules.len() > MAX_RULES_PER_MANIFEST {
            return Err(ManifestError::Invalid(format!(
                "规则数 {} 超过上限 {MAX_RULES_PER_MANIFEST}",
                self.rules.len()
            )));
        }
        let mut c = Complexity::default();
        for (i, r) in self.rules.iter().enumerate() {
            if r.id.trim().is_empty() {
                return Err(ManifestError::Invalid(format!("第 {} 条规则缺 id", i + 1)));
            }
            if r.skip_state_update
                && (r.state != State::Unknown
                    || r.visible_idle
                    || r.visible_blocker
                    || r.visible_working)
            {
                return Err(ManifestError::Invalid(format!(
                    "规则 {} 用了 skip_state_update 但 state 不是 unknown 或带可见证据",
                    r.id
                )));
            }
            region::validate_region_name(&r.region)
                .map_err(|e| ManifestError::Invalid(format!("规则 {} 的 region 非法：{e}", r.id)))?;
            if r.region.trim().starts_with("top_non_empty_lines(")
                && self.min_engine_version != 0
                && self.min_engine_version < region::TOP_NON_EMPTY_LINES_ENGINE_VERSION
            {
                return Err(ManifestError::Invalid(format!(
                    "规则 {} 用了 top_non_empty_lines 但 min_engine_version < {}",
                    r.id, region::TOP_NON_EMPTY_LINES_ENGINE_VERSION
                )));
            }
            validate_gate(&r.gate, "rule", 0, &mut c, true)
                .map_err(|e| ManifestError::Invalid(format!("规则 {} 的门非法：{e}", r.id)))?;
        }
        Ok(())
    }
}

fn validate_gate(
    g: &Gate,
    ctx: &str,
    depth: usize,
    c: &mut Complexity,
    positive_required: bool,
) -> Result<(), String> {
    if depth > MAX_GATE_DEPTH {
        return Err(format!("{ctx} 嵌套超过上限 {MAX_GATE_DEPTH}"));
    }
    c.gates += 1;
    if c.gates > MAX_TOTAL_GATES {
        return Err(format!("门总数超过上限 {MAX_TOTAL_GATES}"));
    }
    let n = g.contains.len() + g.regex.len() + g.line_regex.len();
    if n > MAX_MATCHERS_PER_GATE {
        return Err(format!("{ctx} 直接匹配器 {n} 个，超过上限 {MAX_MATCHERS_PER_GATE}"));
    }
    c.matchers += n;
    if c.matchers > MAX_TOTAL_MATCHERS {
        return Err(format!("匹配器总数超过上限 {MAX_TOTAL_MATCHERS}"));
    }
    for v in g.contains.iter().chain(&g.regex).chain(&g.line_regex) {
        if v.chars().count() > MAX_MATCHER_CHARS {
            return Err(format!("{ctx} 有匹配器超过长度上限 {MAX_MATCHER_CHARS}"));
        }
    }
    if positive_required && !g.has_positive_matcher() {
        return Err(format!("{ctx} 必须含正向匹配器（只有 not 不算）"));
    }
    for field in [&g.regex, &g.line_regex] {
        for p in field {
            dialect::compile_pattern(p)
                .map_err(|e| format!("{ctx} 的模式 {p:?} 编译失败：{e}"))?;
        }
    }
    for n in g.all.iter().chain(&g.any) {
        validate_gate(n, "all 门", depth + 1, c, true)?;
    }
    for n in &g.not {
        if !n.has_any_matcher() {
            return Err(format!("{ctx} 含空的 not 门"));
        }
        // not 门内部不要求正向匹配器（语义就是「不得命中」）。
        validate_gate(n, "not 门", depth + 1, c, false)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// 编译 + 求值
// ---------------------------------------------------------------------------

#[derive(Default)]
struct CompiledGate {
    all: Vec<CompiledGate>,
    any: Vec<CompiledGate>,
    not: Vec<CompiledGate>,
    contains: Vec<String>, // 已小写
    regex: Vec<regex::Regex>,
    line_regex: Vec<regex::Regex>,
}

impl CompiledGate {
    fn compile(g: &Gate) -> Result<Self, String> {
        let mut out = CompiledGate {
            contains: g.contains.iter().map(|s| s.to_lowercase()).collect(),
            ..CompiledGate::default()
        };
        for p in &g.regex {
            out.regex
                .push(dialect::compile_pattern(p).map_err(|e| e.to_string())?);
        }
        for p in &g.line_regex {
            out.line_regex
                .push(dialect::compile_pattern(p).map_err(|e| e.to_string())?);
        }
        for n in &g.all {
            out.all.push(Self::compile(n)?);
        }
        for n in &g.any {
            out.any.push(Self::compile(n)?);
        }
        for n in &g.not {
            out.not.push(Self::compile(n)?);
        }
        Ok(out)
    }

    fn matches(&self, text: &str, lower: &str) -> bool {
        for needle in &self.contains {
            if !lower.contains(needle.as_str()) {
                return false;
            }
        }
        for re in &self.regex {
            if !re.is_match(text) {
                return false;
            }
        }
        for re in &self.line_regex {
            let hit = text.split('\n').any(|line| re.is_match(line));
            if !hit {
                return false;
            }
        }
        for n in &self.all {
            if !n.matches(text, lower) {
                return false;
            }
        }
        if !self.any.is_empty() && !self.any.iter().any(|n| n.matches(text, lower)) {
            return false;
        }
        for n in &self.not {
            if n.matches(text, lower) {
                return false;
            }
        }
        true
    }
}

/// 一条规则的评估轨迹（explain 用）。
#[derive(Debug, Clone)]
pub struct EvaluatedRule {
    pub id: String,
    pub priority: i32,
    pub region: String,
    pub state: State,
    pub matched: bool,
    /// 本次判定的 region 文本字节数（诊断「region 切空了」）。
    pub region_bytes: usize,
}

/// 一次判定的结果。
#[derive(Debug, Clone)]
pub struct Result_ {
    /// 最终状态。无命中且 agent 已知时回落 idle（`fallback_reason` 非空）。
    pub state: State,
    /// 命中的规则（无命中时 None）。
    pub matched_rule: Option<Rule>,
    pub visible_idle: bool,
    pub visible_blocker: bool,
    pub visible_working: bool,
    pub skip_state_update: bool,
    /// 回落标签（非空 = 走了回落）。
    pub fallback_reason: String,
    /// 全部规则的评估轨迹。
    pub rules: Vec<EvaluatedRule>,
}

/// 一份编译好的 manifest（求值入口；正则只编一次）。
pub struct Compiled {
    pub manifest: Manifest,
    gates: Vec<CompiledGate>,
}

impl Compiled {
    /// 编译规则里的正则（解析阶段已校验过可编译）。
    pub fn compile(m: Manifest) -> Result<Self, String> {
        let mut gates = Vec::with_capacity(m.rules.len());
        for r in &m.rules {
            gates.push(CompiledGate::compile(&r.gate)?);
        }
        Ok(Compiled { manifest: m, gates })
    }

    /// 求值：按**文件序**遍历规则，priority **严格大于**当前命中者才替换
    /// ⇒ 最高 priority 胜、同分取文件序靠前。
    pub fn evaluate(&self, in_: &region::Input) -> Result_ {
        let mut best: Option<usize> = None;
        let mut best_priority = i32::MIN;
        let mut trace = Vec::with_capacity(self.manifest.rules.len());
        for (i, r) in self.manifest.rules.iter().enumerate() {
            let region_text = region::region(in_, &r.region);
            let lower = region_text.to_lowercase();
            let matched = self.gates[i].matches(&region_text, &lower);
            trace.push(EvaluatedRule {
                id: r.id.clone(),
                priority: r.priority,
                region: r.region.clone(),
                state: r.state,
                matched,
                region_bytes: region_text.len(),
            });
            if !matched {
                continue;
            }
            if best.is_some() && best_priority >= r.priority {
                continue;
            }
            best = Some(i);
            best_priority = r.priority;
        }
        match best {
            None => Result_ {
                state: State::Idle,
                fallback_reason: DEFAULT_KNOWN_AGENT_IDLE_FALLBACK.to_string(),
                rules: trace,
                ..Result_::default()
            },
            Some(i) => {
                let r = &self.manifest.rules[i];
                Result_ {
                    state: r.state,
                    matched_rule: Some(r.clone()),
                    visible_idle: r.visible_idle && r.state == State::Idle,
                    visible_blocker: r.visible_blocker && r.state == State::Blocked,
                    visible_working: r.visible_working && r.state == State::Working,
                    skip_state_update: r.skip_state_update,
                    fallback_reason: String::new(),
                    rules: trace,
                }
            }
        }
    }

    /// 只回答「有没有命中 skip_state_update 规则」。
    pub fn should_skip_state_update(&self, in_: &region::Input) -> bool {
        self.evaluate(in_).skip_state_update
    }
}

impl Default for Result_ {
    fn default() -> Self {
        Result_ {
            state: State::Unknown,
            matched_rule: None,
            visible_idle: false,
            visible_blocker: false,
            visible_working: false,
            skip_state_update: false,
            fallback_reason: String::new(),
            rules: Vec::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// 加载（内嵌 23 文件 + 本地覆盖优先）
// ---------------------------------------------------------------------------

/// 内嵌 manifest 表（`fixtures/term-manifests/` 单真源直嵌；名字 = Go go:embed 的
/// `manifests/*.toml` 相对路径）。升级规则 = 换 fixtures 目录内容 + 本表同步。
const EMBEDDED: &[(&str, &str)] = &[
    ("index.toml", include_str!("../../../../../fixtures/term-manifests/index.toml")),
    ("amp.toml", include_str!("../../../../../fixtures/term-manifests/amp.toml")),
    ("antigravity.toml", include_str!("../../../../../fixtures/term-manifests/antigravity.toml")),
    ("claude.toml", include_str!("../../../../../fixtures/term-manifests/claude.toml")),
    ("cline.toml", include_str!("../../../../../fixtures/term-manifests/cline.toml")),
    ("codex.toml", include_str!("../../../../../fixtures/term-manifests/codex.toml")),
    ("cursor.toml", include_str!("../../../../../fixtures/term-manifests/cursor.toml")),
    ("devin.toml", include_str!("../../../../../fixtures/term-manifests/devin.toml")),
    ("droid.toml", include_str!("../../../../../fixtures/term-manifests/droid.toml")),
    ("gemini.toml", include_str!("../../../../../fixtures/term-manifests/gemini.toml")),
    ("github-copilot.toml", include_str!("../../../../../fixtures/term-manifests/github-copilot.toml")),
    ("grok.toml", include_str!("../../../../../fixtures/term-manifests/grok.toml")),
    ("hermes.toml", include_str!("../../../../../fixtures/term-manifests/hermes.toml")),
    ("kilo.toml", include_str!("../../../../../fixtures/term-manifests/kilo.toml")),
    ("kimi.toml", include_str!("../../../../../fixtures/term-manifests/kimi.toml")),
    ("kiro.toml", include_str!("../../../../../fixtures/term-manifests/kiro.toml")),
    ("letta.toml", include_str!("../../../../../fixtures/term-manifests/letta.toml")),
    ("maki.toml", include_str!("../../../../../fixtures/term-manifests/maki.toml")),
    ("muse.toml", include_str!("../../../../../fixtures/term-manifests/muse.toml")),
    ("opencode.toml", include_str!("../../../../../fixtures/term-manifests/opencode.toml")),
    ("pi.toml", include_str!("../../../../../fixtures/term-manifests/pi.toml")),
    ("qodercli.toml", include_str!("../../../../../fixtures/term-manifests/qodercli.toml")),
    ("qwen.toml", include_str!("../../../../../fixtures/term-manifests/qwen.toml")),
];

fn embedded_file(name: &str) -> Option<&'static str> {
    EMBEDDED.iter().find(|(n, _)| *n == name).map(|(_, c)| *c)
}

#[derive(serde::Deserialize)]
struct IndexEntry {
    id: String,
    path: String,
    #[serde(default)]
    processes: Vec<String>,
}

#[derive(serde::Deserialize, Default)]
struct IndexFile {
    #[serde(default)]
    #[allow(dead_code)]
    schema_version: i32,
    #[serde(default)]
    agents: Vec<IndexEntry>,
}

/// manifest 表（不可变快照；重载 = 换新表）。加载失败的文件进 warnings，不退出。
#[derive(Default)]
pub struct Loader {
    by_id: HashMap<String, Compiled>,
    by_proc: HashMap<String, String>, // 进程名（小写 basename）→ id
    warnings: Vec<String>,
}

impl Loader {
    /// 建加载器并立即加载一次。`override_dir` = None 时只用内嵌版本。
    pub fn new(override_dir: Option<&Path>) -> Self {
        let mut l = Loader::default();
        l.reload(override_dir);
        l
    }

    /// 重建整张表（失败的文件进 warnings）。
    pub fn reload(&mut self, override_dir: Option<&Path>) {
        let mut by_id = HashMap::new();
        let mut by_proc = HashMap::new();
        let mut warnings = Vec::new();

        let idx = match read_index() {
            Ok(idx) => idx,
            Err(e) => {
                // 索引坏了是内嵌产物的问题（不可能来自用户），如实告警并让表为空。
                warnings.push(format!("index.toml 加载失败：{e}"));
                IndexFile::default()
            }
        };

        for e in idx.agents {
            match load_one(override_dir, &e) {
                (Some(comp), None) => {
                    for p in &e.processes {
                        by_proc.insert(p.trim().to_lowercase(), e.id.clone());
                    }
                    by_id.insert(e.id.clone(), comp);
                }
                (comp, warn) => {
                    if let Some(comp) = comp {
                        for p in &e.processes {
                            by_proc.insert(p.trim().to_lowercase(), e.id.clone());
                        }
                        by_id.insert(e.id.clone(), comp);
                    }
                    if let Some(w) = warn {
                        warnings.push(w);
                    }
                }
            }
        }

        self.by_id = by_id;
        self.by_proc = by_proc;
        self.warnings = warnings;
    }

    /// 按 agent id 取（explain / 诊断用）。
    pub fn for_id(&self, id: &str) -> Option<&Compiled> {
        self.by_id.get(id)
    }

    /// 按**前台进程名**取（出口主入口）：basename 归一 + 小写后查表。
    /// 认不出返回 None——调用方按「未知 CLI」降级，不做任何猜测。
    pub fn for_process(&self, name: &str) -> Option<&Compiled> {
        let key = base_name(name).trim().to_lowercase();
        if key.is_empty() {
            return None;
        }
        let id = self.by_proc.get(&key)?;
        self.for_id(id)
    }

    /// 已加载的 agent id（稳定排序）。
    pub fn ids(&self) -> Vec<&str> {
        let mut out: Vec<&str> = self.by_id.keys().map(String::as_str).collect();
        out.sort_unstable();
        out
    }

    /// 本次加载的告警（空 = 全部干净）。
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }
}

/// 取路径最后一段（`/usr/local/bin/codex` → `codex`；`/` 与 `\` 都算分隔）。
fn base_name(p: &str) -> String {
    let p = p.trim();
    match p.rfind(['/', '\\']) {
        Some(i) => p[i + 1..].to_string(),
        None => p.to_string(),
    }
}

fn read_index() -> Result<IndexFile, String> {
    let data = embedded_file("index.toml").ok_or("内嵌 index.toml 缺失")?;
    let idx: IndexFile = toml::from_str(data).map_err(|e| e.to_string())?;
    if idx.agents.is_empty() {
        return Err("index.toml 里没有 agents".into());
    }
    Ok(idx)
}

/// 按「覆盖 → 内嵌」取一份并编译。返回 (编译产物, 告警)。
fn load_one(
    override_dir: Option<&Path>,
    e: &IndexEntry,
) -> (Option<Compiled>, Option<String>) {
    let embedded_data = match embedded_file(&e.path) {
        Some(d) => d,
        None => return (None, Some(format!("内嵌 manifest {} 缺失", e.path))),
    };

    if let Some(dir) = override_dir {
        let ov_path = dir.join(format!("{}.toml", e.id));
        if let Ok(data) = std::fs::read_to_string(&ov_path) {
            match parse_then_compile(&data, Source::Override) {
                Ok(comp) => return (Some(comp), None),
                Err(perr) => {
                    // 覆盖文件坏了：告警 + 回落内嵌。
                    let warn = format!("覆盖文件 {} 被忽略（{perr}），回落内嵌版本", ov_path.display());
                    match parse_then_compile(embedded_data, Source::Embedded) {
                        Ok(comp) => return (Some(comp), Some(warn)),
                        Err(ierr) => {
                            return (None, Some(format!("{warn}；内嵌版本也不可用：{ierr}")))
                        }
                    }
                }
            }
        }
    }

    match parse_then_compile(embedded_data, Source::Embedded) {
        Ok(comp) => (Some(comp), None),
        Err(ierr) => (None, Some(format!("内嵌 manifest {} 不可用：{ierr}", e.path))),
    }
}

fn parse_then_compile(data: &str, src: Source) -> Result<Compiled, String> {
    let m = parse(data, src).map_err(|e| e.to_string())?;
    if m.min_engine_version > ENGINE_VERSION {
        return Err(format!(
            "声明 min_engine_version={}，高于本引擎 {ENGINE_VERSION}",
            m.min_engine_version
        ));
    }
    Compiled::compile(m).map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// 测试：term_manifest_eval.json 逐规则对拍（region 40 案 + startup 夹具 44 案）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn vecfile() -> serde_json::Value {
        let raw = include_str!("../../../../../fixtures/vectors/term_manifest_eval.json");
        serde_json::from_str(raw).expect("term_manifest_eval.json invalid")
    }

    /// 内嵌 22 份全载 + 进程映射 + 判据计数（「检测规则已加载 22 份」的计数真源）。
    #[test]
    fn manifest_loader_embedded_all() {
        let l = Loader::new(None);
        assert!(l.warnings().is_empty(), "内嵌 manifest 不该有告警：{:?}", l.warnings());
        assert_eq!(l.ids().len(), 22, "检测规则 22 份");
        assert_eq!(l.ids()[0], "agy", "稳定排序");
        // 进程名映射：basename 归一 + 带空格的 runner 形态
        assert!(l.for_process("/usr/local/bin/codex").is_some());
        assert!(l.for_process("GitHub-Copilot").is_some());
        assert!(l.for_process("kimi code").is_some());
        assert!(l.for_process("unknown-cli").is_none());
        assert!(l.for_process("").is_none());
        for id in l.ids() {
            let c = l.for_id(id).expect("ids 表内必有");
            assert!(!c.manifest.rules.is_empty(), "{id} 规则空");
        }
    }

    /// ① region 逐函数对拍（手造屏 × 全部 region 名 → 切片文本）。
    #[test]
    fn manifest_region_parity_with_go_vectors() {
        let v = vecfile();
        let cases = v.get("regions").and_then(|c| c.as_array()).expect("regions");
        assert!(cases.len() >= 40, "region 案数 {}", cases.len());
        for c in cases {
            let name = c.get("name").and_then(|x| x.as_str()).expect("name");
            let spec = c.get("region").and_then(|x| x.as_str()).expect("region");
            let want = c.get("want").and_then(|x| x.as_str()).unwrap_or("");
            let in_ = region::Input {
                screen: c.get("screen").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                osc_title: c.get("osc_title").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                osc_progress: c
                    .get("osc_progress")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string(),
            };
            let got = region::region(&in_, spec);
            assert_eq!(got, want, "{name}/{spec}");
        }
    }

    /// ② startup 夹具 × 22 manifest 的**逐规则**对拍（region/region_bytes/matched/
    /// priority/state + 终局状态/可见位/回落——评审 A2 口径：不只对终局）。
    #[test]
    fn manifest_eval_parity_with_go_vectors() {
        let v = vecfile();
        let l = Loader::new(None);
        let cases = v.get("cases").and_then(|c| c.as_array()).expect("cases");
        assert_eq!(cases.len(), 44, "2 夹具 × 22 manifest");
        // 夹具文本从向量自带（与 clone testdata 同源同字节）
        let mut fixtures: HashMap<String, String> = HashMap::new();
        for c in cases {
            fixtures
                .entry(c.get("fixture").and_then(|x| x.as_str()).expect("fixture").to_string())
                .or_insert_with(|| {
                    read_fixture_from_repo(c.get("fixture").and_then(|x| x.as_str()).unwrap())
                });
        }
        for c in cases {
            let agent = c.get("agent").and_then(|x| x.as_str()).expect("agent");
            let fixture = c.get("fixture").and_then(|x| x.as_str()).expect("fixture");
            let comp = l.for_id(agent).unwrap_or_else(|| panic!("{agent} 缺"));
            let screen = fixtures.get(fixture).expect("fixture 文本");
            let res = comp.evaluate(&region::Input { screen: screen.clone(), ..Default::default() });
            // 逐规则轨迹
            let want_rules = c.get("rules").and_then(|x| x.as_array()).expect("rules");
            assert_eq!(res.rules.len(), want_rules.len(), "{agent}/{fixture} 规则数");
            for (got, want) in res.rules.iter().zip(want_rules) {
                let gid = got.id.as_str();
                let wid = want.get("id").and_then(|x| x.as_str()).expect("id");
                assert_eq!(gid, wid, "{agent}/{fixture} 规则序（文件序契约）");
                assert_eq!(got.region, want.get("region").and_then(|x| x.as_str()).expect("region"));
                assert_eq!(
                    got.matched,
                    want.get("matched").and_then(|x| x.as_bool()).expect("matched"),
                    "{agent}/{fixture}/{gid} matched"
                );
                assert_eq!(
                    got.region_bytes,
                    want.get("region_bytes").and_then(|x| x.as_u64()).expect("region_bytes") as usize,
                    "{agent}/{fixture}/{gid} region_bytes"
                );
                assert_eq!(got.priority as i64, want.get("priority").and_then(|x| x.as_i64()).expect("priority"));
                assert_eq!(got.state as u8 as i64, want.get("state").and_then(|x| x.as_i64()).expect("state"));
            }
            // 终局
            assert_eq!(res.state.as_str(), c.get("state").and_then(|x| x.as_str()).expect("state"));
            let matched = c.get("matched_rule").and_then(|x| x.as_str());
            match (res.matched_rule.as_ref(), matched) {
                (Some(r), Some(id)) => assert_eq!(r.id, id, "{agent}/{fixture} 命中规则"),
                (None, None) => {}
                other => panic!("{agent}/{fixture} 命中规则不符：{other:?}"),
            }
            assert_eq!(res.visible_idle, c.get("visible_idle").and_then(|x| x.as_bool()).unwrap_or(false));
            assert_eq!(res.visible_blocker, c.get("visible_blocker").and_then(|x| x.as_bool()).unwrap_or(false));
            assert_eq!(res.visible_working, c.get("visible_working").and_then(|x| x.as_bool()).unwrap_or(false));
            assert_eq!(
                res.skip_state_update,
                c.get("skip_state_update").and_then(|x| x.as_bool()).unwrap_or(false)
            );
            assert_eq!(
                res.fallback_reason,
                c.get("fallback_reason").and_then(|x| x.as_str()).unwrap_or("")
            );
        }
    }

    fn read_fixture_from_repo(name: &str) -> String {
        let p = format!(
            "{}/../../fixtures/term-manifest/{name}",
            env!("CARGO_MANIFEST_DIR")
        );
        std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("夹具 {name}: {e}"))
    }

    /// 覆盖目录优先 + 坏覆盖回落内嵌 + 引擎版本门（Go TestOverrideWinsAndFallback 同款）。
    #[test]
    fn manifest_override_wins_and_fallback() {
        let dir = std::env::temp_dir().join(format!("homeway-test-manifest-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // 最小修法覆盖：放松 \A 锚点（真实运维场景：上游规则锚点漂移）
        let override_toml = r#"
id = "codex"
version = "local-trust-dialog"
min_engine_version = 3

[[rules]]
id = "trust_directory"
state = "blocked"
priority = 950
region = "top_non_empty_lines(20)"
visible_blocker = true
all = [
  { contains = ["> you are in "] },
  { regex = ['(?s)Do\s+you\s+trust\s+the\s+contents\s+of\s+this\s+directory\?'] },
]
"#;
        std::fs::write(dir.join("codex.toml"), override_toml).unwrap();
        let l = Loader::new(Some(&dir));
        assert!(l.warnings().is_empty(), "合法覆盖不该告警：{:?}", l.warnings());
        let c = l.for_process("codex").expect("codex");
        assert_eq!(c.manifest.source, Source::Override);
        let screen = read_fixture_from_repo("codex-startup.txt");
        let r = c.evaluate(&region::Input { screen, ..Default::default() });
        assert_eq!(r.state, State::Blocked);
        assert!(r.visible_blocker);
        assert_eq!(r.matched_rule.as_ref().map(|x| x.id.as_str()), Some("trust_directory"));

        // 坏覆盖（未知字段）：告警 + 回落内嵌
        std::fs::write(dir.join("codex.toml"), "id = \"codex\"\nunknown_key = 1\n").unwrap();
        let l = Loader::new(Some(&dir));
        assert_eq!(l.warnings().len(), 1, "坏覆盖应告警：{:?}", l.warnings());
        let c = l.for_process("codex").expect("内嵌回落仍可用");
        assert_eq!(c.manifest.source, Source::Embedded);
        assert!(!c.manifest.rules.is_empty());

        // 引擎版本门：声明更高 min_engine_version 的覆盖被拒
        std::fs::write(dir.join("codex.toml"), "id = \"codex\"\nmin_engine_version = 99\n").unwrap();
        let l = Loader::new(Some(&dir));
        assert_eq!(l.warnings().len(), 1);
        assert_eq!(l.for_process("codex").map(|c| c.manifest.source), Some(Source::Embedded));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 校验面负例：未知字段 / 复杂度上限 / 空规则 / 非法 region / 正则编不过 /
    /// skip_state_update 矛盾 / min_engine_version 门。
    #[test]
    fn manifest_validation_negatives() {
        let bad = [
            ("未知字段", "id = \"x\"\nrules = [{ id = \"r\", contains = [\"a\"], state = \"idle\" }]\nextra = 1\n"),
            ("无规则", "id = \"x\"\n"),
            ("缺 id", "rules = [{ id = \"r\", contains = [\"a\"] }]\n"),
            ("非法 region", "id = \"x\"\nrules = [{ id = \"r\", region = \"nope\", contains = [\"a\"] }]\n"),
            ("只有 not", "id = \"x\"\nrules = [{ id = \"r\", state = \"idle\", not = [{ contains = [\"a\"] }] }]\n"),
            ("正则编不过", "id = \"x\"\nrules = [{ id = \"r\", state = \"idle\", regex = ['[unclosed'] }]\n"),
            ("skip 矛盾", "id = \"x\"\nrules = [{ id = \"r\", state = \"idle\", skip_state_update = true, contains = [\"a\"] }]\n"),
            ("未知 state", "id = \"x\"\nrules = [{ id = \"r\", state = \"wat\", contains = [\"a\"] }]\n"),
        ];
        for (name, toml_doc) in bad {
            assert!(parse(toml_doc, Source::Embedded).is_err(), "{name} 应拒载");
        }
        // top_non_empty_lines 的引擎版本门（声明 2 用 top_* → 拒）
        let gated = "id = \"x\"\nmin_engine_version = 2\nrules = [{ id = \"r\", state = \"idle\", region = \"top_non_empty_lines(3)\", contains = [\"a\"] }]\n";
        assert!(parse(gated, Source::Embedded).is_err(), "版本门应拒");
        // 复杂度门：单门 33 个匹配器
        let many: Vec<String> = (0..33).map(|i| format!("\"needle{i}\"")).collect();
        let over = format!(
            "id = \"x\"\nrules = [{{ id = \"r\", state = \"idle\", contains = [{}] }}]\n",
            many.join(", ")
        );
        assert!(parse(&over, Source::Embedded).is_err(), "单门匹配器上限应拒");
    }

    /// 求值语义单元：priority 仲裁（严格大于才替换/同分文件序）、门语义、大小写不敏感、
    /// fallback。
    #[test]
    fn manifest_eval_semantics() {
        let doc = r#"
id = "t"
[[rules]]
id = "low"
state = "working"
priority = 1
contains = ["hello"]
[[rules]]
id = "high"
state = "blocked"
priority = 10
contains = ["hello"]
[[rules]]
id = "high2"
state = "idle"
priority = 10
visible_idle = true
line_regex = ['^world$']
"#;
        let m = parse(doc, Source::Embedded).unwrap();
        let c = Compiled::compile(m).unwrap();
        // 同分（10）：文件序靠前的 high 先命中，high2 同分不替换
        let r = c.evaluate(&region::Input {
            screen: "hello\nworld".into(),
            ..Default::default()
        });
        assert_eq!(r.matched_rule.as_ref().map(|x| x.id.as_str()), Some("high"));
        assert_eq!(r.state, State::Blocked);
        // contains 大小写不敏感
        let r = c.evaluate(&region::Input { screen: "HeLLo".into(), ..Default::default() });
        assert!(r.matched_rule.is_some());
        // 无命中 → 回落
        let r = c.evaluate(&region::Input { screen: "别的".into(), ..Default::default() });
        assert_eq!(r.state, State::Idle);
        assert_eq!(r.fallback_reason, DEFAULT_KNOWN_AGENT_IDLE_FALLBACK);
        // not 门：命中即整体不命中
        let doc2 = r#"
id = "t2"
[[rules]]
id = "r"
state = "working"
priority = 1
contains = ["go"]
not = [{ contains = ["stop"] }]
"#;
        let c2 = Compiled::compile(parse(doc2, Source::Embedded).unwrap()).unwrap();
        assert!(c2
            .evaluate(&region::Input { screen: "go go".into(), ..Default::default() })
            .matched_rule
            .is_some());
        assert!(c2
            .evaluate(&region::Input { screen: "go stop".into(), ..Default::default() })
            .matched_rule
            .is_none());
    }
}
