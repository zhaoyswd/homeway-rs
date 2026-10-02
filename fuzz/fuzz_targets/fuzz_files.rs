#![no_main]
use libfuzzer_sys::fuzz_target;

use homeway_core::files::decode_prefix;
fuzz_target!(|data: &[u8]| {
    let _ = decode_prefix(data);
});
