//! Which service a request belongs to, and who can open it.
//!
//! The gate runs in `/auth/verify`. This endpoint sees only the URI as the browser sent it
//! (`X-Forwarded-Uri`). Traefik routes on the cleaned path. The two must not disagree. A
//! request for `/grafana/../clickhouse/` is routed to ClickHouse. The gate must therefore judge
//! it as a ClickHouse request. The gate reads the path in each way that a proxy or a backend
//! might read it: `%2f` as a character or as a separator, and `\` as a character or as a
//! separator. The gate allows the request only if every reading is allowed.
//!
//! Traefik matches ``PathPrefix(`/files`)`` as a text prefix: it also routes `/filesX` to that
//! service. The gate therefore judges `/filesX` as a path of the service of `/files/` (see
//! [`Catalog::owner`]). The gate reads the `href` of a service with the same percent-decode as a
//! request path.

use crate::catalog::Catalog;
use crate::grants::{holds, Grant, ACCESS};

const MAX_RULE_SEGMENTS: usize = 16;
const MAX_SEGMENT_CHARS: usize = 64;
const METHODS: [&str; 7] = ["GET", "HEAD", "POST", "PUT", "PATCH", "DELETE", "OPTIONS"];

#[derive(Clone, Debug, PartialEq, Eq)]
enum Part {
    /// A literal segment. It uses the characters `[A-Za-z0-9._~-]`.
    Literal(String),
    /// `*`: exactly one segment.
    One,
    /// `**`, last only: one or more segments.
    Rest,
}

/// A path that a service declares as reachable without a user session (`portal.exclude`, one rule per line):
/// `[METHOD[,METHOD]] /path/with/*/and/**`. The rule matches whole segments. There is no partial
/// wildcard and no regular expression. A rule therefore reads in the same way as the portal path model.
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
        Ok(Self {
            methods,
            parts,
            text,
        })
    }

    /// The normalised rule, for display.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Whether the literal start of the rule is `prefix`. The prefix is the path that the service owns.
    pub fn starts_with(&self, prefix: &[String]) -> bool {
        prefix.len() <= self.parts.len()
            && prefix
                .iter()
                .zip(&self.parts)
                .all(|(segment, part)| matches!(part, Part::Literal(l) if l == segment))
    }

    fn matches(&self, method: Option<&str>, segments: &[String]) -> bool {
        if !self.methods.is_empty() && !method.is_some_and(|m| self.methods.iter().any(|x| x == m))
        {
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

/// A segment that a wildcard can match. It has no hidden separator (`%2f` decoded), no control
/// character and no `;`. A backend that removes path parameters reads `..;` or `..;x=1` as `..`.
/// A wildcard therefore never stands for a segment with a `;`, raw or encoded, and the gate never
/// opens a path that such a backend reads as another path. A dot segment never comes here,
/// because [`clean`] resolves it. The check refuses it all the same.
fn plain(segment: &str) -> bool {
    !segment.is_empty()
        && !matches!(segment, "." | "..")
        && !segment.contains(['/', '\\', ';'])
        && !segment.chars().any(char::is_control)
}

/// The service whose `portal.exclude` rule lets this request through without a session, if any.
///
/// The check is conservative. The gate reads the path in every way that a proxy or a backend
/// might read it ([`path_readings`]). Every reading must belong to the same service and must
/// match one of its rules. `/auth/*` is never excluded. A URI that cannot be read safely is
/// never excluded. A rule applies only to the service that declared it and owns the path
/// (longest prefix).
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

/// The segments of a path on this host, after the gate resolves dot segments and empty segments.
pub type Segments = Vec<String>;

/// The path prefix that a service owns, taken from its `href`. The gate reads the path of the
/// `href` as it reads a request path: one percent-decode for each segment, then the dot segments
/// and the empty segments. Thus the `href` `/my%20app/` owns the request `/my%20app/x`.
///
/// The result is `None` for an absolute URL. Such a URL lives on another host, and nothing here
/// can gate it. The result is also `None` for `/`, because that prefix would own everything. The
/// result is also `None` for a path that the gate cannot read. The label parser refuses such an
/// `href` ([`local_href_is_readable`]).
pub fn href_prefix(href: &str) -> Option<Segments> {
    if !href.starts_with('/') || href.starts_with("//") {
        return None;
    }
    let path = href.split(['?', '#']).next().unwrap_or("");
    let mut decoded = Vec::new();
    for raw in path.split('/') {
        decoded.push(percent_decode(raw)?);
    }
    let segments = clean(decoded.iter().map(String::as_str));
    if segments.is_empty() {
        None
    } else {
        Some(segments)
    }
}

/// Whether the gate reads the path of a local `href` in one way only. Each percent escape must be
/// complete. Each decoded segment must be UTF-8 text without a control character, `/` or `\`. An
/// encoded `/` or a `\` lets a backend read other segments. Bytes that are not UTF-8 could give
/// two `href` values the same prefix.
pub fn local_href_is_readable(href: &str) -> bool {
    let path = href.split(['?', '#']).next().unwrap_or("");
    path.split('/').all(|raw| {
        percent_decode_bytes(raw)
            .and_then(|bytes| String::from_utf8(bytes).ok())
            .is_some_and(|text| {
                !text
                    .chars()
                    .any(|c| c == '/' || c == '\\' || c.is_control())
            })
    })
}

/// Every reading of the path of `uri`. The result is `None` when the URI cannot be read safely,
/// for example because of a NUL byte or a broken percent escape. The gate refuses such a request.
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
    // As a backend that decodes the path before it routes: an encoded slash is a separator.
    let slashes = clean(decoded.iter().flat_map(|s| s.split('/')));
    // Also, a backslash is read as a separator.
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
    let out = percent_decode_bytes(raw)?;
    if out.contains(&0) {
        return None;
    }
    // Bytes that are not UTF-8 cannot spell a service prefix: the label parser refuses such an
    // `href`. They must only stay distinct from the prefixes.
    Some(String::from_utf8_lossy(&out).into_owned())
}

/// The bytes of a segment after one percent-decode. `None` for a broken escape.
fn percent_decode_bytes(raw: &str) -> Option<Vec<u8>> {
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
    Some(out)
}

fn hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Whether `grants` let the user open the service `service`. `None` is the superadmin, who has
/// all grants. A new user has no grant. Access comes only from a grant, never from an omission.
pub fn may_open(grants: Option<&[Grant]>, service: &str) -> bool {
    match grants {
        None => true,
        Some(held) => holds(held, ACCESS, service),
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Decision {
    /// The request is allowed. `service` is the service that the request belongs to, if any.
    Allow { service: Option<String> },
    /// The request is refused. The refusal does not state which service it concerns. A user cannot
    /// learn the name of a service that is not open to that user.
    Deny,
}

/// The gate. `grants` is `None` for the superadmin.
///
/// A path that no service owns is open to each signed-in user. This rule needs a complete
/// catalogue: an incomplete snapshot cannot prove that a path has no owner. Thus, until the
/// catalogue is complete, only the superadmin passes. A closed prefix (an entry without a
/// service) also lets only the superadmin pass.
///
/// A service also owns a path whose segment only starts with the last segment of its prefix
/// (`/filesX` for `/files/`, see [`Catalog::owner`]). Such a path needs `access` on the
/// service, but it is not a page of the service. The answer names no service, so the request
/// gets no identity token.
pub fn decide(catalog: &Catalog, grants: Option<&[Grant]>, uri: &str) -> Decision {
    if grants.is_some() && !catalog.is_complete() {
        return Decision::Deny;
    }
    let readings = path_readings(uri);
    let Some(readings) = readings else {
        return if grants.is_none() {
            Decision::Allow { service: None }
        } else {
            Decision::Deny
        };
    };
    let mut service = None;
    for (index, reading) in readings.iter().enumerate() {
        let Some(owner) = catalog.owner(reading) else {
            continue;
        };
        let allowed = owner.services.iter().find(|s| may_open(grants, &s.id));
        match allowed {
            // A closed prefix: the superadmin passes, and the request has no service.
            None if grants.is_none() => {}
            None => return Decision::Deny,
            Some(s) if index == 0 && reading.starts_with(&owner.prefix) => {
                service = Some(s.id.clone());
            }
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

    fn svc(name: &str, href: &str) -> crate::catalog::Service {
        let labels: BTreeMap<String, String> = [("portal.name", name), ("portal.href", href)]
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        parse_labels("x", &labels).unwrap().unwrap()
    }

    fn catalog() -> Catalog {
        Catalog::build(
            vec![
                svc("Grafana", "/grafana/"),
                svc("ClickHouse", "/clickhouse/"),
                svc("Notes", "/notes/"),
                svc("Nested", "/grafana/admin/"),
                svc("External", "https://example.org/"),
            ],
            Vec::new(),
        )
    }

    /// `access` on each named service. The ids have the form `other-<name>`.
    fn grants(services: &[&str]) -> Vec<Grant> {
        services
            .iter()
            .map(|s| Grant::new(ACCESS, &format!("other-{s}")).unwrap())
            .collect()
    }

    fn verdict(held: &[&str], uri: &str) -> Decision {
        decide(&catalog(), Some(&grants(held)), uri)
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

    /// A snapshot that has not seen every source cannot prove that a path is unowned. Until the
    /// catalogue is complete, only the superadmin passes the gate.
    #[test]
    fn an_incomplete_catalogue_lets_only_the_superadmin_through() {
        let start = Catalog::default();
        assert!(
            !start.is_complete(),
            "the snapshot of the start is not complete"
        );
        for uri in ["/grafana/d/1", "/unowned/asset.js"] {
            assert_eq!(decide(&start, Some(&[]), uri), Decision::Deny, "{uri}");
            assert_eq!(
                decide(&start, Some(&grants(&["grafana"])), uri),
                Decision::Deny,
                "{uri}"
            );
            assert!(matches!(decide(&start, None, uri), Decision::Allow { .. }));
        }
        let mut partial = catalog();
        partial.set_complete(false);
        assert_eq!(
            decide(&partial, Some(&grants(&["grafana"])), "/grafana/d/1"),
            Decision::Deny
        );
        assert!(catalog().is_complete(), "a built catalogue is complete");
    }

    /// A workload with unusable labels, or a dropped duplicate id, keeps its prefix closed.
    #[test]
    fn a_closed_prefix_opens_only_to_the_superadmin() {
        let closed = Catalog::build_with_closed(
            vec![svc("Notes", "/notes/")],
            Vec::new(),
            vec![href_prefix("/grafana/").unwrap()],
        );
        assert_eq!(decide(&closed, Some(&[]), "/grafana/d/1"), Decision::Deny);
        assert_eq!(
            decide(&closed, Some(&grants(&["notes"])), "/grafana/d/1"),
            Decision::Deny
        );
        assert_eq!(
            decide(&closed, None, "/grafana/d/1"),
            Decision::Allow { service: None }
        );
        assert!(matches!(
            decide(&closed, Some(&grants(&["notes"])), "/notes/x"),
            Decision::Allow { .. }
        ));
        assert_eq!(excluded(&closed, Some("GET"), "/grafana/api/health"), None);
    }

    #[test]
    fn a_new_user_opens_nothing_and_the_superadmin_opens_everything() {
        assert!(!allowed(&[], "/notes/api/x"));
        assert!(!allowed(&[], "/grafana/"));
        assert!(matches!(
            decide(&catalog(), None, "/notes/api/x"),
            Decision::Allow { .. }
        ));
    }

    #[test]
    fn a_service_needs_the_access_grant_on_it() {
        assert!(allowed(&["grafana"], "/grafana/d/1"));
        assert!(!allowed(&["clickhouse"], "/grafana/d/1"));
        assert!(allowed(&["clickhouse", "grafana"], "/clickhouse/"));
        let other_action = vec![Grant::new("query.run.read", "other-clickhouse").unwrap()];
        assert!(
            !matches!(
                decide(&catalog(), Some(&other_action), "/clickhouse/"),
                Decision::Allow { .. }
            ),
            "only access opens the service"
        );
    }

    /// Traefik reads ``PathPrefix(`/grafana`)`` as a text prefix: it routes `/grafana-evil/` and
    /// `/grafanaapi/` to Grafana. The gate judges these paths as Grafana paths, without a token.
    #[test]
    fn the_owner_is_the_longest_prefix_as_a_router_reads_it() {
        for uri in [
            "/grafana-evil/",
            "/grafanaapi/admin/users",
            "/notes.json",
            "/notesX/secret",
            "/clickhouse-private/x",
        ] {
            assert_eq!(verdict(&[], uri), Decision::Deny, "{uri}");
        }
        assert_eq!(
            verdict(&["grafana"], "/grafana-evil/"),
            Decision::Allow { service: None },
            "the holder passes, but the request is not a page of the service: no token"
        );
        assert!(
            !allowed(&["grafana"], "/grafana/admin/x"),
            "the nested service needs its own grant"
        );
        assert!(allowed(&["grafana", "nested"], "/grafana/admin/x"));
        assert!(
            !allowed(&["grafana"], "/grafana/administrator"),
            "PathPrefix(`/grafana/admin`) routes it to the nested service"
        );
        assert!(allowed(&["grafana", "nested"], "/grafana/administrator"));
        assert_eq!(
            verdict(&["grafana"], "/grafana"),
            Decision::Allow {
                service: Some("other-grafana".into())
            },
            "an exact segment is a page of the service"
        );
    }

    /// The router of the portal for `/auth/` wins over each router of a service. A service whose
    /// last segment is a text prefix of `auth` does not own the pages of the portal.
    #[test]
    fn a_text_prefix_does_not_take_the_pages_of_the_portal() {
        let catalog = Catalog::build(vec![svc("A", "/a/"), svc("Au", "/au/x/")], Vec::new());
        let none: Vec<Grant> = Vec::new();
        for uri in ["/auth/denied", "/auth/settings", "/auth/x/y"] {
            assert_eq!(
                decide(&catalog, Some(&none), uri),
                Decision::Allow { service: None },
                "{uri}"
            );
        }
        // `/auth` alone and `/authx/` do not match the router `PathPrefix(`/auth/`)`.
        assert_eq!(decide(&catalog, Some(&none), "/auth"), Decision::Deny);
        assert_eq!(decide(&catalog, Some(&none), "/authx/y"), Decision::Deny);
        assert_eq!(decide(&catalog, Some(&none), "/apps/y"), Decision::Deny);
    }

    /// The gate reads the `href` with the same percent-decode as the request path.
    #[test]
    fn an_escaped_href_owns_the_decoded_request_path() {
        assert_eq!(href_prefix("/my%20app/").unwrap(), ["my app"]);
        assert_eq!(href_prefix("/caf%C3%A9/").unwrap(), ["café"]);
        assert_eq!(href_prefix("/a/%2e%2e/b/").unwrap(), ["b"]);
        assert!(href_prefix("/a%zz/").is_none());
        let catalog = Catalog::build(
            vec![
                svc("My app", "/my%20app/"),
                svc("Cafe", "/caf%C3%A9/"),
                svc("Notes", "/notes/"),
            ],
            Vec::new(),
        );
        let notes = grants(&["notes"]);
        for uri in [
            "/my%20app/",
            "/my%20app/admin/settings",
            "/caf%C3%A9/x",
            "/café/x",
        ] {
            assert_eq!(decide(&catalog, Some(&notes), uri), Decision::Deny, "{uri}");
        }
        assert_eq!(
            decide(&catalog, Some(&grants(&["my-app"])), "/my%20app/x"),
            Decision::Allow {
                service: Some("other-my-app".into())
            },
            "the holder gets the token of the service"
        );
        // A closed prefix from an escaped href also stays closed.
        let closed = Catalog::build_with_closed(
            vec![svc("Notes", "/notes/")],
            Vec::new(),
            vec![href_prefix("/my%20app/").unwrap()],
        );
        assert_eq!(decide(&closed, Some(&notes), "/my%20app/x"), Decision::Deny);
        assert_eq!(
            decide(&closed, None, "/my%20app/x"),
            Decision::Allow { service: None }
        );
    }

    #[test]
    fn a_local_href_must_read_in_one_way() {
        for good in [
            "/grafana/",
            "/my%20app/",
            "/caf%C3%A9/",
            "/café/",
            "/a?x=%zz",
        ] {
            assert!(local_href_is_readable(good), "{good}");
        }
        for bad in [
            "/a%zz/", "/a%2/", "/a%00b/", "/a%2Fb/", "/a%5Cb/", "/a\\b/", "/a%0Ab/", "/%FF/",
        ] {
            assert!(!local_href_is_readable(bad), "{bad}");
        }
    }

    #[test]
    fn paths_that_no_service_owns_stay_open() {
        assert!(allowed(&[], "/app.js"));
        assert!(allowed(&[], "/auth/denied"));
        assert!(allowed(&[], "/"));
    }

    #[test]
    fn path_tricks_cannot_reach_a_service_the_user_cannot_open() {
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
                !allowed(&["notes", "grafana"], uri),
                "{uri} must be refused to a user without access to clickhouse"
            );
        }
        // The superadmin is never blocked.
        assert!(matches!(
            decide(&catalog(), None, "/clickhouse%00/"),
            Decision::Allow { .. }
        ));
    }

    #[test]
    fn a_refusal_does_not_name_the_service() {
        assert_eq!(verdict(&["clickhouse"], "/grafana/d/1"), Decision::Deny);
        assert_eq!(verdict(&["grafana"], "/clickhouse%00/"), Decision::Deny);
    }

    #[test]
    fn the_service_a_request_belongs_to_is_reported_for_the_history() {
        assert_eq!(
            verdict(&["grafana"], "/grafana/d/1"),
            Decision::Allow {
                service: Some("other-grafana".into())
            }
        );
        assert_eq!(
            verdict(&["grafana"], "/app.js"),
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
        assert!(open(
            Some("POST"),
            "/dagu/api/v1/webhooks/abc?token=1&x=/../y"
        ));
        assert!(!open(Some("GET"), "/dagu/api/v1/webhooks/abc"));
        assert!(
            !open(None, "/dagu/api/v1/webhooks/abc"),
            "no method, no match for a rule that names one"
        );
        assert!(open(Some("GET"), "/dagu/api/v1/health"));
        assert!(!open(Some("POST"), "/dagu/api/v1/health"));
        assert!(open(Some("PUT"), "/dagu/hooks/a/b/c"));
        assert!(
            !open(Some("PUT"), "/dagu/hooks"),
            "** needs at least one segment"
        );
    }

    /// The audit lead portal/gate/exclude-wildcard-accepts-path-parameter-dot-segment: a backend
    /// that removes path parameters reads `..;` as `..`. A wildcard never stands for a segment
    /// with a `;`, raw or encoded. Thus `/dagu/hooks/..;/..;/admin` is not a hook.
    #[test]
    fn a_wildcard_never_stands_for_a_segment_with_a_path_parameter() {
        for uri in [
            "/dagu/hooks/..;/..;/admin/api/v1/dags",
            "/dagu/hooks/..;x=1/..;x=1/admin",
            "/dagu/hooks/%2e%2e;/admin",
            "/dagu/hooks/..%3B/admin",
            "/dagu/hooks/..%3b",
            "/dagu/hooks/.;/x",
            "/dagu/hooks/a/..;",
            "/dagu/hooks/a;b",
            "/dagu/api/v1/webhooks/..;",
            "/dagu/api/v1/webhooks/%2e%2e%3B",
        ] {
            assert!(!open(Some("POST"), uri), "{uri}");
            assert!(!open(Some("PUT"), uri), "{uri}");
        }
        // The same paths without the parameter: the hooks stay open, the dot segments resolve.
        assert!(open(Some("POST"), "/dagu/hooks/a/admin"));
        assert!(open(Some("POST"), "/dagu/api/v1/webhooks/abc"));
        assert!(!open(Some("POST"), "/dagu/hooks/../../admin/api/v1/dags"));
        assert!(!plain(".") && !plain(".."));
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
        assert!(!open(
            Some("POST"),
            "/dagu/api/v1/webhooks/%2e%2e/%2e%2e/auth/settings"
        ));
        assert!(!open(Some("POST"), "/dagu/api/v1/webhooks/a%00b"));
        assert!(!open(Some("POST"), "/dagu/api/v1/webhooks/a%zz"));
        // Dot segments that cancel each other still lead to a declared path.
        // The gate judges the request as that path.
        assert!(open(Some("GET"), "/dagu/api/v1/webhooks/../health"));
        assert!(!open(Some("POST"), "/dagu/api/v1/webhooks/../health"));
    }

    #[test]
    fn dot_segments_cannot_carry_a_request_out_of_the_service() {
        // The gate cleans the path first. These are requests for /test, /grafana/... or /auth/..., not for Dagu.
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
        // A rule itself cannot contain a dot segment.
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
        assert!(
            !open(Some("POST"), "/dagu/admin/hooks/x"),
            "owned by Dagu admin, which declares nothing"
        );
        assert!(open(Some("POST"), "/dagu/hooks/x"));
    }

    #[test]
    fn rules_are_validated_when_the_label_is_read() {
        let parse = |line: &str| ExcludeRule::parse(line);
        assert_eq!(
            parse("post /a/b").unwrap_err(),
            "\"post\" is not one of GET, HEAD, POST, PUT, PATCH, DELETE, OPTIONS"
        );
        for bad in [
            "a/b",
            "/",
            "/a//b",
            "/a/b/",
            "/*/b",
            "/**",
            "/a/**/b",
            "/a/./b",
            "/a/../b",
            "/a/b?x=1",
            "/a/b#x",
            "/a/[0-9]+",
            "/a/b.*",
            "/a/fo*",
            "/a/%2e",
            "/a/b c d",
            "GET POST /a",
            "",
        ] {
            assert!(parse(bad).is_err(), "{bad:?} must be refused");
        }
        let rule = parse("POST,POST,PUT /a/*/c").unwrap();
        assert_eq!(rule.text(), "POST,PUT /a/*/c");
        assert!(rule.starts_with(&["a".to_string()]));
        assert!(!rule.starts_with(&["b".to_string()]));
        assert!(
            !rule.starts_with(&["a".to_string(), "x".to_string()]),
            "a wildcard is not the written-out prefix"
        );
    }
}
