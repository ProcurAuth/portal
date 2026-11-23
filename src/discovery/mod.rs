//! Service discovery: Defines where the catalogue comes from and the state of each service.
//!
//! There are two sources. Both sources are optional. The sources are Kubernetes (Traefik
//! IngressRoute annotations) and Docker (container labels). Each refresh does these steps:
//!
//! 1. It lists the sources.
//! 2. It turns the labels of every workload into a [`Service`](crate::catalog::Service).
//! 3. It resolves a health state, and the runtime health when the runtime has a health check.
//! 4. It publishes a new immutable [`Catalog`] snapshot.
//!
//! The health state ([`Health`]) tells up or down. The runtime health
//! ([`RuntimeHealth`](crate::catalog::RuntimeHealth)) adds the result of the health check of the
//! runtime: the Docker `HEALTHCHECK`, or the readiness probes of the pods. It does not change the
//! health state.
//!
//! A source that fails keeps its last good answer. It adds a problem message to the snapshot.
//! A flaky API does not blank the page.
//!
//! A source that answers can still miss a workload that Traefik routes: a container that
//! restarts, or that Compose recreates, is absent from the list for a moment. The path of such a
//! workload keeps its last owner in the gate for [`KEEP_OWNER_ROUNDS`] refreshes ([`RecentOwners`]).
//! The gate thus never opens it to each signed-in user. A workload that no listing named yet has
//! no owner until the first refresh that lists it.
//!
//! The interface is [`run`] (the refresh loop) and [`Stats`] (for `/metrics`). The loop publishes
//! through a callback. Nothing in this module knows about the HTTP server.

mod docker;
mod kube;
mod probe;

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use crate::catalog::{
    closed_prefix, parse_labels, Catalog, GateEntry, Health, RuntimeHealth, Service,
};
use crate::config::DiscoveryConfig;

use docker::Docker;
use kube::{Kube, PodSet};

/// The most HTTP probes of services at the same time.
const MAX_CONCURRENT_CHECKS: usize = 64;

/// The number of refreshes that a gate prefix keeps its last owner after the last snapshot that
/// holds it: 5 minutes at the default `discovery.interval`.
const KEEP_OWNER_ROUNDS: u32 = 20;

/// The owners of the gate prefixes of recent snapshots. A prefix that a snapshot does not hold
/// keeps the owner of the last snapshot that held it, for [`KEEP_OWNER_ROUNDS`] snapshots. Only
/// the users that may open that service, and the superadmin, pass its path in this time. A closed
/// prefix stays closed in the same way.
#[derive(Default)]
struct RecentOwners {
    /// The last entry of each prefix, and the number of snapshots since the last one that held it.
    entries: std::collections::HashMap<Vec<String>, (GateEntry, u32)>,
}

impl RecentOwners {
    /// Records the gate of `catalog`, then gives it the recent owners of the prefixes that it
    /// does not hold.
    fn apply(&mut self, catalog: &mut Catalog) {
        for (_, missed) in self.entries.values_mut() {
            *missed += 1;
        }
        for entry in catalog.gate() {
            self.entries
                .insert(entry.prefix.clone(), (entry.clone(), 0));
        }
        self.entries
            .retain(|_, (_, missed)| *missed <= KEEP_OWNER_ROUNDS);
        let kept = self
            .entries
            .values()
            .filter(|(_, missed)| *missed > 0)
            .map(|(entry, _)| entry.clone())
            .collect();
        catalog.keep_owners(kept);
    }
}

/// The data that a source reports about one workload. It contains the labels and, when the source knows them, how to check whether the workload runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Workload {
    /// The `namespace/name` of a route, or the Compose service of a container.
    pub origin: String,
    pub labels: BTreeMap<String, String>,
    pub hint: Hint,
    /// The result of the `HEALTHCHECK` of a container (Docker). The Kubernetes source gives it
    /// later, from the pods of `hint`.
    pub runtime: Option<RuntimeHealth>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Hint {
    None,
    /// The state of the container (Docker).
    Running(bool),
    /// The ready pods that match an equality selector (Kubernetes).
    Pods {
        namespace: String,
        selector: String,
    },
}

/// The method that finds the health of one service.
#[derive(Debug, PartialEq, Eq)]
enum Check {
    None,
    Known(Health),
    Http(String),
    Pods { namespace: String, selector: String },
}

/// A declared health URL has priority. Otherwise, the source uses what it knows. With a declared
/// health URL, discovery does not ask for the pods, so the service has no runtime health from the
/// pods.
fn plan(service: &Service, hint: &Hint) -> Check {
    if let Some(url) = &service.health_url {
        return Check::Http(url.clone());
    }
    match hint {
        Hint::Running(true) => Check::Known(Health::Up),
        Hint::Running(false) => Check::Known(Health::Down),
        Hint::Pods {
            namespace,
            selector,
        } => Check::Pods {
            namespace: namespace.clone(),
            selector: selector.clone(),
        },
        Hint::None => Check::None,
    }
}

/// The counters for `/metrics`.
#[derive(Default)]
pub struct Stats {
    refreshes: AtomicU64,
    last_success_unix: AtomicU64,
    services: AtomicUsize,
    down: AtomicUsize,
    duplicates: AtomicUsize,
    complete: AtomicUsize,
    /// The number of items that the last lists hold: routes, containers and pods.
    items: AtomicUsize,
    errors: Mutex<BTreeMap<&'static str, u64>>,
}

impl Stats {
    fn source_error(&self, source: &'static str) {
        *self
            .errors
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .entry(source)
            .or_insert(0) += 1;
    }

    fn record(&self, catalog: &Catalog, healthy_refresh: bool) {
        self.refreshes.fetch_add(1, Ordering::Relaxed);
        self.services
            .store(catalog.service_count(), Ordering::Relaxed);
        self.down.store(
            catalog.groups().iter().map(|g| g.down_count()).sum(),
            Ordering::Relaxed,
        );
        self.duplicates
            .store(catalog.duplicates(), Ordering::Relaxed);
        self.complete
            .store(usize::from(catalog.is_complete()), Ordering::Relaxed);
        if healthy_refresh {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            self.last_success_unix.store(now, Ordering::Relaxed);
        }
    }

    /// The number of refreshes since the start.
    #[cfg(test)]
    pub fn refreshes(&self) -> u64 {
        self.refreshes.load(Ordering::Relaxed)
    }

    pub fn render(&self) -> String {
        let mut out = String::new();
        let gauge = |out: &mut String, name: &str, help: &str, value: u64| {
            let _ = write!(
                out,
                "# HELP {name} {help}\n# TYPE {name} gauge\n{name} {value}\n"
            );
        };
        gauge(
            &mut out,
            "portal_catalog_services",
            "Services in the current catalogue.",
            self.services.load(Ordering::Relaxed) as u64,
        );
        gauge(
            &mut out,
            "portal_catalog_services_down",
            "Services whose health check says down.",
            self.down.load(Ordering::Relaxed) as u64,
        );
        gauge(
            &mut out,
            "portal_catalog_duplicates",
            "Workloads ignored because their service id was already listed (replicas).",
            self.duplicates.load(Ordering::Relaxed) as u64,
        );
        gauge(
            &mut out,
            "portal_catalog_complete",
            "1 when every source answered at least once. At 0, the gate lets only the superadmin through.",
            self.complete.load(Ordering::Relaxed) as u64,
        );
        gauge(
            &mut out,
            "portal_catalog_items",
            "Items that the last lists hold: routes, containers and pods.",
            self.items.load(Ordering::Relaxed) as u64,
        );
        gauge(
            &mut out,
            "portal_discovery_last_success_timestamp_seconds",
            "Unix time of the last refresh where every source answered.",
            self.last_success_unix.load(Ordering::Relaxed),
        );
        let _ = write!(
            out,
            "# HELP portal_discovery_refreshes_total Discovery refreshes.\n# TYPE portal_discovery_refreshes_total counter\nportal_discovery_refreshes_total {}\n",
            self.refreshes.load(Ordering::Relaxed)
        );
        out.push_str("# HELP portal_discovery_errors_total Failed source listings, by source.\n# TYPE portal_discovery_errors_total counter\n");
        let errors = self.errors.lock().unwrap_or_else(|p| p.into_inner());
        for source in ["kubernetes", "docker"] {
            let _ = writeln!(
                out,
                "portal_discovery_errors_total{{source=\"{source}\"}} {}",
                errors.get(source).copied().unwrap_or(0)
            );
        }
        out
    }
}

/// The refresh loop. It runs until the process ends. Each refresh gives a new snapshot to `publish`.
pub async fn run(
    config: DiscoveryConfig,
    stats: Arc<Stats>,
    publish: impl Fn(Catalog) + Send + 'static,
) {
    let mut setup_problems: Vec<String> = Vec::new();
    let kube = config.kubernetes.as_ref().and_then(|api| {
        Kube::in_cluster(api, config.probe_timeout)
            .map_err(|e| setup_problems.push(format!("kubernetes: {e}")))
            .ok()
    });
    let docker = config.docker_socket.as_ref().and_then(|path| {
        Docker::new(path, config.probe_timeout)
            .map_err(|e| setup_problems.push(format!("docker: {e}")))
            .ok()
    });
    let client = match probe::build_client(config.probe_timeout) {
        Ok(client) => Some(client),
        Err(e) => {
            setup_problems.push(e);
            None
        }
    };

    // A source that failed at setup never answers. The snapshot then never becomes complete.
    let source_setup_failed = (config.kubernetes.is_some() && kube.is_none())
        || (config.docker_socket.is_some() && docker.is_none());
    let mut answered_kube = kube.is_none();
    let mut answered_docker = docker.is_none();
    let mut last_kube: Vec<Workload> = Vec::new();
    let mut last_docker: Vec<Workload> = Vec::new();
    let mut pods: Option<PodSet> = None;
    let mut recent = RecentOwners::default();
    loop {
        let mut problems = setup_problems.clone();
        let mut healthy = setup_problems.is_empty();
        if let Some(kube) = &kube {
            match kube.workloads().await {
                Ok(workloads) => {
                    last_kube = workloads;
                    answered_kube = true;
                }
                Err(e) => {
                    stats.source_error("kubernetes");
                    problems.push(format!("kubernetes: {e}"));
                    healthy = false;
                }
            }
            // The pods of all the selectors come from one list. A failed list gives an unknown
            // state to each service that has a selector.
            match kube.pod_set().await {
                Ok(set) => pods = Some(set),
                Err(e) => {
                    pods = None;
                    stats.source_error("kubernetes");
                    problems.push(format!("kubernetes: {e}"));
                    healthy = false;
                }
            }
        }
        if let Some(docker) = &docker {
            match docker.workloads().await {
                Ok(workloads) => {
                    last_docker = workloads;
                    answered_docker = true;
                }
                Err(e) => {
                    stats.source_error("docker");
                    problems.push(format!("docker: {e}"));
                    healthy = false;
                }
            }
        }
        let workloads = last_kube
            .iter()
            .chain(last_docker.iter())
            .cloned()
            .collect();
        stats.items.store(
            last_kube.len() + last_docker.len() + pods.as_ref().map_or(0, PodSet::len),
            Ordering::Relaxed,
        );
        let mut catalog = build_catalog(workloads, problems, pods.as_ref(), client.as_ref()).await;
        recent.apply(&mut catalog);
        catalog.set_complete(is_complete(
            source_setup_failed,
            answered_kube,
            answered_docker,
        ));
        stats.record(&catalog, healthy);
        publish(catalog);
        tokio::time::sleep(config.interval).await;
    }
}

/// Tells whether a snapshot holds every service. Each configured source must have answered at
/// least once, and no source may have failed at setup. After the first answer, a failed listing
/// keeps the last good answer, so the snapshot stays complete.
fn is_complete(source_setup_failed: bool, answered_kube: bool, answered_docker: bool) -> bool {
    !source_setup_failed && answered_kube && answered_docker
}

/// Converts workloads to a catalogue. It parses the labels, resolves the health, and groups the
/// services. The order of `workloads` does not matter. They are sorted by origin first.
/// Duplicates always resolve the same way.
async fn build_catalog(
    mut workloads: Vec<Workload>,
    mut problems: Vec<String>,
    pods: Option<&PodSet>,
    client: Option<&reqwest::Client>,
) -> Catalog {
    workloads.sort_by(|a, b| a.origin.cmp(&b.origin));
    let mut entries: Vec<(Service, Hint)> = Vec::new();
    let mut closed = Vec::new();
    for workload in workloads {
        match parse_labels(&workload.origin, &workload.labels) {
            Ok(Some(mut service)) => {
                // The `HEALTHCHECK` result costs nothing. It also applies with a health URL.
                service.runtime = workload.runtime;
                entries.push((service, workload.hint));
            }
            Ok(None) => {
                // The workload is not listed, but Traefik still routes it. Its path stays closed.
                closed.extend(closed_prefix(&workload.labels));
            }
            Err(problem) => {
                // The workload is not listed, but Traefik still routes it. Its path stays closed.
                closed.extend(closed_prefix(&workload.labels));
                problems.push(problem);
            }
        }
    }

    let limit = Arc::new(Semaphore::new(MAX_CONCURRENT_CHECKS));
    let mut checks: JoinSet<(usize, Health, Option<RuntimeHealth>)> = JoinSet::new();
    for (index, (service, hint)) in entries.iter_mut().enumerate() {
        match plan(service, hint) {
            Check::None => {}
            Check::Known(health) => service.health = health,
            Check::Http(url) => {
                let Some(client) = client.cloned() else {
                    continue;
                };
                let limit = Arc::clone(&limit);
                checks.spawn(async move {
                    let _permit = limit.acquire_owned().await;
                    let health = probe::http(&client, &url).await;
                    (index, health, None)
                });
            }
            Check::Pods {
                namespace,
                selector,
            } => match pods {
                Some(set) => {
                    let found = set.pods(&namespace, &selector);
                    service.health = if found.ready == 0 {
                        Health::Down
                    } else {
                        Health::Up
                    };
                    service.runtime = found.runtime.or(service.runtime);
                }
                None => service.health = Health::Unknown,
            },
        }
    }
    while let Some(result) = checks.join_next().await {
        if let Ok((index, health, runtime)) = result {
            let service = &mut entries[index].0;
            service.health = health;
            service.runtime = runtime.or(service.runtime);
        }
    }

    Catalog::build_with_closed(
        entries.into_iter().map(|(service, _)| service).collect(),
        problems,
        closed,
    )
}

/// Root certificates for the HTTP clients. The image has no CA store. `reqwest` refuses to build
/// a client without trust roots. This applies even to a client that never speaks TLS. The Mozilla
/// roots are therefore embedded (as in `dns-updater`). The Kubernetes client trusts only the
/// cluster CA. It does not use these roots.
fn embedded_roots() -> Vec<reqwest::Certificate> {
    webpki_root_certs::TLS_SERVER_ROOT_CERTS
        .iter()
        .filter_map(|der| reqwest::Certificate::from_der(der).ok())
        .collect()
}

/// The HTTP client of the readers of the services: the actions documents and the options. It
/// verifies the TLS certificate and the host name of the service. Only the health probes use a
/// client without this check ([`probe::build_client`]).
pub fn http_client(timeout: std::time::Duration) -> Result<reqwest::Client, String> {
    probe::build_verified_client(timeout)
}

/// Reads a response body. It fails when the body exceeds `limit` bytes.
pub async fn read_capped(mut response: reqwest::Response, limit: usize) -> Result<Vec<u8>, String> {
    let mut body = Vec::new();
    loop {
        match response.chunk().await {
            Ok(Some(chunk)) => {
                if body.len() + chunk.len() > limit {
                    return Err(format!("response body exceeds {limit} bytes"));
                }
                body.extend_from_slice(&chunk);
            }
            Ok(None) => return Ok(body),
            Err(e) => return Err(format!("cannot read the response: {e}")),
        }
    }
}

/// Percent-encodes a URL path or query component.
fn encode_component(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(byte));
        } else {
            let _ = write!(out, "%{byte:02X}");
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workload(origin: &str, labels: &[(&str, &str)], hint: Hint) -> Workload {
        Workload {
            origin: origin.to_string(),
            labels: labels
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            hint,
            runtime: None,
        }
    }

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
    }

    #[test]
    fn a_declared_health_url_wins_over_what_the_source_knows() {
        let labels = [
            ("portal.name", "A"),
            ("portal.href", "/a/"),
            ("portal.health", "http://a:8080/up"),
        ];
        let w = workload("a", &labels, Hint::Running(false));
        let service = parse_labels(&w.origin, &w.labels).unwrap().unwrap();
        assert_eq!(
            plan(&service, &w.hint),
            Check::Http("http://a:8080/up".into())
        );
    }

    #[test]
    fn hints_map_to_checks() {
        let labels = [("portal.name", "A"), ("portal.href", "/a/")];
        let service = parse_labels(
            "a",
            &labels
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            plan(&service, &Hint::Running(true)),
            Check::Known(Health::Up)
        );
        assert_eq!(
            plan(&service, &Hint::Running(false)),
            Check::Known(Health::Down)
        );
        assert_eq!(plan(&service, &Hint::None), Check::None);
        assert_eq!(
            plan(
                &service,
                &Hint::Pods {
                    namespace: "apps".into(),
                    selector: "app=a".into()
                }
            ),
            Check::Pods {
                namespace: "apps".into(),
                selector: "app=a".into()
            }
        );
    }

    #[test]
    fn catalogue_is_built_from_compose_and_homepage_labels_with_container_state() {
        let catalog = runtime().block_on(build_catalog(
            vec![
                workload(
                    "grafana",
                    &[
                        ("homepage.group", "Monitoring"),
                        ("homepage.name", "Grafana"),
                        ("homepage.href", "/grafana/"),
                    ],
                    Hint::Running(false),
                ),
                workload(
                    "cadvisor",
                    &[
                        ("homepage.group", "Monitoring"),
                        ("homepage.name", "Cadvisor"),
                        ("homepage.href", "/cadvisor/"),
                        ("homepage.weight", "20"),
                    ],
                    Hint::Running(true),
                ),
                workload("db", &[("traefik.enable", "true")], Hint::Running(true)),
                workload(
                    "broken",
                    &[
                        ("portal.name", "Broken"),
                        ("portal.href", "javascript:alert(1)"),
                    ],
                    Hint::None,
                ),
            ],
            vec!["kubernetes: unreachable".into()],
            None,
            None,
        ));
        assert_eq!(catalog.service_count(), 2);
        let monitoring = &catalog.groups()[0];
        assert_eq!(monitoring.name, "Monitoring");
        assert_eq!(
            monitoring.services[0].name, "Grafana",
            "weight 0 sorts before 20"
        );
        assert_eq!(monitoring.services[0].health, Health::Down);
        assert_eq!(monitoring.services[1].health, Health::Up);
        assert_eq!(catalog.problems().len(), 2);
        assert!(catalog.problems().iter().any(|p| p.starts_with("broken:")));
    }

    /// The `HEALTHCHECK` result of a container reaches the service, also when a health URL
    /// gives up or down.
    #[test]
    fn the_runtime_health_of_a_container_reaches_its_service() {
        let mut checked = workload(
            "a",
            &[("portal.name", "A"), ("portal.href", "/a/")],
            Hint::Running(true),
        );
        checked.runtime = Some(RuntimeHealth::Unhealthy);
        let mut probed = workload(
            "b",
            &[
                ("portal.name", "B"),
                ("portal.href", "/b/"),
                ("portal.health", "http://b:8080/up"),
            ],
            Hint::Running(true),
        );
        probed.runtime = Some(RuntimeHealth::Starting);
        let catalog = runtime().block_on(build_catalog(
            vec![
                checked,
                probed,
                workload(
                    "c",
                    &[("portal.name", "C"), ("portal.href", "/c/")],
                    Hint::Running(true),
                ),
            ],
            Vec::new(),
            None,
            None,
        ));
        let services = &catalog.groups()[0].services;
        assert_eq!(
            (services[0].health, services[0].runtime),
            (Health::Up, Some(RuntimeHealth::Unhealthy))
        );
        assert_eq!(
            services[1].runtime,
            Some(RuntimeHealth::Starting),
            "the health URL does not remove it"
        );
        assert_eq!(services[2].runtime, None, "no HEALTHCHECK");
    }

    #[test]
    fn replicas_resolve_to_the_first_origin_in_sorted_order() {
        let labels = [
            ("portal.group", "Monitoring"),
            ("portal.name", "Node exporter"),
            ("portal.href", "/ne/"),
        ];
        let catalog = runtime().block_on(build_catalog(
            vec![
                workload("z-node-exporter", &labels, Hint::Running(false)),
                workload("a-node-exporter", &labels, Hint::Running(true)),
            ],
            Vec::new(),
            None,
            None,
        ));
        assert_eq!(catalog.service_count(), 1);
        assert_eq!(catalog.duplicates(), 1);
        assert_eq!(
            catalog.groups()[0].services[0].health,
            Health::Up,
            "a-… sorts first"
        );
    }

    /// One bad `portal.exclude` line makes the labels unusable. The service is not listed, but
    /// its `href` stays closed in the gate: a user without a grant cannot open it.
    #[test]
    fn unusable_labels_keep_the_href_of_the_workload_closed() {
        let catalog = runtime().block_on(build_catalog(
            vec![workload(
                "grafana",
                &[
                    ("portal.name", "Grafana"),
                    ("portal.href", "/grafana/"),
                    (
                        "portal.exclude",
                        "GET /grafana/api/health\nGET /grafana/api/[a-z]+",
                    ),
                ],
                Hint::None,
            )],
            Vec::new(),
            None,
            None,
        ));
        assert_eq!(catalog.service_count(), 0);
        assert!(catalog.problems().iter().any(|p| p.starts_with("grafana:")));
        assert_eq!(
            crate::access::decide(&catalog, Some(&[]), "/grafana/d/1"),
            crate::access::Decision::Deny
        );
    }

    /// A workload that does not ask for a listing, but that gives a local `href`, keeps this path
    /// closed: Traefik still routes it. A path that no workload declares stays open.
    #[test]
    fn a_workload_that_is_not_listed_keeps_its_href_closed() {
        let catalog = runtime().block_on(build_catalog(
            vec![
                workload(
                    "grafana",
                    &[
                        ("portal.enabled", "false"),
                        ("portal.name", "Grafana"),
                        ("portal.href", "/grafana/"),
                    ],
                    Hint::Running(true),
                ),
                workload(
                    "apps/registry",
                    &[
                        ("gethomepage.dev/name", "Registry"),
                        ("gethomepage.dev/href", "/registry/"),
                    ],
                    Hint::None,
                ),
                workload(
                    "apps/wiki",
                    &[
                        ("gethomepage.dev/enabled", "false"),
                        ("gethomepage.dev/name", "Wiki"),
                        ("gethomepage.dev/href", "/wiki/"),
                    ],
                    Hint::None,
                ),
                workload(
                    "hp",
                    &[("homepage.group", "Tools"), ("homepage.href", "/hp/")],
                    Hint::Running(true),
                ),
                workload(
                    "notes",
                    &[
                        ("portal.id", "notes"),
                        ("portal.name", "Notes"),
                        ("portal.href", "/notes/"),
                    ],
                    Hint::Running(true),
                ),
            ],
            Vec::new(),
            None,
            None,
        ));
        assert_eq!(catalog.service_count(), 1);
        assert!(catalog.problems().is_empty(), "{:?}", catalog.problems());
        let none: Vec<crate::grants::Grant> = Vec::new();
        let notes = vec![crate::grants::Grant::new(crate::grants::ACCESS, "notes").unwrap()];
        for uri in [
            "/grafana/api/admin/users",
            "/registry/v2/_catalog",
            "/wiki/edit",
            "/hp/x",
        ] {
            for grants in [&none, &notes] {
                assert_eq!(
                    crate::access::decide(&catalog, Some(grants), uri),
                    crate::access::Decision::Deny,
                    "{uri}"
                );
            }
            assert_eq!(
                crate::access::decide(&catalog, None, uri),
                crate::access::Decision::Allow { service: None },
                "the superadmin passes {uri}"
            );
        }
        assert_eq!(
            crate::access::decide(&catalog, Some(&none), "/undeclared/x"),
            crate::access::Decision::Allow { service: None }
        );
        assert!(matches!(
            crate::access::decide(&catalog, Some(&notes), "/notes/x"),
            crate::access::Decision::Allow { service: Some(_) }
        ));
    }

    /// The audit lead portal/catalog/snapshot-gap-leaves-rerouted-href-unowned: a listing that
    /// answers but misses a routed workload (a restart, a recreate) does not open its path to
    /// each signed-in user. The path keeps its last owner for `KEEP_OWNER_ROUNDS` refreshes. A
    /// closed path stays closed in the same way. After this time, the path has no owner.
    #[test]
    fn a_listing_that_misses_a_workload_keeps_its_last_owner_for_a_while() {
        use crate::access::{decide, Decision};
        use crate::grants::{Grant, ACCESS};
        fn round(recent: &mut RecentOwners, workloads: Vec<Workload>) -> Catalog {
            let mut catalog = runtime().block_on(build_catalog(workloads, Vec::new(), None, None));
            recent.apply(&mut catalog);
            catalog
        }
        let service = |id: &str, name: &str| {
            let href = format!("/{id}/");
            workload(
                id,
                &[
                    ("portal.id", id),
                    ("portal.name", name),
                    ("portal.href", href.as_str()),
                ],
                Hint::Running(true),
            )
        };
        let hidden = || {
            workload(
                "hidden",
                &[
                    ("portal.enabled", "false"),
                    ("portal.name", "Hidden"),
                    ("portal.href", "/hidden/"),
                ],
                Hint::Running(true),
            )
        };
        let none: Vec<Grant> = Vec::new();
        let files = vec![Grant::new(ACCESS, "files").unwrap()];
        let owned_by_files = Decision::Allow {
            service: Some("files".to_string()),
        };
        let mut recent = RecentOwners::default();
        let listed = round(
            &mut recent,
            vec![service("files", "Files"), service("wiki", "Wiki"), hidden()],
        );
        assert_eq!(decide(&listed, Some(&none), "/files/x"), Decision::Deny);
        for gap in 1..=KEEP_OWNER_ROUNDS {
            let catalog = round(&mut recent, vec![service("wiki", "Wiki")]);
            assert_eq!(catalog.service_count(), 1, "the launcher does not show it");
            for uri in ["/files/x", "/files/api/admin", "/hidden/x"] {
                assert_eq!(
                    decide(&catalog, Some(&none), uri),
                    Decision::Deny,
                    "{uri} in the gap {gap}"
                );
            }
            assert_eq!(decide(&catalog, Some(&files), "/files/x"), owned_by_files);
            assert_eq!(decide(&catalog, None, "/files/x"), owned_by_files);
        }
        // The workload runs again: the new listing owns the path.
        let back = round(
            &mut recent,
            vec![service("files", "Files"), service("wiki", "Wiki")],
        );
        assert_eq!(decide(&back, Some(&none), "/files/x"), Decision::Deny);
        assert_eq!(decide(&back, Some(&files), "/files/x"), owned_by_files);
        // A workload that stays away loses its owner after the bound.
        for _ in 0..KEEP_OWNER_ROUNDS {
            let catalog = round(&mut recent, vec![service("wiki", "Wiki")]);
            assert_eq!(decide(&catalog, Some(&none), "/files/x"), Decision::Deny);
        }
        let gone = round(&mut recent, vec![service("wiki", "Wiki")]);
        assert_eq!(
            decide(&gone, Some(&none), "/files/x"),
            Decision::Allow { service: None }
        );
        assert_eq!(
            decide(&gone, Some(&none), "/hidden/x"),
            Decision::Allow { service: None }
        );
        assert_eq!(decide(&gone, Some(&none), "/wiki/x"), Decision::Deny);
    }

    /// The completeness of the snapshot: each configured source must have answered once.
    #[test]
    fn the_snapshot_is_complete_only_when_every_source_answered() {
        assert!(is_complete(false, true, true));
        assert!(!is_complete(true, true, true), "a source failed at setup");
        assert!(
            !is_complete(false, false, true),
            "kubernetes never answered"
        );
        assert!(!is_complete(false, true, false), "docker never answered");
    }

    #[test]
    fn stats_render_every_series() {
        let stats = Stats::default();
        stats.source_error("docker");
        let text = stats.render();
        assert!(text.contains("portal_catalog_services 0"));
        assert!(text.contains("portal_discovery_errors_total{source=\"docker\"} 1"));
        assert!(text.contains("portal_discovery_errors_total{source=\"kubernetes\"} 0"));
    }

    #[test]
    fn embedded_roots_are_usable() {
        assert!(embedded_roots().len() > 50);
    }

    /// A TLS peer on loopback with a self-signed certificate (`openssl s_server`). The process
    /// stops when the value goes.
    struct TlsPeer {
        child: std::process::Child,
        dir: std::path::PathBuf,
        port: u16,
    }

    impl TlsPeer {
        fn start() -> Self {
            use std::process::{Command, Stdio};
            let dir = std::env::temp_dir().join(format!("portal-tls-peer-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            let (cert, key) = (dir.join("c.pem"), dir.join("k.pem"));
            let made = Command::new("openssl")
                .args(["req", "-x509", "-newkey", "ec", "-pkeyopt"])
                .args([
                    "ec_paramgen_curve:prime256v1",
                    "-nodes",
                    "-subj",
                    "/CN=peer.invalid",
                ])
                .args(["-days", "1", "-keyout"])
                .arg(&key)
                .arg("-out")
                .arg(&cert)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .expect("the test needs the openssl command");
            assert!(made.success());
            let port = crate::util::test_ports::free();
            let child = Command::new("openssl")
                .args(["s_server", "-quiet", "-www", "-accept"])
                .arg(format!("127.0.0.1:{port}"))
                .arg("-cert")
                .arg(&cert)
                .arg("-key")
                .arg(&key)
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("the test needs the openssl command");
            let peer = Self { child, dir, port };
            let start = std::time::Instant::now();
            while std::net::TcpStream::connect(("127.0.0.1", port)).is_err() {
                assert!(start.elapsed() < std::time::Duration::from_secs(10));
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            peer
        }
    }

    impl Drop for TlsPeer {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    /// Only the health probe accepts a certificate that it cannot verify. The client of the
    /// actions documents and of the options (it sends the identity token) refuses the peer.
    #[test]
    fn only_the_health_probe_accepts_a_certificate_that_it_cannot_verify() {
        let runtime = runtime();
        let peer = {
            let _context = runtime.enter();
            TlsPeer::start()
        };
        let url = format!("https://127.0.0.1:{}/", peer.port);
        let timeout = std::time::Duration::from_secs(5);
        let probe = probe::build_client(timeout).unwrap();
        let answer = runtime.block_on(async { probe.get(&url).send().await.map(|r| r.status()) });
        assert_eq!(
            answer.ok().map(|s| s.as_u16()),
            Some(200),
            "the probe reaches the peer"
        );
        assert_eq!(runtime.block_on(probe::http(&probe, &url)), Health::Up);
        let reader = http_client(timeout).unwrap();
        let refused = runtime.block_on(async { reader.get(&url).send().await });
        assert!(refused.is_err(), "{refused:?}");
        let options = crate::options::Options::new(timeout).unwrap();
        let children = runtime.block_on(options.children(
            &format!("https://127.0.0.1:{}", peer.port),
            "x-identity-token",
            "a token that the peer must not get",
            &crate::options::Query {
                service: "db".into(),
                subject: "root".into(),
                parent: String::new(),
                kind: "host".into(),
                q: String::new(),
                after: String::new(),
            },
        ));
        assert!(children.is_err(), "{children:?}");
    }

    #[test]
    fn components_are_percent_encoded() {
        assert_eq!(encode_component("app=a,b"), "app%3Da%2Cb");
        assert_eq!(encode_component("a-b.c_d~e"), "a-b.c_d~e");
    }
}
