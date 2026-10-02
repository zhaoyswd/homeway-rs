#![no_main]
use libfuzzer_sys::fuzz_target;

use homeway_core::server::intercept::nat::Ipv4View;
fuzz_target!(|data: &[u8]| {
    let _ = Ipv4View::parse(data);
});
