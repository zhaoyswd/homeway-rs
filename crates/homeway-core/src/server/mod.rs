//! server：R3 出口侧模块（多 peer WG device / 腿帧收发面 / 设备表 / 台账 / 拦截层）。
//!
//! 对应 Go 侧 `internal/server/` + `pkg/servercore/` + `pkg/intercept/` + `pkg/dns/` 的 Rust 平移；
//! 模块划分按 Rust 惯例（`bind.rs` 对应 `servercore.ServerBind`——与客户端
//! `wtransport::bind`（对应 `wtransport.Bind`）同名是「同一协议两端各一个 Bind」的
//! 对称结构，全路径区分）。

pub mod bind;
pub mod bindwatch;
pub mod ddnscheck;
pub mod device;
pub mod dnsproxy;
pub mod engine;
pub mod egress;
pub mod quic_admit;
pub mod upnp;
pub mod state;
pub mod table;
pub mod intercept;
pub mod relayleg;
pub mod txring;

/// 出口 QUIC 面的 RPK 私钥种子 = `HKDF(后端静态私钥, "homeway/quic-rpk")`（M1 设计
/// §1.3/§12-② 的「出口侧产出」）。
///
/// 为什么从**后端静态私钥**派生而不是另存一枚密钥文件：出口身份已有唯一持久源
/// （`state` 的 `private_key()`），派生式 ⇒ 重启后公钥不变（客户端 token 里的钉定值
/// 不失效），且不新增落盘面；域分离标签与 `psk.rs` 的 `homeway/wg-psk` 同款做法。
///
/// **本函数是出口侧唯一的种子来源**（QUIC 端点与 token 铸造必须用同一份——
/// 分叉 = 客户端钉定必然失败）。
pub fn quic_rpk_seed(backend_priv: &x25519_dalek::StaticSecret) -> homeway_quic::Ed25519Seed {
    const RPK_LABEL: &[u8] = b"homeway/quic-rpk";
    homeway_quic::Ed25519Seed::from_bytes(crate::psk::hkdf_expand_32(
        &backend_priv.to_bytes(),
        RPK_LABEL,
    ))
}
