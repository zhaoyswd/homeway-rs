//! 体积探针：同一份"核壳"分别在有/无 QUIC 依赖下构建为 cdylib，量出增量。
//! 导出面模拟 libclientcore.so 的 C-ABI 形态（no_mangle + #[used] 保活）。

use std::ffi::CString;

#[no_mangle]
pub extern "C" fn ClientCoreVersion() -> *mut std::os::raw::c_char {
    CString::new("size-probe").unwrap().into_raw()
}

/// 被引用即保活的 QUIC 路径（防 LTO 把未用代码全 eliminate）
#[cfg(feature = "quic")]
#[no_mangle]
pub extern "C" fn SizeProbeQuicTouch() -> usize {
    use quinn::{TransportConfig, VarInt};
    let mut t = TransportConfig::default();
    t.datagram_send_buffer_size(1 << 20);
    t.max_concurrent_bidi_streams(VarInt::from_u32(0));
    let _ = rustls::crypto::ring::default_provider();
    std::mem::size_of_val(&t)
        + std::mem::size_of::<tokio::runtime::Runtime>()
        + rustls::crypto::ring::default_provider().cipher_suites.len()
}
