//! The grants of the users: an action on a resource.
//!
//! A grant has one of two text forms:
//!
//! - `<action> on service:<id>`, for example `access on service:grafana`. The grant applies to the
//!   whole service.
//! - `<action> on service:<id>/<type>:<resource id>`, for example
//!   `query.run.write on service:clickhouse-dash/database:analytics`. The grant applies to one
//!   sub-resource of the service.
//!
//! The action `access` lets the user open the service: the access gateway decides it in
//! `/auth/verify`. The action `access` has no grant on a sub-resource. The other actions are the
//! names that a service publishes at `/.well-known/access-actions`. The service decides them: it
//! asks the PDP of this server (see [`crate::pdp`]).
//!
//! A grant on a service covers all the sub-resources of this service. A grant on a sub-resource
//! covers only this sub-resource. The superadmin has all grants. A new user has no grant.
//!
//! Interface: [`Grant`] (one grant, its text form), [`SubResource`] (a resource in a service),
//! [`Mode`] (where the grants come from), [`Policy`] (the grants that apply to one user),
//! [`holds`] and [`covers`] (the checks).

use std::collections::BTreeMap;
use std::fmt;

/// The action that lets a user open a service. Only the access gateway decides it.
pub const ACCESS: &str = "access";
/// The longest action name.
pub const MAX_ACTION_CHARS: usize = 64;
/// The longest service id in a grant.
pub const MAX_SERVICE_CHARS: usize = 64;
/// The longest resource type.
pub const MAX_RESOURCE_TYPE_CHARS: usize = 32;
/// The longest id of a sub-resource.
pub const MAX_RESOURCE_ID_CHARS: usize = 128;
/// The largest number of grants of one user, in the configuration or in the database.
pub const MAX_GRANTS_PER_USER: usize = 512;
/// The resource type of a whole service.
pub const SERVICE_TYPE: &str = "service";

/// A resource in a service, for example the database `analytics`. The service publishes its
/// resource types. The id is exact: the server does not change its case.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SubResource {
    pub kind: String,
    pub id: String,
}

impl SubResource {
    pub fn new(kind: &str, id: &str) -> Result<Self, String> {
        check_resource_type(kind)?;
        if kind == SERVICE_TYPE {
            return Err(format!(
                "the resource type {SERVICE_TYPE:?} is the whole service: write `<action> on service:<id>`"
            ));
        }
        check_resource_id(id)?;
        Ok(Self {
            kind: kind.to_string(),
            id: id.to_string(),
        })
    }
}

impl fmt::Display for SubResource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.kind, self.id)
    }
}

/// One grant: the user can do `action` on the service `service`. When `resource` is given, the
/// grant applies only to this sub-resource of the service.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Grant {
    pub action: String,
    pub service: String,
    pub resource: Option<SubResource>,
}

impl Grant {
    /// A grant on the whole service.
    pub fn new(action: &str, service: &str) -> Result<Self, String> {
        check_action(action)?;
        check_service_id(service)?;
        Ok(Self {
            action: action.to_string(),
            service: service.to_string(),
            resource: None,
        })
    }

    /// A grant on one sub-resource of the service. The action `access` is refused: it opens the
    /// whole service.
    pub fn on_resource(action: &str, service: &str, kind: &str, id: &str) -> Result<Self, String> {
        let mut grant = Self::new(action, service)?;
        if action == ACCESS {
            return Err(format!(
                "the action {ACCESS:?} opens the whole service: it has no grant on a sub-resource"
            ));
        }
        grant.resource = Some(SubResource::new(kind, id)?);
        Ok(grant)
    }

    /// The text form: `<action> on service:<id>` or `<action> on service:<id>/<type>:<id>`. The
    /// parser does not change the text: a text with a different case or more spaces is refused.
    pub fn parse(text: &str) -> Result<Self, String> {
        let (action, resource) = text.split_once(" on ").ok_or_else(|| {
            "expected `<action> on service:<id>` or `<action> on service:<id>/<type>:<id>`"
                .to_string()
        })?;
        let target = resource
            .strip_prefix("service:")
            .ok_or_else(|| "the resource must start with service:<id>".to_string())?;
        match target.split_once('/') {
            None => Self::new(action, target),
            Some((service, sub)) => {
                let (kind, id) = sub.split_once(':').ok_or_else(|| {
                    format!("the sub-resource {sub:?} must be <type>:<id>, for example database:analytics")
                })?;
                Self::on_resource(action, service, kind, id)
            }
        }
    }
}

impl fmt::Display for Grant {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} on {SERVICE_TYPE}:{}", self.action, self.service)?;
        if let Some(resource) = &self.resource {
            write!(f, "/{resource}")?;
        }
        Ok(())
    }
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

/// A resource type: `a-z`, `0-9` and `_`, 1 to 32 characters. The type `service` is valid here.
pub fn check_resource_type(kind: &str) -> Result<(), String> {
    let valid = !kind.is_empty()
        && kind.len() <= MAX_RESOURCE_TYPE_CHARS
        && kind
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
    if valid {
        Ok(())
    } else {
        Err(format!(
            "the resource type {kind:?} must be a-z, 0-9 and _, 1 to {MAX_RESOURCE_TYPE_CHARS} characters"
        ))
    }
}

/// The id of a sub-resource: 1 to 128 characters, without a space or a control character.
pub fn check_resource_id(id: &str) -> Result<(), String> {
    let valid = !id.is_empty()
        && id.chars().count() <= MAX_RESOURCE_ID_CHARS
        && !id.chars().any(|c| c.is_whitespace() || c.is_control());
    if valid {
        Ok(())
    } else {
        Err(format!(
            "the resource id {id:?} must be 1 to {MAX_RESOURCE_ID_CHARS} characters, without a space or a control character"
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
        out.dedup();
        out
    }
}

/// Whether `grants` hold `action` on the whole service `service`. A grant on a sub-resource does
/// not count.
pub fn holds(grants: &[Grant], action: &str, service: &str) -> bool {
    covers(grants, action, service, None)
}

/// Whether `grants` let the user do `action` on `resource` of the service `service`. `None` is
/// the whole service. A grant on the service covers all its sub-resources. A grant on a
/// sub-resource covers only the same type and the same id.
pub fn covers(
    grants: &[Grant],
    action: &str,
    service: &str,
    resource: Option<&SubResource>,
) -> bool {
    grants.iter().any(|grant| {
        grant.action == action
            && grant.service == service
            && (grant.resource.is_none() || grant.resource.as_ref() == resource)
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

    #[test]
    fn a_grant_on_a_sub_resource_reads_and_writes_the_same_text() {
        for text in [
            "query.run.write on service:clickhouse-dash/database:analytics",
            "query.run.read on service:db/table:analytics.events",
            "files.read on service:s3/bucket:a/b:c",
        ] {
            assert_eq!(grant(text).to_string(), text);
        }
        let g = grant("query.run.write on service:clickhouse-dash/database:analytics");
        assert_eq!(g.service, "clickhouse-dash");
        assert_eq!(
            g.resource,
            Some(SubResource {
                kind: "database".into(),
                id: "analytics".into()
            })
        );
        let s3 = grant("files.read on service:s3/bucket:a/b:c");
        assert_eq!(s3.resource.unwrap().id, "a/b:c", "the id keeps its / and :");
        assert!(
            grant("x on service:a") < grant("x on service:a/database:b"),
            "the grant on the service sorts first"
        );
    }

    #[test]
    fn a_bad_grant_on_a_sub_resource_is_refused() {
        for bad in [
            "access on service:db/database:analytics",
            "query on service:db/",
            "query on service:db/database",
            "query on service:db/database:",
            "query on service:db/:analytics",
            "query on service:db/Database:analytics",
            "query on service:db/data-base:analytics",
            "query on service:db/service:db",
            "query on service:db/database:a b",
            "query on service:db/database:a\tb",
            "query on service:Db/database:a",
        ] {
            assert!(Grant::parse(bad).is_err(), "{bad:?} must be refused");
        }
        let long_id = format!("query on service:db/database:{}", "a".repeat(129));
        assert!(Grant::parse(&long_id).is_err());
        let long_type = format!("query on service:db/{}:a", "a".repeat(33));
        assert!(Grant::parse(&long_type).is_err());
        assert!(Grant::parse(&format!("query on service:db/database:{}", "é".repeat(128))).is_ok());
        assert!(Grant::on_resource("access", "db", "database", "x").is_err());
    }

    #[test]
    fn a_grant_on_the_service_covers_its_sub_resources_and_not_the_reverse() {
        let analytics = SubResource::new("database", "analytics").unwrap();
        let logs = SubResource::new("database", "logs").unwrap();
        let table = SubResource::new("table", "analytics").unwrap();
        let service = vec![grant("query.run.read on service:db")];
        assert!(covers(&service, "query.run.read", "db", None));
        assert!(covers(&service, "query.run.read", "db", Some(&analytics)));
        assert!(covers(&service, "query.run.read", "db", Some(&table)));
        assert!(!covers(
            &service,
            "query.run.read",
            "other",
            Some(&analytics)
        ));
        assert!(!covers(&service, "query.run.write", "db", Some(&analytics)));
        let one = vec![grant("query.run.write on service:db/database:analytics")];
        assert!(covers(&one, "query.run.write", "db", Some(&analytics)));
        assert!(
            !covers(&one, "query.run.write", "db", Some(&logs)),
            "another id"
        );
        assert!(
            !covers(&one, "query.run.write", "db", Some(&table)),
            "another type"
        );
        assert!(
            !covers(&one, "query.run.write", "db", None),
            "not the whole service"
        );
        assert!(!holds(&one, "query.run.write", "db"));
        assert!(
            !covers(&one, "query.run.read", "db", Some(&analytics)),
            "another action"
        );
        assert!(
            !covers(&one, "query.run.write", "db2", Some(&analytics)),
            "another service"
        );
    }
}
