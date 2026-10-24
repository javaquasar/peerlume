//! Opt-in, local-only workload. Run alone so resource samples describe this test.
use super::*;
use std::sync::atomic::{AtomicU64, Ordering};
use std::{
    io::Write,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

pub(crate) type Report = Arc<Mutex<std::fs::File>>;
pub(crate) fn record(report: &Report, value: serde_json::Value) {
    let mut file = report.lock().unwrap_or_else(|e| e.into_inner());
    assert!(
        file.metadata().unwrap().len() < 128 * 1024 * 1024,
        "resource evidence exceeded its 128 MiB budget"
    );
    writeln!(file, "{value}").unwrap();
    file.flush().unwrap();
}
pub(crate) struct Sampler {
    stop: std::sync::mpsc::Sender<()>,
    worker: Option<std::thread::JoinHandle<()>>,
    peaks: Arc<[AtomicU64; 3]>,
    pub(crate) cycle: Arc<AtomicU64>,
    pub(crate) latency: Arc<AtomicU64>,
}
impl Sampler {
    pub(crate) fn start(report: Report, profile: std::path::PathBuf, start: Instant) -> Self {
        Self::with_activity(report, profile, start, || serde_json::Value::Null)
    }
    pub(crate) fn with_activity(
        report: Report,
        profile: std::path::PathBuf,
        start: Instant,
        activity: impl Fn() -> serde_json::Value + Send + 'static,
    ) -> Self {
        let (stop, rx) = std::sync::mpsc::channel();
        let peaks = Arc::new(std::array::from_fn(|_| AtomicU64::new(0)));
        let cycle = Arc::new(AtomicU64::new(0));
        let latency = Arc::new(AtomicU64::new(0));
        let observed = peaks.clone();
        let observed_cycle = cycle.clone();
        let observed_latency = latency.clone();
        let worker=std::thread::Builder::new().name("soak-sampler".into()).spawn(move||{
            loop {
                let sample=resources();
                for (index,value) in [sample.0,sample.1,sample.2].into_iter().enumerate(){observed[index].fetch_max(value,Ordering::Relaxed);}
                let (disk,database,journal)=disk_sizes(&profile);
                record(&report,serde_json::json!({"kind":"periodic","elapsed_ms":start.elapsed().as_millis(),"cycle":observed_cycle.load(Ordering::Relaxed),"rss_bytes":sample.0,"threads":sample.1,"open_handles":sample.2,"profile_bytes":disk,"database_bytes":database,"journal_bytes":journal,"ipc_latency_us":observed_latency.load(Ordering::Relaxed),"activity":activity()}));
                match rx.recv_timeout(Duration::from_secs(1)){Err(std::sync::mpsc::RecvTimeoutError::Timeout)=>{},_=>break}
            }
        }).unwrap();
        Self {
            stop,
            worker: Some(worker),
            peaks,
            cycle,
            latency,
        }
    }
    pub(crate) fn peaks(&self) -> [u64; 3] {
        std::array::from_fn(|index| self.peaks[index].load(Ordering::Relaxed))
    }
}
impl Drop for Sampler {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        if let Some(worker) = self.worker.take() {
            let result = worker.join();
            if !std::thread::panicking() {
                assert!(result.is_ok(), "resource sampler failed");
            }
        }
    }
}

#[test]
#[ignore = "resource workload; run with --ignored --test-threads=1"]
fn reliability_resource_soak() {
    let seconds = std::env::var("PEERLUME_SOAK_SECONDS")
        .unwrap_or_else(|_| "90".into())
        .parse::<u64>()
        .unwrap();
    assert!((10..=86400).contains(&seconds));
    let report = std::env::var_os("PEERLUME_SOAK_REPORT")
        .expect("Set PEERLUME_SOAK_REPORT to a new JSONL file");
    let report = Arc::new(Mutex::new(
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(report)
            .unwrap(),
    ));
    let mut fixture = test_support::LiveFixture::new();
    fixture
        .controller
        .configure_agents(fixture.root.path(), fixture.root.path().into())
        .unwrap();
    let service = fixture.controller.agent_access().unwrap();
    service.enabled = true;
    let token = service.grant(access::Grants::default()).unwrap();
    let profile = fixture.root.path().to_path_buf();
    let owner = Arc::new(Mutex::new(fixture));
    let _instance = host::InstanceLock::acquire(&profile).unwrap();
    let host = host::Host::start(owner.clone(), |f| &mut f.controller, profile.clone()).unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let start = Instant::now();
    let sampler = Sampler::start(report.clone(), profile.clone(), start);
    let mut cycle = 0u64;
    let mut baseline = None;
    let mut peak_baseline = None;
    while start.elapsed() < Duration::from_secs(seconds) {
        let latency = Instant::now();
        let request = wire::Request {
            protocol_version: PROTOCOL_VERSION,
            request_id: format!("sample-{cycle}"),
            token: token.clone(),
            instance_id: None,
            method: wire::Method::List {
                offset: 0,
                limit: 1,
                revision: None,
                after_revision: None,
            },
        };
        let reply = loop {
            match runtime.block_on(host::call(&profile, &request)) {
                Ok(reply) => break reply,
                Err(_) if latency.elapsed() < Duration::from_secs(5) => {
                    std::thread::sleep(Duration::from_millis(20))
                }
                Err(error) => panic!("IPC failed: {error:?}"),
            }
        };
        assert!(reply.error.is_none());
        assert!(latency.elapsed() < Duration::from_secs(5));
        sampler
            .latency
            .store(latency.elapsed().as_micros() as u64, Ordering::Relaxed);
        assert!(
            !sampler.worker.as_ref().unwrap().is_finished(),
            "resource sampler stopped unexpectedly"
        );
        let sample = resources();
        let disk = std::fs::read_dir(&profile)
            .unwrap()
            .filter_map(Result::ok)
            .filter_map(|e| e.metadata().ok())
            .filter(|m| m.is_file())
            .map(|m| m.len())
            .sum::<u64>();
        record(
            &report,
            serde_json::json!({"kind":"idle","elapsed_ms":start.elapsed().as_millis(),"cycle":cycle,"rss_bytes":sample.0,"threads":sample.1,"open_handles":sample.2,"profile_bytes":disk,"ipc_latency_us":latency.elapsed().as_micros()}),
        );
        if cycle == 2 {
            baseline = Some(sample);
            peak_baseline = Some(sampler.peaks());
        }
        if let Some(base) = peak_baseline {
            let peaks = sampler.peaks();
            for (index, budget) in [192 * 1024 * 1024, 32, 64].into_iter().enumerate() {
                assert!(
                    peaks[index] <= base[index] + budget,
                    "sampled resource peak grew beyond budget"
                );
            }
        }
        if let Some(base) = baseline {
            assert!(
                sample.0 <= base.0 + 192 * 1024 * 1024,
                "RSS grew beyond the 192 MiB budget"
            );
            assert!(sample.1 <= base.1 + 32, "thread count grew beyond budget");
            assert!(sample.2 <= base.2 + 64, "handle count grew beyond budget");
        }
        // Keep this owner alive throughout the run while repeatedly exercising
        // local parallel transfers and corruption repair in isolated sessions.
        crate::session::reliability_transfer_cycle();
        cycle += 1;
        sampler.cycle.store(cycle, Ordering::Relaxed);
    }
    assert!(cycle > 0);
    assert!(host.error.lock().unwrap().is_none());
    let peaks = sampler.peaks();
    drop(sampler);
    record(
        &report,
        serde_json::json!({"kind":"result","completed":true,"requested_seconds":seconds,"elapsed_ms":start.elapsed().as_millis(),"cycles":cycle,"sampled_peaks":peaks,"continuous_owner":true}),
    );
    drop(host);
    drop(owner);
}

#[cfg(target_os = "linux")]
pub(crate) fn resources() -> (u64, u64, u64) {
    let status = std::fs::read_to_string("/proc/self/status").unwrap();
    let value = |name: &str| -> u64 {
        status
            .lines()
            .find_map(|line| line.strip_prefix(name))
            .unwrap()
            .split_whitespace()
            .next()
            .unwrap()
            .parse()
            .unwrap()
    };
    (
        value("VmRSS:") * 1024,
        value("Threads:"),
        std::fs::read_dir("/proc/self/fd").unwrap().count() as u64,
    )
}
#[cfg(not(target_os = "linux"))]
pub(crate) fn resources() -> (u64, u64, u64) {
    panic!("Resource soak measurement is supported on Linux only")
}

pub(crate) fn disk_sizes(profile: &std::path::Path) -> (u64, u64, u64) {
    let mut total = 0;
    let mut database = 0;
    for entry in std::fs::read_dir(profile).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_file() {
            let length = entry.metadata().unwrap().len();
            total += length;
            if entry.file_name().to_string_lossy().contains(".sqlite3") {
                database += length;
            }
        }
    }
    let journal = std::fs::read_dir(profile.join("announce-logs"))
        .map(|entries| {
            entries
                .map(|e| e.unwrap())
                .filter(|e| e.file_type().unwrap().is_file())
                .map(|e| e.metadata().unwrap().len())
                .sum()
        })
        .unwrap_or(0);
    (total, database, journal)
}

#[test]
fn disk_samples_separate_database_journal_and_other_profile_files() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("state.sqlite3"), [0; 7]).unwrap();
    std::fs::write(root.path().join("state.sqlite3-wal"), [0; 3]).unwrap();
    std::fs::write(root.path().join("settings.toml"), [0; 2]).unwrap();
    std::fs::create_dir(root.path().join("announce-logs")).unwrap();
    std::fs::write(root.path().join("announce-logs/one.jsonl"), [0; 11]).unwrap();
    assert_eq!(disk_sizes(root.path()), (12, 10, 11));
}
