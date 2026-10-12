//! Logging and OpenTelemetry (traces, metrics, logs over OTLP/HTTP protobuf).
//!
//! Telemetry is never a dependency of the service: if the collector is down,
//! exports fail in the background and Inbox keeps delivering alerts.
//!
//! The blocking OTLP HTTP client must be created outside the Tokio runtime, so
//! [`init`] runs before the runtime starts and [`Guard::shutdown`] after it stops.

use std::time::Duration;

use opentelemetry::KeyValue;
use opentelemetry::metrics::{Counter, Histogram, Meter, UpDownCounter};
use opentelemetry::trace::TracerProvider as _;
use opentelemetry_otlp::{Protocol, WithExportConfig};
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::logs::SdkLoggerProvider;
use opentelemetry_sdk::metrics::{PeriodicReader, SdkMeterProvider};
use opentelemetry_sdk::trace::SdkTracerProvider;
use tracing_subscriber::Layer;
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

use crate::config::{Config, LogFormat, LogLevel};

pub struct Guard {
    tracer: Option<SdkTracerProvider>,
    meter: Option<SdkMeterProvider>,
    logger: Option<SdkLoggerProvider>,
}

impl Guard {
    /// Flushes and stops the exporters. Call outside the Tokio runtime.
    pub fn shutdown(self) {
        let timeout = Duration::from_secs(5);
        if let Some(t) = self.tracer {
            let _ = t.shutdown_with_timeout(timeout);
        }
        if let Some(m) = self.meter {
            let _ = m.shutdown_with_timeout(timeout);
        }
        if let Some(l) = self.logger {
            let _ = l.shutdown_with_timeout(timeout);
        }
    }
}

fn level(l: LogLevel) -> LevelFilter {
    match l {
        LogLevel::Error => LevelFilter::ERROR,
        LogLevel::Warn => LevelFilter::WARN,
        LogLevel::Info => LevelFilter::INFO,
        LogLevel::Debug => LevelFilter::DEBUG,
        LogLevel::Trace => LevelFilter::TRACE,
    }
}

/// Installs the global subscriber, and the OTLP pipelines when enabled.
pub fn init(config: &Config) -> Result<(Guard, Meter), String> {
    let filter = level(config.logging.level);
    let fmt = match config.logging.format {
        LogFormat::Json => tracing_subscriber::fmt::layer()
            .json()
            .flatten_event(true)
            .with_current_span(false)
            .with_span_list(false)
            .with_writer(std::io::stderr)
            .boxed(),
        LogFormat::Text => tracing_subscriber::fmt::layer().with_writer(std::io::stderr).boxed(),
    };

    let t = &config.telemetry;
    if !t.enabled {
        tracing_subscriber::registry()
            .with(fmt.with_filter(filter))
            .try_init()
            .map_err(|e| e.to_string())?;
        let meter = opentelemetry::global::meter("inbox");
        return Ok((Guard { tracer: None, meter: None, logger: None }, meter));
    }

    let endpoint = t.otlp_endpoint.as_deref().expect("validated").trim_end_matches('/').to_string();
    let service = t.service_name.clone().expect("validated");
    let interval = t.export_interval.expect("validated");
    let resource = Resource::builder_empty()
        .with_service_name(service)
        .with_attribute(KeyValue::new("service.version", env!("CARGO_PKG_VERSION")))
        .build();
    let err = |what: &str, e: &dyn std::fmt::Display| format!("telemetry: {what} exporter: {e}");

    let spans = opentelemetry_otlp::SpanExporter::builder()
        .with_http()
        .with_protocol(Protocol::HttpBinary)
        .with_endpoint(format!("{endpoint}/v1/traces"))
        .with_timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| err("trace", &e))?;
    let tracer = SdkTracerProvider::builder()
        .with_batch_exporter(spans)
        .with_resource(resource.clone())
        .build();

    let metrics = opentelemetry_otlp::MetricExporter::builder()
        .with_http()
        .with_protocol(Protocol::HttpBinary)
        .with_endpoint(format!("{endpoint}/v1/metrics"))
        .with_timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| err("metric", &e))?;
    let reader = PeriodicReader::builder(metrics).with_interval(interval).build();
    let meter_provider =
        SdkMeterProvider::builder().with_reader(reader).with_resource(resource.clone()).build();
    opentelemetry::global::set_meter_provider(meter_provider.clone());

    let logs = opentelemetry_otlp::LogExporter::builder()
        .with_http()
        .with_protocol(Protocol::HttpBinary)
        .with_endpoint(format!("{endpoint}/v1/logs"))
        .with_timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| err("log", &e))?;
    let logger =
        SdkLoggerProvider::builder().with_batch_exporter(logs).with_resource(resource).build();

    let otel_traces = tracing_opentelemetry::layer().with_tracer(tracer.tracer("inbox"));
    let otel_logs = opentelemetry_appender_tracing::layer::OpenTelemetryTracingBridge::new(&logger);
    tracing_subscriber::registry()
        .with(fmt.with_filter(filter))
        .with(otel_traces.with_filter(filter))
        .with(otel_logs.with_filter(filter))
        .try_init()
        .map_err(|e| e.to_string())?;

    let meter = opentelemetry::global::meter("inbox");
    Ok((Guard { tracer: Some(tracer), meter: Some(meter_provider), logger: Some(logger) }, meter))
}

/// Instruments. Names follow `inbox_<noun>_<unit|total>`.
#[derive(Clone)]
pub struct Metrics {
    pub alerts_accepted: Counter<u64>,
    pub alerts_duplicate: Counter<u64>,
    pub deliveries_created: Counter<u64>,
    pub deliveries_sent: Counter<u64>,
    pub acks: Counter<u64>,
    pub delivery_latency: Histogram<f64>,
    pub auth_failures: Counter<u64>,
    pub enrollments: Counter<u64>,
    pub sessions_active: UpDownCounter<i64>,
    pub session_closes: Counter<u64>,
    pub replica_sync_failures: Counter<u64>,
}

impl Metrics {
    pub fn new(meter: &Meter) -> Metrics {
        Metrics {
            alerts_accepted: meter
                .u64_counter("inbox_alerts_accepted_total")
                .with_description("Alerts durably accepted, by source kind")
                .build(),
            alerts_duplicate: meter
                .u64_counter("inbox_alerts_duplicate_total")
                .with_description("Alert submissions deduplicated by idempotency key")
                .build(),
            deliveries_created: meter
                .u64_counter("inbox_deliveries_created_total")
                .with_description("Per-device delivery rows created by fan-out")
                .build(),
            deliveries_sent: meter
                .u64_counter("inbox_deliveries_sent_total")
                .with_description("Alert frames sent to devices, including retransmissions")
                .build(),
            acks: meter
                .u64_counter("inbox_delivery_receipts_total")
                .with_description("Delivery receipts sent by devices")
                .build(),
            delivery_latency: meter
                .f64_histogram("inbox_delivery_latency_seconds")
                .with_description("Time from alert acceptance to first device receipt")
                .with_unit("s")
                .with_boundaries(vec![
                    0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 15.0, 60.0, 300.0, 3600.0,
                ])
                .build(),
            auth_failures: meter
                .u64_counter("inbox_auth_failures_total")
                .with_description("Failed authentications, by kind")
                .build(),
            enrollments: meter
                .u64_counter("inbox_enrollments_total")
                .with_description("Devices enrolled")
                .build(),
            sessions_active: meter
                .i64_up_down_counter("inbox_device_sessions")
                .with_description("Established device sessions")
                .build(),
            session_closes: meter
                .u64_counter("inbox_device_session_closes_total")
                .with_description("Device sessions closed, by reason")
                .build(),
            replica_sync_failures: meter
                .u64_counter("inbox_replica_sync_failures_total")
                .with_description(
                    "Writes that could not be copied to the replica before the answer",
                )
                .build(),
        }
    }
}

/// Observable gauges fed by the periodic statistics task.
#[derive(Default)]
pub struct Gauges {
    pub pending_deliveries: std::sync::atomic::AtomicI64,
    pub oldest_pending_ms: std::sync::atomic::AtomicI64,
    pub unreachable_devices: std::sync::atomic::AtomicI64,
    pub enrolled_devices: std::sync::atomic::AtomicI64,
}

pub fn register_gauges(meter: &Meter, gauges: std::sync::Arc<Gauges>) {
    use std::sync::atomic::Ordering::Relaxed;
    let g = gauges.clone();
    meter
        .i64_observable_gauge("inbox_deliveries_pending")
        .with_description("Deliveries to non-revoked devices not yet received")
        .with_callback(move |o| o.observe(g.pending_deliveries.load(Relaxed), &[]))
        .build();
    let g = gauges.clone();
    meter
        .f64_observable_gauge("inbox_oldest_pending_delivery_seconds")
        .with_description("Age of the oldest alert still waiting for a device")
        .with_unit("s")
        .with_callback(move |o| o.observe(g.oldest_pending_ms.load(Relaxed) as f64 / 1000.0, &[]))
        .build();
    let g = gauges.clone();
    meter
        .i64_observable_gauge("inbox_devices_unreachable")
        .with_description("Subscribed devices not seen within devices.unreachable_after")
        .with_callback(move |o| o.observe(g.unreachable_devices.load(Relaxed), &[]))
        .build();
    let g = gauges;
    meter
        .i64_observable_gauge("inbox_devices_enrolled")
        .with_description("Non-revoked enrolled devices")
        .with_callback(move |o| o.observe(g.enrolled_devices.load(Relaxed), &[]))
        .build();
}

/// Replication state: which epoch this instance owns and how fresh the shared copy is.
pub fn register_replica_gauges(meter: &Meter, stats: std::sync::Arc<crate::replica::Stats>) {
    use std::sync::atomic::Ordering::Relaxed;
    let s = stats.clone();
    meter
        .u64_observable_gauge("inbox_replica_epoch")
        .with_description("Epoch of the replica lease owned by this instance")
        .with_callback(move |o| o.observe(s.epoch.load(Relaxed), &[]))
        .build();
    let s = stats.clone();
    meter
        .f64_observable_gauge("inbox_replica_last_snapshot_age_seconds")
        .with_description("Age of the newest snapshot in the shared directory")
        .with_unit("s")
        .with_callback(move |o| {
            let at = s.last_snapshot_ms.load(Relaxed);
            if at > 0 {
                o.observe((crate::util::now_ms() - at) as f64 / 1000.0, &[]);
            }
        })
        .build();
    let s = stats.clone();
    meter
        .u64_observable_counter("inbox_replica_snapshots_total")
        .with_description("Snapshots published to the shared directory")
        .with_callback(move |o| o.observe(s.snapshots.load(Relaxed), &[]))
        .build();
    let s = stats;
    meter
        .u64_observable_counter("inbox_replica_snapshot_failures_total")
        .with_description("Snapshots that failed")
        .with_callback(move |o| o.observe(s.failures.load(Relaxed), &[]))
        .build();
}
