use sha1::{Digest, Sha1};

pub const METADATA_BLOCK_BYTES: usize = 16 * 1024;

pub struct MetadataAssembler {
    total_size: usize,
    blocks: Vec<Option<Vec<u8>>>,
}

impl MetadataAssembler {
    pub fn new(total_size: usize, max_size: usize) -> Result<Self, String> {
        if total_size == 0 || total_size > max_size {
            return Err("metadata size is outside configured limits".into());
        }
        let block_count = total_size.div_ceil(METADATA_BLOCK_BYTES);
        Ok(Self {
            total_size,
            blocks: vec![None; block_count],
        })
    }

    pub fn missing_blocks(&self) -> impl Iterator<Item = u32> + '_ {
        self.blocks
            .iter()
            .enumerate()
            .filter(|(_, block)| block.is_none())
            .map(|(index, _)| index as u32)
    }

    pub fn insert(&mut self, index: u32, bytes: Vec<u8>) -> Result<bool, String> {
        let index: usize = index
            .try_into()
            .map_err(|_| "metadata block index is too large".to_owned())?;
        let block_count = self.blocks.len();
        let block = self
            .blocks
            .get_mut(index)
            .ok_or_else(|| "metadata block index is out of range".to_owned())?;
        let expected = if index + 1 == block_count {
            self.total_size - index * METADATA_BLOCK_BYTES
        } else {
            METADATA_BLOCK_BYTES
        };
        if bytes.len() != expected {
            return Err("metadata block has an unexpected length".into());
        }
        if let Some(existing) = block {
            if existing != &bytes {
                return Err("peer sent conflicting duplicate metadata block".into());
            }
            return Ok(false);
        }
        *block = Some(bytes);
        Ok(true)
    }

    pub fn assemble_v1(self, expected_info_hash: &[u8; 20]) -> Result<Vec<u8>, String> {
        if self.blocks.iter().any(Option::is_none) {
            return Err("metadata is incomplete".into());
        }
        let mut metadata = Vec::with_capacity(self.total_size);
        for block in self.blocks {
            metadata.extend(block.expect("completeness checked above"));
        }
        let actual: [u8; 20] = Sha1::digest(&metadata).into();
        if &actual != expected_info_hash {
            return Err("metadata info hash verification failed".into());
        }
        Ok(metadata)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assembles_out_of_order_metadata_and_verifies_hash() {
        let metadata = vec![7; METADATA_BLOCK_BYTES + 5];
        let expected: [u8; 20] = Sha1::digest(&metadata).into();
        let mut assembler = MetadataAssembler::new(metadata.len(), 1_000_000).unwrap();
        assert!(
            assembler
                .insert(1, metadata[METADATA_BLOCK_BYTES..].to_vec())
                .unwrap()
        );
        assert!(
            assembler
                .insert(0, metadata[..METADATA_BLOCK_BYTES].to_vec())
                .unwrap()
        );
        assert_eq!(assembler.assemble_v1(&expected).unwrap(), metadata);
    }

    #[test]
    fn rejects_oversize_wrong_length_and_conflicting_duplicates() {
        assert!(MetadataAssembler::new(10, 9).is_err());
        let mut assembler = MetadataAssembler::new(3, 10).unwrap();
        assert!(assembler.insert(0, vec![1, 2]).is_err());
        assert!(assembler.insert(0, vec![1, 2, 3]).unwrap());
        assert!(assembler.insert(0, vec![3, 2, 1]).is_err());
    }
}
