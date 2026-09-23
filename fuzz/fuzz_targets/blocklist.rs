#![no_main]

use std::io::Cursor;

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let limits = peerlume::limits::ResourceLimits {
        max_blocklist_decompressed_bytes: 64 * 1024,
        max_blocklist_rules: 4096,
        ..Default::default()
    };
    let _ = peerlume::blocklist::Blocklist::from_reader_with_limits(Cursor::new(data), &limits);
});
