//! The alerts that the portal sends about itself: a service of the launcher that stays
//! unhealthy, and a burst of refused sign-ins.
//!
//! The block `inbox.watch` turns this on. Without the block, nothing runs.
//!
//! Two small state machines decide when an alert is due. They read a clock that the caller gives,
//! so a test can move the time. The [`Engine`] joins them to the configuration, and it builds the
//! title and the idempotency key of each alert. [`run`] is the background task: it reads the
//! catalogue and the counter of refused sign-ins once each second, and it gives each due alert to
//! the inbox. The inbox accepts it with the same guarantees as an alert of the API (source
//! `portal`).
//!
//! The task never blocks the portal. The sign-in path only adds to an atomic counter that
//! already exists. A failed send is logged and tried again later. The state of the detectors is
//! in memory: a restart of the portal forgets it.
//!
//! With several instances, each instance detects the same event. The idempotency key holds only
//! shared facts: the kind, the id of the service and the time bucket of `repeat_after`. The
//! inbox accepts one alert for one key, so the phones get one alert.

use std::collections::{HashMap, VecDeque};
use std::fmt::Write as _;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};

use crate::catalog::{Health, RuntimeHealth, Service};
use crate::config::WatchConfig;
use crate::http::AppState;
use crate::inbox::log;

/// How often the task reads the catalogue and the counter.
const TICK: Duration = Duration::from_secs(1);
/// The time that the task waits after a failed send, before it tries again.
const RETRY_AFTER: Duration = Duration::from_secs(10);
/// The longest time that one send can take.
const SEND_LIMIT: Duration = Duration::from_secs(20);
/// The longest idempotency key that the inbox accepts.
const MAX_KEY_BYTES: usize = 128;
/// The longest part of a service name in a title.
const MAX_NAME_CHARS: usize = 64;

/// The kind of an alert. It is also the label of the metrics.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    ServiceUnhealthy,
    ServiceRecovered,
    SignInBurst,
}

impl Kind {
    const ALL: [Kind; 3] = [
        Kind::ServiceUnhealthy,
        Kind::ServiceRecovered,
        Kind::SignInBurst,
    ];

    fn label(self) -> &'static str {
        match self {
            Kind::ServiceUnhealthy => "service_unhealthy",
            Kind::ServiceRecovered => "service_recovered",
            Kind::SignInBurst => "sign_in_burst",
        }
    }

    fn index(self) -> usize {
        match self {
            Kind::ServiceUnhealthy => 0,
            Kind::ServiceRecovered => 1,
            Kind::SignInBurst => 2,
        }
    }
}

/// The counters `portal_watch_sent_total` and `portal_watch_failed_total`.
#[derive(Default)]
pub struct Stats {
    sent: [AtomicU64; 3],
    failed: [AtomicU64; 3],
}

impl Stats {
    fn sent(&self, kind: Kind) {
        self.sent[kind.index()].fetch_add(1, Ordering::Relaxed);
    }

    fn failed(&self, kind: Kind) {
        self.failed[kind.index()].fetch_add(1, Ordering::Relaxed);
    }

    /// The Prometheus text. It is empty when the block `inbox.watch` is absent.
    pub fn render(&self, enabled: bool) -> String {
        if !enabled {
            return String::new();
        }
        let mut out = String::new();
        for (name, help, counters) in [
            (
                "portal_watch_sent_total",
                "Alerts about the portal that the inbox accepted, by kind.",
                &self.sent,
            ),
            (
                "portal_watch_failed_total",
                "Alerts about the portal that the inbox did not accept, by kind. The portal tries again.",
                &self.failed,
            ),
        ] {
            let _ = writeln!(out, "# HELP {name} {help}\n# TYPE {name} counter");
            for kind in Kind::ALL {
                let _ = writeln!(
                    out,
                    "{name}{{kind=\"{}\"}} {}",
                    kind.label(),
                    counters[kind.index()].load(Ordering::Relaxed)
                );
            }
        }
        out
    }
}

/// What the launcher shows for a service. The rules are the rules of the tile.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Seen {
    Healthy,
    Starting,
    Unhealthy,
    /// No probe gives a state. The portal never alerts for such a service.
    Unknown,
}

/// The state of a service, as its tile shows it: `down` and `unhealthy` are unhealthy.
pub fn seen(service: &Service) -> Seen {
    match (service.health, service.runtime) {
        (Health::Down, _) | (_, Some(RuntimeHealth::Unhealthy)) => Seen::Unhealthy,
        (_, Some(RuntimeHealth::Starting)) => Seen::Starting,
        (_, Some(RuntimeHealth::Healthy)) | (Health::Up, None) => Seen::Healthy,
        (Health::Unknown, None) => Seen::Unknown,
    }
}

/// A service at one reading of the catalogue.
#[derive(Clone, Debug)]
pub struct Observed {
    pub id: String,
    pub name: String,
    pub seen: Seen,
}

/// An alert that is due. It is due again at each reading until [`Engine::confirm`] tells that the
/// inbox accepted it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Due {
    pub kind: Kind,
    /// The id of the service. It is empty for a burst.
    pub subject: String,
    /// The name of the service. It is empty for a burst.
    pub name: String,
}

#[derive(Default)]
struct Track {
    /// When the service became unhealthy, if it is unhealthy now without a break.
    unhealthy_since: Option<Duration>,
    /// The portal sent an alert about this service and did not send the recovery yet.
    alerted: bool,
    last_alert: Option<Duration>,
}

/// Decides when a service needs an alert. The time is a monotonic time that the caller measures.
pub struct HealthDetector {
    after: Duration,
    repeat: Duration,
    recovered: bool,
    tracks: HashMap<String, Track>,
}

impl HealthDetector {
    pub fn new(after: Duration, repeat: Duration, recovered: bool) -> HealthDetector {
        HealthDetector {
            after,
            repeat,
            recovered,
            tracks: HashMap::new(),
        }
    }

    /// Reads the services at time `now`. A service that is not in `services` is forgotten.
    /// A service that is starting, or that has no state, breaks the unhealthy time: the
    /// portal alerts only for a service that is unhealthy for the whole time.
    pub fn observe(&mut self, now: Duration, services: &[Observed]) -> Vec<Due> {
        self.tracks
            .retain(|id, _| services.iter().any(|s| &s.id == id));
        let mut due = Vec::new();
        for service in services {
            let track = self.tracks.entry(service.id.clone()).or_default();
            // A clock that goes back never gives a negative time. It restarts the count.
            if let Some(since) = &mut track.unhealthy_since {
                *since = (*since).min(now);
            }
            if let Some(last) = &mut track.last_alert {
                *last = (*last).min(now);
            }
            match service.seen {
                Seen::Unhealthy => {
                    let since = *track.unhealthy_since.get_or_insert(now);
                    let long_enough = now - since >= self.after;
                    let spaced = track
                        .last_alert
                        .is_none_or(|last| now - last >= self.repeat);
                    if long_enough && spaced {
                        due.push(Due {
                            kind: Kind::ServiceUnhealthy,
                            subject: service.id.clone(),
                            name: service.name.clone(),
                        });
                    }
                }
                Seen::Starting | Seen::Unknown => track.unhealthy_since = None,
                Seen::Healthy => {
                    track.unhealthy_since = None;
                    if track.alerted {
                        if self.recovered {
                            due.push(Due {
                                kind: Kind::ServiceRecovered,
                                subject: service.id.clone(),
                                name: service.name.clone(),
                            });
                        } else {
                            track.alerted = false;
                        }
                    }
                }
            }
        }
        due.sort_by(|a, b| a.subject.cmp(&b.subject));
        due
    }

    /// The inbox accepted `due` at time `now`.
    pub fn confirm(&mut self, due: &Due, now: Duration) {
        let Some(track) = self.tracks.get_mut(&due.subject) else {
            return;
        };
        match due.kind {
            Kind::ServiceUnhealthy => {
                track.alerted = true;
                track.last_alert = Some(now);
            }
            Kind::ServiceRecovered => track.alerted = false,
            Kind::SignInBurst => {}
        }
    }
}

/// Decides when the refused sign-ins make a burst. The window slides.
pub struct BurstDetector {
    failures: u64,
    window: Duration,
    repeat: Duration,
    /// The refusals that the last reading saw, by time of the reading.
    events: VecDeque<(Duration, u64)>,
    total: u64,
    last_alert: Option<Duration>,
}

impl BurstDetector {
    pub fn new(failures: u32, window: Duration, repeat: Duration) -> BurstDetector {
        BurstDetector {
            failures: u64::from(failures),
            window,
            repeat,
            events: VecDeque::new(),
            total: 0,
            last_alert: None,
        }
    }

    /// Reads `refused` new refusals at time `now`. The result is true when a burst is due. It is
    /// true again at each reading until [`BurstDetector::confirm`].
    pub fn observe(&mut self, now: Duration, refused: u64) -> bool {
        // A clock that goes back never gives a negative age.
        for (at, _) in self.events.iter_mut().rev() {
            if *at <= now {
                break;
            }
            *at = now;
        }
        if let Some(last) = &mut self.last_alert {
            *last = (*last).min(now);
        }
        if refused > 0 {
            match self.events.back_mut() {
                Some((at, count)) if *at == now => *count = count.saturating_add(refused),
                _ => self.events.push_back((now, refused)),
            }
            self.total = self.total.saturating_add(refused);
        }
        while let Some(&(at, count)) = self.events.front() {
            if now - at < self.window {
                break;
            }
            self.events.pop_front();
            self.total = self.total.saturating_sub(count);
        }
        self.total >= self.failures && self.last_alert.is_none_or(|last| now - last >= self.repeat)
    }

    /// The inbox accepted the alert at time `now`. The portal counts again from zero, so the
    /// same refusals never make a second alert.
    pub fn confirm(&mut self, now: Duration) {
        self.last_alert = Some(now);
        self.events.clear();
        self.total = 0;
    }
}

/// An alert that is ready for the inbox.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Outgoing {
    pub due: Due,
    pub title: String,
    pub key: String,
}

impl Outgoing {
    /// The id of the service that the alert names. The inbox shows the alert only to the readers
    /// that may open this service. A burst of refused sign-ins names no service.
    pub fn service(&self) -> Option<String> {
        match self.due.kind {
            Kind::ServiceUnhealthy | Kind::ServiceRecovered => Some(self.due.subject.clone()),
            Kind::SignInBurst => None,
        }
    }
}

/// The two detectors, the configuration and the memory of the counter.
pub struct Engine {
    config: WatchConfig,
    health: HealthDetector,
    burst: BurstDetector,
    seen_refusals: u64,
}

impl Engine {
    pub fn new(config: WatchConfig) -> Engine {
        Engine {
            health: HealthDetector::new(
                config.unhealthy_after,
                config.repeat_after,
                config.recovered,
            ),
            burst: BurstDetector::new(
                config.sign_in_failures,
                config.sign_in_window,
                config.repeat_after,
            ),
            seen_refusals: 0,
            config,
        }
    }

    /// One reading. `refusals` is the total of the refused sign-ins since the start of the
    /// process. `wall_secs` is the Unix time: it gives the time bucket of the key.
    pub fn tick(
        &mut self,
        now: Duration,
        wall_secs: u64,
        refusals: u64,
        services: &[Observed],
    ) -> Vec<Outgoing> {
        let new = refusals.saturating_sub(self.seen_refusals);
        self.seen_refusals = refusals;
        let mut due = Vec::new();
        if self.config.sign_in_burst && self.burst.observe(now, new) {
            due.push(Due {
                kind: Kind::SignInBurst,
                subject: String::new(),
                name: String::new(),
            });
        }
        if self.config.service_unhealthy {
            due.extend(self.health.observe(now, services));
        }
        due.into_iter()
            .map(|due| Outgoing {
                title: title(&due, &self.config),
                key: key(&due, wall_secs, self.config.repeat_after),
                due,
            })
            .collect()
    }

    /// The inbox accepted `outgoing` at time `now`.
    pub fn confirm(&mut self, outgoing: &Outgoing, now: Duration) {
        match outgoing.due.kind {
            Kind::SignInBurst => self.burst.confirm(now),
            _ => self.health.confirm(&outgoing.due, now),
        }
    }
}

/// "1 minute", "2 minutes", "90 seconds", "3 hours", "1 day".
fn words(d: Duration) -> String {
    let secs = d.as_secs();
    let (n, unit) = if secs != 0 && secs.is_multiple_of(86_400) {
        (secs / 86_400, "day")
    } else if secs != 0 && secs.is_multiple_of(3600) {
        (secs / 3600, "hour")
    } else if secs != 0 && secs.is_multiple_of(60) {
        (secs / 60, "minute")
    } else {
        (secs, "second")
    };
    format!("{n} {unit}{}", if n == 1 { "" } else { "s" })
}

/// The name of a service on one line, short enough for a title.
fn one_line(name: &str, fallback: &str) -> String {
    let spaced: String = name
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let line = spaced.split_whitespace().collect::<Vec<_>>().join(" ");
    let line = if line.is_empty() {
        fallback.to_string()
    } else {
        line
    };
    if line.chars().count() > MAX_NAME_CHARS {
        let cut: String = line.chars().take(MAX_NAME_CHARS - 3).collect();
        format!("{cut}...")
    } else {
        line
    }
}

/// The title of an alert: one line of plain English. It holds no login, no address and no
/// password. It holds only values of the configuration and the name of a service, so each
/// instance builds the same title for the same key.
pub fn title(due: &Due, config: &WatchConfig) -> String {
    match due.kind {
        Kind::ServiceUnhealthy => format!(
            "Service {} is unhealthy for {}",
            one_line(&due.name, &due.subject),
            words(config.unhealthy_after)
        ),
        Kind::ServiceRecovered => {
            format!(
                "Service {} is healthy again",
                one_line(&due.name, &due.subject)
            )
        }
        Kind::SignInBurst => format!(
            "{} refused sign-ins in {}",
            config.sign_in_failures,
            words(config.sign_in_window)
        ),
    }
}

/// The idempotency key of an alert. It holds the kind, the id of the service and the time
/// bucket of `repeat`. It holds no value that differs between two instances.
pub fn key(due: &Due, wall_secs: u64, repeat: Duration) -> String {
    let bucket = wall_secs / repeat.as_secs().max(1);
    let kind = match due.kind {
        Kind::ServiceUnhealthy => "unhealthy",
        Kind::ServiceRecovered => "recovered",
        Kind::SignInBurst => return format!("portal:burst:{bucket}"),
    };
    let key = format!("portal:{kind}:{}:{bucket}", due.subject);
    if key.len() <= MAX_KEY_BYTES {
        return key;
    }
    // An id that makes the key too long gives its digest.
    let digest = Sha256::digest(due.subject.as_bytes());
    let short: String = digest.iter().take(16).fold(String::new(), |mut out, b| {
        let _ = write!(out, "{b:02x}");
        out
    });
    format!("portal:{kind}:{short}:{bucket}")
}

fn observed(services: &crate::catalog::Catalog) -> Vec<Observed> {
    services
        .groups()
        .iter()
        .flat_map(|group| group.services.iter())
        // An external service has no health: it gives no alert.
        .filter(|service| !service.external)
        .map(|service| Observed {
            id: service.id.clone(),
            name: service.name.clone(),
            seen: seen(service),
        })
        .collect()
}

fn unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// The background task. It ends when the portal stops. It returns at once without the block
/// `inbox.watch`.
pub async fn run(state: Arc<AppState>) {
    let Some(config) = state
        .config
        .inbox
        .as_ref()
        .and_then(|inbox| inbox.watch.clone())
    else {
        return;
    };
    let group = config.group.clone();
    log::info!(
        event = "watch_started",
        group = %group,
        service_unhealthy = config.service_unhealthy,
        sign_in_burst = config.sign_in_burst,
    );
    let mut engine = Engine::new(config);
    let started = Instant::now();
    let mut tick = tokio::time::interval(TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut retry_not_before: Option<Instant> = None;
    loop {
        tick.tick().await;
        let now = started.elapsed();
        let services = observed(&crate::http::current_catalog_of(&state));
        let due = engine.tick(
            now,
            unix_secs(),
            state.metrics.refused_sign_ins(),
            &services,
        );
        if due.is_empty() || retry_not_before.is_some_and(|at| Instant::now() < at) {
            continue;
        }
        retry_not_before = None;
        for outgoing in due {
            let kind = outgoing.due.kind;
            let sent = tokio::time::timeout(
                SEND_LIMIT,
                state.inbox.send_watch_alert(
                    &group,
                    outgoing.title.clone(),
                    outgoing.key.clone(),
                    outgoing.service(),
                ),
            )
            .await
            .unwrap_or_else(|_| Err("the inbox did not answer in time".into()));
            match sent {
                Ok(duplicate) => {
                    state.alerts.sent(kind);
                    engine.confirm(&outgoing, started.elapsed());
                    log::info!(
                        event = "watch_sent",
                        kind = kind.label(),
                        subject = %outgoing.due.subject,
                        duplicate,
                    );
                }
                Err(error) => {
                    state.alerts.failed(kind);
                    retry_not_before = Some(Instant::now() + RETRY_AFTER);
                    log::error!(
                        event = "watch_failed",
                        kind = kind.label(),
                        subject = %outgoing.due.subject,
                        error = %error,
                        "the portal tries again later"
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
