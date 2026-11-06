//! The limiter of the failed attempts of a client address.
//!
//! The limiter records an enrollment attempt when it starts, and a successful enrollment clears the
//! record. It also records a session handshake that proves no enrolled phone. It never refuses a
//! phone that proves its key: the failures of one client must not lock out the other phones of the
//! same address.

use std::collections::{HashMap, VecDeque};
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Hard cap on tracked addresses, so a scan cannot exhaust memory.
const MAX_TRACKED: usize = 65_536;

pub struct AuthLimiter {
    max_failures: usize,
    window: Duration,
    failures: Mutex<HashMap<IpAddr, VecDeque<Instant>>>,
}

impl AuthLimiter {
    pub fn new(max_failures: u32, window: Duration) -> AuthLimiter {
        AuthLimiter {
            max_failures: max_failures as usize,
            window,
            failures: Mutex::new(HashMap::new()),
        }
    }

    /// Whether `ip` may attempt to authenticate now.
    pub fn allowed(&self, ip: IpAddr) -> bool {
        let now = Instant::now();
        let mut map = self.failures.lock().expect("limiter lock");
        match map.get_mut(&ip) {
            None => true,
            Some(q) => {
                while q
                    .front()
                    .is_some_and(|t| now.duration_since(*t) > self.window)
                {
                    q.pop_front();
                }
                if q.is_empty() {
                    map.remove(&ip);
                    return true;
                }
                q.len() < self.max_failures
            }
        }
    }

    pub fn record_failure(&self, ip: IpAddr) {
        let now = Instant::now();
        let mut map = self.failures.lock().expect("limiter lock");
        if map.len() >= MAX_TRACKED && !map.contains_key(&ip) {
            let window = self.window;
            map.retain(|_, q| q.back().is_some_and(|t| now.duration_since(*t) <= window));
            if map.len() >= MAX_TRACKED {
                return;
            }
        }
        let q = map.entry(ip).or_default();
        q.push_back(now);
        while q.len() > self.max_failures {
            q.pop_front();
        }
    }

    pub fn record_success(&self, ip: IpAddr) {
        self.failures.lock().expect("limiter lock").remove(&ip);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_after_max_failures() {
        let l = AuthLimiter::new(2, Duration::from_secs(60));
        let ip: IpAddr = "10.0.0.1".parse().unwrap();
        assert!(l.allowed(ip));
        l.record_failure(ip);
        assert!(l.allowed(ip));
        l.record_failure(ip);
        assert!(!l.allowed(ip));
        assert!(l.allowed("10.0.0.2".parse().unwrap()));
        l.record_success(ip);
        assert!(l.allowed(ip));
    }
}
