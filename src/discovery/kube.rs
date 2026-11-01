//! Kubernetes source: Traefik IngressRoute annotations, and the pods for the status.
//!
//! This source works in the cluster only. The ServiceAccount token is read on each request
//! because it rotates. The cluster CA is the only trusted root. The required RBAC is `get` and
//! `list` on `ingressroutes` and `pods`.
//!
//! The source reads two lists, page by page: the IngressRoutes and all the pods (portal 7.3c). It
//! finds the pods of each selector in memory. The number of ready pods gives up or down. The
//! readiness probes of the containers give the runtime health. The pod list holds the spec of
//! each pod, so the runtime health needs no other request and no other RBAC verb.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use serde::Deserialize;

use super::{Hint, Workload};
use crate::catalog::RuntimeHealth;
use crate::config::KubernetesApi;

const SERVICE_ACCOUNT_DIR: &str = "/var/run/secrets/kubernetes.io/serviceaccount";
const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;
/// The number of items that one request of a list asks for.
const PAGE_ITEMS: usize = 500;

#[derive(Clone)]
pub struct Kube {
    client: reqwest::Client,
    base: String,
    token_file: PathBuf,
}

impl Kube {
    /// The configuration gives `api`. It names the variables that hold the host and the port.
    pub fn in_cluster(api: &KubernetesApi, timeout: Duration) -> Result<Self, String> {
        let (host, port) = (&api.host, api.port);
        let ca_path = PathBuf::from(SERVICE_ACCOUNT_DIR).join("ca.crt");
        let ca = std::fs::read(&ca_path)
            .map_err(|e| format!("cannot read {}: {e}", ca_path.display()))?;
        let certificate = reqwest::Certificate::from_pem(&ca)
            .map_err(|e| format!("invalid cluster CA {}: {e}", ca_path.display()))?;
        let client = reqwest::Client::builder()
            .tls_certs_only([certificate])
            .redirect(reqwest::redirect::Policy::none())
            .timeout(timeout)
            .connect_timeout(timeout)
            .user_agent(concat!("portal/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| format!("cannot build the Kubernetes client: {e}"))?;
        Ok(Self {
            client,
            base: format!("https://{host}:{port}"),
            token_file: PathBuf::from(SERVICE_ACCOUNT_DIR).join("token"),
        })
    }

    async fn get(&self, path_and_query: &str) -> Result<Vec<u8>, String> {
        let token = std::fs::read_to_string(&self.token_file)
            .map_err(|e| format!("cannot read the ServiceAccount token: {e}"))?;
        let response = self
            .client
            .get(format!("{}{path_and_query}", self.base))
            .bearer_auth(token.trim())
            .header("accept", "application/json")
            .send()
            .await
            .map_err(|e| format!("cannot reach the Kubernetes API: {e}"))?;
        if !response.status().is_success() {
            return Err(format!(
                "the Kubernetes API answered {} for {path_and_query}",
                response.status()
            ));
        }
        super::read_capped(response, MAX_BODY_BYTES).await
    }

    /// Returns every IngressRoute with its annotations, in as many requests as the list needs.
    pub async fn workloads(&self) -> Result<Vec<Workload>, String> {
        let mut all = Vec::new();
        let mut next = None;
        loop {
            let body = self
                .get(&page_path(
                    "/apis/traefik.io/v1alpha1/ingressroutes",
                    next.as_deref(),
                ))
                .await?;
            let (mut page, token) = parse_ingressroutes_page(&body)?;
            all.append(&mut page);
            match token {
                Some(token) => next = Some(token),
                None => return Ok(all),
            }
        }
    }

    /// Returns the state of every pod of the cluster, in as many requests as the list needs.
    pub async fn pod_set(&self) -> Result<PodSet, String> {
        let mut all = Vec::new();
        let mut next = None;
        loop {
            let body = self
                .get(&page_path("/api/v1/pods", next.as_deref()))
                .await?;
            let (mut page, token) = parse_pods_page(&body)?;
            all.append(&mut page);
            match token {
                Some(token) => next = Some(token),
                None => return Ok(PodSet { pods: all }),
            }
        }
    }
}

/// The path of one page of a list. `token` is the `continue` value of the page before.
fn page_path(path: &str, token: Option<&str>) -> String {
    match token {
        None => format!("{path}?limit={PAGE_ITEMS}"),
        Some(token) => format!(
            "{path}?limit={PAGE_ITEMS}&continue={}",
            super::encode_component(token)
        ),
    }
}

#[derive(Deserialize)]
struct List<T> {
    #[serde(default = "Vec::new")]
    items: Vec<T>,
    #[serde(default)]
    metadata: ListMetadata,
}

#[derive(Deserialize, Default)]
struct ListMetadata {
    /// The value for the next page. The last page has none, or an empty value.
    #[serde(rename = "continue", default)]
    next: Option<String>,
}

impl<T> List<T> {
    fn next(&self) -> Option<String> {
        self.metadata.next.clone().filter(|token| !token.is_empty())
    }
}

#[derive(Deserialize)]
struct Metadata {
    name: String,
    #[serde(default)]
    namespace: String,
    #[serde(default)]
    annotations: Option<BTreeMap<String, String>>,
    #[serde(default)]
    labels: Option<BTreeMap<String, String>>,
    #[serde(rename = "deletionTimestamp", default)]
    deletion_timestamp: Option<String>,
}

#[derive(Deserialize)]
struct Route {
    metadata: Metadata,
}

/// The routes of one page of the list, and the value for the next page.
fn parse_ingressroutes_page(body: &[u8]) -> Result<(Vec<Workload>, Option<String>), String> {
    let list: List<Route> =
        serde_json::from_slice(body).map_err(|e| format!("unexpected IngressRoute list: {e}"))?;
    let next = list.next();
    let workloads = list
        .items
        .into_iter()
        .map(|route| {
            let annotations = route.metadata.annotations.unwrap_or_default();
            let namespace = route.metadata.namespace;
            let selector = ["portal.pods", "gethomepage.dev/pod-selector"]
                .iter()
                .filter_map(|key| annotations.get(*key))
                .map(|v| v.trim())
                .find(|v| !v.is_empty())
                .map(str::to_string);
            let hint = match selector {
                Some(selector) if valid_selector(&selector) => Hint::Pods {
                    namespace: namespace.clone(),
                    selector,
                },
                _ => Hint::None,
            };
            Workload {
                origin: format!("{namespace}/{}", route.metadata.name),
                labels: annotations,
                hint,
                runtime: None,
            }
        })
        .collect();
    Ok((workloads, next))
}

/// Use equality selectors only (`app=registry-ui,tier=web`). The value ends up in a URL.
fn valid_selector(selector: &str) -> bool {
    !selector.is_empty()
        && selector.len() <= 256
        && selector.bytes().all(|b| {
            b.is_ascii_alphanumeric() || matches!(b, b'=' | b',' | b'-' | b'_' | b'.' | b'/')
        })
}

#[derive(Deserialize)]
struct Pod {
    metadata: Metadata,
    #[serde(default)]
    spec: PodSpec,
    #[serde(default)]
    status: PodStatus,
}

#[derive(Deserialize, Default)]
struct PodSpec {
    #[serde(default)]
    containers: Vec<Container>,
}

#[derive(Deserialize)]
struct Container {
    #[serde(rename = "readinessProbe", default)]
    readiness_probe: Option<serde::de::IgnoredAny>,
}

#[derive(Deserialize, Default)]
struct PodStatus {
    #[serde(default)]
    phase: String,
    #[serde(default)]
    conditions: Vec<Condition>,
    #[serde(rename = "containerStatuses", default)]
    container_statuses: Vec<ContainerStatus>,
}

#[derive(Deserialize)]
struct Condition {
    #[serde(rename = "type")]
    kind: String,
    status: String,
}

#[derive(Deserialize)]
struct ContainerStatus {
    #[serde(default)]
    started: Option<bool>,
    #[serde(default)]
    state: ContainerState,
}

#[derive(Deserialize, Default)]
struct ContainerState {
    #[serde(default)]
    waiting: Option<Waiting>,
}

#[derive(Deserialize)]
struct Waiting {
    #[serde(default)]
    reason: String,
}

/// The reasons of a waiting container that tell a normal start. Other reasons, for example
/// `CrashLoopBackOff` or `ImagePullBackOff`, tell a failure.
const START_REASONS: [&str; 3] = ["", "ContainerCreating", "PodInitializing"];

/// The state of the pods of one selector.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pods {
    /// The pods that are ready and not terminating.
    pub ready: usize,
    /// The result of the readiness probes. `None` when no pod has a container with a probe.
    pub runtime: Option<RuntimeHealth>,
}

/// What the portal keeps of one pod: its place, its labels and its state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PodRecord {
    namespace: String,
    labels: BTreeMap<String, String>,
    terminating: bool,
    ready: bool,
    /// The runtime health of a pod with a readiness probe. `None` for a pod without a probe.
    runtime: Option<RuntimeHealth>,
}

/// Every pod of the cluster, from the last complete list.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PodSet {
    pods: Vec<PodRecord>,
}

impl PodSet {
    /// The number of pods that the set holds.
    pub fn len(&self) -> usize {
        self.pods.len()
    }

    /// Counts the ready pods and resolves the runtime health for the pods of `namespace` that
    /// match `selector`. A terminating pod does not count. The runtime health uses only the pods
    /// with a readiness probe. One unhealthy pod makes the service unhealthy. The service is
    /// starting when a pod starts and no pod with a probe is ready.
    pub fn pods(&self, namespace: &str, selector: &str) -> Pods {
        let wanted: Vec<(&str, Option<&str>)> = selector
            .split(',')
            .filter(|pair| !pair.is_empty())
            .map(|pair| match pair.split_once('=') {
                Some((key, value)) => (key, Some(value)),
                None => (pair, None),
            })
            .collect();
        let live = || {
            self.pods.iter().filter(|pod| {
                pod.namespace == namespace
                    && !pod.terminating
                    && wanted.iter().all(|(key, value)| match value {
                        Some(value) => pod.labels.get(*key).map(String::as_str) == Some(*value),
                        None => pod.labels.contains_key(*key),
                    })
            })
        };
        let ready = live().filter(|pod| pod.ready).count();
        let probed: Vec<RuntimeHealth> = live().filter_map(|pod| pod.runtime).collect();
        let any = |state: RuntimeHealth| probed.contains(&state);
        let runtime = if probed.is_empty() {
            None
        } else if any(RuntimeHealth::Unhealthy) {
            Some(RuntimeHealth::Unhealthy)
        } else if any(RuntimeHealth::Starting) && !any(RuntimeHealth::Healthy) {
            Some(RuntimeHealth::Starting)
        } else {
            Some(RuntimeHealth::Healthy)
        };
        Pods { ready, runtime }
    }
}

impl Pod {
    fn ready(&self) -> bool {
        self.status
            .conditions
            .iter()
            .any(|c| c.kind == "Ready" && c.status == "True")
    }

    /// The runtime health of a pod with a readiness probe. `None` for a pod without a probe.
    fn runtime(&self) -> Option<RuntimeHealth> {
        if !self
            .spec
            .containers
            .iter()
            .any(|c| c.readiness_probe.is_some())
        {
            return None;
        }
        if self.ready() {
            return Some(RuntimeHealth::Healthy);
        }
        let statuses = &self.status.container_statuses;
        let waiting = || statuses.iter().filter_map(|c| c.state.waiting.as_ref());
        if waiting().any(|w| !START_REASONS.contains(&w.reason.as_str())) {
            return Some(RuntimeHealth::Unhealthy);
        }
        let starting = self.status.phase == "Pending"
            || statuses.iter().any(|c| c.started == Some(false))
            || waiting().next().is_some();
        Some(if starting {
            RuntimeHealth::Starting
        } else {
            RuntimeHealth::Unhealthy
        })
    }
}

/// The pods of one page of the list, and the value for the next page.
fn parse_pods_page(body: &[u8]) -> Result<(Vec<PodRecord>, Option<String>), String> {
    let list: List<Pod> =
        serde_json::from_slice(body).map_err(|e| format!("unexpected Pod list: {e}"))?;
    let next = list.next();
    let pods = list
        .items
        .iter()
        .map(|pod| PodRecord {
            namespace: pod.metadata.namespace.clone(),
            labels: pod.metadata.labels.clone().unwrap_or_default(),
            terminating: pod.metadata.deletion_timestamp.is_some(),
            ready: pod.ready(),
            runtime: pod.runtime(),
        })
        .collect();
    Ok((pods, next))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_ingressroutes(body: &[u8]) -> Result<Vec<Workload>, String> {
        parse_ingressroutes_page(body).map(|(workloads, _)| workloads)
    }

    /// The state of all the pods of a list, without selector and namespace.
    fn parse_pods(body: &[u8]) -> Result<Pods, String> {
        let (pods, _) = parse_pods_page(body)?;
        Ok(PodSet { pods }.pods("", ""))
    }

    #[test]
    fn ingress_routes_become_workloads_with_their_annotations() {
        let body = br#"{"items":[
          {"metadata":{"name":"registry-ui","namespace":"apps","annotations":{
            "gethomepage.dev/enabled":"true","gethomepage.dev/name":"Registry",
            "gethomepage.dev/href":"/registry/","gethomepage.dev/pod-selector":"app=registry-ui"}}},
          {"metadata":{"name":"auth","namespace":"edge"}},
          {"metadata":{"name":"odd","namespace":"apps","annotations":{"portal.pods":"app in (a,b)"}}}
        ]}"#;
        let workloads = parse_ingressroutes(body).unwrap();
        assert_eq!(workloads.len(), 3);
        assert_eq!(workloads[0].origin, "apps/registry-ui");
        assert_eq!(
            workloads[0].hint,
            Hint::Pods {
                namespace: "apps".into(),
                selector: "app=registry-ui".into()
            }
        );
        assert_eq!(workloads[1].hint, Hint::None);
        assert!(workloads[1].labels.is_empty());
        assert_eq!(
            workloads[2].hint,
            Hint::None,
            "a set-based selector is not accepted"
        );
    }

    #[test]
    fn native_pod_selector_wins_over_the_homepage_one() {
        let body = br#"{"items":[{"metadata":{"name":"x","namespace":"apps","annotations":{
          "portal.pods":"app=new","gethomepage.dev/pod-selector":"app=old"}}}]}"#;
        let workloads = parse_ingressroutes(body).unwrap();
        assert_eq!(
            workloads[0].hint,
            Hint::Pods {
                namespace: "apps".into(),
                selector: "app=new".into()
            }
        );
    }

    #[test]
    fn only_ready_pods_that_are_not_terminating_count() {
        let body = br#"{"items":[
          {"metadata":{"name":"a"},"status":{"conditions":[{"type":"Ready","status":"True"}]}},
          {"metadata":{"name":"b"},"status":{"conditions":[{"type":"Ready","status":"False"}]}},
          {"metadata":{"name":"c","deletionTimestamp":"2026-10-10T00:00:00Z"},"status":{"conditions":[{"type":"Ready","status":"True"}]}},
          {"metadata":{"name":"d"}}
        ]}"#;
        assert_eq!(
            parse_pods(body).unwrap(),
            Pods {
                ready: 1,
                runtime: None
            },
            "no readiness probe: no runtime health"
        );
        assert_eq!(parse_pods(br#"{"items":[]}"#).unwrap().ready, 0);
        assert!(parse_pods(b"nonsense").is_err());
    }

    /// A pod for the tests of the runtime health. `probe` gives the container a readiness probe.
    fn pod(probe: bool, ready: bool, phase: &str, statuses: &str, terminating: bool) -> String {
        format!(
            r#"{{"metadata":{{"name":"p"{}}},"spec":{{"containers":[{{"name":"c"{}}}]}},"status":{{"phase":"{phase}","conditions":[{{"type":"Ready","status":"{}"}}],"containerStatuses":[{statuses}]}}}}"#,
            if terminating {
                r#","deletionTimestamp":"2026-10-10T00:00:00Z""#
            } else {
                ""
            },
            if probe {
                r#","readinessProbe":{"httpGet":{"path":"/healthz","port":8080}}"#
            } else {
                ""
            },
            if ready { "True" } else { "False" },
        )
    }

    fn runtime_of(pods: &[String]) -> Option<RuntimeHealth> {
        parse_pods(format!(r#"{{"items":[{}]}}"#, pods.join(",")).as_bytes())
            .unwrap()
            .runtime
    }

    const RUNNING: &str = r#"{"ready":true,"started":true,"state":{"running":{}}}"#;
    const NOT_READY: &str = r#"{"ready":false,"started":true,"state":{"running":{}}}"#;
    const CREATING: &str =
        r#"{"ready":false,"started":false,"state":{"waiting":{"reason":"ContainerCreating"}}}"#;
    const CRASHING: &str =
        r#"{"ready":false,"started":false,"state":{"waiting":{"reason":"CrashLoopBackOff"}}}"#;
    const NOT_STARTED: &str = r#"{"ready":false,"started":false,"state":{"running":{}}}"#;

    #[test]
    fn readiness_probes_give_the_runtime_health_of_the_pods() {
        let healthy = pod(true, true, "Running", RUNNING, false);
        let failing = pod(true, false, "Running", NOT_READY, false);
        let creating = pod(true, false, "Pending", CREATING, false);
        let unprobed = pod(false, false, "Running", NOT_READY, false);
        assert_eq!(
            runtime_of(std::slice::from_ref(&healthy)),
            Some(RuntimeHealth::Healthy)
        );
        assert_eq!(
            runtime_of(std::slice::from_ref(&unprobed)),
            None,
            "no probe"
        );
        assert_eq!(
            runtime_of(&[healthy.clone(), unprobed]),
            Some(RuntimeHealth::Healthy),
            "a pod without a probe does not count"
        );
        assert_eq!(
            runtime_of(std::slice::from_ref(&failing)),
            Some(RuntimeHealth::Unhealthy),
            "the container runs and its probe fails"
        );
        assert_eq!(
            runtime_of(&[healthy.clone(), failing]),
            Some(RuntimeHealth::Unhealthy),
            "one unhealthy pod is enough"
        );
        assert_eq!(
            runtime_of(std::slice::from_ref(&creating)),
            Some(RuntimeHealth::Starting)
        );
        assert_eq!(
            runtime_of(&[pod(true, false, "Running", NOT_STARTED, false)]),
            Some(RuntimeHealth::Starting),
            "a container with started=false"
        );
        assert_eq!(
            runtime_of(&[healthy.clone(), creating]),
            Some(RuntimeHealth::Healthy),
            "a new pod starts next to a ready one: a rolling update"
        );
        assert_eq!(
            runtime_of(&[pod(true, false, "Running", CRASHING, false)]),
            Some(RuntimeHealth::Unhealthy),
            "a crash loop is not a start"
        );
    }

    #[test]
    fn a_terminating_pod_gives_no_runtime_health() {
        let leaving = pod(true, false, "Running", NOT_READY, true);
        assert_eq!(runtime_of(std::slice::from_ref(&leaving)), None);
        assert_eq!(
            runtime_of(&[leaving, pod(true, true, "Running", RUNNING, false)]),
            Some(RuntimeHealth::Healthy)
        );
    }

    #[test]
    fn a_page_gives_the_value_for_the_next_page_and_the_last_page_gives_none() {
        let first = br#"{"metadata":{"continue":"abc/def=="},"items":[{"metadata":{"name":"a","namespace":"x"}}]}"#;
        let (workloads, next) = parse_ingressroutes_page(first).unwrap();
        assert_eq!(workloads.len(), 1);
        assert_eq!(next.as_deref(), Some("abc/def=="));
        let last = br#"{"metadata":{"continue":""},"items":[]}"#;
        assert_eq!(parse_ingressroutes_page(last).unwrap().1, None);
        assert_eq!(parse_pods_page(br#"{"items":[]}"#).unwrap().1, None);
        assert_eq!(
            page_path("/api/v1/pods", None),
            "/api/v1/pods?limit=500",
            "the first page has no token"
        );
        assert_eq!(
            page_path("/api/v1/pods", Some("a/b=")),
            "/api/v1/pods?limit=500&continue=a%2Fb%3D"
        );
    }

    #[test]
    fn the_pods_of_a_selector_come_from_the_set_in_memory() {
        let pod = |namespace: &str, labels: &str, ready: bool| {
            format!(
                r#"{{"metadata":{{"name":"p","namespace":"{namespace}","labels":{{{labels}}}}},"status":{{"conditions":[{{"type":"Ready","status":"{}"}}]}}}}"#,
                if ready { "True" } else { "False" }
            )
        };
        let body = format!(
            r#"{{"items":[{},{},{},{}]}}"#,
            pod("apps", r#""app":"web","tier":"front""#, true),
            pod("apps", r#""app":"web","tier":"back""#, false),
            pod("other", r#""app":"web""#, true),
            pod("apps", r#""app":"db""#, true),
        );
        let (pods, _) = parse_pods_page(body.as_bytes()).unwrap();
        let set = PodSet { pods };
        assert_eq!(set.len(), 4);
        assert_eq!(set.pods("apps", "app=web").ready, 1);
        assert_eq!(set.pods("apps", "app=web,tier=back").ready, 0);
        assert_eq!(set.pods("apps", "app=web,tier=front").ready, 1);
        assert_eq!(set.pods("other", "app=web").ready, 1);
        assert_eq!(set.pods("apps", "app").ready, 2, "a key without a value");
        assert_eq!(set.pods("apps", "app=none").ready, 0);
    }

    #[test]
    fn selectors_are_restricted_to_url_safe_equality_forms() {
        assert!(valid_selector("app=registry-ui"));
        assert!(valid_selector("app.kubernetes.io/name=grafana,tier=web"));
        assert!(!valid_selector(""));
        assert!(!valid_selector("app in (a)"));
        assert!(!valid_selector("a=b&c=d"));
    }
}
