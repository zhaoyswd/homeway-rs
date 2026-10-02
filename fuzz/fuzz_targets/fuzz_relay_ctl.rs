#![no_main]
use libfuzzer_sys::fuzz_target;

use homeway_core::relaywire as rw;
fuzz_target!(|data: &[u8]| {
    let _ = rw::decode_hello(data);
    let _ = rw::decode_challenge(data);
    let _ = rw::decode_proof(data);
    let _ = rw::decode_ok_auth(data);
    let _ = rw::decode_session(data);
    let _ = rw::decode_release(data);
    let _ = rw::legup_cookie(data);
    // 跨 chunk 状态机：整段一次喂（分块等价断言在 replay 轨）
    let mut dec = rw::CtlDecoder::new();
    let mut out = Vec::new();
    let _ = dec.feed(data, &mut out);
});
