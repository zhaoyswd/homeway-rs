//! region — region 切片全集（R6 6f；行为真源 = baseline 克隆
//! `pkg/term/manifest/region.go`，逐函数移植）。
//!
//! region 是规则的**判定范围选择器**（旧提示残留不误判的锚定机制）。取不到
//! （无提示框/无标记）按 herdr 口径退回整屏或空。行迭代语义 = `str::lines()`
//! （结尾换行不产生空元素——Go 侧为此专门写了 lines() 助手对齐 Rust）。

/// 规则不写 region 时的默认值。
pub const DEFAULT_REGION: &str = "whole_recent";

/// `top_non_empty_lines` 需要的最低引擎版本。
pub const TOP_NON_EMPTY_LINES_ENGINE_VERSION: u32 = 3;

/// 检测输入（四路证据：屏尾文本 / OSC 标题 / OSC 进度载荷）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Input {
    /// 屏幕尾部文本（当前活动屏方向最近约一屏的纯文本，滚动位置不影响）。
    pub screen: String,
    /// 终端标题（OSC 0/2，旁路扫描器维护——标题的单一来源）。
    pub osc_title: String,
    /// OSC 9;4 progress 的原始载荷（如 "4;1;50"）；空 = 无。
    pub osc_progress: String,
}

/// 取一条规则要判定的文本。
pub fn region(in_: &Input, spec: &str) -> String {
    let trimmed = spec.trim();
    match trimmed {
        "osc_title" => return in_.osc_title.clone(),
        "osc_progress" => return in_.osc_progress.clone(),
        _ => {}
    }
    let content = &in_.screen;
    match trimmed {
        "whole_recent" => return content.clone(),
        "after_last_prompt_marker" => return after_last_prompt_marker(content),
        "before_current_prompt_marker" => return before_current_prompt_marker(content),
        "whole_recent_without_current_prompt_marker" => {
            return whole_recent_without_current_prompt_marker(content)
        }
        "current_prompt_block_marker" => return current_prompt_block_marker(content),
        "after_current_prompt_block_marker" => return after_current_prompt_block_marker(content),
        "prompt_box_body" => return prompt_box_body(content),
        "above_prompt_box" => return above_prompt_box(content),
        "last_non_empty_above_prompt_box" => {
            return last_non_empty_line(&above_prompt_box(content))
        }
        "after_last_horizontal_rule" => return after_last_horizontal_rule(content),
        _ => {}
    }
    if let Some(n) = region_count(trimmed, "bottom_lines") {
        return bottom_lines(content, n);
    }
    if let Some(n) = region_count(trimmed, "bottom_non_empty_lines") {
        return bottom_non_empty_lines(content, n);
    }
    if let Some(n) = top_region_count(trimmed) {
        return top_non_empty_lines(content, n);
    }
    String::new()
}

/// 校验 region 名合法（加载期校验；非法名 = 整份 manifest 拒载）。
pub fn validate_region_name(spec: &str) -> Result<(), String> {
    let trimmed = spec.trim();
    match trimmed {
        "whole_recent" | "after_last_prompt_marker" | "before_current_prompt_marker"
        | "whole_recent_without_current_prompt_marker" | "current_prompt_block_marker"
        | "after_current_prompt_block_marker" | "prompt_box_body" | "above_prompt_box"
        | "last_non_empty_above_prompt_box" | "after_last_horizontal_rule" | "osc_title"
        | "osc_progress" => return Ok(()),
        _ => {}
    }
    if region_count(trimmed, "bottom_lines").is_some()
        || region_count(trimmed, "bottom_non_empty_lines").is_some()
        || top_region_count(trimmed).is_some()
    {
        return Ok(());
    }
    Err(format!("未知 region {trimmed}"))
}

/// 解析 `name(N)` 形态（**不**拒绝前导 0——与 Go regionCount 同宽松度）。
fn region_count(spec: &str, name: &str) -> Option<usize> {
    let rest = spec.strip_prefix(name)?;
    let rest = rest.strip_prefix('(')?;
    let rest = rest.strip_suffix(')')?;
    parse_count(rest)
}

/// 解析 `top_non_empty_lines(N)`：拒绝前导 0（避免 "01" 歧义写法）+ 上限 65535
/// （对齐 herdr u16::MAX——再大没有语义，且是病态输入的入口）。
fn top_region_count(spec: &str) -> Option<usize> {
    let rest = spec.strip_prefix("top_non_empty_lines")?;
    let rest = rest.strip_prefix('(')?;
    let rest = rest.strip_suffix(')')?;
    if rest.starts_with('0') {
        return None;
    }
    let n = parse_count(rest)?;
    if n > 65535 {
        return None;
    }
    Some(n)
}

/// 计数解析：全数字、上限 1<<20 防病态大数。
fn parse_count(s: &str) -> Option<usize> {
    if s.is_empty() {
        return None;
    }
    let mut n: usize = 0;
    for b in s.bytes() {
        if !b.is_ascii_digit() {
            return None;
        }
        n = n * 10 + usize::from(b - b'0');
        if n > 1 << 20 {
            return None;
        }
    }
    Some(n)
}

/// 与 `str::lines()` 对齐：按 '\\n' 切、丢掉结尾换行产生的空元素。
fn lines(s: &str) -> Vec<&str> {
    if s.is_empty() {
        return Vec::new();
    }
    let mut out: Vec<&str> = s.split('\n').collect();
    if out.last() == Some(&"") {
        out.pop();
    }
    out
}

/// 第 index 行在原文本里的字节偏移。
fn line_start_offset(content: &str, ls: &[&str], mut index: usize) -> usize {
    if index > ls.len() {
        index = ls.len();
    }
    let mut off = 0usize;
    for l in &ls[..index] {
        off += l.len() + 1;
    }
    off.min(content.len())
}

fn slice_from_line_index<'a>(content: &'a str, ls: &[&str], index: usize) -> &'a str {
    &content[line_start_offset(content, ls, index)..]
}

fn bottom_lines(content: &str, count: usize) -> String {
    let ls = lines(content);
    let start = ls.len().saturating_sub(count);
    slice_from_line_index(content, &ls, start).to_string()
}

fn bottom_non_empty_lines(content: &str, count: usize) -> String {
    let ls = lines(content);
    let mut start_index: Option<usize> = None;
    let mut seen = 0usize;
    for (i, l) in ls.iter().enumerate().rev() {
        if l.trim().is_empty() {
            continue;
        }
        seen += 1;
        start_index = Some(i);
        if seen == count {
            break;
        }
    }
    match start_index {
        Some(i) => slice_from_line_index(content, &ls, i).to_string(),
        None => String::new(),
    }
}

fn top_non_empty_lines(content: &str, count: usize) -> String {
    let ls = lines(content);
    let mut end_index: Option<usize> = None;
    let mut seen = 0usize;
    for (i, l) in ls.iter().enumerate() {
        if l.trim().is_empty() {
            continue;
        }
        seen += 1;
        end_index = Some(i);
        if seen == count {
            break;
        }
    }
    match end_index {
        Some(i) => content[..line_start_offset(content, &ls, i + 1)].to_string(),
        None => String::new(),
    }
}

/// 最后一条 codex 提示行**之后**的文本；没有标记则退回整屏。
fn after_last_prompt_marker(content: &str) -> String {
    let ls = lines(content);
    let index = ls.iter().rposition(|l| codex_prompt_line(l));
    match index {
        None => content.to_string(),
        Some(i) => slice_from_line_index(content, &ls, i + 1).to_string(),
    }
}

fn before_current_prompt_marker(content: &str) -> String {
    let ls = lines(content);
    match current_codex_prompt_index(&ls) {
        None => content.to_string(),
        Some(i) => content[..line_start_offset(content, &ls, i)].to_string(),
    }
}

/// 有「当前提示行」时返回空（提示输入态不当整屏证据），否则整屏。
fn whole_recent_without_current_prompt_marker(content: &str) -> String {
    let ls = lines(content);
    if current_codex_prompt_index(&ls).is_some() {
        return String::new();
    }
    content.to_string()
}

fn current_prompt_block_marker(content: &str) -> String {
    let ls = lines(content);
    let idx = match current_codex_prompt_index(&ls) {
        Some(i) => i,
        None => return String::new(),
    };
    for l in ls[..idx].iter().rev() {
        if codex_block_marker_line(l) {
            return (*l).to_string();
        }
    }
    String::new()
}

fn after_current_prompt_block_marker(content: &str) -> String {
    let ls = lines(content);
    let idx = match current_codex_prompt_index(&ls) {
        Some(i) => i,
        None => return String::new(),
    };
    let block = ls[..idx].iter().rposition(|l| codex_block_marker_line(l));
    match block {
        None => String::new(),
        Some(b) => slice_from_line_index(content, &ls, b).to_string(),
    }
}

/// 最后一条提示行，且其后**不得**再出现块标记（否则那条提示是历史，不是当前输入态）。
fn current_codex_prompt_index(ls: &[&str]) -> Option<usize> {
    let idx = ls.iter().rposition(|l| codex_prompt_line(l))?;
    if ls[idx + 1..].iter().any(|l| codex_block_marker_line(l)) {
        return None;
    }
    Some(idx)
}

fn codex_prompt_line(line: &str) -> bool {
    line == "›" || line.starts_with("› ")
}

fn codex_block_marker_line(line: &str) -> bool {
    ["•", "■", "✗", "✓"].iter().any(|p| line.starts_with(p))
}

/// 提示框**顶边框之后到框内下一条分隔线之前**（框内正文）。
fn prompt_box_body(content: &str) -> String {
    let ls = lines(content);
    let top = match prompt_box_top_border_index(&ls) {
        Some(i) => i,
        None => return String::new(),
    };
    let start = line_start_offset(content, &ls, top + 1);
    let mut end_index = ls.len();
    for (i, l) in ls.iter().enumerate().skip(top + 1) {
        if is_horizontal_rule(l) {
            end_index = i;
            break;
        }
    }
    let end = line_start_offset(content, &ls, end_index);
    if end < start {
        return String::new();
    }
    content[start..end].to_string()
}

fn above_prompt_box(content: &str) -> String {
    let ls = lines(content);
    match prompt_box_top_border_index(&ls) {
        None => content.to_string(),
        Some(top) => content[..line_start_offset(content, &ls, top)].to_string(),
    }
}

fn after_last_horizontal_rule(content: &str) -> String {
    let mut last_rule_end = 0usize;
    let mut offset = 0usize;
    for l in lines(content) {
        let next = offset + l.len() + 1;
        if is_horizontal_rule(l) {
            last_rule_end = next.min(content.len());
        }
        offset = next;
    }
    content[last_rule_end..].to_string()
}

fn last_non_empty_line(content: &str) -> String {
    lines(content)
        .iter()
        .rev()
        .find(|l| !l.trim().is_empty())
        .map(|l| (*l).to_string())
        .unwrap_or_default()
}

/// 从底部数**第二条**水平分隔线（框顶；最后一条是框底）。
fn prompt_box_top_border_index(ls: &[&str]) -> Option<usize> {
    let mut count = 0usize;
    for (i, l) in ls.iter().enumerate().rev() {
        if is_horizontal_rule(l) {
            count += 1;
            if count == 2 {
                return Some(i);
            }
        }
    }
    None
}

/// 以 '─' 开头且「只有横线」或「横线 ≥3 条」（短的横线不算分隔线，避免把内容里的
/// 装饰符号当框线）。
fn is_horizontal_rule(line: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return false;
    }
    let mut rule_chars = 0usize;
    let mut rule_bytes = 0usize;
    for r in trimmed.chars() {
        if r != '─' {
            break;
        }
        rule_chars += 1;
        rule_bytes += r.len_utf8();
    }
    if rule_chars == 0 {
        return false;
    }
    let suffix = trimmed[rule_bytes..].trim_start_matches([' ', '\t']);
    suffix.is_empty() || rule_chars >= 3
}
