//! The labels of one workload (Compose container labels, Kubernetes annotations) become a [`Service`].
//!
//! Three dialects share one field set. `portal.*` is the native dialect. It works in both runtimes.
//! A Kubernetes annotation key may be a bare name. The module reads `homepage.*` (Docker) and
//! `gethomepage.dev/*` (Kubernetes). The existing labels keep working while they are migrated.
//! If a field is set in `portal.*`, that value always wins.

use std::collections::BTreeMap;

use super::{Health, Service};
use crate::access::{href_prefix, ExcludeRule};

const MAX_TEXT_CHARS: usize = 160;
const MAX_EXCLUDES: usize = 8;
/// The id is the audience of the identity tokens. It is also the resource id of the grants.
const MAX_ID_CHARS: usize = crate::grants::MAX_SERVICE_CHARS;
/// The path of the actions document. See contract 11.4.
pub const ACTIONS_PATH: &str = "/.well-known/access-actions";
const DEFAULT_GROUP: &str = "Other";

/// Parses the labels of one workload. `origin` names the workload in errors. It is also the
/// fallback display name: the container name, or `namespace/name` of the route.
///
/// `Ok(None)` means that the workload does not ask to be listed. `Err` means that the workload
/// asks to be listed but declares something unusable. The catalogue reports the error and skips
/// the entry.
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
    let actions_url = match labels.get("portal.actions").map(|v| v.trim()) {
        Some(url) if !url.is_empty() => {
            if !is_http_url(url) {
                return Err(fail(format!("actions {url:?} must be an http(s) URL")));
            }
            Some(url.to_string())
        }
        _ => health_url.as_deref().and_then(url_origin).map(|o| format!("{o}{ACTIONS_PATH}")),
    };
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
    if id.chars().count() > MAX_ID_CHARS {
        return Err(fail(format!(
            "id {id:?} is longer than {MAX_ID_CHARS} characters: set a shorter portal.id"
        )));
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
        actions_url,
        exclude,
        health: Health::Unknown,
    }))
}

/// Whether the labels ask for a listing. Native: a name, unless it is disabled. Homepage-Docker: a name.
/// Homepage-Kubernetes: `enabled: "true"`. The route carries the other fields.
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

/// The first value that is not empty, among the three dialects. The native dialect comes first.
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

/// Display text. It has no control characters and a bounded length. The text is cut on a character boundary.
fn text(raw: &str) -> Result<String, &'static str> {
    if raw.chars().any(char::is_control) {
        return Err("contains control characters");
    }
    Ok(raw.chars().take(MAX_TEXT_CHARS).collect())
}

/// A path (`/grafana/`) or an http(s) URL. The parser refuses anything else (`javascript:`, `//host`).
/// The value ends up in an `href`.
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

/// The `scheme://host[:port]` part of an http(s) URL. The path is not included.
fn url_origin(url: &str) -> Option<&str> {
    let scheme_end = url.find("://")? + 3;
    let end = url[scheme_end..]
        .find(['/', '?', '#'])
        .map_or(url.len(), |i| scheme_end + i);
    (end > scheme_end).then(|| &url[..end])
}

/// The `portal.exclude` label: one rule per line. The parser ignores blank lines and `#` comments.
/// Each rule is a path under the service's own `href`. A service can therefore open only the paths
/// that it owns. If any rule is unusable, the whole label is unusable. Then nothing is excluded,
/// and the parser reports the problem.
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

/// Lower-case. Each run of characters that are not letters or digits becomes one `-`.
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
            ]),
        )
        .unwrap()
        .unwrap();
        assert_eq!(service.id, "infra-notes");
        assert_eq!(service.group, "Infra");
        assert_eq!(service.href, "/notes/");
        assert!(service.new_tab);
        assert_eq!(service.weight, 40);
        assert_eq!(
            service.health_url.as_deref(),
            Some("http://notes-server:8000/api/health/status")
        );
        assert_eq!(
            service.actions_url.as_deref(),
            Some("http://notes-server:8000/.well-known/access-actions"),
            "the origin of the health URL"
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
                ("portal.actions", "/relative")
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
            &labels(&[("portal.name", &long), ("portal.href", "/"), ("portal.id", "long")]),
        )
        .unwrap()
        .unwrap();
        assert_eq!(service.name.chars().count(), MAX_TEXT_CHARS);
    }

    #[test]
    fn the_actions_url_is_the_label_or_the_origin_of_the_health_url() {
        let read = |extra: &[(&str, &str)]| {
            let mut all = vec![("portal.name", "X"), ("portal.href", "/x/")];
            all.extend_from_slice(extra);
            parse("x", &labels(&all)).unwrap().unwrap().actions_url
        };
        assert_eq!(read(&[]), None, "no health URL, no label: only access");
        assert_eq!(
            read(&[("portal.health", "https://db.apps:8443/api/health?x=1")]).as_deref(),
            Some("https://db.apps:8443/.well-known/access-actions")
        );
        assert_eq!(
            read(&[
                ("portal.health", "http://db:80/up"),
                ("portal.actions", "http://db-admin:9000/.well-known/access-actions"),
            ])
            .as_deref(),
            Some("http://db-admin:9000/.well-known/access-actions"),
            "the label wins"
        );
    }

    #[test]
    fn a_label_that_does_not_exist_is_ignored() {
        let service = parse(
            "x",
            &labels(&[("portal.name", "X"), ("portal.href", "/x/"), ("portal.unknown", "ops")]),
        )
        .unwrap()
        .unwrap();
        assert_eq!(service.name, "X");
    }

    #[test]
    fn an_id_longer_than_an_audience_can_be_is_refused() {
        let long = "a".repeat(65);
        let result = parse(
            "x",
            &labels(&[("portal.name", "X"), ("portal.href", "/x/"), ("portal.id", &long)]),
        );
        assert!(result.unwrap_err().contains("set a shorter portal.id"));
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
