#![no_main]
use libfuzzer_sys::fuzz_target;

use homeway_core::server::upnp;
fuzz_target!(|data: &[u8]| {
    let s = String::from_utf8_lossy(data).into_owned();
    let _ = upnp::header_value(&s, "LOCATION");
    let _ = upnp::header_value(&s, "ST");
    let _ = upnp::xml_tag(&s, "controlURL");
    let _ = upnp::xml_tag(&s, "NewExternalPort");
    let _ = upnp::parse_http_url(&s);
});
