#![no_main]
use libfuzzer_sys::fuzz_target;

use homeway_core::legframe;
fuzz_target!(|data: &[u8]| {
    let _ = legframe::decode_frame(data);
    let _ = legframe::decode_tagged(data);
    let _ = legframe::decode_batch(data);
    let _ = legframe::decode_hint_payload(data);
    let _ = homeway_core::relaywire::decode_relay_reg_frame(data);
});
