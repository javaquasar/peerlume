//! Seekable deterministic payloads: fixture memory does not scale with file size.
use sha1::{Digest, Sha1};
use std::{
    fs,
    io::{self, Read, Seek, SeekFrom, Write},
    path::Path,
    sync::Arc,
};

pub(super) const BLOCK: usize = 16 * 1024;
pub(super) const PIECE: usize = 256 * 1024;

#[derive(Clone)]
pub(super) struct Data {
    pub name: String,
    pub files: Vec<(String, u64)>,
    pub bytes: u64,
    pub hash: [u8; 20],
    pub meta: Arc<Vec<u8>>,
    template: Arc<Vec<u8>>,
    seed: u64,
}

impl Data {
    pub fn new(name: &str, bytes: u64, multi: bool, seed: u64) -> Self {
        assert!(bytes >= PIECE as u64 && bytes.is_multiple_of(PIECE as u64));
        let files = if multi {
            vec![
                ("first.bin".into(), bytes / 2 + 123),
                ("nested/second.bin".into(), bytes / 4 + 17),
                ("last.bin".into(), bytes / 4 - 140),
            ]
        } else {
            vec![(name.into(), bytes)]
        };
        let mut data = Self {
            name: name.into(),
            files,
            bytes,
            hash: [0; 20],
            meta: Default::default(),
            template: Arc::new(
                (0..BLOCK)
                    .map(|i| ((i * 31 + (seed as usize) * 17) % 251) as u8)
                    .collect(),
            ),
            seed,
        };
        let mut info = b"d".to_vec();
        if multi {
            info.extend(b"5:filesl");
            for (path, length) in &data.files {
                info.extend(format!("d6:lengthi{length}e4:pathl").bytes());
                for component in path.split('/') {
                    info.extend(format!("{}:{component}", component.len()).bytes());
                }
                info.extend(b"ee");
            }
            info.push(b'e');
        } else {
            info.extend(format!("6:lengthi{bytes}e").bytes());
        }
        info.extend(
            format!(
                "4:name{}:{name}12:piece lengthi{PIECE}e6:pieces{}:",
                name.len(),
                bytes / PIECE as u64 * 20
            )
            .bytes(),
        );
        let mut buffer = vec![0; PIECE];
        for index in 0..bytes / PIECE as u64 {
            data.fill(index * PIECE as u64, &mut buffer);
            info.extend(Sha1::digest(&buffer));
        }
        info.extend(b"7:privatei1ee");
        data.hash = Sha1::digest(&info).into();
        let mut meta = b"d4:info".to_vec();
        meta.extend(info);
        meta.push(b'e');
        data.meta = Arc::new(meta);
        data
    }

    pub fn hash_hex(&self) -> String {
        self.hash.iter().map(|b| format!("{b:02x}")).collect()
    }
    pub fn pieces(&self) -> usize {
        (self.bytes / PIECE as u64) as usize
    }

    pub fn fill(&self, mut offset: u64, mut out: &mut [u8]) {
        assert!(offset <= self.bytes && out.len() as u64 <= self.bytes - offset);
        while !out.is_empty() {
            let within = (offset % BLOCK as u64) as usize;
            let n = out.len().min(BLOCK - within);
            out[..n].copy_from_slice(&self.template[within..within + n]);
            let mut tag = [0; 16];
            tag[..8].copy_from_slice(&self.seed.to_le_bytes());
            tag[8..].copy_from_slice(&(offset / BLOCK as u64).to_le_bytes());
            if within < tag.len() {
                let tagged = n.min(tag.len() - within);
                out[..tagged].copy_from_slice(&tag[within..within + tagged]);
            }
            offset += n as u64;
            out = &mut out[n..];
        }
    }

    pub fn write_files(&self, root: &Path) -> io::Result<()> {
        let mut offset = 0;
        let mut buffer = vec![0; 64 * 1024];
        for (path, length) in &self.files {
            let path = root.join(path);
            fs::create_dir_all(path.parent().unwrap())?;
            let mut file = fs::File::create(path)?;
            let mut left = *length;
            while left > 0 {
                let n = left.min(buffer.len() as u64) as usize;
                self.fill(offset, &mut buffer[..n]);
                file.write_all(&buffer[..n])?;
                offset += n as u64;
                left -= n as u64;
            }
        }
        Ok(())
    }

    pub fn verify_files(&self, root: &Path) -> io::Result<()> {
        let mut offset = 0;
        let mut got = vec![0; 64 * 1024];
        let mut expected = vec![0; 64 * 1024];
        for (path, length) in &self.files {
            let mut file = fs::File::open(root.join(path))?;
            if file.metadata()?.len() != *length {
                return Err(io::Error::other("wrong downloaded file size"));
            }
            let mut left = *length;
            while left > 0 {
                let n = left.min(got.len() as u64) as usize;
                file.read_exact(&mut got[..n])?;
                self.fill(offset, &mut expected[..n]);
                if got[..n] != expected[..n] {
                    return Err(io::Error::other(
                        "downloaded payload differs from the deterministic source",
                    ));
                }
                offset += n as u64;
                left -= n as u64;
            }
        }
        Ok(())
    }

    pub fn corrupt(&self, root: &Path, offsets: &[u64]) -> io::Result<()> {
        for &offset in offsets {
            let mut start = 0;
            let mut found = false;
            for (path, length) in &self.files {
                if offset < start + length {
                    let mut file = fs::OpenOptions::new().write(true).open(root.join(path))?;
                    file.seek(SeekFrom::Start(offset - start))?;
                    let mut value = [0];
                    self.fill(offset, &mut value);
                    file.write_all(&[value[0] ^ 0xff])?;
                    file.sync_all()?;
                    found = true;
                    break;
                }
                start += length;
            }
            if !found {
                return Err(io::Error::other("fault offset out of range"));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn streamed_ranges_match_whole_payload_across_tags_blocks_and_files() {
        let data = Data::new("bundle", (PIECE * 2) as u64, true, 9);
        let mut all = vec![0; data.bytes as usize];
        data.fill(0, &mut all);
        for offset in [0, 3, 15, 16, BLOCK - 3, BLOCK + 7, PIECE - 13, PIECE + 121] {
            let mut part = vec![0; BLOCK + 31];
            data.fill(offset as u64, &mut part);
            assert_eq!(part, all[offset..offset + part.len()]);
        }
        assert_ne!(&all[..BLOCK], &all[BLOCK..2 * BLOCK]);
        let root = tempfile::tempdir().unwrap();
        data.write_files(root.path()).unwrap();
        data.verify_files(root.path()).unwrap();
        let boundary = data.files[0].1;
        data.corrupt(root.path(), &[boundary - 1, boundary + 1])
            .unwrap();
        assert!(data.verify_files(root.path()).is_err());
        data.write_files(root.path()).unwrap();
        data.verify_files(root.path()).unwrap();
    }
    #[test]
    fn generated_private_metainfo_has_exact_sizes_paths_hashes_and_piece_digests() {
        for multi in [false, true] {
            let data = Data::new("test.bin", (PIECE * 3) as u64, multi, 17);
            let parsed = crate::metainfo::TorrentMetainfo::parse(&data.meta).unwrap();
            assert!(parsed.is_private);
            assert_eq!(
                parsed.info_hash_v1_hex.as_deref(),
                Some(data.hash_hex().as_str())
            );
            assert_eq!(
                parsed.files.iter().map(|f| f.size_bytes).sum::<u64>(),
                data.bytes
            );
            assert_eq!(parsed.files.len(), if multi { 3 } else { 1 });
            let mut piece = vec![0; PIECE];
            data.fill(PIECE as u64, &mut piece);
            let digest = Sha1::digest(piece);
            assert!(data.meta.windows(20).any(|v| v == digest.as_slice()));
        }
    }
    #[test]
    fn fixtures_keep_a_single_block_template_and_reject_invalid_ranges() {
        let data = Data::new("small.bin", PIECE as u64, false, 3);
        assert_eq!(data.template.len(), BLOCK);
        assert!(std::panic::catch_unwind(|| data.fill(data.bytes, &mut [0])).is_err());
        let root = tempfile::tempdir().unwrap();
        assert!(data.corrupt(root.path(), &[data.bytes]).is_err());
    }
}
