//! dialect — manifest 正则的方言垫片（R6 6f；行为真源 = baseline 克隆
//! `pkg/term/manifest/dialect.go`，口径 = R6-design §9.5）。
//!
//! manifest 是按 Rust `regex` 方言写的（herdr 携署名移植）；Go 出口侧有一份
//! 「Rust → RE2」垫片（只翻 `\uXXXX`/`\p{Alphabetic}`）。Rust 侧**消费同一份文件**，
//! 但要逐规则对齐 **Go 出口的实跑行为**，所以垫片方向相反：把 Rust 原生语义里
//! 比 Go RE2 **宽**的三族构造收窄到 Go 行为——
//!
//! 1. `\p{Alphabetic}`/`\P{Alphabetic}` → `\p{L}`/`\P{L}`：Rust 原生支持派生属性
//!    Alphabetic（⊋ \p{L}），Go 只支持类别/脚本 ⇒ 按 Go 的近似映射翻（`\p{L}` 是
//!    Alphabetic 的子集，与 Go 出口**逐规则一致**而非「更准」——3 份 manifest 真用）。
//! 2. Perl 类转义 `\s \S \d \D \w \W`：Rust 默认 Unicode（White_Space/Nd/word），
//!    Go RE2 恒 ASCII（`[\t\n\f\r ]`/`[0-9]`/`[0-9A-Za-z_]`）⇒ 展开成显式 ASCII 类
//!    （Go 的 `\s` **不含** `\v`，与 Rust 的 ASCII 形差一员，故不能用 `(?-u:\s)` 偷懒）。
//! 3. `\b \B`：Rust Unicode 词界，Go ASCII 词界 ⇒ `(?-u:\b)`/`(?-u:\B)`。
//!
//! `\uXXXX`/`\u{XXXX}` Rust 原生支持、无需翻译（Go 垫片反而要翻成 `\x{…}`）。
//! `\\` 字面反斜杠保护与 Go 同款（防 `\\u2800` 误翻）。**字符类内外分开处理**：
//! 类内 `\s` 展开成嵌套类 `[\t\n\f\r ]`（Rust regex 支持类内嵌套类=并集），
//! 类外同样可以安全展开（类是合法的单元素）。
//!
//! contains 文本**不过**本垫片（`\s` 在那边是两个普通字符）。

/// 派生属性 → Go RE2 可用类别的近似映射（与 Go propertyAliases 同表）。
const PROPERTY_ALIASES: &[(&str, &str)] = &[("Alphabetic", "L")];

/// ASCII 类展开（Go RE2 语义；⚠️ `\s` 不含 `\v`）。
const ASCII_SPACE: &str = "[\\t\\n\\f\\r ]";
const ASCII_NOT_SPACE: &str = "[^\\t\\n\\f\\r ]";
const ASCII_DIGIT: &str = "[0-9]";
const ASCII_NOT_DIGIT: &str = "[^0-9]";
const ASCII_WORD: &str = "[0-9A-Za-z_]";
const ASCII_NOT_WORD: &str = "[^0-9A-Za-z_]";

/// 翻译方言（幂等：已翻译过的再跑不变）。所有正则编译都必须走这里。
pub fn translate_pattern(p: &str) -> std::borrow::Cow<'_, str> {
    let needs = p.contains("\\p{") || p.contains("\\P{")
        || p.contains("\\s") || p.contains("\\S")
        || p.contains("\\d") || p.contains("\\D")
        || p.contains("\\w") || p.contains("\\W")
        || p.contains("\\b") || p.contains("\\B");
    if !needs {
        return p.into();
    }
    let mut out = String::with_capacity(p.len());
    let bytes = p.as_bytes();
    let mut i = 0usize;
    let mut in_class = false;
    while i < bytes.len() {
        let c = bytes[i];
        if c != b'\\' {
            if c == b'[' && !in_class {
                in_class = true;
            } else if c == b']' && in_class {
                in_class = false;
            }
            // 按 UTF-8 整字符吐出（逐字节 `as char` 会把多字节字符拆成 latin-1 乱码）
            let ch_len = utf8_char_len(c, &bytes[i..]);
            out.push_str(&p[i..i + ch_len]);
            i += ch_len;
            continue;
        }
        if i + 1 >= bytes.len() {
            out.push('\\');
            break;
        }
        let next = bytes[i + 1];
        if next == b'\\' {
            // 字面反斜杠：原样吐出，防 `\\u2800`/`\\p{…}` 误翻。
            out.push_str("\\\\");
            i += 2;
            continue;
        }
        if next == b'p' || next == b'P' {
            if let Some((name, consumed)) = parse_property(&p[i + 2..]) {
                let mapped = PROPERTY_ALIASES
                    .iter()
                    .find(|(from, _)| *from == name)
                    .map(|(_, to)| *to)
                    .unwrap_or(&name);
                out.push('\\');
                out.push(next as char);
                out.push('{');
                out.push_str(mapped);
                out.push('}');
                i += 2 + consumed;
                continue;
            }
        }
        if in_class {
            // 类内展开成**嵌套类**（Rust regex 的类内嵌套类 = 并集；取反的嵌套类也合法）。
            match next {
                b's' => out.push_str(ASCII_SPACE),
                b'S' => out.push_str(ASCII_NOT_SPACE),
                b'd' => out.push_str(ASCII_DIGIT),
                b'D' => out.push_str(ASCII_NOT_DIGIT),
                b'w' => out.push_str(ASCII_WORD),
                b'W' => out.push_str(ASCII_NOT_WORD),
                _ => {
                    out.push('\\');
                    out.push(next as char);
                }
            }
            i += 2;
            continue;
        }
        match next {
            b's' => out.push_str(ASCII_SPACE),
            b'S' => out.push_str(ASCII_NOT_SPACE),
            b'd' => out.push_str(ASCII_DIGIT),
            b'D' => out.push_str(ASCII_NOT_DIGIT),
            b'w' => out.push_str(ASCII_WORD),
            b'W' => out.push_str(ASCII_NOT_WORD),
            // 已在 (?-u: 前缀内的不重包（字节级幂等）
            b'b' if !out.ends_with("(?-u:") => out.push_str("(?-u:\\b)"),
            b'B' if !out.ends_with("(?-u:") => out.push_str("(?-u:\\B)"),
            _ => {
                out.push('\\');
                out.push(next as char);
            }
        }
        i += 2;
    }
    out.into()
}

/// 一个 UTF-8 首字节起的全字符长度（模式串不会是病态 UTF-8——manifest 来自 TOML 字符串）。
fn utf8_char_len(first: u8, rest: &[u8]) -> usize {
    let need = match first {
        b if b < 0x80 => 1,
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        _ => 4,
    };
    need.min(rest.len())
}

/// 解析 `{Name}`（p/P 已消费），返回属性名与消费字节数（名字取第一对花括号之间，
/// 空/无闭合 = 非法属性形态，原样留给编译器报错——与 Go parseProperty 同宽松度）。
fn parse_property(rest: &str) -> Option<(String, usize)> {
    if !rest.starts_with('{') {
        return None;
    }
    let end = rest.find('}')?;
    let name = &rest[1..end];
    if name.is_empty() {
        return None;
    }
    Some((name.to_string(), end + 1))
}

/// 编译一个模式（翻译方言后交 regex crate；失败 = manifest 校验不通过）。
pub fn compile_pattern(p: &str) -> Result<regex::Regex, regex::Error> {
    regex::Regex::new(&translate_pattern(p))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dialect_ascii_classes_expanded() {
        // 类外：直接展开；类内：嵌套类形态
        assert_eq!(translate_pattern(r"a\s+b"), "a[\\t\\n\\f\\r ]+b");
        assert_eq!(translate_pattern(r"[\w-]+"), "[[0-9A-Za-z_]-]+");
        assert_eq!(translate_pattern(r"\d{2,4}"), "[0-9]{2,4}");
        // \b → ASCII 词界
        assert_eq!(translate_pattern(r"\bword\b"), "(?-u:\\b)word(?-u:\\b)");
        // \s 不吃 \v（Go 口径）：展开式与 (?-u:\s) 的差别 = \x0b
        assert!(!translate_pattern(r"\s").contains('u'));
        let re = compile_pattern(r"\s").unwrap();
        assert!(!re.is_match("\u{0b}"), "\\v 不是 Go \\s 的成员");
        assert!(re.is_match(" ") && re.is_match("\n"));
    }

    #[test]
    fn dialect_property_alias_and_backslash_guard() {
        assert_eq!(translate_pattern(r"\p{Alphabetic}+"), "\\p{L}+");
        assert_eq!(translate_pattern(r"\P{Alphabetic}"), "\\P{L}");
        // 非 Alphabetic 属性原样保留（Go 支持类别/脚本）
        assert_eq!(translate_pattern(r"\p{Han}"), "\\p{Han}");
        // 字面反斜杠保护
        assert_eq!(translate_pattern(r"\\u2800"), "\\\\u2800");
        assert_eq!(translate_pattern(r"\\p{Alphabetic}"), "\\\\p{Alphabetic}");
        // \u 转义 Rust 原生（不翻）
        assert_eq!(translate_pattern(r"\u2800"), "\\u2800");
        assert_eq!(translate_pattern(r"\u{fe0f}"), "\\u{fe0f}");
    }

    #[test]
    fn dialect_idempotent_and_passthrough() {
        for p in [
            r"(?i)abc",
            r"\A> You are in [^\r\n]+",
            r"(?s)Do\s+you\s+trust",
            r"\bword\b",
            r"[\w-]+\S",
        ] {
            let once = translate_pattern(p);
            let twice = translate_pattern(&once);
            assert_eq!(once, twice, "幂等：{p}");
        }
        // 无关模式零拷贝直通
        assert!(matches!(translate_pattern("plain"), std::borrow::Cow::Borrowed(_)));
    }

    /// 类内状态跟踪：`]` 只在类内闭合、类外 `[` 开类。
    #[test]
    fn dialect_class_state_tracking() {
        assert_eq!(translate_pattern(r"a]b\s"), "a]b[\\t\\n\\f\\r ]");
        assert_eq!(translate_pattern(r"[\]]\s"), "[\\]][\\t\\n\\f\\r ]");
        assert_eq!(translate_pattern(r"[a\sb]"), "[a[\\t\\n\\f\\r ]b]");
    }
}
