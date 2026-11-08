//! Deduplicated, bounded background reads with aged answers.

use std::collections::HashMap;
use std::hash::Hash;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

struct Entry<V> {
    value: Option<Arc<V>>,
    ready_at: Option<Instant>,
    used_at: Instant,
    pending: bool,
    active: Arc<AtomicBool>,
}

struct Pool<K, V> {
    jobs: mpsc::SyncSender<(K, Arc<AtomicBool>)>,
    answers: mpsc::Receiver<(K, Arc<AtomicBool>, Option<V>)>,
}

/// A lazy fixed worker pool and bounded LRU answer cache.
///
/// Requests never wait for capacity. Pending keys deduplicate, including during
/// refresh. `poll` refreshes retained answers even when their pure pane is cached;
/// unchanged answers renew their age without advancing `generation`. Completed
/// entries alone may be evicted; retirement invalidates pending work by identity.
/// One refused key is retained for admission before the next refresh pass.
/// Dropping the store closes its queue without joining blocked readers.
pub struct WorkerStore<K, V> {
    entries: HashMap<K, Entry<V>>,
    pool: Option<Pool<K, V>>,
    read: Arc<dyn Fn(&K) -> V + Send + Sync>,
    workers: usize,
    capacity: usize,
    ttl: Duration,
    generation: u64,
    waiting: Option<K>,
}

impl<K, V> WorkerStore<K, V>
where
    K: Clone + Eq + Hash + Send + 'static,
    V: PartialEq + Send + 'static,
{
    /// Each channel and the answer cache hold at most `capacity` items;
    /// at most `workers` reads execute at once. TTL must be positive.
    pub fn new(
        workers: usize,
        capacity: usize,
        ttl: Duration,
        read: impl Fn(&K) -> V + Send + Sync + 'static,
    ) -> Self {
        assert!(workers > 0 && capacity > 0 && !ttl.is_zero());
        Self {
            entries: HashMap::new(),
            pool: None,
            read: Arc::new(read),
            workers,
            capacity,
            ttl,
            generation: 0,
            waiting: None,
        }
    }

    fn start_pool(&mut self) {
        if self.pool.is_some() {
            return;
        }
        let (jobs, incoming) = mpsc::sync_channel::<(K, Arc<AtomicBool>)>(self.capacity);
        let (outgoing, answers) = mpsc::sync_channel(self.capacity);
        let incoming = Arc::new(Mutex::new(incoming));
        for _ in 0..self.workers {
            let incoming = incoming.clone();
            let outgoing = outgoing.clone();
            let read = self.read.clone();
            std::thread::spawn(move || loop {
                let key = incoming.lock().unwrap().recv();
                let Ok((key, active)) = key else { break };
                let value = active.load(Ordering::Acquire).then(|| read(&key));
                if outgoing.send((key, active, value)).is_err() {
                    break;
                }
            });
        }
        self.pool = Some(Pool { jobs, answers });
    }

    /// Return the last answer, or `None` until admitted and completed.
    /// A stale answer remains available during its background refresh.
    pub fn request(&mut self, key: K) -> Option<Arc<V>> {
        let now = Instant::now();
        if let Some(entry) = self.entries.get_mut(&key) {
            entry.used_at = now;
            return entry.value.clone();
        }
        if self.entries.len() == self.capacity {
            let oldest = self
                .entries
                .iter()
                .filter(|(_, entry)| !entry.pending)
                .min_by_key(|(_, entry)| entry.used_at)
                .map(|(key, _)| key.clone());
            let Some(oldest) = oldest else {
                self.waiting = Some(key);
                return None;
            };
            self.entries.remove(&oldest);
        }
        self.start_pool();
        let active = Arc::new(AtomicBool::new(true));
        if self
            .pool
            .as_ref()?
            .jobs
            .try_send((key.clone(), active.clone()))
            .is_err()
        {
            self.waiting = Some(key);
            return None;
        }
        self.entries.insert(
            key,
            Entry {
                value: None,
                ready_at: None,
                used_at: now,
                pending: true,
                active,
            },
        );
        None
    }

    /// Retire unneeded keys. Queued work skips the read; an active read may
    /// finish, but its result cannot overwrite a later request for the same key.
    /// Retiring the deferred key also signals a change: other refused readers
    /// may need a repaint to request admission again.
    pub fn retain(&mut self, mut keep: impl FnMut(&K) -> bool) -> bool {
        let before = self.entries.len();
        let retired_waiting = self.waiting.as_ref().is_some_and(|key| !keep(key));
        if retired_waiting {
            self.waiting = None;
        }
        self.entries.retain(|key, entry| {
            let keep = keep(key);
            if !keep {
                entry.active.store(false, Ordering::Release);
            }
            keep
        });
        retired_waiting || self.entries.len() != before
    }

    /// Collect completions and refresh expired answers without blocking.
    /// Returns whether a visible answer changed.
    pub fn poll(&mut self) -> bool {
        let before = self.generation;
        let Some(pool) = &self.pool else { return false };
        let now = Instant::now();
        while let Ok((key, active, value)) = pool.answers.try_recv() {
            if !active.load(Ordering::Acquire) {
                continue;
            }
            let Some(value) = value else { continue };
            if let Some(entry) = self.entries.get_mut(&key) {
                if entry.value.as_deref() != Some(&value) {
                    entry.value = Some(Arc::new(value));
                    self.generation = self.generation.wrapping_add(1);
                }
                entry.ready_at = Some(now);
                entry.pending = false;
            }
        }
        if let Some(key) = self.waiting.take() {
            self.request(key);
        }
        let pool = self.pool.as_ref().expect("pool was started");
        for (key, entry) in &mut self.entries {
            if !entry.pending
                && entry
                    .ready_at
                    .is_some_and(|at| now.duration_since(at) >= self.ttl)
                && pool
                    .jobs
                    .try_send((key.clone(), entry.active.clone()))
                    .is_ok()
            {
                entry.pending = true;
            }
        }
        before != self.generation
    }

    /// Changes only when a collected answer differs from its predecessor.
    pub fn generation(&self) -> u64 {
        self.generation
    }
}

impl<K, V> Drop for WorkerStore<K, V> {
    fn drop(&mut self) {
        for entry in self.entries.values() {
            entry.active.store(false, Ordering::Release);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collect<K, V>(
        store: &mut WorkerStore<K, V>,
        mut done: impl FnMut(&WorkerStore<K, V>) -> bool,
    ) where
        K: Clone + Eq + Hash + Send + 'static,
        V: PartialEq + Send + 'static,
    {
        let deadline = Instant::now() + Duration::from_secs(2);
        while !done(store) {
            store.poll();
            assert!(Instant::now() < deadline, "worker did not answer");
            std::thread::yield_now();
        }
    }

    #[test]
    fn requests_deduplicate_and_identical_refreshes_only_renew_age() {
        let (started, starts) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let released = Mutex::new(released);
        let mut store = WorkerStore::new(1, 1, Duration::from_secs(60), move |key: &u8| {
            started.send(*key).unwrap();
            released
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(2))
                .unwrap()
        });
        assert!(store.request(1).is_none());
        assert_eq!(starts.recv_timeout(Duration::from_secs(2)).unwrap(), 1);
        for _ in 0..10 {
            assert!(store.request(1).is_none());
        }
        assert!(starts.try_recv().is_err());
        release.send(10).unwrap();
        collect(&mut store, |store| store.generation() == 1);
        assert_eq!(*store.request(1).unwrap(), 10);
        for value in [10, 11] {
            store.entries.get_mut(&1).unwrap().ready_at =
                Some(Instant::now() - Duration::from_secs(61));
            assert!(!store.poll());
            assert_eq!(starts.recv_timeout(Duration::from_secs(2)).unwrap(), 1);
            assert_eq!(
                *store.request(1).unwrap(),
                10,
                "refresh keeps the previous text available"
            );
            release.send(value).unwrap();
            collect(&mut store, |store| !store.entries[&1].pending);
            assert_eq!(*store.request(1).unwrap(), value);
            assert_eq!(store.generation(), if value == 10 { 1 } else { 2 });
        }
    }

    #[test]
    fn the_pool_and_pending_keys_are_bounded_and_completed_keys_can_be_evicted() {
        let (started, starts) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let released = Arc::new(Mutex::new(released));
        let mut store = WorkerStore::new(2, 3, Duration::from_secs(60), move |key: &u8| {
            started.send(*key).unwrap();
            released
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(2))
                .unwrap();
            *key
        });
        for key in 1..=3 {
            assert!(store.request(key).is_none());
        }
        starts.recv_timeout(Duration::from_secs(2)).unwrap();
        starts.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(
            starts.try_recv().is_err(),
            "the third job must remain queued"
        );
        assert!(store.request(4).is_none());
        assert_eq!(store.entries.len(), 3, "pending entries cannot be evicted");
        for _ in 0..4 {
            release.send(()).unwrap();
        }
        collect(&mut store, |store| store.generation() == 4);
        assert_eq!(store.entries.len(), 3);
        assert_eq!(*store.request(4).unwrap(), 4);
    }
    #[test]
    fn an_identical_refresh_wakes_a_request_refused_under_backpressure() {
        let (started, starts) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let released = Mutex::new(released);
        let mut store = WorkerStore::new(1, 1, Duration::from_secs(60), move |key: &u8| {
            started.send(*key).unwrap();
            released
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(2))
                .unwrap();
            *key
        });
        store.request(1);
        starts.recv_timeout(Duration::from_secs(2)).unwrap();
        release.send(()).unwrap();
        collect(&mut store, |store| store.generation() == 1);
        store.entries.get_mut(&1).unwrap().ready_at =
            Some(Instant::now() - Duration::from_secs(61));
        assert!(!store.poll());
        starts.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(store.request(2).is_none());
        release.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while !store.entries.contains_key(&2) {
            store.poll();
            assert!(
                Instant::now() < deadline,
                "the refused pure reader needs admission before refreshes"
            );
            std::thread::yield_now();
        }
        assert_eq!(store.generation(), 1);
        assert_eq!(starts.recv_timeout(Duration::from_secs(2)).unwrap(), 2);
        release.send(()).unwrap();
        collect(&mut store, |store| store.generation() == 2);
    }
    #[test]
    fn retired_jobs_skip_queued_reads_and_cannot_overwrite_a_reused_key() {
        let (started, starts) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let released = Mutex::new(released);
        let mut store = WorkerStore::new(1, 2, Duration::from_secs(60), move |key: &u8| {
            started.send(*key).unwrap();
            released
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(2))
                .unwrap()
        });
        store.request(1);
        assert_eq!(starts.recv_timeout(Duration::from_secs(2)).unwrap(), 1);
        store.request(2);
        store.request(3);
        store.retain(|_| false);
        store.request(1);
        release.send(10).unwrap();
        assert_eq!(
            starts.recv_timeout(Duration::from_secs(2)).unwrap(),
            1,
            "retired queued key 2 must not read"
        );
        assert!(!store.poll());
        assert!(store.request(1).is_none());
        release.send(20).unwrap();
        collect(&mut store, |store| store.generation() == 1);
        assert_eq!(*store.request(1).unwrap(), 20);
        assert_eq!(
            store.entries.len(),
            1,
            "retired deferred key 3 must not be admitted"
        );
        assert!(starts.try_recv().is_err());
    }
    #[test]
    fn skipped_jobs_wake_an_admission_refused_by_a_full_queue() {
        let (started, starts) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let released = Mutex::new(released);
        let mut store = WorkerStore::new(1, 1, Duration::from_secs(60), move |key: &u8| {
            started.send(*key).unwrap();
            released
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(2))
                .unwrap();
            *key
        });
        store.request(1);
        starts.recv_timeout(Duration::from_secs(2)).unwrap();
        store.retain(|_| false);
        store.request(2);
        store.retain(|_| false);
        store.request(3);
        assert!(
            store.entries.is_empty(),
            "the cancelled job still fills the queue"
        );
        release.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while !store.entries.contains_key(&3) {
            store.poll();
            assert!(
                Instant::now() < deadline,
                "skipped jobs must admit the refused reader"
            );
            std::thread::yield_now();
        }
        assert_eq!(store.generation(), 0);
        assert_eq!(starts.recv_timeout(Duration::from_secs(2)).unwrap(), 3);
        release.send(()).unwrap();
        collect(&mut store, |store| store.generation() == 1);
    }
    #[test]
    fn pending_keys_deduplicate_even_with_spare_capacity() {
        let (started, starts) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let released = Mutex::new(released);
        let mut store = WorkerStore::new(2, 3, Duration::from_secs(60), move |key: &u8| {
            started.send(*key).unwrap();
            released
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(2))
                .unwrap();
            *key
        });
        store.request(1);
        assert_eq!(starts.recv_timeout(Duration::from_secs(2)).unwrap(), 1);
        for _ in 0..10 {
            assert!(store.request(1).is_none());
        }
        store.request(2);
        assert_eq!(
            starts.recv_timeout(Duration::from_secs(2)).unwrap(),
            2,
            "spare workers must not run duplicate key 1"
        );
        release.send(()).unwrap();
        release.send(()).unwrap();
        collect(&mut store, |store| store.generation() == 2);
        assert_eq!(*store.request(1).unwrap(), 1);
        assert_eq!(*store.request(2).unwrap(), 2);
    }
    #[test]
    fn retiring_a_deferred_key_wakes_other_refused_readers() {
        let (started, starts) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let released = Mutex::new(released);
        let mut store = WorkerStore::new(1, 1, Duration::from_secs(60), move |key: &u8| {
            started.send(*key).unwrap();
            released
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(2))
                .unwrap();
            *key
        });
        store.request(0);
        assert_eq!(starts.recv_timeout(Duration::from_secs(2)).unwrap(), 0);
        store.retain(|_| false);
        store.request(1);
        store.retain(|_| false);
        store.request(2);
        store.request(3);
        let mut repaint = store.retain(|key| *key == 2);
        release.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            repaint |= store.poll();
            if repaint {
                store.request(2);
                repaint = false;
            }
            if let Ok(key) = starts.try_recv() {
                assert_eq!(key, 2, "only the surviving reader may start");
                break;
            }
            assert!(
                Instant::now() < deadline,
                "retiring deferred work must wake the surviving pure reader"
            );
            std::thread::yield_now();
        }
        release.send(()).unwrap();
        collect(&mut store, |store| store.generation() == 1);
        assert_eq!(*store.request(2).unwrap(), 2);
    }
}
