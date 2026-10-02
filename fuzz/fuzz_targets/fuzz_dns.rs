#![no_main]
use libfuzzer_sys::fuzz_target;

use homeway_core::server::dnsproxy as dp;
use homeway_core::server::intercept::dnsface;
fuzz_target!(|data: &[u8]| {
    let _ = dp::qtype(data);
    let _ = dp::empty_response(data);
    let mut m = data.to_vec();
    dp::clamp_ttl(&mut m, 60);
    let _ = dp::count_aaaa(&m);
    let _ = dp::truncate(&m, 1232);
    let _ = dnsface::decode_tcp_frame(data);
});
