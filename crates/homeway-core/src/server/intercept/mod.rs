//! 拦截层（R3；语义真源 `pkg/intercept`）。
//!
//! 挂在出口隧道侧栈上，把「WG 解密后的明文 IP 包」按目的地址分流（tun2socks 同款语义，
//! smoltcp 形态 = 包级 NAT 重写——见 `nat.rs` 头注释）：
//!
//!	dst == 隧道IP → 豁免：LocalServices 命中端口转投 UDS、其余回环同端口重拨
//!	dst == 其它   → 过境：终结（栈内 TCP 状态机）+ 本机 socket 重拨
//!
//! **拨号先行**（设计 §4.1 / 评审 H2）：TCP SYN 不立即回 SYN-ACK——先建映射缓存 SYN、
//! worker 拨 upstream 成功（DialOk → Adopt）后才建栈内 socket 注入缓存（SYN-ACK 由此
//! 产生）；失败构造 RST 回客户端。三态（建立时点/失败可见性/黑洞 10s）与 Go
//! （Forwarder 先拨号后 CreateEndpoint）等价。

pub mod nat;
pub mod pool;
