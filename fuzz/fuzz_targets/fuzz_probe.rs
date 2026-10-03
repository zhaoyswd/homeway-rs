#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // 固定 nonce（nonce 不匹配拒绝分支）+ 从输入自取 nonce（深层解析可达——
    // nonce 门在载荷解析之前，固定 nonce 的变异打不进端点计数/flags/列表段；
    // 服务端攻击面本来就是任意源全控字节，nonce 只是语义层校验。第二道门 高-3）
    let _ = homeway_core::probe::decode_response(data, &[0x11; 8]);
    if data.len() >= 13 {
        let nonce: [u8; 8] = data[5..13].try_into().expect("已判 13B");
        if let Ok(r) = homeway_core::probe::decode_response(data, &nonce) {
            assert!(r.endpoints.len() <= homeway_core::probe::MAX_ENDPOINTS);
        }
    }
});
