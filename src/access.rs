//! Which service a request belongs to, and who may open it.
//!
//! The gate runs in `/auth/verify`, which only sees the URI as the browser wrote it
//! (`X-Forwarded-Uri`), while Traefik routes on the cleaned path. The two must not disagree: a
//! request for `/grafana/../clickhouse/` is routed to ClickHouse, so it has to be judged as a
//! ClickHouse request. The path is therefore read the ways a proxy or a backend might read it
//! (`%2f` as a character or as a separator, `\` as a character or as a separator), and the request
//! is allowed only if every reading is allowed.

use crate::catalog::Catalog;

const MAX_RULE_SEGMENTS: usize = 16;
const MAX_SEGMENT_CHARS: usize = 64;
const METHODS: [&str; 7] = ["GET", "HEAD", "POST", "PUT", "PATCH", "DELETE", "OPTIONS"];

#[derive(Clone, Debug, PartialEq, Eq)]
enum Part {
    /// A segment written out: `[A-Za-z0-9._~-]`.
    Literal(String),
    /// `*`: exactly one segment.
    One,
    /// `**`, last only: one or more segments.
    Rest,
}

/// A path a service declares reachable without signing in (`portal.exclude`, one rule per line):
/// `[METHOD[,METHOD]] /path/with/*/and/**`. Segments are matched whole: there is no partial
/// wildcard and no regular expression, so a rule reads the way the portal's own path model does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExcludeRule {
    methods: Vec<String>,
    parts: Vec<Part>,
    text: String,
}

impl ExcludeRule {
    pub fn parse(line: &str) -> Result<Self, String> {
        let tokens: Vec<&str> = line.split_whitespace().collect();
        let (methods, path) = match tokens.as_slice() {
            [path] => (Vec::new(), *path),
            [methods, path] => {
                let mut list: Vec<String> = Vec::new();
                for method in methods.split(',') {
                    if !METHODS.contains(&method) {
                        return Err(format!("{method:?} is not one of {}", METHODS.join(", ")));
                    }
                    if !list.iter().any(|m| m == method) {
                        list.push(method.to_string());
                    }
                }
                (list, *path)
            }
            _ => return Err("expected `[METHOD[,METHOD]] /path`".into()),
        };
        if !path.starts_with('/') {
            return Err("the path must start with /".into());
        }
        let segments: Vec<&str> = path[1..].split('/').collect();
        if segments.len() > MAX_RULE_SEGMENTS {
            return Err(format!("more than {MAX_RULE_SEGMENTS} segments"));
        }
        let mut parts = Vec::with_capacity(segments.len());
        for (index, segment) in segments.iter().enumerate() {
            let last = index + 1 == segments.len();
            parts.push(match *segment {
                "" => return Err("empty segment (a leading, doubled or trailing /)".into()),
                "*" if index > 0 => Part::One,
                "**" if index > 0 && last => Part::Rest,
                "*" | "**" if index == 0 => return Err("the first segment must be written out".into()),
                "**" => return Err("** is only allowed as the last segment".into()),
                "." | ".." => return Err("dot segments are not allowed".into()),
                literal => {
                    let valid = literal.len() <= MAX_SEGMENT_CHARS
                        && literal
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'~' | b'-'));
                    if !valid {
                        return Err(format!(
                            "segment {literal:?} must be *, ** or 1-{MAX_SEGMENT_CHARS} characters of a-z, A-Z, 0-9, . _ ~ -"
                        ));
                    }
                    Part::Literal(literal.to_string())
                }
            });
        }
        let text = if methods.is_empty() {
            path.to_string()
        } else {
            format!("{} {path}", methods.join(","))
        };
        Ok(Self { methods, parts, text })
    }

    /// The rule as normalised, for display.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Whether the written-out start of the rule is `prefix` (the path the service owns).
    pub fn starts_with(&self, prefix: &[String]) -> bool {
        prefix.len() <= self.parts.len()
            && prefix
                .iter()
                .zip(&self.parts)
                .all(|(segment, part)| matches!(part, Part::Literal(l) if l == segment))
    }

    fn matches(&self, method: Option<&str>, segments: &[String]) -> bool {
        if !self.methods.is_empty() && !method.is_some_and(|m| self.methods.iter().any(|x| x == m)) {
            return false;
        }
        let mut at = 0;
        for part in &self.parts {
            match part {
                Part::Literal(literal) => {
                    if segments.get(at) != Some(literal) {
                        return false;
                    }
                    at += 1;
                }
                Part::One => match segments.get(at) {
                    Some(segment) if plain(segment) => at += 1,
                    _ => return false,
                },
                Part::Rest => {
                    let rest = segments.get(at..).unwrap_or(&[]);
                    return !rest.is_empty() && rest.iter().all(|s| plain(s));
                }
            }
        }
        at == segments.len()
    }
}

/// A segment a wildcard may stand for: no separator hidden in it (`%2f` decoded), no control character.
fn plain(segment: &str) -> bool {
    !segment.is_empty() && !segment.contains(['/', '\\']) && !segment.chars().any(char::is_control)
}

/// The service whose `portal.exclude` rule lets this request through without a session, if any.
///
/// Conservative on every point: the path is read every way a proxy or a backend might read it
/// ([`path_readings`]) and every reading must be owned by the same service and match one of its
/// rules; `/auth/*` is never excluded; a URI that cannot be read safely is never excluded. A rule
/// only ever applies to the service that declared it and owns the path (longest prefix).
pub fn excluded(catalog: &Catalog, method: Option<&str>, uri: &str) -> Option<String> {
    let readings = path_readings(uri)?;
    let mut prefix: Option<&Vec<String>> = None;
    let mut hit = None;
    for (index, reading) in readings.iter().enumerate() {
        if reading.first().map(String::as_str) == Some("auth") {
            return None;
        }
        let owner = catalog.owner(reading)?;
        match prefix {
            Some(known) if *known != owner.prefix => return None,
            _ => prefix = Some(&owner.prefix),
        }
        let service = owner
            .services
            .iter()
            .find(|s| s.exclude.iter().any(|rule| rule.matches(method, reading)))?;
        if index == 0 {
            hit = Some(service.id.clone());
        }
    }
    hit
}

/// The segments of a path on this host once dot segments and empty segments are resolved.
pub type Segments = Vec<String>;

/// The path prefix a service owns, from its `href`. `None` for an absolute URL (it lives on another
/// host, nothing here can gate it) and for `/` (it would own everything).
pub fn href_prefix(href: &str) -> Option<Segments> {
    if !href.starts_with('/') || href.starts_with("//") {
        return None;
    }
    let path = href.split(['?', '#']).next().unwrap_or("");
    let segments = clean(path.split('/'));
    if segments.is_empty() {
        None
    } else {
        Some(segments)
    }
}

/// Every reading of the path of `uri`. `None` when the URI cannot be read safely (a NUL byte, a
/// broken percent escape): such a request is refused.
pub fn path_readings(uri: &str) -> Option<Vec<Segments>> {
    let path = uri.split(['?', '#']).next().unwrap_or("");
    if !path.starts_with('/') {
        return None;
    }
    let mut decoded = Vec::new();
    for raw in path.split('/') {
        decoded.push(percent_decode(raw)?);
    }
    // As written: an encoded slash stays inside its segment.
    let literal = clean(decoded.iter().map(String::as_str));
    // As a backend that decodes before it routes: an encoded slash separates.
    let slashes = clean(decoded.iter().flat_map(|s| s.split('/')));
    // And with a backslash read as a separator too.
    let backslashes = clean(decoded.iter().flat_map(|s| s.split(['/', '\\'])));
    let mut readings = vec![literal];
    for reading in [slashes, backslashes] {
        if !readings.contains(&reading) {
            readings.push(reading);
        }
    }
    Some(readings)
}

fn clean<'a>(segments: impl Iterator<Item = &'a str>) -> Segments {
    let mut out: Segments = Vec::new();
    for segment in segments {
        match segment {
            "" | "." => {}
            ".." => {
                out.pop();
            }
            other => out.push(other.to_string()),
        }
    }
    out
}

fn percent_decode(raw: &str) -> Option<String> {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hi = hex(*bytes.get(i + 1)?)?;
            let lo = hex(*bytes.get(i + 2)?)?;
            out.push((hi << 4) | lo);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    if out.contains(&0) {
        return None;
    }
    // Bytes that are not UTF-8 cannot spell a service prefix; they only have to stay distinct.
    Some(String::from_utf8_lossy(&out).into_owned())
}

fn hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Whether a service bound to `service_roles` may be used by someone holding `user_roles` (`None`
/// is the superadmin, who may use everything). A service without roles is open to nobody but the
/// superadmin: access is granted by naming a role, never by omission. A role missing from `registry`
/// does not exist: it opens the service to nobody either.
pub fn service_allows(
    service_roles: &[String],
    registry: &[String],
    user_roles: Option<&[String]>,
) -> bool {
    match user_roles {
        None => true,
        Some(held) => service_roles
            .iter()
            .any(|role| registry.contains(role) && held.contains(role)),
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Decision {
    /// The request may go through; `service` is the one it belongs to, if any.
    Allow { service: Option<String> },
    /// The request is refused; `service` is the one it belongs to, so the page can name it.
    Deny { service: Option<String> },
}

/// The gate. `roles` is `None` for the superadmin.
pub fn decide(
    catalog: &Catalog,
    registry: &[String],
    roles: Option<&[String]>,
    uri: &str,
) -> Decision {
    let readings = path_readings(uri);
    let Some(readings) = readings else {
        return if roles.is_none() {
            Decision::Allow { service: None }
        } else {
            Decision::Deny { service: None }
        };
    };
    let mut service = None;
    for (index, reading) in readings.iter().enumerate() {
        let Some(owner) = catalog.owner(reading) else {
            continue;
        };
        let allowed = owner
            .services
            .iter()
            .find(|s| service_allows(&s.roles, registry, roles));
        match allowed {
            None => {
                return Decision::Deny {
                    service: owner.services.first().map(|s| s.id.clone()),
                }
            }
            Some(s) if index == 0 => service = Some(s.id.clone()),
            Some(_) => {}
        }
    }
    Decision::Allow { service }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::{parse_labels, Catalog};
    use std::collections::BTreeMap;

    fn svc(name: &str, href: &str, roles: &str) -> crate::catalog::Service {
        let labels: BTreeMap<String, String> = [
            ("portal.name", name),
            ("portal.href", href),
            ("portal.roles", roles),
        ]
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        parse_labels("x", &labels).unwrap().unwrap()
    }

    fn catalog() -> Catalog {
        Catalog::build(
            vec![
                svc("Grafana", "/grafana/", "ops"),
                svc("ClickHouse", "/clickhouse/", "data"),
                svc("Notes", "/notes/", ""),
                svc("Nested", "/grafana/admin/", "root-only"),
                svc("Typo", "/typo/", "opz"),
                svc("External", "https://example.org/", "ops"),
            ],
            Vec::new(),
        )
    }

    fn registry() -> Vec<String> {
        ["ops", "data", "root-only"]
            .iter()
            .map(|r| r.to_string())
            .collect()
    }

    fn roles(list: &[&str]) -> Vec<String> {
        list.iter().map(|r| r.to_string()).collect()
    }

    fn verdict(held: &[&str], uri: &str) -> Decision {
        decide(&catalog(), &registry(), Some(&roles(held)), uri)
    }

    fn allowed(held: &[&str], uri: &str) -> bool {
        matches!(verdict(held, uri), Decision::Allow { .. })
    }

    #[test]
    fn prefixes_come_from_local_hrefs_only() {
        assert_eq!(href_prefix("/grafana/").unwrap(), ["grafana"]);
        assert_eq!(href_prefix("/a/b?x=1#y").unwrap(), ["a", "b"]);
        assert!(href_prefix("/").is_none());
        assert!(href_prefix("https://example.org/x").is_none());
        assert!(href_prefix("//evil/x").is_none());
    }

    #[test]
    fn paths_are_cleaned_like_a_proxy_would() {
        let read = |uri: &str| path_readings(uri).unwrap()[0].clone();
        assert_eq!(read("/grafana/d/abc?x=1"), ["grafana", "d", "abc"]);
        assert_eq!(read("//grafana///x/"), ["grafana", "x"]);
        assert_eq!(read("/grafana/./x"), ["grafana", "x"]);
        assert_eq!(read("/grafana/../clickhouse/"), ["clickhouse"]);
        assert_eq!(read("/grafana/%2e%2e/clickhouse"), ["clickhouse"]);
        assert_eq!(read("/../../grafana"), ["grafana"]);
        assert!(path_readings("/a%00b").is_none());
        assert!(path_readings("/a%zz").is_none());
        assert!(path_readings("/a%2").is_none());
        assert!(path_readings("no-slash").is_none());
    }

    #[test]
    fn an_encoded_slash_is_read_both_ways() {
        let readings = path_readings("/grafana%2F..%2Fclickhouse/").unwrap();
        assert!(
            readings.contains(&vec!["grafana/../clickhouse".to_string()]),
            "as written"
        );
        assert!(
            readings.contains(&vec!["clickhouse".to_string()]),
            "as a separator"
        );
        let backslash = path_readings("/grafana%5C..%5Cclickhouse").unwrap();
        assert!(backslash.contains(&vec!["clickhouse".to_string()]));
    }

    #[test]
    fn a_service_without_roles_is_open_to_the_superadmin_only() {
        assert!(!allowed(&[], "/notes/api/x"));
        assert!(!allowed(&["ops", "data", "root-only"], "/notes"));
        assert!(matches!(
            decide(&catalog(), &registry(), None, "/notes/api/x"),
            Decision::Allow { .. }
        ));
    }

    #[test]
    fn a_service_with_roles_needs_one_of_them() {
        assert!(allowed(&["ops"], "/grafana/d/1"));
        assert!(!allowed(&["data"], "/grafana/d/1"));
        assert!(!allowed(&[], "/grafana/"));
        assert!(allowed(&["data", "ops"], "/clickhouse/"));
    }

    #[test]
    fn matching_is_on_whole_segments_and_the_longest_prefix_wins() {
        assert!(allowed(&[], "/grafana-evil/"), "another path, no service");
        assert!(
            !allowed(&["ops"], "/grafana/admin/x"),
            "the nested service needs root-only"
        );
        assert!(allowed(&["ops", "root-only"], "/grafana/admin/x"));
        assert!(
            allowed(&["ops"], "/grafana/administrator"),
            "not the nested prefix"
        );
    }

    #[test]
    fn paths_that_no_service_owns_stay_open() {
        assert!(allowed(&[], "/app.js"));
        assert!(allowed(&[], "/auth/denied"));
        assert!(allowed(&[], "/"));
    }

    #[test]
    fn a_role_missing_from_the_registry_opens_nothing_but_the_superadmin_may_pass() {
        assert!(!allowed(&["opz"], "/typo/"), "the role does not exist");
        assert!(matches!(
            decide(&catalog(), &registry(), None, "/typo/"),
            Decision::Allow { .. }
        ));
    }

    #[test]
    fn path_tricks_cannot_reach_a_service_the_user_may_not_open() {
        for uri in [
            "/notes/../clickhouse/",
            "/notes/%2e%2e/clickhouse/",
            "/notes/%2E%2E/clickhouse/",
            "//clickhouse/",
            "/./clickhouse/",
            "/notes%2F..%2Fclickhouse/",
            "/notes%5C..%5Cclickhouse/",
            "/clickhouse/./x",
            "/grafana/../clickhouse?x=/notes/",
            "/clickhouse%00/",
            "/clickhouse%zz/",
        ] {
            assert!(
                !allowed(&["ops"], uri),
                "{uri} must be refused to an `ops` user"
            );
        }
        // The superadmin is never held back.
        assert!(matches!(
            decide(&catalog(), &registry(), None, "/clickhouse%00/"),
            Decision::Allow { .. }
        ));
    }

    #[test]
    fn a_refusal_names_the_service_it_concerns() {
        assert_eq!(
            verdict(&["data"], "/grafana/d/1"),
            Decision::Deny {
                service: Some("other-grafana".into())
            }
        );
        assert_eq!(
            verdict(&["ops"], "/clickhouse%00/"),
            Decision::Deny { service: None },
            "an unreadable path belongs to no service"
        );
    }

    #[test]
    fn the_service_a_request_belongs_to_is_reported_for_the_history() {
        assert_eq!(
            verdict(&["ops"], "/grafana/d/1"),
            Decision::Allow {
                service: Some("other-grafana".into())
            }
        );
        assert_eq!(
            verdict(&["ops"], "/app.js"),
            Decision::Allow { service: None }
        );
    }
}

#[cfg(test)]
mod exclude_tests {
    use super::*;
    use crate::catalog::{parse_labels, Catalog, Service};
    use std::collections::BTreeMap;

    fn svc(name: &str, href: &str, exclude: &str) -> Service {
        let labels: BTreeMap<String, String> = [
            ("portal.name", name),
            ("portal.href", href),
            ("portal.roles", "ops"),
            ("portal.exclude", exclude),
        ]
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        parse_labels("x", &labels).unwrap().unwrap()
    }

    fn catalog() -> Catalog {
        Catalog::build(
            vec![
                svc(
                    "Dagu",
                    "/dagu",
                    "POST /dagu/api/v1/webhooks/*\nGET /dagu/api/v1/health\nPUT,POST /dagu/hooks/**",
                ),
                svc("Dagu admin", "/dagu/admin", ""),
                svc("Grafana", "/grafana/", ""),
            ],
            Vec::new(),
        )
    }

    fn open(method: Option<&str>, uri: &str) -> bool {
        excluded(&catalog(), method, uri).is_some()
    }

    #[test]
    fn a_declared_path_is_open_for_its_methods_only() {
        assert!(open(Some("POST"), "/dagu/api/v1/webhooks/abc"));
        assert!(open(Some("POST"), "/dagu/api/v1/webhooks/abc?token=1&x=/../y"));
        assert!(!open(Some("GET"), "/dagu/api/v1/webhooks/abc"));
        assert!(!open(None, "/dagu/api/v1/webhooks/abc"), "no method, no match for a rule that names one");
        assert!(open(Some("GET"), "/dagu/api/v1/health"));
        assert!(!open(Some("POST"), "/dagu/api/v1/health"));
        assert!(open(Some("PUT"), "/dagu/hooks/a/b/c"));
        assert!(!open(Some("PUT"), "/dagu/hooks"), "** needs at least one segment");
    }

    #[test]
    fn a_wildcard_stands_for_whole_segments_only() {
        assert!(!open(Some("POST"), "/dagu/api/v1/webhooks"));
        assert!(!open(Some("POST"), "/dagu/api/v1/webhooks/"));
        assert!(!open(Some("POST"), "/dagu/api/v1/webhooks/a/b"));
        assert!(!open(Some("POST"), "/dagu/api/v1/webhooksx/a"));
        assert!(!open(Some("GET"), "/dagu/api/v1/health/extra"));
    }

    #[test]
    fn an_encoded_path_cannot_smuggle_the_wildcard_past_the_gate() {
        assert!(!open(Some("POST"), "/dagu/api/v1/webhooks/..%2f..%2fadmin"));
        assert!(!open(Some("POST"), "/dagu/api/v1/webhooks/%2e%2e%2fsecret"));
        assert!(!open(Some("POST"), "/dagu/api/v1/webhooks/a%5Cb"));
        assert!(!open(Some("POST"), "/dagu/api/v1/webhooks/%2e%2e/%2e%2e/auth/settings"));
        assert!(!open(Some("POST"), "/dagu/api/v1/webhooks/a%00b"));
        assert!(!open(Some("POST"), "/dagu/api/v1/webhooks/a%zz"));
        // Dot segments that cancel out still land on a declared path: judged as that path.
        assert!(open(Some("GET"), "/dagu/api/v1/webhooks/../health"));
        assert!(!open(Some("POST"), "/dagu/api/v1/webhooks/../health"));
    }

    #[test]
    fn dot_segments_cannot_carry_a_request_out_of_the_service() {
        // The path is cleaned first: these are requests for /test, /grafana/... or /auth/..., not for Dagu.
        for uri in [
            "/dagu/../test",
            "/dagu/%2e%2e/test",
            "/dagu/api/v1/webhooks/../../../test",
            "/dagu/hooks/../../test",
            "/dagu/hooks/../../grafana/x",
            "/dagu/%2e%2e%2ftest",
            "/dagu/hooks/..%5c..%5ctest",
        ] {
            for method in ["GET", "POST", "PUT"] {
                assert!(!open(Some(method), uri), "{method} {uri}");
            }
        }
        // A rule itself cannot be written with a dot segment.
        assert!(ExcludeRule::parse("POST /dagu/../test").is_err());
        assert!(ExcludeRule::parse("POST /dagu/./x").is_err());
    }

    #[test]
    fn the_portal_itself_and_other_services_are_never_opened() {
        assert!(!open(Some("POST"), "/auth/login"));
        assert!(!open(Some("POST"), "/dagu/../auth/settings"));
        assert!(!open(Some("POST"), "/grafana/api/v1/webhooks/abc"));
        assert!(!open(Some("POST"), "/unowned/api/v1/webhooks/abc"));
    }

    #[test]
    fn a_nested_service_keeps_its_own_path_whatever_the_parent_declares() {
        assert!(!open(Some("POST"), "/dagu/admin/hooks/x"), "owned by Dagu admin, which declares nothing");
        assert!(open(Some("POST"), "/dagu/hooks/x"));
    }

    #[test]
    fn rules_are_validated_when_the_label_is_read() {
        let parse = |line: &str| ExcludeRule::parse(line);
        assert_eq!(parse("post /a/b").unwrap_err(), "\"post\" is not one of GET, HEAD, POST, PUT, PATCH, DELETE, OPTIONS");
        for bad in [
            "a/b", "/", "/a//b", "/a/b/", "/*/b", "/**", "/a/**/b", "/a/./b", "/a/../b", "/a/b?x=1", "/a/b#x",
            "/a/[0-9]+", "/a/b.*", "/a/fo*", "/a/%2e", "/a/b c d", "GET POST /a", "",
        ] {
            assert!(parse(bad).is_err(), "{bad:?} must be refused");
        }
        let rule = parse("POST,POST,PUT /a/*/c").unwrap();
        assert_eq!(rule.text(), "POST,PUT /a/*/c");
        assert!(rule.starts_with(&["a".to_string()]));
        assert!(!rule.starts_with(&["b".to_string()]));
        assert!(!rule.starts_with(&["a".to_string(), "x".to_string()]), "a wildcard is not the written-out prefix");
    }
}
