//! The actions that each service publishes at `/.well-known/access-actions` (contract 11.4).
//!
//! The settings pages show these actions as check boxes on the page of a user, next to `access`.
//! A service without the document has only `access`: for it, `access` is the full permission.
//!
//! The server reads the document of each service of the catalogue, with a timeout and a size
//! limit, and keeps the result. It reads it again after `access.actions_refresh`. A `404` means
//! that the service does not publish actions. A different failure keeps the last good list and
//! gives a problem for the settings pages.
//!
//! Interface: [`Registry`] (the results, shared), [`run`] (the refresh loop), [`parse`] (one
//! document, checked).

use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use serde::Deserialize;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use crate::catalog::Catalog;
use crate::grants::{check_action, ACCESS};

/// The largest document that the server reads.
const MAX_DOCUMENT_BYTES: usize = 64 * 1024;
/// The largest number of actions of one service.
pub const MAX_ACTIONS: usize = 64;
const MAX_DESCRIPTION_CHARS: usize = 300;
const MAX_RESOURCE_TYPES: usize = 16;
const MAX_CONCURRENT_READS: usize = 8;
/// How often the loop looks for a document that is due.
const TICK: Duration = Duration::from_secs(5);

/// One action of a service.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Action {
    pub name: String,
    pub resource_types: Vec<String>,
    pub description: String,
}

#[derive(Clone, Debug, Default)]
struct Entry {
    /// The URL that gave `actions`.
    url: Option<String>,
    actions: Vec<Action>,
    /// The last failure that was not a `404`.
    problem: Option<String>,
    read_at: Option<Instant>,
}

/// The actions of each service, by service id.
#[derive(Default)]
pub struct Registry {
    entries: RwLock<HashMap<String, Entry>>,
}

impl Registry {
    /// The actions of a service. Empty: the service has only `access`.
    pub fn actions(&self, service: &str) -> Vec<Action> {
        self.read()
            .get(service)
            .map(|entry| entry.actions.clone())
            .unwrap_or_default()
    }

    /// `access`, then the names of the actions of the service.
    pub fn action_names(&self, service: &str) -> Vec<String> {
        std::iter::once(ACCESS.to_string())
            .chain(self.actions(service).into_iter().map(|a| a.name))
            .collect()
    }

    /// The problems of the last reads, sorted by service id.
    pub fn problems(&self) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = self
            .read()
            .iter()
            .filter_map(|(id, entry)| entry.problem.clone().map(|p| (id.clone(), p)))
            .collect();
        out.sort();
        out
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, HashMap<String, Entry>> {
        self.entries.read().unwrap_or_else(|p| p.into_inner())
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, HashMap<String, Entry>> {
        self.entries.write().unwrap_or_else(|p| p.into_inner())
    }

    /// Record the result of one read.
    fn record(&self, service: &str, url: &str, outcome: Outcome, now: Instant) {
        let mut entries = self.write();
        let entry = entries.entry(service.to_string()).or_default();
        entry.read_at = Some(now);
        match outcome {
            Outcome::Actions(actions) => {
                entry.url = Some(url.to_string());
                entry.actions = actions;
                entry.problem = None;
            }
            Outcome::Absent => {
                entry.url = Some(url.to_string());
                entry.actions = Vec::new();
                entry.problem = None;
            }
            Outcome::Failed(problem) => {
                // A different URL: the last list belongs to another document.
                if entry.url.as_deref() != Some(url) {
                    entry.actions = Vec::new();
                    entry.url = Some(url.to_string());
                }
                entry.problem = Some(problem);
            }
        }
    }

    /// Keep only the services of the catalogue, and the services without a document URL lose
    /// their actions.
    fn retain(&self, wanted: &HashMap<String, Option<String>>) {
        let mut entries = self.write();
        entries.retain(|id, _| wanted.contains_key(id));
        for (id, url) in wanted {
            if url.is_none() {
                entries.remove(id);
            }
        }
    }

    /// The services whose document is due: never read, read from another URL, or read more than
    /// `refresh` ago.
    fn due(
        &self,
        wanted: &HashMap<String, Option<String>>,
        refresh: Duration,
        now: Instant,
    ) -> Vec<(String, String)> {
        let entries = self.read();
        let mut out: Vec<(String, String)> = wanted
            .iter()
            .filter_map(|(id, url)| {
                let url = url.as_ref()?;
                let fresh = entries.get(id).is_some_and(|entry| {
                    entry.url.as_deref() == Some(url.as_str())
                        && entry
                            .read_at
                            .is_some_and(|at| now.saturating_duration_since(at) < refresh)
                });
                (!fresh).then(|| (id.clone(), url.clone()))
            })
            .collect();
        out.sort();
        out
    }

    #[cfg(test)]
    pub fn set(&self, service: &str, actions: Vec<Action>) {
        self.record(
            service,
            "http://test/.well-known/access-actions",
            Outcome::Actions(actions),
            Instant::now(),
        );
    }
}

/// The result of one read.
#[derive(Debug, PartialEq, Eq)]
enum Outcome {
    Actions(Vec<Action>),
    /// `404`: the service does not publish actions.
    Absent,
    Failed(String),
}

#[derive(Deserialize)]
struct Document {
    server: String,
    actions: Vec<RawAction>,
}

#[derive(Deserialize)]
struct RawAction {
    name: String,
    #[serde(default)]
    resource_types: Vec<String>,
    #[serde(default)]
    description: String,
}

/// Check one document of the service `service`. Any bad action makes the whole document bad.
pub fn parse(service: &str, body: &[u8]) -> Result<Vec<Action>, String> {
    let document: Document =
        serde_json::from_slice(body).map_err(|e| format!("the document is not valid: {e}"))?;
    if document.server != service {
        return Err(format!(
            "the document names the server {:?}, the id of the service is {service:?}: set portal.id to the name of the server",
            document.server
        ));
    }
    if document.actions.len() > MAX_ACTIONS {
        return Err(format!("more than {MAX_ACTIONS} actions"));
    }
    let mut out: Vec<Action> = Vec::with_capacity(document.actions.len());
    for raw in document.actions {
        check_action(&raw.name)?;
        if raw.name == ACCESS {
            return Err("the action access is for the access gateway: remove it from the list".into());
        }
        if out.iter().any(|a| a.name == raw.name) {
            return Err(format!("the action {:?} is in the list two times", raw.name));
        }
        if raw.resource_types.len() > MAX_RESOURCE_TYPES {
            return Err(format!("the action {:?} has more than {MAX_RESOURCE_TYPES} resource types", raw.name));
        }
        for kind in &raw.resource_types {
            let valid = !kind.is_empty()
                && kind.len() <= 32
                && kind
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
            if !valid {
                return Err(format!(
                    "the action {:?} has a bad resource type {kind:?}",
                    raw.name
                ));
            }
        }
        if raw.description.chars().count() > MAX_DESCRIPTION_CHARS
            || raw.description.chars().any(char::is_control)
        {
            return Err(format!(
                "the description of the action {:?} is longer than {MAX_DESCRIPTION_CHARS} characters or has a control character",
                raw.name
            ));
        }
        out.push(Action {
            name: raw.name,
            resource_types: raw.resource_types,
            description: raw.description,
        });
    }
    Ok(out)
}

/// Read the document of one service.
async fn read(client: &reqwest::Client, service: &str, url: &str) -> Outcome {
    let response = match client
        .get(url)
        .header("accept", "application/json")
        .send()
        .await
    {
        Ok(response) => response,
        Err(e) => return Outcome::Failed(format!("cannot read {url}: {e}")),
    };
    match response.status().as_u16() {
        200 => {}
        404 => return Outcome::Absent,
        status => return Outcome::Failed(format!("{url} gave the status {status}")),
    }
    match crate::discovery::read_capped(response, MAX_DOCUMENT_BYTES).await {
        Ok(body) => match parse(service, &body) {
            Ok(actions) => Outcome::Actions(actions),
            Err(e) => Outcome::Failed(format!("{url}: {e}")),
        },
        Err(e) => Outcome::Failed(format!("{url}: {e}")),
    }
}

/// The document URL of each service of the catalogue.
fn wanted(catalog: &Catalog) -> HashMap<String, Option<String>> {
    catalog
        .groups()
        .iter()
        .flat_map(|g| g.services.iter())
        .map(|s| (s.id.clone(), s.actions_url.clone()))
        .collect()
}

/// One round: read every document that is due.
async fn round(
    registry: &Registry,
    client: &reqwest::Client,
    catalog: &Catalog,
    refresh: Duration,
) {
    let wanted = wanted(catalog);
    registry.retain(&wanted);
    let due = registry.due(&wanted, refresh, Instant::now());
    let limit = Arc::new(Semaphore::new(MAX_CONCURRENT_READS));
    let mut reads: JoinSet<(String, String, Outcome)> = JoinSet::new();
    for (service, url) in due {
        let client = client.clone();
        let limit = Arc::clone(&limit);
        reads.spawn(async move {
            let _permit = limit.acquire_owned().await;
            let outcome = read(&client, &service, &url).await;
            (service, url, outcome)
        });
    }
    while let Some(result) = reads.join_next().await {
        if let Ok((service, url, outcome)) = result {
            registry.record(&service, &url, outcome, Instant::now());
        }
    }
}

/// The refresh loop. `catalog` gives the current catalogue. Runs until the process ends.
pub async fn run(
    registry: Arc<Registry>,
    catalog: impl Fn() -> Arc<Catalog> + Send + 'static,
    timeout: Duration,
    refresh: Duration,
) {
    let client = match crate::discovery::http_client(timeout) {
        Ok(client) => client,
        Err(_) => return,
    };
    loop {
        round(&registry, &client, &catalog(), refresh).await;
        tokio::time::sleep(TICK).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::parse_labels;
    use std::collections::BTreeMap;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    fn doc(server: &str, actions: &str) -> Vec<u8> {
        format!(r#"{{"server":"{server}","actions":[{actions}]}}"#).into_bytes()
    }

    #[test]
    fn a_good_document_gives_its_actions() {
        let actions = parse(
            "db",
            &doc(
                "db",
                r#"{"name":"query.run.read","resource_types":["service","database"],"description":"Run read-only queries."},{"name":"system.read"}"#,
            ),
        )
        .unwrap();
        assert_eq!(actions.len(), 2);
        assert_eq!(actions[0].name, "query.run.read");
        assert_eq!(actions[0].resource_types, ["service", "database"]);
        assert_eq!(actions[1].description, "");
    }

    #[test]
    fn a_bad_document_is_refused_as_a_whole() {
        for (server, actions) in [
            ("other", r#"{"name":"a"}"#),
            ("db", r#"{"name":"access"}"#),
            ("db", r#"{"name":"Query.Run"}"#),
            ("db", r#"{"name":"a"},{"name":"a"}"#),
            ("db", r#"{"name":"a","resource_types":["Data base"]}"#),
            ("db", r#"{"name":"a","description":"line\nbreak"}"#),
        ] {
            assert!(parse("db", &doc(server, actions)).is_err(), "{server} {actions}");
        }
        assert!(parse("db", b"not json").is_err());
        let many = (0..65).map(|i| format!(r#"{{"name":"a{i}"}}"#)).collect::<Vec<_>>().join(",");
        assert!(parse("db", &doc("db", &many)).is_err());
    }

    /// Serves canned answers on a loopback port: `(status line, body)` for each connection.
    fn serve(answers: Vec<(&'static str, String)>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for (stream, (status, body)) in listener.incoming().zip(answers) {
                let Ok(mut stream) = stream else { return };
                let mut buffer = [0u8; 1024];
                let _ = stream.read(&mut buffer);
                let _ = write!(
                    stream,
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
            }
        });
        format!("http://127.0.0.1:{port}")
    }

    fn catalog_with(id: &str, base: &str) -> Catalog {
        let labels: BTreeMap<String, String> = [
            ("portal.name", "Db"),
            ("portal.id", id),
            ("portal.href", "/db/"),
            ("portal.health", &format!("{base}/healthz")),
        ]
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        Catalog::build(vec![parse_labels("db", &labels).unwrap().unwrap()], Vec::new())
    }

    #[test]
    fn the_loop_reads_the_document_keeps_it_on_a_failure_and_drops_it_on_a_404() {
        let good = String::from_utf8(doc("db", r#"{"name":"query.run.read"}"#)).unwrap();
        let base = serve(vec![
            ("200 OK", good),
            ("500 Internal Server Error", String::new()),
            ("404 Not Found", String::new()),
        ]);
        let catalog = catalog_with("db", &base);
        let registry = Registry::default();
        let client = crate::discovery::http_client(Duration::from_secs(2)).unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(round(&registry, &client, &catalog, Duration::ZERO));
        assert_eq!(registry.action_names("db"), ["access", "query.run.read"]);
        runtime.block_on(round(&registry, &client, &catalog, Duration::ZERO));
        assert_eq!(registry.action_names("db"), ["access", "query.run.read"], "kept");
        assert_eq!(registry.problems().len(), 1);
        runtime.block_on(round(&registry, &client, &catalog, Duration::ZERO));
        assert_eq!(registry.action_names("db"), ["access"], "a 404: only access");
        assert!(registry.problems().is_empty());
        // A service that left the catalogue leaves the registry.
        runtime.block_on(round(&registry, &client, &Catalog::default(), Duration::ZERO));
        assert!(registry.read().is_empty());
    }

    #[test]
    fn a_document_is_due_when_never_read_or_older_than_the_refresh() {
        let registry = Registry::default();
        let wanted: HashMap<String, Option<String>> = [
            ("a".to_string(), Some("http://a/x".to_string())),
            ("b".to_string(), None),
        ]
        .into_iter()
        .collect();
        let now = Instant::now();
        assert_eq!(registry.due(&wanted, Duration::from_secs(60), now).len(), 1);
        registry.record("a", "http://a/x", Outcome::Absent, now);
        assert!(registry.due(&wanted, Duration::from_secs(60), now).is_empty());
        assert_eq!(
            registry
                .due(&wanted, Duration::from_secs(60), now + Duration::from_secs(61))
                .len(),
            1
        );
    }
}
