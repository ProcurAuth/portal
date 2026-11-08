//! The forms of the page "Keys": make a key and change a key (portal 5.8c, 5.8d, 4.1 to 4.2c).
//!
//! The module reads the fields of a form, and it checks them against the rules of the keys. It has
//! no input or output: the caller gives the grants of the owner, the documents of the services and
//! the time. A refusal is a code of portal 5.8f. The page shows a fixed text for each code.
//!
//! The fields of a form:
//! - `name`: the name of the key (portal 4.1);
//! - `expires_at`: a date and a time in UTC, or empty for no expiry date (only for a new key);
//! - `grant<n>`: the text form of a grant. `<n>` is a whole number from 0, with no leading zero;
//! - `limit<n>.<name>`: the value of a ceiling of the grant `<n>`;
//! - `limit<n>.<name>.<window>`: the amount of one window of a budget of the grant `<n>`.
//!
//! A field that is unknown, or a field name that has none of these forms, is a bad request (HTTP
//! 400). The caller reads the fields with `parse_form`, which refuses a repeated field.

#![allow(dead_code)]

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use crate::actions::Document;
use crate::grants::{Grant, Segment, ACCESS};
use crate::keys::KeyKind;
use crate::limits::{Budget, Item, Limits, Value, Window};
use crate::portal_actions;

/// A key holds at most 512 grants (portal 4.1b).
pub const MAX_GRANTS: usize = 512;
/// The largest number of a grant field. The page makes numbers from 0 up.
const MAX_FIELD_NUMBER: usize = 99_999;
/// A grant text is at most 4 KiB after it is decoded (portal 5.8c).
pub const MAX_GRANT_TEXT: usize = 4096;

/// A limit field of a grant.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LimitField {
    pub name: String,
    /// `None` for a ceiling, `Some` for the window of a budget.
    pub window: Option<Window>,
    /// The text of the value. Empty: no value.
    pub value: String,
}

/// The fields of a form, read but not checked.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RawForm {
    pub name: String,
    /// The text of the field `expires_at`. `None` when the form has no such field.
    pub expires_at: Option<String>,
    pub grants: BTreeMap<usize, String>,
    pub limits: BTreeMap<usize, Vec<LimitField>>,
}

/// A form that the rules accept.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyForm {
    pub name: String,
    /// The expiry date. `None`: no expiry date.
    pub expires_at: Option<i64>,
    /// Sorted.
    pub grants: Vec<Grant>,
}

/// A field number: whole decimal digits, no leading zero.
fn number(text: &str) -> Option<usize> {
    let valid = !text.is_empty()
        && text.len() <= 5
        && text.bytes().all(|b| b.is_ascii_digit())
        && (text == "0" || !text.starts_with('0'));
    valid
        .then(|| text.parse().ok())
        .flatten()
        .filter(|n| *n <= MAX_FIELD_NUMBER)
}

/// Reads the fields. `edit` is true for the form of a change: it has no `expires_at`. The fields
/// `csrf` and `id` are not here: the caller took them. `Err`: a bad request.
pub fn read(mut fields: HashMap<String, String>, edit: bool) -> Result<RawForm, String> {
    let mut form = RawForm {
        name: fields.remove("name").unwrap_or_default(),
        ..RawForm::default()
    };
    if !edit {
        form.expires_at = fields.remove("expires_at");
    }
    for (field, value) in fields {
        if let Some(rest) = field.strip_prefix("grant") {
            let n = number(rest).ok_or_else(|| format!("the field {field:?} is not known"))?;
            if value.len() > MAX_GRANT_TEXT {
                return Err(format!("the field {field:?} is too long"));
            }
            form.grants.insert(n, value);
        } else if let Some(rest) = field.strip_prefix("limit") {
            let (n, rest) = rest
                .split_once('.')
                .ok_or_else(|| format!("the field {field:?} is not known"))?;
            let n = number(n).ok_or_else(|| format!("the field {field:?} is not known"))?;
            let (name, window) = match rest.split_once('.') {
                Some((name, window)) => (
                    name,
                    Some(Window::parse(window).map_err(|_| {
                        format!("the field {field:?} has a window that is not known")
                    })?),
                ),
                None => (rest, None),
            };
            crate::limits::check_name(name)
                .map_err(|_| format!("the field {field:?} has a bad limit name"))?;
            form.limits.entry(n).or_default().push(LimitField {
                name: name.to_string(),
                window,
                value,
            });
        } else {
            return Err(format!("the field {field:?} is not known"));
        }
    }
    Ok(form)
}

/// What the check needs from the rest of the portal.
pub struct Context<'a> {
    /// The time of the request, in Unix seconds.
    pub now: i64,
    /// The grants of the owner. `None`: the owner is the superadmin, who has every right.
    pub owner_grants: Option<&'a [Grant]>,
    /// The `max_key_lifetime` of the owner, in seconds (portal 5.3). `None`: no value.
    pub max_lifetime: Option<u64>,
    /// The last good document of a service. `None`: the portal has none.
    pub document: &'a dyn Fn(&str) -> Option<Arc<Document>>,
    /// Whether the catalogue holds the service.
    pub service_exists: &'a dyn Fn(&str) -> bool,
    /// Whether the service is an external service: a link with no action. Its only grant is `access` on the whole
    /// service, and only an account holds it.
    pub is_external: &'a dyn Fn(&str) -> bool,
    /// The grants of the key before the change, for the form of a change.
    pub old_grants: Option<&'a [Grant]>,
}

/// The codes of portal 5.8f that the check gives.
pub const BAD_NAME: &str = "bad_name";
pub const BAD_EXPIRY: &str = "bad_expiry";
pub const BAD_GRANT: &str = "bad_grant";
pub const NOT_COVERED: &str = "not_covered";
pub const BAD_LIMITS: &str = "bad_limits";
/// A right on a service, and no grant `access` on this service.
pub const NEEDS_ACCESS: &str = "needs_access";

/// Checks the expiry date of a new key (portal 5.3, 5.3a).
fn check_expiry(text: Option<&str>, ctx: &Context<'_>) -> Result<Option<i64>, &'static str> {
    let at = match text {
        None | Some("") => None,
        Some(text) => Some(crate::util::parse_utc_instant(text).ok_or(BAD_EXPIRY)?),
    };
    if let Some(at) = at {
        if at <= ctx.now {
            return Err(BAD_EXPIRY);
        }
    }
    // With a maximum lifetime, a new key needs an expiry date, and its lifetime is at most the
    // maximum. The value 0 refuses each new key (portal 5.3a).
    if let Some(max) = ctx.max_lifetime {
        let Some(at) = at else {
            return Err(BAD_EXPIRY);
        };
        if u64::try_from(at - ctx.now).map_or(true, |lifetime| lifetime > max) {
            return Err(BAD_EXPIRY);
        }
    }
    Ok(at)
}

/// The grants of the form, from their texts. Empty fields are blank rows. The check stops at the
/// first grant over [`MAX_GRANTS`], and it finds a repeated node in a hash set. Thus the work stays
/// linear in [`MAX_GRANTS`], whatever the number of fields.
fn read_grants(raw: &RawForm) -> Result<Vec<(usize, Grant)>, &'static str> {
    let mut out: Vec<(usize, Grant)> = Vec::new();
    let mut nodes: HashSet<(String, String, Vec<Segment>)> = HashSet::new();
    for (n, text) in &raw.grants {
        if text.is_empty() {
            continue;
        }
        if out.len() == MAX_GRANTS {
            return Err(BAD_GRANT);
        }
        let grant = Grant::parse(text).map_err(|_| BAD_GRANT)?;
        let node = (
            grant.action.clone(),
            grant.service.clone(),
            grant.path.clone(),
        );
        if !nodes.insert(node) {
            return Err(BAD_GRANT);
        }
        out.push((*n, grant));
    }
    Ok(out)
}

/// The values of the limit fields of grant `n`, as a set of limit values.
pub fn limits_of(fields: &[LimitField]) -> Result<Limits, &'static str> {
    let mut ceilings: Vec<(&str, u64)> = Vec::new();
    let mut budgets: BTreeMap<&str, Vec<Item>> = BTreeMap::new();
    for field in fields {
        if field.value.is_empty() {
            continue;
        }
        let amount: u64 =
            if field.value.bytes().all(|b| b.is_ascii_digit()) && field.value.len() <= 12 {
                field.value.parse().map_err(|_| BAD_LIMITS)?
            } else {
                return Err(BAD_LIMITS);
            };
        match field.window {
            None => ceilings.push((&field.name, amount)),
            Some(window) => budgets
                .entry(&field.name)
                .or_default()
                .push(Item { amount, window }),
        }
    }
    let mut limits = Limits::new();
    for (name, value) in ceilings {
        limits
            .set(name, Value::Ceiling(value))
            .map_err(|_| BAD_LIMITS)?;
    }
    for (name, items) in budgets {
        if limits.get(name).is_some() {
            return Err(BAD_LIMITS);
        }
        let budget = Budget::new(items).map_err(|_| BAD_LIMITS)?;
        limits
            .set(name, Value::Budget(budget))
            .map_err(|_| BAD_LIMITS)?;
    }
    Ok(limits)
}

/// Whether a value set only narrows the old one: each old ceiling has a new value that is as low,
/// and each old budget window has a new amount that is as low (portal 4.2c).
fn only_narrows(old: &Limits, new: &Limits) -> bool {
    old.iter()
        .all(|(name, old_value)| match (old_value, new.get(name)) {
            (Value::Ceiling(old), Some(Value::Ceiling(new))) => new <= old,
            (Value::Budget(old), Some(Value::Budget(new))) => old.items().iter().all(|item| {
                new.amount(item.window)
                    .is_some_and(|amount| amount <= item.amount)
            }),
            _ => false,
        })
}

/// Whether the rights of the owner cover the grant: a grant of the owner with the same action on
/// the same service, on the node of the grant or above it (portal 4.2).
fn covered(owner: Option<&[Grant]>, grant: &Grant) -> bool {
    match owner {
        None => true,
        Some(held) => held.iter().any(|g| {
            g.action == grant.action
                && g.service == grant.service
                && grant.path.starts_with(&g.path)
        }),
    }
}

/// Who holds the grants of a form.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Holder {
    /// A key of this kind. It holds `access` on the service `portal` and the actions of the service
    /// `portal` that its kind lists (portal 6.5). An API key holds no grant on another service: the
    /// API of the inbox verifies it, and no gate opens for it.
    Key(KeyKind),
    /// An account: the portal features are in the editor next to the services, so the form gives
    /// grants on the service `portal` too.
    Account,
}

/// The first service where `grants` give a right and no grant `access`. A right on a service works
/// only when the holder can open the service, so the right alone is dead. The service `portal`
/// has no gate: its rights work as they are (portal 4.2b).
pub fn service_without_access(grants: &[Grant]) -> Option<&str> {
    grants
        .iter()
        .filter(|g| g.service != portal_actions::SERVICE && g.action != ACCESS)
        .map(|g| g.service.as_str())
        .find(|service| {
            !grants
                .iter()
                .any(|a| a.action == ACCESS && a.path.is_empty() && a.service == *service)
        })
}

/// Checks the grants of a form and their limit values, against the documents of the services. The
/// rights of the owner are not checked here.
pub fn check_grants(
    raw: &RawForm,
    ctx: &Context<'_>,
    holder: Holder,
) -> Result<Vec<Grant>, &'static str> {
    let grants = check_grants_apart(raw, ctx, holder)?;
    if service_without_access(&grants).is_some() {
        return Err(NEEDS_ACCESS);
    }
    Ok(grants)
}

/// The grants of a form that the check refuses with `NEEDS_ACCESS`, and the service that has no
/// `access`. The page uses it to draw the form again with the grants that the person ticked.
/// `None`: the check refuses the form for another reason, or accepts it.
pub fn grants_without_access(
    raw: &RawForm,
    ctx: &Context<'_>,
    holder: Holder,
) -> Option<(Vec<Grant>, String)> {
    let grants = check_grants_apart(raw, ctx, holder).ok()?;
    let service = service_without_access(&grants)?.to_string();
    Some((grants, service))
}

/// The check of `check_grants` without the rule of `access`.
fn check_grants_apart(
    raw: &RawForm,
    ctx: &Context<'_>,
    holder: Holder,
) -> Result<Vec<Grant>, &'static str> {
    let mut grants = Vec::new();
    for (n, grant) in read_grants(raw)? {
        let limits = match raw.limits.get(&n) {
            Some(fields) => limits_of(fields)?,
            None => Limits::new(),
        };
        // An external service has no document and no action. A key never holds a right on it. An account holds
        // `access` on the whole service and nothing else, whatever it held before.
        if (ctx.is_external)(&grant.service)
            && (holder != Holder::Account
                || grant.action != ACCESS
                || !grant.path.is_empty()
                || !limits.is_empty())
        {
            return Err(BAD_GRANT);
        }
        // An account keeps a grant that it holds and that the form does not change, also when the
        // document of the service does not name it now. A person can then remove it.
        if holder == Holder::Account
            && ctx.old_grants.is_some_and(|old| {
                old.iter()
                    .any(|o| o.same_node(&grant) && o.limits == limits)
            })
        {
            grants.push(grant.with_limits(limits));
            continue;
        }
        if let Holder::Key(kind) = holder {
            let on_portal = grant.service == portal_actions::SERVICE;
            if on_portal
                && grant.action != ACCESS
                && !portal_actions::key_actions(kind).contains(&grant.action.as_str())
            {
                return Err(BAD_GRANT);
            }
            if !on_portal && kind == KeyKind::Api {
                return Err(BAD_GRANT);
            }
        }
        if !(ctx.service_exists)(&grant.service) && grant.service != portal_actions::SERVICE {
            return Err(BAD_GRANT);
        }
        let document = (ctx.document)(&grant.service);
        let mut declarations: &[crate::limits::Declaration] = &[];
        let mut listed: Vec<String> = Vec::new();
        if grant.action != ACCESS {
            // An action that the document of the service does not publish is unknown, and a
            // grant must follow the tree of the service.
            let document = document.as_ref().ok_or(BAD_GRANT)?;
            let action = document
                .actions
                .iter()
                .find(|a| a.name == grant.action)
                .ok_or(BAD_GRANT)?;
            if grant.path.is_empty() && !action.resource_types.iter().any(|t| t == "service") {
                return Err(BAD_GRANT);
            }
            document.check_grant(&grant).map_err(|_| BAD_GRANT)?;
            // The nodes of the service `portal` are the groups of the inbox and the kinds of key.
            if grant.service == portal_actions::SERVICE
                && !grant.path.is_empty()
                && !document.resources.iter().any(|r| r.path == grant.path)
            {
                return Err(BAD_GRANT);
            }
            listed = action.limits.clone();
        }
        if let Some(document) = &document {
            declarations = &document.limits;
        }
        if !limits.is_empty() {
            if document.is_none() {
                return Err(BAD_LIMITS);
            }
            limits
                .check(declarations, &grant.action, &listed)
                .map_err(|_| BAD_LIMITS)?;
        }
        let compact = limits.to_json();
        if compact.len() > crate::limits::MAX_JSON_BYTES {
            return Err(BAD_LIMITS);
        }
        grants.push(grant.with_limits(limits));
    }
    // A limit field of a grant number that has no grant gives no value.
    for (n, fields) in &raw.limits {
        if raw.grants.get(n).is_none_or(|t| t.is_empty())
            && fields.iter().any(|f| !f.value.is_empty())
        {
            return Err(BAD_LIMITS);
        }
    }
    Ok(grants)
}

/// Checks a form for a key of this kind. A form for a new key gives `old_grants: None`.
pub fn check(raw: &RawForm, ctx: &Context<'_>, kind: KeyKind) -> Result<KeyForm, &'static str> {
    crate::keys::check_name(&raw.name).map_err(|_| BAD_NAME)?;
    let expires_at = if ctx.old_grants.is_none() {
        check_expiry(raw.expires_at.as_deref(), ctx)?
    } else {
        None
    };
    let mut grants = check_grants(raw, ctx, Holder::Key(kind))?;
    // The rights of the owner cover each grant that the form adds or changes.
    for grant in &grants {
        let unchanged = ctx.old_grants.is_some_and(|old| {
            old.iter()
                .any(|o| o.same_node(grant) && only_narrows(&o.limits, &grant.limits))
        });
        if !unchanged && !covered(ctx.owner_grants, grant) {
            return Err(NOT_COVERED);
        }
    }
    grants.sort();
    Ok(KeyForm {
        name: raw.name.clone(),
        expires_at,
        grants,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::parse;

    fn document() -> Arc<Document> {
        Arc::new(
            parse(
                "datastore",
                br#"{"server":"datastore",
                  "resource_types":[{"name":"host","parent":"service","description":"h"},
                                    {"name":"database","parent":"host","description":"d"}],
                  "actions":[{"name":"data.read","resource_types":["host","database"],"description":"r",
                              "limits":["requests","max_rows"]},
                             {"name":"data.admin","resource_types":["service"],"description":"a"}],
                  "limits":[{"name":"requests","label":"R","description":"d","unit":"request","kind":"budget"},
                            {"name":"max_rows","label":"M","description":"d","unit":"row","kind":"ceiling","min":1,"max":100000}]}"#,
            )
            .unwrap(),
        )
    }

    fn grants(texts: &[&str]) -> Vec<Grant> {
        texts.iter().map(|t| Grant::parse(t).unwrap()).collect()
    }

    fn fields(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    const NOW: i64 = 1_800_000_000;

    fn run(
        pairs: &[(&str, &str)],
        owner: Option<&[Grant]>,
        max_lifetime: Option<u64>,
        old: Option<&[Grant]>,
    ) -> Result<KeyForm, &'static str> {
        let raw = read(fields(pairs), old.is_some()).expect("good fields");
        let document = document();
        let lookup = move |service: &str| (service == "datastore").then(|| Arc::clone(&document));
        let exists = |service: &str| service == "datastore";
        check(
            &raw,
            &Context {
                now: NOW,
                owner_grants: owner,
                max_lifetime,
                document: &lookup,
                service_exists: &exists,
                is_external: &|_| false,
                old_grants: old,
            },
            KeyKind::Mcp,
        )
    }

    /// The same check for an API key, with the document of the service `portal` and one
    /// service `datastore`.
    fn run_api(pairs: &[(&str, &str)], owner: Option<&[Grant]>) -> Result<KeyForm, &'static str> {
        let raw = read(fields(pairs), false).expect("good fields");
        let portal = crate::portal_actions::document(&crate::portal_actions::Features {
            inbox_groups: Some(vec![("infra".to_string(), "Infra".to_string())]),
            kinds: vec![KeyKind::Api],
        });
        let datastore = document();
        let lookup = move |service: &str| match service {
            "portal" => Some(Arc::clone(&portal)),
            "datastore" => Some(Arc::clone(&datastore)),
            _ => None,
        };
        let exists = |service: &str| service == "datastore";
        check(
            &raw,
            &Context {
                now: NOW,
                owner_grants: owner,
                max_lifetime: None,
                document: &lookup,
                service_exists: &exists,
                is_external: &|_| false,
                old_grants: None,
            },
            KeyKind::Api,
        )
    }

    #[test]
    fn an_api_key_holds_the_alert_actions_and_nothing_of_the_keys_or_of_other_services() {
        let owner = grants(&[
            "alert.send on service:portal/inbox_group:infra",
            "alert.read on service:portal/inbox_group:infra",
            "keys.own.read on service:portal",
            "inbox.receive on service:portal/inbox_group:infra",
            "access on service:datastore",
        ]);
        let ok = |grant: &str| run_api(&[("name", "n"), ("grant0", grant)], Some(&owner));
        assert!(ok("alert.send on service:portal/inbox_group:infra").is_ok());
        assert!(ok("alert.read on service:portal/inbox_group:infra").is_ok());
        // A budget is on the grant `access` of the service portal.
        assert!(run_api(
            &[
                ("name", "n"),
                ("grant0", "access on service:portal"),
                ("grant1", "alert.send on service:portal/inbox_group:infra"),
                ("limit0.alerts.minute", "5"),
            ],
            None
        )
        .is_ok());
        for refused in [
            "keys.own.read on service:portal",
            "inbox.receive on service:portal/inbox_group:infra",
            "phone.read on service:portal",
            "access on service:datastore",
            "alert.send on service:portal/inbox_group:missing",
            "alert.send on service:portal",
        ] {
            assert_eq!(ok(refused), Err(BAD_GRANT), "{refused}");
        }
        // The owner must cover the grant (portal 4.2).
        let weak = grants(&["alert.read on service:portal/inbox_group:infra"]);
        assert_eq!(
            run_api(
                &[
                    ("name", "n"),
                    ("grant0", "alert.send on service:portal/inbox_group:infra")
                ],
                Some(&weak)
            ),
            Err(NOT_COVERED)
        );
    }

    #[test]
    fn an_mcp_key_holds_no_action_of_the_service_portal() {
        let owner = grants(&["alert.send on service:portal/inbox_group:infra"]);
        assert_eq!(
            run(
                &[
                    ("name", "n"),
                    ("grant0", "alert.send on service:portal/inbox_group:infra")
                ],
                Some(&owner),
                None,
                None
            ),
            Err(BAD_GRANT)
        );
    }

    fn owner() -> Vec<Grant> {
        grants(&[
            "access on service:datastore",
            "data.read on service:datastore/host:prod",
        ])
    }

    #[test]
    fn the_fields_have_their_forms_and_any_other_field_is_a_bad_request() {
        let form = read(
            fields(&[
                ("name", "report"),
                ("expires_at", "2026-11-23T00:00:00Z"),
                ("grant0", "access on service:datastore"),
                ("grant7", "data.read on service:datastore/host:prod"),
                ("limit0.requests.minute", "60"),
                ("limit7.max_rows", ""),
            ]),
            false,
        )
        .unwrap();
        assert_eq!(form.name, "report");
        assert_eq!(form.expires_at.as_deref(), Some("2026-11-23T00:00:00Z"));
        assert_eq!(form.grants.keys().copied().collect::<Vec<_>>(), [0, 7]);
        assert_eq!(form.limits[&0][0].window, Some(Window::Minute));
        assert_eq!(form.limits[&7][0].window, None);
        for bad in [
            "grant",
            "grant01",
            "grant-1",
            "grant1.5",
            "grant100000",
            "limit0",
            "limit0.",
            "limit.requests",
            "limit0.Requests",
            "limit0.requests.week",
            "limit0.requests.minute.x",
            "other",
            "Name",
            "csrf2",
        ] {
            assert!(read(fields(&[(bad, "x")]), false).is_err(), "{bad}");
        }
        // The form of a change has no `expires_at`.
        assert!(read(fields(&[("expires_at", "")]), true).is_err());
        assert!(read(
            fields(&[("grant0", &"x".repeat(MAX_GRANT_TEXT + 1))]),
            false
        )
        .is_err());
    }

    #[test]
    fn a_good_form_gives_a_key_with_its_limit_values_and_sorted_grants() {
        let owner = owner();
        let form = run(
            &[
                ("name", "nightly report"),
                ("expires_at", "2027-03-01T00:00:00Z"),
                (
                    "grant0",
                    "data.read on service:datastore/host:prod/database:test1",
                ),
                ("grant1", "access on service:datastore"),
                ("grant2", ""),
                ("limit0.max_rows", "100"),
                ("limit1.requests.minute", "60"),
                ("limit1.requests.second", "5"),
                ("limit1.requests.hour", ""),
            ],
            Some(&owner),
            None,
            None,
        )
        .unwrap();
        assert_eq!(form.name, "nightly report");
        assert_eq!(
            form.expires_at,
            crate::util::parse_utc_instant("2027-03-01T00:00:00Z")
        );
        assert_eq!(form.grants.len(), 2);
        let read = form
            .grants
            .iter()
            .find(|g| g.action == "data.read")
            .unwrap();
        assert_eq!(read.limits.to_json(), r#"{"max_rows":100}"#);
        let access = form.grants.iter().find(|g| g.action == "access").unwrap();
        assert_eq!(
            access.limits.to_json(),
            r#"{"requests":[{"amount":5,"window":"second"},{"amount":60,"window":"minute"}]}"#
        );
        // The superadmin has every right; a key with no grant is good.
        assert!(run(&[("name", "n")], None, None, None).is_ok());
    }

    #[test]
    fn the_name_the_expiry_and_the_maximum_lifetime_follow_their_rules() {
        let owner = owner();
        let ok = |pairs: &[(&str, &str)], max| run(pairs, Some(&owner), max, None);
        assert_eq!(ok(&[("name", "")], None), Err(BAD_NAME));
        assert_eq!(ok(&[("name", "a\nb")], None), Err(BAD_NAME));
        assert_eq!(ok(&[("name", &"x".repeat(65))], None), Err(BAD_NAME));
        assert!(ok(&[("name", &"x".repeat(64))], None).is_ok());
        // The time is in the past, or it has a bad form.
        for bad in [
            "2020-01-01T00:00:00Z",
            "2026-11-23",
            "tomorrow",
            "2026-11-23T00:00:00+01:00",
        ] {
            assert_eq!(
                ok(&[("name", "n"), ("expires_at", bad)], None),
                Err(BAD_EXPIRY),
                "{bad}"
            );
        }
        // No maximum: no expiry date is good.
        assert_eq!(
            ok(&[("name", "n"), ("expires_at", "")], None)
                .unwrap()
                .expires_at,
            None
        );
        // A maximum of 30 days: a date is needed, and it is at most 30 days away.
        let in_days = |days: i64| crate::util::format_utc_instant(NOW + days * 86_400);
        let month = Some(30 * 86_400);
        assert_eq!(
            ok(&[("name", "n"), ("expires_at", "")], month),
            Err(BAD_EXPIRY)
        );
        assert_eq!(ok(&[("name", "n")], month), Err(BAD_EXPIRY));
        assert!(ok(&[("name", "n"), ("expires_at", &in_days(30))], month).is_ok());
        assert_eq!(
            ok(&[("name", "n"), ("expires_at", &in_days(31))], month),
            Err(BAD_EXPIRY)
        );
        // The value 0 refuses each new key (portal 5.3a).
        assert_eq!(
            ok(&[("name", "n"), ("expires_at", &in_days(1))], Some(0)),
            Err(BAD_EXPIRY)
        );
    }

    #[test]
    fn a_grant_needs_a_good_text_a_known_action_and_the_tree_of_its_service() {
        let owner = owner();
        let bad = |pairs: &[(&str, &str)]| run(pairs, Some(&owner), None, None);
        for grant in [
            "nonsense",
            "data.read on service:datastore/host:prod/",
            // An action that the document does not publish.
            "data.write on service:datastore/host:prod",
            // A node that does not follow the tree: the database needs a host.
            "data.read on service:datastore/database:test1",
            // An action that does not list the type `service`.
            "data.read on service:datastore",
            // A service that is not in the catalogue.
            "access on service:ghost",
            // An action of the service portal (portal 6.5).
            "keys.own.read on service:portal",
        ] {
            assert_eq!(
                bad(&[("name", "n"), ("grant0", grant)]),
                Err(BAD_GRANT),
                "{grant}"
            );
        }
        // The same grant two times, and too many grants.
        assert_eq!(
            bad(&[
                ("name", "n"),
                ("grant0", "access on service:datastore"),
                ("grant1", "access on service:datastore"),
            ]),
            Err(BAD_GRANT)
        );
        let many: Vec<(String, String)> = (0..513)
            .map(|i| {
                (
                    format!("grant{i}"),
                    format!("access on service:datastore{i}"),
                )
            })
            .collect();
        let mut pairs: Vec<(&str, &str)> = vec![("name", "n")];
        pairs.extend(many.iter().map(|(k, v)| (k.as_str(), v.as_str())));
        assert_eq!(run(&pairs, None, None, None), Err(BAD_GRANT));
        // An action that lists the type `service` is valid on the service.
        assert!(run(
            &[
                ("name", "n"),
                ("grant0", "data.admin on service:datastore"),
                ("grant1", "access on service:datastore")
            ],
            None,
            None,
            None
        )
        .is_ok());
    }

    /// A form with many grant fields is refused at the first grant over the cap (portal 4.1b).
    /// The work does not grow with the square of the number of fields.
    #[test]
    fn a_form_with_too_many_grants_is_refused_without_a_quadratic_scan() {
        let text = |i: usize| format!("a.b on service:{}/h:{i:05}", "a".repeat(32));
        let mut many: HashMap<String, String> = (0..20_000)
            .map(|i| (format!("grant{i}"), text(i)))
            .collect();
        many.insert("name".into(), "x".into());
        let raw = read(many, false).unwrap();
        let started = std::time::Instant::now();
        assert_eq!(read_grants(&raw), Err(BAD_GRANT));
        assert!(
            started.elapsed() < std::time::Duration::from_secs(2),
            "{:?}",
            started.elapsed()
        );
        // The cap itself, with blank rows that do not count.
        let mut raw = RawForm {
            grants: (0..MAX_GRANTS).map(|i| (i, text(i))).collect(),
            ..RawForm::default()
        };
        raw.grants
            .extend((MAX_GRANTS..MAX_GRANTS + 50).map(|i| (i, String::new())));
        assert_eq!(read_grants(&raw).unwrap().len(), MAX_GRANTS);
        raw.grants.insert(MAX_GRANTS + 60, text(MAX_GRANTS));
        assert_eq!(read_grants(&raw), Err(BAD_GRANT));
        // A repeated node is still refused.
        raw.grants = [(0, text(1)), (1, text(1))].into_iter().collect();
        assert_eq!(read_grants(&raw), Err(BAD_GRANT));
    }

    #[test]
    fn a_limit_value_follows_the_declaration_and_the_list_of_the_action() {
        let owner = owner();
        let limit = |grant: &str, field: &str, value: &str| {
            let mut pairs = vec![("name", "n"), ("grant0", grant), (field, value)];
            // A right needs the grant `access` on its service.
            if grant != "access on service:datastore" {
                pairs.push(("grant9", "access on service:datastore"));
            }
            run(&pairs, Some(&owner), None, None)
        };
        let read = "data.read on service:datastore/host:prod";
        let access = "access on service:datastore";
        assert!(limit(read, "limit0.max_rows", "1").is_ok());
        assert!(limit(read, "limit0.max_rows", "100000").is_ok());
        assert_eq!(
            limit(read, "limit0.max_rows", "0"),
            Err(BAD_LIMITS),
            "below the minimum"
        );
        assert_eq!(
            limit(read, "limit0.max_rows", "100001"),
            Err(BAD_LIMITS),
            "above the maximum"
        );
        assert_eq!(limit(read, "limit0.max_rows", "1.5"), Err(BAD_LIMITS));
        assert_eq!(limit(read, "limit0.max_rows", "-1"), Err(BAD_LIMITS));
        assert_eq!(
            limit(read, "limit0.max_rows", "1000000001"),
            Err(BAD_LIMITS)
        );
        assert_eq!(
            limit(read, "limit0.unknown", "1"),
            Err(BAD_LIMITS),
            "not declared"
        );
        // A budget is only on `access`, and a ceiling only where the action lists it.
        assert_eq!(limit(read, "limit0.requests.minute", "5"), Err(BAD_LIMITS));
        assert_eq!(limit(access, "limit0.max_rows", "5"), Err(BAD_LIMITS));
        assert!(limit(access, "limit0.requests.minute", "5").is_ok());
        assert_eq!(
            limit(access, "limit0.requests.minute", "0"),
            Err(BAD_LIMITS),
            "an amount from 1"
        );
        // A ceiling field and a budget field with one name.
        assert_eq!(
            run(
                &[
                    ("name", "n"),
                    ("grant0", access),
                    ("limit0.requests", "5"),
                    ("limit0.requests.minute", "5")
                ],
                Some(&owner),
                None,
                None
            ),
            Err(BAD_LIMITS)
        );
        // A value without a grant.
        assert_eq!(
            run(
                &[("name", "n"), ("limit3.max_rows", "5")],
                Some(&owner),
                None,
                None
            ),
            Err(BAD_LIMITS)
        );
        assert!(run(
            &[("name", "n"), ("limit3.max_rows", "")],
            Some(&owner),
            None,
            None
        )
        .is_ok());
    }

    #[test]
    fn the_rights_of_the_owner_cover_each_grant_that_the_form_adds_or_changes() {
        let owner = owner();
        let key = |pairs: &[(&str, &str)], old: Option<&[Grant]>| {
            let mut all = vec![("name", "n")];
            all.extend_from_slice(pairs);
            // A right needs the grant `access` on its service.
            if !pairs
                .iter()
                .any(|(_, v)| *v == "access on service:datastore")
            {
                all.push(("grant9", "access on service:datastore"));
            }
            run(&all, Some(&owner), None, old)
        };
        // The owner holds `host:prod`: the key can hold it, or a node below it.
        assert!(key(
            &[("grant0", "data.read on service:datastore/host:prod")],
            None
        )
        .is_ok());
        assert!(key(
            &[(
                "grant0",
                "data.read on service:datastore/host:prod/database:a"
            )],
            None
        )
        .is_ok());
        // A node above, a node beside and an action that the owner lacks.
        assert_eq!(
            key(
                &[("grant0", "data.read on service:datastore/host:dev")],
                None
            ),
            Err(NOT_COVERED)
        );
        assert_eq!(
            key(&[("grant0", "data.admin on service:datastore")], None),
            Err(NOT_COVERED)
        );
        // The owner lost a right: the key keeps an unchanged grant, and it cannot get a new one.
        let before = grants(&[
            "data.read on service:datastore/host:old",
            "access on service:datastore",
        ]);
        assert!(key(
            &[
                ("grant0", "data.read on service:datastore/host:old"),
                ("grant1", "access on service:datastore"),
            ],
            Some(&before)
        )
        .is_ok());
        assert_eq!(
            key(
                &[("grant0", "data.read on service:datastore/host:other")],
                Some(&before)
            ),
            Err(NOT_COVERED)
        );
        // Lower or add a value on a kept grant: no check (portal 4.2c). Remove it or raise it: the check.
        let with_value = vec![Grant::parse("data.read on service:datastore/host:old")
            .unwrap()
            .with_limits(Limits::from_json(r#"{"max_rows":500}"#).unwrap())];
        let kept = |value: &str| {
            key(
                &[
                    ("grant0", "data.read on service:datastore/host:old"),
                    ("limit0.max_rows", value),
                ],
                Some(&with_value),
            )
        };
        assert!(kept("500").is_ok());
        assert!(kept("100").is_ok(), "a lower value");
        assert_eq!(kept("501"), Err(NOT_COVERED), "a higher value");
        assert_eq!(kept(""), Err(NOT_COVERED), "the removal of the value");
        // A key of the superadmin: everything is covered.
        assert!(run(
            &[
                ("name", "n"),
                ("grant0", "data.read on service:datastore/host:x"),
                ("grant1", "access on service:datastore")
            ],
            None,
            None,
            None
        )
        .is_ok());
    }

    #[test]
    fn a_right_on_a_service_needs_the_grant_access_on_the_same_service() {
        let with = |texts: &[&str]| service_without_access(&grants(texts)).map(str::to_string);
        // A right on the whole service, on a resource and on a sub-resource has the same rule.
        for right in [
            "data.admin on service:datastore",
            "data.read on service:datastore/host:prod",
            "data.read on service:datastore/host:prod/database:db1",
        ] {
            assert_eq!(with(&[right]), Some("datastore".to_string()), "{right}");
            assert_eq!(
                with(&[right, "access on service:datastore"]),
                None,
                "{right}"
            );
            assert_eq!(
                with(&[right, "access on service:other"]),
                Some("datastore".to_string()),
                "the grant access of another service does not help: {right}"
            );
        }
        // `access` alone is allowed: the person can want access and nothing else.
        assert_eq!(with(&["access on service:datastore"]), None);
        assert_eq!(with(&[]), None);
        // The service portal has no gate: its rights work as they are.
        assert_eq!(with(&["keys.own.read on service:portal"]), None);
        // The first service of the list is named.
        assert_eq!(
            with(&[
                "access on service:b",
                "x.y on service:a",
                "z.w on service:b"
            ]),
            Some("a".to_string())
        );
    }

    #[test]
    fn the_refused_grants_and_the_service_come_back_for_the_page() {
        let raw = read(
            fields(&[
                ("name", "n"),
                ("grant0", "data.read on service:datastore/host:prod"),
            ]),
            false,
        )
        .unwrap();
        let document = document();
        let lookup = move |service: &str| (service == "datastore").then(|| Arc::clone(&document));
        let exists = |service: &str| service == "datastore";
        let ctx = Context {
            now: NOW,
            owner_grants: None,
            max_lifetime: None,
            document: &lookup,
            service_exists: &exists,
            is_external: &|_| false,
            old_grants: None,
        };
        let (grants, service) = grants_without_access(&raw, &ctx, Holder::Account).unwrap();
        assert_eq!(service, "datastore");
        assert_eq!(grants.len(), 1);
        // A form that has the access is no case, and a form with another fault is none.
        let ok = read(
            fields(&[
                ("grant0", "data.read on service:datastore/host:prod"),
                ("grant1", "access on service:datastore"),
            ]),
            false,
        )
        .unwrap();
        assert!(grants_without_access(&ok, &ctx, Holder::Account).is_none());
        let bad = read(fields(&[("grant0", "x.y on service:ghost")]), false).unwrap();
        assert!(grants_without_access(&bad, &ctx, Holder::Account).is_none());
    }

    #[test]
    fn a_form_with_a_right_and_no_access_is_refused_for_a_key_and_for_an_account() {
        let owner = grants(&[
            "access on service:datastore",
            "data.read on service:datastore",
        ]);
        let right = ("grant0", "data.read on service:datastore/host:prod");
        assert_eq!(
            run(&[("name", "n"), right], Some(&owner), None, None),
            Err(NEEDS_ACCESS)
        );
        let ok = run(
            &[
                ("name", "n"),
                right,
                ("grant1", "access on service:datastore"),
            ],
            Some(&owner),
            None,
            None,
        )
        .unwrap();
        assert_eq!(ok.grants.len(), 2);
        // The change of a key that holds a dead right is refused too: the form must add `access`.
        let old = grants(&["data.read on service:datastore/host:prod"]);
        assert_eq!(
            run(&[("name", "n"), right], Some(&owner), None, Some(&old)),
            Err(NEEDS_ACCESS)
        );
        // An account has the same rule.
        let raw = read(fields(&[right]), true).unwrap();
        let document = document();
        let lookup = move |service: &str| (service == "datastore").then(|| Arc::clone(&document));
        let exists = |service: &str| service == "datastore";
        let ctx = Context {
            now: NOW,
            owner_grants: None,
            max_lifetime: None,
            document: &lookup,
            service_exists: &exists,
            is_external: &|_| false,
            old_grants: None,
        };
        assert_eq!(check_grants(&raw, &ctx, Holder::Account), Err(NEEDS_ACCESS));
        let raw = read(
            fields(&[right, ("grant1", "access on service:datastore")]),
            true,
        )
        .unwrap();
        assert_eq!(check_grants(&raw, &ctx, Holder::Account).unwrap().len(), 2);
    }

    /// An external service is a link. An account holds `access` on its whole service and nothing else. A key holds
    /// no right on it.
    #[test]
    fn a_grant_on_an_external_service_is_only_access_on_the_whole_service_and_only_for_an_account()
    {
        let document = document();
        let lookup = move |service: &str| (service == "datastore").then(|| Arc::clone(&document));
        let exists = |service: &str| matches!(service, "datastore" | "ext-wiki");
        let external = |service: &str| service == "ext-wiki";
        let ctx = Context {
            now: NOW,
            owner_grants: None,
            max_lifetime: None,
            document: &lookup,
            service_exists: &exists,
            is_external: &external,
            old_grants: None,
        };
        let account = |texts: &[(&str, &str)]| {
            let raw = read(fields(texts), true).unwrap();
            check_grants(&raw, &ctx, Holder::Account).map(|g| g.len())
        };
        assert_eq!(account(&[("grant0", "access on service:ext-wiki")]), Ok(1));
        assert_eq!(
            account(&[
                ("grant0", "access on service:ext-wiki"),
                ("grant1", "access on service:datastore"),
                ("grant2", "data.read on service:datastore/host:prod"),
            ]),
            Ok(3),
            "the other services are as before"
        );
        for refused in [
            "data.read on service:ext-wiki",
            "data.admin on service:ext-wiki",
            "access on service:ext-wiki/host:prod",
            "data.read on service:ext-wiki/host:prod",
            "x.y on service:ext-wiki",
        ] {
            assert_eq!(
                account(&[
                    ("grant0", "access on service:ext-wiki"),
                    ("grant1", refused)
                ]),
                Err(BAD_GRANT),
                "{refused}"
            );
        }
        // A limit value on `access` has no document to follow.
        let raw = read(
            fields(&[
                ("grant0", "access on service:ext-wiki"),
                ("limit0.requests.minute", "10"),
            ]),
            true,
        )
        .unwrap();
        assert_eq!(check_grants(&raw, &ctx, Holder::Account), Err(BAD_GRANT));
        // A grant that the account holds and that the form does not change is no way round the rule.
        let old = grants(&["data.read on service:ext-wiki"]);
        let kept = Context {
            old_grants: Some(&old),
            ..ctx
        };
        let raw = read(
            fields(&[
                ("grant0", "access on service:ext-wiki"),
                ("grant1", "data.read on service:ext-wiki"),
            ]),
            true,
        )
        .unwrap();
        assert_eq!(check_grants(&raw, &kept, Holder::Account), Err(BAD_GRANT));
        // A key never holds a right on it, `access` included.
        for kind in [KeyKind::Mcp, KeyKind::Api] {
            let raw = read(fields(&[("grant0", "access on service:ext-wiki")]), true).unwrap();
            assert_eq!(
                check_grants(&raw, &ctx, Holder::Key(kind)),
                Err(BAD_GRANT),
                "{kind:?}"
            );
        }
    }
}
