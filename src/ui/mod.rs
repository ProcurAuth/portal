//! Server-rendered pages: the launcher, the authentication cards and the settings pages.
//!
//! The browser receives finished HTML. The only script of every page is the theme selector (the
//! Users settings page adds the password generator); everything else works without JavaScript. No
//! page references anything outside this binary (the CSS and the scripts are served from `/auth/`,
//! the icon sprite is inlined, reduced to the symbols the page draws).
//!
//! What a page costs the browser is a design input: a page carries only what it shows (the settings
//! are pages of their own, not a hidden block of the launcher), its stylesheet only the rules it
//! uses, and the login and error pages draw no icon at all.
//!
//! Interface: [`home_page`], [`auth_document`], [`message_page`], [`denied_page`], the settings pages
//! of [`settings`], the embedded assets ([`CSS`], [`SETTINGS_CSS`], [`THEME_JS`], [`SETTINGS_JS`]),
//! and [`demo_page`] (synthetic data for `portal render-demo`).

pub mod settings;

use std::fmt::Write as _;

use crate::catalog::{parse_labels, Catalog, Group, Health, Service};
use crate::icons::Icons;
use crate::util::html_escape;
pub use settings::{
    logins_page, notice_for, roles_page, services_page, users_page, Frame, LoginsView, RolesView,
    ServicesView, UserCard, UsersView,
};

pub const CSS: &str = include_str!("../../web/portal.css");
pub const SETTINGS_CSS: &str = include_str!("../../web/settings.css");
pub const THEME_JS: &str = include_str!("../../web/theme.js");
pub const SETTINGS_JS: &str = include_str!("../../web/settings.js");
const SPRITE: &str = include_str!("../../web/sprite.svg");

const MAX_LISTED_PROBLEMS: usize = 3;

/// Who is looking at the launcher, and what the page may offer them.
pub struct HomeContext<'a> {
    /// Login of the signed-in account, for the "Viewing as" banner.
    pub viewer: &'a str,
    /// The superadmin is browsing as `viewer`.
    pub impersonating: bool,
    pub logout_token: &'a str,
    /// Anti-CSRF token of the account actions (the banner's "Back to superadmin").
    pub users_token: &'a str,
    /// Services the roles of the viewer hide from the page.
    pub hidden_services: usize,
    /// The gear that opens the settings: only for the superadmin.
    pub settings: bool,
}

/// The launcher: one full-width row per group, a tile per service.
pub fn home_page(catalog: &Catalog, icons: &Icons, ctx: &HomeContext) -> String {
    let mut body = String::new();
    body.push_str(&header(ctx));

    if !catalog.problems().is_empty() {
        body.push_str(&problems_notice(catalog.problems()));
    }

    if catalog.groups().is_empty() && ctx.hidden_services > 0 {
        body.push_str(
            r#"<main class="empty"><h1>No service available</h1><p>None of the services is open to your roles. Ask the administrator.</p></main>"#,
        );
    } else if catalog.groups().is_empty() {
        body.push_str(
            r#"<main class="empty"><h1>No services yet</h1><p>Add <code>portal.name</code> and <code>portal.href</code> labels to a container or an IngressRoute and it appears here.</p></main>"#,
        );
    } else {
        body.push_str(r#"<main class="rows"><h1 class="visually-hidden">Services</h1>"#);
        for (index, group) in catalog.groups().iter().enumerate() {
            group_row(&mut body, index, group, icons);
        }
        body.push_str("</main>");
    }
    let mut icons = vec!["i-device-desktop", "i-moon", "i-sun", "i-logout"];
    if ctx.settings {
        icons.push("i-settings");
    }
    document("Portal", "", &icons, &body)
}

/// Wraps an authentication card (`<h1>`, text, form) in the shared shell.
pub fn auth_document(title: &str, card_html: &str) -> String {
    let body = format!(r#"<main class="authShell"><div class="card">{card_html}</div></main>"#);
    document(title, "", &[], &body)
}

/// A card with a title, a sentence and one button back to the launcher: the 404 and the 403.
pub fn message_page(title: &str, text: &str, button: &str) -> String {
    auth_document(
        title,
        &format!(
            r#"<h1>{}</h1><p>{}</p><a class="button button--primary" href="/">{}</a>"#,
            html_escape(title),
            html_escape(text),
            html_escape(button)
        ),
    )
}

/// The 403: names the service when it is known, says what to do, and lets the visitor sign out
/// (the account they hold may be the wrong one).
pub fn denied_page(text: &str, viewer: &str, logout_token: &str) -> String {
    auth_document(
        "Access denied",
        &format!(
            r#"<h1>Access denied</h1><p>{}</p><div class="authActions"><a class="button button--primary" href="/">Back to panel</a><form method="post" action="/auth/logout"><input type="hidden" name="csrf" value="{}"><button class="button" type="submit">Sign out</button></form></div><p class="hint">Signed in as <strong>{}</strong>.</p>"#,
            html_escape(text),
            html_escape(logout_token),
            html_escape(viewer)
        ),
    )
}

/// A whole page. `head` holds the extra stylesheet of a page that has one; `icons` names the sprite
/// symbols the page draws.
fn document(title: &str, head: &str, icons: &[&str], body: &str) -> String {
    format!(
        r#"<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><meta name="color-scheme" content="dark light"><meta name="robots" content="noindex,nofollow"><title>{}</title><link rel="stylesheet" href="/auth/style.css">{head}<script src="/auth/assets/theme.js"></script></head><body>{}{body}</body></html>"#,
        html_escape(title),
        sprite(icons),
    )
}

/// The inline sprite reduced to `ids` (the file holds one `<symbol>` per line); nothing at all when
/// the page draws no icon.
fn sprite(ids: &[&str]) -> String {
    if ids.is_empty() {
        return String::new();
    }
    let mut out = String::from(
        r#"<svg class="sprite" xmlns="http://www.w3.org/2000/svg" width="0" height="0" aria-hidden="true" focusable="false">"#,
    );
    for line in SPRITE.lines() {
        let id = line
            .strip_prefix(r#"<symbol id=""#)
            .and_then(|rest| rest.split('"').next());
        if id.is_some_and(|id| ids.contains(&id)) {
            out.push_str(line);
        }
    }
    out.push_str("</svg>");
    out
}

/// The System / Dark / Light selector; the script fills in the pressed state and shows it (without
/// script it stays hidden, since it would do nothing).
fn theme_switch() -> String {
    let mut out = String::from(
        r#"<div class="themeSwitch" data-theme-switch role="group" aria-label="Theme">"#,
    );
    for (value, label, icon) in [
        ("system", "System theme", "i-device-desktop"),
        ("dark", "Dark theme", "i-moon"),
        ("light", "Light theme", "i-sun"),
    ] {
        let _ = write!(
            out,
            r##"<button type="button" data-theme-value="{value}" aria-pressed="false" aria-label="{label}" title="{label}"><svg class="icon" aria-hidden="true"><use href="#{icon}"/></svg></button>"##
        );
    }
    out.push_str("</div>");
    out
}

/// A form, not a link: signing out is a POST that ends the session at once, with no page in between.
fn sign_out_form(token: &str) -> String {
    format!(
        r##"<form method="post" action="/auth/logout"><input type="hidden" name="csrf" value="{}"><button class="button" type="submit"><svg class="icon" aria-hidden="true"><use href="#i-logout"/></svg>Sign out</button></form>"##,
        html_escape(token)
    )
}

fn header(ctx: &HomeContext) -> String {
    let mut out = String::new();
    out.push_str(r#"<header class="appHeader"><div class="appHeader__right">"#);
    if ctx.settings {
        out.push_str(
            r##"<a class="button button--icon" href="/auth/settings" aria-label="Settings" title="Settings"><svg class="icon" aria-hidden="true"><use href="#i-settings"/></svg></a>"##,
        );
    }
    out.push_str(&theme_switch());
    out.push_str(&sign_out_form(ctx.logout_token));
    out.push_str("</div></header>");
    if ctx.impersonating {
        let _ = write!(
            out,
            r#"<div class="viewAs" role="status"><span>Viewing as <strong>{}</strong>, signed in by the superadmin.</span><form method="post" action="/auth/users/impersonate-end"><input type="hidden" name="csrf" value="{}"><button class="button" type="submit">Back to superadmin</button></form></div>"#,
            html_escape(ctx.viewer),
            html_escape(ctx.users_token)
        );
    }
    out
}

fn problems_notice(problems: &[String]) -> String {
    let mut out = String::from(r#"<div class="notice" role="status">"#);
    for problem in problems.iter().take(MAX_LISTED_PROBLEMS) {
        let _ = write!(out, "<p>{}</p>", html_escape(problem));
    }
    if problems.len() > MAX_LISTED_PROBLEMS {
        let _ = write!(
            out,
            "<p>and {} more</p>",
            problems.len() - MAX_LISTED_PROBLEMS
        );
    }
    out.push_str("</div>");
    out
}

fn group_row(out: &mut String, index: usize, group: &Group, icons: &Icons) {
    let count = group.services.len();
    let _ = write!(
        out,
        r#"<section class="group" aria-labelledby="g-{index}"><div class="group__head"><h2 class="group__title" id="g-{index}">{}</h2><span class="group__count">{count} {}</span>"#,
        html_escape(&group.name),
        if count == 1 { "service" } else { "services" },
    );
    let down = group.down_count();
    if down > 0 {
        let _ = write!(
            out,
            r#"<span class="badge badge--danger">{down} down</span>"#
        );
    }
    out.push_str(r#"</div><ul class="tiles">"#);
    for service in &group.services {
        tile(out, service, icons);
    }
    out.push_str("</ul></section>");
}

fn tile(out: &mut String, service: &Service, icons: &Icons) {
    let target = if service.new_tab {
        r#" target="_blank" rel="noopener noreferrer""#
    } else {
        ""
    };
    let title = if service.description.is_empty() {
        String::new()
    } else {
        format!(r#" title="{}""#, html_escape(&service.description))
    };
    let state = match service.health {
        Health::Up => r#"<span class="tile__state tile__state--up">up</span>"#,
        Health::Down => r#"<span class="tile__state tile__state--down">down</span>"#,
        Health::Unknown => "",
    };
    let class = if service.health == Health::Down {
        "tile tile--down"
    } else {
        "tile"
    };
    let _ = write!(
        out,
        r#"<li><a class="{class}" href="{}"{target}{title}><span class="tile__icon" aria-hidden="true">{}</span><span class="tile__body"><span class="tile__name">{}</span><span class="tile__desc">{}</span></span>{state}</a></li>"#,
        html_escape(&service.href),
        icon_html(service, icons),
        html_escape(&service.name),
        html_escape(&service.description),
    );
}

/// The tile icon: the declared one, else one named like the service, else the service's initial.
fn icon_html(service: &Service, icons: &Icons) -> String {
    let declared = service.icon.as_deref();
    let by_name = service.name.as_str();
    match declared
        .into_iter()
        .chain([by_name])
        .find_map(|spec| icons.resolve(spec))
    {
        Some(icon) => format!(
            r#"<img class="tile__img{}" src="{}" alt="" width="24" height="24" loading="lazy" decoding="async">"#,
            if icon.set.is_monochrome() {
                " tile__img--mono"
            } else {
                ""
            },
            html_escape(&icon.url())
        ),
        None => html_escape(&monogram(&service.name)),
    }
}

/// Placeholder glyph for a service whose icon is not in the library: its first letter or digit.
fn monogram(name: &str) -> String {
    name.chars()
        .find(|c| c.is_alphanumeric())
        .map(|c| c.to_uppercase().collect())
        .unwrap_or_else(|| "?".to_string())
}

/// Synthetic services for `portal render-demo` and the tests. Parsed through the real label
/// parser so the demo also exercises it.
pub fn demo_catalog() -> Catalog {
    type Row<'a> = (&'a str, &'a [(&'a str, &'a str)], Health);
    let rows: [Row; 12] = [
        (
            "windmill",
            &[
                ("portal.group", "Infra"),
                ("portal.name", "Windmill"),
                ("portal.description", "Workflow automation and scripts"),
                ("portal.href", "/windmill/"),
                ("portal.weight", "40"),
                ("portal.roles", "ops, dev"),
            ],
            Health::Up,
        ),
        (
            "registry-ui",
            &[
                ("portal.group", "Infra"),
                ("portal.name", "Registry"),
                ("portal.description", "OCI registry browser"),
                ("portal.href", "/registry/"),
                ("portal.weight", "20"),
                ("portal.roles", "ops"),
            ],
            Health::Up,
        ),
        (
            "dns-updater",
            &[
                ("portal.group", "Infra"),
                ("portal.name", "DDNS"),
                (
                    "portal.description",
                    "Cloudflare DNS keeps the public address up to date",
                ),
                ("portal.href", "/dns-updater/"),
                ("portal.weight", "10"),
            ],
            Health::Up,
        ),
        (
            "traefik",
            &[
                ("portal.group", "Infra"),
                ("portal.name", "Traefik"),
                ("portal.description", "Reverse proxy and routing"),
                ("portal.href", "/traefik/"),
                ("portal.weight", "30"),
            ],
            Health::Up,
        ),
        (
            "s3-browser",
            &[
                ("portal.group", "Infra"),
                ("portal.name", "S3 browser"),
                ("portal.description", "SeaweedFS buckets"),
                ("portal.href", "/s3/"),
                ("portal.weight", "50"),
            ],
            Health::Unknown,
        ),
        (
            "grafana",
            &[
                ("portal.group", "Monitoring"),
                ("portal.name", "Grafana"),
                ("portal.description", "Dashboards and alerts"),
                ("portal.href", "/grafana/"),
                ("portal.weight", "10"),
                ("portal.roles", "ops, viewer"),
            ],
            Health::Down,
        ),
        (
            "prometheus",
            &[
                ("portal.group", "Monitoring"),
                ("portal.name", "Prometheus"),
                ("portal.description", "Metrics and targets"),
                ("portal.href", "/prometheus/"),
                ("portal.weight", "20"),
            ],
            Health::Up,
        ),
        (
            "cadvisor",
            &[
                ("portal.group", "Monitoring"),
                ("portal.name", "Cadvisor"),
                ("portal.description", "Container resource usage"),
                ("portal.href", "/cadvisor/"),
                ("portal.weight", "30"),
            ],
            Health::Up,
        ),
        (
            "session-tracer",
            &[
                ("portal.group", "Monitoring"),
                ("portal.name", "Session Tracer"),
                ("portal.description", "Linux audit session tracing"),
                ("portal.href", "/session-tracer/"),
                ("portal.weight", "40"),
            ],
            Health::Up,
        ),
        (
            "clickhouse-dash",
            &[
                ("portal.group", "Data"),
                ("portal.name", "ClickHouse Dash"),
                ("portal.description", "Query and explore ClickHouse"),
                ("portal.href", "/clickhouse-dash/"),
                ("portal.weight", "10"),
                ("portal.roles", "data"),
            ],
            Health::Up,
        ),
        (
            "pgweb",
            &[
                ("portal.group", "Data"),
                ("portal.name", "pgweb"),
                ("portal.description", "PostgreSQL browser"),
                ("portal.href", "/pgweb/"),
                ("portal.weight", "20"),
            ],
            Health::Up,
        ),
        (
            "inbox",
            &[
                ("portal.group", "Data"),
                ("portal.name", "Inbox"),
                ("portal.description", "Inbox service with encrypted devices"),
                ("portal.href", "/inbox/"),
                ("portal.weight", "30"),
            ],
            Health::Unknown,
        ),
    ];
    let mut services = Vec::new();
    for (origin, labels, health) in rows {
        let map = labels
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        if let Ok(Some(mut service)) = parse_labels(origin, &map) {
            service.health = health;
            services.push(service);
        }
    }
    Catalog::build(services, Vec::new())
}

/// The roles of the demo: `data`, asked by one demo service, is deliberately missing so the
/// Services page shows the warning.
pub fn demo_registry() -> Vec<String> {
    ["ops", "dev", "viewer"]
        .iter()
        .map(|r| r.to_string())
        .collect()
}

/// Synthetic accounts and history for `portal render-demo`.
fn demo_users<'a>(catalog: &'a Catalog, registry: &'a [String]) -> UsersView<'a> {
    use crate::store::{AccountActivity, AuditRow, DayStat, ServiceUse, SessionRow};
    let now = 1_791_623_502_i64;
    let first_day = now - now.rem_euclid(86_400) - 29 * 86_400;
    let days = |seed: u32| -> Vec<DayStat> {
        (0..30u32)
            .map(|i| {
                let sessions = (i * 7 + seed) % 5;
                DayStat {
                    sessions,
                    seconds: i64::from(sessions) * 1_800,
                }
            })
            .collect()
    };
    let card = |login: &str, roles: &[&str], seed: u32, superadmin: bool| {
        UserCard {
        login: login.into(),
        roles: roles.iter().map(|r| r.to_string()).collect(),
        created_at: (!superadmin).then_some(now - 20 * 86_400),
        superadmin,
        activity: AccountActivity {
            login: login.into(),
            days: days(seed),
            recent: vec![
                SessionRow {
                    started_at: now - 3_600,
                    last_seen_at: now - 60,
                    ended_at: None,
                    end_reason: None,
                    ip: Some("192.168.1.20".into()),
                    user_agent: "Mozilla/5.0 (X11; Linux x86_64; rv:131.0) Gecko/20100101 Firefox/131.0".into(),
                    impersonated: false,
                },
                SessionRow {
                    started_at: now - 90_000,
                    last_seen_at: now - 86_000,
                    ended_at: Some(now - 86_400),
                    end_reason: Some("logout".into()),
                    ip: Some("192.168.1.31".into()),
                    user_agent: "Mozilla/5.0 (iPhone; CPU iPhone OS 17_0 like Mac OS X) AppleWebKit/605.1.15 Safari/604.1".into(),
                    impersonated: false,
                },
            ],
            services: vec![
                ServiceUse { service_id: "infra-windmill".into(), hits: 412, last_at: now - 120 },
                ServiceUse { service_id: "monitoring-grafana".into(), hits: 96, last_at: now - 5_000 },
            ],
            sessions_30d: 18,
            failed_30d: 1,
            last_login: Some(now - 3_600),
        },
    }
    };
    UsersView {
        notice: None,
        registry,
        catalog,
        users: vec![
            card("alice", &["ops", "dev"], 2, false),
            card("bob", &["viewer"], 4, false),
        ],
        superadmin: card("root", &[], 1, true),
        days: days(3),
        first_day,
        recent_events: vec![
            AuditRow {
                ts: now - 300,
                kind: "user_added".into(),
                login: Some("bob".into()),
                actor: Some("root".into()),
                ip: Some("192.168.1.20".into()),
            },
            AuditRow {
                ts: now - 4_000,
                kind: "login_failed".into(),
                login: Some("alice".into()),
                actor: None,
                ip: Some("192.168.1.99".into()),
            },
        ],
        unknown_failures_30d: 3,
        max_users: 64,
        token: "demo",
    }
}

/// Synthetic sign-in attempts for `portal render-demo`.
fn demo_logins() -> crate::store::LoginPage {
    use crate::store::{LoginPage, LoginRow};
    let now = 1_791_623_502_i64;
    let firefox = "Mozilla/5.0 (X11; Linux x86_64; rv:131.0) Gecko/20100101 Firefox/131.0";
    let row = |ago: i64, login: &str, known: bool, success: bool, ip: &str, agent: &str| LoginRow {
        ts: now - ago,
        login: login.into(),
        known,
        success,
        ip: Some(ip.into()),
        user_agent: agent.into(),
    };
    LoginPage {
        rows: vec![
            row(120, "root", true, true, "192.168.1.20", firefox),
            row(2_000, "admin", false, false, "203.0.113.7", "curl/8.5.0"),
            row(2_010, "root", true, false, "203.0.113.7", "curl/8.5.0"),
            row(2_030, "'; DROP TABLE users; --", false, false, "198.51.100.23", "python-requests/2.31"),
            row(2_040, "<script>alert(1)</script>", false, false, "198.51.100.23", "python-requests/2.31"),
            row(9_000, "alice", true, true, "192.168.1.31", "Mozilla/5.0 (iPhone; CPU iPhone OS 17_0 like Mac OS X) AppleWebKit/605.1.15 Safari/604.1"),
        ],
        matching: 6,
        attempts_24h: 6,
        failed_24h: 4,
        addresses_24h: 2,
        ..LoginPage::default()
    }
}

/// A page of `portal render-demo`: the launcher or one settings page, on synthetic data. `None` for a
/// name that is not a page.
pub fn demo_page(name: &str, icons: &Icons) -> Option<String> {
    let catalog = demo_catalog();
    let registry = demo_registry();
    let frame = Frame {
        logout_token: "demo",
    };
    Some(match name {
        "launcher" => home_page(
            &catalog,
            icons,
            &HomeContext {
                viewer: "root",
                impersonating: false,
                logout_token: "demo",
                users_token: "demo",
                hidden_services: 0,
                settings: true,
            },
        ),
        "users" => users_page(&frame, &demo_users(&catalog, &registry)),
        "roles" => {
            let users = demo_users(&catalog, &registry);
            let holders: Vec<(String, Vec<String>)> = users
                .users
                .iter()
                .map(|u| (u.login.clone(), u.roles.clone()))
                .collect();
            roles_page(
                &frame,
                &RolesView {
                    registry: &registry,
                    catalog: &catalog,
                    holders: &holders,
                },
            )
        }
        "services" => services_page(
            &frame,
            &ServicesView {
                registry: &registry,
                catalog: &catalog,
            },
        ),
        "logins" => logins_page(
            &frame,
            &LoginsView {
                page: &demo_logins(),
                only_failed: false,
                retention_days: 90,
            },
        ),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn ctx<'a>() -> HomeContext<'a> {
        HomeContext {
            viewer: "alice",
            impersonating: false,
            logout_token: "t",
            users_token: "u",
            hidden_services: 0,
            settings: false,
        }
    }

    fn service(name: &str, description: &str, href: &str) -> Service {
        let labels: BTreeMap<String, String> = [
            ("portal.name", name),
            ("portal.description", description),
            ("portal.href", href),
            ("portal.group", "Infra"),
        ]
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        parse_labels("x", &labels).unwrap().unwrap()
    }

    #[test]
    fn page_renders_one_row_per_group_with_a_tile_per_service() {
        let page = home_page(&demo_catalog(), &Icons::empty(), &ctx());
        assert_eq!(page.matches(r#"<section class="group""#).count(), 3);
        assert_eq!(page.matches(r#"<a class="tile"#).count(), 12);
        assert!(page.contains(r#"<span class="badge badge--danger">1 down</span>"#));
        assert!(page.contains("Sign out"));
    }

    #[test]
    fn sign_out_is_a_post_form_carrying_its_token_and_no_page_shows_a_brand() {
        let page = home_page(
            &demo_catalog(),
            &Icons::empty(),
            &HomeContext {
                logout_token: "tok<en",
                ..ctx()
            },
        );
        assert!(page.contains(r#"<form method="post" action="/auth/logout"><input type="hidden" name="csrf" value="tok&lt;en">"#));
        assert!(
            !page.contains(r#"href="/auth/logout""#),
            "no link to a confirmation page"
        );
        for page in [page, auth_document("Authentication", "<p>x</p>")] {
            assert!(!page.contains("appBrand"));
            assert!(!page.contains(">portal</span>"));
        }
    }

    #[test]
    fn only_the_superadmin_gets_the_gear_and_it_opens_a_page_not_a_popup() {
        let plain = home_page(&demo_catalog(), &Icons::empty(), &ctx());
        assert!(!plain.contains("/auth/settings") && !plain.contains("#i-settings"));
        let page = home_page(
            &demo_catalog(),
            &Icons::empty(),
            &HomeContext {
                settings: true,
                ..ctx()
            },
        );
        assert!(page.contains(
            r##"<a class="button button--icon" href="/auth/settings" aria-label="Settings""##
        ));
        assert!(page.contains(r#"<symbol id="i-settings""#));
    }

    #[test]
    fn the_launcher_carries_none_of_the_settings_whoever_looks_at_it() {
        let page = home_page(
            &demo_catalog(),
            &Icons::empty(),
            &HomeContext {
                settings: true,
                ..ctx()
            },
        );
        for gone in ["id=\"settings\"", "modal", "r_ops", "userList", "tabInput", "class=\"warn\""] {
            assert!(!page.contains(gone), "{gone}");
        }
        assert!(
            page.matches('<').count() < 700,
            "a launcher of twelve services stays a small DOM"
        );
        assert!(!page.contains("settings.css") && !page.contains("settings.js"));
    }

    #[test]
    fn a_page_inlines_only_the_icons_it_draws() {
        let launcher = home_page(&demo_catalog(), &Icons::empty(), &ctx());
        assert_eq!(launcher.matches("<symbol ").count(), 4, "no gear for a user");
        assert!(!launcher.contains("i-alert-triangle") && !launcher.contains("i-chevron-left"));
        for page in [
            auth_document("Authentication", "<p>x</p>"),
            message_page("Page not found", "x", "Back"),
            denied_page("x", "alice", "t"),
        ] {
            assert!(!page.contains("<symbol") && !page.contains("class=\"sprite\""));
        }
        let settings = demo_page("users", &Icons::empty()).unwrap();
        assert_eq!(settings.matches("<symbol ").count(), 6);
        assert!(settings.contains(r#"<symbol id="i-chevron-left""#));
    }

    #[test]
    fn every_demo_page_renders_and_an_unknown_one_does_not() {
        for name in ["launcher", "users", "roles", "services", "logins"] {
            let page = demo_page(name, &Icons::empty()).unwrap();
            assert!(page.starts_with("<!doctype html>"), "{name}");
        }
        assert!(demo_page("nope", &Icons::empty()).is_none());
        let roles = demo_page("roles", &Icons::empty()).unwrap();
        assert!(roles.contains("alice") && roles.contains("viewer"));
        let services = demo_page("services", &Icons::empty()).unwrap();
        assert!(services.contains(r#"class="warn""#), "a demo service asks for a missing role");
        assert!(!services.contains(">data<"), "the missing role is never named");
        let logins = demo_page("logins", &Icons::empty()).unwrap();
        assert!(logins.contains("&#39;; DROP TABLE users; --") && !logins.contains("<script>alert"));
    }

    #[test]
    fn an_impersonated_page_says_so_and_offers_the_way_back() {
        let page = home_page(
            &demo_catalog(),
            &Icons::empty(),
            &HomeContext {
                impersonating: true,
                viewer: "<al>",
                users_token: "u<",
                ..ctx()
            },
        );
        assert!(page.contains("Viewing as <strong>&lt;al&gt;</strong>"));
        assert!(page.contains(
            r#"action="/auth/users/impersonate-end"><input type="hidden" name="csrf" value="u&lt;""#
        ));
        assert!(!home_page(&demo_catalog(), &Icons::empty(), &ctx()).contains("Viewing as"));
    }

    #[test]
    fn the_denied_page_names_the_service_and_offers_the_way_out() {
        let page = denied_page(
            "You do not have access to Grafana. Ask the administrator to give your account a role for it.",
            "al<ice",
            "tok<en",
        );
        assert!(page.contains("You do not have access to Grafana. Ask the administrator"));
        assert!(page.contains(r#"<a class="button button--primary" href="/">Back to panel</a>"#));
        assert!(page.contains(r#"action="/auth/logout""#) && page.contains("Sign out"));
        assert!(page.contains("Signed in as <strong>al&lt;ice</strong>"));
        assert!(page.contains(r#"value="tok&lt;en""#));
        assert!(!page.contains("al<ice"));
    }

    #[test]
    fn a_down_service_marks_its_whole_tile_and_the_launcher_has_a_heading() {
        let mut down = demo_catalog();
        let page = home_page(&down, &Icons::empty(), &ctx());
        assert!(page.contains(r#"<h1 class="visually-hidden">Services</h1>"#));
        let healthy = page.matches(r#"class="tile tile--down""#).count();
        down = Catalog::build(
            demo_catalog()
                .groups()
                .iter()
                .flat_map(|g| g.services.iter().cloned())
                .map(|mut s| {
                    s.health = Health::Down;
                    s
                })
                .collect(),
            Vec::new(),
        );
        let broken = home_page(&down, &Icons::empty(), &ctx());
        assert!(broken.matches(r#"class="tile tile--down""#).count() > healthy);
        assert!(broken.contains(r#"tile__state--down">down</span>"#));
    }

    #[test]
    fn an_empty_launcher_says_why() {
        let none = home_page(
            &Catalog::default(),
            &Icons::empty(),
            &HomeContext {
                hidden_services: 3,
                ..ctx()
            },
        );
        assert!(none.contains("No service available") && !none.contains("portal.name"));
    }

    #[test]
    fn the_message_page_has_one_way_back_to_the_launcher() {
        let page = message_page("Page not found", "Nothing lives here.", "Back to panel");
        assert!(page.contains("<h1>Page not found</h1>"));
        assert!(page.contains(r#"<a class="button button--primary" href="/">Back to panel</a>"#));
        assert!(!page.contains("appBrand"));
        assert!(message_page("<x>", "<y>", "<z>").contains("&lt;x&gt;"));
    }

    #[test]
    fn service_text_is_escaped() {
        let catalog = Catalog::build(
            vec![service(
                r#"<img src=x onerror=alert(1)>"#,
                r#""><script>alert(1)</script>"#,
                "/a?b=1&c=2",
            )],
            vec!["<b>docker</b>: unreachable".into()],
        );
        let page = home_page(&catalog, &Icons::empty(), &ctx());
        assert!(!page.contains("<img src=x"));
        assert!(!page.contains("<script>alert"));
        assert!(!page.contains("<b>docker</b>"));
        assert!(page.contains("&lt;img src=x onerror=alert(1)&gt;"));
        assert!(page.contains(r#"href="/a?b=1&amp;c=2""#));
    }

    #[test]
    fn external_links_open_in_a_new_tab_safely_and_local_ones_do_not() {
        let mut external = service("Docs", "", "https://example.org/docs");
        external.new_tab = true;
        let page = home_page(
            &Catalog::build(vec![external, service("Local", "", "/local/")], Vec::new()),
            &Icons::empty(),
            &ctx(),
        );
        assert_eq!(
            page.matches(r#"target="_blank" rel="noopener noreferrer""#)
                .count(),
            1
        );
    }

    #[test]
    fn empty_catalog_explains_how_to_populate_it() {
        let page = home_page(&Catalog::default(), &Icons::empty(), &ctx());
        assert!(page.contains("No services yet"));
        assert!(page.contains("portal.name"));
        assert!(!page.contains(r#"class="rows""#));
    }

    #[test]
    fn problems_are_shown_and_capped() {
        let problems: Vec<String> = (1..=5).map(|i| format!("source {i} unreachable")).collect();
        let page = home_page(
            &Catalog::build(Vec::new(), problems),
            &Icons::empty(),
            &ctx(),
        );
        assert!(page.contains("source 3 unreachable"));
        assert!(!page.contains("source 4 unreachable"));
        assert!(page.contains("and 2 more"));
    }

    #[test]
    fn pages_reference_only_local_resources() {
        for page in [
            home_page(&demo_catalog(), &Icons::empty(), &ctx()),
            auth_document("Authentication", "<h1>x</h1>"),
        ] {
            assert!(!page.contains("src=\"http"), "no remote script or image");
            assert!(
                !page.contains("href=\"http"),
                "no remote stylesheet or link"
            );
            assert!(!page.contains("<style"), "no inline style (CSP)");
            assert!(!page.contains(" style="), "no style attribute (CSP)");
            assert!(page.contains(r#"<link rel="stylesheet" href="/auth/style.css">"#));
        }
    }

    #[test]
    fn tiles_use_the_declared_icon_then_one_named_like_the_service_then_the_initial() {
        let root = std::env::temp_dir().join(format!("portal-ui-icons-{}", std::process::id()));
        for file in ["di/windmill.svg", "si/grafana.svg"] {
            std::fs::create_dir_all(root.join(file).parent().unwrap()).unwrap();
            std::fs::write(root.join(file), "<svg/>").unwrap();
        }
        let icons = Icons::load(&root);
        let mut declared = service("Workflows", "", "/w/");
        declared.icon = Some("windmill.png".into());
        let catalog = Catalog::build(
            vec![
                declared,
                service("Grafana", "", "/g/"),
                service("Unknown", "", "/u/"),
            ],
            Vec::new(),
        );
        let page = home_page(&catalog, &icons, &ctx());
        assert!(page.contains(r#"<img class="tile__img" src="/auth/icons/di/windmill.svg""#));
        assert!(page.contains(
            r#"<img class="tile__img tile__img--mono" src="/auth/icons/si/grafana.svg""#
        ));
        assert!(
            page.contains(r#"aria-hidden="true">U</span>"#),
            "no icon: the initial"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn monogram_skips_symbols_and_falls_back() {
        assert_eq!(monogram("  s3 browser"), "S");
        assert_eq!(monogram("é-app"), "É");
        assert_eq!(monogram("---"), "?");
    }
}
