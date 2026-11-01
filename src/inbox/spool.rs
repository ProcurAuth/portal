//! Degraded mode: the alerts that the inbox accepts while PostgreSQL has no writable primary.
//!
//! A failover of the database takes from a few seconds to a few minutes. During this time, an
//! instance goes on. It accepts alerts and delivers them to the phones that are connected to it.
//! It keeps each alert in memory, with one delivery for each subscribed phone. It copies the
//! alert to the other instances (`peer.rs`) before it answers the producer. It writes the alert
//! to the database when the database has a primary again (`PgStore::replay`).
//!
//! The id of a spooled alert comes from its source and its idempotency key. Two instances that
//! accept the same alert while the database is away give it the same id. The replay stores it
//! one time (`ON CONFLICT (source, idempotency_key)`). A spooled delivery has a negative
//! sequence number that is local to the instance. The sequence numbers of the database are
//! positive. Both kinds can be in the in-flight table of one session.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::db::{AckOutcome, PendingDelivery};
use super::util::hex;

/// Alerts held at most. Beyond this the producer gets a 503 and retries: the database has been away for very long.
pub const MAX_SPOOLED: usize = 10_000;
/// Receipts for deliveries that live in the database, kept until it is back.
const MAX_DB_ACKS: usize = 10_000;

/// The id of an alert accepted without the database: 128 bits of a hash of (source, idempotency key), in the same 32 hex
/// digits as a random id.
pub fn alert_id(source: &str, idempotency_key: &str) -> String {
    let mut h = Sha256::new();
    h.update(b"inbox-alert-v1\0");
    h.update(source.as_bytes());
    h.update(b"\0");
    h.update(idempotency_key.as_bytes());
    hex(&h.finalize()[..16])
}

/// One device an alert is delivered to, as the other instances see it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireDelivery {
    pub device: String,
    pub received_at: Option<i64>,
}

/// A spooled alert as it travels between instances.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireAlert {
    pub id: String,
    pub group: String,
    pub title: String,
    pub source: String,
    pub key: String,
    pub created_at: i64,
    pub deliveries: Vec<WireDelivery>,
}

#[derive(Debug, Clone)]
pub struct Delivery {
    pub device: String,
    /// Negative, unique in this instance.
    pub seq: i64,
    pub attempts: i64,
    pub first_sent_at: Option<i64>,
    pub last_sent_at: Option<i64>,
    pub received_at: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct Entry {
    pub id: String,
    pub group: String,
    pub title: String,
    pub source: String,
    pub key: String,
    pub created_at: i64,
    pub deliveries: Vec<Delivery>,
    /// Changes whenever the entry does: the replay drops an entry only if nothing changed since it copied it.
    pub rev: u64,
}

impl Entry {
    pub fn wire(&self) -> WireAlert {
        WireAlert {
            id: self.id.clone(),
            group: self.group.clone(),
            title: self.title.clone(),
            source: self.source.clone(),
            key: self.key.clone(),
            created_at: self.created_at,
            deliveries: self
                .deliveries
                .iter()
                .map(|d| WireDelivery {
                    device: d.device.clone(),
                    received_at: d.received_at,
                })
                .collect(),
        }
    }

    /// The devices that have not received the alert yet.
    pub fn waiting(&self) -> Vec<String> {
        self.deliveries
            .iter()
            .filter(|d| d.received_at.is_none())
            .map(|d| d.device.clone())
            .collect()
    }
}

/// A receipt for a delivery stored in the database, which could not be written yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DbAck {
    pub device: String,
    pub alert_id: String,
    pub at: i64,
}

#[derive(Debug)]
pub enum Accepted {
    Created(Entry),
    /// Same source and key, same content: the entry that exists.
    Duplicate(Entry),
    /// Same source and key, different content.
    Conflict,
    Full,
}

#[derive(Default)]
struct Inner {
    alerts: HashMap<String, Entry>,
    /// Oldest first.
    order: VecDeque<String>,
    by_key: HashMap<(String, String), String>,
    by_seq: HashMap<i64, (String, usize)>,
    db_acks: Vec<DbAck>,
    rev: u64,
}

pub struct Spool {
    inner: Mutex<Inner>,
    next_seq: AtomicI64,
}

impl Default for Spool {
    fn default() -> Self {
        Spool {
            inner: Mutex::new(Inner::default()),
            next_seq: AtomicI64::new(-1),
        }
    }
}

impl Inner {
    fn touch(&mut self, id: &str) {
        self.rev += 1;
        let rev = self.rev;
        if let Some(e) = self.alerts.get_mut(id) {
            e.rev = rev;
        }
    }

    fn insert(&mut self, mut entry: Entry, next_seq: &AtomicI64) -> Entry {
        self.rev += 1;
        entry.rev = self.rev;
        for (i, d) in entry.deliveries.iter_mut().enumerate() {
            d.seq = next_seq.fetch_sub(1, Ordering::Relaxed);
            self.by_seq.insert(d.seq, (entry.id.clone(), i));
        }
        self.order.push_back(entry.id.clone());
        self.by_key
            .insert((entry.source.clone(), entry.key.clone()), entry.id.clone());
        self.alerts.insert(entry.id.clone(), entry.clone());
        entry
    }

    fn remove(&mut self, id: &str) -> Option<Entry> {
        let entry = self.alerts.remove(id)?;
        self.order.retain(|o| o != id);
        self.by_key
            .remove(&(entry.source.clone(), entry.key.clone()));
        for d in &entry.deliveries {
            self.by_seq.remove(&d.seq);
        }
        Some(entry)
    }
}

fn fresh(deliveries: impl IntoIterator<Item = (String, Option<i64>)>) -> Vec<Delivery> {
    deliveries
        .into_iter()
        .map(|(device, received_at)| Delivery {
            device,
            seq: 0,
            attempts: 0,
            first_sent_at: None,
            last_sent_at: None,
            received_at,
        })
        .collect()
}

impl Spool {
    /// Keeps a new alert for `devices`, or finds the one with the same source and key.
    #[allow(clippy::too_many_arguments)]
    pub fn accept(
        &self,
        id: String,
        group: String,
        title: String,
        source: String,
        key: String,
        created_at: i64,
        devices: Vec<String>,
    ) -> Accepted {
        let mut inner = self.inner.lock().expect("spool lock");
        if let Some(existing) = inner.by_key.get(&(source.clone(), key.clone())) {
            let e = &inner.alerts[existing];
            return if e.group == group && e.title == title {
                Accepted::Duplicate(e.clone())
            } else {
                Accepted::Conflict
            };
        }
        if inner.alerts.len() >= MAX_SPOOLED {
            return Accepted::Full;
        }
        let entry = Entry {
            id,
            group,
            title,
            source,
            key,
            created_at,
            deliveries: fresh(devices.into_iter().map(|d| (d, None))),
            rev: 0,
        };
        Accepted::Created(inner.insert(entry, &self.next_seq))
    }

    /// Stores an alert that another instance accepted. `Created` when it is new here, `Duplicate` when this instance knew it
    /// already (receipts it did not know are merged in).
    pub fn merge(&self, wire: WireAlert) -> Accepted {
        let mut guard = self.inner.lock().expect("spool lock");
        let inner = &mut *guard;
        if let Some(existing) = inner
            .by_key
            .get(&(wire.source.clone(), wire.key.clone()))
            .cloned()
        {
            let known = inner.alerts.get_mut(&existing).expect("indexed");
            if known.group != wire.group || known.title != wire.title {
                return Accepted::Conflict;
            }
            let mut changed = false;
            let mut unknown = Vec::new();
            for w in &wire.deliveries {
                match known.deliveries.iter_mut().find(|d| d.device == w.device) {
                    Some(d) if d.received_at.is_none() && w.received_at.is_some() => {
                        d.received_at = w.received_at;
                        changed = true;
                    }
                    Some(_) => {}
                    None => unknown.push(w),
                }
            }
            // A device this instance did not know about when it took its own copy.
            for w in unknown {
                let mut d = fresh([(w.device.clone(), w.received_at)]).remove(0);
                d.seq = self.next_seq.fetch_sub(1, Ordering::Relaxed);
                inner
                    .by_seq
                    .insert(d.seq, (existing.clone(), known.deliveries.len()));
                known.deliveries.push(d);
                changed = true;
            }
            if changed {
                inner.touch(&existing);
            }
            return Accepted::Duplicate(inner.alerts[&existing].clone());
        }
        if inner.alerts.len() >= MAX_SPOOLED {
            return Accepted::Full;
        }
        let entry = Entry {
            id: wire.id,
            group: wire.group,
            title: wire.title,
            source: wire.source,
            key: wire.key,
            created_at: wire.created_at,
            deliveries: fresh(
                wire.deliveries
                    .into_iter()
                    .map(|d| (d.device, d.received_at)),
            ),
            rev: 0,
        };
        Accepted::Created(inner.insert(entry, &self.next_seq))
    }

    /// Deliveries of `device` still waiting for its receipt, oldest first, among alerts created at or after `cutoff`.
    pub fn pending_for(&self, device: &str, limit: usize, cutoff: i64) -> Vec<PendingDelivery> {
        let inner = self.inner.lock().expect("spool lock");
        let mut out = Vec::new();
        for id in &inner.order {
            if out.len() >= limit {
                break;
            }
            let e = &inner.alerts[id];
            if e.created_at < cutoff {
                continue;
            }
            if let Some(d) = e
                .deliveries
                .iter()
                .find(|d| d.device == device && d.received_at.is_none())
            {
                out.push(PendingDelivery {
                    seq: d.seq,
                    alert_id: e.id.clone(),
                    group: e.group.clone(),
                    title: e.title.clone(),
                    created_at: e.created_at,
                });
            }
        }
        out
    }

    /// Notes that these deliveries (negative sequence numbers) were sent.
    pub fn mark_sent(&self, seqs: &[i64], now: i64) {
        let mut inner = self.inner.lock().expect("spool lock");
        for seq in seqs {
            let Some((id, i)) = inner.by_seq.get(seq).cloned() else {
                continue;
            };
            if let Some(d) = inner
                .alerts
                .get_mut(&id)
                .and_then(|e| e.deliveries.get_mut(i))
            {
                d.attempts += 1;
                d.first_sent_at.get_or_insert(now);
                d.last_sent_at = Some(now);
            }
            inner.touch(&id);
        }
    }

    /// A receipt from a device of this instance. `None` when the alert is not in the spool.
    pub fn ack(&self, device: &str, alert_id: &str, now: i64) -> Option<AckOutcome> {
        let mut inner = self.inner.lock().expect("spool lock");
        let e = inner.alerts.get_mut(alert_id)?;
        let created_at = e.created_at;
        let d = e.deliveries.iter_mut().find(|d| d.device == device)?;
        let first = d.received_at.is_none();
        d.received_at.get_or_insert(now);
        let seq = d.seq;
        if first {
            inner.touch(alert_id);
        }
        Some(AckOutcome {
            seq,
            first_receipt_latency_ms: first.then_some(now - created_at),
        })
    }

    /// A receipt another instance reports. True when it changed anything.
    pub fn ack_remote(&self, device: &str, alert_id: &str, at: i64) -> bool {
        let mut inner = self.inner.lock().expect("spool lock");
        let Some(e) = inner.alerts.get_mut(alert_id) else {
            return false;
        };
        let Some(d) = e.deliveries.iter_mut().find(|d| d.device == device) else {
            return false;
        };
        if d.received_at.is_some() {
            return false;
        }
        d.received_at = Some(at);
        inner.touch(alert_id);
        true
    }

    /// A receipt for a delivery that is in the database, which cannot be written now.
    pub fn note_db_ack(&self, device: &str, alert_id: &str, at: i64) {
        let mut inner = self.inner.lock().expect("spool lock");
        if inner.db_acks.len() < MAX_DB_ACKS
            && !inner
                .db_acks
                .iter()
                .any(|a| a.device == device && a.alert_id == alert_id)
        {
            inner.db_acks.push(DbAck {
                device: device.to_string(),
                alert_id: alert_id.to_string(),
                at,
            });
        }
    }

    pub fn take_db_acks(&self) -> Vec<DbAck> {
        std::mem::take(&mut self.inner.lock().expect("spool lock").db_acks)
    }

    pub fn restore_db_acks(&self, acks: Vec<DbAck>) {
        let mut inner = self.inner.lock().expect("spool lock");
        for a in acks {
            if inner.db_acks.len() < MAX_DB_ACKS && !inner.db_acks.contains(&a) {
                inner.db_acks.push(a);
            }
        }
    }

    /// Every entry, oldest first.
    pub fn snapshot(&self) -> Vec<Entry> {
        let inner = self.inner.lock().expect("spool lock");
        inner
            .order
            .iter()
            .map(|id| inner.alerts[id].clone())
            .collect()
    }

    /// Drops the entry once it is in the database, unless it changed after it was copied (`rev`).
    pub fn remove_if_unchanged(&self, id: &str, rev: u64) -> bool {
        let mut inner = self.inner.lock().expect("spool lock");
        if inner.alerts.get(id).is_some_and(|e| e.rev == rev) {
            inner.remove(id);
            true
        } else {
            false
        }
    }

    /// Drops the alerts created before `cutoff`: they have expired for delivery anyway.
    pub fn expire(&self, cutoff: i64) -> usize {
        let mut inner = self.inner.lock().expect("spool lock");
        let old: Vec<String> = inner
            .alerts
            .values()
            .filter(|e| e.created_at < cutoff)
            .map(|e| e.id.clone())
            .collect();
        for id in &old {
            inner.remove(id);
        }
        old.len()
    }

    pub fn len(&self) -> usize {
        self.inner.lock().expect("spool lock").alerts.len()
    }

    /// Something waits for the database: an alert, or a receipt.
    pub fn has_work(&self) -> bool {
        let inner = self.inner.lock().expect("spool lock");
        !inner.alerts.is_empty() || !inner.db_acks.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn accept(s: &Spool, key: &str, title: &str, devices: &[&str]) -> Accepted {
        s.accept(
            alert_id("producer:ci", key),
            "infra".into(),
            title.into(),
            "producer:ci".into(),
            key.into(),
            1_000,
            devices.iter().map(|d| d.to_string()).collect(),
        )
    }

    #[test]
    fn the_id_is_deterministic_and_looks_like_a_random_id() {
        let a = alert_id("producer:ci", "k1");
        assert_eq!(a, alert_id("producer:ci", "k1"));
        assert_ne!(a, alert_id("producer:ci", "k2"));
        assert_ne!(a, alert_id("producer:cj", "k1"));
        // The parts cannot be shifted into each other.
        assert_ne!(alert_id("a", "bc"), alert_id("ab", "c"));
        assert!(crate::inbox::util::is_hex_id(&a));
    }

    #[test]
    fn accepting_twice_finds_the_same_entry_and_a_changed_title_conflicts() {
        let s = Spool::default();
        let Accepted::Created(first) = accept(&s, "k", "disk full", &["d1", "d2"]) else {
            panic!("created")
        };
        assert_eq!(first.deliveries.len(), 2);
        assert!(first.deliveries.iter().all(|d| d.seq < 0));
        assert_ne!(first.deliveries[0].seq, first.deliveries[1].seq);
        let Accepted::Duplicate(again) = accept(&s, "k", "disk full", &["d3"]) else {
            panic!("duplicate")
        };
        assert_eq!(again.id, first.id);
        assert_eq!(
            again.deliveries.len(),
            2,
            "the targets of the first acceptance stay"
        );
        assert!(matches!(
            accept(&s, "k", "another title", &[]),
            Accepted::Conflict
        ));
        assert_eq!(s.len(), 1);
    }

    #[test]
    fn deliveries_are_pending_until_the_receipt_and_expire() {
        let s = Spool::default();
        let Accepted::Created(e) = accept(&s, "k", "t", &["d1", "d2"]) else {
            panic!()
        };
        let p = s.pending_for("d1", 10, 0);
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].alert_id, e.id);
        assert!(p[0].seq < 0);
        assert!(
            s.pending_for("d1", 10, 2_000).is_empty(),
            "expired for delivery"
        );
        s.mark_sent(&[p[0].seq], 1_500);
        let sent = s.snapshot();
        assert_eq!(sent[0].deliveries[0].attempts, 1);
        assert_eq!(sent[0].deliveries[0].first_sent_at, Some(1_500));
        let out = s.ack("d1", &e.id, 1_700).unwrap();
        assert_eq!(out.seq, p[0].seq);
        assert_eq!(out.first_receipt_latency_ms, Some(700));
        // A repeated receipt is accepted and is not a first one.
        assert_eq!(
            s.ack("d1", &e.id, 1_800).unwrap().first_receipt_latency_ms,
            None
        );
        assert!(s.pending_for("d1", 10, 0).is_empty());
        assert_eq!(s.pending_for("d2", 10, 0).len(), 1);
        assert!(s.ack("nobody", &e.id, 1).is_none());
        assert!(s.ack("d1", "unknown", 1).is_none());
        assert_eq!(s.expire(2_000), 1);
        assert_eq!(s.len(), 0);
    }

    #[test]
    fn the_limit_applies_to_pending_deliveries() {
        let s = Spool::default();
        for i in 0..5 {
            accept(&s, &format!("k{i}"), "t", &["d1"]);
        }
        assert_eq!(s.pending_for("d1", 3, 0).len(), 3);
    }

    #[test]
    fn an_alert_from_a_peer_is_stored_once_and_merges_receipts() {
        let s = Spool::default();
        let wire = WireAlert {
            id: alert_id("producer:ci", "k"),
            group: "infra".into(),
            title: "t".into(),
            source: "producer:ci".into(),
            key: "k".into(),
            created_at: 5,
            deliveries: vec![
                WireDelivery {
                    device: "d1".into(),
                    received_at: None,
                },
                WireDelivery {
                    device: "d2".into(),
                    received_at: None,
                },
            ],
        };
        assert!(matches!(s.merge(wire.clone()), Accepted::Created(_)));
        assert_eq!(s.pending_for("d1", 10, 0).len(), 1);
        // The peer sends it again with a receipt: stored once, the receipt is merged.
        let mut again = wire.clone();
        again.deliveries[0].received_at = Some(9);
        let Accepted::Duplicate(e) = s.merge(again) else {
            panic!("duplicate")
        };
        assert_eq!(e.deliveries[0].received_at, Some(9));
        assert_eq!(s.len(), 1);
        assert!(s.pending_for("d1", 10, 0).is_empty());
        // A peer that knew another device adds it.
        let mut more = wire.clone();
        more.deliveries.push(WireDelivery {
            device: "d3".into(),
            received_at: None,
        });
        s.merge(more);
        assert_eq!(s.pending_for("d3", 10, 0).len(), 1);
        // Same key, different title.
        let mut other = wire;
        other.title = "other".into();
        assert!(matches!(s.merge(other), Accepted::Conflict));
    }

    #[test]
    fn a_remote_receipt_is_applied_once() {
        let s = Spool::default();
        let Accepted::Created(e) = accept(&s, "k", "t", &["d1"]) else {
            panic!()
        };
        assert!(s.ack_remote("d1", &e.id, 7));
        assert!(!s.ack_remote("d1", &e.id, 8));
        assert!(!s.ack_remote("d9", &e.id, 8));
        assert_eq!(s.snapshot()[0].deliveries[0].received_at, Some(7));
    }

    #[test]
    fn the_replay_keeps_an_entry_that_changed_while_it_was_copied() {
        let s = Spool::default();
        let Accepted::Created(e) = accept(&s, "k", "t", &["d1"]) else {
            panic!()
        };
        let copied = s.snapshot().remove(0);
        assert_eq!(copied.rev, e.rev);
        s.ack("d1", &e.id, 3);
        assert!(
            !s.remove_if_unchanged(&e.id, copied.rev),
            "a receipt arrived meanwhile"
        );
        let now = s.snapshot().remove(0);
        assert!(s.remove_if_unchanged(&e.id, now.rev));
        assert_eq!(s.len(), 0);
        assert!(!s.has_work());
    }

    #[test]
    fn the_spool_is_bounded() {
        let s = Spool::default();
        for i in 0..MAX_SPOOLED {
            assert!(matches!(
                accept(&s, &format!("k{i}"), "t", &[]),
                Accepted::Created(_)
            ));
        }
        assert!(matches!(accept(&s, "one more", "t", &[]), Accepted::Full));
        // A duplicate is still answered when the spool is full.
        assert!(matches!(accept(&s, "k0", "t", &[]), Accepted::Duplicate(_)));
    }

    #[test]
    fn receipts_for_the_database_wait_without_duplicates() {
        let s = Spool::default();
        s.note_db_ack("d1", "a1", 1);
        s.note_db_ack("d1", "a1", 2);
        s.note_db_ack("d2", "a1", 3);
        assert!(s.has_work());
        let acks = s.take_db_acks();
        assert_eq!(acks.len(), 2);
        assert!(!s.has_work());
        s.restore_db_acks(acks);
        assert_eq!(s.take_db_acks().len(), 2);
    }
}
