#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = homeway_core::speedtest::decode_frame(data);
    let _ = homeway_core::speedtest::parse_report(data);
});
