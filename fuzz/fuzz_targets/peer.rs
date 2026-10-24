#![no_main]
use libfuzzer_sys::fuzz_target;
fuzz_target!(|data: &[u8]| { let _=peerlume::protocol::Handshake::decode(data);let _=peerlume::protocol::PeerMessage::decode(data,65536); });
