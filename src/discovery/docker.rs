//! Docker (Compose, Swarm) source: container labels read from the Docker socket.
//!
//! The list of the containers also gives the result of the `HEALTHCHECK` of each container. The
//! source reads it from the same answer. It makes no other request.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use serde::Deserialize;

use super::{Hint, Workload};
use crate::catalog::RuntimeHealth;

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
    /// The human text of the state, for example `Up 2 hours (healthy)`.
    #[serde(rename = "Status", default)]
    status: String,
    /// The health of the container. Only recent Engine APIs give this member.
    #[serde(rename = "Health", default)]
    health: Option<ContainerHealth>,
}

#[derive(Deserialize)]
struct ContainerHealth {
    #[serde(rename = "Status", default)]
    status: String,
}

/// The result of the `HEALTHCHECK` of a running container. The `Health` member has priority. An
/// older Engine API gives only the end of the `Status` text. A container without a
/// `HEALTHCHECK` gives `None`.
fn runtime_health(health: Option<&ContainerHealth>, status: &str) -> Option<RuntimeHealth> {
    let from_word = |word: &str| match word {
        "healthy" => Some(RuntimeHealth::Healthy),
        "unhealthy" => Some(RuntimeHealth::Unhealthy),
        "starting" => Some(RuntimeHealth::Starting),
        _ => None,
    };
    if let Some(health) = health.filter(|h| !h.status.is_empty()) {
        return from_word(&health.status);
    }
    let status = status.trim_end();
    if status.ends_with("(healthy)") {
        Some(RuntimeHealth::Healthy)
    } else if status.ends_with("(unhealthy)") {
        Some(RuntimeHealth::Unhealthy)
    } else if status.ends_with("(health: starting)") {
        Some(RuntimeHealth::Starting)
    } else {
        None
    }
}

pub(super) fn parse_containers(body: &[u8]) -> Result<Vec<Workload>, String> {
    let containers: Vec<Container> =
        serde_json::from_slice(body).map_err(|e| format!("unexpected Docker answer: {e}"))?;
    Ok(containers
        .into_iter()
        .filter_map(|container| {
            let labels = container.labels.unwrap_or_default();
            // A Swarm task container has a new name for each task. The name of its Swarm
            // service stays the same.
            let origin = labels
                .get("com.docker.compose.service")
                .or_else(|| labels.get("com.docker.swarm.service.name"))
                .cloned()
                .or_else(|| {
                    container
                        .names
                        .first()
                        .map(|n| n.trim_start_matches('/').to_string())
                })?;
            let running = container.state == "running";
            let runtime = running
                .then(|| runtime_health(container.health.as_ref(), &container.status))
                .flatten();
            Some(Workload {
                origin,
                labels,
                hint: Hint::Running(running),
                runtime,
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
    fn the_health_member_gives_the_runtime_health_of_a_running_container() {
        let body = br#"[
          {"Names":["/a"],"State":"running","Status":"Up 1 hour (unhealthy)","Health":{"Status":"healthy","FailingStreak":0}},
          {"Names":["/b"],"State":"running","Status":"Up 1 hour","Health":{"Status":"unhealthy","FailingStreak":3}},
          {"Names":["/c"],"State":"running","Status":"Up 3 seconds","Health":{"Status":"starting"}},
          {"Names":["/d"],"State":"running","Status":"Up 1 hour","Health":{"Status":"none"}},
          {"Names":["/e"],"State":"running","Status":"Up 1 hour (healthy)","Health":{"Status":""}}
        ]"#;
        let runtime: Vec<_> = parse_containers(body)
            .unwrap()
            .into_iter()
            .map(|w| w.runtime)
            .collect();
        assert_eq!(
            runtime,
            [
                Some(RuntimeHealth::Healthy),
                Some(RuntimeHealth::Unhealthy),
                Some(RuntimeHealth::Starting),
                None,
                Some(RuntimeHealth::Healthy),
            ],
            "the member wins over the text; an empty member gives the text"
        );
    }

    #[test]
    fn the_end_of_the_status_text_gives_the_runtime_health_without_the_member() {
        let body = br#"[
          {"Names":["/a"],"State":"running","Status":"Up 2 hours (healthy)"},
          {"Names":["/b"],"State":"running","Status":"Up 2 hours (unhealthy)"},
          {"Names":["/c"],"State":"running","Status":"Up 4 seconds (health: starting)"},
          {"Names":["/d"],"State":"running","Status":"Up 2 hours"},
          {"Names":["/e"],"State":"exited","Status":"Exited (1) 3 minutes ago (unhealthy)","Health":{"Status":"unhealthy"}},
          {"Names":["/f"],"State":"running"}
        ]"#;
        let runtime: Vec<_> = parse_containers(body)
            .unwrap()
            .into_iter()
            .map(|w| w.runtime)
            .collect();
        assert_eq!(
            runtime,
            [
                Some(RuntimeHealth::Healthy),
                Some(RuntimeHealth::Unhealthy),
                Some(RuntimeHealth::Starting),
                None,
                None,
                None,
            ],
            "no HEALTHCHECK and a stopped container give no runtime health"
        );
    }

    #[test]
    fn a_swarm_task_is_named_after_its_swarm_service() {
        let body = br#"[
          {"Names":["/web.1.x8k2"],"State":"running","Labels":{"com.docker.swarm.service.name":"web"}},
          {"Names":["/api.1.p3q9"],"State":"running","Labels":{"com.docker.swarm.service.name":"stack_api","com.docker.compose.service":"api"}}
        ]"#;
        let workloads = parse_containers(body).unwrap();
        assert_eq!(workloads[0].origin, "web");
        assert_eq!(
            workloads[1].origin, "api",
            "the Compose service has priority"
        );
    }

    #[test]
    fn a_non_array_answer_is_an_error() {
        assert!(parse_containers(br#"{"message":"nope"}"#).is_err());
    }
}
