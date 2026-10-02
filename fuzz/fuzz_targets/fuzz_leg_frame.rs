#![no_main]
use libfuzzer_sys::fuzz_target;

use homeway_core::wtransport::frame;
fuzz_target!(|data: &[u8]| {
    let _ = frame::decode_frame(data);
    let _ = frame::decode_tagged(data);
    let _ = frame::decode_batch(data);
    let _ = frame::decode_hint_payload(data);
    let _ = homeway_core::relaywire::decode_relay_reg_frame(data);
});
