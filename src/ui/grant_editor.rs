//! The editor of grants: one component for the forms of the keys.
//!
//! A service can have hundreds of actions and hundreds of resources. A box for each pair of an
//! action and a node does not fit in a page. The editor shows the tree of the resources on the
//! left and the actions of one node on the right, with a filter, and the ticked grants in a table
//! of one line for each grant. The script (`settings.js`) draws it from a JSON block in the page.
//!
//! The page keeps the ticked grants as hidden fields with the names that the handlers read. A
//! browser without script posts these fields as they are, so a change that does not touch the
//! grants keeps them. A text field for each blank grant stands in a `<noscript>` element.

use std::fmt::Write as _;

use super::settings::ServiceActions;
use crate::actions::Source;
use crate::grants::{Grant, ACCESS, SERVICE_TYPE};
use crate::limits::{Input, Limits, Value};
use crate::util::html_escape;

/// One action that the editor offers.
#[derive(Clone, Debug)]
pub struct EditorAction {
    pub name: String,
    pub description: String,
    /// The action applies to the whole service: its document lists the type `service`.
    pub whole: bool,
    /// The types of the nodes where the action applies, below the service.
    pub types: Vec<String>,
    pub limits: Vec<Input>,
    /// The actions that this action needs (portal 6.3): it counts only when the account holds
    /// them. Only the actions of the service `portal` have any.
    pub needs: Vec<String>,
}

/// One node of the tree of a service.
#[derive(Clone, Debug)]
pub struct EditorNode {
    /// `host:prod/database:db1`. Empty: the whole service.
    pub path: String,
    pub kind: String,
    pub label: String,
    /// The actions that the document names for this node. Empty: each action of its type.
    pub only: Vec<String>,
    /// Actions that apply here although their type does not list this node.
    pub more: Vec<String>,
    /// The node is a critical group of the inbox: a limit never counts its alerts.
    pub critical: bool,
}

/// The actions and the tree of one service.
#[derive(Clone, Debug)]
pub struct EditorService {
    pub id: String,
    pub name: String,
    pub actions: Vec<EditorAction>,
    pub nodes: Vec<EditorNode>,
    /// The types of the document with their parents and descriptions: the types of the children of
    /// a node.
    pub types: Vec<(String, String, String)>,
    /// The portal can ask the service for the children of a node (portal 3.1a).
    pub fetch: bool,
    /// Where the document comes from and how old the copy is. The service `portal` is always
    /// `Absent`: its document is in the code, and the script says so.
    pub source: Source,
}

/// What the form needs to draw the editor. It has two parts: the services, which are the workloads
/// of the launcher, and the portal features, which are the one internal scope of the portal.
#[derive(Clone, Debug)]
pub struct EditorData {
    /// The workloads of the launcher, with the actions that their documents publish.
    pub services: Vec<EditorService>,
    /// The features of the portal (the service `portal`): the keys and the inbox. `None`: the file
    /// turns none on, or the holder of the form cannot give any.
    pub features: Option<EditorService>,
    /// The grants that the form has at the start, with their limit values.
    pub ticked: Vec<Grant>,
    /// The grants of the owner. `None`: the superadmin, who has every right.
    pub owner: Option<Vec<Grant>>,
    /// Grants that the form shows and does not change: they come from the configuration file.
    pub locked: Vec<Grant>,
}

/// The icons that the script draws. The page inlines the symbols that its text uses.
const ICONS: &str = r##"<svg class="icon" hidden aria-hidden="true"><use href="#i-chevron-down"/><use href="#i-chevron-right"/><use href="#i-x"/><use href="#i-check"/><use href="#i-alert-triangle"/><use href="#i-lock"/><use href="#i-key"/><use href="#i-inbox"/><use href="#i-device-mobile"/><use href="#i-webhook"/><use href="#i-robot"/><use href="#i-bell-ringing"/><use href="#i-circle-check"/><use href="#i-clock"/><use href="#i-ban"/><use href="#i-tags"/><use href="#i-user"/><use href="#i-shield-check"/></svg>"##;

/// How the editor sits in its page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Place {
    /// The id of the form that the fields belong to, when the editor is not inside it.
    pub form: Option<&'static str>,
    /// The title of the table of the ticked grants.
    pub title: &'static str,
    /// The number of text fields for a grant in a browser without script. The forms of the keys
    /// have them.
    pub blank_rows: usize,
    /// Draw the portal features as a group beside the services, always open (the page of an
    /// account). Without it, the features are one more row of the list (the forms of the keys).
    pub groups: bool,
}

/// A limit value for the script: `{"c":100}` for a ceiling, `{"b":{"minute":60}}` for a budget.
fn value_json(value: &Value) -> serde_json::Value {
    match value {
        Value::Ceiling(n) => serde_json::json!({ "c": n }),
        Value::Budget(budget) => {
            let mut windows = serde_json::Map::new();
            for item in budget.items() {
                windows.insert(item.window.name().to_string(), item.amount.into());
            }
            serde_json::json!({ "b": windows })
        }
    }
}

fn limits_json(limits: &Limits) -> serde_json::Value {
    let mut out = serde_json::Map::new();
    for (name, value) in limits.iter() {
        out.insert(name.to_string(), value_json(value));
    }
    out.into()
}

fn grant_json(grant: &Grant) -> serde_json::Value {
    serde_json::json!([
        grant.action,
        grant.service,
        crate::grants::path_text(&grant.path),
        limits_json(&grant.limits)
    ])
}

fn input_json(input: &Input) -> serde_json::Value {
    let mut title = input.description.clone();
    if !input.unit.is_empty() {
        let _ = write!(title, " Unit: {}.", input.unit);
    }
    serde_json::json!({
        "name": input.name,
        "label": input.label,
        "title": title,
        "budget": input.budget,
        "range": input.range.map(|(min, max)| [min, max]),
    })
}

/// The JSON of one service for the script.
fn service_json(service: &EditorService) -> serde_json::Value {
    let actions: Vec<serde_json::Value> = service
        .actions
        .iter()
        .map(|a| {
            serde_json::json!({
                "name": a.name,
                "description": a.description,
                "whole": a.whole,
                "types": a.types,
                "limits": a.limits.iter().map(input_json).collect::<Vec<_>>(),
                "needs": a.needs,
            })
        })
        .collect();
    let nodes: Vec<serde_json::Value> = service
        .nodes
        .iter()
        .map(|n| {
            serde_json::json!({
                "path": n.path,
                "kind": n.kind,
                "label": n.label,
                "only": n.only,
                "more": n.more,
                "critical": n.critical,
            })
        })
        .collect();
    serde_json::json!({
        "id": service.id,
        "name": service.name,
        "actions": actions,
        "nodes": nodes,
        "types": service
            .types
            .iter()
            .map(|(name, parent, description)| {
                serde_json::json!({ "name": name, "parent": parent, "description": description })
            })
            .collect::<Vec<_>>(),
        "fetch": service.fetch,
        "source": source_json(service.source),
    })
}

/// The source of a document for the script: its state, and the age of the copy in seconds.
fn source_json(source: Source) -> serde_json::Value {
    match source {
        Source::Absent => serde_json::json!({ "state": "absent" }),
        Source::Fresh(age) => serde_json::json!({ "state": "fresh", "secs": age.as_secs() }),
        Source::Stale(age) => serde_json::json!({ "state": "stale", "secs": age.as_secs() }),
    }
}

/// The JSON block of the editor. `<`, `>` and `&` are written as `\u00XX`, so the text cannot
/// close the element.
fn json_block(data: &EditorData) -> String {
    let value = serde_json::json!({
        "services": data.services.iter().map(service_json).collect::<Vec<_>>(),
        "features": data.features.as_ref().map(service_json),
        "ticked": data.ticked.iter().map(grant_json).collect::<Vec<_>>(),
        "owner": data.owner.as_ref().map(|grants| {
            grants
                .iter()
                .map(grant_json)
                .collect::<Vec<_>>()
        }),
        "locked": data.locked.iter().map(grant_json).collect::<Vec<_>>(),
    });
    value
        .to_string()
        .replace('<', "\\u003c")
        .replace('>', "\\u003e")
        .replace('&', "\\u0026")
}

/// The hidden fields of the ticked grants, as the script writes them again after each change:
/// `grant<n>` and `limit<n>.<name>[.<window>]`, the fields that the handlers read.
fn hidden_fields(out: &mut String, data: &EditorData, form: Option<&str>) {
    let form = form
        .map(|id| format!(r#" form="{id}""#))
        .unwrap_or_default();
    for (n, grant) in data.ticked.iter().enumerate() {
        let _ = write!(
            out,
            r#"<input type="hidden" name="grant{n}" value="{}"{form}>"#,
            html_escape(&grant.clone().with_limits(Limits::new()).to_string())
        );
        for (name, value) in grant.limits.iter() {
            match value {
                Value::Ceiling(amount) => {
                    let _ = write!(
                        out,
                        r#"<input type="hidden" name="limit{n}.{}" value="{amount}"{form}>"#,
                        html_escape(name)
                    );
                }
                Value::Budget(budget) => {
                    for item in budget.items() {
                        let _ = write!(
                            out,
                            r#"<input type="hidden" name="limit{n}.{}.{}" value="{}"{form}>"#,
                            html_escape(name),
                            item.window.name(),
                            item.amount
                        );
                    }
                }
            }
        }
    }
}

/// The editor: the data, the hidden fields and, in the forms of the keys, the text fields for a
/// browser without script.
pub fn render(out: &mut String, data: &EditorData, place: Place) {
    let _ = write!(
        out,
        r#"<div class="grantEditor" data-title="{}"{}{}><script type="application/json" class="grantEditor__data">{}</script><div class="grantEditor__fields">"#,
        html_escape(place.title),
        if place.groups {
            r#" data-layout="groups""#
        } else {
            ""
        },
        place
            .form
            .map(|id| format!(r#" data-form="{id}""#))
            .unwrap_or_default(),
        json_block(data)
    );
    hidden_fields(out, data, place.form);
    out.push_str(ICONS);
    // The script says here what it did that the person did not tick: a service that opens by
    // itself, rights that go with the switch. A screen reader reads it.
    out.push_str(r#"</div><span class="visually-hidden" role="status" data-ge-live></span>"#);
    if place.groups {
        // Without script the page cannot draw the editor. It lists the grants as text.
        out.push_str(r#"<noscript><ul class="grantList">"#);
        for grant in data.ticked.iter().chain(data.locked.iter()) {
            let _ = write!(
                out,
                "<li><code>{}</code></li>",
                html_escape(&grant.to_string())
            );
        }
        out.push_str("</ul></noscript>");
    }
    if place.blank_rows > 0 {
        out.push_str("<noscript><div class=\"grantRows\">");
        for k in 0..place.blank_rows {
            let n = data.ticked.len() + k;
            let form = place
                .form
                .map(|id| format!(r#" form="{id}""#))
                .unwrap_or_default();
            let _ = write!(
                out,
                r#"<div class="grantRow"><label>Another grant <input class="uiInput" type="text" name="grant{n}" value="" size="48" maxlength="4096" placeholder="data.read on service:datastore/host:prod/database:test1"{form}></label></div>"#
            );
        }
        out.push_str("</div></noscript>");
    }
    out.push_str("</div>");
}

// The data of an editor ------------------------------------------------------------------------

/// The text of the grant `access`.
const ACCESS_DESCRIPTION: &str = "Open the service. The gate lets the holder in.";

/// The actions and the tree of one service. The service `portal` is the scope of the portal
/// features: its action `access` has no use but to carry a budget, so the editor offers it only when
/// the document declares one.
fn editor_service(service: &ServiceActions) -> EditorService {
    let portal = service.id == crate::portal_actions::SERVICE;
    let access_limits = service.document.limit_inputs(ACCESS);
    let mut actions = Vec::new();
    if !portal || !access_limits.is_empty() {
        actions.push(EditorAction {
            name: ACCESS.to_string(),
            description: if portal {
                ACCESS_BUDGET_DESCRIPTION.to_string()
            } else {
                ACCESS_DESCRIPTION.to_string()
            },
            whole: true,
            types: Vec::new(),
            limits: access_limits,
            needs: Vec::new(),
        });
    }
    let mut nodes = vec![EditorNode {
        path: String::new(),
        kind: SERVICE_TYPE.to_string(),
        label: if portal {
            "Server permissions".to_string()
        } else {
            "Whole service".to_string()
        },
        only: Vec::new(),
        more: Vec::new(),
        critical: false,
    }];
    for (name, description) in &service.actions {
        let found = service.document.actions.iter().find(|a| a.name == *name);
        let types = found
            .map(|a| a.sub_resource_types().map(str::to_string).collect())
            .unwrap_or_default();
        let whole = found.is_some_and(|a| a.resource_types.iter().any(|t| t == SERVICE_TYPE));
        actions.push(EditorAction {
            name: name.clone(),
            description: description.clone(),
            whole,
            types,
            limits: service.document.limit_inputs(name),
            needs: if portal {
                crate::portal_actions::needs(name)
                    .iter()
                    .map(|n| n.to_string())
                    .collect()
            } else {
                Vec::new()
            },
        });
    }
    for node in &service.document.resources {
        nodes.push(EditorNode {
            path: crate::grants::path_text(&node.path),
            kind: node.path.last().map(|s| s.kind.clone()).unwrap_or_default(),
            label: node.label.clone(),
            only: node.actions.clone(),
            more: Vec::new(),
            critical: portal
                && node.path.len() == 1
                && node.path[0].kind == crate::portal_actions::INBOX_GROUP
                && service.critical.iter().any(|id| *id == node.path[0].id),
        });
    }
    EditorService {
        id: service.id.clone(),
        name: service.name.clone(),
        actions,
        nodes,
        types: service
            .document
            .types
            .iter()
            .map(|t| (t.name.clone(), t.parent.clone(), t.description.clone()))
            .collect(),
        fetch: service.fetch,
        source: service.source,
    }
}

/// The text of the grant `access` on the service `portal`.
const ACCESS_BUDGET_DESCRIPTION: &str =
    "Carry the limits of the alerts that the holder sends. It opens no page.";

/// Whether the editor shows `action` on `node`: the same rule as the script.
fn applies(action: &EditorAction, node: &EditorNode) -> bool {
    node.more.contains(&action.name)
        || if node.path.is_empty() {
            action.whole
        } else {
            action.types.contains(&node.kind)
                && (node.only.is_empty() || node.only.contains(&action.name))
        }
}

/// A service with no action: the base of a service that the editor adds for a grant.
fn bare_service(id: &str, name: &str) -> EditorService {
    EditorService {
        id: id.to_string(),
        name: name.to_string(),
        actions: Vec::new(),
        nodes: vec![EditorNode {
            path: String::new(),
            kind: SERVICE_TYPE.to_string(),
            label: "Whole service".to_string(),
            only: Vec::new(),
            more: Vec::new(),
            critical: false,
        }],
        types: Vec::new(),
        fetch: false,
        source: Source::Absent,
    }
}

/// Whether the service shows the grant: its action and its node are in it.
fn service_shows(service: &EditorService, grant: &Grant) -> bool {
    let path = grant.path_text();
    let (Some(action), Some(node)) = (
        service.actions.iter().find(|a| a.name == grant.action),
        service.nodes.iter().find(|n| n.path == path),
    ) else {
        return false;
    };
    applies(action, node)
}

impl EditorData {
    /// The part of the editor that holds the grants of this service.
    fn part(&self, service: &str) -> Option<&EditorService> {
        if service == crate::portal_actions::SERVICE {
            self.features.as_ref()
        } else {
            self.services.iter().find(|s| s.id == service)
        }
    }

    /// Whether the editor can show the grant `grant`: its service, its action and its node are in it.
    pub fn shows(&self, grant: &Grant) -> bool {
        self.part(&grant.service)
            .is_some_and(|service| service_shows(service, grant))
    }

    /// Make the editor able to show the grant `grant`: its service, its action and its node. A grant
    /// that the document does not name, for example of a service that went away, gets a line of its
    /// own.
    pub fn keep(&mut self, grant: &Grant) {
        let service = if grant.service == crate::portal_actions::SERVICE {
            self.features
                .get_or_insert_with(|| bare_service(&grant.service, "Server permissions"))
        } else {
            let at = match self.services.iter().position(|s| s.id == grant.service) {
                Some(at) => at,
                None => {
                    self.services
                        .push(bare_service(&grant.service, &grant.service));
                    self.services.len() - 1
                }
            };
            &mut self.services[at]
        };
        if !service.actions.iter().any(|a| a.name == grant.action) {
            service.actions.push(EditorAction {
                name: grant.action.clone(),
                description: String::new(),
                whole: false,
                types: Vec::new(),
                limits: Vec::new(),
                needs: Vec::new(),
            });
        }
        let path = grant.path_text();
        let node_at = match service.nodes.iter().position(|n| n.path == path) {
            Some(at) => at,
            None => {
                service.nodes.push(EditorNode {
                    path: path.clone(),
                    kind: grant.kind().to_string(),
                    label: path,
                    only: Vec::new(),
                    more: Vec::new(),
                    critical: false,
                });
                service.nodes.len() - 1
            }
        };
        let action = service
            .actions
            .iter()
            .find(|a| a.name == grant.action)
            .cloned();
        if let Some(action) = action {
            let node = &mut service.nodes[node_at];
            if !applies(&action, node) {
                node.more.push(action.name);
            }
        }
    }
}

/// The data of an editor from the services of the catalogue and the features of the portal.
///
/// - `features`: the service `portal` with the document of the features that the holder of the form
///   can give. `None`: the form has no part for the features.
/// - `owner`: the grants that the holder of the form can give. `None`: each grant. A right on a
///   service counts only when the owner also holds `access` on it: without it the right does not
///   work, so the editor does not offer it (portal 4.2b).
/// - `offered`: whether a grant can be ticked at all.
/// - `ticked`: the grants that the form has at the start.
/// - `keep_extras`: show a grant of the owner or a ticked grant that the documents do not
///   name. Without it, such a ticked grant is left out. The handler then keeps it as it is.
pub fn build(
    services: &[ServiceActions],
    features: Option<&ServiceActions>,
    owner: Option<&[Grant]>,
    offered: impl Fn(&Grant) -> bool,
    ticked: Vec<Grant>,
    keep_extras: bool,
) -> EditorData {
    let usable: Option<Vec<Grant>> = owner.map(|held| {
        held.iter()
            .filter(|g| {
                g.service == crate::portal_actions::SERVICE
                    || crate::grants::holds(held, ACCESS, &g.service)
            })
            .cloned()
            .collect()
    });
    let owner = usable.as_deref();
    let holds =
        |id: &str| owner.is_none_or(|held| held.iter().any(|g| g.service == id && offered(g)));
    let wanted = |id: &str| keep_extras && ticked.iter().any(|g| g.service == id);
    let built: Vec<EditorService> = services
        .iter()
        .filter(|service| holds(&service.id) || wanted(&service.id))
        .map(editor_service)
        .collect();
    let built_features = features
        .filter(|service| holds(&service.id) || wanted(&service.id))
        .map(editor_service);
    let mut data = EditorData {
        services: built,
        features: built_features,
        ticked,
        owner: owner.map(|held| held.iter().filter(|g| offered(g)).cloned().collect()),
        locked: Vec::new(),
    };
    if keep_extras {
        // Each grant of the owner, and each grant of the key, can be ticked (portal 4.2).
        let held: Vec<Grant> = owner
            .unwrap_or_default()
            .iter()
            .filter(|g| offered(g))
            .chain(data.ticked.iter())
            .cloned()
            .collect();
        for grant in &held {
            data.keep(grant);
        }
    } else {
        let mut ticked = std::mem::take(&mut data.ticked);
        ticked.retain(|g| data.shows(g));
        data.ticked = ticked;
    }
    data.ticked.sort();
    data
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grant(text: &str) -> Grant {
        Grant::parse(text).unwrap()
    }

    const PLACE: Place = Place {
        form: None,
        title: "Permissions of the key",
        blank_rows: 2,
        groups: false,
    };

    fn data() -> EditorData {
        EditorData {
            services: vec![EditorService {
                id: "db".into(),
                name: "Data</script>".into(),
                actions: vec![EditorAction {
                    name: "data.read".into(),
                    description: "Read & show.".into(),
                    whole: true,
                    types: vec!["host".into()],
                    limits: Vec::new(),
                    needs: Vec::new(),
                }],
                nodes: vec![EditorNode {
                    path: "host:prod".into(),
                    kind: "host".into(),
                    label: "Prod".into(),
                    only: Vec::new(),
                    more: Vec::new(),
                    critical: false,
                }],
                types: Vec::new(),
                fetch: false,
                source: Source::Fresh(std::time::Duration::from_secs(120)),
            }],
            features: None,
            ticked: vec![grant("data.read on service:db/host:prod")],
            owner: None,
            locked: Vec::new(),
        }
    }

    /// The service `portal` with a document that has the right to alert one account.
    fn portal_with_direct_alerts() -> ServiceActions {
        let text = r#"{"server":"portal","resource_types":[{"name":"inbox_group","parent":"service","description":"A group."}],"actions":[{"name":"alert.send","resource_types":["inbox_group"],"description":"Send an alert to the group."},{"name":"alert.send_user","resource_types":["service"],"description":"Send an alert to one account."}]}"#;
        let document = crate::actions::parse("portal", text.as_bytes()).unwrap();
        ServiceActions {
            id: "portal".into(),
            name: "Server permissions".into(),
            actions: document
                .actions
                .iter()
                .map(|a| (a.name.clone(), a.description.clone()))
                .collect(),
            document: std::sync::Arc::new(document),
            known: true,
            fetch: false,
            critical: Vec::new(),
            source: Source::Absent,
        }
    }

    #[test]
    fn a_document_with_the_right_to_alert_an_account_gives_the_editor_the_row_direct_alerts() {
        // The page draws the row "Direct alerts" from the document, and only when the document has
        // the action. The editor has no code for the row but the list of the actions.
        let features = portal_with_direct_alerts();
        let data = build(&[], Some(&features), None, |_| true, Vec::new(), true);
        let mut out = String::new();
        render(&mut out, &data, PLACE);
        assert!(out.contains(r#""name":"alert.send_user""#), "{out}");
        let without = ServiceActions {
            actions: vec![("alert.send".into(), "Send.".into())],
            ..portal_with_direct_alerts()
        };
        let data = build(&[], Some(&without), None, |_| true, Vec::new(), true);
        let mut out = String::new();
        render(&mut out, &data, PLACE);
        assert!(!out.contains("alert.send_user"));
        let script = include_str!("../../web/settings.js");
        assert!(
            script.contains(r#"var DIRECT_ROWS = ["alert.send_user"];"#)
                && script.contains(r#"flatSection("user", "Direct alerts", DIRECT_ROWS);"#)
        );
    }

    #[test]
    fn the_json_block_tells_where_the_document_comes_from_and_how_old_it_is() {
        let mut out = String::new();
        render(&mut out, &data(), PLACE);
        assert!(
            out.contains(r#""source":{"secs":120,"state":"fresh"}"#),
            "{out}"
        );
        let mut stale = data();
        stale.services[0].source = Source::Stale(std::time::Duration::from_secs(2400));
        let mut out = String::new();
        render(&mut out, &stale, PLACE);
        assert!(out.contains(r#""source":{"secs":2400,"state":"stale"}"#));
        stale.services[0].source = Source::Absent;
        let mut out = String::new();
        render(&mut out, &stale, PLACE);
        assert!(out.contains(r#""source":{"state":"absent"}"#));
    }

    #[test]
    fn the_json_block_cannot_close_its_element() {
        let mut out = String::new();
        render(&mut out, &data(), PLACE);
        let block = &out[out.find("application/json").unwrap()..];
        let block = &block[..block.find("</script>").unwrap()];
        assert!(!block.contains("<"), "{block}");
        assert!(block.contains("Data\\u003c/script\\u003e"), "{block}");
        assert!(block.contains("Read \\u0026 show."), "{block}");
        assert!(block.contains(r#""owner":null"#), "{block}");
    }

    #[test]
    fn the_ticked_grants_are_hidden_fields_and_the_blank_rows_follow_them() {
        let mut out = String::new();
        render(&mut out, &data(), PLACE);
        assert!(out.contains(
            r#"<input type="hidden" name="grant0" value="data.read on service:db/host:prod">"#
        ));
        assert!(out.contains(r#"name="grant1""#) && out.contains(r#"name="grant2""#));
        assert!(!out.contains(r#"name="grant3""#));
        assert!(out.contains("<noscript>"));
    }

    /// The service `portal` for an editor, with a critical group and both kinds of key.
    fn portal_service() -> ServiceActions {
        let features = crate::portal_actions::Features {
            inbox_groups: Some(vec![
                ("infra".into(), "Infra".into()),
                ("crit".into(), "Critical".into()),
            ]),
            kinds: vec![crate::keys::KeyKind::Mcp, crate::keys::KeyKind::Api],
        };
        let document = crate::portal_actions::document(&features);
        ServiceActions {
            id: crate::portal_actions::SERVICE.into(),
            name: "Server permissions".into(),
            actions: document
                .actions
                .iter()
                .map(|a| (a.name.clone(), a.description.clone()))
                .collect(),
            document,
            known: true,
            fetch: false,
            critical: vec!["crit".into()],
            source: crate::actions::Source::Absent,
        }
    }

    fn features_json(data: &EditorData) -> serde_json::Value {
        let mut out = String::new();
        render(&mut out, data, PLACE);
        let block = &out[out.find("application/json").unwrap()..];
        let block = &block[block.find('>').unwrap() + 1..];
        let block = block[..block.find("</script>").unwrap()].replace("\\u003c", "<");
        serde_json::from_str::<serde_json::Value>(&block).unwrap()["features"].clone()
    }

    #[test]
    fn the_data_of_the_features_has_the_nodes_the_critical_mark_and_the_needs_for_the_matrices() {
        let service = portal_service();
        let data = build(&[], Some(&service), None, |_| true, Vec::new(), true);
        let features = features_json(&data);
        let nodes: Vec<(&str, bool)> = features["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|n| {
                (
                    n["path"].as_str().unwrap(),
                    n["critical"].as_bool().unwrap(),
                )
            })
            .collect();
        // The kinds of key and the groups of the inbox are nodes. Only the group `crit` is critical.
        assert!(
            nodes.contains(&("key_kind:api", false)) && nodes.contains(&("key_kind:mcp", false))
        );
        assert!(nodes.contains(&("inbox_group:infra", false)));
        assert!(nodes.contains(&("inbox_group:crit", true)), "{nodes:?}");
        let action = |name: &str| {
            features["actions"]
                .as_array()
                .unwrap()
                .iter()
                .find(|a| a["name"] == name)
                .unwrap()
                .clone()
        };
        // The types of the actions decide the matrix of each one.
        assert_eq!(
            action("keys.own.create")["types"],
            serde_json::json!(["key_kind"])
        );
        assert_eq!(
            action("alert.send")["types"],
            serde_json::json!(["inbox_group"])
        );
        assert_eq!(
            action("phone.revoke")["needs"],
            serde_json::json!(["phone.read"])
        );
        assert_eq!(
            action("keys.own.edit")["needs"],
            serde_json::json!(["keys.own.read"])
        );
        // The ceiling of the lifetime and the budget of the alerts are the limits of two actions.
        assert_eq!(
            action("keys.own.use")["limits"][0]["name"],
            "max_key_lifetime"
        );
        assert_eq!(action("access")["limits"][0]["name"], "alerts");
        assert_eq!(action("access")["limits"][0]["budget"], true);
    }

    #[test]
    fn a_feature_that_the_file_turns_off_has_no_action_and_no_node() {
        let mut service = portal_service();
        let features = crate::portal_actions::Features {
            inbox_groups: None,
            kinds: vec![crate::keys::KeyKind::Mcp],
        };
        service.document = crate::portal_actions::document(&features);
        service.actions = service
            .document
            .actions
            .iter()
            .map(|a| (a.name.clone(), a.description.clone()))
            .collect();
        let data = build(&[], Some(&service), None, |_| true, Vec::new(), true);
        let features = features_json(&data);
        let text = features.to_string();
        assert!(
            !text.contains("alert.send")
                && !text.contains("phone.read")
                && !text.contains("inbox_group")
        );
        assert!(text.contains("key_kind:mcp") && !text.contains("key_kind:api"));
    }

    /// A service with one action on the whole service and on a host.
    fn datastore() -> ServiceActions {
        let text = r#"{"server":"db","resource_types":[{"name":"host","parent":"service","description":"A host."}],"actions":[{"name":"data.read","resource_types":["service","host"],"description":"Read."}],"resources":[{"path":"host:prod","label":"Prod"}]}"#;
        let document = crate::actions::parse("db", text.as_bytes()).unwrap();
        ServiceActions {
            id: "db".into(),
            name: "Data".into(),
            actions: vec![("data.read".into(), "Read.".into())],
            document: std::sync::Arc::new(document),
            known: true,
            fetch: false,
            critical: Vec::new(),
            source: Source::Absent,
        }
    }

    fn owner_json(data: &EditorData) -> serde_json::Value {
        let mut out = String::new();
        render(&mut out, data, PLACE);
        let block = &out[out.find("application/json").unwrap()..];
        let block = &block[block.find('>').unwrap() + 1..];
        let block = block[..block.find("</script>").unwrap()].replace("\\u003c", "<");
        serde_json::from_str::<serde_json::Value>(&block).unwrap()
    }

    #[test]
    fn an_owner_without_access_on_a_service_cannot_give_a_right_on_it() {
        // The right of a key works only when the owner can open the service too (portal 4.2b): the
        // editor offers no service, no node and no right that the owner holds without `access`.
        let services = [datastore()];
        let rights = vec![grant("data.read on service:db/host:prod")];
        let data = build(&services, None, Some(&rights), |_| true, Vec::new(), true);
        assert!(data.services.is_empty(), "{:?}", data.services);
        assert_eq!(data.owner.as_deref(), Some(&[][..]));
        // With `access`, the service and its rights are in.
        let both = vec![
            grant("access on service:db"),
            grant("data.read on service:db/host:prod"),
        ];
        let data = build(&services, None, Some(&both), |_| true, Vec::new(), true);
        assert_eq!(data.services.len(), 1);
        assert_eq!(owner_json(&data)["owner"].as_array().unwrap().len(), 2);
        // A key that holds a right already keeps its line, so the person can remove it.
        let kept = vec![grant("data.read on service:db/host:prod")];
        let data = build(&services, None, Some(&rights), |_| true, kept, true);
        assert_eq!(data.services.len(), 1);
        assert_eq!(data.owner.as_deref(), Some(&[][..]));
    }

    #[test]
    fn the_rights_on_the_portal_features_need_no_access() {
        let features = portal_service();
        let rights = vec![grant("keys.own.read on service:portal")];
        let data = build(
            &[],
            Some(&features),
            Some(&rights),
            |_| true,
            Vec::new(),
            true,
        );
        assert!(data.features.is_some());
        assert_eq!(data.owner.as_deref().map(<[Grant]>::len), Some(1));
    }

    #[test]
    fn the_editor_has_a_live_region_and_the_symbols_of_the_source_lines() {
        let mut out = String::new();
        render(&mut out, &data(), PLACE);
        assert!(out.contains(r#"role="status" data-ge-live"#), "{out}");
        for symbol in ["#i-circle-check", "#i-clock", "#i-ban"] {
            assert!(out.contains(symbol), "{symbol}");
        }
    }

    #[test]
    fn the_script_keeps_the_note_the_undo_and_the_focus() {
        let script = include_str!("../../web/settings.js");
        for needle in [
            "data-undo",
            "function undoClose(serviceId)",
            "function headFocus()",
            "restore: function (kept)",
            "aria-level",
        ] {
            assert!(script.contains(needle), "{needle}");
        }
        // The timer of the note is gone: the note stays until the next change.
        assert!(!script.contains("}, 5000)"));
    }

    #[test]
    fn the_script_ticks_access_for_each_right_and_removes_the_rights_with_access() {
        // The page has no script test harness: the rule is in one function that every path uses.
        let script = include_str!("../../web/settings.js");
        for needle in [
            "function hasSwitch(serviceId)",
            "if (action !== \"access\" && hasSwitch(serviceId)) openService(serviceId);",
            "closeService(serviceId);",
            "function closeService(serviceId)",
        ] {
            assert!(script.contains(needle), "{needle}");
        }
        // The only direct writes to the map of the grants are the load, the one function, the
        // budget of the portal, the "give access" link and the undo of "Access off".
        let writes = script.matches("ticked.set(").count();
        assert_eq!(writes, 7, "a new write must go through setGrant");
    }
}
