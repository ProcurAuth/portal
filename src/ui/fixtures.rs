//! Shared fixtures of the tests of the settings pages.

use std::collections::BTreeMap;

use super::access::{AccessView, Accounts, GroupLine, Query};
use super::keys::{Cover, GrantLine, KeyLine, KeyState, KeysList};
use super::settings::{Frame, Holder, ServiceActions, ServicesView, UserCard, UserView, PORTAL};
use crate::catalog::{parse_labels, Catalog, Service};
use crate::grants::{Grant, Mode, ACCESS};
use crate::keys::KeyKind;
use crate::store::{AccountActivity, AuditRow, DayStat, LoginPage, LoginRow};

pub(super) fn service(name: &str, href: &str) -> Service {
    let labels: BTreeMap<String, String> = [
        ("portal.name", name),
        ("portal.href", href),
        ("portal.group", "Infra"),
    ]
    .iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect();
    parse_labels("x", &labels).unwrap().unwrap()
}

pub(super) fn grant(action: &str, service: &str) -> Grant {
    Grant::new(action, service).unwrap()
}

pub(super) fn sub(text: &str) -> Grant {
    Grant::parse(text).unwrap()
}

/// The data that the page gives to the editor of grants.
pub(super) fn editor_of(html: &str) -> serde_json::Value {
    let start =
        html.find(r#"class="grantEditor__data">"#).unwrap() + r#"class="grantEditor__data">"#.len();
    let end = start + html[start..].find("</script>").unwrap();
    serde_json::from_str(&html[start..end]).unwrap()
}

/// `access` on each named service (ids `infra-<name>`).
pub(super) fn access(services: &[&str]) -> Vec<Grant> {
    services
        .iter()
        .map(|s| grant(ACCESS, &format!("infra-{s}")))
        .collect()
}

pub(super) fn card(login: &str, services: &[&str]) -> UserCard {
    UserCard {
        login: login.into(),
        grants: access(services),
        configured: Vec::new(),
        created_at: Some(1_791_000_000),
        superadmin: false,
        file: false,
        keys: 0,
        groups: Vec::new(),
        activity: AccountActivity {
            login: login.into(),
            days: vec![DayStat::default(); 30],
            ..AccountActivity::default()
        },
    }
}

pub(super) fn frame() -> Frame<'static> {
    Frame {
        logout_token: "out<",
        keys: false,
        inbox: false,
        superadmin: true,
        notify: None,
        security: false,
        tab: crate::ui::TabLook::DEFAULT,
    }
}

/// The four tabs: the file has the blocks `keys` and `inbox`.
pub(super) fn full_frame() -> Frame<'static> {
    Frame {
        keys: true,
        inbox: true,
        ..frame()
    }
}

/// The document of Grafana in the tests: a folder, and the dashboards of a folder.
pub(super) const GRAFANA: &str = r#"{"server":"infra-grafana","resource_types":[{"name":"folder","parent":"service","description":"A folder."},{"name":"dashboard","parent":"folder","description":"The type dashboard."}],"actions":[{"name":"dashboards.read","resource_types":["service","folder"],"description":"See the dashboards."},{"name":"dashboards.write","resource_types":["service","folder","dashboard"],"description":"Change the <dashboards>."}]}"#;

pub(super) fn services() -> Vec<ServiceActions> {
    let plain = |id: &str, name: &str| ServiceActions {
        external: false,
        id: id.into(),
        name: name.into(),
        actions: Vec::new(),
        document: Default::default(),
        known: true,
        fetch: false,
        critical: Vec::new(),
        source: crate::actions::Source::Absent,
    };
    let grafana = crate::actions::parse("infra-grafana", GRAFANA.as_bytes()).unwrap();
    vec![
        ServiceActions {
            external: false,
            id: "infra-grafana".into(),
            name: "Grafana".into(),
            actions: grafana
                .actions
                .iter()
                .map(|a| (a.name.clone(), a.description.clone()))
                .collect(),
            document: std::sync::Arc::new(grafana),
            known: true,
            fetch: false,
            critical: Vec::new(),
            source: crate::actions::Source::Absent,
        },
        plain("infra-notes", "Notes"),
        plain("infra-typo", "Typo"),
        plain("infra-mixed", "Mixed"),
    ]
}

pub(super) fn catalog() -> Catalog {
    Catalog::build(
        vec![
            service("Grafana", "/grafana/"),
            service("Notes", "/notes/"),
            service("Typo", "/typo/"),
            service("Mixed", "/mixed/"),
        ],
        Vec::new(),
    )
}

/// The portal features with the keys and the inbox: the real document of the code.
pub(super) fn features() -> ServiceActions {
    use crate::keys::KeyKind;
    use crate::portal_actions::{document, Features};
    let document = document(&Features {
        inbox_groups: Some(vec![("infra".into(), "Infra".into())]),
        kinds: vec![KeyKind::Api, KeyKind::Mcp],
    });
    ServiceActions {
        external: false,
        id: PORTAL.into(),
        name: "Server permissions".into(),
        actions: document
            .actions
            .iter()
            .map(|a| (a.name.clone(), a.description.clone()))
            .collect(),
        document,
        known: true,
        fetch: false,
        critical: Vec::new(),
        source: crate::actions::Source::Absent,
    }
}

pub(super) struct Data {
    pub(super) logins: LoginPage,
    pub(super) days: Vec<DayStat>,
    pub(super) events: Vec<AuditRow>,
}

pub(super) fn login_row_of(login: &str, success: bool, known: bool) -> LoginRow {
    LoginRow {
        ts: 1_791_623_502,
        login: login.into(),
        known,
        success,
        ip: Some("203.0.113.7".into()),
        user_agent: "Mozilla/5.0 (X11; Linux x86_64; rv:131.0) Gecko/20100101 Firefox/131.0".into(),
    }
}

/// The start of the first day of the graph in the fixtures: 2026-10-06 UTC, and 29 days before today.
pub(super) const FIRST_DAY: i64 = 1791244800;

pub(super) fn data() -> Data {
    let mut days = vec![DayStat::default(); 30];
    days[29] = DayStat {
        sessions: 4,
        seconds: 10,
    };
    Data {
        logins: LoginPage {
            rows: vec![
                login_row_of("alice", true, true),
                login_row_of("<b>x</b>", false, false),
            ],
            matching: 2,
            total: 12,
            refused: 3,
        },
        days,
        events: vec![AuditRow {
            ts: 1_791_623_000,
            kind: "user_updated".into(),
            login: Some("alice".into()),
            actor: Some("root".into()),
            ip: Some("192.0.2.10".into()),
        }],
    }
}

/// The accounts of the tests: alice and bob are in the group infra, carol is in none.
pub(super) fn accounts(catalog: &Catalog) -> Accounts<'_> {
    let mut users = vec![
        card("alice", &["grafana", "mixed"]),
        card("bob", &["mixed"]),
        card("carol", &[]),
    ];
    users[0].groups = vec!["infra".into(), "oncall".into()];
    users[1].groups = vec!["infra".into()];
    Accounts {
        allow_creation: true,
        mode: Mode::Dynamic,
        catalog,
        services: services(),
        users,
        features: Some(features()),
        max_users: 64,
        token: "tok<en",
        two_factor: Some(["alice".to_string()].into_iter().collect()),
    }
}

pub(super) fn groups() -> Vec<GroupLine> {
    vec![
        GroupLine {
            id: "infra".into(),
            name: "Infra".into(),
            critical: false,
        },
        GroupLine {
            id: "oncall".into(),
            name: "On call".into(),
            critical: true,
        },
    ]
}

/// A key of the tests. `grants` are texts of grants, all covered by the owner.
pub(super) fn key(id: &str, owner: Option<&str>, kind: KeyKind, grants: &[&str]) -> KeyLine {
    KeyLine {
        id: id.into(),
        owner: owner.map(str::to_string),
        mine: owner == Some("alice"),
        name: format!("{id}-key"),
        hint: "AbCd".into(),
        kind: Some(kind),
        state: KeyState::Active,
        file: false,
        created_at: 1_791_000_000,
        expires_at: None,
        last_check_at: None,
        check_count: 0,
        revoked_at: None,
        grants: grants
            .iter()
            .map(|text| GrantLine {
                grant: Grant::parse(text).unwrap(),
                cover: Cover::Full,
                imposed: false,
            })
            .collect(),
        no_access: Vec::new(),
        interaction: Some(crate::keys::Interaction::default_for(kind)),
        can_change: owner == Some("alice"),
    }
}

/// The list of keys of the tests: all the keys of the accounts, for a viewer that may do all.
pub(super) fn keys_list() -> KeysList {
    KeysList {
        lines: vec![
            key(
                "k1",
                Some("alice"),
                KeyKind::Api,
                &["alert.send on service:portal/inbox_group:infra"],
            ),
            key(
                "k2",
                Some("alice"),
                KeyKind::Mcp,
                &[
                    "access on service:infra-notes",
                    "notes.read on service:infra-notes",
                ],
            ),
            key(
                "k3",
                Some("bob"),
                KeyKind::Api,
                &["alert.read on service:portal/inbox_group:oncall"],
            ),
        ],
        truncated: false,
        all: true,
        create_kinds: vec![KeyKind::Mcp, KeyKind::Api],
        can_revoke: true,
        can_revoke_all: true,
        impersonated: false,
        cannot_use: false,
        groups: vec![
            ("infra".into(), "Infra <b>".into()),
            ("oncall".into(), "On call".into()),
        ],
        csrf: "tok<".into(),
    }
}

/// The tab Access with everything on: the three accounts, the keys and the groups.
pub(super) fn view(catalog: &Catalog) -> AccessView<'_> {
    AccessView {
        notice: None,
        query: Query::default(),
        accounts: Some(accounts(catalog)),
        keys: Some(keys_list()),
        groups: groups(),
    }
}

pub(super) fn user_view(catalog: &Catalog, user: UserCard) -> UserView<'_> {
    UserView {
        notice: None,
        mode: Mode::Dynamic,
        catalog,
        services: services(),
        user,
        token: "tok<en",
        features: Some(features()),
        owned_keys: 0,
        groups: Vec::new(),
        keys: Vec::new(),
        two_factor: None,
    }
}

pub(super) fn holder(login: &str, grants: Vec<Grant>) -> Holder {
    Holder {
        login: login.into(),
        grants,
        file: false,
    }
}

pub(super) fn services_view<'a>(
    catalog: &'a Catalog,
    actions: &'a [ServiceActions],
    holders: &'a [Holder],
) -> ServicesView<'a> {
    ServicesView {
        catalog,
        services: actions,
        holders,
        problems: Vec::new(),
        mode: Mode::Dynamic,
        selected: None,
        only_check: false,
        checks: false,
        notice: None,
        token: "demo",
        can_add: false,
        from_file: &[],
    }
}

pub(super) fn main_of(html: &str) -> &str {
    &html[html.find("<main").unwrap()..html.find("</main>").unwrap()]
}
