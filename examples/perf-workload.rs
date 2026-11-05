//! Disposable, network-free wall-clock workload for release comparisons.
use peerlume::{
    client::torrent_from_record,
    file_selection::FileSelectionModel,
    resume::ResumeCounters,
    settings::ClientSettings,
    state::{StateStore, StoredFile, TorrentRecord},
    torrent_filter::StatusFilter,
    torrent_view::{Sort, ViewCache},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{hint::black_box, io::Write, path::PathBuf, time::Instant};

fn sample(repetitions: usize, mut operation: impl FnMut(usize)) -> Value {
    let mut samples = Vec::with_capacity(repetitions);
    for iteration in 0..repetitions {
        let start = Instant::now();
        operation(iteration);
        samples.push(start.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64);
    }
    let total: u128 = samples.iter().map(|value| u128::from(*value)).sum();
    samples.sort_unstable();
    let percentile = |percent: usize| samples[((samples.len() - 1) * percent).div_ceil(100)];
    json!({
        "repetitions": repetitions,
        "total_ns": total.min(u128::from(u64::MAX)) as u64,
        "p50_ns": percentile(50), "p95_ns": percentile(95), "p99_ns": percentile(99),
    })
}

fn record(index: usize, files: usize) -> TorrentRecord {
    TorrentRecord {
        files_complete: true,
        resume: ResumeCounters::default(),
        transfer_settings: None,
        repair_policy: Default::default(),
        verify_after_download: false,
        download_mode: Default::default(),
        freeze_after_bytes: 0,
        upload_ratio: Default::default(),
        id: format!("v1: {index:040x}"),
        name: format!("Synthetic mixed CASE torrent {:04}", 1000 - index),
        source: "synthetic-never-opened.torrent".into(),
        info_hash_v1: Some(format!("{index:040x}")),
        info_hash_v2: None,
        is_private: true,
        state: "paused".into(),
        queue_position: index as i64,
        download_directory: None,
        trackers: vec![],
        files: (0..files)
            .map(|file| StoredFile {
                index: file as u32,
                path: format!("synthetic/file-{file:06}.bin"),
                size_bytes: 262_144,
                downloaded_bytes: 0,
                selected: true,
                priority: 0,
            })
            .collect(),
        verified_pieces: vec![],
        downloaded_bytes: 0,
        uploaded_bytes: 0,
        tracker_audit: None,
    }
}

fn compiled_inputs() -> Value {
    let inputs: [(&str, &[u8]); 5] = [
        ("Cargo.toml", include_bytes!("../Cargo.toml")),
        ("Cargo.lock", include_bytes!("../Cargo.lock")),
        (
            "src/torrent_view.rs",
            include_bytes!("../src/torrent_view.rs"),
        ),
        (
            "src/file_selection.rs",
            include_bytes!("../src/file_selection.rs"),
        ),
        ("src/state.rs", include_bytes!("../src/state.rs")),
    ];
    inputs
        .into_iter()
        .map(|(name, bytes)| {
            (
                name.to_owned(),
                json!(format!("{:x}", Sha256::digest(bytes))),
            )
        })
        .collect::<serde_json::Map<_, _>>()
        .into()
}

fn main() -> anyhow::Result<()> {
    let mut output = None;
    let mut label = "unspecified".to_owned();
    let mut repetitions = 200usize;
    let mut arguments = std::env::args().skip(1);
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--output" => {
                output =
                    Some(PathBuf::from(arguments.next().ok_or_else(|| {
                        anyhow::anyhow!("--output needs a new file path")
                    })?))
            }
            "--source-label" => {
                label = arguments
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("--source-label needs a value"))?
            }
            "--repetitions" => {
                repetitions = arguments
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("--repetitions needs a value"))?
                    .parse()?
            }
            "--help" => {
                println!(
                    "perf-workload [--output NEW_JSON_FILE] [--source-label LABEL] [--repetitions 1..10000]"
                );
                return Ok(());
            }
            _ => anyhow::bail!("Unknown argument: {argument}"),
        }
    }
    anyhow::ensure!(
        (1..=10_000).contains(&repetitions),
        "repetitions must be 1..10000"
    );
    let settings = ClientSettings::default();
    let mut cases = Vec::new();
    for count in [1, 10, 100] {
        let records: Vec<_> = (0..count).map(|index| record(index, 100)).collect();
        let torrents: Vec<_> = records
            .iter()
            .cloned()
            .map(|item| torrent_from_record(item, &settings))
            .collect();
        let mut view = settings.ui.clone();
        view.sort = Sort::Name;
        let mut cache = ViewCache::default();
        for _ in 0..3 {
            black_box(cache.indices(&torrents, StatusFilter::All, &view));
        }
        let stable_view = cache.indices(&torrents, StatusFilter::All, &view);
        anyhow::ensure!(
            stable_view.as_ref() == &(0..count).rev().collect::<Vec<_>>(),
            "name sort did not preserve synthetic torrent identities"
        );
        let view_result = sample(repetitions, |_| {
            black_box(cache.indices(&torrents, StatusFilter::All, &view));
        });
        anyhow::ensure!(
            std::sync::Arc::ptr_eq(
                &stable_view,
                &cache.indices(&torrents, StatusFilter::All, &view)
            ),
            "unchanged view was not retained"
        );
        anyhow::ensure!(cache.rebuild_count() == 1, "unchanged view was rebuilt");
        let directory = tempfile::tempdir()?;
        let database = directory.path().join("synthetic.sqlite3");
        let mut store = StateStore::open(&database)?;
        for item in &records {
            store.save_torrent(item)?;
        }
        let rows = |changed: u64| {
            records.iter().map(move |item| {
                (
                    item.id.clone(),
                    ResumeCounters {
                        uploaded: changed,
                        downloaded: changed * 2,
                        seeded_seconds: changed,
                    },
                    changed,
                )
            })
        };
        store.checkpoint_resumes(rows(0))?;
        let checkpoint_repetitions = repetitions.min(20);
        let checkpoint_unchanged = sample(checkpoint_repetitions, |_| {
            store
                .checkpoint_resumes(rows(0))
                .expect("synthetic unchanged checkpoint");
        });
        let checkpoint_changed = sample(checkpoint_repetitions, |iteration| {
            store
                .checkpoint_resumes(rows(iteration as u64 + 1))
                .expect("synthetic changed checkpoint");
        });
        let last = store
            .load_torrent(&records[0].id)?
            .expect("synthetic row exists");
        anyhow::ensure!(
            last.resume.uploaded == checkpoint_repetitions as u64,
            "checkpoint did not preserve the final synthetic counters"
        );
        cases.push(json!({ "torrents": count, "files_per_torrent": 100, "view_operation": "repeated_name_sort", "view_implementation": "retained_view_cache", "view_rebuilds": cache.rebuild_count(), "view": view_result, "checkpoint_unchanged": checkpoint_unchanged, "checkpoint_changed": checkpoint_changed }));
    }
    let large_model = sample(3, |_| {
        black_box(FileSelectionModel::from_files(
            (0..100_000).map(|index| (format!("synthetic/file-{index:06}.bin"), 262_144)),
        ));
    });
    let model = FileSelectionModel::from_files(
        (0..100_000).map(|index| (format!("synthetic/file-{index:06}.bin"), 262_144)),
    );
    black_box(model.view_rows("file-09", 1));
    let large_view = sample(repetitions, |_| {
        black_box(model.view_rows("file-09", 1));
    });
    let report = json!({
        "schema_version": 1, "workload": "synthetic_frontend_and_checkpoint", "completed": true,
        "source_label": label, "compiled_inputs_sha256": compiled_inputs(),
        "package_version": env!("CARGO_PKG_VERSION"), "debug_assertions": cfg!(debug_assertions),
        "os": std::env::consts::OS, "architecture": std::env::consts::ARCH,
        "warmup_view_operations": 3, "cases": cases,
        "large_model": { "files": 100_000, "construction": large_model, "cached_search": large_view },
        "resource_samples": { "cpu_percent": null, "rss_bytes": null, "allocations": null },
        "limitations": ["Wall-clock workload, not an allocation or sustained-transfer profiler", "No tracker, peer, session runtime or user profile is opened", "No disk throughput, cold-start or one-second resource peak claim", "Source label is caller supplied; compiled input digests identify the measured source inputs"],
    });
    if let Some(path) = output {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)?;
        serde_json::to_writer_pretty(&mut file, &report)?;
        file.write_all(b"\n")?;
    } else {
        println!("{}", serde_json::to_string_pretty(&report)?);
    }
    Ok(())
}
