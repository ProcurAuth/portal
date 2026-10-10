//! Kubernetes source: Traefik IngressRoute annotations, plus pod readiness for the status.
//!
//! In-cluster only. The ServiceAccount token is read on each request (it rotates); the cluster
//! CA is the only trusted root. The RBAC needed is `get` and `list` on `ingressroutes` and `pods`.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use serde::Deserialize;

use super::{Hint, Workload};

const SERVICE_ACCOUNT_DIR: &str = "/var/run/secrets/kubernetes.io/serviceaccount";
const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone)]
pub struct Kube {
    client: reqwest::Client,
    base: String,
    token_file: PathBuf,
}

impl Kube {
    pub fn in_cluster(timeout: Duration) -> Result<Self, String> {
        let host = std::env::var("KUBERNETES_SERVICE_HOST").map_err(|_| {
            "KUBERNETES_SERVICE_HOST is not set: not running in a cluster".to_string()
        })?;
        let port = std::env::var("KUBERNETES_SERVICE_PORT_HTTPS")
            .or_else(|_| std::env::var("KUBERNETES_SERVICE_PORT"))
            .unwrap_or_else(|_| "443".to_string());
        let host = if host.contains(':') {
            format!("[{host}]")
        } else {
            host
        };

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

    /// Every IngressRoute with its annotations.
    pub async fn workloads(&self) -> Result<Vec<Workload>, String> {
        let body = self
            .get("/apis/traefik.io/v1alpha1/ingressroutes?limit=1000")
            .await?;
        parse_ingressroutes(&body)
    }

    /// How many pods matching `selector` in `namespace` are ready.
    pub async fn ready_pods(&self, namespace: &str, selector: &str) -> Result<usize, String> {
        let body = self
            .get(&format!(
                "/api/v1/namespaces/{}/pods?labelSelector={}",
                super::encode_component(namespace),
                super::encode_component(selector)
            ))
            .await?;
        count_ready(&body)
    }
}

#[derive(Deserialize)]
struct List<T> {
    #[serde(default = "Vec::new")]
    items: Vec<T>,
}

#[derive(Deserialize)]
struct Metadata {
    name: String,
    #[serde(default)]
    namespace: String,
    #[serde(default)]
    annotations: Option<BTreeMap<String, String>>,
    #[serde(rename = "deletionTimestamp", default)]
    deletion_timestamp: Option<String>,
}

#[derive(Deserialize)]
struct Route {
    metadata: Metadata,
}

pub(super) fn parse_ingressroutes(body: &[u8]) -> Result<Vec<Workload>, String> {
    let list: List<Route> =
        serde_json::from_slice(body).map_err(|e| format!("unexpected IngressRoute list: {e}"))?;
    Ok(list
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
            }
        })
        .collect())
}

/// Equality selectors only (`app=registry-ui,tier=web`): the value ends up in a URL.
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
    status: PodStatus,
}

#[derive(Deserialize, Default)]
struct PodStatus {
    #[serde(default)]
    conditions: Vec<Condition>,
}

#[derive(Deserialize)]
struct Condition {
    #[serde(rename = "type")]
    kind: String,
    status: String,
}

pub(super) fn count_ready(body: &[u8]) -> Result<usize, String> {
    let list: List<Pod> =
        serde_json::from_slice(body).map_err(|e| format!("unexpected Pod list: {e}"))?;
    Ok(list
        .items
        .iter()
        .filter(|pod| {
            pod.metadata.deletion_timestamp.is_none()
                && pod
                    .status
                    .conditions
                    .iter()
                    .any(|c| c.kind == "Ready" && c.status == "True")
        })
        .count())
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(count_ready(body).unwrap(), 1);
        assert_eq!(count_ready(br#"{"items":[]}"#).unwrap(), 0);
        assert!(count_ready(b"nonsense").is_err());
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
