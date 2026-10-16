//! The grants of the users: an action on a resource.
//!
//! A grant is written `<action> on service:<id>`, for example `access on service:grafana`. The
//! action `access` lets the user open the service: the access gateway decides it in `/auth/verify`.
//! The other actions are the names that a service publishes at `/.well-known/access-actions`. The
//! service decides them: it asks the PDP of this server (see [`crate::pdp`]).
//!
//! A grant on a service covers all the resources of this service. The superadmin has all grants.
//! A new user has no grant.
//!
//! Interface: [`Grant`] (one grant, its text form), [`Mode`] (where the grants come from) and
//! [`Policy`] (the grants that apply to one user).

use std::collections::BTreeMap;
use std::fmt;

/// The action that lets a user open a service. Only the access gateway decides it.
pub const ACCESS: &str = "access";
/// The longest action name.
pub const MAX_ACTION_CHARS: usize = 64;
/// The longest service id in a grant.
pub const MAX_SERVICE_CHARS: usize = 64;
/// The only resource type of a grant.
pub const SERVICE_TYPE: &str = "service";

/// One grant: the user can do `action` on the service `service`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Grant {
    pub action: String,
    pub service: String,
}

impl Grant {
    pub fn new(action: &str, service: &str) -> Result<Self, String> {
        check_action(action)?;
        check_service_id(service)?;
        Ok(Self {
            action: action.to_string(),
            service: service.to_string(),
        })
    }

    /// The text form: `<action> on service:<id>`. The parser does not change the text: a text
    /// with a different case or more spaces is refused.
    pub fn parse(text: &str) -> Result<Self, String> {
        let (action, resource) = text
            .split_once(" on ")
            .ok_or_else(|| "expected `<action> on service:<id>`".to_string())?;
        let service = resource
            .strip_prefix("service:")
            .ok_or_else(|| "the resource must be service:<id>".to_string())?;
        Self::new(action, service)
    }
}

impl fmt::Display for Grant {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} on {SERVICE_TYPE}:{}", self.action, self.service)
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

/// Whether `grants` hold `action` on the service `service`.
pub fn holds(grants: &[Grant], action: &str, service: &str) -> bool {
    grants
        .iter()
        .any(|grant| grant.action == action && grant.service == service)
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
        assert_eq!((g.action.as_str(), g.service.as_str()), ("query.run.write", "db"));
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
        assert!(Grant::parse("access on service:é-app").is_ok(), "a slug keeps letters");
    }

    #[test]
    fn modes_say_where_the_grants_come_from() {
        let configured: BTreeMap<String, Vec<Grant>> = [(
            "alice".to_string(),
            vec![grant("access on service:grafana")],
        )]
        .into_iter()
        .collect();
        let stored = vec![grant("access on service:notes"), grant("access on service:grafana")];
        let names = |policy: &Policy, login: &str| -> Vec<String> {
            policy
                .effective(login, &stored)
                .iter()
                .map(|g| g.service.clone())
                .collect()
        };
        let static_policy = Policy::new(Mode::Static, configured.clone());
        assert_eq!(names(&static_policy, "alice"), ["grafana"], "the database is not read");
        assert!(names(&static_policy, "bob").is_empty(), "a new user has no grant");
        let hybrid = Policy::new(Mode::Hybrid, configured);
        assert_eq!(names(&hybrid, "alice"), ["grafana", "notes"], "no duplicate");
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
        let grants = vec![grant("access on service:a"), grant("query.run.read on service:b")];
        assert!(holds(&grants, "access", "a"));
        assert!(!holds(&grants, "access", "b"));
        assert!(holds(&grants, "query.run.read", "b"));
        assert!(!holds(&grants, "query.run.write", "b"));
    }
}
