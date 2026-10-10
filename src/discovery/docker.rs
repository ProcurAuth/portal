//! Docker (Compose) source: container labels read from the Docker socket.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use serde::Deserialize;

use super::{Hint, Workload};

const MAX_BODY_BYTES: usize = 8 * 1024 * 1024;

#[derive(Clone)]
pub struct Docker {
    client: reqwest::Client,
}

impl Docker {
    pub fn new(socket: &Path, timeout: Duration) -> Result<Self, String> {
        let client = reqwest::Client::builder()
            .tls_certs_only(super::embedded_roots())
            .unix_socket(socket)
            .timeout(timeout)
            .connect_timeout(timeout)
            .build()
            .map_err(|e| format!("cannot build the Docker client: {e}"))?;
        Ok(Self { client })
    }

    /// Running containers with their labels.
    pub async fn workloads(&self) -> Result<Vec<Workload>, String> {
        let response = self
            .client
            .get("http://docker/containers/json")
            .send()
            .await
            .map_err(|e| format!("cannot reach the Docker socket: {e}"))?;
        if !response.status().is_success() {
            return Err(format!("Docker answered {}", response.status()));
        }
        let body = super::read_capped(response, MAX_BODY_BYTES).await?;
        parse_containers(&body)
    }
}

#[derive(Deserialize)]
struct Container {
    #[serde(rename = "Names", default)]
    names: Vec<String>,
    #[serde(rename = "Labels", default)]
    labels: Option<BTreeMap<String, String>>,
    #[serde(rename = "State", default)]
    state: String,
}

pub(super) fn parse_containers(body: &[u8]) -> Result<Vec<Workload>, String> {
    let containers: Vec<Container> =
        serde_json::from_slice(body).map_err(|e| format!("unexpected Docker answer: {e}"))?;
    Ok(containers
        .into_iter()
        .filter_map(|container| {
            let labels = container.labels.unwrap_or_default();
            let origin = labels
                .get("com.docker.compose.service")
                .cloned()
                .or_else(|| {
                    container
                        .names
                        .first()
                        .map(|n| n.trim_start_matches('/').to_string())
                })?;
            Some(Workload {
                origin,
                labels,
                hint: Hint::Running(container.state == "running"),
            })
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn containers_become_workloads_named_after_their_compose_service() {
        let body = br#"[
          {"Id":"a","Names":["/cadvisor-1"],"State":"running","Labels":{"com.docker.compose.service":"cadvisor","homepage.name":"Cadvisor","homepage.href":"/cadvisor/"}},
          {"Id":"b","Names":["/plain"],"State":"exited","Labels":null},
          {"Id":"c","Names":[],"State":"running","Labels":{}}
        ]"#;
        let workloads = parse_containers(body).unwrap();
        assert_eq!(
            workloads.len(),
            2,
            "a container with neither a service label nor a name is skipped"
        );
        assert_eq!(workloads[0].origin, "cadvisor");
        assert_eq!(workloads[0].hint, Hint::Running(true));
        assert_eq!(workloads[0].labels["homepage.name"], "Cadvisor");
        assert_eq!(workloads[1].origin, "plain");
        assert_eq!(workloads[1].hint, Hint::Running(false));
    }

    #[test]
    fn a_non_array_answer_is_an_error() {
        assert!(parse_containers(br#"{"message":"nope"}"#).is_err());
    }
}
