//! Server-rendered pages: the launcher, the authentication cards and the settings pages.
//!
//! The browser receives finished HTML. Every page has the theme selector as its first script. The
//! settings pages add `settings.js`, and a page of an account that receives notifications adds
//! `notify.js`. Everything works without JavaScript. No page references anything outside this binary. The server serves the CSS and the scripts from `/auth/`.
//! The server puts the icon sprite into the HTML, reduced to the symbols that the page draws.
//!
//! The cost of a page for the browser is a design input. A page carries only what it shows. The
//! settings are pages of their own, not a hidden block of the launcher. A page has a stylesheet with
//! only the rules that it uses. The login and error pages draw no icon at all.
//! Interface: [`home_page`], [`auth_document`], [`message_page`], [`denied_page`], the settings pages
//! of [`settings`], the embedded assets ([`CSS`], [`SETTINGS_CSS`], [`THEME_JS`], [`SETTINGS_JS`]),
//! and [`demo_page`] (synthetic data for `portal render-demo`).

pub mod access;
pub mod activity;
#[cfg(test)]
pub(crate) mod body_words;
pub mod components;
#[cfg(test)]
mod fixtures;
pub mod grant_editor;
pub mod inbox;
pub mod keys;
pub mod mask;
pub mod mfa;
pub mod minify;
pub mod notify;
pub mod settings;
pub mod signin;

use std::fmt::Write as _;

use crate::catalog::{parse_labels, Catalog, Group, Health, RuntimeHealth, Service};
use crate::icons::Icons;
use crate::util::html_escape;
pub use access::{access_page, AccessView, Accounts};
pub use activity::{activity_page, ActivityView};
pub use settings::{
    new_user_page, notice_for, services_page, user_page, Frame, GroupChip, Holder, KeyChip,
    NewUserView, ServiceActions, ServicesView, UserCard, UserView,
};

pub const CSS: &str = include_str!("../../web/portal.css");
pub const SETTINGS_CSS: &str = include_str!("../../web/settings.css");
pub const THEME_JS: &str = include_str!("../../web/theme.js");
pub const SETTINGS_JS: &str = include_str!("../../web/settings.js");
pub const NOTIFY_JS: &str = include_str!("../../web/notify.js");
/// The icon of the tab: the logo on a rounded tile of Signal Blue. Only the pages after sign-in link to it.
pub const FAVICON_SVG: &str = include_str!("../../web/favicon.svg");
const SPRITE: &str = include_str!("../../web/sprite.svg");

const MAX_LISTED_PROBLEMS: usize = 3;

/// The prefix of the neutral asset paths. The file name is a hash of the content.
const ASSET_PREFIX: &str = "/auth/a/";

/// The stylesheet and the script that every page loads, in the form that the server sends.
/// The text is minified. The path has no name of the product, and it changes when the content
/// changes. This is obscurity, not security: see `docs/login-hardening.md`.
pub struct Assets {
    pub css_path: String,
    pub css: String,
    pub js_path: String,
    pub js: String,
    /// The script of the notifications. Only a page with the inbox icon loads it.
    pub notify_path: String,
    pub notify: String,
}

/// The assets that every page loads. The server makes them once, at the first call.
pub fn assets() -> &'static Assets {
    static ASSETS: std::sync::OnceLock<Assets> = std::sync::OnceLock::new();
    ASSETS.get_or_init(|| {
        let css = minify::stylesheet(CSS);
        // The shipped script is inside the subset that the minifier accepts: a test proves it.
        // If the transform fails, the page keeps the source: a script that works is better.
        let js = minify::script(THEME_JS).unwrap_or_else(|_| THEME_JS.to_string());
        let notify = minify::script(NOTIFY_JS).unwrap_or_else(|_| NOTIFY_JS.to_string());
        Assets {
            css_path: asset_path(&css, "css"),
            js_path: asset_path(&js, "js"),
            notify_path: asset_path(&notify, "js"),
            css,
            js,
            notify,
        }
    })
}

/// The path of the tab icon: `/auth/a/<hash>.svg`. The hash follows the content, as for the other assets.
/// The server sends the file to a signed-in browser only: a visitor who is not signed in cannot use it to
/// know the product.
pub fn favicon_path() -> &'static str {
    static PATH: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    PATH.get_or_init(|| asset_path(FAVICON_SVG, "svg"))
}

/// The `link` element of the tab icon. A page after sign-in puts it in its head. A page before sign-in has the empty
/// icon, [`BLANK_ICON_LINK`], and never this one.
fn favicon_link() -> String {
    format!(
        r#"<link rel="icon" type="image/svg+xml" href="{}">"#,
        favicon_path()
    )
}

/// The `link` element of an empty tab icon. Every page that a visitor can reach before sign-in has it: the sign-in
/// pages, the cards of the second step and the status pages. The empty icon stops two things. The browser does not
/// ask for `/favicon.ico`, and it does not show an icon that it keeps from a signed-in page. The page of a visitor
/// never shows an icon, so the icon cannot tell that the product runs on this server.
const BLANK_ICON_LINK: &str = r#"<link rel="icon" href="data:,">"#;

/// `/auth/a/<16 hexadecimal digits of the SHA-256 of the content>.<extension>`.
fn asset_path(content: &str, extension: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(content.as_bytes());
    let mut name = String::with_capacity(16);
    for byte in &digest[..8] {
        let _ = write!(name, "{byte:02x}");
    }
    format!("{ASSET_PREFIX}{name}.{extension}")
}

/// The text of the 403 page. The text does not name a service. A user does not learn the name of a
/// service that is not open to them.
pub const DENIED_TEXT: &str =
    "You do not have access to this page. Ask the administrator to give your account access.";

/// The viewer of the launcher, and what the page may offer that viewer.
pub struct HomeContext<'a> {
    /// The login of the signed-in account. The login appears in the "Viewing as" banner.
    pub viewer: &'a str,
    /// The superadmin browses as `viewer`.
    pub impersonating: bool,
    pub logout_token: &'a str,
    /// The anti-CSRF token of the account actions. The banner's "Back to superadmin" action uses it.
    pub users_token: &'a str,
    /// The services that the viewer cannot open. The page does not show these services.
    pub hidden_services: usize,
    /// The target of the gear. `None`: no gear. The superadmin gets the settings. Another account
    /// that holds a permission of the inbox gets the tab Inbox: the inbox is a feature of the
    /// portal, and the launcher has no tile for it.
    pub settings: Option<&'a str>,
    /// The inbox icon next to the gear, with its popover. `None`: the viewer receives no
    /// notification, and the page has no icon.
    pub bell: Option<&'a notify::Bell<'a>>,
    /// What the tab of the browser shows.
    pub tab: TabLook<'a>,
    /// Each tile opens its service in a new tab (`ui.services_new_tab`). One value for every service.
    pub services_new_tab: bool,
}

/// What the tab of the browser shows on a page after sign-in. The name and the icon are in the head of the page. No
/// page body shows them.
#[derive(Clone, Copy, Debug)]
pub struct TabLook<'a> {
    /// The name of the instance (`server.name`). It ends the title of the page.
    pub name: &'a str,
    /// The page links the icon of the tab (`ui.favicon` is `default`).
    pub icon: bool,
}

impl TabLook<'_> {
    /// The defaults of the configuration: the name "Portal" and the icon.
    pub const DEFAULT: TabLook<'static> = TabLook {
        name: "Portal",
        icon: true,
    };

    /// The `link` element of the icon, or nothing when the icon is off.
    fn icon_link(&self) -> String {
        if self.icon {
            favicon_link()
        } else {
            String::new()
        }
    }
}

/// The launcher. It has one full-width row for each group and one tile for each service.
///
/// `catalog` holds only the services that the viewer can open. A row has a head with the name of
/// the group, its count and its badges. When the viewer sees one group only, the head is not
/// necessary: the page omits it, unless the group has a down or an unhealthy service. The section
/// then keeps the name of the group as its accessible name.
pub fn home_page(catalog: &Catalog, icons: &Icons, ctx: &HomeContext) -> String {
    let mut body = String::new();
    body.push_str(&header(ctx));

    if !catalog.problems().is_empty() {
        body.push_str(&problems_notice(catalog.problems()));
    }

    // The notice has the space below it, so the page under it has no space above it.
    let empty = if catalog.problems().is_empty() {
        "empty"
    } else {
        "empty empty--after"
    };
    if catalog.groups().is_empty() && ctx.hidden_services > 0 {
        let _ = write!(
            body,
            r#"<main class="{empty}"><h1>No service available</h1><p>No service is open to your account. Ask the administrator.</p></main>"#,
        );
    } else if catalog.groups().is_empty() {
        let _ = write!(
            body,
            r#"<main class="{empty}"><h1>No services yet</h1><p>Add the catalogue labels to a container or an IngressRoute and it appears here.</p></main>"#,
        );
    } else {
        body.push_str(r#"<main class="rows"><h1 class="visually-hidden">Services</h1>"#);
        let single = catalog.groups().len() == 1;
        for (index, group) in catalog.groups().iter().enumerate() {
            group_row(&mut body, index, group, icons, single, ctx.services_new_tab);
        }
        body.push_str("</main>");
    }
    let mut icons = vec!["i-device-desktop", "i-moon", "i-sun", "i-logout"];
    if ctx.settings.is_some() {
        icons.push("i-settings");
    }
    if catalog
        .groups()
        .iter()
        .any(|g| g.services.iter().any(|s| s.external))
    {
        icons.push("i-arrow-up-right");
    }
    if let Some(bell) = ctx.bell {
        // The toasts and the script come after the page. The script loads once the page is read.
        body.push_str(&notify::toasts(&bell.header));
        let _ = write!(
            body,
            r#"<script src="{}" defer></script>"#,
            assets().notify_path
        );
        icons.extend([
            "i-inbox",
            "i-x",
            "i-alert-triangle",
            "i-bell",
            "i-bell-ringing",
        ]);
    }
    document(ctx.tab.name, &ctx.tab.icon_link(), &icons, &body)
}

/// Wraps an authentication card (`<h1>`, text, form) in the shared shell.
pub fn auth_document(title: &str, card_html: &str) -> String {
    let body = format!(r#"<main class="authShell"><div class="card">{card_html}</div></main>"#);
    document(title, BLANK_ICON_LINK, &[], &body)
}

/// The shell of the sign-in page and of the card of the second step. The card has its own class, so none of the
/// rules of the other cards applies to it. `icons` names the sprite symbols that the card draws. The title is
/// always "Authentication": a page title that changed would tell which step a person is on.
fn sign_in_document(card_html: &str, icons: &[&str], with_strip: bool) -> String {
    // A card with a strip is higher than a card without one. The class makes room for the strip above the card, so
    // the fields stay at the same place when a refusal gives the page again.
    let class = if with_strip {
        "signin signin--strip"
    } else {
        "signin"
    };
    let body = format!(r#"<main class="authShell"><div class="{class}">{card_html}</div></main>"#);
    document("Authentication", BLANK_ICON_LINK, icons, &body)
}

/// A card with a title, a sentence and one button back to the launcher. The 404 and the 403 pages
/// use this card.
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

/// The 403 page. It names the service when the service is known. It tells the visitor what to do. It
/// lets the visitor sign out, because the account that the visitor holds can be the wrong account.
pub fn denied_page(text: &str, viewer: &str, logout_token: &str) -> String {
    auth_document(
        "Access denied",
        &format!(
            r#"<h1>Access denied</h1><p>{}</p><div class="authActions"><a class="button button--primary" href="/">Back to services</a><form method="post" action="/auth/logout"><input type="hidden" name="csrf" value="{}"><button class="button" type="submit">Sign out</button></form></div><p class="hint">Signed in as <strong>{}</strong>.</p>"#,
            html_escape(text),
            html_escape(logout_token),
            html_escape(viewer)
        ),
    )
}

/// A complete page. `head` holds the extra stylesheet of a page that has one. `icons` names the
/// sprite symbols that the page draws.
fn document(title: &str, head: &str, icons: &[&str], body: &str) -> String {
    document_class(title, head, icons, "", body)
}

/// The same, with a class on the `body` element. The settings pages use `shell`: the page is as
/// high as the window, and only its `main` element scrolls.
fn document_class(title: &str, head: &str, icons: &[&str], class: &str, body: &str) -> String {
    format!(
        r#"<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><meta name="color-scheme" content="dark light"><meta name="robots" content="noindex,nofollow"><title>{}</title><link rel="stylesheet" href="{}">{head}<script src="{}"></script></head><body{}>{}{body}</body></html>"#,
        html_escape(title),
        assets().css_path,
        assets().js_path,
        if class.is_empty() {
            String::new()
        } else {
            format!(r#" class="{class}""#)
        },
        sprite(icons),
    )
}

/// Returns the inline sprite, reduced to `ids`. The sprite file holds one `<symbol>` on each line.
/// The result is empty when the page draws no icon.
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

/// A state as one icon. The word stays in the page for a screen reader and in the tooltip.
/// `tone` is `ok`, `warn` or `error`; `icon` is a symbol of the sprite.
pub fn status_icon(tone: &str, icon: &str, word: &str) -> String {
    format!(
        r##"<span class="statusIcon statusIcon--{tone}" title="{word}"><svg class="icon" aria-hidden="true"><use href="#i-{icon}"/></svg><span class="visually-hidden">{word}</span></span>"##
    )
}

/// The theme selector of ClickHouse Dash: one icon button that shows the current mode, and a list of
/// the modes under it. The script hides the current mode in the list. The page has no script state in
/// the markup: the script sets the mode, `aria-selected` and the open list. Without script, the stylesheet hides the whole selector.
fn theme_switch() -> String {
    let modes = [
        ("system", "System theme", "i-device-desktop"),
        ("dark", "Dark theme", "i-moon"),
        ("light", "Light theme", "i-sun"),
    ];
    let mut out = String::from(
        r#"<div class="themeSelect" data-theme-switch><button class="themeSelect__button" type="button" aria-haspopup="listbox" aria-expanded="false" aria-label="Theme" title="Theme">"#,
    );
    for (value, _, icon) in modes {
        let _ = write!(
            out,
            r##"<svg class="icon themeNow themeNow--{value}" aria-hidden="true"><use href="#{icon}"/></svg>"##
        );
    }
    out.push_str(
        r#"</button><div class="themeSelect__menu" role="listbox" aria-label="Theme" hidden>"#,
    );
    for (value, label, icon) in modes {
        let _ = write!(
            out,
            r##"<button type="button" tabindex="-1" class="themeSelect__option" role="option" data-theme-value="{value}" aria-selected="false" aria-label="{label}" title="{label}"><svg class="icon" aria-hidden="true"><use href="#{icon}"/></svg></button>"##
        );
    }
    out.push_str("</div></div>");
    out
}

/// A form, not a link. The sign-out action is a POST request. It ends the session at once, with no
/// page between.
fn sign_out_form(token: &str) -> String {
    format!(
        r##"<form method="post" action="/auth/logout"><input type="hidden" name="csrf" value="{}"><button class="button" type="submit"><svg class="icon" aria-hidden="true"><use href="#i-logout"/></svg>Sign out</button></form>"##,
        html_escape(token)
    )
}

/// The same form for the header of the settings pages. The words sit in a `span` that the
/// stylesheet hides on a narrow screen. The button keeps its name for a screen reader.
fn sign_out_form_short(token: &str) -> String {
    format!(
        r##"<form method="post" action="/auth/logout"><input type="hidden" name="csrf" value="{}"><button class="button" type="submit" aria-label="Sign out" title="Sign out"><svg class="icon" aria-hidden="true"><use href="#i-logout"/></svg><span class="btnText">Sign out</span></button></form>"##,
        html_escape(token)
    )
}

fn header(ctx: &HomeContext) -> String {
    let mut out = String::new();
    out.push_str(r#"<header class="appHeader"><div class="appHeader__right">"#);
    if let Some(bell) = ctx.bell {
        out.push_str(&notify::bell(bell));
    }
    if let Some(target) = ctx.settings {
        let _ = write!(
            out,
            r##"<a class="button button--icon" href="{}" aria-label="Settings" title="Settings"><svg class="icon" aria-hidden="true"><use href="#i-settings"/></svg></a>"##,
            html_escape(target)
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

/// One row. `single` tells that the row is the only row of the page. A single row without a
/// down or an unhealthy service has no head.
fn group_row(
    out: &mut String,
    index: usize,
    group: &Group,
    icons: &Icons,
    single: bool,
    new_tab: bool,
) {
    let count = group.services.len();
    let down = group.down_count();
    let unhealthy = group.unhealthy_count();
    if single && down == 0 && unhealthy == 0 {
        let _ = write!(
            out,
            r#"<section class="group" aria-label="{}">"#,
            html_escape(&group.name)
        );
    } else {
        let _ = write!(
            out,
            r#"<section class="group" aria-labelledby="g-{index}"><div class="group__head"><h2 class="group__title" id="g-{index}">{}</h2><span class="group__count">{count} {}</span>"#,
            html_escape(&group.name),
            if count == 1 { "service" } else { "services" },
        );
        if down > 0 {
            let _ = write!(
                out,
                r#"<span class="badge badge--danger">{down} down</span>"#
            );
        }
        if unhealthy > 0 {
            let _ = write!(
                out,
                r#"<span class="badge badge--warn">{unhealthy} unhealthy</span>"#
            );
        }
        out.push_str("</div>");
    }
    out.push_str(r#"<ul class="tiles">"#);
    for service in &group.services {
        tile(out, service, icons, new_tab);
    }
    out.push_str("</ul></section>");
}

/// The state word of a tile, and the modifier of its class. A down service shows `down`. A
/// service that runs shows the result of the runtime health check when the runtime has one,
/// else `up`. A healthy service has the same green dot as `up`. The word is always present:
/// the colour is never the only sign of the state.
fn tile_state(service: &Service) -> Option<(&'static str, &'static str)> {
    match (service.health, service.runtime) {
        (Health::Down, _) => Some(("down", "down")),
        (_, Some(RuntimeHealth::Healthy)) => Some(("up", "healthy")),
        (_, Some(RuntimeHealth::Starting)) => Some(("starting", "starting")),
        (_, Some(RuntimeHealth::Unhealthy)) => Some(("unhealthy", "unhealthy")),
        (Health::Up, None) => Some(("up", "up")),
        (Health::Unknown, None) => None,
    }
}

/// The tooltip of the badge "external". The Services page says the same.
const EXTERNAL_NOTE: &str = "External link. This server does not protect it.";

fn tile(out: &mut String, service: &Service, icons: &Icons, new_tab: bool) {
    let target = if new_tab {
        r#" target="_blank" rel="noopener noreferrer""#
    } else {
        ""
    };
    let title = if service.description.is_empty() {
        String::new()
    } else {
        format!(r#" title="{}""#, html_escape(&service.description))
    };
    // An external service has no health. Its mark is the badge "external": it tells that the portal does not
    // protect the link.
    let state = if service.external {
        format!(
            r##"<span class="tile__state tile__state--external" title="{EXTERNAL_NOTE}">{}external</span>"##,
            r##"<svg class="icon icon--sm" aria-hidden="true"><use href="#i-arrow-up-right"/></svg>"##
        )
    } else {
        tile_state(service)
            .map(|(modifier, word)| {
                format!(r#"<span class="tile__state tile__state--{modifier}">{word}</span>"#)
            })
            .unwrap_or_default()
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

/// The tile icon. It uses the declared icon first. If there is none, it uses an icon named like the
/// service. Otherwise, it uses the initial of the service.
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

/// The placeholder glyph for a service whose icon is not in the library. The glyph is the first
/// letter or digit of the service name.
fn monogram(name: &str) -> String {
    name.chars()
        .find(|c| c.is_alphanumeric())
        .map(|c| c.to_uppercase().collect())
        .unwrap_or_else(|| "?".to_string())
}

/// Synthetic services for `portal render-demo` and the tests. They are parsed through the real label
/// parser. The demo also tests that parser. The demo shows each state of a tile: `up`, `healthy`,
/// `unhealthy`, `starting`, `down`, and no state.
pub fn demo_catalog() -> Catalog {
    type Row<'a> = (
        &'a str,
        &'a [(&'a str, &'a str)],
        Health,
        Option<RuntimeHealth>,
    );
    let rows: [Row; 12] = [
        (
            "notes",
            &[
                ("portal.group", "Infra"),
                ("portal.name", "Notes"),
                ("portal.description", "Workflow automation and scripts"),
                ("portal.href", "/notes/"),
                ("portal.weight", "40"),
            ],
            Health::Up,
            None,
        ),
        (
            "registry-ui",
            &[
                ("portal.group", "Infra"),
                ("portal.name", "Registry"),
                ("portal.description", "OCI registry browser"),
                ("portal.href", "/registry/"),
                ("portal.weight", "20"),
            ],
            Health::Up,
            Some(RuntimeHealth::Healthy),
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
            Some(RuntimeHealth::Healthy),
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
            None,
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
            None,
        ),
        (
            "grafana",
            &[
                ("portal.group", "Monitoring"),
                ("portal.name", "Grafana"),
                ("portal.description", "Dashboards and alerts"),
                ("portal.href", "/grafana/"),
                ("portal.weight", "10"),
            ],
            Health::Down,
            None,
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
            Some(RuntimeHealth::Unhealthy),
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
            Some(RuntimeHealth::Starting),
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
            None,
        ),
        (
            "clickhouse-dash",
            &[
                ("portal.group", "Data"),
                ("portal.name", "ClickHouse Dash"),
                ("portal.description", "Query and explore ClickHouse"),
                ("portal.href", "/clickhouse-dash/"),
                ("portal.weight", "10"),
            ],
            Health::Up,
            Some(RuntimeHealth::Healthy),
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
            None,
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
            None,
        ),
    ];
    let mut services = Vec::new();
    for (origin, labels, health, runtime) in rows {
        let map = labels
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        if let Ok(Some(mut service)) = parse_labels(origin, &map) {
            service.health = health;
            service.runtime = runtime;
            services.push(service);
        }
    }
    Catalog::build(services, Vec::new())
}

/// The document of the demo service that publishes actions: a host, and the databases of a host.
const DEMO_DOCUMENT: &str = r#"{"server":"data-clickhouse-dash","resource_types":[{"name":"host","parent":"service","description":"One ClickHouse server."},{"name":"database","parent":"host","description":"One database of a host."}],"actions":[{"name":"query.run.read","resource_types":["service","host","database"],"description":"Run read-only queries."},{"name":"query.run.write","resource_types":["service","host","database"],"description":"Run queries that change data."},{"name":"system.read","resource_types":["service","host"],"description":"See the System page."}]}"#;

/// The actions of the demo services. One service publishes actions and a tree of types. The
/// other services have only `access`.
fn demo_services(catalog: &Catalog) -> Vec<ServiceActions> {
    catalog
        .groups()
        .iter()
        .flat_map(|g| g.services.iter())
        .map(|service| {
            let document = if service.id == "data-clickhouse-dash" {
                crate::actions::parse(&service.id, DEMO_DOCUMENT.as_bytes()).unwrap_or_default()
            } else {
                crate::actions::Document::default()
            };
            ServiceActions {
                external: false,
                id: service.id.clone(),
                name: service.name.clone(),
                actions: document
                    .actions
                    .iter()
                    .map(|a| (a.name.clone(), a.description.clone()))
                    .collect(),
                document: std::sync::Arc::new(document),
                known: true,
                fetch: false,
                critical: Vec::new(),
                source: crate::actions::Source::Absent,
            }
        })
        .collect()
}

/// Synthetic accounts and history for `portal render-demo`. The data that the view borrows is in
/// `DemoData`.
struct DemoData {
    logins: crate::store::LoginPage,
    days: Vec<crate::store::DayStat>,
    events: Vec<crate::store::AuditRow>,
}

impl DemoData {
    fn new() -> Self {
        Self {
            logins: demo_logins(),
            days: demo_days(3),
            events: demo_events(),
        }
    }
}

/// The features of the portal in the demo: the keys of both kinds and three groups of the inbox.
fn demo_features() -> ServiceActions {
    use crate::keys::KeyKind;
    use crate::portal_actions::{document, Features};
    let features = Features {
        inbox_groups: Some(vec![
            ("infra".into(), "Infra".into()),
            ("database".into(), "Database".into()),
            ("oncall".into(), "On call".into()),
        ]),
        kinds: vec![KeyKind::Api, KeyKind::Mcp],
    };
    let document = document(&features);
    ServiceActions {
        external: false,
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
        critical: Vec::new(),
        source: crate::actions::Source::Absent,
    }
}

/// The groups of the demo for the chips of the forms of an account.
fn demo_group_chips() -> Vec<GroupChip> {
    [("infra", "Infra", false, 4), ("oncall", "On call", true, 2)]
        .iter()
        .map(|(id, name, critical, members)| GroupChip {
            id: id.to_string(),
            name: name.to_string(),
            critical: *critical,
            members: *members,
        })
        .collect()
}

fn demo_users<'a>(catalog: &'a Catalog, panel_only: bool) -> Accounts<'a> {
    use crate::store::{AccountActivity, ServiceUse, SessionRow};
    let now = 1_791_623_502_i64;
    let days = demo_days;
    let card = |login: &str, grants: &[&str], seed: u32, superadmin: bool, file: bool| {
        UserCard {
        login: login.into(),
        grants: grants.iter().map(|g| crate::grants::Grant::parse(g).unwrap()).collect(),
        configured: if file { grants.iter().map(|g| crate::grants::Grant::parse(g).unwrap()).collect() } else { Vec::new() },
        created_at: (!superadmin && !file).then_some(now - 20 * 86_400),
        superadmin,
        file,
        keys: if superadmin { 0 } else { seed as usize % 3 },
        groups: if panel_only || superadmin || seed % 2 == 1 {
            Vec::new()
        } else {
            vec!["infra".to_string()]
        },
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
                    remembered: false,
                    mfa: false,
                },
                SessionRow {
                    started_at: now - 90_000,
                    last_seen_at: now - 86_000,
                    ended_at: Some(now - 86_400),
                    end_reason: Some("logout".into()),
                    ip: Some("192.168.1.31".into()),
                    user_agent: "Mozilla/5.0 (iPhone; CPU iPhone OS 17_0 like Mac OS X) AppleWebKit/605.1.15 Safari/604.1".into(),
                    impersonated: false,
                    remembered: false,
                    mfa: false,
                },
            ],
            services: vec![
                ServiceUse { service_id: "infra-notes".into(), hits: 412, last_at: now - 120 },
                ServiceUse { service_id: "monitoring-grafana".into(), hits: 96, last_at: now - 5_000 },
            ],
            sessions_30d: 18,
            failed_30d: u32::from(login == "root"),
            last_login: Some(now - 3_600),
        },
    }
    };
    let portal = |grants: &[&str]| -> Vec<String> {
        if panel_only {
            Vec::new()
        } else {
            grants.iter().map(|g| g.to_string()).collect()
        }
    };
    let alice_portal = portal(&[
        "keys.own.read on service:portal",
        "keys.own.edit on service:portal",
        "inbox.receive on service:portal/inbox_group:infra",
        "alert.send on service:portal/inbox_group:infra",
    ]);
    let mut alice_grants: Vec<&str> = vec![
        "access on service:infra-notes",
        "access on service:data-clickhouse-dash",
        "query.run.read on service:data-clickhouse-dash",
        "query.run.write on service:data-clickhouse-dash/host:local/database:analytics",
    ];
    alice_grants.extend(alice_portal.iter().map(String::as_str));
    Accounts {
        allow_creation: !panel_only,
        mode: crate::grants::Mode::Dynamic,
        catalog,
        services: demo_services(catalog),
        users: vec![
            card("alice", &alice_grants, 2, false, false),
            card(
                "bob",
                &[
                    "access on service:data-clickhouse-dash",
                    "query.run.read on service:data-clickhouse-dash/host:lyon",
                    "access on service:monitoring-grafana",
                ],
                4,
                false,
                false,
            ),
            card("carol", &[], 5, false, false),
            card("dave", &["access on service:infra-notes"], 7, false, true),
        ],
        features: (!panel_only).then(demo_features),
        max_users: 64,
        token: "demo",
        two_factor: Some(["alice".to_string()].into_iter().collect()),
    }
}

/// The keys of the demo: one of each kind, and a key of the superadmin.
fn demo_keys() -> keys::KeysList {
    use crate::keys::KeyKind;
    let now = 1_791_623_502_i64;
    let line = |id: &str, owner: Option<&str>, kind: KeyKind, grants: &[&str]| keys::KeyLine {
        id: id.into(),
        owner: owner.map(str::to_string),
        mine: owner.is_none(),
        name: format!("{id}-key"),
        hint: "AbCd".into(),
        kind: Some(kind),
        state: keys::KeyState::Active,
        file: false,
        created_at: now - 86_400,
        expires_at: None,
        last_check_at: Some(now - 600),
        check_count: 4,
        revoked_at: None,
        grants: grants
            .iter()
            .map(|g| keys::GrantLine {
                grant: crate::grants::Grant::parse(g).unwrap(),
                cover: keys::Cover::Full,
                imposed: false,
            })
            .collect(),
        no_access: Vec::new(),
        interaction: Some(crate::keys::Interaction::default_for(kind)),
        can_change: owner.is_none(),
    };
    keys::KeysList {
        lines: vec![
            line(
                "ci",
                Some("alice"),
                KeyKind::Api,
                &["alert.send on service:portal/inbox_group:infra"],
            ),
            line(
                "agent",
                Some("alice"),
                KeyKind::Mcp,
                &["access on service:infra-notes"],
            ),
            line("root", None, KeyKind::Api, &[]),
        ],
        truncated: false,
        all: true,
        create_kinds: vec![KeyKind::Api, KeyKind::Mcp],
        can_revoke: true,
        can_revoke_all: true,
        impersonated: false,
        cannot_use: false,
        groups: vec![("infra".into(), "Infra".into())],
        csrf: "demo".into(),
    }
}

/// The days of the demo graphs: a fixed pattern, so a page renders the same each time.
fn demo_days(seed: u32) -> Vec<crate::store::DayStat> {
    (0..30u32)
        .map(|i| {
            let sessions = (i * 7 + seed) % 5;
            crate::store::DayStat {
                sessions,
                seconds: i64::from(sessions) * 1_800,
            }
        })
        .collect()
}

/// Synthetic audit events for `portal render-demo`.
fn demo_events() -> Vec<crate::store::AuditRow> {
    use crate::store::AuditRow;
    let now = 1_791_623_502_i64;
    vec![
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
    ]
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
        total: 6,
        refused: 4,
    }
}

/// The pages of `portal render-demo`.
pub const DEMO_PAGES: [&str; 10] = [
    "launcher",
    "access",
    "access-panel-only",
    "activity",
    "activity-panel-only",
    "user",
    "new-user",
    "services",
    "services-panel-only",
    "services-empty",
];

/// A page of `portal render-demo`. The page is the launcher or one settings page, with synthetic data.
/// The function returns `None` for a name that is not a page.
pub fn demo_page(name: &str, icons: &Icons) -> Option<String> {
    let catalog = demo_catalog();
    let data = DemoData::new();
    let panel_only = name.ends_with("panel-only");
    let frame = Frame {
        logout_token: "demo",
        keys: !panel_only,
        inbox: !panel_only,
        superadmin: true,
        notify: None,
        security: false,
        tab: TabLook::DEFAULT,
    };
    let holders = |users: &Accounts| -> Vec<Holder> {
        users
            .users
            .iter()
            .map(|u| Holder {
                login: u.login.clone(),
                grants: u.grants.clone(),
                file: u.file,
            })
            .collect()
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
                settings: Some("/auth/settings"),
                bell: None,
                tab: TabLook::DEFAULT,
                services_new_tab: false,
            },
        ),
        "access" | "access-panel-only" => access_page(
            &frame,
            &AccessView {
                notice: None,
                query: access::Query::default(),
                accounts: Some(demo_users(&catalog, panel_only)),
                keys: (!panel_only).then(demo_keys),
                groups: if panel_only {
                    Vec::new()
                } else {
                    vec![
                        access::GroupLine {
                            id: "infra".into(),
                            name: "Infra".into(),
                            critical: false,
                        },
                        access::GroupLine {
                            id: "oncall".into(),
                            name: "On call".into(),
                            critical: true,
                        },
                    ]
                },
            },
        ),
        "activity" | "activity-panel-only" => activity_page(
            &frame,
            &ActivityView {
                notice: None,
                logins: &data.logins,
                changes: false,
                only_refused: false,
                days: &data.days,
                first_day: 1_791_244_800,
                recent_events: &data.events,
            },
        ),
        "user" => {
            let mut users = demo_users(&catalog, false);
            user_page(
                &frame,
                &UserView {
                    notice: None,
                    mode: users.mode,
                    catalog: &catalog,
                    services: std::mem::take(&mut users.services),
                    user: users.users.remove(0),
                    token: "demo",
                    features: users.features.take(),
                    owned_keys: 2,
                    groups: demo_group_chips(),
                    keys: vec![
                        KeyChip {
                            name: "ci".into(),
                            kind: crate::keys::KeyKind::Api,
                        },
                        KeyChip {
                            name: "agent".into(),
                            kind: crate::keys::KeyKind::Mcp,
                        },
                    ],
                    two_factor: Some(true),
                },
            )
        }
        "new-user" => {
            let mut users = demo_users(&catalog, false);
            new_user_page(
                &frame,
                &NewUserView {
                    notice: None,
                    field: None,
                    groups: demo_group_chips(),
                    services: std::mem::take(&mut users.services),
                    features: users.features.take(),
                    token: "demo",
                    refused: None,
                },
            )
        }
        "services" | "services-panel-only" => {
            let users = demo_users(&catalog, panel_only);
            let holders = holders(&users);
            services_page(
                &frame,
                &ServicesView {
                    catalog: &catalog,
                    services: &users.services,
                    holders: &holders,
                    problems: vec![(
                        "infra-s3-browser".into(),
                        "http://s3-browser:8080/.well-known/procurauth/actions gave the status 500"
                            .into(),
                    )],
                    mode: users.mode,
                    selected: None,
                    only_check: false,
                    checks: false,
                    notice: None,
                    token: "demo",
                    can_add: !panel_only,
                },
            )
        }
        "services-empty" => {
            let empty = Catalog::build(Vec::new(), Vec::new());
            services_page(
                &frame,
                &ServicesView {
                    catalog: &empty,
                    services: &[],
                    holders: &[],
                    problems: Vec::new(),
                    mode: crate::grants::Mode::Dynamic,
                    selected: None,
                    only_check: false,
                    checks: false,
                    notice: None,
                    token: "demo",
                    can_add: false,
                },
            )
        }
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_theme_selector_is_a_list_of_three_modes_that_starts_closed() {
        let html = theme_switch();
        assert_eq!(html.matches(r#"aria-haspopup="listbox""#).count(), 1);
        assert_eq!(html.matches("data-theme-value=").count(), 3);
        assert_eq!(html.matches("themeNow--").count(), 3);
        assert!(
            html.contains(r#"aria-expanded="false""#) && !html.contains(r#"aria-expanded="true""#)
        );
        assert!(html.contains(r#"role="listbox" aria-label="Theme" hidden>"#));
        assert_eq!(html.matches(r#"role="option""#).count(), 3);
        assert_eq!(html.matches(r#"aria-selected="false""#).count(), 3);
        assert!(!html.contains("aria-pressed"));
    }

    #[test]
    fn the_assets_have_neutral_paths_no_comment_and_no_name_of_the_product() {
        let assets = assets();
        for (path, content) in [
            (&assets.css_path, &assets.css),
            (&assets.js_path, &assets.js),
        ] {
            assert!(path.starts_with("/auth/a/") && !path.to_lowercase().contains("portal"));
            assert!(!content.to_lowercase().contains("portal"), "{path}");
            assert!(!content.contains("/*") && !content.contains("//"), "{path}");
            assert!(!content.contains("ClickHouse"), "{path}");
        }
        assert!(assets.css.len() < CSS.len());
        assert!(assets.js.len() < THEME_JS.len() * 3 / 4);
        // The same content gives the same path, on any instance.
        assert_eq!(assets.css_path, asset_path(&assets.css, "css"));
        // The script is not the source: the minifier accepted it, and it renamed a local name.
        assert!(!assets.js.contains("buttons"));
    }

    #[test]
    fn the_notify_script_is_inside_the_subset_and_has_no_inline_or_dynamic_code() {
        let minified = minify::script(NOTIFY_JS).expect("the minifier accepts the script");
        assert!(minified.len() < NOTIFY_JS.len() * 3 / 4);
        assert!(!minified.contains("/*") && !minified.contains("//"));
        for banned in [
            "eval(",
            "innerHTML",
            "document.write",
            "Function(",
            "setTimeout(\"",
        ] {
            assert!(!NOTIFY_JS.contains(banned), "{banned}");
        }
        // The script uses attributes to find its elements: no class name goes through the pass.
        assert!(!NOTIFY_JS.contains("classList") && !NOTIFY_JS.contains("className"));
        assert!(!NOTIFY_JS.contains("\"js\""));
        let plain = assets();
        assert_eq!(plain.notify_path, asset_path(&plain.notify, "js"));
        assert!(plain.notify_path.starts_with("/auth/a/"));
        // The same script runs for a masked page: the pass changes no string of it.
        let mask = mask::Mask::new(b"a secret of the tests").unwrap();
        assert_eq!(mask.assets().notify, plain.notify);
    }

    #[test]
    fn the_notify_script_passes_a_syntax_check_when_node_is_present() {
        let dir = std::env::temp_dir().join(format!("notify-script-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("script.js");
        std::fs::write(&file, &assets().notify).unwrap();
        match std::process::Command::new("node")
            .arg("--check")
            .arg(&file)
            .output()
        {
            Ok(output) => assert!(
                output.status.success(),
                "node --check: {}",
                String::from_utf8_lossy(&output.stderr)
            ),
            Err(_) => eprintln!("node is not in this image: the syntax check is skipped"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_served_script_passes_a_syntax_check_when_node_is_present() {
        let dir = std::env::temp_dir().join(format!("assets-script-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("script.js");
        std::fs::write(&file, &assets().js).unwrap();
        match std::process::Command::new("node")
            .arg("--check")
            .arg(&file)
            .output()
        {
            Ok(output) => assert!(
                output.status.success(),
                "node --check: {}",
                String::from_utf8_lossy(&output.stderr)
            ),
            Err(_) => eprintln!("node is not in this image: the syntax check is skipped"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
    use std::collections::BTreeMap;

    fn ctx<'a>() -> HomeContext<'a> {
        HomeContext {
            viewer: "alice",
            impersonating: false,
            logout_token: "t",
            users_token: "u",
            hidden_services: 0,
            settings: None,
            bell: None,
            tab: TabLook::DEFAULT,
            services_new_tab: false,
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

    /// What a page shows when it draws the logo in its body. The list names each part: the symbols, the class of
    /// the dot and the classes of the header.
    const LOGO_PARTS: [&str; 4] = ["i-logo", "logoDot", "appMark", "appSep"];

    #[test]
    fn the_launcher_has_no_logo_in_its_body_and_links_the_tab_icon() {
        let page = home_page(&demo_catalog(), &Icons::empty(), &ctx());
        for part in LOGO_PARTS {
            assert!(!page.replace("i-logout", "").contains(part), "{part}");
        }
        // The header holds the controls only.
        assert!(page.contains(r#"<header class="appHeader"><div class="appHeader__right">"#));
        // The tab icon: a link to the same-origin hashed file.
        let link = format!(
            r#"<link rel="icon" type="image/svg+xml" href="{}">"#,
            favicon_path()
        );
        assert!(page.contains(&link), "{page}");
        assert!(favicon_path().starts_with("/auth/a/") && favicon_path().ends_with(".svg"));
        assert_eq!(page.matches("rel=\"icon\"").count(), 1);
    }

    #[test]
    fn the_tab_icon_is_a_file_of_its_own_with_the_colours_of_the_brand() {
        // The tile of Signal Blue with the mark in white and the dot in cyan.
        assert!(FAVICON_SVG.contains(r##"fill="#1238F0""##));
        assert!(FAVICON_SVG.contains(r##"fill="#7FF0FF""##));
        assert!(!FAVICON_SVG.contains("<script") && !FAVICON_SVG.contains("<style"));
        assert!(!FAVICON_SVG.to_ascii_lowercase().contains("portal"));
        // The sprite of the pages has no symbol of the logo.
        assert!(!SPRITE.replace("i-logout", "").contains("i-logo"));
    }

    #[test]
    fn no_sign_in_page_and_no_status_page_shows_the_logo_or_links_the_icon_file() {
        let pages = [
            auth_document("Authentication", "<p>x</p>"),
            message_page("Not found", "No such page.", "Back to services"),
            denied_page(DENIED_TEXT, "alice", "tok"),
            sign_in_document("<p>x</p>", &["i-lock"], false),
            sign_in_document("<p>x</p>", &["i-lock", "i-alert-triangle"], true),
        ];
        for page in &pages {
            for part in LOGO_PARTS {
                assert!(
                    !page.replace("i-logout", "").contains(part),
                    "{part}: {page}"
                );
            }
            // The only icon is the empty one: no file, no SVG, no path of the icon.
            assert_eq!(page.matches("rel=\"icon\"").count(), 1, "{page}");
            assert!(page.contains(BLANK_ICON_LINK), "{page}");
            assert!(!page.contains("image/svg+xml") && !page.contains("shortcut"));
            assert!(!page.contains(favicon_path()) && !page.contains("apple-touch"));
            // The head links the stylesheet and the empty icon.
            assert_eq!(page.matches("<link ").count(), 2, "{page}");
        }
    }

    #[test]
    fn no_body_of_the_launcher_or_of_a_status_page_names_the_product() {
        use crate::inbox::notify::{Importance, Level};
        use body_words::assert_clean;
        let rows = [notify::Row {
            id: "aa".into(),
            title: "Disk full".into(),
            group: "Infra".into(),
            importance: Importance::Alert,
            created_at_ms: 0,
            href: None,
            unread: true,
        }];
        let bell = notify::Bell {
            header: notify::Header {
                level: Level::All,
                unread: 1,
                cursor_ms: 0,
            },
            rows: &rows,
            now_ms: 0,
            csrf: "tok",
            forms: true,
        };
        let full = HomeContext {
            settings: Some("/auth/settings"),
            bell: Some(&bell),
            impersonating: true,
            ..ctx()
        };
        let empty = Catalog::build(Vec::new(), Vec::new());
        // The launcher: with services, with the gear, the bell and the banner, and the two empty states.
        assert_clean(
            "launcher",
            &home_page(&demo_catalog(), &Icons::empty(), &ctx()),
        );
        assert_clean(
            "launcher, all controls",
            &home_page(&demo_catalog(), &Icons::empty(), &full),
        );
        assert_clean(
            "launcher, no service",
            &home_page(&empty, &Icons::empty(), &ctx()),
        );
        assert_clean(
            "launcher, no service is open",
            &home_page(
                &empty,
                &Icons::empty(),
                &HomeContext {
                    hidden_services: 2,
                    ..ctx()
                },
            ),
        );
        // A name that the operator sets is not in the body.
        let named = HomeContext {
            tab: TabLook {
                name: "Prod <Zeta>",
                icon: true,
            },
            ..ctx()
        };
        let page = home_page(&demo_catalog(), &Icons::empty(), &named);
        assert!(page.contains("<title>Prod &lt;Zeta&gt;</title>"));
        assert!(!page[page.find("<body").unwrap()..].contains("Zeta"));
        // The status pages and the sign-in pages.
        assert_clean(
            "not found",
            &message_page(
                "Page not found",
                "There is nothing at this address.",
                "Back to services",
            ),
        );
        assert_clean("denied", &denied_page(DENIED_TEXT, "alice", "tok"));
        assert_clean(
            "card",
            &auth_document("Authentication", "<h1>Authenticated</h1>"),
        );
    }

    #[test]
    fn no_text_that_a_script_draws_names_the_product() {
        // A string of a script ends in the body of a page. A comment and a name of a variable do not.
        for (name, script) in [
            ("settings.js", SETTINGS_JS),
            ("notify.js", NOTIFY_JS),
            ("theme.js", THEME_JS),
        ] {
            for (number, line) in script.lines().enumerate() {
                let line = line.trim_start();
                if line.starts_with("//") || line.starts_with("/*") || line.starts_with('*') {
                    continue;
                }
                for quote in ['"', '\''] {
                    for (at, text) in line.split(quote).enumerate() {
                        assert!(
                            at % 2 == 0 || !text.to_ascii_lowercase().contains("portal"),
                            "{name}:{}: {line}",
                            number + 1
                        );
                    }
                }
            }
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
                settings: Some("/auth/settings"),
                ..ctx()
            },
        );
        assert!(page.contains(
            r##"<a class="button button--icon" href="/auth/settings" aria-label="Settings""##
        ));
        assert!(page.contains(r#"<symbol id="i-settings""#));
    }

    #[test]
    fn the_inbox_icon_sits_next_to_the_gear_with_a_popover_that_opens_without_script() {
        use crate::inbox::notify::{Importance, Level};
        let rows = [notify::Row {
            id: "aa".into(),
            title: "Disk <full>".into(),
            group: "Infra".into(),
            importance: Importance::Alert,
            created_at_ms: 0,
            href: None,
            unread: true,
        }];
        let bell = notify::Bell {
            header: notify::Header {
                level: Level::All,
                unread: 1,
                cursor_ms: 0,
            },
            rows: &rows,
            now_ms: 0,
            csrf: "tok",
            forms: true,
        };
        let page = |bell: Option<&notify::Bell>| {
            home_page(
                &demo_catalog(),
                &Icons::empty(),
                &HomeContext {
                    settings: Some("/auth/settings"),
                    bell,
                    ..ctx()
                },
            )
        };
        let html = page(Some(&bell));
        let (icon, gear) = (
            html.find("data-notify-bell").unwrap(),
            html.find(r#"aria-label="Settings""#).unwrap(),
        );
        let header = &html[html.find("<header").unwrap()..html.find("</header>").unwrap()];
        assert!(header.contains("data-notify-bell") && header.contains("Settings"));
        assert!(
            icon < gear,
            "the icon comes before the gear, in the same group"
        );
        assert!(html.contains("<details") && html.contains("<summary"));
        assert!(html.contains("Disk &lt;full&gt;") && !html.contains("Disk <full>"));
        // One more script, a file with a neutral path that the pass maps. The toasts are in the page.
        assert_eq!(html.matches("<script").count(), 2);
        assert!(html.contains(&format!(r#"src="{}" defer"#, assets().notify_path)));
        assert!(html.contains("data-notify-toast"));
        for symbol in [
            "i-inbox",
            "i-x",
            "i-alert-triangle",
            "i-bell",
            "i-bell-ringing",
        ] {
            assert!(
                html.contains(&format!(r#"<symbol id="{symbol}""#)),
                "{symbol}"
            );
        }
        // No icon and no script without the right.
        let plain = page(None);
        for gone in ["data-notify", "notify", "nbell", "<details"] {
            assert!(!plain.contains(gone), "{gone}");
        }
        assert_eq!(plain.matches("<script").count(), 1);
        // The session of "Log in as" has the list and no form.
        let read_only = notify::Bell {
            forms: false,
            ..notify::Bell {
                header: bell.header,
                rows: &rows,
                now_ms: 0,
                csrf: "tok",
                forms: true,
            }
        };
        let html = page(Some(&read_only));
        assert!(html.contains("data-notify-list") && !html.contains("Mark all read"));
        assert!(!html.contains("data-notify-level"));
    }

    #[test]
    fn the_launcher_carries_none_of_the_settings_whoever_looks_at_it() {
        let page = home_page(
            &demo_catalog(),
            &Icons::empty(),
            &HomeContext {
                settings: Some("/auth/settings"),
                ..ctx()
            },
        );
        for gone in [
            "id=\"settings\"",
            "modal",
            "r_ops",
            "userList",
            "tabInput",
            "class=\"warn\"",
        ] {
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
        assert_eq!(
            launcher.matches("<symbol ").count(),
            4,
            "no gear for a user"
        );
        assert!(!launcher.contains("i-alert-triangle") && !launcher.contains("i-chevron-left"));
        for page in [
            auth_document("Authentication", "<p>x</p>"),
            message_page("Page not found", "x", "Back"),
            denied_page("x", "alice", "t"),
        ] {
            assert!(!page.contains("<symbol") && !page.contains("class=\"sprite\""));
        }
        // A settings page inlines the symbols that it draws, and no other.
        for name in DEMO_PAGES.iter().filter(|n| **n != "launcher") {
            let settings = demo_page(name, &Icons::empty()).unwrap();
            let inlined: Vec<&str> = settings
                .match_indices(r#"<symbol id=""#)
                .map(|(at, m)| settings[at + m.len()..].split('"').next().unwrap())
                .collect();
            assert!(inlined.contains(&"i-arrow-left"), "{name}: the back arrow");
            for id in &inlined {
                assert!(
                    settings.contains(&format!(r##"href="#{id}""##)),
                    "{name}: {id} is inlined and not drawn"
                );
            }
            for (at, m) in settings.match_indices(r##"href="#i-"##) {
                let id = settings[at + m.len() - 2..].split('"').next().unwrap();
                assert!(
                    inlined.contains(&id),
                    "{name}: {id} is drawn and not inlined"
                );
            }
        }
    }

    #[test]
    fn every_demo_page_renders_and_an_unknown_one_does_not() {
        for name in DEMO_PAGES {
            let page = demo_page(name, &Icons::empty()).unwrap();
            assert!(page.starts_with("<!doctype html>"), "{name}");
        }
        assert!(demo_page("nope", &Icons::empty()).is_none());
        assert!(
            demo_page("roles", &Icons::empty()).is_none(),
            "the page does not exist"
        );
        for gone in ["logins", "service"] {
            assert!(
                demo_page(gone, &Icons::empty()).is_none(),
                "the page {gone} is removed"
            );
        }
        let services = demo_page("services", &Icons::empty()).unwrap();
        assert!(
            services.contains("statusIcon--warn"),
            "a demo service has a problem"
        );
        let users = demo_page("activity", &Icons::empty()).unwrap();
        assert!(
            users.contains("&#39;; DROP TABLE users; --") && !users.contains("<script>alert"),
            "what a visitor typed is text"
        );
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
    fn the_denied_page_names_no_service_and_offers_the_way_out() {
        let page = denied_page(DENIED_TEXT, "al<ice", "tok<en");
        assert!(page.contains("You do not have access to this page. Ask the administrator"));
        assert!(page.contains(r#"<a class="button button--primary" href="/">Back to services</a>"#));
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
    fn the_runtime_health_gives_one_state_word_and_down_wins() {
        let mut service = service("A", "", "/a/");
        let mut state = |health, runtime| {
            service.health = health;
            service.runtime = runtime;
            tile_state(&service)
        };
        use RuntimeHealth::{Healthy, Starting, Unhealthy};
        assert_eq!(state(Health::Down, None), Some(("down", "down")));
        assert_eq!(state(Health::Down, Some(Healthy)), Some(("down", "down")));
        assert_eq!(state(Health::Down, Some(Unhealthy)), Some(("down", "down")));
        assert_eq!(state(Health::Up, Some(Healthy)), Some(("up", "healthy")));
        assert_eq!(
            state(Health::Up, Some(Starting)),
            Some(("starting", "starting"))
        );
        assert_eq!(
            state(Health::Up, Some(Unhealthy)),
            Some(("unhealthy", "unhealthy"))
        );
        assert_eq!(state(Health::Up, None), Some(("up", "up")));
        assert_eq!(
            state(Health::Unknown, Some(Unhealthy)),
            Some(("unhealthy", "unhealthy"))
        );
        assert_eq!(
            state(Health::Unknown, Some(Healthy)),
            Some(("up", "healthy"))
        );
        assert_eq!(state(Health::Unknown, None), None);
    }

    #[test]
    fn the_demo_shows_each_state_and_a_warning_badge_for_the_unhealthy_services() {
        let page = home_page(&demo_catalog(), &Icons::empty(), &ctx());
        for state in [
            r#"tile__state--up">up</span>"#,
            r#"tile__state--up">healthy</span>"#,
            r#"tile__state--unhealthy">unhealthy</span>"#,
            r#"tile__state--starting">starting</span>"#,
            r#"tile__state--down">down</span>"#,
        ] {
            assert!(page.contains(state), "{state}");
        }
        assert_eq!(
            page.matches(r#"<span class="badge badge--warn">1 unhealthy</span>"#)
                .count(),
            1
        );
        assert_eq!(
            page.matches(r#"class="tile tile--down""#).count(),
            1,
            "an unhealthy tile keeps the plain tile"
        );
    }

    fn in_group(group: &str, name: &str, href: &str) -> Service {
        Service {
            group: group.into(),
            ..service(name, "", href)
        }
    }

    #[test]
    fn a_single_group_has_no_head_unless_it_has_something_to_report() {
        let one = Catalog::build(
            vec![in_group("Infra", "A", "/a/"), in_group("Infra", "B", "/b/")],
            Vec::new(),
        );
        let page = home_page(&one, &Icons::empty(), &ctx());
        assert!(!page.contains("group__head") && !page.contains("group__title"));
        assert!(page.contains(r#"<section class="group" aria-label="Infra"><ul class="tiles">"#));
        assert!(page.contains(r#"<h1 class="visually-hidden">Services</h1>"#));
        assert_eq!(page.matches(r#"<a class="tile"#).count(), 2);

        let two = Catalog::build(
            vec![in_group("Infra", "A", "/a/"), in_group("Data", "B", "/b/")],
            Vec::new(),
        );
        let page = home_page(&two, &Icons::empty(), &ctx());
        assert_eq!(page.matches(r#"<div class="group__head">"#).count(), 2);
        assert_eq!(page.matches(r#"aria-labelledby="g-"#).count(), 2);

        let mut down = in_group("Infra", "A", "/a/");
        down.health = Health::Down;
        let page = home_page(
            &Catalog::build(vec![down, in_group("Infra", "B", "/b/")], Vec::new()),
            &Icons::empty(),
            &ctx(),
        );
        assert_eq!(page.matches(r#"<div class="group__head">"#).count(), 1);
        assert!(page.contains(r#"<span class="badge badge--danger">1 down</span>"#));

        let mut sick = in_group("Infra", "A", "/a/");
        sick.health = Health::Up;
        sick.runtime = Some(RuntimeHealth::Unhealthy);
        let page = home_page(
            &Catalog::build(vec![sick], Vec::new()),
            &Icons::empty(),
            &ctx(),
        );
        assert!(page.contains(r#"<span class="badge badge--warn">1 unhealthy</span>"#));
        assert!(page.contains(r#"<h2 class="group__title" id="g-0">Infra</h2>"#));
    }

    #[test]
    fn the_name_of_a_single_group_is_escaped_in_its_accessible_name() {
        let page = home_page(
            &Catalog::build(vec![in_group(r#"a"b<c"#, "A", "/a/")], Vec::new()),
            &Icons::empty(),
            &ctx(),
        );
        assert!(page.contains(r#"aria-label="a&quot;b&lt;c""#), "{page}");
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
        let page = message_page("Page not found", "Nothing lives here.", "Back to services");
        assert!(page.contains("<h1>Page not found</h1>"));
        assert!(page.contains(r#"<a class="button button--primary" href="/">Back to services</a>"#));
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

    fn external_entry(name: &str, href: &str) -> crate::catalog::external::ExternalEntry {
        let (name, id) = crate::catalog::external::check_name(name).unwrap();
        crate::catalog::external::ExternalEntry {
            id,
            name,
            href: href.into(),
            created_at: 1,
        }
    }

    #[test]
    fn a_tile_of_an_external_service_has_the_badge_the_link_and_no_health() {
        let base = Catalog::build(vec![service("Local", "", "/local/")], Vec::new());
        let plain = home_page(&base, &Icons::empty(), &ctx());
        assert!(!plain.contains("tile__state--external") && !plain.contains("i-arrow-up-right"));
        let catalog = base.with_externals(&[external_entry(
            "Team wiki",
            "https://wiki.example.org/start?a=1&b=2",
        )]);
        let page = home_page(&catalog, &Icons::empty(), &ctx());
        // The badge: the word, the arrow and the note, once, on the tile of the external service.
        assert_eq!(page.matches("tile__state--external").count(), 1, "{page}");
        assert!(page.contains(
            r##"<span class="tile__state tile__state--external" title="External link. This server does not protect it."><svg class="icon icon--sm" aria-hidden="true"><use href="#i-arrow-up-right"/></svg>external</span>"##
        ));
        assert!(
            page.contains(r#"<symbol id="i-arrow-up-right""#),
            "the sprite has the arrow"
        );
        // The link is the absolute URL, escaped. The tile has the initial of the name.
        assert!(page.contains(r#"href="https://wiki.example.org/start?a=1&amp;b=2""#));
        assert!(page.contains(r#"<span class="tile__icon" aria-hidden="true">T</span>"#));
        assert!(
            page.contains(r#">External</h2>"#),
            "the group External: {page}"
        );
        // No health chip, whatever the state of the other tiles.
        let tile = &page[page.find("Team wiki").unwrap()..];
        let tile = &tile[..tile.find("</a>").unwrap()];
        assert!(!tile.contains("tile__state--up") && !tile.contains("tile__state--down"));
        // The setting `ui.services_new_tab` applies to it as to the other tiles.
        let on = HomeContext {
            services_new_tab: true,
            ..ctx()
        };
        let page = home_page(&catalog, &Icons::empty(), &on);
        assert_eq!(
            page.matches(r#"target="_blank" rel="noopener noreferrer""#)
                .count(),
            2
        );
        let off = home_page(&catalog, &Icons::empty(), &ctx());
        assert!(!off.contains("_blank"));
    }

    #[test]
    fn the_setting_makes_every_tile_open_in_a_new_tab_safely_or_none() {
        let catalog = Catalog::build(
            vec![
                service("Docs", "", "https://example.org/docs"),
                service("Local", "", "/local/"),
            ],
            Vec::new(),
        );
        let on = HomeContext {
            services_new_tab: true,
            ..ctx()
        };
        let page = home_page(&catalog, &Icons::empty(), &on);
        assert_eq!(
            page.matches(r#"target="_blank" rel="noopener noreferrer""#)
                .count(),
            2,
            "one value for each service"
        );
        let off = home_page(&catalog, &Icons::empty(), &ctx());
        assert!(!off.contains("_blank"));
    }

    #[test]
    fn empty_catalog_explains_how_to_populate_it() {
        let page = home_page(&Catalog::default(), &Icons::empty(), &ctx());
        assert!(page.contains("No services yet"));
        assert!(page.contains("Add the catalogue labels to a container or an IngressRoute"));
        // The hint names no label key and no product word: the README documents the label names.
        body_words::assert_clean("empty launcher", &page);
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
            assert!(page.contains(&format!(
                r#"<link rel="stylesheet" href="{}">"#,
                assets().css_path
            )));
        }
    }

    #[test]
    fn tiles_use_the_declared_icon_then_one_named_like_the_service_then_the_initial() {
        let root = std::env::temp_dir().join(format!("portal-ui-icons-{}", std::process::id()));
        for file in ["di/notes.svg", "si/grafana.svg"] {
            std::fs::create_dir_all(root.join(file).parent().unwrap()).unwrap();
            std::fs::write(root.join(file), "<svg/>").unwrap();
        }
        let icons = Icons::load(&root);
        let mut declared = service("Workflows", "", "/w/");
        declared.icon = Some("notes.png".into());
        let catalog = Catalog::build(
            vec![
                declared,
                service("Grafana", "", "/g/"),
                service("Unknown", "", "/u/"),
            ],
            Vec::new(),
        );
        let page = home_page(&catalog, &icons, &ctx());
        assert!(page.contains(r#"<img class="tile__img" src="/auth/icons/di/notes.svg""#));
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
