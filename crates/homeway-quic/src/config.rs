//! 岛的构造配置（**全 std 类型**：同步面也要构造它——本文件受隔离门层 3 的 ② 条约束）。
//!
//! 为什么凭据在**构造期**注入（S2a 的实现决定，理由写在这里；设计 §2.1 未给凭据命令面）：
//!
//! - 岛的登记面必须持有 token secret（`hr-reg4` 的 MAC 输入之一，`reg4::ProofFrame::encode`）；
//! - 服务端身份钉定（出口 RPK）必须**在建端点之前**定案（`exit::rpk::client_pin`）；
//! - 形态照出口面的 `ExitQuicConfig`（一次装配，不留「半初始化岛」这种可表达态）。
//!
//! 巡检节拍（[`IslandConfig::patrol`]）同时是两件事的节拍：C15' 的 60s 注册刷新与
//! §2.3 的「rebind 后 N 拍无回包 ⇒ 回落」判据——两者在既有实现里共用同一个巡检间隔
//! （`facade/tun_exec.rs` 的 `PATROL_INTERVAL = 60s`），故只暴露一个旋钮。

use std::fmt;
use std::net::SocketAddrV4;
use std::time::Duration;

use crate::rpk::RpkPublicKey;
use crate::tuning::{ProbeTuning, StreamLimits};

/// 巡检节拍缺省值（= 既有 `PATROL_INTERVAL`；判据行 C15 的 60s 刷新同源）。
pub const DEFAULT_PATROL: Duration = Duration::from_secs(60);

/// QUIC MTU 上限旋钮的缺省 / 有效区间（M1 设计 §12-① 的实现契约）。
///
/// 缺省 1400；有效区间 `[1320,1400]`——低于 1320 时 `max_datagram_size()` < 1280，
/// 1280 内层包**每一个**都会被丢（端到端 TCP 反复重传同尺寸段 ⇒ 用户感知是断），
/// 故区间下限与出口侧 `min_mtu` 同值。**区间内产不出 `mds < 1280`**
/// （1320 ⇒ mds 1282）⇒「窄路径不可用」行的注入只能走测试缝（直接给
/// [`IslandConfig::mtu_cap`] 更小的值，见 `client/tests.rs` 的用例）。
pub const QUIC_MTU_CAP_DEFAULT: u16 = 1400;
/// 有效区间下限（= `exit::transport::MIN_MTU`）。
pub const QUIC_MTU_CAP_MIN: u16 = 1320;
/// 有效区间上限（= `exit::transport::INITIAL_MTU`；上探被信封头寸锁死，见 §1.2）。
pub const QUIC_MTU_CAP_MAX: u16 = 1400;

/// token secret（32B）——**敏感材料**。
///
/// 纪律照 `crate::rpk::Ed25519Seed` 与 `homeway-core::token::Secret`：Debug 全脱敏、
/// **不 `Copy`**（每处 `Copy` 都留下追踪不到的栈副本）、Drop 擦除。本 crate 是叶子
/// （不得依赖 `homeway-core`），故自持一份最小实现。
pub struct TokenSecret([u8; 32]);

impl TokenSecret {
    /// 从 32B 载荷构造（生产来源 = `homeway-core` 的 `token::Secret`）。
    pub fn from_bytes(b: [u8; 32]) -> Self {
        Self(b)
    }

    /// 借视图（喂 `hr-reg4` 的 MAC；不拷贝出所有权）。
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl Clone for TokenSecret {
    fn clone(&self) -> Self {
        Self(self.0)
    }
}

impl fmt::Debug for TokenSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // 敏感材料：不落任何指纹（连前缀都不出——跨实例可关联）
        write!(f, "TokenSecret(<redacted>)")
    }
}

impl Drop for TokenSecret {
    fn drop(&mut self) {
        for x in self.0.iter_mut() {
            // SAFETY: 引用来源合法；volatile 写只保证不被优化消除，不引入 UB
            unsafe { std::ptr::write_volatile(x, 0) };
        }
        std::sync::atomic::compiler_fence(std::sync::atomic::Ordering::SeqCst);
    }
}

/// 岛的准入材料：token secret + 自身 WG 公钥/设备标签 + 出口 RPK 钉定。
///
/// 四件缺一不可：MAC 需要 secret/pubkey/devTag，TLS 握手需要 pin（错 pin ⇒ 握手中止，
/// 设计 §1.3/§12-②）。
pub struct IslandCredential {
    secret: TokenSecret,
    pubkey: [u8; 32],
    dev_tag: [u8; 8],
    pin: RpkPublicKey,
}

impl IslandCredential {
    /// 装配（`pubkey` = 设备 WG 公钥；`dev_tag` = 跨连接稳定的设备标签）。
    pub fn new(secret: TokenSecret, pubkey: [u8; 32], dev_tag: [u8; 8], pin: RpkPublicKey) -> Self {
        Self {
            secret,
            pubkey,
            dev_tag,
            pin,
        }
    }

    /// 出口 RPK 钉定（建端点用；公开材料）。
    pub(crate) fn pin(&self) -> RpkPublicKey {
        self.pin
    }

    /// MAC 三件（借视图；只在岛内异步面用）。
    pub(crate) fn parts(&self) -> (&[u8; 32], &[u8; 32], &[u8; 8]) {
        (self.secret.as_bytes(), &self.pubkey, &self.dev_tag)
    }

    /// devTag 短指纹（4B hex——判据行 `dev=%s` 与 `table.rs`/`exit` 的 `dev_short` 同形）。
    pub(crate) fn dev_short(&self) -> String {
        self.dev_tag.iter().take(4).map(|b| format!("{b:02x}")).collect()
    }
}

impl fmt::Debug for IslandCredential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // secret 脱敏（其余是公开/半公开材料：公钥与标签按短指纹纪律出 4B）
        write!(f, "IslandCredential({:?}, dev={}…)", self.pin, self.dev_short())
    }
}

/// 岛构造配置。
pub struct IslandConfig {
    /// 准入材料（构造期注入；见模块头）。
    pub credential: IslandCredential,
    /// 初始本地绑定（`None` = `0.0.0.0:0` 交内核挑端口）。
    ///
    /// 为什么可指定：本地验证要在一个**已知源地址**上起跑，再 `Rebind` 到另一个地址
    /// 制造真迁移（§2.3 的本地注入代理形态）。
    pub bind: Option<SocketAddrV4>,
    /// 巡检节拍（刷新 + 迁移保持判据；缺省 [`DEFAULT_PATROL`]）。
    pub patrol: Duration,
    /// QUIC MTU 上限（`initial_mtu` 与 DPLPMTUD `upper_bound` 同值；缺省
    /// [`QUIC_MTU_CAP_DEFAULT`]）。
    ///
    /// 生产取值由世代层从 `HOMEWAY_QUIC_MTU` / `tunConfig.quicMtuCap` 解析并**夹到**
    /// `[QUIC_MTU_CAP_MIN, QUIC_MTU_CAP_MAX]`（facade 侧）；本字段不再夹——测试缝
    /// （窄路径注入）需要能给出区间外的值。
    pub mtu_cap: u16,
    /// **流面限制**（M3 §1.7 / §15-3）：并发上限、每流接收窗、连接级接收窗（聚合上界）、
    /// 连接级发送窗、每流待发队列。
    ///
    /// 缺省 = 设计定值（[`StreamLimits::design`]）；启动时按 `HOMEWAY_QUIC_STREAMS` /
    /// `HOMEWAY_QUIC_STREAM_WINDOW` / `HOMEWAY_QUIC_RECV_WINDOW` /
    /// `HOMEWAY_QUIC_SEND_WINDOW` / `HOMEWAY_QUIC_STREAM_PENDING` 覆盖（env 优先，
    /// 非法项按缺省 + 记行）。
    pub streams: StreamLimits,
    /// **快探/恢复参数**（M3 §15-2；S4 消费）：首探预算/复探倍数/待机节拍/抖动与 B 门阈值/
    /// 发送面新鲜度窗。缺省 = 设计初值；启动时按 `HOMEWAY_QUIC_PROBE_*` 族覆盖。
    pub probe: ProbeTuning,
}

impl IslandConfig {
    /// 生产缺省（绑定 `0.0.0.0:0`、节拍 60s、MTU 上限 1400、流面与快探取设计定值）。
    pub fn new(credential: IslandCredential) -> Self {
        Self {
            credential,
            bind: None,
            patrol: DEFAULT_PATROL,
            mtu_cap: QUIC_MTU_CAP_DEFAULT,
            streams: StreamLimits::design(),
            probe: ProbeTuning::design(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 敏感材料纪律：secret Debug 全脱敏；凭据 Debug 不落 secret 指纹。
    #[test]
    fn secret_is_redacted_in_debug() {
        let secret = TokenSecret::from_bytes([0xCD; 32]);
        assert_eq!(format!("{secret:?}"), "TokenSecret(<redacted>)");
        let cred = IslandCredential::new(
            TokenSecret::from_bytes([0xCD; 32]),
            [7u8; 32],
            [0xAB; 8],
            RpkPublicKey::from_bytes([9u8; 32]),
        );
        let s = format!("{cred:?}");
        assert!(!s.contains("cd"), "secret 不得出指纹：{s}");
        assert!(s.contains("dev=abababab…"), "devTag 短指纹应在：{s}");
    }

    /// 配置缺省面（生产形态 = 通配绑定 + 60s 节拍）。
    #[test]
    fn config_defaults() {
        let cfg = IslandConfig::new(IslandCredential::new(
            TokenSecret::from_bytes([1u8; 32]),
            [2u8; 32],
            [3u8; 8],
            RpkPublicKey::from_bytes([4u8; 32]),
        ));
        assert!(cfg.bind.is_none(), "缺省不钉绑定地址");
        assert_eq!(cfg.patrol, DEFAULT_PATROL);
        assert_eq!(DEFAULT_PATROL, Duration::from_secs(60));
        assert_eq!(cfg.mtu_cap, QUIC_MTU_CAP_DEFAULT, "缺省 MTU 上限 = 1400");
        // M3 §15-3：流面/快探缺省 = 设计定值（env 覆盖在 `Island::start` 施加）
        assert_eq!(cfg.streams, crate::tuning::StreamLimits::design());
        assert_eq!(cfg.probe, crate::tuning::ProbeTuning::design());
        assert_eq!(cfg.streams.service_capacity(), 62, "N14：64 − 2");
        assert_eq!(QUIC_MTU_CAP_DEFAULT, 1400);
        assert_eq!(QUIC_MTU_CAP_MIN, 1320, "有效区间下限 = 出口 min_mtu 同值");
        // 区间自洽（编译期断言——常量关系不允许漂移）
        const { assert!(QUIC_MTU_CAP_MIN <= QUIC_MTU_CAP_DEFAULT) };
        const { assert!(QUIC_MTU_CAP_DEFAULT <= QUIC_MTU_CAP_MAX) };
    }
}
