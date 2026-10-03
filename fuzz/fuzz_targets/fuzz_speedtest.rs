#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = homeway_core::speedtest::decode_frame(data);
    let _ = homeway_core::speedtest::parse_report(data);
    // 服务端半边（第二道门 高-4：请求 JSON 手解面——引号感知切分的服务端形态）
    if let Some(req) = homeway_core::speedtest_server::parse_request(data) {
        assert!(!req.role.is_empty() && req.role.len() <= data.len());
    }
});
