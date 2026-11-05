//! Standalone audit tasks outlive their display receivers, but frontend exit is
//! bounded and reports unfinished work before process termination.
use std::{
    io,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

const MAX_WORKERS: usize = 64;
struct Task {
    cancel: Arc<AtomicBool>,
    done: AtomicBool,
    error: Mutex<Option<String>>,
}
#[derive(Default)]
struct Registry {
    closed: bool,
    tasks: Vec<Arc<Task>>,
}
static TASKS: OnceLock<Mutex<Registry>> = OnceLock::new();

pub(crate) fn spawn(
    name: &str,
    cancel: Arc<AtomicBool>,
    worker: impl FnOnce() + Send + 'static,
) -> io::Result<()> {
    spawn_in(TASKS.get_or_init(Default::default), name, cancel, worker)
}

fn spawn_in(
    registry: &Mutex<Registry>,
    name: &str,
    cancel: Arc<AtomicBool>,
    worker: impl FnOnce() + Send + 'static,
) -> io::Result<()> {
    let task = Arc::new(Task {
        cancel,
        done: AtomicBool::new(false),
        error: Mutex::new(None),
    });
    let mut registry = registry.lock().unwrap_or_else(|error| error.into_inner());
    registry.tasks.retain(|task| {
        !task.done.load(Ordering::Acquire)
            || task
                .error
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .is_some()
    });
    if registry.closed || registry.tasks.len() >= MAX_WORKERS {
        return Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "Standalone audit worker admission is closed or full",
        ));
    }
    let running = task.clone();
    std::thread::Builder::new()
        .name(name.into())
        .spawn(move || {
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(worker)).is_err() {
                *running
                    .error
                    .lock()
                    .unwrap_or_else(|error| error.into_inner()) =
                    Some("Standalone audit worker panicked; its history may be incomplete".into());
            }
            running.done.store(true, Ordering::Release);
        })?;
    registry.tasks.push(task);
    Ok(())
}

/// Called only after the GUI window or terminal has closed. Network requests
/// already inside the blocking tracker client cannot be forcibly interrupted.
pub fn finish_frontend_audits(timeout: Duration) -> Result<(), String> {
    finish_in(TASKS.get_or_init(Default::default), timeout)
}

fn finish_in(registry: &Mutex<Registry>, timeout: Duration) -> Result<(), String> {
    let tasks = {
        let mut registry = registry.lock().unwrap_or_else(|error| error.into_inner());
        registry.closed = true;
        registry.tasks.clone()
    };
    for task in &tasks {
        task.cancel.store(true, Ordering::Release);
    }
    let deadline = Instant::now() + timeout;
    while tasks.iter().any(|task| !task.done.load(Ordering::Acquire)) {
        if Instant::now() >= deadline {
            let pending = tasks
                .iter()
                .filter(|task| !task.done.load(Ordering::Acquire))
                .count();
            return Err(format!(
                "Standalone audit shutdown timed out with {pending} worker(s) unfinished. In-flight tracker replies and their audit history may be incomplete; no transfer owner was stopped."
            ));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let errors: Vec<_> = tasks
        .iter()
        .filter_map(|task| {
            task.error
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .clone()
        })
        .collect();
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

/// All waits run after the frontend surface has closed. Each boundary has its
/// own deadline, and an earlier error never suppresses later flush attempts.
pub fn finish_after_close() -> Result<(), String> {
    let workers = finish_frontend_audits(Duration::from_secs(8));
    let actions = crate::control::frontend::finish_pending_actions(Duration::from_secs(8));
    let history = crate::live_history::finish_all_audit_writers(Duration::from_secs(5));
    let errors: Vec<_> = [workers, actions, history]
        .into_iter()
        .filter_map(Result::err)
        .collect();
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn cancelled_worker_keeps_its_late_exchange_even_after_receiver_disappears() {
        let registry = Mutex::new(Registry::default());
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = cancel.clone();
        let (visible, receiver) = mpsc::channel();
        let recorded = Arc::new(Mutex::new(Vec::new()));
        let output = recorded.clone();
        spawn_in(&registry, "audit-late-response-test", cancel, move || {
            while !worker_cancel.load(Ordering::Acquire) {
                std::thread::sleep(Duration::from_millis(1));
            }
            // Model a reply already in flight: durable admission precedes the
            // optional display delivery, which is allowed to be disconnected.
            output.lock().unwrap().push("late exchange");
            let _ = visible.send("late exchange");
        })
        .unwrap();
        drop(receiver);
        finish_in(&registry, Duration::from_secs(2)).unwrap();
        assert_eq!(&*recorded.lock().unwrap(), &["late exchange"]);
        assert!(spawn_in(&registry, "closed", Arc::new(AtomicBool::new(false)), || {}).is_err());
    }

    #[test]
    fn stuck_blocking_call_has_a_bounded_explicit_incomplete_result() {
        let registry = Mutex::new(Registry::default());
        let (release, blocked) = mpsc::channel();
        spawn_in(
            &registry,
            "audit-blocked-test",
            Arc::new(AtomicBool::new(false)),
            move || {
                let _ = blocked.recv();
            },
        )
        .unwrap();
        let started = Instant::now();
        assert!(
            finish_in(&registry, Duration::from_millis(20))
                .unwrap_err()
                .contains("unfinished")
        );
        assert!(started.elapsed() < Duration::from_secs(1));
        release.send(()).unwrap();
        finish_in(&registry, Duration::from_secs(1)).unwrap();
    }

    #[test]
    fn late_http_reply_is_written_once_after_ui_receiver_drop_and_worker_cancellation() {
        use std::{
            io::{BufRead, Write},
            net::TcpListener,
        };
        let directory = tempfile::tempdir().unwrap();
        let history =
            crate::live_history::LiveHistory::<crate::live_history::AuditExchange>::new_in_for_test(
                directory.path().to_owned(),
            );
        let mut producer = history.producer();
        let registry = Mutex::new(Registry::default());
        let cancel = Arc::new(AtomicBool::new(false));
        let tracker_cancel = cancel.clone();
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let (started, request_started) = mpsc::channel();
        let tracker = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut reader = std::io::BufReader::new(&stream);
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap() == 0 || line == "\r\n" {
                    break;
                }
            }
            started.send(()).unwrap();
            let deadline = Instant::now() + Duration::from_secs(2);
            while !tracker_cancel.load(Ordering::Acquire) {
                assert!(Instant::now() < deadline);
                std::thread::sleep(Duration::from_millis(1));
            }
            let body = b"d8:intervali60e5:peers0:e";
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: {}\r\n\r\n",
                body.len()
            )
            .unwrap();
            stream.write_all(body).unwrap();
        });
        let (visible, receiver) = mpsc::channel();
        spawn_in(&registry, "audit-real-late-http", cancel, move || {
            let exchange = crate::live_history::AuditExchange::execute(
                &format!("http://{address}/announce"),
                &[],
                crate::tracker::AnnounceRequest {
                    info_hash: [1; 20],
                    peer_id: *b"-PL0001-123456789012",
                    port: 6881,
                    uploaded: 0,
                    downloaded: 0,
                    left: 1,
                    event: Some(crate::tracker::AnnounceEvent::Started),
                },
                0,
            );
            assert!(
                exchange.error.is_none(),
                "late loopback reply failed: {:?}",
                exchange.error
            );
            producer.push(exchange.clone());
            let _ = visible.send(exchange);
        })
        .unwrap();
        request_started
            .recv_timeout(Duration::from_secs(2))
            .unwrap();
        drop(receiver);
        finish_in(&registry, Duration::from_secs(2)).unwrap();
        tracker.join().unwrap();
        history.flush_disk().unwrap();
        let mut records = Vec::new();
        for entry in std::fs::read_dir(directory.path()).unwrap() {
            let path = entry.unwrap().path();
            if path
                .extension()
                .is_some_and(|extension| extension == "jsonl")
            {
                records.extend(
                    std::fs::read_to_string(path)
                        .unwrap()
                        .lines()
                        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap()),
                );
            }
        }
        assert_eq!(
            records.len(),
            1,
            "The accepted late exchange must not disappear or be duplicated"
        );
        assert_eq!(records[0]["step_index"], 0);
    }
}
