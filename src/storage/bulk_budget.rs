//! Bounded bulk-job admission by pinned filesystem volume, plus a shared I/O budget.
//! Volume identity does not imply physical-disk identity. Unknown volumes serialize
//! against every coordinated job. Checks also retain the configured initializer cap.
use std::{
    collections::{BTreeMap, HashSet},
    io,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Volume {
    Known(u64),
    Unknown,
}
#[derive(Default)]
struct State {
    next: u64,
    waiting: BTreeMap<u64, Work>,
    active: BTreeMap<u64, Work>,
    last_io: Option<Instant>,
    io_tokens: f64,
}
struct Work {
    volumes: Vec<Volume>,
    bulk: bool,
}
pub(crate) struct Budget {
    state: Mutex<State>,
    changed: Condvar,
    active_limit: usize,
    bytes_per_second: u64,
}
impl Budget {
    pub fn new(active_limit: usize, kib_per_second: u64) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(State::default()),
            changed: Condvar::new(),
            active_limit: active_limit.clamp(1, 32),
            bytes_per_second: kib_per_second.saturating_mul(1024),
        })
    }
    pub fn register(self: &Arc<Self>, mut volumes: Vec<Volume>) -> io::Result<Admission> {
        self.register_kind(&mut volumes, true)
    }
    fn register_kind(
        self: &Arc<Self>,
        volumes: &mut Vec<Volume>,
        bulk: bool,
    ) -> io::Result<Admission> {
        let mut seen = HashSet::new();
        volumes.retain(|volume| seen.insert(*volume));
        if volumes.is_empty() {
            volumes.push(Volume::Unknown);
        }
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        let count = state
            .waiting
            .values()
            .chain(state.active.values())
            .filter(|work| work.bulk == bulk)
            .count();
        if count >= if bulk { 64 } else { 32 } {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "Disk admission queue is full",
            ));
        }
        let id = state.next;
        state.next = state
            .next
            .checked_add(1)
            .ok_or_else(|| io::Error::other("Bulk job sequence exhausted"))?;
        state.waiting.insert(
            id,
            Work {
                volumes: std::mem::take(volumes),
                bulk,
            },
        );
        Ok(Admission {
            budget: self.clone(),
            id,
        })
    }
    fn conflicts(state: &State, volumes: &[Volume]) -> bool {
        state.active.values().any(|active| {
            active.volumes.contains(&Volume::Unknown)
                || volumes.contains(&Volume::Unknown)
                || active.volumes.iter().any(|volume| volumes.contains(volume))
        })
    }
    /// The engine's initializer future can wait without owning a blocking thread.
    pub async fn enter_check(
        self: &Arc<Self>,
        volume: Option<u64>,
        cancel: &AtomicBool,
    ) -> io::Result<Permit> {
        let admission = self.register_kind(
            &mut vec![volume.map(Volume::Known).unwrap_or(Volume::Unknown)],
            false,
        )?;
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err(cancelled());
            }
            if let Some(permit) = admission.try_enter() {
                return Ok(permit);
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
    pub fn throttle(&self, bytes: u64, cancel: &AtomicBool) -> io::Result<()> {
        if cancel.load(Ordering::Relaxed) {
            return Err(cancelled());
        }
        if self.bytes_per_second == 0 || bytes == 0 {
            return Ok(());
        }
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        let mut remaining = bytes as f64;
        let rate = self.bytes_per_second as f64;
        let capacity = rate.min(1024.0 * 1024.0);
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err(cancelled());
            }
            let now = Instant::now();
            let elapsed = now.saturating_duration_since(state.last_io.unwrap_or(now));
            state.last_io = Some(now);
            state.io_tokens = (state.io_tokens + elapsed.as_secs_f64() * rate).min(capacity);
            let used = remaining.min(state.io_tokens);
            remaining -= used;
            state.io_tokens -= used;
            if remaining < 0.5 {
                return Ok(());
            }
            let delay = Duration::from_secs_f64((remaining / rate).min(0.05));
            state = self
                .changed
                .wait_timeout(state, delay)
                .unwrap_or_else(|error| error.into_inner())
                .0;
        }
    }
    pub fn wake(&self) {
        self.changed.notify_all();
    }
}
pub(crate) struct Admission {
    budget: Arc<Budget>,
    id: u64,
}
impl Admission {
    /// The actor retries admission while polling. Queued jobs own no OS thread.
    pub fn try_enter(&self) -> Option<Permit> {
        let mut state = self
            .budget
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let active_bulk = state.active.values().filter(|work| work.bulk).count();
        let next = state
            .waiting
            .iter()
            .find(|(_, work)| {
                (!work.bulk || active_bulk < self.budget.active_limit)
                    && !Budget::conflicts(&state, &work.volumes)
            })
            .map(|(id, _)| *id)?;
        if next != self.id {
            return None;
        }
        let work = state.waiting.remove(&self.id)?;
        state.active.insert(self.id, work);
        Some(Permit {
            budget: self.budget.clone(),
            id: self.id,
        })
    }
}
impl Drop for Admission {
    fn drop(&mut self) {
        self.budget
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .waiting
            .remove(&self.id);
        self.budget.wake();
    }
}
pub(crate) struct Permit {
    budget: Arc<Budget>,
    id: u64,
}
impl Drop for Permit {
    fn drop(&mut self) {
        self.budget
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .active
            .remove(&self.id);
        self.budget.wake();
    }
}
fn cancelled() -> io::Error {
    io::Error::new(io::ErrorKind::Interrupted, "Bulk disk operation cancelled")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn checks_share_volume_admission_and_cancelled_or_dropped_futures_release_the_queue() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        let budget = Budget::new(1, 0);
        let bulk = budget.register(vec![Volume::Known(1)]).unwrap();
        let permit = bulk.try_enter().unwrap();
        let cancel = AtomicBool::new(false);
        runtime.block_on(async {
            assert!(
                tokio::time::timeout(
                    Duration::from_millis(20),
                    budget.enter_check(Some(1), &cancel)
                )
                .await
                .is_err()
            );
            assert!(budget.state.lock().unwrap().waiting.is_empty());
            let independent = budget.enter_check(Some(2), &cancel).await.unwrap();
            assert_eq!(budget.state.lock().unwrap().active.len(), 2);
            drop(independent);
            let pending = budget.enter_check(Some(1), &cancel);
            tokio::pin!(pending);
            tokio::select! {
                _ = &mut pending => panic!("conflicting check entered"),
                _ = tokio::time::sleep(Duration::from_millis(10)) => {}
            }
            cancel.store(true, Ordering::Relaxed);
            assert_eq!(
                tokio::time::timeout(Duration::from_secs(1), pending)
                    .await
                    .unwrap()
                    .err()
                    .expect("cancelled admission")
                    .kind(),
                io::ErrorKind::Interrupted
            );
            assert!(budget.state.lock().unwrap().waiting.is_empty());
        });
        drop(permit);
        cancel.store(false, Ordering::Relaxed);
        assert!(
            runtime
                .block_on(budget.enter_check(Some(1), &cancel))
                .is_ok()
        );
    }
    #[test]
    fn same_volume_waits_independent_volume_runs_and_unknown_serializes() {
        let budget = Budget::new(3, 0);
        let a = budget.register(vec![Volume::Known(1)]).unwrap();
        let b = budget.register(vec![Volume::Known(1)]).unwrap();
        let c = budget.register(vec![Volume::Known(2)]).unwrap();
        let permit_a = a.try_enter().unwrap();
        assert!(b.try_enter().is_none());
        let permit_c = c.try_enter().unwrap();
        let unknown = budget.register(vec![Volume::Unknown]).unwrap();
        assert!(unknown.try_enter().is_none());
        drop(permit_a);
        let permit_b = b.try_enter().unwrap();
        drop(permit_c);
        assert!(unknown.try_enter().is_none());
        drop(permit_b);
        let unknown_permit = unknown.try_enter().unwrap();
        let d = budget.register(vec![Volume::Known(3)]).unwrap();
        assert!(d.try_enter().is_none());
        drop(unknown_permit);
        assert!(d.try_enter().is_some());
    }
    #[test]
    fn pending_admission_is_bounded_and_cancellation_releases_its_slot() {
        let budget = Budget::new(1, 0);
        let mut jobs = (0..64)
            .map(|_| budget.register(vec![Volume::Unknown]).unwrap())
            .collect::<Vec<_>>();
        assert!(budget.register(vec![]).is_err());
        jobs.remove(0);
        assert!(budget.register(vec![]).is_ok());
        assert!(jobs[0].try_enter().is_some());
    }
    #[test]
    fn throttled_work_observes_cancellation_without_waiting_for_its_rate_deadline() {
        let budget = Budget::new(1, 1);
        let cancel = Arc::new(AtomicBool::new(false));
        let (worker_budget, worker_cancel) = (budget.clone(), cancel.clone());
        let worker =
            std::thread::spawn(move || worker_budget.throttle(1024 * 1024, &worker_cancel));
        std::thread::sleep(Duration::from_millis(20));
        let start = Instant::now();
        cancel.store(true, Ordering::Relaxed);
        budget.wake();
        assert_eq!(
            worker.join().unwrap().unwrap_err().kind(),
            io::ErrorKind::Interrupted
        );
        assert!(start.elapsed() < Duration::from_secs(1));
    }
    #[test]
    fn shared_rate_charges_bytes_instead_of_a_limit_per_worker() {
        let budget = Budget::new(2, 10);
        let start = Instant::now();
        let workers = (0..2)
            .map(|_| {
                let budget = budget.clone();
                std::thread::spawn(move || budget.throttle(512, &AtomicBool::new(false)).unwrap())
            })
            .collect::<Vec<_>>();
        for worker in workers {
            worker.join().unwrap();
        }
        assert!(start.elapsed() >= Duration::from_millis(90));
    }
}
