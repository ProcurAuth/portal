//! Service discovery: where the catalogue comes from and what state each service is in.
//!
//! Two sources, both optional: Kubernetes (Traefik IngressRoute annotations) and Docker
//! (container labels). Each refresh lists the sources, turns every workload's labels into a
//! [`Service`](crate::catalog::Service), resolves a health state, and publishes a new immutable
//! [`Catalog`] snapshot. A source that fails keeps its last good answer and adds a problem message
//! to the snapshot, so a flaky API does not blank the page.
//!
//! Interface: [`run`] (the refresh loop) and [`Stats`] (for `/metrics`). The loop publishes through
//! a callback, so nothing here knows about the HTTP server.

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

use crate::catalog::{parse_labels, Catalog, Health, Service};
use crate::config::DiscoveryConfig;

use docker::Docker;
use kube::Kube;

const MAX_CONCURRENT_CHECKS: usize = 8;

/// What a source reports about one workload: its labels and, when it knows, how to tell if it runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Workload {
    /// `namespace/name` of a route, or the Compose service of a container.
    pub origin: String,
    pub labels: BTreeMap<String, String>,
    pub hint: Hint,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Hint {
    None,
    /// The container's own state (Docker).
    Running(bool),
    /// Ready pods matching an equality selector (Kubernetes).
    Pods {
        namespace: String,
        selector: String,
    },
}

/// How the health of one service is found.
#[derive(Debug, PartialEq, Eq)]
enum Check {
    None,
    Known(Health),
    Http(String),
    Pods { namespace: String, selector: String },
}

/// A declared health URL wins; else what the source knows.
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

/// Counters for `/metrics`.
#[derive(Default)]
pub struct Stats {
    refreshes: AtomicU64,
    last_success_unix: AtomicU64,
    services: AtomicUsize,
    down: AtomicUsize,
    duplicates: AtomicUsize,
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
        if healthy_refresh {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            self.last_success_unix.store(now, Ordering::Relaxed);
        }
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

/// The refresh loop. Runs until the process ends; each refresh hands a new snapshot to `publish`.
pub async fn run(
    config: DiscoveryConfig,
    stats: Arc<Stats>,
    publish: impl Fn(Catalog) + Send + 'static,
) {
    let mut setup_problems: Vec<String> = Vec::new();
    let kube = if config.kubernetes {
        Kube::in_cluster(config.probe_timeout)
            .map_err(|e| setup_problems.push(format!("kubernetes: {e}")))
            .ok()
    } else {
        None
    };
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

    let mut last_kube: Vec<Workload> = Vec::new();
    let mut last_docker: Vec<Workload> = Vec::new();
    loop {
        let mut problems = setup_problems.clone();
        let mut healthy = setup_problems.is_empty();
        if let Some(kube) = &kube {
            match kube.workloads().await {
                Ok(workloads) => last_kube = workloads,
                Err(e) => {
                    stats.source_error("kubernetes");
                    problems.push(format!("kubernetes: {e}"));
                    healthy = false;
                }
            }
        }
        if let Some(docker) = &docker {
            match docker.workloads().await {
                Ok(workloads) => last_docker = workloads,
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
        let catalog = build_catalog(workloads, problems, kube.as_ref(), client.as_ref()).await;
        stats.record(&catalog, healthy);
        publish(catalog);
        tokio::time::sleep(config.interval).await;
    }
}

/// Workloads to a catalogue: parse labels, resolve health, group. Order of `workloads` does not
/// matter: they are sorted by origin first so duplicates always resolve the same way.
async fn build_catalog(
    mut workloads: Vec<Workload>,
    mut problems: Vec<String>,
    kube: Option<&Kube>,
    client: Option<&reqwest::Client>,
) -> Catalog {
    workloads.sort_by(|a, b| a.origin.cmp(&b.origin));
    let mut entries: Vec<(Service, Hint)> = Vec::new();
    for workload in workloads {
        match parse_labels(&workload.origin, &workload.labels) {
            Ok(Some(service)) => entries.push((service, workload.hint)),
            Ok(None) => {}
            Err(problem) => problems.push(problem),
        }
    }

    let limit = Arc::new(Semaphore::new(MAX_CONCURRENT_CHECKS));
    let mut checks: JoinSet<(usize, Health)> = JoinSet::new();
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
                    (index, probe::http(&client, &url).await)
                });
            }
            Check::Pods {
                namespace,
                selector,
            } => {
                let Some(kube) = kube.cloned() else { continue };
                let limit = Arc::clone(&limit);
                checks.spawn(async move {
                    let _permit = limit.acquire_owned().await;
                    let health = match kube.ready_pods(&namespace, &selector).await {
                        Ok(0) => Health::Down,
                        Ok(_) => Health::Up,
                        Err(_) => Health::Unknown,
                    };
                    (index, health)
                });
            }
        }
    }
    while let Some(result) = checks.join_next().await {
        if let Ok((index, health)) = result {
            entries[index].0.health = health;
        }
    }

    Catalog::build(
        entries.into_iter().map(|(service, _)| service).collect(),
        problems,
    )
}

/// Root certificates for the HTTP clients. The image has no CA store and `reqwest` refuses to build
/// a client without trust roots, even one that never speaks TLS, so the Mozilla roots are embedded
/// (as in `dns-updater`). The Kubernetes client trusts only the cluster CA and does not use these.
fn embedded_roots() -> Vec<reqwest::Certificate> {
    webpki_root_certs::TLS_SERVER_ROOT_CERTS
        .iter()
        .filter_map(|der| reqwest::Certificate::from_der(der).ok())
        .collect()
}

/// Reads a response body, failing once it exceeds `limit` bytes.
async fn read_capped(mut response: reqwest::Response, limit: usize) -> Result<Vec<u8>, String> {
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

    #[test]
    fn components_are_percent_encoded() {
        assert_eq!(encode_component("app=a,b"), "app%3Da%2Cb");
        assert_eq!(encode_component("a-b.c_d~e"), "a-b.c_d~e");
    }
}
