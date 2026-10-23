//! Streaming redacted disk exports, outside the controller/UI lock.
use super::{agent::safe_announce, operations::now_ms, wire::ErrorCode};
use std::{
    fs::File,
    io::{BufRead, BufReader, BufWriter, Read, Write},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
};
pub(crate) struct ExportJob {
    pub receiver: mpsc::Receiver<Result<(), ErrorCode>>,
    pub entries: Arc<AtomicU64>,
}
pub(crate) fn start(
    journal: &crate::announce_journal::AnnounceJournal,
    hash: String,
    output: File,
    deadline: u64,
) -> Result<ExportJob, ErrorCode> {
    let (input, length) = journal
        .disk_snapshot()
        .map_err(|_| ErrorCode::PersistenceUnavailable)?;
    if length > 512 * 1024 * 1024 {
        return Err(ErrorCode::TooLarge);
    }
    let (sender, receiver) = mpsc::sync_channel(1);
    let entries = Arc::new(AtomicU64::new(0));
    let count = entries.clone();
    std::thread::Builder::new()
        .name("peerlume-agent-export".into())
        .spawn(move || {
            let result = stream(
                input,
                output,
                length,
                &hash,
                deadline.min(now_ms() + 300_000),
                &count,
            );
            let _ = sender.send(result);
        })
        .map_err(|_| ErrorCode::Busy)?;
    Ok(ExportJob { receiver, entries })
}
fn stream(
    input: File,
    output: File,
    length: u64,
    hash: &str,
    deadline: u64,
    count: &AtomicU64,
) -> Result<(), ErrorCode> {
    let mut input = BufReader::new(input.take(length));
    let mut output = BufWriter::new(output);
    loop {
        if now_ms() >= deadline {
            return Err(ErrorCode::DeadlineExceeded);
        }
        let mut line = Vec::new();
        let n = input
            .by_ref()
            .take(crate::announce_journal::LIVE_BYTE_BUDGET as u64 + 1)
            .read_until(b'\n', &mut line)
            .map_err(|_| ErrorCode::PersistenceUnavailable)?;
        if n == 0 {
            break;
        }
        if n > crate::announce_journal::LIVE_BYTE_BUDGET {
            return Err(ErrorCode::TooLarge);
        }
        let entry: crate::announce_journal::JournalEntry =
            serde_json::from_slice(&line).map_err(|_| ErrorCode::PersistenceUnavailable)?;
        if entry.announce.info_hash != hash {
            continue;
        }
        serde_json::to_writer(&mut output, &safe_announce(&entry))
            .map_err(|_| ErrorCode::PersistenceUnavailable)?;
        output
            .write_all(b"\n")
            .map_err(|_| ErrorCode::PersistenceUnavailable)?;
        count.fetch_add(1, Ordering::Relaxed);
    }
    output
        .flush()
        .map_err(|_| ErrorCode::PersistenceUnavailable)?;
    output
        .get_ref()
        .sync_all()
        .map_err(|_| ErrorCode::PersistenceUnavailable)
}

#[cfg(test)]
mod tests;
