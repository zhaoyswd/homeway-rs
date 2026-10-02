#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = homeway_core::probe::decode_response(data, &[0x11; 8]);
    let _ = homeway_core::probe::decode_response(data, &[0; 8]);
});
