//! The actions and the resource tree that each service publishes at
//! `/.well-known/access-actions` (contract 11.4).
//!
//! The settings pages show the actions as check boxes on the page of a user, next to `access`.
//! A service without the document has only `access`: for it, `access` is the full permission.
//!
//! The document can declare a tree of resource types below the root type `service`, in its
//! top-level `resource_types`. Each type has one parent: `service` or another declared type. A
//! document without this list has one level: each type of an action, other than `service`, has
//! the parent `service`. A grant and a PDP request must follow the parents (see [`Document`]).
//!
//! The server reads the document of each service of the catalogue, with a timeout and a size
//! limit, and keeps the result. It reads it again after `access.actions_refresh`. A `404` means
//! that the service does not publish actions. A different failure keeps the last good document
//! and gives a problem for the settings pages.
//!
//! The PDP uses the last known document to check the chain of a resource and to decide which
//! grants apply (see [`Document::check_grant`]). A service without a known document has no
//! applicable grant below the service.
//!
//! Interface: [`Registry`] (the results, shared), [`run`] (the refresh loop), [`parse`] (one
//! document, checked), [`Document`] (the tree and the actions of one service).

use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use serde::Deserialize;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use crate::catalog::Catalog;
use crate::grants::{check_action, check_resource_type, Grant, ACCESS, MAX_DEPTH, SERVICE_TYPE};

/// The largest document that the server reads.
const MAX_DOCUMENT_BYTES: usize = 64 * 1024;
/// The largest number of actions of one service.
pub const MAX_ACTIONS: usize = 64;
const MAX_DESCRIPTION_CHARS: usize = 300;
/// The largest number of types in the list of one action, and in the top-level list.
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

/// One resource type below the root `service`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResourceType {
    pub name: String,
    /// `service` or another declared type.
    pub parent: String,
    pub description: String,
}

/// The checked document of one service: its tree of types and its actions. The tree has no
/// cycle, and each type is at a depth of [`MAX_DEPTH`] or less. A document of one level has a
/// tree too: each of its types has the parent `service`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Document {
    /// The declared types, in the order of the document.
    pub types: Vec<ResourceType>,
    pub actions: Vec<Action>,
}

/// One level of the settings form: a declared type, the chain of types from the service to it,
/// and the actions that list it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Level {
    /// The types from depth 1 to the type itself.
    pub chain: Vec<String>,
    pub actions: Vec<String>,
}

impl Document {
    /// A document of one level, as a document without the top-level `resource_types` gives.
    pub fn one_level(actions: Vec<Action>) -> Self {
        let mut names: Vec<String> = Vec::new();
        for action in &actions {
            for kind in action.sub_resource_types() {
                if !names.iter().any(|n| n == kind) {
                    names.push(kind.to_string());
                }
            }
        }
        Self {
            types: names
                .into_iter()
                .map(|name| ResourceType {
                    name,
                    parent: SERVICE_TYPE.to_string(),
                    description: String::new(),
                })
                .collect(),
            actions,
        }
    }

    /// The parent of a declared type. `None`: the document does not declare the type.
    pub fn parent(&self, kind: &str) -> Option<&str> {
        self.types
            .iter()
            .find(|t| t.name == kind)
            .map(|t| t.parent.as_str())
    }

    /// The chain of types from depth 1 to `kind`. `None`: the document does not declare the type.
    pub fn chain(&self, kind: &str) -> Option<Vec<&str>> {
        let mut chain = vec![self.types.iter().find(|t| t.name == kind)?.name.as_str()];
        while chain.len() <= MAX_DEPTH {
            let parent = self.parent(chain[chain.len() - 1])?;
            if parent == SERVICE_TYPE {
                chain.reverse();
                return Some(chain);
            }
            chain.push(parent);
        }
        None
    }

    /// Whether the types, from depth 1 down, follow the declared parents. An empty list is the
    /// service itself: it follows.
    pub fn follows<'a>(&self, kinds: impl IntoIterator<Item = &'a str>) -> bool {
        let mut parent = SERVICE_TYPE;
        let mut depth = 0;
        for kind in kinds {
            depth += 1;
            if depth > MAX_DEPTH || self.parent(kind) != Some(parent) {
                return false;
            }
            parent = kind;
        }
        true
    }

    /// Check a grant against the document. A grant on the whole service needs no declaration. A
    /// grant below the service needs an action of this name that lists the type of the last
    /// segment, and a path that follows the declared parents.
    pub fn check_grant(&self, grant: &Grant) -> Result<(), String> {
        if grant.path.is_empty() {
            return Ok(());
        }
        let Some(action) = self.actions.iter().find(|a| a.name == grant.action) else {
            return Err(format!(
                "the service {:?} does not publish the action {:?}",
                grant.service, grant.action
            ));
        };
        let kind = grant.kind();
        if !action.sub_resource_types().any(|k| k == kind) {
            return Err(format!(
                "the action {:?} of the service {:?} does not list the resource type {kind:?}",
                grant.action, grant.service
            ));
        }
        if !self.follows(grant.path.iter().map(|s| s.kind.as_str())) {
            let expected = self
                .chain(kind)
                .map(|c| c.join(" then "))
                .unwrap_or_default();
            return Err(format!(
                "the path of the grant does not follow the types of the service {:?}: expected {expected}",
                grant.service
            ));
        }
        Ok(())
    }

    /// The levels that a grant below the service can name: each declared type that an action
    /// lists, in the order of the document.
    pub fn levels(&self) -> Vec<Level> {
        self.types
            .iter()
            .filter_map(|t| {
                let chain = self.chain(&t.name)?;
                let actions: Vec<String> = self
                    .actions
                    .iter()
                    .filter(|a| a.sub_resource_types().any(|k| k == t.name))
                    .map(|a| a.name.clone())
                    .collect();
                (!actions.is_empty()).then(|| Level {
                    chain: chain.into_iter().map(str::to_string).collect(),
                    actions,
                })
            })
            .collect()
    }
}

#[derive(Clone, Debug, Default)]
struct Entry {
    /// The URL that gave `document`.
    url: Option<String>,
    document: Arc<Document>,
    /// The last failure that was not a `404`.
    problem: Option<String>,
    /// `document` is the answer of the service: a good document or a `404`. A failure on a new
    /// URL makes it `false`.
    known: bool,
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
            .map(|entry| entry.document.actions.clone())
            .unwrap_or_default()
    }

    /// The document of a service when the server knows it: the last read gave a good document or
    /// a `404`. A `404` gives an empty document. `None`: the service is not in the catalogue, or
    /// no read has answered yet. The PDP fails closed on `None`.
    pub fn declared(&self, service: &str) -> Option<Arc<Document>> {
        self.read()
            .get(service)
            .filter(|entry| entry.known)
            .map(|entry| Arc::clone(&entry.document))
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

    /// Record the result of one read. Gives the new document when the known document of the
    /// service changes: a first answer, or an answer that differs from the last one.
    fn record(
        &self,
        service: &str,
        url: &str,
        outcome: Outcome,
        now: Instant,
    ) -> Option<Arc<Document>> {
        let mut entries = self.write();
        let entry = entries.entry(service.to_string()).or_default();
        entry.read_at = Some(now);
        let before = entry.known.then(|| Arc::clone(&entry.document));
        match outcome {
            Outcome::Document(document) => {
                entry.url = Some(url.to_string());
                entry.document = Arc::new(document);
                entry.problem = None;
                entry.known = true;
            }
            Outcome::Absent => {
                entry.url = Some(url.to_string());
                entry.document = Arc::default();
                entry.problem = None;
                entry.known = true;
            }
            Outcome::Failed(problem) => {
                // A different URL: the last list belongs to another document.
                if entry.url.as_deref() != Some(url) {
                    entry.document = Arc::default();
                    entry.url = Some(url.to_string());
                    entry.known = false;
                }
                entry.problem = Some(problem);
            }
        }
        let after = entry.known.then(|| Arc::clone(&entry.document));
        match (before, after) {
            (Some(before), Some(after)) if before == after => None,
            (_, after) => after,
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

    /// A failed read of the URL that `set` uses.
    #[cfg(test)]
    pub fn fail(&self, service: &str) {
        self.record(
            service,
            TEST_URL,
            Outcome::Failed("the read failed".into()),
            Instant::now(),
        );
    }

    /// A good document of one level, read from the URL that `fail` uses.
    #[cfg(test)]
    pub fn set(&self, service: &str, actions: Vec<Action>) {
        self.set_document(service, Document::one_level(actions));
    }

    /// A good document, read from the URL that `fail` uses.
    #[cfg(test)]
    pub fn set_document(&self, service: &str, document: Document) {
        self.record(
            service,
            TEST_URL,
            Outcome::Document(document),
            Instant::now(),
        );
    }
}

#[cfg(test)]
const TEST_URL: &str = "http://test/.well-known/access-actions";

/// The result of one read.
#[derive(Debug, PartialEq, Eq)]
enum Outcome {
    Document(Document),
    /// `404`: the service does not publish actions.
    Absent,
    Failed(String),
}

#[derive(Deserialize)]
struct RawDocument {
    server: String,
    /// `None`: a document of one level.
    #[serde(default)]
    resource_types: Option<Vec<RawType>>,
    actions: Vec<RawAction>,
}

#[derive(Deserialize)]
struct RawType {
    name: String,
    parent: String,
    #[serde(default)]
    description: String,
}

#[derive(Deserialize)]
struct RawAction {
    name: String,
    #[serde(default)]
    resource_types: Vec<String>,
    #[serde(default)]
    description: String,
}

impl Action {
    /// The resource types of the action that a grant below the service can name: the listed
    /// types without `service`.
    pub fn sub_resource_types(&self) -> impl Iterator<Item = &str> {
        self.resource_types
            .iter()
            .map(String::as_str)
            .filter(|kind| *kind != SERVICE_TYPE)
    }
}

fn check_description(description: &str, what: &str) -> Result<(), String> {
    if description.chars().count() > MAX_DESCRIPTION_CHARS
        || description.chars().any(char::is_control)
    {
        return Err(format!(
            "the description of {what} is longer than {MAX_DESCRIPTION_CHARS} characters or has a control character"
        ));
    }
    Ok(())
}

/// Check the top-level `resource_types`: valid names, not `service`, each name one time, each
/// parent declared, no cycle, a depth of [`MAX_DEPTH`] or less.
fn check_types(raw: Vec<RawType>) -> Result<Vec<ResourceType>, String> {
    if raw.len() > MAX_RESOURCE_TYPES {
        return Err(format!("more than {MAX_RESOURCE_TYPES} resource types"));
    }
    let mut types: Vec<ResourceType> = Vec::with_capacity(raw.len());
    for t in raw {
        if check_resource_type(&t.name).is_err() || t.name == SERVICE_TYPE {
            return Err(format!(
                "the resource type {:?} is not valid: a-z first, then a-z, 0-9 and _, 1 to 32 characters, not service",
                t.name
            ));
        }
        if types.iter().any(|known| known.name == t.name) {
            return Err(format!(
                "the resource type {:?} is in the list two times",
                t.name
            ));
        }
        check_description(&t.description, &format!("the resource type {:?}", t.name))?;
        types.push(ResourceType {
            name: t.name,
            parent: t.parent,
            description: t.description,
        });
    }
    let document = Document {
        types,
        actions: Vec::new(),
    };
    for t in &document.types {
        if t.parent != SERVICE_TYPE && document.parent(&t.parent).is_none() {
            return Err(format!(
                "the parent {:?} of the resource type {:?} is not service and is not declared",
                t.parent, t.name
            ));
        }
        if document.chain(&t.name).is_none() {
            return Err(format!(
                "the resource type {:?} is in a cycle or deeper than {MAX_DEPTH} levels below service",
                t.name
            ));
        }
    }
    Ok(document.types)
}

/// Check one document of the service `service`. Any bad action or type makes the whole document
/// bad.
pub fn parse(service: &str, body: &[u8]) -> Result<Document, String> {
    let document: RawDocument =
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
    let declared = document.resource_types.map(check_types).transpose()?;
    let mut out: Vec<Action> = Vec::with_capacity(document.actions.len());
    for raw in document.actions {
        check_action(&raw.name)?;
        if raw.name == ACCESS {
            return Err(
                "the action access is for the access gateway: remove it from the list".into(),
            );
        }
        if out.iter().any(|a| a.name == raw.name) {
            return Err(format!(
                "the action {:?} is in the list two times",
                raw.name
            ));
        }
        if raw.resource_types.len() > MAX_RESOURCE_TYPES {
            return Err(format!(
                "the action {:?} has more than {MAX_RESOURCE_TYPES} resource types",
                raw.name
            ));
        }
        for (index, kind) in raw.resource_types.iter().enumerate() {
            if check_resource_type(kind).is_err() {
                return Err(format!(
                    "the action {:?} has a bad resource type {kind:?}",
                    raw.name
                ));
            }
            if raw.resource_types[..index].contains(kind) {
                return Err(format!(
                    "the action {:?} lists the resource type {kind:?} two times",
                    raw.name
                ));
            }
            if let Some(types) = &declared {
                if kind != SERVICE_TYPE && !types.iter().any(|t| t.name == *kind) {
                    return Err(format!(
                        "the action {:?} lists the resource type {kind:?}, which the top-level resource_types does not declare",
                        raw.name
                    ));
                }
            }
        }
        check_description(&raw.description, &format!("the action {:?}", raw.name))?;
        out.push(Action {
            name: raw.name,
            resource_types: raw.resource_types,
            description: raw.description,
        });
    }
    Ok(match declared {
        Some(types) => Document {
            types,
            actions: out,
        },
        None => Document::one_level(out),
    })
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
            Ok(document) => Outcome::Document(document),
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

/// One round: read every document that is due. Gives the services whose known document changed,
/// with the new document.
async fn round(
    registry: &Registry,
    client: &reqwest::Client,
    catalog: &Catalog,
    refresh: Duration,
) -> Vec<(String, Arc<Document>)> {
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
    let mut changed = Vec::new();
    while let Some(result) = reads.join_next().await {
        if let Ok((service, url, outcome)) = result {
            if let Some(document) = registry.record(&service, &url, outcome, Instant::now()) {
                changed.push((service, document));
            }
        }
    }
    changed
}

/// The refresh loop. `catalog` gives the current catalogue. `changed` receives each service whose
/// known document changed: the first answer after the start, and each different answer. Runs
/// until the process ends.
pub async fn run(
    registry: Arc<Registry>,
    catalog: impl Fn() -> Arc<Catalog> + Send + 'static,
    changed: impl Fn(&str, &Document) + Send + 'static,
    timeout: Duration,
    refresh: Duration,
) {
    let client = match crate::discovery::http_client(timeout) {
        Ok(client) => client,
        Err(_) => return,
    };
    loop {
        for (service, document) in round(&registry, &client, &catalog(), refresh).await {
            changed(&service, &document);
        }
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
        let document = parse(
            "db",
            &doc(
                "db",
                r#"{"name":"query.run.read","resource_types":["service","database"],"description":"Run read-only queries."},{"name":"system.read"}"#,
            ),
        )
        .unwrap();
        let actions = &document.actions;
        assert_eq!(actions.len(), 2);
        assert_eq!(actions[0].name, "query.run.read");
        assert_eq!(actions[0].resource_types, ["service", "database"]);
        assert_eq!(actions[1].description, "");
        assert_eq!(
            document.types,
            [ResourceType {
                name: "database".into(),
                parent: "service".into(),
                description: String::new()
            }],
            "a document of one level: each type has the parent service"
        );
    }

    fn tree(types: &str, actions: &str) -> Vec<u8> {
        format!(r#"{{"server":"db","resource_types":[{types}],"actions":[{actions}]}}"#)
            .into_bytes()
    }

    const HOST_DATABASE: &str = r#"{"name":"host","parent":"service","description":"One server."},{"name":"database","parent":"host","description":"One database of a host."}"#;
    const TREE_ACTIONS: &str = r#"{"name":"query.run.read","resource_types":["service","host","database"]},{"name":"system.read","resource_types":["service","host"]},{"name":"mcp.keys.read","resource_types":["service"]}"#;

    #[test]
    fn a_document_with_a_tree_gives_its_types_and_their_chains() {
        let document = parse("db", &tree(HOST_DATABASE, TREE_ACTIONS)).unwrap();
        assert_eq!(document.types.len(), 2);
        assert_eq!(document.parent("database"), Some("host"));
        assert_eq!(document.parent("host"), Some("service"));
        assert_eq!(document.parent("table"), None);
        assert_eq!(document.chain("database").unwrap(), ["host", "database"]);
        assert_eq!(document.chain("host").unwrap(), ["host"]);
        assert!(document.chain("service").is_none());
        assert!(document.follows(["host", "database"]));
        assert!(document.follows([]));
        assert!(
            !document.follows(["database"]),
            "database has the parent host"
        );
        assert!(!document.follows(["host", "host"]));
        assert!(!document.follows(["host", "table"]));
        assert_eq!(
            document.levels(),
            [
                Level {
                    chain: vec!["host".into()],
                    actions: vec!["query.run.read".into(), "system.read".into()]
                },
                Level {
                    chain: vec!["host".into(), "database".into()],
                    actions: vec!["query.run.read".into()]
                }
            ]
        );
        // The order of the list does not matter: a parent can come after its child.
        let reversed = r#"{"name":"database","parent":"host"},{"name":"host","parent":"service"}"#;
        let document = parse("db", &tree(reversed, TREE_ACTIONS)).unwrap();
        assert_eq!(document.chain("database").unwrap(), ["host", "database"]);
        // Four levels below service.
        let deep = r#"{"name":"a","parent":"service"},{"name":"b","parent":"a"},{"name":"c","parent":"b"},{"name":"d","parent":"c"}"#;
        let document = parse("db", &tree(deep, r#"{"name":"x","resource_types":["d"]}"#)).unwrap();
        assert_eq!(document.chain("d").unwrap(), ["a", "b", "c", "d"]);
        // An empty tree: the actions name only service.
        let document = parse(
            "db",
            &tree("", r#"{"name":"x","resource_types":["service"]}"#),
        )
        .unwrap();
        assert!(document.types.is_empty());
    }

    #[test]
    fn a_bad_tree_makes_the_whole_document_bad() {
        let actions = r#"{"name":"x","resource_types":["service"]}"#;
        for (types, why) in [
            (
                r#"{"name":"service","parent":"service"}"#,
                "service is the root",
            ),
            (r#"{"name":"Host","parent":"service"}"#, "upper case"),
            (r#"{"name":"1host","parent":"service"}"#, "a digit first"),
            (
                r#"{"name":"host","parent":"service"},{"name":"host","parent":"service"}"#,
                "two times",
            ),
            (
                r#"{"name":"database","parent":"host"}"#,
                "a parent that is not declared",
            ),
            (
                r#"{"name":"a","parent":"b"},{"name":"b","parent":"a"}"#,
                "a cycle",
            ),
            (r#"{"name":"a","parent":"a"}"#, "its own parent"),
            (
                r#"{"name":"a","parent":"service"},{"name":"b","parent":"a"},{"name":"c","parent":"b"},{"name":"d","parent":"c"},{"name":"e","parent":"d"}"#,
                "five levels",
            ),
            (
                r#"{"name":"host","parent":"service","description":"a\nb"}"#,
                "a control character",
            ),
            (r#"{"name":"host"}"#, "no parent"),
        ] {
            assert!(parse("db", &tree(types, actions)).is_err(), "{why}");
        }
        assert!(
            parse(
                "db",
                &tree(HOST_DATABASE, r#"{"name":"x","resource_types":["table"]}"#)
            )
            .is_err(),
            "an action lists a type that the tree does not declare"
        );
        assert!(
            parse(
                "db",
                &tree(
                    HOST_DATABASE,
                    r#"{"name":"x","resource_types":["host","host"]}"#
                )
            )
            .is_err(),
            "a type two times in one action"
        );
        let many = (0..17)
            .map(|i| format!(r#"{{"name":"t{i}","parent":"service"}}"#))
            .collect::<Vec<_>>()
            .join(",");
        assert!(parse("db", &tree(&many, actions)).is_err());
    }

    #[test]
    fn a_grant_must_follow_the_tree_and_name_a_type_of_its_action() {
        let document = parse("db", &tree(HOST_DATABASE, TREE_ACTIONS)).unwrap();
        let check = |text: &str| document.check_grant(&Grant::parse(text).unwrap());
        assert!(check("query.run.read on service:db").is_ok());
        assert!(
            check("unknown on service:db").is_ok(),
            "a grant on the service needs no declaration"
        );
        assert!(check("query.run.read on service:db/host:h").is_ok());
        assert!(check("query.run.read on service:db/host:h/database:d").is_ok());
        assert!(check("system.read on service:db/host:h").is_ok());
        assert!(
            check("query.run.read on service:db/database:d").is_err(),
            "the database without its host"
        );
        assert!(
            check("system.read on service:db/host:h/database:d").is_err(),
            "the action does not list database"
        );
        assert!(
            check("mcp.keys.read on service:db/host:h").is_err(),
            "service only"
        );
        assert!(
            check("unknown on service:db/host:h").is_err(),
            "no such action"
        );
        assert!(check("query.run.read on service:db/host:h/host:i").is_err());
        assert!(check("query.run.read on service:db/database:d/host:h").is_err());
    }

    #[test]
    fn a_document_of_one_level_keeps_its_meaning() {
        let document = parse(
            "db",
            &doc(
                "db",
                r#"{"name":"query.run.read","resource_types":["service","database"]},{"name":"system.read","resource_types":["service"]},{"name":"plain"}"#,
            ),
        )
        .unwrap();
        let check = |text: &str| document.check_grant(&Grant::parse(text).unwrap());
        assert!(check("query.run.read on service:db/database:analytics").is_ok());
        assert!(check("query.run.read on service:db").is_ok());
        assert!(
            check("query.run.read on service:db/table:x").is_err(),
            "a type not declared"
        );
        assert!(
            check("system.read on service:db/database:x").is_err(),
            "service only"
        );
        assert!(check("plain on service:db/database:x").is_err(), "no type");
        assert!(
            check("query.run.read on service:db/database:a/database:b").is_err(),
            "one level only"
        );
        assert!(document.follows(["database"]));
        assert_eq!(
            document.actions[0].sub_resource_types().collect::<Vec<_>>(),
            ["database"]
        );
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
            assert!(
                parse("db", &doc(server, actions)).is_err(),
                "{server} {actions}"
            );
        }
        assert!(parse("db", b"not json").is_err());
        let many = (0..65)
            .map(|i| format!(r#"{{"name":"a{i}"}}"#))
            .collect::<Vec<_>>()
            .join(",");
        assert!(parse("db", &doc("db", &many)).is_err());
    }

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
        Catalog::build(
            vec![parse_labels("db", &labels).unwrap().unwrap()],
            Vec::new(),
        )
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
        assert_eq!(
            registry.action_names("db"),
            ["access", "query.run.read"],
            "kept"
        );
        assert_eq!(registry.problems().len(), 1);
        runtime.block_on(round(&registry, &client, &catalog, Duration::ZERO));
        assert_eq!(
            registry.action_names("db"),
            ["access"],
            "a 404: only access"
        );
        assert!(registry.problems().is_empty());
        // A service that left the catalogue leaves the registry.
        runtime.block_on(round(
            &registry,
            &client,
            &Catalog::default(),
            Duration::ZERO,
        ));
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
        assert!(registry
            .due(&wanted, Duration::from_secs(60), now)
            .is_empty());
        assert_eq!(
            registry
                .due(
                    &wanted,
                    Duration::from_secs(60),
                    now + Duration::from_secs(61)
                )
                .len(),
            1
        );
    }

    #[test]
    fn the_declared_document_is_known_only_after_an_answer_of_the_service() {
        let registry = Registry::default();
        let now = Instant::now();
        assert!(registry.declared("a").is_none(), "never read");
        registry.record("a", "http://a/x", Outcome::Failed("down".into()), now);
        assert!(
            registry.declared("a").is_none(),
            "a failure is not an answer"
        );
        registry.record("a", "http://a/x", Outcome::Absent, now);
        assert_eq!(
            *registry.declared("a").unwrap(),
            Document::default(),
            "a 404: no action"
        );
        let read = Action {
            name: "read".into(),
            resource_types: vec!["database".into()],
            description: String::new(),
        };
        let document = Document::one_level(vec![read]);
        registry.record("a", "http://a/x", Outcome::Document(document.clone()), now);
        registry.record("a", "http://a/x", Outcome::Failed("down".into()), now);
        assert_eq!(
            *registry.declared("a").unwrap(),
            document,
            "the last good document stays"
        );
        registry.record("a", "http://a/y", Outcome::Failed("down".into()), now);
        assert!(registry.declared("a").is_none(), "a new URL: not known");
    }

    #[test]
    fn a_read_reports_the_document_only_when_it_changes() {
        let registry = Registry::default();
        let now = Instant::now();
        let url = "http://a/x";
        let one = Document::one_level(vec![Action {
            name: "read".into(),
            resource_types: vec!["database".into()],
            description: String::new(),
        }]);
        assert!(registry
            .record("a", url, Outcome::Failed("down".into()), now)
            .is_none());
        assert_eq!(
            registry
                .record("a", url, Outcome::Document(one.clone()), now)
                .as_deref(),
            Some(&one),
            "the first answer"
        );
        assert!(
            registry
                .record("a", url, Outcome::Document(one.clone()), now)
                .is_none(),
            "the same document"
        );
        assert!(registry
            .record("a", url, Outcome::Failed("down".into()), now)
            .is_none());
        assert_eq!(
            registry.record("a", url, Outcome::Absent, now).as_deref(),
            Some(&Document::default()),
            "a 404 removes the actions"
        );
    }

    #[test]
    fn the_last_known_document_decides_which_grants_apply() {
        let registry = Registry::default();
        let now = Instant::now();
        let url = "http://a/x";
        let grant = Grant::parse("query.run.write on service:a/database:d").unwrap();
        let applies = || {
            registry
                .declared("a")
                .is_some_and(|d| d.check_grant(&grant).is_ok())
        };
        let typed = |kinds: &[&str]| {
            Document::one_level(vec![Action {
                name: "query.run.write".into(),
                resource_types: kinds.iter().map(|k| k.to_string()).collect(),
                description: String::new(),
            }])
        };
        assert!(!applies(), "never read");
        registry.record("a", url, Outcome::Failed("down".into()), now);
        assert!(!applies(), "a failure is not a document");
        registry.record(
            "a",
            url,
            Outcome::Document(typed(&["service", "database"])),
            now,
        );
        assert!(applies(), "declared");
        registry.record("a", url, Outcome::Failed("down".into()), now);
        assert!(applies(), "a failed read keeps the last document");
        registry.record("a", url, Outcome::Document(typed(&["service"])), now);
        assert!(!applies(), "the new document removes the type");
        registry.record("a", url, Outcome::Failed("down".into()), now);
        assert!(!applies(), "the removal stays after a failure");
        let host_tree = parse(
            "a",
            br#"{"server":"a","resource_types":[{"name":"host","parent":"service"},{"name":"database","parent":"host"}],"actions":[{"name":"query.run.write","resource_types":["service","host","database"]}]}"#,
        )
        .unwrap();
        registry.record("a", url, Outcome::Document(host_tree), now);
        assert!(!applies(), "database now has the parent host");
        registry.record("a", url, Outcome::Document(typed(&["database"])), now);
        registry.record("a", url, Outcome::Absent, now);
        assert!(!applies(), "a 404: no action");
    }
}
