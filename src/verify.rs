use sha1::{Digest, Sha1};
use sha2::Sha256;

pub fn verify_v1_piece(bytes: &[u8], expected: &[u8; 20]) -> bool {
    Sha1::digest(bytes).as_slice() == expected
}

pub fn verify_v2_block(bytes: &[u8], expected: &[u8; 32]) -> bool {
    Sha256::digest(bytes).as_slice() == expected
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verifies_content_before_it_becomes_trusted() {
        let v1: [u8; 20] = Sha1::digest(b"piece").into();
        let v2: [u8; 32] = Sha256::digest(b"piece").into();
        assert!(verify_v1_piece(b"piece", &v1));
        assert!(!verify_v1_piece(b"corrupt", &v1));
        assert!(verify_v2_block(b"piece", &v2));
    }
}
