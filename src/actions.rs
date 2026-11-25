//! The actions and the resource tree that each service publishes at
//! `/.well-known/procurauth/actions` (protocol 5.1).
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
//! The PDP can ask for an early read of one document (see [`Registry::read_soon`]). The PDP does
//! this when a service sends a resource that the last known document does not accept: the
//! service can have a new tree. The early read goes through the same round, with the same limits.
//! There is at most one early read of a service in each `access.actions_refresh` interval.
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
use tokio::sync::{Notify, Semaphore};
use tokio::task::JoinSet;

use crate::catalog::Catalog;
use crate::grants::{
    check_action, check_resource_type, parse_path, Grant, Segment, ACCESS, MAX_DEPTH, SERVICE_TYPE,
};

/// The largest number of actions of one service, when the configuration does not say it
/// (`access.max_actions`). Protocol 5.3c asks a reader to accept 64 at least.
pub const DEFAULT_MAX_ACTIONS: usize = 1024;
/// The largest number of resources of one service, when the configuration does not say it
/// (`access.max_resources`). Protocol 5.3c asks a reader to accept 256 at least.
pub const DEFAULT_MAX_RESOURCES: usize = 4096;
/// The largest document that the server reads, when the configuration does not say it
/// (`access.max_document_bytes`). Protocol 5.3c asks a reader to accept 64 KiB at least.
pub const DEFAULT_MAX_DOCUMENT_BYTES: usize = 256 * 1024;
const MAX_DESCRIPTION_CHARS: usize = 300;
/// The largest number of types in the list of one action, and in the top-level list.
const MAX_RESOURCE_TYPES: usize = 16;
/// The most documents that the loop reads at the same time.
const MAX_CONCURRENT_READS: usize = 64;
/// How often the loop looks for a document that is due.
const TICK: Duration = Duration::from_secs(5);

/// The size that a document of a service can have. A larger document is bad.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bounds {
    pub max_actions: usize,
    pub max_resources: usize,
    pub max_document_bytes: usize,
}

impl Default for Bounds {
    fn default() -> Self {
        Self {
            max_actions: DEFAULT_MAX_ACTIONS,
            max_resources: DEFAULT_MAX_RESOURCES,
            max_document_bytes: DEFAULT_MAX_DOCUMENT_BYTES,
        }
    }
}

/// One action of a service.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Action {
    pub name: String,
    pub resource_types: Vec<String>,
    pub description: String,
    /// The names of the limits (see [`crate::limits`]) that a grant of this action can give.
    pub limits: Vec<String>,
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
    /// The resources that the service names, in the order of the document. The list is optional:
    /// without it, portal knows no id, and the superadmin types the ids.
    pub resources: Vec<Resource>,
    /// The limits that the service declares. The actions list the limits that apply to them.
    pub limits: Vec<crate::limits::Declaration>,
}

/// One resource that the service names: a node of the tree, such as one host. The settings pages
/// show each such resource with the actions that apply on it, so a grant on a resource needs no
/// typed id.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resource {
    /// The path from the service to the node: one segment for each type of the chain.
    pub path: Vec<Segment>,
    /// The name to show. It is the last id when the document gives none.
    pub label: String,
    /// The actions that apply on the node. These actions list the type of the node.
    pub actions: Vec<String>,
}

impl Resource {}

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
            resources: Vec::new(),
            limits: Vec::new(),
        }
    }

    /// The limits that a grant of `action` can hold: on the action `access`, each
    /// budget that the document declares; on another action, each ceiling that the action lists.
    pub fn limit_inputs(&self, action: &str) -> Vec<crate::limits::Input> {
        let listed: Vec<&String> = if action == ACCESS {
            Vec::new()
        } else {
            match self.actions.iter().find(|a| a.name == action) {
                Some(found) => found.limits.iter().collect(),
                None => return Vec::new(),
            }
        };
        self.limits
            .iter()
            .filter(|d| {
                if action == ACCESS {
                    d.kind == crate::limits::Kind::Budget
                } else {
                    d.kind == crate::limits::Kind::Ceiling && listed.contains(&&d.name)
                }
            })
            .map(|d| crate::limits::Input {
                name: d.name.clone(),
                label: d.label.clone(),
                description: d.description.clone(),
                unit: d.unit.clone(),
                budget: d.kind == crate::limits::Kind::Budget,
                range: d.range,
            })
            .collect()
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
    /// The time of the last good read: a good document.
    good_at: Option<Instant>,
    /// The PDP asked for an early read. The next round reads the document, also when it is fresh.
    early: bool,
    /// The time of the last early read that the PDP asked for.
    early_at: Option<Instant>,
}

/// The longest time that the last good document of a service stays valid, when the configuration
/// does not say it (`access.actions_max_age`).
pub const DEFAULT_MAX_AGE: Duration = Duration::from_secs(3600);

/// Where the document of a service comes from, and how old the copy is. The forms of the keys
/// show it, so a person who makes a key knows what the service published.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Source {
    /// The service publishes no document, or its last good copy is older than
    /// `access.actions_max_age`: the service has only `access`.
    #[default]
    Absent,
    /// The last read gave this document. The time is the age of the read.
    Fresh(Duration),
    /// The last read failed. The time is the age of the last good copy, which still applies.
    Stale(Duration),
}

/// The actions of each service, by service id.
pub struct Registry {
    entries: RwLock<HashMap<String, Entry>>,
    /// Wakes the refresh loop before the end of its tick.
    wake: Notify,
    /// The time of the start. A service with no good read counts its age from here.
    started: Instant,
    /// `access.actions_max_age`.
    max_age: Duration,
    /// The size of a document that the reads accept.
    bounds: Bounds,
    /// The document of the service `portal`. It is in the code: it holds the features that the
    /// file turns on (see [`crate::portal_actions`]).
    portal: Arc<Document>,
}

impl Default for Registry {
    fn default() -> Self {
        Self::new(DEFAULT_MAX_AGE)
    }
}

impl Registry {
    /// The same registry, with these bounds for the documents that it reads.
    pub fn with_bounds(mut self, bounds: Bounds) -> Self {
        self.bounds = bounds;
        self
    }

    /// The same registry, with the document of the service `portal` for the features that the file
    /// turns on (see [`crate::portal_actions::document`]).
    pub fn with_portal(mut self, document: Arc<Document>) -> Self {
        self.portal = document;
        self
    }

    /// A registry whose documents stay valid for `max_age` after their last good read.
    pub fn new(max_age: Duration) -> Self {
        Self {
            entries: RwLock::default(),
            wake: Notify::new(),
            started: Instant::now(),
            max_age,
            bounds: Bounds::default(),
            portal: crate::portal_actions::document(&crate::portal_actions::Features::default()),
        }
    }

    /// The document of a service that the code holds: the service `portal`. Such a document never
    /// gets old.
    fn internal(&self, service: &str) -> Option<Arc<Document>> {
        (service == crate::portal_actions::SERVICE).then(|| Arc::clone(&self.portal))
    }

    /// The time since the last good read of the document of a service. A service that has no good
    /// read yet counts from the start of the program.
    pub fn age(&self, service: &str) -> Duration {
        let good = self.read().get(service).and_then(|entry| entry.good_at);
        Instant::now().saturating_duration_since(good.unwrap_or(self.started))
    }

    /// Whether the last good document of a service is older than `access.actions_max_age`. The
    /// portal then acts as with no good document of the service (protocol 5.3a, 11.1). The
    /// service `portal` has its document in the code: it never gets old.
    pub fn too_old(&self, service: &str) -> bool {
        self.internal(service).is_none() && self.age(service) > self.max_age
    }

    /// The state of the document of a service: fresh, a stale copy, or none. The service `portal`
    /// has no entry: its document is in the code.
    pub fn source(&self, service: &str) -> Source {
        let entries = self.read();
        let Some(entry) = entries.get(service) else {
            return Source::Absent;
        };
        let Some(good) = entry.good_at else {
            return Source::Absent;
        };
        let age = Instant::now().saturating_duration_since(good);
        if age > self.max_age {
            Source::Absent
        } else if entry.read_at.is_some_and(|read| read > good) {
            // A read after the last good one failed. The first spread of the reads moves the time
            // of a read back, so a good read is never after its own mark.
            Source::Stale(age)
        } else {
            Source::Fresh(age)
        }
    }

    /// The age of the oldest document among `services`.
    pub fn oldest_age<'a>(&self, services: impl Iterator<Item = &'a str>) -> Duration {
        services
            .filter(|service| self.internal(service).is_none())
            .map(|service| self.age(service))
            .max()
            .unwrap_or_default()
    }

    /// The actions of a service. Empty: the service has only `access`.
    pub fn actions(&self, service: &str) -> Vec<Action> {
        if let Some(document) = self.internal(service) {
            return document.actions.clone();
        }
        self.read()
            .get(service)
            .map(|entry| entry.document.actions.clone())
            .unwrap_or_default()
    }

    /// The document of a service when the server knows it: the last read gave a good document or
    /// a `404`. A `404` gives an empty document. `None`: the service is not in the catalogue, or
    /// no read has answered yet. The PDP fails closed on `None`.
    pub fn declared(&self, service: &str) -> Option<Arc<Document>> {
        // The document of the service `portal` is in the code.
        if let Some(document) = self.internal(service) {
            return Some(document);
        }
        if self.too_old(service) {
            return None;
        }
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

    /// Asks for an early read of the document of `service`, and wakes the refresh loop. The
    /// request applies only to a service that the loop knows, and at most once in each `refresh`
    /// interval for each service. Gives `true` when the loop reads the document again soon.
    pub fn read_soon(&self, service: &str, refresh: Duration, now: Instant) -> bool {
        {
            let mut entries = self.write();
            let Some(entry) = entries.get_mut(service) else {
                return false;
            };
            if entry
                .early_at
                .is_some_and(|at| now.saturating_duration_since(at) < refresh)
            {
                return false;
            }
            entry.early = true;
            entry.early_at = Some(now);
        }
        self.wake.notify_one();
        true
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
                entry.good_at = Some(now);
            }
            Outcome::Absent => {
                // A status 404 is a failed read (protocol 5.3): the last good document stays for
                // `access.actions_max_age`. A service that has no document gives no problem.
                if entry.url.as_deref() != Some(url) {
                    entry.document = Arc::default();
                    entry.url = Some(url.to_string());
                    entry.known = false;
                    entry.good_at = None;
                }
                entry.problem = None;
            }
            Outcome::Failed(problem) => {
                // A different URL: the last list belongs to another document.
                if entry.url.as_deref() != Some(url) {
                    entry.document = Arc::default();
                    entry.url = Some(url.to_string());
                    entry.known = false;
                    entry.good_at = None;
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

    /// The services whose document is due: never read, read from another URL, read more than
    /// `refresh` ago, or marked for an early read. This call consumes the early marks. A request
    /// that comes during the read thus gives one more read.
    fn due(
        &self,
        wanted: &HashMap<String, Option<String>>,
        refresh: Duration,
        now: Instant,
    ) -> Vec<(String, String)> {
        let mut entries = self.write();
        let mut out: Vec<(String, String)> = wanted
            .iter()
            .filter_map(|(id, url)| {
                let url = url.as_ref()?;
                let fresh = match entries.get_mut(id) {
                    Some(entry) => {
                        let early = std::mem::take(&mut entry.early);
                        !early
                            && entry.url.as_deref() == Some(url.as_str())
                            && entry
                                .read_at
                                .is_some_and(|at| now.saturating_duration_since(at) < refresh)
                    }
                    None => false,
                };
                (!fresh).then(|| (id.clone(), url.clone()))
            })
            .collect();
        out.sort();
        out
    }

    fn has_read(&self, service: &str) -> bool {
        self.read()
            .get(service)
            .is_some_and(|e| e.read_at.is_some())
    }

    /// Wakes the refresh loop, so it reads the documents of a new catalogue at once.
    pub fn wake_loop(&self) {
        self.wake.notify_one();
    }

    /// Tells whether each service of the catalogue that has a document URL had a first read, good or
    /// not.
    pub fn first_reads_done(&self, catalog: &Catalog) -> bool {
        let wanted = wanted(catalog);
        let entries = self.read();
        wanted
            .iter()
            .filter(|(_, url)| url.is_some())
            .all(|(id, _)| entries.get(id).is_some_and(|e| e.read_at.is_some()))
    }

    /// Moves the next read of a service to a stable point of the `refresh` period, so that the
    /// reads of all services come at equal intervals. The point depends on the id
    /// of the service. Only the first read of a service applies this; each next read keeps it.
    fn spread(&self, service: &str, refresh: Duration) {
        let mut entries = self.write();
        let Some(entry) = entries.get_mut(service) else {
            return;
        };
        let phase = phase_of(service, refresh);
        if let Some(at) = entry.read_at {
            entry.read_at = at.checked_sub(phase).or(Some(at));
        }
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

    /// Removes what the registry knows of the service: its document is then unknown.
    #[cfg(test)]
    pub fn forget(&self, service: &str) {
        self.write().remove(service);
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
const TEST_URL: &str = "http://test/.well-known/procurauth/actions";

#[cfg(test)]
impl Registry {
    /// A good document whose last good read was `age` ago.
    pub fn set_document_aged(&self, service: &str, document: Document, age: Duration) {
        let read_at = Instant::now()
            .checked_sub(age)
            .expect("the clock of the test is long enough");
        self.record(service, TEST_URL, Outcome::Document(document), read_at);
    }
}

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
    /// The tree of types below the service: it can be empty, and it is mandatory (protocol 5.2).
    resource_types: Vec<RawType>,
    actions: Vec<RawAction>,
    /// The resources of the service. The field is optional.
    #[serde(default)]
    resources: Vec<RawResource>,
    /// The limits of the service. The field is optional.
    #[serde(default)]
    limits: Vec<crate::limits::RawDeclaration>,
}

#[derive(Deserialize)]
struct RawResource {
    /// The path in the text form of a grant, for example `host:prod/database:logs`.
    path: String,
    #[serde(default)]
    label: String,
    /// The actions that apply on the node. Empty: each action that lists the type of the node.
    #[serde(default)]
    actions: Vec<String>,
}

#[derive(Deserialize)]
struct RawType {
    name: String,
    parent: String,
    /// Mandatory and not empty (protocol 5.2).
    description: String,
}

#[derive(Deserialize)]
struct RawAction {
    name: String,
    /// Mandatory (protocol 5.2).
    resource_types: Vec<String>,
    /// Mandatory and not empty (protocol 5.2).
    description: String,
    /// The names of the limits that apply to the action.
    #[serde(default)]
    limits: Vec<String>,
    // The member `approval` of an earlier version is not read. The parser ignores it, as it
    // ignores each member that it does not know.
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

/// A description that the document must give: not empty (protocol 5.2).
fn require_description(description: &str, what: &str) -> Result<(), String> {
    if description.trim().is_empty() {
        return Err(format!("the description of {what} is empty"));
    }
    check_description(description, what)
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
        require_description(&t.description, &format!("the resource type {:?}", t.name))?;
        types.push(ResourceType {
            name: t.name,
            parent: t.parent,
            description: t.description,
        });
    }
    let document = Document {
        types,
        ..Document::default()
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
    parse_within(service, body, &Bounds::default())
}

/// Check one document of the service `service`, with the bounds `bounds`.
pub fn parse_within(service: &str, body: &[u8], bounds: &Bounds) -> Result<Document, String> {
    let document: RawDocument =
        serde_json::from_slice(body).map_err(|e| format!("the document is not valid: {e}"))?;
    if document.server != service {
        return Err(format!(
            "the document names the server {:?}, the id of the service is {service:?}: set the id label of the catalogue to the name of the server",
            document.server
        ));
    }
    if document.actions.len() > bounds.max_actions {
        return Err(format!("more than {} actions", bounds.max_actions));
    }
    let declared = Some(check_types(document.resource_types)?);
    let raw_resources = document.resources;
    let limits = crate::limits::check_declarations(document.limits)?;
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
        // The list of an action can name `service` and each declared type.
        if raw.resource_types.len() > MAX_RESOURCE_TYPES + 1 {
            return Err(format!(
                "the action {:?} has more than {} resource types",
                raw.name,
                MAX_RESOURCE_TYPES + 1
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
        require_description(&raw.description, &format!("the action {:?}", raw.name))?;
        for (index, name) in raw.limits.iter().enumerate() {
            if !limits.iter().any(|d| d.name == *name) {
                return Err(format!(
                    "the action {:?} lists the limit {name:?}, which the document does not declare",
                    raw.name
                ));
            }
            if raw.limits[..index].contains(name) {
                return Err(format!(
                    "the action {:?} lists the limit {name:?} two times",
                    raw.name
                ));
            }
        }
        out.push(Action {
            name: raw.name,
            resource_types: raw.resource_types,
            description: raw.description,
            limits: raw.limits,
        });
    }
    let mut document = match declared {
        Some(types) => Document {
            types,
            actions: out,
            resources: Vec::new(),
            limits: Vec::new(),
        },
        None => Document::one_level(out),
    };
    document.limits = limits;
    document.resources = check_resources(&document, raw_resources, bounds.max_resources)?;
    Ok(document)
}

/// Check the optional `resources`: each path follows the declared types, each path is there one
/// time, and each action of a resource exists and lists the type of the node.
fn check_resources(
    document: &Document,
    raw: Vec<RawResource>,
    max_resources: usize,
) -> Result<Vec<Resource>, String> {
    if raw.len() > max_resources {
        return Err(format!("more than {max_resources} resources"));
    }
    let mut out: Vec<Resource> = Vec::with_capacity(raw.len());
    for item in raw {
        let path = parse_path(&item.path)?;
        let Some(last) = path.last() else {
            return Err("a resource has an empty path".into());
        };
        if !document.follows(path.iter().map(|s| s.kind.as_str())) {
            return Err(format!(
                "the path {:?} of a resource does not follow the resource types of the service",
                item.path
            ));
        }
        if out.iter().any(|known| known.path == path) {
            return Err(format!(
                "the resource {:?} is in the list two times",
                item.path
            ));
        }
        check_description(&item.label, &format!("the resource {:?}", item.path))?;
        let mut named = std::collections::HashSet::new();
        for name in &item.actions {
            // A name that occurs more than one time makes the document bad (protocol 5.2).
            if !named.insert(name.as_str()) {
                return Err(format!(
                    "the resource {:?} names the action {name:?} two times",
                    item.path
                ));
            }
            let listed = document
                .actions
                .iter()
                .find(|a| a.name == *name)
                .is_some_and(|a| a.sub_resource_types().any(|k| k == last.kind));
            if !listed {
                return Err(format!(
                    "the resource {:?} names the action {name:?}, which does not list the type {:?}",
                    item.path, last.kind
                ));
            }
        }
        out.push(Resource {
            label: if item.label.is_empty() {
                last.id.clone()
            } else {
                item.label
            },
            path,
            actions: item.actions,
        });
    }
    Ok(out)
}

/// Read the document of one service.
async fn read(client: &reqwest::Client, service: &str, url: &str, bounds: &Bounds) -> Outcome {
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
    match crate::discovery::read_capped(response, bounds.max_document_bytes).await {
        Ok(body) => match parse_within(service, &body, bounds) {
            Ok(document) => Outcome::Document(document),
            Err(e) => Outcome::Failed(format!("{url}: {e}")),
        },
        Err(e) => Outcome::Failed(format!("{url}: {e}")),
    }
}

/// The offset of a service in the period `refresh`: a stable value from 0 to `refresh`.
fn phase_of(service: &str, refresh: Duration) -> Duration {
    // FNV-1a: the same value in each run and on each instance.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in service.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    let millis = u64::try_from(refresh.as_millis())
        .unwrap_or(u64::MAX)
        .max(1);
    Duration::from_millis(hash % millis)
}

/// The document URL of each service of the catalogue.
fn wanted(catalog: &Catalog) -> HashMap<String, Option<String>> {
    catalog
        .groups()
        .iter()
        .flat_map(|g| g.services.iter())
        // An external service is a link. It has no document, and the portal never requests it.
        .filter(|s| !s.external)
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
    let first: std::collections::HashSet<String> = due
        .iter()
        .filter(|(service, _)| !registry.has_read(service))
        .map(|(service, _)| service.clone())
        .collect();
    let limit = Arc::new(Semaphore::new(MAX_CONCURRENT_READS));
    let mut reads: JoinSet<(String, String, Outcome)> = JoinSet::new();
    for (service, url) in due {
        let client = client.clone();
        let limit = Arc::clone(&limit);
        let bounds = registry.bounds;
        reads.spawn(async move {
            let _permit = limit.acquire_owned().await;
            let outcome = read(&client, &service, &url, &bounds).await;
            (service, url, outcome)
        });
    }
    let mut changed = Vec::new();
    while let Some(result) = reads.join_next().await {
        if let Ok((service, url, outcome)) = result {
            let document = registry.record(&service, &url, outcome, Instant::now());
            if first.contains(&service) {
                registry.spread(&service, refresh);
            }
            if let Some(document) = document {
                changed.push((service, document));
            }
        }
    }
    changed
}

/// The refresh loop. `catalog` gives the current catalogue. `changed` receives each service whose
/// known document changed: the first answer after the start, and each different answer. Runs
/// until the process ends. The loop waits one tick between two rounds. [`Registry::read_soon`]
/// stops this wait.
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
        wait(&registry, TICK).await;
    }
}

/// Waits `tick`, or less when [`Registry::read_soon`] wakes the loop. A request that comes during
/// a round stays stored, so the next wait ends at once.
async fn wait(registry: &Registry, tick: Duration) {
    let _ = tokio::time::timeout(tick, registry.wake.notified()).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::parse_labels;
    use std::collections::BTreeMap;
    use std::io::{Read, Write};

    /// A document with these actions. An action that lacks `resource_types` lists `service`, and
    /// one that lacks `description` gets one. Each type that an action lists is declared with the
    /// parent `service`. A text that is not a list of objects stays as it is.
    fn doc(server: &str, actions: &str) -> Vec<u8> {
        let parsed: Result<Vec<serde_json::Value>, _> =
            serde_json::from_str(&format!("[{actions}]"));
        let Ok(mut list) = parsed else {
            return format!(r#"{{"server":"{server}","resource_types":[],"actions":[{actions}]}}"#)
                .into_bytes();
        };
        let mut types: Vec<String> = Vec::new();
        for action in &mut list {
            let Some(object) = action.as_object_mut() else {
                continue;
            };
            object
                .entry("resource_types")
                .or_insert_with(|| serde_json::json!(["service"]));
            object
                .entry("description")
                .or_insert_with(|| serde_json::json!("The action."));
            for kind in object["resource_types"].as_array().into_iter().flatten() {
                if let Some(kind) = kind.as_str() {
                    if kind != "service" && !types.iter().any(|t| t == kind) {
                        types.push(kind.to_string());
                    }
                }
            }
        }
        let declared: Vec<serde_json::Value> = types
            .iter()
            .map(|t| serde_json::json!({"name": t, "parent": "service", "description": "A type."}))
            .collect();
        serde_json::json!({"server": server, "resource_types": declared, "actions": list})
            .to_string()
            .into_bytes()
    }

    /// An external service is a link: the portal never reads a document for it, and it never asks it anything.
    #[test]
    fn an_external_service_is_never_wanted_by_the_readers_of_the_documents() {
        let found = crate::catalog::parse_labels(
            "x",
            &[
                ("portal.name", "Db"),
                ("portal.href", "/db/"),
                ("portal.actions", "http://db/.well-known/procurauth/actions"),
            ]
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
        )
        .unwrap()
        .unwrap();
        let (name, id) = crate::catalog::external::check_name("Wiki").unwrap();
        let catalog = Catalog::build(vec![found], Vec::new()).with_externals(&[
            crate::catalog::external::ExternalEntry {
                id: id.clone(),
                name,
                href: "https://wiki.example.org/".into(),
                created_at: 1,
                ..Default::default()
            },
        ]);
        assert_eq!(catalog.service_count(), 2);
        let wanted = wanted(&catalog);
        assert_eq!(wanted.len(), 1);
        assert!(!wanted.contains_key(&id));
        let registry = Registry::new(Duration::from_secs(60));
        registry.retain(&wanted);
        assert!(registry.declared(&id).is_none());
        assert!(registry.actions(&id).is_empty());
        assert!(registry.first_reads_done(&Catalog::build(Vec::new(), Vec::new())));
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
        assert_eq!(actions[1].description, "The action.");
        assert_eq!(
            document.types,
            [ResourceType {
                name: "database".into(),
                parent: "service".into(),
                description: "A type.".into()
            }],
            "a document of one level: each type has the parent service"
        );
    }

    fn tree(types: &str, actions: &str) -> Vec<u8> {
        format!(r#"{{"server":"db","resource_types":[{types}],"actions":[{actions}]}}"#)
            .into_bytes()
    }

    const HOST_DATABASE: &str = r#"{"name":"host","parent":"service","description":"One server."},{"name":"database","parent":"host","description":"One database of a host."}"#;
    const TREE_ACTIONS: &str = r#"{"name":"query.run.read","resource_types":["service","host","database"],"description":"The action query.run.read."},{"name":"system.read","resource_types":["service","host"],"description":"The action system.read."},{"name":"mcp.keys.read","resource_types":["service"],"description":"The action mcp.keys.read."}"#;

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
        // The order of the list does not matter: a parent can come after its child.
        let reversed = r#"{"name":"database","parent":"host","description":"The type database."},{"name":"host","parent":"service","description":"The type host."}"#;
        let document = parse("db", &tree(reversed, TREE_ACTIONS)).unwrap();
        assert_eq!(document.chain("database").unwrap(), ["host", "database"]);
        // Four levels below service.
        let deep = r#"{"name":"a","parent":"service","description":"The type a."},{"name":"b","parent":"a","description":"The type b."},{"name":"c","parent":"b","description":"The type c."},{"name":"d","parent":"c","description":"The type d."}"#;
        let document = parse(
            "db",
            &tree(
                deep,
                r#"{"name":"x","resource_types":["d"],"description":"The action x."}"#,
            ),
        )
        .unwrap();
        assert_eq!(document.chain("d").unwrap(), ["a", "b", "c", "d"]);
        // An empty tree: the actions name only service.
        let document = parse(
            "db",
            &tree(
                "",
                r#"{"name":"x","resource_types":["service"],"description":"The action x."}"#,
            ),
        )
        .unwrap();
        assert!(document.types.is_empty());
    }

    #[test]
    fn a_bad_tree_makes_the_whole_document_bad() {
        let actions = r#"{"name":"x","resource_types":["service"],"description":"The action x."}"#;
        for (types, why) in [
            (
                r#"{"name":"service","parent":"service","description":"The type service."}"#,
                "service is the root",
            ),
            (r#"{"name":"Host","parent":"service"}"#, "upper case"),
            (
                r#"{"name":"1host","parent":"service","description":"The type 1host."}"#,
                "a digit first",
            ),
            (
                r#"{"name":"host","parent":"service","description":"The type host."},{"name":"host","parent":"service","description":"The type host."}"#,
                "two times",
            ),
            (
                r#"{"name":"database","parent":"host","description":"The type database."}"#,
                "a parent that is not declared",
            ),
            (
                r#"{"name":"a","parent":"b","description":"The type a."},{"name":"b","parent":"a","description":"The type b."}"#,
                "a cycle",
            ),
            (
                r#"{"name":"a","parent":"a","description":"The type a."}"#,
                "its own parent",
            ),
            (
                r#"{"name":"a","parent":"service","description":"The type a."},{"name":"b","parent":"a","description":"The type b."},{"name":"c","parent":"b","description":"The type c."},{"name":"d","parent":"c","description":"The type d."},{"name":"e","parent":"d","description":"The type e."}"#,
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
                &tree(
                    HOST_DATABASE,
                    r#"{"name":"x","resource_types":["table"],"description":"The action x."}"#
                )
            )
            .is_err(),
            "an action lists a type that the tree does not declare"
        );
        assert!(
            parse(
                "db",
                &tree(
                    HOST_DATABASE,
                    r#"{"name":"x","resource_types":["host","host"],"description":"The action x."}"#
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
    fn the_resources_of_a_document_name_the_hosts_with_the_actions_that_apply_on_each() {
        let body = |resources: &str| {
            format!(
                r#"{{"server":"db","resource_types":[{{"name":"host","parent":"service","description":"The type host."}},{{"name":"database","parent":"host","description":"The type database."}}],"actions":[{{"name":"query.run.read","resource_types":["service","host","database"],"description":"The action query.run.read."}},{{"name":"explorer.read","resource_types":["service","host"],"description":"The action explorer.read."}},{{"name":"mcp.keys.read","resource_types":["service"],"description":"The action mcp.keys.read."}}],"resources":{resources}}}"#
            )
            .into_bytes()
        };
        let document = parse(
            "db",
            &body(
                r#"[{"path":"host:prod","label":"Production","actions":["query.run.read","explorer.read"]},{"path":"host:replica"},{"path":"host:prod/database:logs"}]"#,
            ),
        )
        .unwrap();
        assert_eq!(document.resources.len(), 3);
        assert_eq!(document.resources[0].label, "Production");
        assert_eq!(
            document.resources[1].label, "replica",
            "the id when no label"
        );
        // A document without the field has no resource.
        assert!(parse("db", &body("[]")).unwrap().resources.is_empty());
        // Any bad resource makes the whole document bad.
        for (bad, why) in [
            (
                r#"[{"path":"host:a","actions":["mcp.keys.read"]}]"#,
                "an action that does not list the type",
            ),
            (
                r#"[{"path":"host:a","actions":["nope"]}]"#,
                "an unknown action",
            ),
            (
                r#"[{"path":"database:a"}]"#,
                "a path that does not follow the parents",
            ),
            (
                r#"[{"path":"table:a"}]"#,
                "a type that the service does not declare",
            ),
            (
                r#"[{"path":"host:a"},{"path":"host:a"}]"#,
                "the same path twice",
            ),
            (
                r#"[{"path":"host:a","actions":["query.run.read","query.run.read"]}]"#,
                "the same action twice in one item",
            ),
            (r#"[{"path":""}]"#, "an empty path"),
        ] {
            assert!(parse("db", &body(bad)).is_err(), "{why}");
        }
    }

    #[test]
    fn the_member_approval_of_an_action_is_ignored_with_any_value() {
        let one = |member: &str| {
            format!(
                r#"{{"server":"db","resource_types":[],"actions":[{{"name":"query.run.write","resource_types":["service"],"description":"d"{member}}}]}}"#
            )
            .into_bytes()
        };
        let plain = parse("db", &one("")).unwrap();
        for member in [
            r#","approval":"optional""#,
            r#","approval":"required""#,
            r#","approval":true"#,
            r#","approval":{"mode":"optional"}"#,
        ] {
            let document = parse("db", &one(member)).expect(member);
            assert_eq!(document, plain, "{member}: the member changes nothing");
        }
    }

    #[test]
    fn a_document_declares_limits_and_each_action_lists_the_limits_that_apply() {
        let body = |limits: &str, actions: &str| {
            format!(
                r#"{{"server":"db","resource_types":[],"limits":{limits},"actions":{actions}}}"#
            )
            .into_bytes()
        };
        let limits = r#"[
          {"name":"requests","label":"Queries","description":"The queries that a subject sends.",
           "unit":"request","kind":"budget"},
          {"name":"max_duration","label":"Maximum duration of a query","description":"The longest run of one query.",
           "unit":"seconds","kind":"ceiling","min":0,"max":3600}]"#;
        let actions = r#"[
          {"name":"query.run.read","resource_types":["service"],"description":"A query.","limits":["max_duration"]},
          {"name":"system.read","resource_types":["service"],"description":"The action system.read."}]"#;
        let document = parse("db", &body(limits, actions)).unwrap();
        assert_eq!(document.limits.len(), 2);
        assert_eq!(document.limits[0].label, "Queries");
        assert_eq!(document.actions[0].limits, ["max_duration"]);
        assert!(document.actions[1].limits.is_empty());
        // A document without the field has no limit.
        let plain = parse("db", br#"{"server":"db","resource_types":[],"actions":[{"name":"a.b","resource_types":["service"],"description":"d"}]}"#).unwrap();
        assert!(plain.limits.is_empty() && plain.actions[0].limits.is_empty());
        // Any bad limit makes the whole document bad.
        for (limits, actions, why) in [
            (
                limits,
                r#"[{"name":"a.b","limits":["unknown"]}]"#,
                "an undeclared limit",
            ),
            (
                limits,
                r#"[{"name":"a.b","limits":["max_duration","max_duration"]}]"#,
                "a limit two times",
            ),
            (
                r#"[{"name":"max_x","label":"x","description":"d","unit":"u","kind":"ceiling","min":2,"max":1}]"#,
                "[]",
                "bad range",
            ),
            (
                r#"[{"name":"max_x","label":"x","description":"d","unit":"u","kind":"ceiling","min":1,"max":2,"colour":1}]"#,
                "[]",
                "unknown member",
            ),
        ] {
            assert!(parse("db", &body(limits, actions)).is_err(), "{why}");
        }
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
                r#"{"name":"query.run.read","resource_types":["service","database"],"description":"The action query.run.read."},{"name":"system.read","resource_types":["service"],"description":"The action system.read."},{"name":"plain"}"#,
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
            (
                "db",
                r#"{"name":"a","resource_types":["Data base"],"description":"The action a."}"#,
            ),
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
        let least = Bounds {
            max_actions: 64,
            ..Bounds::default()
        };
        assert!(parse_within("db", &doc("db", &many), &least).is_err());
        assert!(
            parse("db", &doc("db", &many)).is_ok(),
            "the default is higher"
        );
    }

    #[test]
    fn a_service_with_hundreds_of_actions_and_resources_is_good_within_the_bounds() {
        let actions = (0..400)
            .map(|i| {
                format!(
                    r#"{{"name":"area{}.verb{}","resource_types":["service","host"],"description":"The action {i}."}}"#,
                    i / 20,
                    i % 20
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        let resources = (0..300)
            .map(|i| format!(r#"{{"path":"host:h{i}"}}"#))
            .collect::<Vec<_>>()
            .join(",");
        let body = format!(
            r#"{{"server":"db","resource_types":[{{"name":"host","parent":"service","description":"A host."}}],"actions":[{actions}],"resources":[{resources}]}}"#
        );
        let document = parse("db", body.as_bytes()).unwrap();
        assert_eq!(
            (document.actions.len(), document.resources.len()),
            (400, 300)
        );
        let small = Bounds {
            max_resources: 256,
            ..Bounds::default()
        };
        let message = parse_within("db", body.as_bytes(), &small).unwrap_err();
        assert!(message.contains("more than 256 resources"), "{message}");
    }

    fn serve(answers: Vec<(&'static str, String)>) -> String {
        let listener = crate::util::test_ports::listener();
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
            ["access", "query.run.read"],
            "a 404 is a failed read: the last good document stays (protocol 5.3)"
        );
        assert!(
            registry.problems().is_empty(),
            "a service with no document is not a problem"
        );
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
    fn an_early_read_makes_the_document_due_at_once_and_once_in_each_interval() {
        let registry = Registry::default();
        let wanted: HashMap<String, Option<String>> =
            [("a".to_string(), Some("http://a/x".to_string()))]
                .into_iter()
                .collect();
        let refresh = Duration::from_secs(300);
        let now = Instant::now();
        assert!(
            !registry.read_soon("a", refresh, now),
            "a service that the loop does not know yet: its first read is due anyway"
        );
        registry.record("a", "http://a/x", Outcome::Absent, now);
        assert!(registry.due(&wanted, refresh, now).is_empty(), "fresh");

        let first = now + Duration::from_secs(1);
        assert!(registry.read_soon("a", refresh, first));
        assert_eq!(
            registry.due(&wanted, refresh, first),
            [("a".to_string(), "http://a/x".to_string())],
            "due at once"
        );
        assert!(
            registry.due(&wanted, refresh, first).is_empty(),
            "the round consumes the request"
        );
        registry.record("a", "http://a/x", Outcome::Absent, first);

        let again = first + Duration::from_secs(299);
        assert!(
            !registry.read_soon("a", refresh, again),
            "a second request in the same interval"
        );
        assert!(registry.due(&wanted, refresh, again).is_empty());

        let later = first + refresh;
        assert!(registry.read_soon("a", refresh, later), "a new interval");
        assert_eq!(registry.due(&wanted, refresh, later).len(), 1);
    }

    #[test]
    fn an_early_read_wakes_the_loop_and_a_tick_ends_the_wait_without_it() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let registry = Registry::default();
        registry.record("a", "http://a/x", Outcome::Absent, Instant::now());
        let long = Duration::from_secs(3600);
        // A request before the wait: the permit stays, the wait ends at once.
        assert!(registry.read_soon("a", Duration::from_secs(60), Instant::now()));
        let started = Instant::now();
        runtime.block_on(wait(&registry, long));
        assert!(started.elapsed() < Duration::from_secs(5));
        // No request: the tick ends the wait.
        let started = Instant::now();
        runtime.block_on(wait(&registry, Duration::from_millis(50)));
        assert!(started.elapsed() >= Duration::from_millis(50));
    }

    #[test]
    fn the_loop_reads_a_fresh_document_again_after_an_early_request() {
        let one = String::from_utf8(doc("db", r#"{"name":"query.run.read"}"#)).unwrap();
        let two = String::from_utf8(doc(
            "db",
            r#"{"name":"query.run.read"},{"name":"query.run.write"}"#,
        ))
        .unwrap();
        let base = serve(vec![("200 OK", one), ("200 OK", two)]);
        let catalog = catalog_with("db", &base);
        let registry = Registry::default();
        let client = crate::discovery::http_client(Duration::from_secs(2)).unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let refresh = Duration::from_secs(300);
        runtime.block_on(round(&registry, &client, &catalog, refresh));
        assert_eq!(registry.action_names("db"), ["access", "query.run.read"]);
        assert!(
            runtime
                .block_on(round(&registry, &client, &catalog, refresh))
                .is_empty(),
            "fresh: no read"
        );
        assert!(registry.read_soon("db", refresh, Instant::now()));
        let changed = runtime.block_on(round(&registry, &client, &catalog, refresh));
        assert_eq!(changed.len(), 1, "the early read gives the new document");
        assert_eq!(
            registry.action_names("db"),
            ["access", "query.run.read", "query.run.write"]
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
        assert!(
            registry.declared("a").is_none(),
            "a 404 is a failed read, not an empty document (protocol 5.3)"
        );
        let read = Action {
            name: "read".into(),
            resource_types: vec!["database".into()],
            description: String::new(),
            limits: Vec::new(),
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
    fn the_source_of_a_document_is_fresh_stale_or_absent() {
        let registry = Registry::default();
        assert_eq!(registry.source("db"), Source::Absent, "never read");
        registry.set("db", Vec::new());
        assert!(
            matches!(registry.source("db"), Source::Fresh(age) if age < Duration::from_secs(5))
        );
        registry.fail("db");
        assert!(
            matches!(registry.source("db"), Source::Stale(age) if age < Duration::from_secs(5)),
            "the last read failed: the last good copy still applies"
        );
        registry.set("db", Vec::new());
        assert!(
            matches!(registry.source("db"), Source::Fresh(_)),
            "a good read again"
        );
        let short = Registry::new(Duration::ZERO);
        short.set("db", Vec::new());
        std::thread::sleep(Duration::from_millis(5));
        assert_eq!(
            short.source("db"),
            Source::Absent,
            "a copy past the maximum age is no copy"
        );
    }

    #[test]
    fn a_document_older_than_the_maximum_age_counts_as_no_good_document() {
        let registry = Registry::new(Duration::from_secs(60));
        let url = "http://a/x";
        let document = Document::one_level(Vec::new());
        // A good read now.
        registry.record(
            "a",
            url,
            Outcome::Document(document.clone()),
            Instant::now(),
        );
        assert!(registry.declared("a").is_some());
        assert!(!registry.too_old("a"));
        assert!(registry.age("a") < Duration::from_secs(5));
        // A good read an hour ago.
        let old = Instant::now()
            .checked_sub(Duration::from_secs(3600))
            .expect("the clock of the test is long enough");
        registry.record("b", url, Outcome::Document(document), old);
        assert!(registry.too_old("b"));
        assert!(
            registry.declared("b").is_none(),
            "the portal acts as with no document"
        );
        assert!(registry.age("b") >= Duration::from_secs(3600));
        // A service with no good read counts its age from the start of the registry.
        assert!(!registry.too_old("never"));
        assert!(registry.age("never") < Duration::from_secs(5));
        // The service `portal` has its document in the code and never gets old.
        assert!(registry.declared("portal").is_some());
        assert!(!registry.too_old("portal"));
        assert_eq!(registry.action_names("portal")[0], "access");
        // The document of the code follows the features that the file turns on.
        let features = crate::portal_actions::Features {
            links: false,
            inbox_groups: Some(vec![("infra".into(), "Infra".into())]),
            kinds: crate::keys::KeyKind::ALL.to_vec(),
        };
        let with_features = Registry::new(Duration::from_secs(3600))
            .with_portal(crate::portal_actions::document(&features));
        assert_eq!(with_features.action_names("portal").len(), 1 + 20);
        // The oldest age among some services.
        assert!(registry.oldest_age(["a", "b", "portal"].into_iter()) >= Duration::from_secs(3600));
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
            limits: Vec::new(),
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
        assert!(
            registry.record("a", url, Outcome::Absent, now).is_none(),
            "a 404 is a failed read: the document does not change"
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
                limits: Vec::new(),
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
            br#"{"server":"a","resource_types":[{"name":"host","parent":"service","description":"The type host."},{"name":"database","parent":"host","description":"The type database."}],"actions":[{"name":"query.run.write","resource_types":["service","host","database"],"description":"The action query.run.write."}]}"#,
        )
        .unwrap();
        registry.record("a", url, Outcome::Document(host_tree), now);
        assert!(!applies(), "database now has the parent host");
        registry.record("a", url, Outcome::Document(typed(&["database"])), now);
        registry.record("a", url, Outcome::Absent, now);
        assert!(applies(), "a 404 is a failed read: the last document stays");
    }
}
