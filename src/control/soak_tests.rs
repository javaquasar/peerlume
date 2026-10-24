//! Opt-in, local-only workload. Run alone so resource samples describe this test.
use super::*;
use std::{
    io::Write,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

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
    let mut report = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(report)
        .unwrap();
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
    let mut cycle = 0u64;
    let mut baseline = None;
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
        let sample = resources();
        let disk = std::fs::read_dir(&profile)
            .unwrap()
            .filter_map(Result::ok)
            .filter_map(|e| e.metadata().ok())
            .filter(|m| m.is_file())
            .map(|m| m.len())
            .sum::<u64>();
        let record = serde_json::json!({"elapsed_ms":start.elapsed().as_millis(),"cycle":cycle,"rss_bytes":sample.0,"threads":sample.1,"open_handles":sample.2,"profile_bytes":disk,"ipc_latency_us":latency.elapsed().as_micros()});
        writeln!(report, "{record}").unwrap();
        report.flush().unwrap();
        // Compare idle boundaries after warmup, not transfer-time allocation peaks.
        if cycle == 2 {
            baseline = Some(sample);
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
    }
    assert!(cycle > 0);
    assert!(host.error.lock().unwrap().is_none());
    drop(host);
    drop(owner);
}

#[cfg(target_os = "linux")]
fn resources() -> (u64, u64, u64) {
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
fn resources() -> (u64, u64, u64) {
    panic!("Resource soak measurement is supported on Linux only")
}
