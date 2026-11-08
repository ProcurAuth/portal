//! External services: a name and a link that the superadmin adds by hand.
//!
//! An external service is not a workload. It needs no container and no IngressRoute. It is only a tile of the
//! launcher that opens a link. The portal never requests the link, so it cannot tell if the link works. It does not
//! protect the link either: anyone who knows the address can open it. The only right is the grant `access`, and it
//! only shows or hides the tile.
//!
//! An external service is never a client of the portal. It has no actions document, no identity token, no PDP
//! question, no health probe and no path in the gate, even when its link points to the host of the portal.
//!
//! The id is [`ID_PREFIX`] and the slug of the name. The id is the resource id of the grant `access`. The portal
//! refuses a name whose id exists. A service that discovery lists with the same id wins over the external service.

use super::labels::{is_http_url, slug, url_origin};
use super::{Group, Health, Service};
use crate::grants::{check_service_id, MAX_SERVICE_CHARS};

/// The group of every external service.
pub const GROUP: &str = "External";
/// The start of the id of every external service.
pub const ID_PREFIX: &str = "ext-";
/// The most external services that the portal keeps.
pub const MAX_SERVICES: usize = 256;
/// The longest name, in characters.
pub const MAX_NAME_CHARS: usize = 32;
/// The longest link, in bytes.
pub const MAX_HREF_BYTES: usize = 2048;

/// One external service, as the database keeps it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExternalEntry {
    pub id: String,
    pub name: String,
    /// An absolute http or https URL.
    pub href: String,
    pub created_at: i64,
}

/// Why the portal refuses a name or a link. Each text is for the person who typed the value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    BadName,
    BadLink,
}

/// Checks the name, and gives it without the spaces around it and the id that it makes. The name has 1 to
/// [`MAX_NAME_CHARS`] characters, no control character, and at least one letter or digit.
pub fn check_name(raw: &str) -> Result<(String, String), Refusal> {
    let name = raw.trim();
    if name.is_empty()
        || name.chars().count() > MAX_NAME_CHARS
        || name.chars().any(char::is_control)
    {
        return Err(Refusal::BadName);
    }
    let body = slug(name);
    let id = format!("{ID_PREFIX}{body}");
    if body.is_empty() || id.chars().count() > MAX_SERVICE_CHARS || check_service_id(&id).is_err() {
        return Err(Refusal::BadName);
    }
    Ok((name.to_string(), id))
}

/// Checks the link, and gives it without the spaces around it. The rules of `portal.href` for an absolute URL
/// apply: `http://` or `https://`, no space and no control character. The portal also refuses a link with user
/// information (`user:password@host`), a backslash, a missing host, a bad port, and more than [`MAX_HREF_BYTES`]
/// bytes. The portal does not change the link to make it fit.
pub fn check_href(raw: &str) -> Result<String, Refusal> {
    let href = raw.trim();
    if href.is_empty() || href.len() > MAX_HREF_BYTES || href.contains('\\') || !is_http_url(href) {
        return Err(Refusal::BadLink);
    }
    let Some(authority) = url_origin(href).and_then(|origin| origin.split_once("://")) else {
        return Err(Refusal::BadLink);
    };
    let host_port = authority.1;
    if host_port.contains('@') {
        return Err(Refusal::BadLink);
    }
    let (host, port) = match host_port.strip_prefix('[') {
        // An IPv6 address: the port, if any, follows the bracket.
        Some(rest) => match rest.split_once(']') {
            Some((address, "")) => (address, None),
            Some((address, tail)) if tail.starts_with(':') => (address, Some(&tail[1..])),
            _ => return Err(Refusal::BadLink),
        },
        None => match host_port.split_once(':') {
            Some((host, port)) => (host, Some(port)),
            None => (host_port, None),
        },
    };
    let port_is_good = port.is_none_or(|p| {
        p.len() <= 5
            && p.bytes().all(|b| b.is_ascii_digit())
            && p.parse::<u32>().is_ok_and(|n| (1..=65_535).contains(&n))
    });
    if host.is_empty() || !port_is_good {
        return Err(Refusal::BadLink);
    }
    Ok(href.to_string())
}

/// The tile of an entry: a service with no description, no icon, no health probe and no actions.
pub fn service(entry: &ExternalEntry) -> Service {
    Service {
        id: entry.id.clone(),
        group: GROUP.to_string(),
        name: entry.name.clone(),
        description: String::new(),
        icon: None,
        href: entry.href.clone(),
        // The group External is the last row of the launcher: the links come after the services of the portal.
        weight: i32::MAX,
        health_url: None,
        actions_url: None,
        exclude: Vec::new(),
        health: Health::Unknown,
        runtime: None,
        external: true,
    }
}

/// Adds the entries to the groups of a catalogue. A discovered service with the same id wins: the entry then gives
/// a problem and no tile. The function returns the problems.
pub(super) fn merge(groups: &mut Vec<Group>, entries: &[ExternalEntry]) -> Vec<String> {
    let mut problems = Vec::new();
    for entry in entries {
        let taken = groups
            .iter()
            .flat_map(|g| g.services.iter())
            .any(|s| s.id == entry.id);
        if taken {
            problems.push(format!(
                "{}: a discovered service has this id, so it wins over the external service {:?}: delete the external service",
                entry.id, entry.name
            ));
            continue;
        }
        let tile = service(entry);
        match groups.iter_mut().find(|g| g.name == GROUP) {
            Some(group) => group.services.push(tile),
            None => groups.push(Group {
                name: GROUP.to_string(),
                services: vec![tile],
            }),
        }
    }
    problems
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name(raw: &str) -> Result<String, Refusal> {
        check_name(raw).map(|(_, id)| id)
    }

    #[test]
    fn the_name_table_accepts_and_refuses() {
        let table: &[(&str, Result<&str, Refusal>)] = &[
            ("Grafana public", Ok("ext-grafana-public")),
            ("  Wiki  ", Ok("ext-wiki")),
            ("A", Ok("ext-a")),
            ("Café 2", Ok("ext-café-2")),
            ("Docs / Team (v2)", Ok("ext-docs-team-v2")),
            ("", Err(Refusal::BadName)),
            ("   ", Err(Refusal::BadName)),
            ("---", Err(Refusal::BadName)),
            ("!!!", Err(Refusal::BadName)),
            ("Line\nbreak", Err(Refusal::BadName)),
            ("Tab\there", Err(Refusal::BadName)),
            ("nul\0", Err(Refusal::BadName)),
        ];
        for (raw, expected) in table {
            assert_eq!(
                name(raw).as_deref().map_err(|e| *e),
                expected.as_ref().map(|s| *s).map_err(|e| *e),
                "{raw:?}"
            );
        }
        assert!(name(&"a".repeat(MAX_NAME_CHARS)).is_ok());
        assert_eq!(name(&"a".repeat(MAX_NAME_CHARS + 1)), Err(Refusal::BadName));
        // Two names with the same slug have the same id: the store refuses the second one.
        assert_eq!(name("My App"), name("my-app"));
    }

    #[test]
    fn the_longest_name_makes_an_id_that_fits_the_grants() {
        let (_, id) = check_name(&"z".repeat(MAX_NAME_CHARS)).unwrap();
        assert!(id.chars().count() <= MAX_SERVICE_CHARS);
        assert!(check_service_id(&id).is_ok());
    }

    #[test]
    fn the_link_table_accepts_and_refuses() {
        let accepted = [
            "http://example.org",
            "https://example.org/",
            "https://example.org/a/b?x=1&y=2#top",
            "https://example.org:8443/x",
            "http://10.0.0.5:3000/",
            "http://[::1]:8080/x",
            "http://[2001:db8::1]/",
            "https://sub.example.org/%C3%A9",
            "  https://example.org/trim  ",
            // A link to the host of the portal is only a link.
            "https://portal.example.org/auth/verify",
        ];
        for link in accepted {
            assert_eq!(check_href(link).as_deref(), Ok(link.trim()), "{link:?}");
        }
        let refused = [
            "",
            "   ",
            "/grafana/",
            "//example.org/",
            "example.org",
            "ftp://example.org/",
            "javascript:alert(1)",
            "data:text/html,x",
            "https://",
            "https:///path",
            "https://:8080/",
            "https://user@example.org/",
            "https://user:secret@example.org/",
            "https://@example.org/",
            "https://example.org@evil.example/",
            "https://example.org\\@evil.example/",
            "https://example.org:/x",
            "https://example.org:99999/x",
            "https://example.org:0/x",
            "https://example.org:80a/x",
            "https://[::1/x",
            "https://[]/x",
            "https://exa mple.org/",
            "https://example.org/a b",
            "https://example.org/\u{7}",
            "https://example.org/\nx",
            "https://example.org/\u{85}x",
        ];
        for link in refused {
            assert_eq!(check_href(link), Err(Refusal::BadLink), "{link:?}");
        }
        let long = format!("https://example.org/{}", "a".repeat(MAX_HREF_BYTES));
        assert_eq!(check_href(&long), Err(Refusal::BadLink));
        let limit = format!(
            "https://example.org/{}",
            "a".repeat(MAX_HREF_BYTES - "https://example.org/".len())
        );
        assert_eq!(limit.len(), MAX_HREF_BYTES);
        assert_eq!(check_href(&limit).as_deref(), Ok(limit.as_str()));
    }
}
