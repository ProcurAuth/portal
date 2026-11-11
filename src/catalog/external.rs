//! External services: a name, a link, a description and an icon.
//!
//! An external service is not a workload. It needs no container and no IngressRoute. It is only a tile of the
//! launcher that opens a link. The portal never requests the link, so it cannot tell if the link works. It does not
//! protect the link either: anyone who knows the address can open it. The only right is the grant `access`, and it
//! only shows or hides the tile.
//!
//! An external service is never a client of the portal. It has no actions document, no identity token, no PDP
//! question, no health probe and no path in the gate, even when its link points to the host of the portal.
//!
//! Two sources give external services: the blocks `external_service` of the file, and the rows that the superadmin
//! adds on the Settings page. The id is [`ID_PREFIX`] and the slug of the name. The id is the resource id of the grant
//! `access`. A service of the file wins over a row of the database with the same id. A service that discovery lists
//! with the same id wins over both.
//!
//! The link rule is one rule for the four places that read a link: the file, the form, the rows of the database and
//! the tile. See [`check_link`].

use std::net::Ipv6Addr;
use std::sync::Arc;

use super::image::Image;
use super::labels::slug;
use super::{Group, Health, Service};
use crate::grants::{check_service_id, MAX_SERVICE_CHARS};

/// The group of every external service.
pub const GROUP: &str = "External";
/// The start of the id of every external service.
pub const ID_PREFIX: &str = "ext-";
/// The most external services that the portal keeps in its database.
pub const MAX_SERVICES: usize = 256;
/// The longest name, in characters.
pub const MAX_NAME_CHARS: usize = 32;
/// The longest link, in bytes.
pub const MAX_HREF_BYTES: usize = 2048;
/// The longest description, in characters.
pub const MAX_DESCRIPTION_CHARS: usize = 200;
/// The start of the path of the icon of an external service. The id follows, percent-encoded.
pub const ICON_PREFIX: &str = "/auth/ext-icon/";

/// One external service, as the database or the file keeps it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ExternalEntry {
    pub id: String,
    pub name: String,
    /// An absolute http or https URL that follows [`check_link`].
    pub href: String,
    /// At most [`MAX_DESCRIPTION_CHARS`] characters, with no control character. Empty: no description.
    pub description: String,
    /// The own icon. `None`: the tile uses the icon of the library, then the first letter.
    pub icon: Option<Arc<Image>>,
    pub created_at: i64,
    /// The service comes from a block `external_service` of the file. It is read-only.
    pub from_file: bool,
}

/// Why the portal refuses a name, a link or a description. Each text is for the person who typed the value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    Name,
    Link,
    Description,
}

/// Checks the name, and gives it without the spaces around it and the id that it makes. The name has 1 to
/// [`MAX_NAME_CHARS`] characters, no control character, and at least one letter or digit.
pub fn check_name(raw: &str) -> Result<(String, String), Refusal> {
    let name = raw.trim();
    if name.is_empty()
        || name.chars().count() > MAX_NAME_CHARS
        || name.chars().any(char::is_control)
    {
        return Err(Refusal::Name);
    }
    let body = slug(name);
    let id = format!("{ID_PREFIX}{body}");
    if body.is_empty() || id.chars().count() > MAX_SERVICE_CHARS || check_service_id(&id).is_err() {
        return Err(Refusal::Name);
    }
    Ok((name.to_string(), id))
}

/// Checks the link of the form, and gives it without the spaces around it. The portal then applies [`check_link`].
/// It changes nothing else.
pub fn check_href(raw: &str) -> Result<String, Refusal> {
    let href = raw.trim();
    check_link(href).map_err(|_| Refusal::Link)?;
    Ok(href.to_string())
}

/// Checks the description of the form, and gives it without the spaces around it.
pub fn check_description(raw: &str) -> Result<String, Refusal> {
    let text = raw.trim();
    check_description_text(text).map_err(|_| Refusal::Description)?;
    Ok(text.to_string())
}

/// The rule of the description: 0 to [`MAX_DESCRIPTION_CHARS`] characters, with no control character. The file and
/// the database apply it as it is.
pub fn check_description_text(text: &str) -> Result<(), &'static str> {
    if text.chars().count() > MAX_DESCRIPTION_CHARS {
        return Err("the description has more than 200 characters");
    }
    if text.chars().any(char::is_control) {
        return Err("the description has a control character");
    }
    Ok(())
}

/// The rule of the link. The file, the form and the database apply it as it is: the function changes nothing, and
/// a space around the link is a refusal.
///
/// - The link is an absolute URL with the scheme `http` or `https` in lower case, 1 to [`MAX_HREF_BYTES`] bytes.
/// - It has no space, no control character and no backslash.
/// - The host is a DNS name (RFC 1123: ASCII labels of 1 to 63 letters, digits and hyphens, with no hyphen at the
///   start or at the end, 253 characters at most), a dotted IPv4 address, or an IPv6 address in square brackets. A
///   host that has only digits and dots must be an IPv4 address.
/// - The URL has no user information (`user:password@host`). The port is a number from 1 to 65535, without a
///   leading zero.
/// - The path, the query and the fragment are free.
pub fn check_link(href: &str) -> Result<(), &'static str> {
    if href.is_empty() {
        return Err("the link is empty");
    }
    if href.len() > MAX_HREF_BYTES {
        return Err("the link has more than 2048 bytes");
    }
    if href.chars().any(|c| c.is_control()) {
        return Err("the link has a control character");
    }
    if href.chars().any(char::is_whitespace) {
        return Err("the link has a space");
    }
    if href.contains('\\') {
        return Err("the link has a backslash");
    }
    let Some(rest) = href
        .strip_prefix("https://")
        .or_else(|| href.strip_prefix("http://"))
    else {
        return Err("the link is not an absolute URL with the scheme http or https");
    };
    let authority = &rest[..rest.find(['/', '?', '#']).unwrap_or(rest.len())];
    if authority.contains('@') {
        return Err("the link has user information (user@host)");
    }
    let (host, port) = match authority.strip_prefix('[') {
        // An IPv6 address: the port, if any, follows the bracket.
        Some(inner) => match inner.split_once(']') {
            Some((address, "")) => {
                check_ipv6(address)?;
                (address, None)
            }
            Some((address, tail)) if tail.starts_with(':') => {
                check_ipv6(address)?;
                (address, Some(&tail[1..]))
            }
            _ => return Err("the IPv6 address has no closing bracket, or text follows it"),
        },
        None => match authority.split_once(':') {
            Some((host, port)) => {
                check_host(host)?;
                (host, Some(port))
            }
            None => {
                check_host(authority)?;
                (authority, None)
            }
        },
    };
    debug_assert!(!host.is_empty());
    if let Some(port) = port {
        let digits = !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit());
        if !digits || port.len() > 5 || port.starts_with('0') {
            return Err("the port is not a number from 1 to 65535");
        }
        if !port.parse::<u32>().is_ok_and(|n| (1..=65_535).contains(&n)) {
            return Err("the port is not a number from 1 to 65535");
        }
    }
    Ok(())
}

fn check_ipv6(address: &str) -> Result<(), &'static str> {
    address
        .parse::<Ipv6Addr>()
        .map(|_| ())
        .map_err(|_| "the host in square brackets is not an IPv6 address")
}

/// A DNS name or a dotted IPv4 address.
fn check_host(host: &str) -> Result<(), &'static str> {
    if host.is_empty() {
        return Err("the link has no host");
    }
    if host.bytes().all(|b| b.is_ascii_digit() || b == b'.') {
        return check_ipv4(host);
    }
    if !host.is_ascii() {
        return Err("the host has a character that is not ASCII (write an internationalized name in its xn-- form)");
    }
    if host.len() > 253 {
        return Err("the host has more than 253 characters");
    }
    for label in host.split('.') {
        if label.is_empty() {
            return Err("the host has an empty label (two dots, or a dot at the start or the end)");
        }
        if label.len() > 63 {
            return Err("a label of the host has more than 63 characters");
        }
        if label.starts_with('-') || label.ends_with('-') {
            return Err("a label of the host starts or ends with a hyphen");
        }
        if !label
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            return Err(
                "a label of the host has a character other than a letter, a digit and a hyphen",
            );
        }
    }
    Ok(())
}

/// Four numbers from 0 to 255 with dots, without a leading zero.
fn check_ipv4(host: &str) -> Result<(), &'static str> {
    const BAD: &str = "the host has only digits and dots, and it is not an IPv4 address (four numbers from 0 to 255)";
    let parts: Vec<&str> = host.split('.').collect();
    if parts.len() != 4 {
        return Err(BAD);
    }
    for part in parts {
        let good = !part.is_empty()
            && part.len() <= 3
            && (part == "0" || !part.starts_with('0'))
            && part.parse::<u16>().is_ok_and(|n| n <= 255);
        if !good {
            return Err(BAD);
        }
    }
    Ok(())
}

/// Checks a row or a block as stored: the name, the id, the link, the description and the icon. The error names the
/// part and says why. It does not trim a value: a stored value is already as the rule wants it.
pub fn check_entry(entry: &ExternalEntry) -> Result<(), String> {
    match check_name(&entry.name) {
        Ok((name, id)) if name == entry.name && id == entry.id => {}
        Ok((_, id)) => {
            return Err(format!(
                "name {:?}: the name has spaces around it, or the id {:?} is not the id {id:?} that the name makes",
                entry.name, entry.id
            ))
        }
        Err(_) => {
            return Err(format!(
                "name {:?}: expected 1 to {MAX_NAME_CHARS} characters, with no control character and at least one letter or digit",
                entry.name
            ))
        }
    }
    check_link(&entry.href).map_err(|reason| {
        format!(
            "href {:?}: expected an absolute http or https URL with a valid host ({reason})",
            shown(&entry.href)
        )
    })?;
    check_description_text(&entry.description).map_err(|reason| {
        format!(
            "description {:?}: expected at most {MAX_DESCRIPTION_CHARS} characters with no control character ({reason})",
            shown(&entry.description)
        )
    })?;
    Ok(())
}

/// The first 200 characters of a value, for an error.
fn shown(text: &str) -> String {
    let mut out: String = text.chars().take(200).collect();
    if text.chars().count() > 200 {
        out.push_str("...");
    }
    out
}

/// The path of the icon of an entry.
pub fn icon_path(id: &str) -> String {
    format!(
        "{ICON_PREFIX}{}",
        crate::util::percent_encode_query_component(id)
    )
}

/// The tile of an entry: a service with the description and the icon of the entry, no health probe and no actions.
pub fn service(entry: &ExternalEntry) -> Service {
    Service {
        id: entry.id.clone(),
        group: GROUP.to_string(),
        name: entry.name.clone(),
        description: entry.description.clone(),
        icon: None,
        own_icon: entry.icon.as_ref().map(|_| icon_path(&entry.id)),
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

/// The external services that apply: the services of the file, then the rows of the database whose id the file does
/// not hold. A service of the file wins, and the portal ignores the row of the database with the same id. It does
/// not delete the row.
pub fn effective(file: &[ExternalEntry], database: &[ExternalEntry]) -> Vec<ExternalEntry> {
    let mut out: Vec<ExternalEntry> = file.to_vec();
    out.extend(
        database
            .iter()
            .filter(|row| !file.iter().any(|f| f.id == row.id))
            .cloned(),
    );
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
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
                "{}: a discovered service has this id, so it wins over the external service {:?}: {}",
                entry.id,
                entry.name,
                if entry.from_file {
                    "remove or rename the block external_service in the file"
                } else {
                    "delete the external service"
                }
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
            ("", Err(Refusal::Name)),
            ("   ", Err(Refusal::Name)),
            ("---", Err(Refusal::Name)),
            ("!!!", Err(Refusal::Name)),
            ("Line\nbreak", Err(Refusal::Name)),
            ("Tab\there", Err(Refusal::Name)),
            ("nul\0", Err(Refusal::Name)),
        ];
        for (raw, expected) in table {
            assert_eq!(
                name(raw).as_deref().map_err(|e| *e),
                expected.as_ref().map(|s| *s).map_err(|e| *e),
                "{raw:?}"
            );
        }
        assert!(name(&"a".repeat(MAX_NAME_CHARS)).is_ok());
        assert_eq!(name(&"a".repeat(MAX_NAME_CHARS + 1)), Err(Refusal::Name));
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
            "http://0.0.0.0/",
            "http://255.255.255.255:65535/",
            "http://[::1]:8080/x",
            "http://[2001:db8::1]/",
            "http://[::ffff:192.0.2.1]/",
            "https://sub.example.org/%C3%A9",
            "https://nas/",
            "http://localhost:8080",
            "https://xn--caf-dma.example/",
            "https://a-b.c-d.example.org/",
            "https://1.2.3.4.example.org/",
            "https://example.org/a@b",
            "https://example.org/?next=https://other.example/",
            "https://example.org/#frag",
            "https://example.org?x=1",
            "https://example.org/caf\u{e9}",
            // A link to the host of the portal is only a link.
            "https://portal.example.org/auth/verify",
        ];
        for link in accepted {
            assert_eq!(check_link(link), Ok(()), "{link:?}");
            assert_eq!(check_href(link).as_deref(), Ok(link), "{link:?}");
        }
        // The form removes the spaces around the link. The rule itself does not.
        assert_eq!(
            check_href("  https://example.org/trim  ").as_deref(),
            Ok("https://example.org/trim")
        );
        assert!(check_link(" https://example.org/").is_err());
        assert!(check_link("https://example.org/ ").is_err());
        let refused = [
            "",
            "   ",
            "/grafana/",
            "//example.org/",
            "example.org",
            "ftp://example.org/",
            "HTTPS://example.org/",
            "javascript:alert(1)",
            "data:text/html,x",
            "https://",
            "https:///path",
            "https://:8080/",
            // The host rules.
            "https://caf\u{e9}.example/",
            "https://\u{4f8b}\u{3048}.jp/",
            "https://exa_mple.org/",
            "https://example..org/",
            "https://.example.org/",
            "https://example.org./",
            "https://-example.org/",
            "https://example-.org/",
            "https://ex-.example.org/",
            "https://exam%70le.org/",
            "https://exa*mple.org/",
            // A host of digits and dots is an IPv4 address or nothing.
            "http://1.2.3/",
            "http://1.2.3.4.5/",
            "http://256.1.1.1/",
            "http://1.2.3.04/",
            "http://01.2.3.4/",
            "http://1..3.4/",
            "http://1.2.3./",
            "http://.1.2.3/",
            "http://123/",
            "http://999.999.999.999/",
            // IPv6.
            "https://[::1/x",
            "https://[]/x",
            "https://[::1]x/",
            "https://[::1]:/",
            "https://[::g]/",
            "https://[1:2:3]/",
            "https://[::1%25eth0]/",
            "https://::1/",
            "https://[v1.x]/",
            // The port.
            "https://example.org:/x",
            "https://example.org:0/x",
            "https://example.org:65536/x",
            "https://example.org:99999/x",
            "https://example.org:080/x",
            "https://example.org:80a/x",
            "https://example.org:-1/x",
            "https://example.org:+80/x",
            "https://example.org:80:80/x",
            "https://[::1]:0/",
            "https://[::1]:65536/",
            // User information.
            "https://user@example.org/",
            "https://user:secret@example.org/",
            "https://@example.org/",
            "https://example.org@evil.example/",
            "https://example.org\\@evil.example/",
            "https://[::1]@example.org/",
            // Spaces, control characters and backslashes anywhere.
            "https://exa mple.org/",
            "https://example.org/a b",
            "https://example.org/?q=a b",
            "https://example.org/#a b",
            "https://example.org/\u{7}",
            "https://example.org/\nx",
            "https://example.org/\tx",
            "https://example.org/\u{85}x",
            "https://example.org/\u{a0}x",
            "https://example.org/\u{3000}x",
            "https://example.org/a\\b",
            "https://example.org\\a",
        ];
        for link in refused {
            assert!(check_link(link).is_err(), "{link:?}");
            assert_eq!(check_href(link), Err(Refusal::Link), "{link:?}");
        }
    }

    #[test]
    fn the_limits_of_the_link_and_of_the_host_hold() {
        let long = format!("https://example.org/{}", "a".repeat(MAX_HREF_BYTES));
        assert!(check_link(&long).is_err());
        let limit = format!(
            "https://example.org/{}",
            "a".repeat(MAX_HREF_BYTES - "https://example.org/".len())
        );
        assert_eq!(limit.len(), MAX_HREF_BYTES);
        assert_eq!(check_link(&limit), Ok(()));
        // A label of 63 characters is the largest.
        let label63 = "a".repeat(63);
        assert_eq!(check_link(&format!("https://{label63}.org/")), Ok(()));
        let label64 = "a".repeat(64);
        assert!(check_link(&format!("https://{label64}.org/")).is_err());
        // A host of 253 characters is the largest: three labels of 63 and one of 61 with the dots.
        let host253 = format!("{label63}.{label63}.{label63}.{}", "b".repeat(61));
        assert_eq!(host253.len(), 253);
        assert_eq!(check_link(&format!("https://{host253}/")), Ok(()));
        let host254 = format!("{label63}.{label63}.{label63}.{}", "b".repeat(62));
        assert_eq!(host254.len(), 254);
        assert!(check_link(&format!("https://{host254}/")).is_err());
        // The reason names the rule.
        assert!(check_link("http://1.2.3/").unwrap_err().contains("IPv4"));
        assert!(check_link("https://caf\u{e9}.example/")
            .unwrap_err()
            .contains("ASCII"));
        assert!(check_link(&format!("https://{label64}.org/"))
            .unwrap_err()
            .contains("63"));
        assert!(check_link("https://example.org:0/")
            .unwrap_err()
            .contains("port"));
    }

    #[test]
    fn the_description_has_200_characters_and_no_control_character() {
        assert_eq!(check_description("").as_deref(), Ok(""));
        assert_eq!(
            check_description("  Team wiki  ").as_deref(),
            Ok("Team wiki")
        );
        assert_eq!(check_description_text(&"\u{e9}".repeat(200)), Ok(()));
        assert!(check_description_text(&"\u{e9}".repeat(201)).is_err());
        assert_eq!(
            check_description(&"a".repeat(201)),
            Err(Refusal::Description)
        );
        for bad in ["a\nb", "a\tb", "a\u{7}b", "a\u{85}b", "a\0b"] {
            assert_eq!(check_description(bad), Err(Refusal::Description), "{bad:?}");
            assert!(check_description_text(bad).is_err());
        }
        // The rule applied to a stored value does not trim.
        assert!(check_description_text("  x ").is_ok());
    }

    fn entry(name: &str, href: &str) -> ExternalEntry {
        let (name, id) = check_name(name).unwrap();
        ExternalEntry {
            id,
            name,
            href: href.to_string(),
            ..ExternalEntry::default()
        }
    }

    #[test]
    fn a_stored_entry_is_checked_as_it_is() {
        assert_eq!(
            check_entry(&entry("Wiki", "https://wiki.example.org/")),
            Ok(())
        );
        let mut e = entry("Wiki", "https://wiki.example.org/");
        e.description = "The team wiki".into();
        assert_eq!(check_entry(&e), Ok(()));
        // A bad link names the part, the value and the reason.
        let mut bad = entry("Wiki", "https://wiki.example.org/");
        bad.href = "http://1.2.3/".into();
        let error = check_entry(&bad).unwrap_err();
        assert!(error.starts_with("href \"http://1.2.3/\""), "{error}");
        assert!(error.contains("IPv4"), "{error}");
        // A stored link with a space around it is a refusal: the rule does not trim.
        bad.href = " https://wiki.example.org/".into();
        assert!(check_entry(&bad).is_err());
        // A name that does not make the id.
        let mut bad = entry("Wiki", "https://wiki.example.org/");
        bad.id = "ext-other".into();
        assert!(check_entry(&bad).unwrap_err().contains("ext-other"));
        let mut bad = entry("Wiki", "https://wiki.example.org/");
        bad.name = " Wiki".into();
        assert!(check_entry(&bad).is_err());
        // A bad description.
        let mut bad = entry("Wiki", "https://wiki.example.org/");
        bad.description = "a".repeat(201);
        assert!(check_entry(&bad).unwrap_err().starts_with("description"));
    }

    #[test]
    fn the_file_wins_over_the_database_for_one_id() {
        let mut file = entry("Wiki", "https://file.example.org/");
        file.from_file = true;
        let row = entry("Wiki", "https://row.example.org/");
        let other = entry("Docs", "https://docs.example.org/");
        let merged = effective(&[file.clone()], &[row, other.clone()]);
        assert_eq!(merged, vec![other, file]);
        assert!(merged
            .iter()
            .filter(|e| e.id == "ext-wiki")
            .all(|e| e.from_file));
        assert_eq!(effective(&[], &[]), Vec::new());
    }

    #[test]
    fn the_tile_has_the_description_and_the_path_of_the_own_icon() {
        let mut e = entry("Caf\u{e9} 2", "https://x.example.org/");
        e.description = "A description".into();
        let tile = service(&e);
        assert_eq!(tile.description, "A description");
        assert_eq!(tile.own_icon, None);
        e.icon = Some(Arc::new(
            Image::from_bytes(
                b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>".to_vec(),
                None,
            )
            .unwrap(),
        ));
        let tile = service(&e);
        assert_eq!(
            tile.own_icon.as_deref(),
            Some("/auth/ext-icon/ext-caf%C3%A9-2")
        );
        assert!(tile.external && tile.icon.is_none());
    }
}
