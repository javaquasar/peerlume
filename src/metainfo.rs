use std::{fs, path::Path};

use sha1::{Digest, Sha1};
use sha2::Sha256;

use crate::limits::{ResourceLimits, validate_relative_paths};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MetainfoFile {
    pub path: String,
    pub size_bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TorrentMetainfo {
    pub name: String,
    pub files: Vec<MetainfoFile>,
    pub trackers: Vec<String>,
    pub is_private: bool,
    pub info_hash_v1_hex: Option<String>,
    pub info_hash_v2_hex: Option<String>,
}

impl TorrentMetainfo {
    pub fn load(path: &Path) -> Result<Self, String> {
        Self::load_with_limits(path, &ResourceLimits::default())
    }

    pub fn load_with_limits(path: &Path, limits: &ResourceLimits) -> Result<Self, String> {
        let metadata = fs::metadata(path).map_err(|error| error.to_string())?;
        if metadata.len() > limits.max_torrent_bytes {
            return Err(format!(
                "torrent file is larger than {} MiB",
                limits.max_torrent_bytes / 1024 / 1024
            ));
        }
        let bytes = fs::read(path).map_err(|error| error.to_string())?;
        Self::parse_with_limits(&bytes, limits)
    }

    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        Self::parse_with_limits(bytes, &ResourceLimits::default())
    }

    pub fn parse_with_limits(bytes: &[u8], limits: &ResourceLimits) -> Result<Self, String> {
        if bytes.len() as u64 > limits.max_torrent_bytes {
            return Err("torrent exceeds configured input limit".into());
        }
        let (root, info_bytes) = Parser::new(bytes, limits).parse_metainfo()?;
        let root = root.as_dict("torrent root")?;
        let info = root
            .get(b"info".as_slice())
            .ok_or("torrent has no info dictionary")?
            .as_dict("info")?;

        let name = info
            .get(b"name.utf-8".as_slice())
            .or_else(|| info.get(b"name".as_slice()))
            .ok_or("info dictionary has no name")?
            .as_text("name")?;
        if name.trim().is_empty() {
            return Err("torrent name is empty".into());
        }
        validate_component(&name)?;

        let files = parse_files(info, &name)?;
        if files.is_empty() {
            return Err("torrent contains no files".into());
        }
        if files.len() > limits.max_files {
            return Err("torrent exceeds configured file-count limit".into());
        }
        validate_relative_paths(files.iter().map(|file| file.path.as_str()), limits)?;
        files.iter().try_fold(0_u64, |total, file| {
            total
                .checked_add(file.size_bytes)
                .filter(|total| *total <= limits.max_total_content_bytes)
                .ok_or_else(|| "torrent exceeds configured content-size limit".to_owned())
        })?;

        let mut trackers = Vec::new();
        if let Some(value) = root.get(b"announce".as_slice()) {
            push_tracker(&mut trackers, value.as_text("announce")?);
        }
        if let Some(BValue::List(tiers)) = root.get(b"announce-list".as_slice()) {
            for tier in tiers {
                if let BValue::List(urls) = tier {
                    for url in urls {
                        if let Ok(url) = url.as_text("announce-list URL") {
                            push_tracker(&mut trackers, url);
                        }
                    }
                }
            }
        }
        if trackers.len() > limits.max_trackers {
            return Err("torrent exceeds configured tracker-count limit".into());
        }

        let is_private = matches!(info.get(b"private".as_slice()), Some(BValue::Int(1)));
        let is_v2 = matches!(info.get(b"meta version".as_slice()), Some(BValue::Int(2)));
        let has_v1_pieces = info.contains_key(b"pieces".as_slice());
        let info_hash_v1_hex =
            (!is_v2 || has_v1_pieces).then(|| format!("{:x}", Sha1::digest(info_bytes)));
        let info_hash_v2_hex = is_v2.then(|| format!("{:x}", Sha256::digest(info_bytes)));
        Ok(Self {
            name,
            files,
            trackers,
            is_private,
            info_hash_v1_hex,
            info_hash_v2_hex,
        })
    }

    pub fn total_size(&self) -> u64 {
        self.files.iter().map(|file| file.size_bytes).sum()
    }
}

pub fn normalize_peer_id_prefix(value: &str) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() || value.len() > 8 || !value.is_ascii() {
        return Err("peer ID prefix must contain 1 to 8 ASCII bytes".into());
    }
    if value.bytes().any(|byte| byte.is_ascii_control()) {
        return Err("peer ID prefix cannot contain control characters".into());
    }
    Ok(value.to_owned())
}

pub fn validate_peer_id(value: &str) -> Result<(), String> {
    if value.len() != 20 || !value.is_ascii() {
        return Err("peer ID must contain exactly 20 ASCII bytes".into());
    }
    if value.bytes().any(|byte| byte.is_ascii_control()) {
        return Err("peer ID cannot contain control characters".into());
    }
    Ok(())
}

pub fn generate_peer_id(prefix: &str) -> String {
    crate::identity::generate_peer_id(prefix)
        .or_else(|_| crate::identity::generate_peer_id("-PL0001-"))
        .expect("operating-system random generator must be available")
}

fn push_tracker(trackers: &mut Vec<String>, tracker: String) {
    if !tracker.trim().is_empty() && !trackers.contains(&tracker) {
        trackers.push(tracker);
    }
}

fn parse_files(info: &BDict<'_>, name: &str) -> Result<Vec<MetainfoFile>, String> {
    if let Some(BValue::List(values)) = info.get(b"files".as_slice()) {
        let mut files = Vec::with_capacity(values.len());
        for value in values {
            let file = value.as_dict("file")?;
            let length = nonnegative_integer(file.get(b"length".as_slice()), "file length")?;
            let path_value = file
                .get(b"path.utf-8".as_slice())
                .or_else(|| file.get(b"path".as_slice()))
                .ok_or("file has no path")?;
            let BValue::List(components) = path_value else {
                return Err("file path is not a list".into());
            };
            let mut path = Vec::with_capacity(components.len());
            for component in components {
                path.push(safe_component_bytes(component.as_bytes("path component")?)?);
            }
            files.push(MetainfoFile {
                path: path.join("/"),
                size_bytes: length,
            });
        }
        return Ok(files);
    }

    if let Some(BValue::Dict(tree)) = info.get(b"file tree".as_slice()) {
        let mut files = Vec::new();
        let mut prefix = Vec::new();
        parse_v2_tree(tree, &mut prefix, &mut files)?;
        return Ok(files);
    }

    let length = nonnegative_integer(info.get(b"length".as_slice()), "file length")?;
    Ok(vec![MetainfoFile {
        path: safe_component(name)?,
        size_bytes: length,
    }])
}

fn parse_v2_tree(
    tree: &BDict<'_>,
    prefix: &mut Vec<String>,
    files: &mut Vec<MetainfoFile>,
) -> Result<(), String> {
    for (name, value) in &tree.0 {
        let dictionary = value.as_dict("file tree entry")?;
        if name.is_empty() {
            let length =
                nonnegative_integer(dictionary.get(b"length".as_slice()), "v2 file length")?;
            files.push(MetainfoFile {
                path: prefix.join("/"),
                size_bytes: length,
            });
        } else {
            prefix.push(safe_component_bytes(name)?);
            parse_v2_tree(dictionary, prefix, files)?;
            prefix.pop();
        }
    }
    Ok(())
}

fn nonnegative_integer(value: Option<&BValue>, field: &str) -> Result<u64, String> {
    match value {
        Some(BValue::Int(value)) if *value >= 0 => Ok(*value as u64),
        _ => Err(format!("{field} is missing or invalid")),
    }
}

fn validate_component(value: &str) -> Result<(), String> {
    if value.is_empty() || value == "." || value == ".." || value.contains(['/', '\0', '\\']) {
        return Err(format!("unsafe path component: {value:?}"));
    }
    Ok(())
}

fn safe_component(value: &str) -> Result<String, String> {
    validate_component(value)?;
    Ok(value.to_owned())
}

fn safe_component_bytes(value: &[u8]) -> Result<String, String> {
    let value = String::from_utf8_lossy(value);
    validate_component(&value)?;
    Ok(value.into_owned())
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum BValue<'a> {
    Int(i64),
    Bytes(&'a [u8]),
    List(Vec<BValue<'a>>),
    Dict(BDict<'a>),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct BDict<'a>(Vec<(&'a [u8], BValue<'a>)>);

impl<'a> BDict<'a> {
    fn get(&self, key: &[u8]) -> Option<&BValue<'a>> {
        self.0
            .iter()
            .find_map(|(candidate, value)| (*candidate == key).then_some(value))
    }

    fn contains_key(&self, key: &[u8]) -> bool {
        self.get(key).is_some()
    }

    fn insert(&mut self, key: &'a [u8], value: BValue<'a>) {
        if let Some((_, existing)) = self.0.iter_mut().find(|(candidate, _)| *candidate == key) {
            *existing = value;
        } else {
            self.0.push((key, value));
        }
    }
}

impl<'a> BValue<'a> {
    fn as_dict(&self, field: &str) -> Result<&BDict<'a>, String> {
        match self {
            Self::Dict(value) => Ok(value),
            _ => Err(format!("{field} is not a dictionary")),
        }
    }

    fn as_text(&self, field: &str) -> Result<String, String> {
        match self {
            Self::Bytes(value) => Ok(String::from_utf8_lossy(value).into_owned()),
            _ => Err(format!("{field} is not a byte string")),
        }
    }

    fn as_bytes(&self, field: &str) -> Result<&'a [u8], String> {
        match self {
            Self::Bytes(value) => Ok(value),
            _ => Err(format!("{field} is not a byte string")),
        }
    }
}

struct Parser<'a> {
    input: &'a [u8],
    position: usize,
    limits: &'a ResourceLimits,
    nodes: usize,
}

impl<'a> Parser<'a> {
    fn new(input: &'a [u8], limits: &'a ResourceLimits) -> Self {
        Self {
            input,
            position: 0,
            limits,
            nodes: 0,
        }
    }

    fn parse_metainfo(mut self) -> Result<(BValue<'a>, &'a [u8]), String> {
        self.expect(b'd')?;
        let mut root = BDict::default();
        let mut info_range = None;
        while self.peek()? != b'e' {
            let key = self.parse_bytes()?;
            let start = self.position;
            if !matches!(key, b"info" | b"announce" | b"announce-list") {
                self.skip_value(1)?;
                continue;
            }
            let value = self.parse_value(1)?;
            if key == b"info" {
                info_range = Some(start..self.position);
            }
            root.insert(key, value);
        }
        self.position += 1;
        if self.position != self.input.len() {
            return Err("trailing data after torrent dictionary".into());
        }
        let range = info_range.ok_or("torrent has no info dictionary")?;
        Ok((BValue::Dict(root), &self.input[range]))
    }

    fn parse_value(&mut self, depth: usize) -> Result<BValue<'a>, String> {
        self.nodes = self.nodes.saturating_add(1);
        if depth > self.limits.max_bencode_depth || self.nodes > self.limits.max_bencode_nodes {
            return Err("bencode nesting limit exceeded".into());
        }
        match self.peek()? {
            b'i' => self.parse_integer().map(BValue::Int),
            b'l' => {
                self.position += 1;
                let mut values = Vec::new();
                while self.peek()? != b'e' {
                    values.push(self.parse_value(depth + 1)?);
                }
                self.position += 1;
                Ok(BValue::List(values))
            }
            b'd' => {
                self.position += 1;
                let mut values = BDict::default();
                while self.peek()? != b'e' {
                    let key = self.parse_bytes()?;
                    values.insert(key, self.parse_value(depth + 1)?);
                }
                self.position += 1;
                Ok(BValue::Dict(values))
            }
            b'0'..=b'9' => self.parse_bytes().map(BValue::Bytes),
            value => Err(format!("invalid bencode marker 0x{value:02x}")),
        }
    }

    fn skip_value(&mut self, depth: usize) -> Result<(), String> {
        self.nodes = self.nodes.saturating_add(1);
        if depth > self.limits.max_bencode_depth || self.nodes > self.limits.max_bencode_nodes {
            return Err("bencode nesting limit exceeded".into());
        }
        match self.peek()? {
            b'i' => {
                self.parse_integer()?;
            }
            b'l' => {
                self.position += 1;
                while self.peek()? != b'e' {
                    self.skip_value(depth + 1)?;
                }
                self.position += 1;
            }
            b'd' => {
                self.position += 1;
                while self.peek()? != b'e' {
                    self.parse_bytes()?;
                    self.skip_value(depth + 1)?;
                }
                self.position += 1;
            }
            b'0'..=b'9' => {
                self.parse_bytes()?;
            }
            value => return Err(format!("invalid bencode marker 0x{value:02x}")),
        }
        Ok(())
    }

    fn parse_integer(&mut self) -> Result<i64, String> {
        self.expect(b'i')?;
        let start = self.position;
        while self.peek()? != b'e' {
            self.position += 1;
        }
        let text = std::str::from_utf8(&self.input[start..self.position])
            .map_err(|_| "integer is not ASCII")?;
        self.position += 1;
        text.parse::<i64>()
            .map_err(|_| format!("invalid integer: {text}"))
    }

    fn parse_bytes(&mut self) -> Result<&'a [u8], String> {
        let start = self.position;
        while self.peek()?.is_ascii_digit() {
            self.position += 1;
        }
        if self.position == start || self.peek()? != b':' {
            return Err("invalid byte string length".into());
        }
        let length = std::str::from_utf8(&self.input[start..self.position])
            .map_err(|_| "byte string length is not ASCII")?
            .parse::<usize>()
            .map_err(|_| "invalid byte string length")?;
        self.position += 1;
        let end = self
            .position
            .checked_add(length)
            .filter(|end| *end <= self.input.len())
            .ok_or("byte string extends past end of file")?;
        let value = &self.input[self.position..end];
        self.position = end;
        Ok(value)
    }

    fn expect(&mut self, value: u8) -> Result<(), String> {
        if self.peek()? != value {
            return Err(format!("expected bencode marker 0x{value:02x}"));
        }
        self.position += 1;
        Ok(())
    }

    fn peek(&self) -> Result<u8, String> {
        self.input
            .get(self.position)
            .copied()
            .ok_or_else(|| "unexpected end of bencode data".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn parses_private_multifile_torrent_and_exact_info_hash() {
        let bytes = b"d8:announce24:https://tracker/announce13:announce-listll24:https://tracker/announceee4:infod5:filesld6:lengthi12e4:pathl5:a.txteed6:lengthi34e4:pathl3:dir5:b.bineee4:name4:demo7:privatei1eee";
        let parsed = TorrentMetainfo::parse(bytes).unwrap();
        assert_eq!(parsed.name, "demo");
        assert_eq!(parsed.files.len(), 2);
        assert_eq!(parsed.files[0].path, "a.txt");
        assert_eq!(parsed.files[1].path, "dir/b.bin");
        assert_eq!(parsed.total_size(), 46);
        assert!(parsed.is_private);
        assert_eq!(parsed.trackers, vec!["https://tracker/announce"]);

        let info_start = bytes.windows(6).position(|part| part == b"4:info").unwrap() + 6;
        assert_eq!(
            parsed.info_hash_v1_hex.unwrap(),
            format!("{:x}", Sha1::digest(&bytes[info_start..bytes.len() - 1]))
        );
    }

    #[test]
    fn rejects_parent_directory_path_components() {
        let bytes = b"d4:infod5:filesld6:lengthi1e4:pathl2:..1:xeee4:name4:demoe e";
        assert!(TorrentMetainfo::parse(bytes).is_err());
    }

    #[test]
    fn parses_v2_file_tree_and_sha256_info_hash() {
        let bytes =
            b"d4:infod9:file treed5:a.txtd0:d6:lengthi7eeee12:meta versioni2e4:name4:demoee";
        let parsed = TorrentMetainfo::parse(bytes).unwrap();
        assert_eq!(parsed.files[0].path, "a.txt");
        assert_eq!(parsed.files[0].size_bytes, 7);
        assert!(parsed.info_hash_v1_hex.is_none());

        let info_start = bytes.windows(6).position(|part| part == b"4:info").unwrap() + 6;
        assert_eq!(
            parsed.info_hash_v2_hex.unwrap(),
            format!("{:x}", Sha256::digest(&bytes[info_start..bytes.len() - 1]))
        );
    }

    #[test]
    fn peer_ids_are_valid_and_use_configured_prefix() {
        let first = generate_peer_id("-PL0001-");
        let second = generate_peer_id("TEST");
        assert_eq!(first.len(), 20);
        assert!(first.starts_with("-PL0001-"));
        assert_eq!(second.len(), 20);
        assert!(second.starts_with("TEST"));
        assert_ne!(first, second);
        assert!(validate_peer_id(&first).is_ok());
        assert!(normalize_peer_id_prefix("123456789").is_err());
    }

    #[test]
    fn parser_borrows_byte_strings_and_skips_unused_root_values() {
        let bytes = b"d7:comment12:not retained4:infod6:lengthi1e4:name1:x6:pieces20:01234567890123456789e12:piece layersd4:hash20:abcdefghijklmnopqrstee";
        let limits = ResourceLimits::default();
        let (root, _) = Parser::new(bytes, &limits).parse_metainfo().unwrap();
        let root = root.as_dict("root").unwrap();
        assert_eq!(root.0.len(), 1);
        let info = root.get(b"info").unwrap().as_dict("info").unwrap();
        let BValue::Bytes(name) = info.get(b"name").unwrap() else {
            panic!("name must be borrowed bytes");
        };
        let input = bytes.as_ptr_range();
        assert!(input.contains(&name.as_ptr()));
    }

    proptest! {
        #[test]
        fn arbitrary_metainfo_bytes_never_panic(bytes in proptest::collection::vec(any::<u8>(), 0..4096)) {
            let _ = TorrentMetainfo::parse(&bytes);
        }
    }
}
