#![no_main]
use libfuzzer_sys::fuzz_target;
fuzz_target!(|data: &[u8]| { if data.len()<=65536 {let _=peerlume::tracker::parse_announce_response(data);} });
