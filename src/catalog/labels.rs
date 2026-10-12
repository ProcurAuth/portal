//! One workload's labels (Compose container labels, Kubernetes annotations) become a [`Service`].
//!
//! Three dialects share one field set. `portal.*` is native and works in both runtimes (a
//! Kubernetes annotation key may be a bare name). `homepage.*` (Docker) and `gethomepage.dev/*`
//! (Kubernetes) are read so the existing labels keep working while they are migrated; a field set
//! in `portal.*` always wins.

use std::collections::BTreeMap;

use super::{Health, Service};
use crate::access::{href_prefix, ExcludeRule};

const MAX_TEXT_CHARS: usize = 160;
const MAX_ROLES: usize = 16;
const MAX_EXCLUDES: usize = 8;
const DEFAULT_GROUP: &str = "Other";

/// Parses the labels of one workload. `origin` names it in errors and is the fallback display
/// name (container, or `namespace/name` of the route).
///
/// `Ok(None)` means the workload does not ask to be listed. `Err` means it does but declares
/// something unusable; the catalogue reports it and skips the entry.
pub fn parse(origin: &str, labels: &BTreeMap<String, String>) -> Result<Option<Service>, String> {
    if !is_listed(labels) {
        return Ok(None);
    }
    let fail = |message: String| format!("{origin}: {message}");

    let name = text(field(labels, "name").unwrap_or_else(|| short_origin(origin)))
        .map_err(|e| fail(format!("name {e}")))?;
    let group = text(field(labels, "group").unwrap_or(DEFAULT_GROUP))
        .map_err(|e| fail(format!("group {e}")))?;
    let description = text(field(labels, "description").unwrap_or(""))
        .map_err(|e| fail(format!("description {e}")))?;
    let href = field(labels, "href").ok_or_else(|| fail("href is required".into()))?;
    if !is_safe_href(href) {
        return Err(fail(format!(
            "href {href:?} must be a path or an http(s) URL"
        )));
    }
    let health_url = match field(labels, "health").or_else(|| homepage_site_monitor(labels)) {
        Some(url) if is_http_url(url) => Some(url.to_string()),
        Some(url) => return Err(fail(format!("health {url:?} must be an http(s) URL"))),
        None => None,
    };
    let weight = match field(labels, "weight") {
        Some(raw) => raw
            .parse::<i32>()
            .map_err(|_| fail(format!("weight {raw:?} is not an integer")))?,
        None => 0,
    };
    let roles = parse_roles(labels.get("portal.roles").map(String::as_str).unwrap_or(""))
        .map_err(|e| fail(format!("roles {e}")))?;
    let exclude = parse_exclude(labels.get("portal.exclude").map(String::as_str).unwrap_or(""), href)
        .map_err(|e| fail(format!("exclude {e}")))?;
    let id = match labels
        .get("portal.id")
        .map(|v| v.trim())
        .filter(|v| !v.is_empty())
    {
        Some(explicit) => slug(explicit),
        None => slug(&format!("{group}/{name}")),
    };
    if id.is_empty() {
        return Err(fail("id is empty after normalisation".into()));
    }

    Ok(Some(Service {
        id,
        group,
        name,
        description,
        icon: field(labels, "icon").map(str::to_string),
        href: href.to_string(),
        new_tab: field(labels, "target") == Some("_blank"),
        weight,
        health_url,
        roles,
        exclude,
        health: Health::Unknown,
    }))
}

/// Whether the labels ask for a listing. Native: a name, unless disabled. Homepage-Docker: a name.
/// Homepage-Kubernetes: `enabled: "true"` (the route carries the other fields).
fn is_listed(labels: &BTreeMap<String, String>) -> bool {
    let value = |key: &str| labels.get(key).map(|v| v.trim());
    if value("portal.enabled") == Some("false") {
        return false;
    }
    if value("portal.enabled") == Some("true")
        || value("portal.name").is_some_and(|v| !v.is_empty())
    {
        return true;
    }
    if value("gethomepage.dev/enabled") == Some("true") {
        return true;
    }
    value("homepage.name").is_some_and(|v| !v.is_empty())
}

/// First non-empty value among the three dialects, native first.
fn field<'a>(labels: &'a BTreeMap<String, String>, name: &str) -> Option<&'a str> {
    [
        format!("portal.{name}"),
        format!("homepage.{name}"),
        format!("gethomepage.dev/{name}"),
    ]
    .iter()
    .filter_map(|key| labels.get(key))
    .map(|v| v.trim())
    .find(|v| !v.is_empty())
}

/// Homepage called the health probe `siteMonitor`.
fn homepage_site_monitor(labels: &BTreeMap<String, String>) -> Option<&str> {
    ["homepage.siteMonitor", "gethomepage.dev/siteMonitor"]
        .iter()
        .filter_map(|key| labels.get(*key))
        .map(|v| v.trim())
        .find(|v| !v.is_empty())
}

fn short_origin(origin: &str) -> &str {
    origin.rsplit('/').next().unwrap_or(origin)
}

/// Display text: no control characters, bounded length (cut on a character boundary).
fn text(raw: &str) -> Result<String, &'static str> {
    if raw.chars().any(char::is_control) {
        return Err("contains control characters");
    }
    Ok(raw.chars().take(MAX_TEXT_CHARS).collect())
}

/// A path (`/grafana/`) or an http(s) URL. Anything else (`javascript:`, `//host`) is refused: the
/// value ends up in an `href`.
fn is_safe_href(value: &str) -> bool {
    if value.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return false;
    }
    (value.starts_with('/') && !value.starts_with("//")) || is_http_url(value)
}

fn is_http_url(value: &str) -> bool {
    !value.chars().any(|c| c.is_control() || c.is_whitespace())
        && (value.starts_with("http://") || value.starts_with("https://"))
        && value.len() > "https://".len()
}

/// A comma-separated role list (`portal.roles` label, `roles.names` configuration): lower-case,
/// trimmed, de-duplicated, each `[a-z0-9_-]{1,32}`, at most [`MAX_ROLES`].
pub fn parse_roles(raw: &str) -> Result<Vec<String>, String> {
    let mut out: Vec<String> = Vec::new();
    for item in raw
        .split(',')
        .map(|v| v.trim().to_ascii_lowercase())
        .filter(|v| !v.is_empty())
    {
        let valid = item.len() <= 32
            && item
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_');
        if !valid {
            return Err(format!(
                "{item:?} must be 1-32 characters of a-z, 0-9, - or _"
            ));
        }
        if !out.contains(&item) {
            out.push(item);
        }
    }
    if out.len() > MAX_ROLES {
        return Err(format!("more than {MAX_ROLES} roles"));
    }
    Ok(out)
}

/// The `portal.exclude` label: one rule per line (blank lines and `#` comments ignored), each a path
/// under the service's own `href`, so a service can only open what it owns. Any unusable rule
/// makes the whole label unusable: nothing is excluded then, and the problem is reported.
fn parse_exclude(raw: &str, href: &str) -> Result<Vec<ExcludeRule>, String> {
    let lines: Vec<&str> = raw
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .collect();
    if lines.is_empty() {
        return Ok(Vec::new());
    }
    if lines.len() > MAX_EXCLUDES {
        return Err(format!("more than {MAX_EXCLUDES} rules"));
    }
    let prefix = href_prefix(href).ok_or_else(|| "needs a local href other than /".to_string())?;
    let mut rules: Vec<ExcludeRule> = Vec::new();
    for line in lines {
        let rule = ExcludeRule::parse(line).map_err(|e| format!("{line:?}: {e}"))?;
        if !rule.starts_with(&prefix) {
            return Err(format!("{line:?}: must stay under {href}"));
        }
        if !rules.contains(&rule) {
            rules.push(rule);
        }
    }
    Ok(rules)
}

/// Lower-case, runs of anything but letters and digits become one `-`.
pub(super) fn slug(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut pending_dash = false;
    for c in value.chars().flat_map(char::to_lowercase) {
        if c.is_alphanumeric() {
            if pending_dash && !out.is_empty() {
                out.push('-');
            }
            pending_dash = false;
            out.push(c);
        } else {
            pending_dash = true;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(entries: &[(&str, &str)]) -> BTreeMap<String, String> {
        entries
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn native_labels_cover_every_field() {
        let service = parse(
            "notes",
            &labels(&[
                ("portal.group", "Infra"),
                ("portal.name", "Notes"),
                ("portal.description", "Workflow automation"),
                ("portal.icon", "notes"),
                ("portal.href", "/notes/"),
                ("portal.target", "_blank"),
                ("portal.weight", "40"),
                (
                    "portal.health",
                    "http://notes-server:8000/api/health/status",
                ),
                ("portal.roles", "Admin, ops,admin"),
            ]),
        )
        .unwrap()
        .unwrap();
        assert_eq!(service.id, "infra-notes");
        assert_eq!(service.group, "Infra");
        assert_eq!(service.href, "/notes/");
        assert!(service.new_tab);
        assert_eq!(service.weight, 40);
        assert_eq!(service.roles, ["admin", "ops"]);
        assert_eq!(
            service.health_url.as_deref(),
            Some("http://notes-server:8000/api/health/status")
        );
        assert_eq!(service.icon.as_deref(), Some("notes"));
    }

    #[test]
    fn homepage_docker_labels_still_work() {
        let service = parse(
            "cadvisor",
            &labels(&[
                ("homepage.group", "Monitoring"),
                ("homepage.name", "Cadvisor"),
                ("homepage.href", "/cadvisor/"),
                ("homepage.icon", "cadvisor.png"),
                (
                    "homepage.siteMonitor",
                    "http://cadvisor:8080/cadvisor/healthz",
                ),
                ("homepage.target", "_blank"),
                ("homepage.weight", "20"),
                ("homepage.showStats", "false"),
            ]),
        )
        .unwrap()
        .unwrap();
        assert_eq!(service.name, "Cadvisor");
        assert_eq!(
            service.health_url.as_deref(),
            Some("http://cadvisor:8080/cadvisor/healthz")
        );
        assert_eq!(service.icon.as_deref(), Some("cadvisor.png"));
        assert!(service.roles.is_empty());
    }

    #[test]
    fn homepage_kubernetes_annotations_need_enabled() {
        let annotations = [
            ("gethomepage.dev/group", "Infra"),
            ("gethomepage.dev/name", "Registry"),
            ("gethomepage.dev/href", "/registry/"),
            ("gethomepage.dev/pod-selector", "app=registry-ui"),
        ];
        assert_eq!(
            parse("apps/registry-ui", &labels(&annotations)).unwrap(),
            None
        );

        let mut enabled = annotations.to_vec();
        enabled.push(("gethomepage.dev/enabled", "true"));
        let service = parse("apps/registry-ui", &labels(&enabled))
            .unwrap()
            .unwrap();
        assert_eq!(service.name, "Registry");
        assert_eq!(service.group, "Infra");
    }

    #[test]
    fn native_field_wins_over_homepage_field() {
        let service = parse(
            "x",
            &labels(&[
                ("homepage.name", "Old"),
                ("homepage.group", "Old group"),
                ("homepage.href", "/old/"),
                ("portal.name", "New"),
                ("portal.href", "/new/"),
            ]),
        )
        .unwrap()
        .unwrap();
        assert_eq!(service.name, "New");
        assert_eq!(service.href, "/new/");
        assert_eq!(service.group, "Old group");
    }

    #[test]
    fn unlabelled_or_disabled_workloads_are_not_listed() {
        assert_eq!(
            parse("db", &labels(&[("traefik.enable", "true")])).unwrap(),
            None
        );
        assert_eq!(
            parse(
                "db",
                &labels(&[
                    ("portal.name", "DB"),
                    ("portal.href", "/"),
                    ("portal.enabled", "false")
                ])
            )
            .unwrap(),
            None
        );
    }

    #[test]
    fn name_defaults_to_the_last_part_of_the_origin_and_group_to_other() {
        let service = parse(
            "apps/pgweb",
            &labels(&[("portal.enabled", "true"), ("portal.href", "/pgweb/")]),
        )
        .unwrap()
        .unwrap();
        assert_eq!(service.name, "pgweb");
        assert_eq!(service.group, "Other");
    }

    #[test]
    fn unsafe_values_are_refused() {
        for href in [
            "javascript:alert(1)",
            "//evil.example/",
            "/a b",
            "data:text/html,x",
            "http://",
            "/o\u{7}k",
        ] {
            let result = parse("x", &labels(&[("portal.name", "X"), ("portal.href", href)]));
            assert!(result.is_err(), "{href:?} must be refused");
        }
        assert!(
            parse("x", &labels(&[("portal.name", "X")])).is_err(),
            "href is required"
        );
        assert!(parse(
            "x",
            &labels(&[
                ("portal.name", "X"),
                ("portal.href", "/"),
                ("portal.health", "/relative")
            ])
        )
        .is_err());
        assert!(parse(
            "x",
            &labels(&[
                ("portal.name", "X"),
                ("portal.href", "/"),
                ("portal.weight", "heavy")
            ])
        )
        .is_err());
        assert!(parse(
            "x",
            &labels(&[
                ("portal.name", "X"),
                ("portal.href", "/"),
                ("portal.roles", "ad min")
            ])
        )
        .is_err());
        assert!(parse(
            "x",
            &labels(&[("portal.name", "bad\u{7}name"), ("portal.href", "/")])
        )
        .is_err());
    }

    #[test]
    fn long_text_is_cut_on_a_character_boundary() {
        let long = "é".repeat(500);
        let service = parse(
            "x",
            &labels(&[("portal.name", &long), ("portal.href", "/")]),
        )
        .unwrap()
        .unwrap();
        assert_eq!(service.name.chars().count(), MAX_TEXT_CHARS);
    }

    #[test]
    fn explicit_id_is_normalised() {
        let service = parse(
            "x",
            &labels(&[
                ("portal.name", "X"),
                ("portal.href", "/"),
                ("portal.id", " My App_1 "),
            ]),
        )
        .unwrap()
        .unwrap();
        assert_eq!(service.id, "my-app-1");
    }
}

#[cfg(test)]
mod exclude_tests {
    use super::*;

    fn labels(href: &str, exclude: &str) -> BTreeMap<String, String> {
        [("portal.name", "Dagu"), ("portal.href", href), ("portal.exclude", exclude)]
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn the_label_holds_one_rule_per_line_and_ignores_comments_and_repeats() {
        let service = parse(
            "dagu",
            &labels(
                "/dagu",
                "# webhooks\n  POST /dagu/api/v1/webhooks/*  \n\nPOST /dagu/api/v1/webhooks/*\nGET /dagu/api/v1/health\r\n",
            ),
        )
        .unwrap()
        .unwrap();
        let texts: Vec<&str> = service.exclude.iter().map(ExcludeRule::text).collect();
        assert_eq!(texts, ["POST /dagu/api/v1/webhooks/*", "GET /dagu/api/v1/health"]);
    }

    #[test]
    fn no_label_means_nothing_is_excluded() {
        assert!(parse("x", &labels("/dagu", "")).unwrap().unwrap().exclude.is_empty());
        assert!(parse("x", &labels("https://example.org/", "")).unwrap().unwrap().exclude.is_empty());
    }

    #[test]
    fn a_service_can_only_open_paths_under_its_own_href() {
        for rule in ["POST /grafana/api/x", "POST /dagu", "POST /*/api", "/", "POST /dagux/api"] {
            let err = parse("dagu", &labels("/dagu/api", rule)).unwrap_err();
            assert!(err.contains("exclude"), "{rule}: {err}");
        }
        assert!(parse("dagu", &labels("/dagu", "POST /dagu/*")).is_ok());
        assert!(parse("dagu", &labels("/dagu/api", "POST /dagu/api/x")).is_ok());
    }

    #[test]
    fn a_service_without_a_local_prefix_cannot_open_anything() {
        assert!(parse("x", &labels("/", "GET /a")).is_err());
        assert!(parse("x", &labels("https://example.org/", "GET /a")).is_err());
    }

    #[test]
    fn a_bad_rule_makes_the_whole_label_unusable_and_is_reported_with_its_origin() {
        let err = parse("apps/dagu", &labels("/dagu", "GET /dagu/ok\nGET /dagu/[a-z]+")).unwrap_err();
        assert!(err.starts_with("apps/dagu: exclude "), "{err}");
        let many = (0..9).map(|i| format!("GET /dagu/p{i}")).collect::<Vec<_>>().join("\n");
        assert!(parse("x", &labels("/dagu", &many)).unwrap_err().contains("more than 8 rules"));
    }
}
