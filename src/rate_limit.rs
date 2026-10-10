use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant};

#[derive(Debug)]
struct Entry {
    failures: VecDeque<Instant>,
    lockout_until: Option<Instant>,
    last_seen: Instant,
}

#[derive(Debug)]
pub struct RateLimiter {
    entries: Mutex<HashMap<String, Entry>>,
    window: Duration,
    lockout: Duration,
    max_entries: usize,
}

impl RateLimiter {
    pub fn new(window: Duration, lockout: Duration, max_entries: usize) -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            window,
            lockout,
            max_entries,
        }
    }

    pub fn check(&self, key: &str, limit: usize) -> Result<(), Duration> {
        let now = Instant::now();
        let mut map = self.entries.lock().expect("rate limiter mutex poisoned");
        self.prune_map(&mut map, now);
        if let Some(entry) = map.get_mut(key) {
            entry.last_seen = now;
            prune_failures(entry, now, self.window);
            if let Some(until) = entry.lockout_until {
                if until > now {
                    return Err(until.duration_since(now));
                }
                entry.lockout_until = None;
                entry.failures.clear();
            }
            if entry.failures.len() >= limit {
                let until = now + self.lockout;
                entry.lockout_until = Some(until);
                return Err(self.lockout);
            }
        }
        Ok(())
    }

    pub fn failure(&self, key: String, limit: usize) {
        let now = Instant::now();
        let mut map = self.entries.lock().expect("rate limiter mutex poisoned");
        self.prune_map(&mut map, now);
        if !map.contains_key(&key) {
            ensure_capacity(&mut map, self.max_entries);
        }
        let entry = map.entry(key).or_insert_with(|| Entry {
            failures: VecDeque::new(),
            lockout_until: None,
            last_seen: now,
        });
        entry.last_seen = now;
        prune_failures(entry, now, self.window);
        entry.failures.push_back(now);
        if entry.failures.len() >= limit {
            entry.lockout_until = Some(now + self.lockout);
        }
    }

    pub fn success(&self, key: &str) {
        self.entries
            .lock()
            .expect("rate limiter mutex poisoned")
            .remove(key);
    }

    pub fn entry_count(&self) -> usize {
        let now = Instant::now();
        let mut map = self.entries.lock().expect("rate limiter mutex poisoned");
        self.prune_map(&mut map, now);
        map.len()
    }

    fn prune_map(&self, map: &mut HashMap<String, Entry>, now: Instant) {
        let stale_after = self.window + self.lockout + self.window;
        map.retain(|_, entry| {
            let locked = entry.lockout_until.is_some_and(|until| until > now);
            locked || now.duration_since(entry.last_seen) <= stale_after
        });
    }
}

fn prune_failures(entry: &mut Entry, now: Instant, window: Duration) {
    while entry
        .failures
        .front()
        .is_some_and(|oldest| now.duration_since(*oldest) > window)
    {
        entry.failures.pop_front();
    }
}

fn ensure_capacity(map: &mut HashMap<String, Entry>, max_entries: usize) {
    if map.len() < max_entries {
        return;
    }
    if let Some(oldest_key) = map
        .iter()
        .min_by_key(|(_, entry)| entry.last_seen)
        .map(|(key, _)| key.clone())
    {
        map.remove(&oldest_key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_limit_blocks_after_failures() {
        let rl = RateLimiter::new(Duration::from_secs(60), Duration::from_secs(60), 64);
        let key = "ip:127.0.0.1";
        assert!(rl.check(key, 2).is_ok());
        rl.failure(key.to_string(), 2);
        assert!(rl.check(key, 2).is_ok());
        rl.failure(key.to_string(), 2);
        assert!(rl.check(key, 2).is_err());
    }

    #[test]
    fn entry_count_tracks_bounded_keys() {
        let rl = RateLimiter::new(Duration::from_secs(60), Duration::from_secs(60), 2);
        rl.failure("a".to_string(), 10);
        rl.failure("b".to_string(), 10);
        assert_eq!(rl.entry_count(), 2);
        rl.failure("c".to_string(), 10);
        assert_eq!(rl.entry_count(), 2);
    }
}
