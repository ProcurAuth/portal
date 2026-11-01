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
}

/// The actions and the tree of one service.
#[derive(Clone, Debug)]
pub struct EditorService {
    pub id: String,
    pub name: String,
    pub actions: Vec<EditorAction>,
    pub nodes: Vec<EditorNode>,
    /// The types of the document with their parents: the types of the children of a node.
    pub types: Vec<(String, String)>,
    /// The portal can ask the service for the children of a node (portal 3.1a).
    pub fetch: bool,
}

/// What the form needs to draw the editor.
#[derive(Clone, Debug)]
pub struct EditorData {
    pub services: Vec<EditorService>,
    /// The grants that the form has at the start, with their limit values.
    pub ticked: Vec<Grant>,
    /// The grants of the owner. `None`: the superadmin, who has every right.
    pub owner: Option<Vec<Grant>>,
    /// Grants that the form shows and does not change: they come from the configuration file.
    pub locked: Vec<Grant>,
}

/// The icons that the script draws. The page inlines the symbols that its text uses.
const ICONS: &str = r##"<svg class="icon" hidden aria-hidden="true"><use href="#i-chevron-down"/><use href="#i-chevron-right"/><use href="#i-x"/><use href="#i-check"/><use href="#i-alert-triangle"/><use href="#i-lock"/></svg>"##;

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

/// The JSON block of the editor. `<`, `>` and `&` are written as `\u00XX`, so the text cannot
/// close the element.
fn json_block(data: &EditorData) -> String {
    let services: Vec<serde_json::Value> = data
        .services
        .iter()
        .map(|service| {
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
                    .map(|(name, parent)| serde_json::json!({ "name": name, "parent": parent }))
                    .collect::<Vec<_>>(),
                "fetch": service.fetch,
            })
        })
        .collect();
    let value = serde_json::json!({
        "services": services,
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
        r#"<div class="grantEditor" data-title="{}"{}><script type="application/json" class="grantEditor__data">{}</script><div class="grantEditor__fields">"#,
        html_escape(place.title),
        place
            .form
            .map(|id| format!(r#" data-form="{id}""#))
            .unwrap_or_default(),
        json_block(data)
    );
    hidden_fields(out, data, place.form);
    out.push_str(ICONS);
    out.push_str("</div>");
    if place.blank_rows > 0 {
        out.push_str("<noscript><div class=\"grantRows\">");
        for k in 0..place.blank_rows {
            let n = data.ticked.len() + k;
            let _ = write!(
                out,
                r#"<div class="grantRow"><label>Another grant <input class="uiInput" type="text" name="grant{n}" value="" size="48" maxlength="4096" placeholder="data.read on service:datastore/host:prod/database:test1"></label></div>"#
            );
        }
        out.push_str("</div></noscript>");
    }
    out.push_str("</div>");
}

// The data of an editor ------------------------------------------------------------------------

/// The text of the grant `access`.
const ACCESS_DESCRIPTION: &str = "Open the service. The gate lets the holder in.";

/// The actions and the tree of one service.
fn editor_service(service: &ServiceActions, portal_service: &str) -> EditorService {
    let mut actions = vec![EditorAction {
        name: ACCESS.to_string(),
        description: ACCESS_DESCRIPTION.to_string(),
        whole: true,
        types: Vec::new(),
        limits: service.document.limit_inputs(ACCESS),
    }];
    let mut nodes = vec![EditorNode {
        path: String::new(),
        kind: SERVICE_TYPE.to_string(),
        label: "Whole service".to_string(),
        only: Vec::new(),
        more: Vec::new(),
    }];
    if service.id != portal_service {
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
            });
        }
        for node in &service.document.resources {
            nodes.push(EditorNode {
                path: crate::grants::path_text(&node.path),
                kind: node.path.last().map(|s| s.kind.clone()).unwrap_or_default(),
                label: node.label.clone(),
                only: node.actions.clone(),
                more: Vec::new(),
            });
        }
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
            .map(|t| (t.name.clone(), t.parent.clone()))
            .collect(),
        fetch: service.fetch,
    }
}

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

/// Whether the editor can show the grant `grant`: its service, its action and its node are in it.
pub fn shows(services: &[EditorService], grant: &Grant) -> bool {
    let Some(service) = services.iter().find(|s| s.id == grant.service) else {
        return false;
    };
    let path = grant.path_text();
    let (Some(action), Some(node)) = (
        service.actions.iter().find(|a| a.name == grant.action),
        service.nodes.iter().find(|n| n.path == path),
    ) else {
        return false;
    };
    applies(action, node)
}

/// Make the editor able to show the grant `grant`: its service, its action and its node. A grant
/// that the document does not name, for example of a service that went away, gets a line of its
/// own.
pub fn keep(services: &mut Vec<EditorService>, grant: &Grant) {
    let at = match services.iter().position(|s| s.id == grant.service) {
        Some(at) => at,
        None => {
            services.push(EditorService {
                id: grant.service.clone(),
                name: grant.service.clone(),
                actions: Vec::new(),
                nodes: vec![EditorNode {
                    path: String::new(),
                    kind: SERVICE_TYPE.to_string(),
                    label: "Whole service".to_string(),
                    only: Vec::new(),
                    more: Vec::new(),
                }],
                types: Vec::new(),
                fetch: false,
            });
            services.len() - 1
        }
    };
    let service = &mut services[at];
    if !service.actions.iter().any(|a| a.name == grant.action) {
        service.actions.push(EditorAction {
            name: grant.action.clone(),
            description: String::new(),
            whole: false,
            types: Vec::new(),
            limits: Vec::new(),
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

/// The data of an editor from the services of the catalogue.
///
/// - `owner`: the grants that the holder of the form can give. `None`: each grant.
/// - `offered`: whether a grant can be ticked at all.
/// - `ticked`: the grants that the form has at the start.
/// - `keep_extras`: show a grant of the owner or a ticked grant that the documents do not
///   name. Without it, such a ticked grant is left out. The handler then keeps it as it is.
pub fn build(
    services: &[ServiceActions],
    owner: Option<&[Grant]>,
    offered: impl Fn(&Grant) -> bool,
    ticked: Vec<Grant>,
    keep_extras: bool,
) -> EditorData {
    let portal_service = crate::portal_actions::SERVICE;
    let mut built: Vec<EditorService> = services
        .iter()
        .filter(|service| {
            owner.is_none_or(|held| held.iter().any(|g| g.service == service.id && offered(g)))
                || (keep_extras && ticked.iter().any(|g| g.service == service.id))
        })
        .map(|service| editor_service(service, portal_service))
        .collect();
    let mut ticked = ticked;
    if keep_extras {
        // Each grant of the owner, and each grant of the key, can be ticked (portal 4.2).
        let held: Vec<&Grant> = owner
            .unwrap_or_default()
            .iter()
            .filter(|g| offered(g))
            .chain(ticked.iter())
            .collect();
        for grant in held {
            keep(&mut built, grant);
        }
    } else {
        ticked.retain(|g| shows(&built, g));
    }
    ticked.sort();
    EditorData {
        services: built,
        ticked,
        owner: owner.map(|held| held.iter().filter(|g| offered(g)).cloned().collect()),
        locked: Vec::new(),
    }
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
                }],
                nodes: vec![EditorNode {
                    path: "host:prod".into(),
                    kind: "host".into(),
                    label: "Prod".into(),
                    only: Vec::new(),
                    more: Vec::new(),
                }],
                types: Vec::new(),
                fetch: false,
            }],
            ticked: vec![grant("data.read on service:db/host:prod")],
            owner: None,
            locked: Vec::new(),
        }
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
}
