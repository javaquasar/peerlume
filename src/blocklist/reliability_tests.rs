use super::*;
fn legacy(count: u64, ranges: &[(u32, u32)]) -> Vec<u8> {
    let mut b = V1_CACHE_MAGIC.to_vec();
    b.extend(count.to_le_bytes());
    b.extend(count.to_le_bytes());
    for (a, z) in ranges {
        b.extend(a.to_le_bytes());
        b.extend(z.to_le_bytes());
    }
    b
}
#[test]
fn legacy_cache_rejects_huge_count_before_allocation() {
    let root = tempfile::tempdir().unwrap();
    let p = root.path().join("cache");
    std::fs::write(&p, legacy(u64::MAX, &[])).unwrap();
    assert!(Blocklist::read_v1_binary_cache(&p).is_err());
}
#[test]
fn legacy_cache_checks_order_length_overlap_and_boundaries() {
    let root = tempfile::tempdir().unwrap();
    let p = root.path().join("cache");
    for ranges in [vec![(2, 1)], vec![(0, 10), (5, 20)], vec![(10, 20), (0, 5)]] {
        std::fs::write(&p, legacy(ranges.len() as u64, &ranges)).unwrap();
        assert!(Blocklist::read_v1_binary_cache(&p).is_err());
    }
    let mut bytes = legacy(1, &[(0, u32::MAX)]);
    bytes.push(0);
    std::fs::write(&p, &bytes).unwrap();
    assert!(Blocklist::read_v1_binary_cache(&p).is_err());
    bytes.pop();
    std::fs::write(&p, bytes).unwrap();
    let b = Blocklist::read_v1_binary_cache(&p).unwrap();
    assert!(b.is_blocked("0.0.0.0".parse().unwrap()));
    assert!(b.is_blocked("255.255.255.255".parse().unwrap()));
}
#[test]
fn every_truncation_of_a_valid_v2_cache_is_rejected() {
    let root = tempfile::tempdir().unwrap();
    let p = root.path().join("cache");
    Blocklist::parse("a:1.2.3.4-1.2.3.5\nv6:2001:db8::-2001:db8::ff")
        .write_binary_cache(&p)
        .unwrap();
    let bytes = std::fs::read(&p).unwrap();
    for length in 0..bytes.len() {
        std::fs::write(&p, &bytes[..length]).unwrap();
        assert!(Blocklist::read_binary_cache(&p).is_err(), "{length}");
    }
}
#[test]
fn gzip_truncation_and_small_compressed_budget_are_errors() {
    use flate2::{Compression, write::GzEncoder};
    let mut e = GzEncoder::new(Vec::new(), Compression::fast());
    e.write_all(b"a:1.2.3.4-1.2.3.5\n").unwrap();
    let bytes = e.finish().unwrap();
    assert!(decode_gzip_blocklist(&bytes[..bytes.len() - 4], &ResourceLimits::default()).is_err());
}
