//! Network-free validation/import and journal-contention comparison workload.
use peerlume::{
    announce_journal::{AnnounceJournal, AnnounceMetrics, AnnounceObservation, AnnounceReply},
    file_selection::FileSelectionModel,
    limits::{ResourceLimits, validate_relative_paths},
    metainfo::TorrentMetainfo,
};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    hint::black_box,
    io::{BufRead, Write},
    sync::{Arc, Barrier},
    time::Instant,
};

fn identity() -> serde_json::Value {
    let inputs: [(&str, &[u8]); 7] = [
        ("Cargo.toml", include_bytes!("../Cargo.toml")),
        ("Cargo.lock", include_bytes!("../Cargo.lock")),
        ("src/limits.rs", include_bytes!("../src/limits.rs")),
        (
            "src/announce_journal.rs",
            include_bytes!("../src/announce_journal.rs"),
        ),
        (
            "src/announce_journal/writer.rs",
            include_bytes!("../src/announce_journal/writer.rs"),
        ),
        (
            "src/file_selection.rs",
            include_bytes!("../src/file_selection.rs"),
        ),
        (
            "src/file_selection/deferred.rs",
            include_bytes!("../src/file_selection/deferred.rs"),
        ),
    ];
    json!({"debug_assertions":cfg!(debug_assertions),"os":std::env::consts::OS,"architecture":std::env::consts::ARCH,
        "compiled_inputs_sha256": inputs.into_iter().map(|(name,bytes)|(name.to_owned(),json!(format!("{:x}",Sha256::digest(bytes))))).collect::<serde_json::Map<_,_>>()})
}

fn bstring(output: &mut Vec<u8>, text: &[u8]) {
    output.extend_from_slice(text.len().to_string().as_bytes());
    output.push(b':');
    output.extend_from_slice(text);
}

fn metainfo(paths: &[String]) -> Vec<u8> {
    let mut output = b"d4:infod5:filesl".to_vec();
    for path in paths {
        output.extend_from_slice(b"d6:lengthi1e4:pathl");
        for component in path.split('/') {
            bstring(&mut output, component.as_bytes());
        }
        output.extend_from_slice(b"ee");
    }
    output.extend_from_slice(b"e4:name9:synthetic12:piece lengthi262144e6:pieces20:");
    output.extend_from_slice(&[0u8; 20]);
    output.extend_from_slice(b"ee");
    output
}

fn observation() -> AnnounceObservation {
    AnnounceObservation {
        started_unix_ms: 1,
        elapsed_ms: 2,
        info_hash: "0000000000000000000000000000000000000000".into(),
        peer_id_hex: "00".into(),
        tracker: "http://localhost:1".into(),
        transport: "http".into(),
        mode: "normal".into(),
        event: Some("started".into()),
        port: 42,
        actual: AnnounceMetrics::default(),
        verified_bytes: 0,
        full_size: 1,
        selected_bytes: Some(1),
        request_transmitted: Some(true),
        sent: AnnounceMetrics::default(),
        outcome: "ok".into(),
        reply: AnnounceReply {
            peers: vec!["127.0.0.1:45".parse().unwrap(); 512],
            ..Default::default()
        },
    }
}

fn memory_phase(phase: &str, model: &FileSelectionModel) -> anyhow::Result<()> {
    let usage = model.memory_usage();
    println!(
        "{}",
        json!({"workload":"cold_model_phase_0_3_0","phase":phase,"identity":identity(),"files":model.file_count(),"total_size":model.total_size(),"selected_count":model.selected_count(),"selected_size":model.selected_size(),"selected_downloaded":model.selected_downloaded(),"details_loaded":model.details_loaded(),"inline_model_bytes":std::mem::size_of::<FileSelectionModel>(),"accounted_buffers":{"file_slots_bytes":usage.file_slots_bytes,"path_capacity_bytes":usage.path_capacity_bytes,"tree_capacity_bytes":usage.tree_capacity_bytes,"view_capacity_bytes":usage.view_capacity_bytes,"compact_capacity_bytes":usage.compact_capacity_bytes,"priority_capacity_bytes":usage.priority_capacity_bytes,"total_bytes":usage.total_bytes()}})
    );
    std::io::stdout().flush()?;
    let mut acknowledgement = String::new();
    anyhow::ensure!(
        std::io::stdin().read_line(&mut acknowledgement)? > 0
            && acknowledgement.trim() == "continue",
        "memory phases require the dedicated sampler's acknowledgement"
    );
    Ok(())
}

fn main() -> anyhow::Result<()> {
    let mut files = 1000usize;
    let mut operation = "validate".to_owned();
    let mut arguments = std::env::args().skip(1);
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--files" => {
                files = arguments
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("missing file count"))?
                    .parse()?
            }
            "--operation" => {
                operation = arguments
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("missing operation"))?
            }
            "--describe" => {
                println!("{}", identity());
                return Ok(());
            }
            _ => anyhow::bail!("Unknown argument: {argument}"),
        }
    }
    anyhow::ensure!(
        [1000, 10_000, 100_000].contains(&files),
        "file count must be 1000,10000 or100000"
    );
    let mut result = json!({"schema_version":1,"workload":"optimization_workload_0_3_0","operation":operation,"files":files,"identity":identity(),"completed":true});
    match operation.as_str() {
        "validate" | "import" => {
            let paths: Vec<_> = (0..files)
                .map(|index| format!("group-{:02}/file-{index:06}.bin", index % 32))
                .collect();
            let encoded = if operation == "import" {
                metainfo(&paths)
            } else {
                Vec::new()
            };
            let started = Instant::now();
            if operation == "validate" {
                validate_relative_paths(
                    paths.iter().map(String::as_str),
                    &ResourceLimits::default(),
                )
                .map_err(anyhow::Error::msg)?;
            } else {
                let parsed =
                    TorrentMetainfo::parse_with_limits(&encoded, &ResourceLimits::default())
                        .map_err(anyhow::Error::msg)?;
                anyhow::ensure!(parsed.files.len() == files, "imported file count differs");
                black_box(parsed);
            }
            result["elapsed_ns"] =
                json!(started.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64);
            result["encoded_metainfo_bytes"] = json!(encoded.len());
        }
        "journal" => {
            let directory = tempfile::tempdir()?;
            let journal = AnnounceJournal::open(directory.path());
            let gate = Arc::new(Barrier::new(2));
            let (writer, writer_gate) = (journal.clone(), gate.clone());
            let worker = std::thread::spawn(move || {
                writer_gate.wait();
                let started = Instant::now();
                for _ in 0..128 {
                    writer.record(observation());
                }
                started.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64
            });
            gate.wait();
            let mut samples = Vec::new();
            for _ in 0..1000 {
                let started = Instant::now();
                black_box(journal.page(None, 0, 32));
                samples.push(started.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64);
                std::thread::yield_now();
            }
            result["record_128_elapsed_ns"] =
                json!(worker.join().expect("synthetic journal writer"));
            samples.sort_unstable();
            result["page_p50_ns"] = json!(samples[500]);
            result["page_p95_ns"] = json!(samples[950]);
            let export_started = Instant::now();
            let export = directory.path().join("full.jsonl");
            anyhow::ensure!(
                journal.export(&export, None).map_err(anyhow::Error::msg)? == 128,
                "export is incomplete"
            );
            result["export_elapsed_ns"] = json!(
                export_started
                    .elapsed()
                    .as_nanos()
                    .min(u128::from(u64::MAX)) as u64
            );
            let mut count = 0u64;
            for line in std::io::BufReader::new(std::fs::File::open(export)?).lines() {
                let entry: peerlume::announce_journal::JournalEntry = serde_json::from_str(&line?)?;
                count += 1;
                anyhow::ensure!(entry.sequence == count, "export sequence is not contiguous");
            }
            anyhow::ensure!(count == 128, "export count differs");
            result["exported_records"] = json!(count);
        }
        "memory" => {
            let mut model = FileSelectionModel::from_files((0..files).map(|index| {
                (
                    format!("group-{:02}/file-{index:06}.bin", index % 32),
                    262_144,
                )
            }));
            model.set_high_priority(&[0, files - 1]);
            black_box(model.view_rows("file-09", 1));
            memory_phase("loaded", &model)?;
            let summary = model.summary();
            anyhow::ensure!(model.defer_details(), "loaded model did not defer details");
            memory_phase("owner_cold", &model)?;
            let mirror = FileSelectionModel::from_summary(summary);
            drop(model);
            memory_phase("summary_mirror", &mirror)?;
            anyhow::ensure!(
                mirror.file_count() == files && mirror.selected_count() == files,
                "summary mirror lost authoritative totals"
            );
            result["completed_phases"] = json!(["loaded", "owner_cold", "summary_mirror"]);
        }
        _ => anyhow::bail!("Unknown operation"),
    }
    println!("{result}");
    Ok(())
}
