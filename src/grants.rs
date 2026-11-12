//! The grants of the users: an action on a node of the resource tree of a service.
//!
//! The text form of a grant is `<action> on service:<id>[/<type>:<id>]...` (contract 11.8):
//!
//! - `access on service:grafana`: the grant applies to the whole service.
//! - `query.run.read on service:clickhouse-dash/host:local`: the grant applies to the host `local`
//!   and to each node below it.
//! - `query.run.write on service:clickhouse-dash/host:local/database:analytics`: the grant applies
//!   to one database of the host `local`.
//!
//! The part after the service has 0 to [`MAX_DEPTH`] segments. Each id is percent-encoded
//! (RFC 3986): each byte outside `A-Z a-z 0-9 - . _ ~` is `%XX`, with upper-case hex digits. Thus a
//! `/` or a `:` in an id never divides a segment. The encoding is canonical: the parser refuses a
//! different encoding of the same id. In memory, an id is decoded.
//!
//! The action `access` lets the user open the service: the access gateway decides it in
//! `/auth/verify`. The action `access` has grants on the service only. The other actions are the
//! names that a service publishes at `/.well-known/procurauth/actions`. The service decides them: it
//! asks the PDP of this server (see [`crate::pdp`]).
//!
//! A grant covers the node that it names and each node below it: the PDP compares the segments
//! one by one (prefix match). A grant on the service covers all the service. The superadmin has
//! all grants. A new user has no grant. The service declares its tree of types; see
//! [`crate::actions`] for the check of a grant against this tree.
//!
//! Interface: [`Grant`] (one grant, its text form), [`Segment`] (one node below the service),
//! [`encode_id`] and [`decode_id`] (the id encoding), [`Mode`] (where the grants come from),
//! [`Policy`] (the grants that apply to one user), [`holds`] and [`covers`] (the checks).

use std::collections::BTreeMap;
use std::fmt;

use crate::limits::Limits;

/// The action that lets a user open a service. Only the access gateway decides it.
pub const ACCESS: &str = "access";
/// The action that opens an MCP session and answers a method with no tool (binding 3.6a). It has no effect to
/// approve, so `confirm` has no effect on it and the portal refuses a grant of it that has `confirm`.
pub const MCP_SESSION: &str = "mcp.session";
/// The longest action name.
pub const MAX_ACTION_CHARS: usize = 64;
/// The longest service id in a grant.
pub const MAX_SERVICE_CHARS: usize = 64;
/// The longest resource type.
pub const MAX_RESOURCE_TYPE_CHARS: usize = 32;
/// The longest id of a node, in bytes after decoding.
pub const MAX_RESOURCE_ID_BYTES: usize = 256;
/// The largest number of segments after the service, in a grant or in a request.
pub const MAX_DEPTH: usize = 4;
/// The resource type of a whole service. It is the root of each tree.
pub const SERVICE_TYPE: &str = "service";

/// One node below the service, for example the database `analytics`. The service publishes its
/// resource types. The id is decoded and exact: the server does not change its case.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Segment {
    pub kind: String,
    pub id: String,
}

impl Segment {
    /// A segment with a decoded id. The type `service` is refused: it is the root.
    pub fn new(kind: &str, id: &str) -> Result<Self, String> {
        check_resource_type(kind)?;
        if kind == SERVICE_TYPE {
            return Err(format!(
                "the resource type {SERVICE_TYPE:?} is the whole service: it is never a segment"
            ));
        }
        check_resource_id(id)?;
        Ok(Self {
            kind: kind.to_string(),
            id: id.to_string(),
        })
    }

    /// A segment in its text form, `<type>:<encoded id>`.
    pub fn parse(text: &str) -> Result<Self, String> {
        let (kind, id) = text.split_once(':').ok_or_else(|| {
            format!("the segment {text:?} must be <type>:<id>, for example host:local")
        })?;
        Self::new(kind, &decode_id(id)?)
    }
}

impl fmt::Display for Segment {
    /// The text form, `<type>:<encoded id>`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.kind, encode_id(&self.id))
    }
}

/// Why a grant cannot carry `confirm`, or `None` when it can. The service `portal` has no action
/// that asks for an approval. The action `access` opens a service: a person cannot approve it one
/// time. The action `mcp.session` opens a session and answers the methods that have no tool: it has
/// no effect to approve, and a method of this kind cannot wait. The file refuses these grants at
/// start, and the forms refuse them too.
pub fn confirm_refusal(grant: &Grant) -> Option<&'static str> {
    if grant.service == crate::portal_actions::SERVICE {
        Some("confirm is not for a grant of the service portal")
    } else if grant.action == ACCESS {
        Some("confirm is not for the action access")
    } else if grant.action == MCP_SESSION {
        Some("confirm is not for the action mcp.session")
    } else {
        None
    }
}

/// Joins the grants that differ only by `confirm`: one grant stays, with `confirm` when one of
/// them has it (OR rule). The grants must be sorted.
pub fn merge_confirm(grants: &mut Vec<Grant>) {
    grants.dedup_by(|later, earlier| {
        let same = later.same_node(earlier) && later.limits == earlier.limits;
        if same {
            earlier.confirm |= later.confirm;
        }
        same
    });
}

/// The text form of a path: the segments joined by `/`. The path of the service itself is empty.
pub fn path_text(path: &[Segment]) -> String {
    path.iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("/")
}

/// Read a path in its text form. An empty text is the service itself.
pub fn parse_path(text: &str) -> Result<Vec<Segment>, String> {
    if text.is_empty() {
        return Ok(Vec::new());
    }
    let path = text
        .split('/')
        .map(Segment::parse)
        .collect::<Result<Vec<_>, _>>()?;
    if path.len() > MAX_DEPTH {
        return Err(format!(
            "the path has {} segments after the service, the maximum is {MAX_DEPTH}",
            path.len()
        ));
    }
    Ok(path)
}

/// One grant: the user can do `action` on the node `path` of the service `service`, and on each
/// node below it. An empty `path` is the whole service. A grant can carry limit values (see
/// [`crate::limits`]). The text form does not show them: the settings pages and the database keep
/// them apart.
///
/// A grant can also carry `confirm`: the holder can do the action only with the approval of a
/// person (design of the approvals, 6.3). It is a property of the grant, not a limit: it has no
/// value and no counter, and the text form does not show it. It is the last field, so two grants
/// that differ only by `confirm` sort one after the other and [`merge_confirm`] joins them.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Grant {
    pub action: String,
    pub service: String,
    pub path: Vec<Segment>,
    pub limits: Limits,
    pub confirm: bool,
}

impl Grant {
    /// A grant on the whole service.
    pub fn new(action: &str, service: &str) -> Result<Self, String> {
        check_action(action)?;
        check_service_id(service)?;
        Ok(Self {
            action: action.to_string(),
            service: service.to_string(),
            path: Vec::new(),
            limits: Limits::new(),
            confirm: false,
        })
    }

    /// The same grant with these limit values. The values are not checked here: [`Limits::check`]
    /// does it against the document of the service.
    pub fn with_limits(mut self, limits: Limits) -> Self {
        self.limits = limits;
        self
    }

    /// The same grant with `confirm` set to this value. The check of the action and the service
    /// that allow `confirm` is not here: [`confirm_refusal`] does it.
    pub fn with_confirm(mut self, confirm: bool) -> Self {
        self.confirm = confirm;
        self
    }

    /// Whether both grants name the same action and the same node, whatever their limit values.
    pub fn same_node(&self, other: &Self) -> bool {
        self.action == other.action && self.service == other.service && self.path == other.path
    }

    /// A grant on a node of the service. An empty path is the whole service. The action `access`
    /// has no grant below the service: it opens the whole service.
    pub fn on_path(action: &str, service: &str, path: Vec<Segment>) -> Result<Self, String> {
        let mut grant = Self::new(action, service)?;
        if path.is_empty() {
            return Ok(grant);
        }
        if action == ACCESS {
            return Err(format!(
                "the action {ACCESS:?} opens the whole service: it has no grant on a sub-resource"
            ));
        }
        if path.len() > MAX_DEPTH {
            return Err(format!(
                "the path has {} segments after the service, the maximum is {MAX_DEPTH}",
                path.len()
            ));
        }
        grant.path = path;
        Ok(grant)
    }

    /// A grant from the columns of the database: the path in its text form.
    pub fn from_path_text(action: &str, service: &str, path: &str) -> Result<Self, String> {
        Self::on_path(action, service, parse_path(path)?)
    }

    /// The text form: `<action> on service:<id>[/<type>:<encoded id>]...`. The parser does not
    /// change the text: a text with a different case, more spaces or a different encoding is
    /// refused.
    pub fn parse(text: &str) -> Result<Self, String> {
        let (action, resource) = text.split_once(" on ").ok_or_else(|| {
            "expected `<action> on service:<id>`, then optionally `/<type>:<id>` for each level"
                .to_string()
        })?;
        let target = resource
            .strip_prefix("service:")
            .ok_or_else(|| "the resource must start with service:<id>".to_string())?;
        match target.split_once('/') {
            None => Self::new(action, target),
            Some((service, path)) => {
                if path.is_empty() {
                    return Err(format!(
                        "the text {text:?} ends with '/': write a segment <type>:<id> after it"
                    ));
                }
                let path = parse_path(path)?;
                Self::on_path(action, service, path)
            }
        }
    }

    /// The text form of the path after the service. Empty for the whole service.
    pub fn path_text(&self) -> String {
        path_text(&self.path)
    }

    /// The type of the node that the grant names. `service` for the whole service.
    pub fn kind(&self) -> &str {
        self.path.last().map_or(SERVICE_TYPE, |s| s.kind.as_str())
    }
}

impl fmt::Display for Grant {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} on {SERVICE_TYPE}:{}", self.action, self.service)?;
        for segment in &self.path {
            write!(f, "/{segment}")?;
        }
        Ok(())
    }
}

/// Whether a byte stays as it is in an encoded id (the unreserved bytes of RFC 3986).
fn unreserved(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~')
}

/// The text form of an id: each byte outside `A-Z a-z 0-9 - . _ ~` becomes `%XX`, with upper-case
/// hex digits.
pub fn encode_id(id: &str) -> String {
    let mut out = String::with_capacity(id.len());
    for byte in id.bytes() {
        if unreserved(byte) {
            out.push(byte as char);
        } else {
            out.push('%');
            out.push(char::from(b"0123456789ABCDEF"[usize::from(byte >> 4)]));
            out.push(char::from(b"0123456789ABCDEF"[usize::from(byte & 0x0f)]));
        }
    }
    out
}

/// Decode the text form of an id. Only the canonical form is accepted: an unreserved byte as it
/// is, each other byte as `%XX` with upper-case hex digits. The decoded id must obey
/// [`check_resource_id`].
pub fn decode_id(text: &str) -> Result<String, String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        if unreserved(byte) {
            out.push(byte);
            index += 1;
            continue;
        }
        if byte != b'%' {
            return Err(format!(
                "the id {text:?} has the character {:?}: encode each byte outside A-Z a-z 0-9 - . _ ~ as %XX",
                char::from(byte)
            ));
        }
        let hex = |at: usize| -> Option<u8> {
            match bytes.get(at) {
                Some(b @ b'0'..=b'9') => Some(b - b'0'),
                Some(b @ b'A'..=b'F') => Some(b - b'A' + 10),
                _ => None,
            }
        };
        let (Some(high), Some(low)) = (hex(index + 1), hex(index + 2)) else {
            return Err(format!(
                "the id {text:?} has a bad escape: write %XX with two upper-case hex digits"
            ));
        };
        let decoded = high << 4 | low;
        if unreserved(decoded) {
            return Err(format!(
                "the id {text:?} encodes {:?}: write this character as it is",
                char::from(decoded)
            ));
        }
        out.push(decoded);
        index += 3;
    }
    let id = String::from_utf8(out).map_err(|_| format!("the id {text:?} is not UTF-8"))?;
    check_resource_id(&id)?;
    Ok(id)
}

/// An action name: lower case parts of `a-z`, `0-9` and `_`, joined by `.`. At most 64 characters.
pub fn check_action(action: &str) -> Result<(), String> {
    let valid = !action.is_empty()
        && action.len() <= MAX_ACTION_CHARS
        && action.split('.').all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        });
    if valid {
        Ok(())
    } else {
        Err(format!(
            "the action {action:?} must be parts of a-z, 0-9 and _, joined by '.', 1 to {MAX_ACTION_CHARS} characters"
        ))
    }
}

/// A service id as the catalogue makes it: lower-case letters and digits, joined by `-`.
pub fn check_service_id(id: &str) -> Result<(), String> {
    let valid = !id.is_empty()
        && id.chars().count() <= MAX_SERVICE_CHARS
        && !id.starts_with('-')
        && !id.ends_with('-')
        && !id.contains("--")
        && id
            .chars()
            .all(|c| c == '-' || (c.is_alphanumeric() && !c.is_uppercase()));
    if valid {
        Ok(())
    } else {
        Err(format!(
            "the service id {id:?} must be lower-case letters and digits joined by '-', 1 to {MAX_SERVICE_CHARS} characters"
        ))
    }
}

/// A resource type: `a-z` first, then `a-z`, `0-9` and `_`, 1 to 32 characters. The type
/// `service` is valid here.
pub fn check_resource_type(kind: &str) -> Result<(), String> {
    let valid = kind.len() <= MAX_RESOURCE_TYPE_CHARS
        && kind.as_bytes().first().is_some_and(u8::is_ascii_lowercase)
        && kind
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
    if valid {
        Ok(())
    } else {
        Err(format!(
            "the resource type {kind:?} must start with a-z, then a-z, 0-9 and _, 1 to {MAX_RESOURCE_TYPE_CHARS} characters"
        ))
    }
}

/// The decoded id of a node: 1 to 256 bytes of UTF-8, without a control character.
pub fn check_resource_id(id: &str) -> Result<(), String> {
    let valid =
        !id.is_empty() && id.len() <= MAX_RESOURCE_ID_BYTES && !id.chars().any(char::is_control);
    if valid {
        Ok(())
    } else {
        Err(format!(
            "the resource id {id:?} must be 1 to {MAX_RESOURCE_ID_BYTES} bytes, without a control character"
        ))
    }
}

/// Where the grants come from (the configuration key `access.grants_mode`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Only the `user` blocks of the configuration. The settings pages do not change the grants.
    Static,
    /// Only the settings pages.
    Dynamic,
    /// Both. The grants of the configuration are read-only in the settings pages.
    Hybrid,
}

impl Mode {
    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "static" => Ok(Self::Static),
            "dynamic" => Ok(Self::Dynamic),
            "hybrid" => Ok(Self::Hybrid),
            _ => Err(format!("invalid access.grants_mode: {value:?}")),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Static => "static",
            Self::Dynamic => "dynamic",
            Self::Hybrid => "hybrid",
        }
    }

    /// The settings pages can give and remove grants.
    pub fn editable(self) -> bool {
        !matches!(self, Self::Static)
    }

    /// The server uses the grants that the settings pages keep in the database.
    pub fn uses_stored(self) -> bool {
        !matches!(self, Self::Static)
    }
}

/// The grants that apply: the mode and the grants of the configuration. The grants of the
/// database come from the directory of the accounts.
#[derive(Clone, Debug)]
pub struct Policy {
    pub mode: Mode,
    /// The `user` blocks: login to grants, sorted.
    pub configured: BTreeMap<String, Vec<Grant>>,
}

impl Policy {
    pub fn new(mode: Mode, configured: BTreeMap<String, Vec<Grant>>) -> Self {
        Self { mode, configured }
    }

    /// The grants of the configuration for `login`.
    pub fn configured(&self, login: &str) -> &[Grant] {
        self.configured.get(login).map(Vec::as_slice).unwrap_or(&[])
    }

    /// All the grants of a user: those of the configuration and, unless the mode is `static`, those
    /// of the database. Sorted, without duplicates.
    pub fn effective(&self, login: &str, stored: &[Grant]) -> Vec<Grant> {
        let mut out: Vec<Grant> = self.configured(login).to_vec();
        if self.mode.uses_stored() {
            out.extend(stored.iter().cloned());
        }
        out.sort();
        merge_confirm(&mut out);
        out
    }
}

/// One counter of a budget: the service, the name of the budget and the window.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CounterId {
    pub service: String,
    pub budget: String,
    pub window: crate::limits::Window,
}

/// The counters that a set of grants needs for one subject: one for each window of each budget
/// value of each grant `access` on a service (portal 3.2a, 7.2a). Sorted, without duplicates.
pub fn counter_ids(grants: &[Grant]) -> Vec<CounterId> {
    let mut out: Vec<CounterId> = Vec::new();
    for grant in grants
        .iter()
        .filter(|g| g.action == ACCESS && g.path.is_empty())
    {
        for (name, value) in grant.limits.iter() {
            if let crate::limits::Value::Budget(budget) = value {
                out.extend(budget.items().iter().map(|item| CounterId {
                    service: grant.service.clone(),
                    budget: name.to_string(),
                    window: item.window,
                }));
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

/// Whether `grants` hold `action` on the whole service `service`. A grant below the service does
/// not count.
pub fn holds(grants: &[Grant], action: &str, service: &str) -> bool {
    covers(grants, action, service, &[])
}

/// Whether `grants` let the user do `action` on the node `chain` of the service `service`. The
/// chain is the path from the service to the node; empty for the whole service. A grant covers
/// the node when its path is a prefix of the chain: the types and the ids are equal, segment by
/// segment.
pub fn covers(grants: &[Grant], action: &str, service: &str, chain: &[Segment]) -> bool {
    grants.iter().any(|grant| {
        grant.action == action && grant.service == service && chain.starts_with(&grant.path)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grant(text: &str) -> Grant {
        Grant::parse(text).unwrap()
    }

    #[test]
    fn a_grant_reads_and_writes_the_same_text() {
        for text in [
            "access on service:grafana",
            "query.run.read on service:infra-clickhouse",
            "mcp.keys_write on service:db2",
        ] {
            assert_eq!(grant(text).to_string(), text);
        }
        let g = grant("query.run.write on service:db");
        assert_eq!(
            (g.action.as_str(), g.service.as_str()),
            ("query.run.write", "db")
        );
    }

    #[test]
    fn a_bad_grant_is_refused_and_never_changed_to_fit() {
        for bad in [
            "",
            "access",
            "access on grafana",
            "access on database:x",
            "Access on service:grafana",
            "access  on service:grafana",
            "access on service:Grafana",
            "access on service:",
            "access on service:-x",
            "access on service:a--b",
            "query..run on service:db",
            ".query on service:db",
            "query. on service:db",
            "query-run on service:db",
            " access on service:db",
            "access on service:db ",
        ] {
            assert!(Grant::parse(bad).is_err(), "{bad:?} must be refused");
        }
        assert!(Grant::parse(&format!("{} on service:db", "a".repeat(65))).is_err());
        assert!(Grant::parse(&format!("access on service:{}", "a".repeat(65))).is_err());
        assert!(
            Grant::parse("access on service:é-app").is_ok(),
            "a slug keeps letters"
        );
    }

    #[test]
    fn modes_say_where_the_grants_come_from() {
        let configured: BTreeMap<String, Vec<Grant>> = [(
            "alice".to_string(),
            vec![grant("access on service:grafana")],
        )]
        .into_iter()
        .collect();
        let stored = vec![
            grant("access on service:notes"),
            grant("access on service:grafana"),
        ];
        let names = |policy: &Policy, login: &str| -> Vec<String> {
            policy
                .effective(login, &stored)
                .iter()
                .map(|g| g.service.clone())
                .collect()
        };
        let static_policy = Policy::new(Mode::Static, configured.clone());
        assert_eq!(
            names(&static_policy, "alice"),
            ["grafana"],
            "the database is not read"
        );
        assert!(
            names(&static_policy, "bob").is_empty(),
            "a new user has no grant"
        );
        let hybrid = Policy::new(Mode::Hybrid, configured);
        assert_eq!(
            names(&hybrid, "alice"),
            ["grafana", "notes"],
            "no duplicate"
        );
        let dynamic = Policy::new(Mode::Dynamic, BTreeMap::new());
        assert_eq!(names(&dynamic, "bob"), ["grafana", "notes"]);
        assert!(!Mode::Static.editable() && Mode::Hybrid.editable() && Mode::Dynamic.editable());
        for mode in [Mode::Static, Mode::Dynamic, Mode::Hybrid] {
            assert_eq!(Mode::parse(mode.as_str()).unwrap(), mode);
        }
        assert!(Mode::parse("Static").is_err());
    }

    #[test]
    fn holds_needs_the_action_and_the_service() {
        let grants = vec![
            grant("access on service:a"),
            grant("query.run.read on service:b"),
        ];
        assert!(holds(&grants, "access", "a"));
        assert!(!holds(&grants, "access", "b"));
        assert!(holds(&grants, "query.run.read", "b"));
        assert!(!holds(&grants, "query.run.write", "b"));
    }

    fn seg(kind: &str, id: &str) -> Segment {
        Segment::new(kind, id).unwrap()
    }

    #[test]
    fn a_grant_with_segments_reads_and_writes_the_same_text() {
        for text in [
            "query.run.write on service:clickhouse-dash/database:analytics",
            "query.run.read on service:db/host:local",
            "query.run.read on service:db/host:local/database:analytics.events",
            "files.read on service:s3/bucket:a%2Fb%3Ac",
            "files.read on service:s3/host:h/bucket:b/prefix:x/object:%C3%A9t%C3%A9%20report",
            "files.read on service:s3/object:~a-b_c.d",
        ] {
            assert_eq!(grant(text).to_string(), text);
        }
        let g = grant("query.run.write on service:clickhouse-dash/host:local/database:analytics");
        assert_eq!(g.service, "clickhouse-dash");
        assert_eq!(g.path, [seg("host", "local"), seg("database", "analytics")]);
        assert_eq!(g.path_text(), "host:local/database:analytics");
        assert_eq!(g.kind(), "database");
        assert_eq!(grant("x on service:a").kind(), SERVICE_TYPE);
        let s3 = grant("files.read on service:s3/bucket:a%2Fb%3Ac");
        assert_eq!(s3.path[0].id, "a/b:c", "the id is decoded in memory");
        assert!(
            grant("x on service:a") < grant("x on service:a/database:b"),
            "the grant on the service sorts first"
        );
        assert_eq!(
            Grant::from_path_text("x", "a", "host:h/database:d").unwrap(),
            grant("x on service:a/host:h/database:d")
        );
        assert_eq!(
            Grant::from_path_text("x", "a", "").unwrap(),
            grant("x on service:a")
        );
    }

    #[test]
    fn an_id_has_one_text_form() {
        assert_eq!(encode_id("analytics"), "analytics");
        assert_eq!(encode_id("a/b:c d"), "a%2Fb%3Ac%20d");
        assert_eq!(encode_id("été"), "%C3%A9t%C3%A9");
        assert_eq!(encode_id("%41"), "%2541");
        for id in ["a", "a/b", "été", "%", "a b", "~-._", &"x".repeat(256)] {
            assert_eq!(decode_id(&encode_id(id)).unwrap(), id);
        }
        for bad in [
            "a%2fb", // lower-case hex
            "%41",   // an unreserved byte that needs no escape
            "a/b",   // a raw separator
            "a:b",   // a raw separator
            "a b",   // a raw space
            "é",     // a raw byte outside the set
            "%2",    // a short escape
            "%G0",   // not hex
            "%C3",   // not UTF-8
            "%0A",   // a control character
            "",      // empty
        ] {
            assert!(decode_id(bad).is_err(), "{bad:?} must be refused");
        }
        assert!(decode_id(&"x".repeat(257)).is_err(), "more than 256 bytes");
        assert!(
            decode_id(&"%C3%A9".repeat(129)).is_err(),
            "the limit is in bytes after decoding"
        );
        assert!(decode_id(&"%C3%A9".repeat(128)).is_ok());
    }

    #[test]
    fn a_bad_grant_with_segments_is_refused() {
        for bad in [
            "access on service:db/database:analytics",
            "query on service:db/",
            "query on service:db/database",
            "query on service:db/database:",
            "query on service:db/:analytics",
            "query on service:db/Database:analytics",
            "query on service:db/data-base:analytics",
            "query on service:db/1database:analytics",
            "query on service:db/service:db",
            "query on service:db/host:h/service:db",
            "query on service:db/database:a b",
            "query on service:db/database:a%2fb",
            "query on service:db/database:%61",
            "query on service:db/database:a\tb",
            "query on service:db/host:h//database:a",
            "query on service:db/host:h/",
            "query on service:Db/database:a",
            "query on service:db/a:1/b:2/c:3/d:4/e:5",
        ] {
            assert!(Grant::parse(bad).is_err(), "{bad:?} must be refused");
        }
        assert!(Grant::parse("query on service:db/a:1/b:2/c:3/d:4").is_ok());
        let long_type = format!("query on service:db/{}:a", "a".repeat(33));
        assert!(Grant::parse(&long_type).is_err());
        assert!(Grant::on_path("access", "db", vec![seg("database", "x")]).is_err());
        assert!(Grant::on_path("access", "db", Vec::new()).is_ok());
        assert!(Segment::new("service", "db").is_err());
        assert!(Grant::from_path_text("x", "db", "database:a/").is_err());
    }

    #[test]
    fn a_grant_covers_its_node_and_each_node_below_it() {
        let local = seg("host", "local");
        let other = seg("host", "other");
        let analytics = seg("database", "analytics");
        let logs = seg("database", "logs");
        let service = vec![grant("read on service:db")];
        assert!(covers(&service, "read", "db", &[]));
        assert!(covers(&service, "read", "db", std::slice::from_ref(&local)));
        assert!(covers(
            &service,
            "read",
            "db",
            &[local.clone(), analytics.clone()]
        ));
        assert!(!covers(
            &service,
            "read",
            "other",
            std::slice::from_ref(&local)
        ));
        assert!(!covers(
            &service,
            "write",
            "db",
            std::slice::from_ref(&local)
        ));

        let host = vec![grant("read on service:db/host:local")];
        assert!(covers(&host, "read", "db", std::slice::from_ref(&local)));
        assert!(covers(&host, "read", "db", &[local.clone(), logs.clone()]));
        assert!(
            !covers(&host, "read", "db", &[other.clone(), logs.clone()]),
            "another host"
        );
        assert!(!covers(&host, "read", "db", &[]), "not the whole service");
        assert!(!holds(&host, "read", "db"));
        assert!(
            !covers(&host, "read", "db", &[seg("host", "Local")]),
            "the id is exact"
        );
        assert!(
            !covers(&host, "read", "db", &[seg("server", "local")]),
            "another type"
        );

        let one = vec![grant("write on service:db/host:local/database:analytics")];
        assert!(covers(
            &one,
            "write",
            "db",
            &[local.clone(), analytics.clone()]
        ));
        assert!(!covers(&one, "write", "db", &[local.clone(), logs.clone()]));
        assert!(
            !covers(&one, "write", "db", std::slice::from_ref(&local)),
            "not the parent"
        );
        assert!(
            !covers(&one, "write", "db", &[other.clone(), analytics.clone()]),
            "the same database on another host"
        );
        assert!(
            !covers(&one, "write", "db", std::slice::from_ref(&analytics)),
            "the database without its host"
        );

        let encoded = vec![grant("read on service:s3/bucket:a%2Fb")];
        assert!(covers(&encoded, "read", "s3", &[seg("bucket", "a/b")]));
        assert!(
            !covers(&encoded, "read", "s3", &[seg("bucket", "a%2Fb")]),
            "the ids are compared decoded"
        );
        assert!(
            !covers(&encoded, "read", "s3", &[seg("bucket", "a"), seg("b", "x")]),
            "a / in an id never divides a segment"
        );
    }

    #[test]
    fn confirm_is_not_a_limit_and_not_in_the_text_form() {
        let plain = grant("data.write on service:db/host:prod");
        let asking = plain.clone().with_confirm(true);
        assert!(asking.confirm && !plain.confirm);
        // The text form does not show it, and the limit values do not hold it.
        assert_eq!(asking.to_string(), plain.to_string());
        assert_eq!(asking.limits, plain.limits);
        assert!(asking.same_node(&plain));
        assert_ne!(asking, plain);
        // The parser gives grants without confirm: a text cannot ask for it.
        assert!(!grant(&asking.to_string()).confirm);
    }

    #[test]
    fn two_grants_that_differ_only_by_confirm_merge_with_or() {
        let plain = grant("data.write on service:db/host:prod");
        let asking = plain.clone().with_confirm(true);
        let other = grant("data.read on service:db/host:prod");
        let configured: BTreeMap<String, Vec<Grant>> =
            [("alice".to_string(), vec![plain.clone(), other.clone()])]
                .into_iter()
                .collect();
        let policy = Policy::new(Mode::Hybrid, configured);
        let merged = policy.effective("alice", std::slice::from_ref(&asking));
        assert_eq!(
            merged,
            [other.clone(), asking.clone()],
            "one grant, with confirm"
        );
        // The order of the sources does not matter.
        let configured: BTreeMap<String, Vec<Grant>> =
            [("alice".to_string(), vec![asking.clone()])]
                .into_iter()
                .collect();
        let policy = Policy::new(Mode::Hybrid, configured);
        assert_eq!(
            policy.effective("alice", &[plain, other.clone()]),
            [other, asking]
        );
    }

    #[test]
    fn confirm_is_refused_on_the_service_portal_on_access_and_on_mcp_session() {
        let on_portal = grant("alert.send on service:portal").with_confirm(true);
        let on_access = grant("access on service:db").with_confirm(true);
        let fine = grant("data.write on service:db").with_confirm(true);
        let on_session = grant("mcp.session on service:db").with_confirm(true);
        assert!(confirm_refusal(&on_portal).is_some());
        assert!(confirm_refusal(&on_access).is_some());
        assert!(confirm_refusal(&on_session).is_some());
        assert!(confirm_refusal(&fine).is_none());
    }
}
