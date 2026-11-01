//! The metrics of the inbox, in the Prometheus text format.
//!
//! The portal serves them at its metrics endpoint, after its own metrics. The names are the names
//! that the inbox has always exported: `inbox_<noun>_<unit|total>`. A metric never depends on an
//! export: if nothing scrapes the endpoint, the inbox keeps delivering alerts.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering::Relaxed};
use std::sync::Mutex;

use super::replica::Stats as ReplicaStats;
use super::util::now_ms;

/// The upper bounds, in seconds, of the histogram of the delivery latency.
const LATENCY_BOUNDS: [f64; 11] = [
    0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 15.0, 60.0, 300.0, 3600.0,
];

/// A counter with one label.
#[derive(Default)]
struct Labeled(Mutex<BTreeMap<&'static str, u64>>);

impl Labeled {
    fn add(&self, label: &'static str, n: u64) {
        *self
            .0
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .entry(label)
            .or_insert(0) += n;
    }

    fn render(&self, out: &mut String, name: &str, help: &str, label: &str) {
        header(out, name, help, "counter");
        for (value, count) in self.0.lock().unwrap_or_else(|p| p.into_inner()).iter() {
            let _ = writeln!(out, "{name}{{{label}=\"{value}\"}} {count}");
        }
    }
}

#[derive(Default)]
struct Histogram {
    /// The count of the observations at or below each bound, not cumulative.
    buckets: [u64; LATENCY_BOUNDS.len()],
    count: u64,
    sum: f64,
}

/// The metrics of the inbox. Each method is one event.
#[derive(Default)]
pub struct InboxMetrics {
    alerts_accepted: Labeled,
    alerts_duplicate: Labeled,
    alerts_spooled: Labeled,
    auth_failures: Labeled,
    session_closes: Labeled,
    deliveries_created: AtomicU64,
    deliveries_sent: AtomicU64,
    receipts: AtomicU64,
    enrollments: AtomicU64,
    replica_sync_failures: AtomicU64,
    spool_rejected: AtomicU64,
    latency: Mutex<Histogram>,
    /// The open sessions of this instance.
    sessions: AtomicI64,
    // The gauges that the task of the statistics sets.
    pub pending_deliveries: AtomicI64,
    pub oldest_pending_ms: AtomicI64,
    pub unreachable_devices: AtomicI64,
    pub enrolled_devices: AtomicI64,
    /// 1 while PostgreSQL has no usable primary and this instance runs on its memory.
    pub storage_degraded: AtomicI64,
    /// The alerts that the memory holds and the database does not hold yet.
    pub spool_alerts: AtomicI64,
}

impl InboxMetrics {
    pub fn alert_accepted(&self, source: &'static str) {
        self.alerts_accepted.add(source, 1);
    }

    pub fn alert_duplicate(&self, source: &'static str) {
        self.alerts_duplicate.add(source, 1);
    }

    pub fn alert_spooled(&self, source: &'static str) {
        self.alerts_spooled.add(source, 1);
    }

    pub fn auth_failure(&self, kind: &'static str) {
        self.auth_failures.add(kind, 1);
    }

    pub fn session_closed(&self, reason: &'static str) {
        self.session_closes.add(reason, 1);
    }

    pub fn deliveries_created(&self, n: u64) {
        self.deliveries_created.fetch_add(n, Relaxed);
    }

    pub fn deliveries_sent(&self, n: u64) {
        self.deliveries_sent.fetch_add(n, Relaxed);
    }

    pub fn receipt(&self) {
        self.receipts.fetch_add(1, Relaxed);
    }

    pub fn enrollment(&self) {
        self.enrollments.fetch_add(1, Relaxed);
    }

    pub fn replica_sync_failure(&self) {
        self.replica_sync_failures.fetch_add(1, Relaxed);
    }

    pub fn spool_rejected(&self) {
        self.spool_rejected.fetch_add(1, Relaxed);
    }

    pub fn session_opened(&self) {
        self.sessions.fetch_add(1, Relaxed);
    }

    pub fn session_ended(&self) {
        self.sessions.fetch_sub(1, Relaxed);
    }

    /// Records the time from the acceptance of an alert to its first receipt.
    pub fn delivery_latency(&self, seconds: f64) {
        let mut h = self.latency.lock().unwrap_or_else(|p| p.into_inner());
        h.count += 1;
        h.sum += seconds;
        if let Some(i) = LATENCY_BOUNDS.iter().position(|bound| seconds <= *bound) {
            h.buckets[i] += 1;
        }
    }

    /// The text of all metrics of the inbox. `replica` is the state of the replication, when
    /// the block `inbox.replica` is present.
    pub fn render(&self, replica: Option<&ReplicaStats>) -> String {
        let mut out = String::new();
        self.alerts_accepted.render(
            &mut out,
            "inbox_alerts_accepted_total",
            "Alerts durably accepted, by source kind",
            "source",
        );
        self.alerts_duplicate.render(
            &mut out,
            "inbox_alerts_duplicate_total",
            "Alert submissions deduplicated by idempotency key",
            "source",
        );
        counter(
            &mut out,
            "inbox_deliveries_created_total",
            "Per-device delivery rows created by fan-out",
            self.deliveries_created.load(Relaxed),
        );
        counter(
            &mut out,
            "inbox_deliveries_sent_total",
            "Alert frames sent to devices, including retransmissions",
            self.deliveries_sent.load(Relaxed),
        );
        counter(
            &mut out,
            "inbox_delivery_receipts_total",
            "Delivery receipts sent by devices",
            self.receipts.load(Relaxed),
        );
        self.render_latency(&mut out);
        self.auth_failures.render(
            &mut out,
            "inbox_auth_failures_total",
            "Failed authentications, by kind",
            "kind",
        );
        counter(
            &mut out,
            "inbox_enrollments_total",
            "Devices enrolled",
            self.enrollments.load(Relaxed),
        );
        gauge(
            &mut out,
            "inbox_device_sessions",
            "Established device sessions",
            self.sessions.load(Relaxed),
        );
        self.session_closes.render(
            &mut out,
            "inbox_device_session_closes_total",
            "Device sessions closed, by reason",
            "reason",
        );
        counter(
            &mut out,
            "inbox_replica_sync_failures_total",
            "Writes that could not be copied to the replica before the answer",
            self.replica_sync_failures.load(Relaxed),
        );
        self.alerts_spooled.render(
            &mut out,
            "inbox_alerts_spooled_total",
            "Alerts accepted without the database, by source kind",
            "source",
        );
        counter(
            &mut out,
            "inbox_spool_rejected_total",
            "Alerts refused because the spool was full",
            self.spool_rejected.load(Relaxed),
        );
        gauge(
            &mut out,
            "inbox_deliveries_pending",
            "Deliveries to non-revoked devices not yet received",
            self.pending_deliveries.load(Relaxed),
        );
        gauge_f64(
            &mut out,
            "inbox_oldest_pending_delivery_seconds",
            "Age of the oldest alert still waiting for a device",
            self.oldest_pending_ms.load(Relaxed) as f64 / 1000.0,
        );
        gauge(
            &mut out,
            "inbox_devices_unreachable",
            "Subscribed devices not seen within inbox.devices.unreachable_after",
            self.unreachable_devices.load(Relaxed),
        );
        gauge(
            &mut out,
            "inbox_storage_degraded",
            "1 while the database has no usable primary and alerts are held in memory",
            self.storage_degraded.load(Relaxed),
        );
        gauge(
            &mut out,
            "inbox_spool_alerts",
            "Alerts accepted without the database and not yet written to it",
            self.spool_alerts.load(Relaxed),
        );
        gauge(
            &mut out,
            "inbox_devices_enrolled",
            "Non-revoked enrolled devices",
            self.enrolled_devices.load(Relaxed),
        );
        if let Some(stats) = replica {
            gauge(
                &mut out,
                "inbox_replica_epoch",
                "Epoch of the replica lease owned by this instance",
                stats.epoch.load(Relaxed) as i64,
            );
            let at = stats.last_snapshot_ms.load(Relaxed);
            if at > 0 {
                gauge_f64(
                    &mut out,
                    "inbox_replica_last_snapshot_age_seconds",
                    "Age of the newest snapshot in the shared directory",
                    (now_ms() - at) as f64 / 1000.0,
                );
            }
            counter(
                &mut out,
                "inbox_replica_snapshots_total",
                "Snapshots published to the shared directory",
                stats.snapshots.load(Relaxed),
            );
            counter(
                &mut out,
                "inbox_replica_snapshot_failures_total",
                "Snapshots that failed",
                stats.failures.load(Relaxed),
            );
        }
        out
    }

    fn render_latency(&self, out: &mut String) {
        let name = "inbox_delivery_latency_seconds";
        header(
            out,
            name,
            "Time from alert acceptance to first device receipt",
            "histogram",
        );
        let h = self.latency.lock().unwrap_or_else(|p| p.into_inner());
        let mut cumulative = 0;
        for (bound, count) in LATENCY_BOUNDS.iter().zip(h.buckets.iter()) {
            cumulative += count;
            let _ = writeln!(out, "{name}_bucket{{le=\"{bound}\"}} {cumulative}");
        }
        let _ = writeln!(out, "{name}_bucket{{le=\"+Inf\"}} {}", h.count);
        let _ = writeln!(out, "{name}_sum {}", h.sum);
        let _ = writeln!(out, "{name}_count {}", h.count);
    }
}

fn header(out: &mut String, name: &str, help: &str, kind: &str) {
    let _ = writeln!(out, "# HELP {name} {help}\n# TYPE {name} {kind}");
}

fn counter(out: &mut String, name: &str, help: &str, value: u64) {
    header(out, name, help, "counter");
    let _ = writeln!(out, "{name} {value}");
}

fn gauge(out: &mut String, name: &str, help: &str, value: i64) {
    header(out, name, help, "gauge");
    let _ = writeln!(out, "{name} {value}");
}

fn gauge_f64(out: &mut String, name: &str, help: &str, value: f64) {
    header(out, name, help, "gauge");
    let _ = writeln!(out, "{name} {value}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_text_has_every_metric_of_the_documentation() {
        let metrics = InboxMetrics::default();
        metrics.alert_accepted("producer");
        metrics.alert_accepted("producer");
        metrics.auth_failure("device_key");
        metrics.delivery_latency(0.2);
        metrics.delivery_latency(7000.0);
        metrics.unreachable_devices.store(2, Relaxed);
        metrics.oldest_pending_ms.store(1500, Relaxed);
        let text = metrics.render(None);
        for expected in [
            "inbox_alerts_accepted_total{source=\"producer\"} 2",
            "inbox_auth_failures_total{kind=\"device_key\"} 1",
            "inbox_devices_unreachable 2",
            "inbox_oldest_pending_delivery_seconds 1.5",
            "inbox_delivery_latency_seconds_bucket{le=\"0.25\"} 1",
            "inbox_delivery_latency_seconds_bucket{le=\"+Inf\"} 2",
            "inbox_delivery_latency_seconds_count 2",
            "# TYPE inbox_storage_degraded gauge",
            "# TYPE inbox_spool_alerts gauge",
            "inbox_deliveries_pending 0",
            "inbox_device_sessions 0",
        ] {
            assert!(text.contains(expected), "{expected}\n{text}");
        }
        assert!(
            !text.contains("inbox_replica_"),
            "no replica metrics without a replica"
        );
    }

    #[test]
    fn the_replica_metrics_appear_with_a_replica() {
        let metrics = InboxMetrics::default();
        let stats = ReplicaStats::default();
        stats.epoch.store(3, Relaxed);
        let text = metrics.render(Some(&stats));
        assert!(text.contains("inbox_replica_epoch 3"), "{text}");
        assert!(text.contains("inbox_replica_snapshots_total 0"), "{text}");
    }
}
