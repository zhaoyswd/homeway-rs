//! **准入拒绝的 CONNECTION_CLOSE 码**（M3 §4；**单源**——出口写、客户端读，两侧共用本表）。
//!
//! 事实（M2 真机发现①）：出口能分「`hr-reg4` MAC 不符 / `revoked` / `table-full`」，但
//! 核侧三种一律 `登记失败（连接在登记窗内关闭）`，随后 WG 回落把世代撑成 `readyBy=wg`
//! ⇒ **黑洞期设备侧不可见**。本表用**既有的 CONNECTION_CLOSE 通道**把归因带回去：
//! **不发新帧、不改准入状态机、不放松任何闸**（§4 的安全面）。
//!
//! **粗粒度两桶**（`0x11`/`0x12`）是**有意选择**：细粒度会让未认证对端拿到的可分辨性
//! 超过出口既有归因行给它的信息（§4 的安全面论证）。出口侧详细归因行（E-q 族）逐字不变。
//!
//! 本文件**纯 std**（隔离门 ② 条扫描面：不得出现 `tokio::|quinn|rustls|async fn|.await`）。

/// 准入关闭码（**扩 = 协议变化**；出口三处落点见 `exit/conn.rs`，客户端映射见
/// `client/register.rs`，判据行见 §8.2-13）。
pub mod code {
    /// **凭证不被接受**：`reject()` 的 MAC 不符/证明失败闸 + 引擎裁决的
    /// `no-token`/`revoked`。稳定短语 = [`super::text`] 的表。
    pub const CREDENTIAL: u64 = 0x11;
    /// **资源暂不可用（稍后重试）**：引擎裁决的 `table-full`/表压类 + 入境队列满。
    pub const RESOURCE: u64 = 0x12;
    /// **准入数据非法**：帧格式/版本族（首帧不是 Hello、魔数/长度不符、重复 Hello…）。
    pub const BAD_DATA: u64 = 0x13;
    /// **准入超时**：`ADMIT_DEADLINE` / `NONCE_TTL` 两族。
    pub const TIMEOUT: u64 = 0x14;

    /// 全部准入码（校验集与遍历面的单源；顺序 = 值序）。
    pub const ALL: [u64; 4] = [CREDENTIAL, RESOURCE, BAD_DATA, TIMEOUT];
}

/// 码 → 稳定短语（客户端归因行 `quic: 准入回执（code=0x%02x %s）` 的 `%s`；
/// **两侧共用**：出口的码与客户端的词不许各写一份）。`None` = 非准入码
/// （如出口会话级关闭留下的 `0x00` 或对端自选码）⇒ 调用方按「非准入面」处置。
pub const fn text(code: u64) -> Option<&'static str> {
    match code {
        code::CREDENTIAL => Some("凭证不被接受"),
        code::RESOURCE => Some("资源暂不可用（稍后重试）"),
        code::BAD_DATA => Some("准入数据非法"),
        code::TIMEOUT => Some("准入超时"),
        _ => None,
    }
}

/// 是否准入关闭码（白名单判定；**客户端映射限定在这个集合内**）。
pub const fn is_admission_code(code: u64) -> bool {
    text(code).is_some()
}

/// 客户端归因行（`quic: 准入回执（code=0x%02x %s）——本世代回落 WG 承载`，§4）。
///
/// 行名前缀 **刻意**与出口的 `quic: 准入被拒（dev=…` 区分：同前缀会让按前缀 grep 的
/// 脚本混淆（设计门 P3）。
pub fn client_line(code: u64) -> String {
    let t = text(code).unwrap_or("（未知准入码）");
    format!("quic: 准入回执（code=0x{code:02x} {t}）——本世代回落 WG 承载")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **判据（码表单源，§4）**：四个码逐值 + 短语逐字（客户端归因行的 `%s`）。
    #[test]
    fn codes_and_texts_are_the_designed_table() {
        assert_eq!(code::CREDENTIAL, 0x11);
        assert_eq!(code::RESOURCE, 0x12);
        assert_eq!(code::BAD_DATA, 0x13);
        assert_eq!(code::TIMEOUT, 0x14);
        assert_eq!(text(code::CREDENTIAL), Some("凭证不被接受"));
        assert_eq!(text(code::RESOURCE), Some("资源暂不可用（稍后重试）"));
        assert_eq!(text(code::BAD_DATA), Some("准入数据非法"));
        assert_eq!(text(code::TIMEOUT), Some("准入超时"));
        // `ALL` 与值域一致（加码忘改表 = 本断言红）
        let mut all = code::ALL;
        all.sort_unstable();
        assert_eq!(all, [0x11, 0x12, 0x13, 0x14]);
        for c in code::ALL {
            assert!(is_admission_code(c), "{c:#x} 必须在白名单内");
            assert!(text(c).is_some());
        }
        // 非准入码：0x00（今天出口的普通 close 码）/相邻值/大值一律不认
        for c in [0u64, 0x10, 0x15, 0x20, 0x21, 7, u64::MAX] {
            assert!(!is_admission_code(c), "{c:#x} 不得被当准入码");
            assert_eq!(text(c), None);
        }
    }

    /// **判据（客户端归因行形态，§4 设计门 P3）**：行首必须是 `quic: 准入回执（`——
    /// 与出口的 `quic: 准入被拒（` **不同前缀**（脚本按前缀 grep 不许混淆）。
    #[test]
    fn client_line_has_its_own_prefix_and_the_stable_phrase() {
        let l = client_line(code::CREDENTIAL);
        assert!(l.starts_with("quic: 准入回执（code=0x11 凭证不被接受）"), "{l}");
        assert!(l.contains("本世代回落 WG 承载"), "{l}");
        assert!(!l.starts_with("quic: 准入被拒"), "前缀必须与出口行区分：{l}");
        assert_eq!(
            client_line(code::TIMEOUT),
            "quic: 准入回执（code=0x14 准入超时）——本世代回落 WG 承载"
        );
        // 未知码也产出可读行（不 panic、不静默）
        assert!(client_line(0x99).contains("（未知准入码）"));
    }
}
