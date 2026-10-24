use std::{
    fs,
    io::{self, BufRead, BufReader, BufWriter, Read, Write},
    net::IpAddr,
    path::Path,
    time::Duration,
};

use flate2::read::GzDecoder;
use serde::{Deserialize, Serialize};

use crate::limits::ResourceLimits;
use crate::settings::data_directory;

const CACHE_MAGIC: &[u8; 8] = b"PLBL\x02\0\0\0";
#[cfg(test)]
mod reliability_tests;
const V1_CACHE_MAGIC: &[u8; 8] = b"PLBL\x01\0\0\0";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ipv4Range {
    start: u32,
    end: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ipv6Range {
    start: u128,
    end: u128,
}

enum IpRange {
    V4(Ipv4Range),
    V6(Ipv6Range),
}

#[derive(Debug, Default)]
pub struct Blocklist {
    ranges: Vec<Ipv4Range>,
    v6_ranges: Vec<Ipv6Range>,
    source_rules: usize,
}

#[derive(Debug)]
pub struct BlocklistUpdate {
    pub blocklist: Blocklist,
    pub downloaded_bytes: usize,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct BlocklistMetadata {
    source_url: String,
    etag: Option<String>,
    last_modified: Option<String>,
}

impl Blocklist {
    #[cfg(test)]
    pub fn parse(contents: &str) -> Self {
        Self::from_reader(std::io::Cursor::new(contents.as_bytes())).unwrap_or_default()
    }

    pub fn from_reader(reader: impl BufRead) -> io::Result<Self> {
        Self::from_reader_with_limits(reader, &ResourceLimits::default())
    }

    pub fn from_reader_with_limits(
        mut reader: impl BufRead,
        limits: &ResourceLimits,
    ) -> io::Result<Self> {
        let mut ranges = Vec::new();
        let mut v6_ranges = Vec::new();
        let mut source_rules = 0;
        let mut decompressed_bytes = 0_usize;
        let mut line = Vec::new();
        loop {
            line.clear();
            let count = reader.read_until(b'\n', &mut line)?;
            if count == 0 {
                break;
            }
            decompressed_bytes = decompressed_bytes.checked_add(count).ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "blocklist size overflow")
            })?;
            if decompressed_bytes > limits.max_blocklist_decompressed_bytes {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "blocklist exceeds decompressed-size limit",
                ));
            }
            if let Some(range) = parse_range_line(&line) {
                match range {
                    IpRange::V4(range) => ranges.push(range),
                    IpRange::V6(range) => v6_ranges.push(range),
                }
                source_rules += 1;
                if source_rules > limits.max_blocklist_rules {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "blocklist exceeds rule-count limit",
                    ));
                }
            }
        }
        ranges.sort_unstable_by_key(|range| range.start);
        merge_ranges_in_place(&mut ranges);
        ranges.shrink_to_fit();
        v6_ranges.sort_unstable_by_key(|range| range.start);
        merge_v6_ranges_in_place(&mut v6_ranges);
        v6_ranges.shrink_to_fit();
        Ok(Self {
            ranges,
            v6_ranges,
            source_rules,
        })
    }

    pub fn load_cache() -> io::Result<Self> {
        let binary = cache_path();
        match Self::read_binary_cache(&binary) {
            Ok(blocklist) => Ok(blocklist),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let legacy_binary = v1_cache_path();
                let blocklist = if legacy_binary.exists() {
                    Self::read_v1_binary_cache(&legacy_binary)?
                } else {
                    Self::from_reader(BufReader::new(fs::File::open(legacy_cache_path())?))?
                };
                blocklist.write_binary_cache(&binary)?;
                remove_if_exists(&legacy_binary)?;
                remove_if_exists(&legacy_cache_path())?;
                Ok(blocklist)
            }
            Err(error) => Err(error),
        }
    }

    pub fn rule_count(&self) -> usize {
        self.source_rules
    }

    pub fn range_count(&self) -> usize {
        self.ranges.len() + self.v6_ranges.len()
    }

    pub fn is_blocked(&self, ip: IpAddr) -> bool {
        match ip {
            IpAddr::V4(ip) => contains_range(&self.ranges, u32::from(ip)),
            IpAddr::V6(ip) => contains_v6_range(&self.v6_ranges, u128::from(ip)),
        }
    }

    fn write_binary_cache(&self, path: &Path) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let temporary = path.with_extension("tmp");
        {
            let mut writer = BufWriter::new(fs::File::create(&temporary)?);
            let mut checksum = crc32fast::Hasher::new();
            write_checksummed(&mut writer, &mut checksum, CACHE_MAGIC)?;
            write_checksummed(
                &mut writer,
                &mut checksum,
                &(self.source_rules as u64).to_le_bytes(),
            )?;
            write_checksummed(
                &mut writer,
                &mut checksum,
                &(self.ranges.len() as u64).to_le_bytes(),
            )?;
            write_checksummed(
                &mut writer,
                &mut checksum,
                &(self.v6_ranges.len() as u64).to_le_bytes(),
            )?;
            for range in &self.ranges {
                write_checksummed(&mut writer, &mut checksum, &range.start.to_le_bytes())?;
                write_checksummed(&mut writer, &mut checksum, &range.end.to_le_bytes())?;
            }
            for range in &self.v6_ranges {
                write_checksummed(&mut writer, &mut checksum, &range.start.to_le_bytes())?;
                write_checksummed(&mut writer, &mut checksum, &range.end.to_le_bytes())?;
            }
            writer.write_all(&checksum.finalize().to_le_bytes())?;
            writer.flush()?;
        }
        if path.exists() {
            fs::remove_file(path)?;
        }
        fs::rename(temporary, path)
    }

    fn read_binary_cache(path: &Path) -> io::Result<Self> {
        let mut reader = BufReader::new(fs::File::open(path)?);
        let mut checksum = crc32fast::Hasher::new();
        let mut magic = [0; 8];
        read_checksummed(&mut reader, &mut checksum, &mut magic)?;
        if &magic != CACHE_MAGIC {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unsupported blocklist cache format",
            ));
        }
        let source_rules = read_u64_checksummed(&mut reader, &mut checksum)?;
        let range_count = read_u64_checksummed(&mut reader, &mut checksum)?;
        let v6_range_count = read_u64_checksummed(&mut reader, &mut checksum)?;
        let range_count: usize = range_count
            .try_into()
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "range count is too large"))?;
        let v6_range_count: usize = v6_range_count.try_into().map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidData, "IPv6 range count is too large")
        })?;
        let expected_bytes = range_count
            .checked_mul(8)
            .and_then(|v4| {
                v6_range_count
                    .checked_mul(32)
                    .and_then(|v6| v4.checked_add(v6))
            })
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "range data is too large"))?;
        let file_bytes = reader.get_ref().metadata()?.len();
        if file_bytes != (CACHE_MAGIC.len() + 24 + expected_bytes + 4) as u64 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "blocklist cache length does not match its header",
            ));
        }
        let mut ranges = Vec::with_capacity(range_count);
        for _ in 0..range_count {
            let start = read_u32_checksummed(&mut reader, &mut checksum)?;
            let end = read_u32_checksummed(&mut reader, &mut checksum)?;
            if start > end
                || ranges
                    .last()
                    .is_some_and(|last: &Ipv4Range| last.end >= start)
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "blocklist cache contains invalid or overlapping ranges",
                ));
            }
            ranges.push(Ipv4Range { start, end });
        }
        let mut v6_ranges = Vec::with_capacity(v6_range_count);
        for _ in 0..v6_range_count {
            let start = read_u128_checksummed(&mut reader, &mut checksum)?;
            let end = read_u128_checksummed(&mut reader, &mut checksum)?;
            if start > end
                || v6_ranges
                    .last()
                    .is_some_and(|last: &Ipv6Range| last.end >= start)
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "blocklist cache contains invalid IPv6 ranges",
                ));
            }
            v6_ranges.push(Ipv6Range { start, end });
        }
        let expected_checksum = read_u32(&mut reader)?;
        if checksum.finalize() != expected_checksum {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "blocklist cache checksum mismatch",
            ));
        }
        Ok(Self {
            ranges,
            v6_ranges,
            source_rules: source_rules.try_into().map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidData, "rule count is too large")
            })?,
        })
    }

    fn read_v1_binary_cache(path: &Path) -> io::Result<Self> {
        let mut reader = BufReader::new(fs::File::open(path)?);
        let mut magic = [0; 8];
        reader.read_exact(&mut magic)?;
        if &magic != V1_CACHE_MAGIC {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unsupported legacy cache",
            ));
        }
        let source_rules = read_u64(&mut reader)?;
        let count: usize = read_u64(&mut reader)?.try_into().map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidData, "legacy range count too large")
        })?;
        let expected = count
            .checked_mul(8)
            .and_then(|n| n.checked_add(V1_CACHE_MAGIC.len() + 16))
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "legacy range data is too large")
            })?;
        if count > ResourceLimits::default().max_blocklist_rules
            || reader.get_ref().metadata()?.len() != expected as u64
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "legacy cache length or range count is invalid",
            ));
        }
        let mut ranges = Vec::with_capacity(count);
        for _ in 0..count {
            let start = read_u32(&mut reader)?;
            let end = read_u32(&mut reader)?;
            if start > end
                || ranges
                    .last()
                    .is_some_and(|last: &Ipv4Range| last.end >= start)
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "invalid legacy cache ranges",
                ));
            }
            ranges.push(Ipv4Range { start, end });
        }
        Ok(Self {
            ranges,
            v6_ranges: Vec::new(),
            source_rules: source_rules.try_into().map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidData, "legacy rule count too large")
            })?,
        })
    }
}

pub fn download_and_cache(url: &str) -> Result<BlocklistUpdate, String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(120))
        .user_agent("PeerLume/0.1 blocklist updater")
        .build()
        .map_err(|error| error.to_string())?;
    let previous = load_metadata().unwrap_or_default();
    let mut request = client.get(url);
    if previous.source_url == url {
        if let Some(etag) = &previous.etag {
            request = request.header(reqwest::header::IF_NONE_MATCH, etag);
        }
        if let Some(last_modified) = &previous.last_modified {
            request = request.header(reqwest::header::IF_MODIFIED_SINCE, last_modified);
        }
    }
    let response = request
        .send()
        .map_err(|error| format!("download failed: {error}"))?;
    if response.status() == reqwest::StatusCode::NOT_MODIFIED {
        remove_if_exists(&v1_cache_path()).map_err(|error| error.to_string())?;
        remove_if_exists(&legacy_cache_path()).map_err(|error| error.to_string())?;
        return Ok(BlocklistUpdate {
            blocklist: Blocklist::load_cache().map_err(|error| error.to_string())?,
            downloaded_bytes: 0,
        });
    }
    let response = response
        .error_for_status()
        .map_err(|error| format!("download failed: {error}"))?;
    let metadata = BlocklistMetadata {
        source_url: url.to_owned(),
        etag: response
            .headers()
            .get(reqwest::header::ETAG)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned),
        last_modified: response
            .headers()
            .get(reqwest::header::LAST_MODIFIED)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned),
    };

    let (blocklist, downloaded_bytes) = decode_gzip_blocklist(response, &ResourceLimits::default())
        .map_err(|error| format!("gzip decode or parse failed: {error}"))?;
    if blocklist.rule_count() == 0 {
        return Err("downloaded blocklist contains no valid IPv4 ranges".into());
    }
    blocklist
        .write_binary_cache(&cache_path())
        .map_err(|error| error.to_string())?;
    remove_if_exists(&legacy_cache_path()).map_err(|error| error.to_string())?;
    remove_if_exists(&v1_cache_path()).map_err(|error| error.to_string())?;
    save_metadata(&metadata).map_err(|error| error.to_string())?;

    Ok(BlocklistUpdate {
        blocklist,
        downloaded_bytes,
    })
}

fn decode_gzip_blocklist(
    reader: impl Read,
    limits: &ResourceLimits,
) -> io::Result<(Blocklist, usize)> {
    let limited = LimitedReader::new(reader, limits.max_blocklist_compressed_bytes);
    let mut compressed = CountingReader::new(limited);
    let blocklist = {
        let decoder = GzDecoder::new(&mut compressed);
        Blocklist::from_reader_with_limits(BufReader::new(decoder), limits)?
    };
    Ok((blocklist, compressed.bytes_read))
}

fn parse_range_line(line: &[u8]) -> Option<IpRange> {
    let line = std::str::from_utf8(line)
        .ok()?
        .trim()
        .trim_end_matches('\r');
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let (start, end) = std::iter::once(0)
        .chain(line.match_indices(':').map(|(index, _)| index + 1))
        .find_map(|offset| {
            let (start, end) = line[offset..].split_once('-')?;
            Some((
                start.trim().parse::<IpAddr>().ok()?,
                end.trim().parse::<IpAddr>().ok()?,
            ))
        })?;
    match (start, end) {
        (IpAddr::V4(start), IpAddr::V4(end)) => {
            let (start, end) = (u32::from(start), u32::from(end));
            (start <= end).then_some(IpRange::V4(Ipv4Range { start, end }))
        }
        (IpAddr::V6(start), IpAddr::V6(end)) => {
            let (start, end) = (u128::from(start), u128::from(end));
            (start <= end).then_some(IpRange::V6(Ipv6Range { start, end }))
        }
        _ => None,
    }
}

fn merge_ranges_in_place(ranges: &mut Vec<Ipv4Range>) {
    let mut write = 0;
    for read in 0..ranges.len() {
        let range = ranges[read];
        if write > 0 && range.start <= ranges[write - 1].end.saturating_add(1) {
            ranges[write - 1].end = ranges[write - 1].end.max(range.end);
        } else {
            ranges[write] = range;
            write += 1;
        }
    }
    ranges.truncate(write);
}

fn merge_v6_ranges_in_place(ranges: &mut Vec<Ipv6Range>) {
    let mut write = 0;
    for read in 0..ranges.len() {
        let range = ranges[read];
        if write > 0 && range.start <= ranges[write - 1].end.saturating_add(1) {
            ranges[write - 1].end = ranges[write - 1].end.max(range.end);
        } else {
            ranges[write] = range;
            write += 1;
        }
    }
    ranges.truncate(write);
}

fn contains_range(ranges: &[Ipv4Range], ip: u32) -> bool {
    ranges
        .binary_search_by(|range| {
            if ip < range.start {
                std::cmp::Ordering::Greater
            } else if ip > range.end {
                std::cmp::Ordering::Less
            } else {
                std::cmp::Ordering::Equal
            }
        })
        .is_ok()
}

fn contains_v6_range(ranges: &[Ipv6Range], ip: u128) -> bool {
    ranges
        .binary_search_by(|range| {
            if ip < range.start {
                std::cmp::Ordering::Greater
            } else if ip > range.end {
                std::cmp::Ordering::Less
            } else {
                std::cmp::Ordering::Equal
            }
        })
        .is_ok()
}

struct CountingReader<R> {
    inner: R,
    bytes_read: usize,
}

struct LimitedReader<R> {
    inner: R,
    remaining: usize,
}

impl<R> LimitedReader<R> {
    fn new(inner: R, limit: usize) -> Self {
        Self {
            inner,
            remaining: limit,
        }
    }
}

impl<R: Read> Read for LimitedReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if self.remaining == 0 {
            let mut probe = [0_u8; 1];
            return match self.inner.read(&mut probe)? {
                0 => Ok(0),
                _ => Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "compressed blocklist exceeds configured limit",
                )),
            };
        }
        let limit = buffer.len().min(self.remaining);
        let count = self.inner.read(&mut buffer[..limit])?;
        self.remaining -= count;
        Ok(count)
    }
}

impl<R> CountingReader<R> {
    fn new(inner: R) -> Self {
        Self {
            inner,
            bytes_read: 0,
        }
    }
}

impl<R: Read> Read for CountingReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let count = self.inner.read(buffer)?;
        self.bytes_read = self.bytes_read.saturating_add(count);
        Ok(count)
    }
}

fn read_u64(reader: &mut impl Read) -> io::Result<u64> {
    let mut bytes = [0; 8];
    reader.read_exact(&mut bytes)?;
    Ok(u64::from_le_bytes(bytes))
}

fn read_u32(reader: &mut impl Read) -> io::Result<u32> {
    let mut bytes = [0; 4];
    reader.read_exact(&mut bytes)?;
    Ok(u32::from_le_bytes(bytes))
}

fn write_checksummed(
    writer: &mut impl Write,
    checksum: &mut crc32fast::Hasher,
    bytes: &[u8],
) -> io::Result<()> {
    writer.write_all(bytes)?;
    checksum.update(bytes);
    Ok(())
}

fn read_checksummed(
    reader: &mut impl Read,
    checksum: &mut crc32fast::Hasher,
    bytes: &mut [u8],
) -> io::Result<()> {
    reader.read_exact(bytes)?;
    checksum.update(bytes);
    Ok(())
}

fn read_u64_checksummed(
    reader: &mut impl Read,
    checksum: &mut crc32fast::Hasher,
) -> io::Result<u64> {
    let mut bytes = [0; 8];
    read_checksummed(reader, checksum, &mut bytes)?;
    Ok(u64::from_le_bytes(bytes))
}

fn read_u32_checksummed(
    reader: &mut impl Read,
    checksum: &mut crc32fast::Hasher,
) -> io::Result<u32> {
    let mut bytes = [0; 4];
    read_checksummed(reader, checksum, &mut bytes)?;
    Ok(u32::from_le_bytes(bytes))
}

fn read_u128_checksummed(
    reader: &mut impl Read,
    checksum: &mut crc32fast::Hasher,
) -> io::Result<u128> {
    let mut bytes = [0; 16];
    read_checksummed(reader, checksum, &mut bytes)?;
    Ok(u128::from_le_bytes(bytes))
}

fn remove_if_exists(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn metadata_path() -> std::path::PathBuf {
    data_directory().join("blocklist-metadata.toml")
}

fn load_metadata() -> io::Result<BlocklistMetadata> {
    let text = fs::read_to_string(metadata_path())?;
    toml::from_str(&text).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

fn save_metadata(metadata: &BlocklistMetadata) -> io::Result<()> {
    let path = metadata_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension("tmp");
    let text = toml::to_string(metadata).map_err(io::Error::other)?;
    fs::write(&temporary, text)?;
    if path.exists() {
        fs::remove_file(&path)?;
    }
    fs::rename(temporary, path)
}

fn cache_path() -> std::path::PathBuf {
    data_directory().join("blocklist-v2.bin")
}

fn v1_cache_path() -> std::path::PathBuf {
    data_directory().join("blocklist-v1.bin")
}

fn legacy_cache_path() -> std::path::PathBuf {
    data_directory().join("blocklist.txt")
}

#[cfg(test)]
mod tests {
    use std::{
        io::{BufReader, Cursor, Write},
        net::{IpAddr, Ipv4Addr},
        time::{SystemTime, UNIX_EPOCH},
    };

    use flate2::{Compression, write::GzEncoder};
    use proptest::prelude::*;

    use super::{Blocklist, CACHE_MAGIC, decode_gzip_blocklist};
    use crate::limits::ResourceLimits;

    #[test]
    fn streaming_parser_merges_ranges_and_releases_extra_capacity() {
        let input = Cursor::new(b"company:division:10.0.0.1-10.0.0.4\nother:10.0.0.5-10.0.0.8\r\n");
        let blocklist = Blocklist::from_reader(BufReader::new(input)).unwrap();
        assert_eq!(blocklist.rule_count(), 2);
        assert_eq!(blocklist.range_count(), 1);
        assert_eq!(blocklist.ranges.capacity(), blocklist.ranges.len());
        assert!(blocklist.is_blocked(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 7))));
        assert!(!blocklist.is_blocked(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 9))));
    }

    #[test]
    fn parses_and_merges_ipv6_ranges() {
        let input = Cursor::new(b"v6:2001:db8::-2001:db8::ff\n2001:db8::100-2001:db8::1ff\n");
        let blocklist = Blocklist::from_reader(BufReader::new(input)).unwrap();
        assert_eq!(blocklist.rule_count(), 2);
        assert_eq!(blocklist.range_count(), 1);
        assert!(blocklist.is_blocked("2001:db8::150".parse().unwrap()));
        assert!(!blocklist.is_blocked("2001:db8::300".parse().unwrap()));
    }

    #[test]
    fn binary_cache_round_trip_preserves_compact_ranges() {
        let blocklist =
            Blocklist::parse("first:10.0.0.1-10.0.0.2\nsecond:192.168.1.1-192.168.1.4\n");
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("peerlume-blocklist-{nonce}.bin"));
        blocklist.write_binary_cache(&path).unwrap();
        let loaded = Blocklist::read_binary_cache(&path).unwrap();
        std::fs::remove_file(path).unwrap();
        assert_eq!(loaded.source_rules, blocklist.source_rules);
        assert_eq!(loaded.ranges, blocklist.ranges);
    }

    #[test]
    fn binary_cache_rejects_truncated_data() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("peerlume-blocklist-bad-{nonce}.bin"));
        std::fs::write(&path, CACHE_MAGIC).unwrap();
        let error = Blocklist::read_binary_cache(&path).unwrap_err();
        std::fs::remove_file(path).unwrap();
        assert_eq!(error.kind(), std::io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn binary_cache_rejects_checksum_corruption() {
        let blocklist = Blocklist::parse("one:10.0.0.1-10.0.0.2\n");
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("peerlume-blocklist-crc-{nonce}.bin"));
        blocklist.write_binary_cache(&path).unwrap();
        let mut bytes = std::fs::read(&path).unwrap();
        let corrupt_index = bytes.len() - 5;
        bytes[corrupt_index] ^= 0x80;
        std::fs::write(&path, bytes).unwrap();
        let error = Blocklist::read_binary_cache(&path).unwrap_err();
        std::fs::remove_file(path).unwrap();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    }

    #[test]
    fn gzip_input_is_decoded_as_a_stream_and_counted() {
        let source = b"one:10.0.0.1-10.0.0.2\ntwo:10.0.0.3-10.0.0.4\n";
        let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
        encoder.write_all(source).unwrap();
        let compressed = encoder.finish().unwrap();
        let limits = ResourceLimits::default();
        let (blocklist, bytes_read) =
            decode_gzip_blocklist(Cursor::new(&compressed), &limits).unwrap();
        assert_eq!(bytes_read, compressed.len());
        assert_eq!(blocklist.rule_count(), 2);
        assert_eq!(blocklist.range_count(), 1);
    }

    #[test]
    fn streaming_parser_enforces_decompressed_and_rule_limits() {
        let input = b"a:10.0.0.1-10.0.0.2\nb:10.0.0.3-10.0.0.4\n";
        let mut limits = ResourceLimits {
            max_blocklist_rules: 1,
            ..ResourceLimits::default()
        };
        assert!(Blocklist::from_reader_with_limits(Cursor::new(input), &limits).is_err());
        limits.max_blocklist_rules = 10;
        limits.max_blocklist_decompressed_bytes = 4;
        assert!(Blocklist::from_reader_with_limits(Cursor::new(input), &limits).is_err());
    }

    proptest! {
        #[test]
        fn arbitrary_blocklist_text_never_panics(bytes in proptest::collection::vec(any::<u8>(), 0..8192)) {
            let limits = ResourceLimits::default();
            let _ = Blocklist::from_reader_with_limits(Cursor::new(bytes), &limits);
        }
    }
}
