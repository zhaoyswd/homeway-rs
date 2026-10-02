#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let s = String::from_utf8_lossy(data).into_owned();
    let _ = homeway_core::token::decode(&s);
    let _ = homeway_core::token::parse_body(data);
});
