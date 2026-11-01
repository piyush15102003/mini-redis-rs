use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

pub struct Entry {
    pub value: Vec<u8>,
    pub expires_at: Option<Instant>,
    /// Logical clock value of the last read/write — the smallest one is the LRU victim.
    pub last_used: u64,
}

impl Entry {
    fn is_expired(&self, now: Instant) -> bool {
        self.expires_at.is_some_and(|t| t <= now)
    }
}

struct Inner {
    map: HashMap<Vec<u8>, Entry>,
    max_keys: Option<usize>,
    clock: u64,
}

impl Inner {
    fn tick(&mut self) -> u64 {
        self.clock += 1;
        self.clock
    }

    /// Passive expiry: drop the key if it has expired, then return the live entry.
    fn live(&mut self, key: &[u8]) -> Option<&mut Entry> {
        let now = Instant::now();
        if self.map.get(key).is_some_and(|e| e.is_expired(now)) {
            self.map.remove(key);
            return None;
        }
        self.map.get_mut(key)
    }

    /// Insert or overwrite. Only a brand-new key can trigger an eviction.
    fn insert(&mut self, key: Vec<u8>, value: Vec<u8>, expires_at: Option<Instant>) {
        let last_used = self.tick();
        if let Some(max) = self.max_keys {
            if !self.map.contains_key(&key) && self.map.len() >= max {
                self.remove_expired();
                if self.map.len() >= max {
                    // O(n) scan for the least recently used key. Fine at this scale; a real
                    // implementation would keep an intrusive linked list for O(1).
                    if let Some(victim) =
                        self.map.iter().min_by_key(|(_, e)| e.last_used).map(|(k, _)| k.clone())
                    {
                        self.map.remove(&victim);
                    }
                }
            }
        }
        self.map.insert(key, Entry { value, expires_at, last_used });
    }

    fn remove_expired(&mut self) -> usize {
        let now = Instant::now();
        let before = self.map.len();
        self.map.retain(|_, e| !e.is_expired(now));
        before - self.map.len()
    }
}

/// Shared, TTL-aware key space. Cloning is cheap (it clones the Arc).
/// Every method takes the lock, does its work, and releases it before returning,
/// so the lock is never held across an `.await`.
#[derive(Clone)]
pub struct Store {
    inner: Arc<Mutex<Inner>>,
}

impl Store {
    pub fn new(max_keys: Option<usize>) -> Self {
        let inner = Inner { map: HashMap::new(), max_keys, clock: 0 };
        Store { inner: Arc::new(Mutex::new(inner)) }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        // A panic while holding the lock leaves the map usable; don't take the server down.
        self.inner.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn get(&self, key: &[u8]) -> Option<Vec<u8>> {
        let mut inner = self.lock();
        let now = inner.tick();
        let entry = inner.live(key)?;
        entry.last_used = now;
        Some(entry.value.clone())
    }

    pub fn set(&self, key: Vec<u8>, value: Vec<u8>, ttl: Option<Duration>) {
        let expires_at = ttl.map(|d| Instant::now() + d);
        self.lock().insert(key, value, expires_at);
    }

    /// Returns how many of the keys existed and were removed.
    pub fn del(&self, keys: &[Vec<u8>]) -> i64 {
        let mut inner = self.lock();
        keys.iter().filter(|k| inner.live(k).is_some() && inner.map.remove(*k).is_some()).count() as i64
    }

    pub fn exists(&self, keys: &[Vec<u8>]) -> i64 {
        let mut inner = self.lock();
        keys.iter().filter(|k| inner.live(k).is_some()).count() as i64
    }

    pub fn dbsize(&self) -> i64 {
        let now = Instant::now();
        self.lock().map.values().filter(|e| !e.is_expired(now)).count() as i64
    }

    pub fn flushall(&self) {
        self.lock().map.clear();
    }

    /// Read-modify-write under ONE lock acquisition, so concurrent INCRs never lose updates.
    pub fn incr(&self, key: &[u8]) -> Result<i64, String> {
        let mut inner = self.lock();
        let now = inner.tick();
        let not_int = || "ERR value is not an integer or out of range".to_string();
        let (current, expires_at) = match inner.live(key) {
            Some(e) => {
                let n = std::str::from_utf8(&e.value)
                    .ok()
                    .and_then(|s| s.parse::<i64>().ok())
                    .ok_or_else(not_int)?;
                (n, e.expires_at)
            }
            None => (0, None),
        };
        let next = current.checked_add(1).ok_or_else(not_int)?;
        match inner.live(key) {
            // Existing key: keep its TTL.
            Some(e) => {
                e.value = next.to_string().into_bytes();
                e.last_used = now;
            }
            None => inner.insert(key.to_vec(), next.to_string().into_bytes(), expires_at),
        }
        Ok(next)
    }

    /// -2 = no such key, -1 = no expiry, otherwise whole seconds remaining (rounded up).
    pub fn ttl(&self, key: &[u8]) -> i64 {
        let mut inner = self.lock();
        match inner.live(key) {
            None => -2,
            Some(e) => match e.expires_at {
                None => -1,
                Some(t) => {
                    let ms = t.saturating_duration_since(Instant::now()).as_millis() as i64;
                    (ms + 999) / 1000
                }
            },
        }
    }

    /// Returns 1 if the timeout was set, 0 if the key does not exist.
    pub fn expire(&self, key: &[u8], secs: i64) -> i64 {
        let mut inner = self.lock();
        let Some(e) = inner.live(key) else { return 0 };
        if secs <= 0 {
            inner.map.remove(key);
        } else {
            e.expires_at = Some(Instant::now() + Duration::from_secs(secs as u64));
        }
        1
    }

    /// Active expiry: called periodically by a background task.
    pub fn sweep(&self) -> usize {
        self.lock().remove_expired()
    }
}
