//! The registry of the live sessions of the phones.

use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};

use serde::Serialize;
use tokio::sync::{watch, Notify};

/// Handle the hub keeps for one established session.
pub struct Live {
    pub session_id: u64,
    pub ip: String,
    pub app_version: String,
    pub connected_at: i64,
    pub last_seen: Arc<AtomicI64>,
    /// Woken when new deliveries exist for the device.
    pub wake: Arc<Notify>,
    /// Set to a WebSocket close code to terminate the session.
    pub kick: watch::Sender<Option<u16>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct LiveInfo {
    pub ip: String,
    pub app_version: String,
    pub connected_at: i64,
    pub last_seen_at: i64,
}

#[derive(Default)]
pub struct Hub {
    sessions: Mutex<HashMap<String, Live>>,
}

impl Hub {
    /// Registers a session, closing any previous session of the same device.
    pub fn register(&self, device_id: &str, live: Live, replaced_code: u16) {
        let old = self
            .sessions
            .lock()
            .expect("hub lock")
            .insert(device_id.to_string(), live);
        if let Some(old) = old {
            old.kick.send_replace(Some(replaced_code));
        }
    }

    /// Removes the session if it is still the registered one.
    pub fn unregister(&self, device_id: &str, session_id: u64) {
        let mut map = self.sessions.lock().expect("hub lock");
        if map
            .get(device_id)
            .is_some_and(|l| l.session_id == session_id)
        {
            map.remove(device_id);
        }
    }

    pub fn wake(&self, device_ids: &[String]) {
        let map = self.sessions.lock().expect("hub lock");
        for id in device_ids {
            if let Some(l) = map.get(id) {
                l.wake.notify_one();
            }
        }
    }

    /// Wakes every session: used after the notifications of the other instances may have been missed.
    pub fn wake_all(&self) {
        for l in self.sessions.lock().expect("hub lock").values() {
            l.wake.notify_one();
        }
    }

    /// Closes the session of `device_id` unless it is newer than `before_ms` (the device opened another one elsewhere).
    pub fn kick_older(&self, device_id: &str, before_ms: i64, code: u16) {
        if let Some(l) = self.sessions.lock().expect("hub lock").get(device_id) {
            if l.connected_at < before_ms {
                l.kick.send_replace(Some(code));
            }
        }
    }

    pub fn kick(&self, device_id: &str, code: u16) {
        if let Some(l) = self.sessions.lock().expect("hub lock").get(device_id) {
            l.kick.send_replace(Some(code));
        }
    }

    pub fn snapshot(&self) -> HashMap<String, LiveInfo> {
        let map = self.sessions.lock().expect("hub lock");
        map.iter()
            .map(|(id, l)| {
                (
                    id.clone(),
                    LiveInfo {
                        ip: l.ip.clone(),
                        app_version: l.app_version.clone(),
                        connected_at: l.connected_at,
                        last_seen_at: l.last_seen.load(Ordering::Relaxed),
                    },
                )
            })
            .collect()
    }

    /// Closes every session with `code` (server shutdown).
    pub fn kick_all(&self, code: u16) {
        for l in self.sessions.lock().expect("hub lock").values() {
            l.kick.send_replace(Some(code));
        }
    }
}
