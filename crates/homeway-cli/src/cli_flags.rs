//! 取值 flag 纪律（Q-H F2/F7）：`--flag value` / `--flag=value` 的**唯一**取值器。
//!
//! 语义拍板（QH-design D3，对 Go `flag` 包吞值语义的**有意偏离**）：
//! - 缺值（末尾 / 下一个 token 是 flag）⇒ fail-fast（exit 2 + 可行动文案）——
//!   吞掉的后果 = 静默指向错目录/错值（`--state --verbose` 打生产 state），代价不对称；
//! - `--flag=` 空值 ⇒ 同样 fail-fast；
//! - **唯一 carve-out** = 自由文本旗（`--recover-cause`，值可以 `-` 开头，走
//!   `allow_dash = true`）——见 QH-design §6-12；
//! - 内联形态（`--flag=<值>`）不做 `-` 开头检查（用户显式绑定该值）。
//!
//! 布尔族（F7b）按 Go `strconv.ParseBool` 值集（`1/t/T/true/TRUE/True/0/f/F/FALSE`
//! ——含 `False`）；裸 flag = 默认值；其余 = exit 2。

use std::path::PathBuf;

/// 取值结果（区分「缺值」与「等号形空值」——两者文案不同）。
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Val<T> {
    Ok(T),
    /// 缺值：`why` = 可行动原因（「下一个是 --verbose」/「已到末尾」）。
    Missing(String),
    /// `--flag=`（等号形空值）。
    Empty,
}

/// `--flag=value` / `-flag` / `--flag` 拆名（不含前导 `-`）与内联值；非 flag = None。
pub(crate) fn split_flag(arg: &str) -> Option<(&str, Option<&str>)> {
    let body = arg.strip_prefix("--").or_else(|| arg.strip_prefix('-'))?;
    match body.split_once('=') {
        Some((k, v)) => Some((k, Some(v))),
        None => Some((body, None)),
    }
}

/// 取值（全站点唯一实现）。`inline` = `=` 后内联值；`next` = 下一个 token。
/// `allow_dash = false`（默认纪律）时下一个 token 以 `-` 开头 ⇒ `Missing`（不吞 flag）。
pub(crate) fn take_value(inline: Option<&str>, next: Option<&str>, allow_dash: bool) -> Val<String> {
    if let Some(v) = inline {
        return if v.is_empty() { Val::Empty } else { Val::Ok(v.to_owned()) };
    }
    match next {
        None => Val::Missing("已到末尾".to_owned()),
        Some("") => Val::Empty,
        Some(v) if !allow_dash && v.starts_with('-') => Val::Missing(format!("下一个是 {v}")),
        Some(v) => Val::Ok(v.to_owned()),
    }
}

/// 取值或 fail-fast（exit 2）。`name` = flag 名（不含 `--`）。
pub(crate) fn take_value_or_exit(
    name: &str,
    inline: Option<&str>,
    next: Option<&str>,
    allow_dash: bool,
) -> String {
    match take_value(inline, next, allow_dash) {
        Val::Ok(v) => v,
        Val::Empty => fail(name, "空值（`=` 后为空）"),
        Val::Missing(why) => fail(name, &format!("缺值（{why}）")),
    }
}

/// 取值或 fail-fast——**空值 carve-out**（Q-I 尾段 F0）：`--flag=` / `--flag ""`
/// 的空值返回 `Ok(String::new())` 而非 exit。**只给空串有明确语义的 flag 用**
/// （Go 基线文档化「空 = 关」且实测接受：`--stun`/`--stun6`（空 = 关公网观测）、
/// `--relay`（空 = 不用中继/关注册腿）——`bin/homeway-go` 实跑三形态均正常起服；
/// `--public-endpoint=` 空值 Go **同拒**（`--public-endpoint "" 非法`），不属本
/// carve-out）。缺值（末尾 / 下一个是 flag）与其它取值纪律不变，仍 fail-fast。
pub(crate) fn take_value_empty_ok_or_exit(name: &str, inline: Option<&str>, next: Option<&str>) -> String {
    match take_value(inline, next, false) {
        Val::Ok(v) => v,
        Val::Empty => String::new(),
        Val::Missing(why) => fail(name, &format!("缺值（{why}）")),
    }
}

/// `--state` 专用取值（fail-fast；`Value` 形）：
/// `--state --json` ⇒ `--state 缺值（下一个是 --json）`；`--state=` ⇒ `--state 空值`。
pub(crate) fn take_state_or_exit(name: &str, inline: Option<&str>, next: Option<&str>) -> PathBuf {
    PathBuf::from(take_value_or_exit(name, inline, next, false))
}

/// 数值取值或 fail-fast（`hint` = 值域提示，如「端口数字 1-65535」）。
pub(crate) fn take_num_or_exit<T: std::str::FromStr>(
    name: &str,
    inline: Option<&str>,
    next: Option<&str>,
    hint: &str,
) -> T {
    let v = take_value_or_exit(name, inline, next, false);
    match v.parse::<T>() {
        Ok(n) => n,
        Err(_) => fail(name, &format!("非法（{v:?}——{hint}）")),
    }
}

/// 布尔取值（Go `strconv.ParseBool` 值集；`bare_default` = 裸 flag / 无内联值时的值）。
pub(crate) fn take_bool(name: &str, inline: Option<&str>, bare_default: bool) -> Result<bool, String> {
    let Some(v) = inline else { return Ok(bare_default) };
    match v {
        "1" | "t" | "T" | "true" | "TRUE" | "True" => Ok(true),
        "0" | "f" | "F" | "false" | "FALSE" | "False" => Ok(false),
        _ => Err(format!(
            "--{name} 非法值 {v:?}（布尔：true/false/1/0；裸 --{name} = bool 默认）"
        )),
    }
}

/// 布尔取值或 fail-fast（exit 2）。
pub(crate) fn take_bool_or_exit(name: &str, inline: Option<&str>, bare_default: bool) -> bool {
    match take_bool(name, inline, bare_default) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    }
}

fn fail(name: &str, why: &str) -> ! {
    eprintln!("--{name} {why}——取值不会吞下一个参数（值以 `-` 开头请用 --{name}=<值> 形态）");
    std::process::exit(2);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// F2 表驱动：空格形/等号形/缺值/吞 flag/空值 五形态 × take_value/take_state。
    #[test]
    fn take_value_forms() {
        // 空格形
        assert_eq!(take_value(None, Some("/tmp/a"), false), Val::Ok("/tmp/a".to_owned()));
        // 等号形
        assert_eq!(take_value(Some("/tmp/b"), None, false), Val::Ok("/tmp/b".to_owned()));
        // 末尾缺值
        assert!(matches!(take_value(None, None, false), Val::Missing(_)));
        // 吞 flag：下一个以 '-' 开头 ⇒ Missing（不吞）
        match take_value(None, Some("--verbose"), false) {
            Val::Missing(why) => assert_eq!(why, "下一个是 --verbose"),
            other => panic!("应 Missing，实得 {other:?}"),
        }
        // 空值
        assert_eq!(take_value(Some(""), None, false), Val::Empty);
        assert_eq!(take_value(None, Some(""), false), Val::Empty);
        // carve-out：allow_dash = true 收 '-' 开头的值
        assert_eq!(
            take_value(None, Some("-自由文本"), true),
            Val::Ok("-自由文本".to_owned())
        );
        // 内联形态不做 dash 检查（用户显式绑定）
        assert_eq!(take_value(Some("-x"), None, false), Val::Ok("-x".to_owned()));
    }

    #[test]
    fn take_state_returns_pathbuf() {
        assert_eq!(take_state_or_exit("state", Some("/tmp/c"), None), PathBuf::from("/tmp/c"));
    }

    /// F0 carve-out：空值两形态（等号形/空格形）返回空串（stun/stun6/relay/ddns 四站点共用）；
    /// 非空值原样透传。
    #[test]
    fn empty_value_carveout_forms() {
        assert_eq!(take_value_empty_ok_or_exit("stun", Some(""), None), "");
        assert_eq!(take_value_empty_ok_or_exit("stun", None, Some("")), "");
        assert_eq!(take_value_empty_ok_or_exit("relay", None, Some("")), "");
        assert_eq!(take_value_empty_ok_or_exit("ddns", Some(""), None), "");
        assert_eq!(take_value_empty_ok_or_exit("stun6", Some("h:1"), None), "h:1");
        // 非 carve-out flag 的空值仍判 `Empty`（parser 站点据此 exit 2）
        assert_eq!(take_value(Some(""), None, false), Val::Empty);
    }

    /// F7b：布尔六档（缺值=裸 flag / 非法 / =false / =0 / =true / 裸 flag）。
    #[test]
    fn bool_forms() {
        assert!(take_bool("json", None, true).unwrap(), "裸 flag = bare_default");
        assert!(!take_bool("json", Some("false"), true).unwrap());
        assert!(!take_bool("json", Some("0"), true).unwrap());
        assert!(!take_bool("json", Some("F"), true).unwrap());
        assert!(take_bool("json", Some("true"), true).unwrap());
        assert!(take_bool("json", Some("1"), true).unwrap());
        assert!(take_bool("json", Some("True"), true).unwrap());
        let e = take_bool("json", Some("maybe"), true).unwrap_err();
        assert!(e.contains("非法值"), "{e}");
        // bare_default=false（如 --no-spawn 语义为 true，但显式 =false 收下）
        assert!(!take_bool("no-spawn", Some("FALSE"), true).unwrap());
    }

    /// F7a：数值取值（缺值/非法 fail-fast 由 exit 承担，这里只测合法形与错误分类）。
    #[test]
    fn num_forms() {
        assert_eq!(take_num_or_exit::<u64>("hold", Some("15"), None, "秒"), 15u64);
        assert_eq!(take_num_or_exit::<u16>("listen", None, Some("1080"), "端口"), 1080u16);
    }

    #[test]
    fn split_flag_forms() {
        assert_eq!(split_flag("--state=/tmp/x"), Some(("state", Some("/tmp/x"))));
        assert_eq!(split_flag("--state"), Some(("state", None)));
        assert_eq!(split_flag("-h"), Some(("h", None)));
        assert_eq!(split_flag("--upnp=false"), Some(("upnp", Some("false"))));
        assert_eq!(split_flag("positional"), None);
        assert_eq!(split_flag("-"), Some(("", None)));
    }
}
