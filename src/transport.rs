//! How the browser reached the portal. What a session cookie is tied to.
//!
//! Traefik can serve plain HTTP, for example on a LAN without certificates or in a test stack. It
//! can also serve HTTPS. Browsers refuse `Secure` and `__Host-` cookies on HTTP. The cookie names
//! and attributes therefore follow the scheme of each request. The scheme comes from the
//! `X-Forwarded-Proto` header that Traefik sets. The value `trustForwardHeader: false` applies, so
//! a client cannot choose the header.
//!
//! Over plain HTTP, a cookie can be read on the network. The binding limits the value of a stolen
//! cookie. It works only from the client address and User-Agent that logged in.

use std::net::IpAddr;

use hyper::header::HeaderMap;
use sha2::{Digest, Sha256};

use crate::config::SessionBind;
use crate::session::Binding;

const HOST_PREFIX: &str = "__Host-";
const MAX_USER_AGENT_BYTES: usize = 512;

/// Tells whether the browser used HTTPS. Only a single `https` value counts as HTTPS. Any other
/// value counts as plain HTTP.
pub fn is_https(headers: &HeaderMap) -> bool {
    let mut values = headers.get_all("x-forwarded-proto").iter();
    match (values.next(), values.next()) {
        (Some(value), None) => value
            .to_str()
            .is_ok_and(|v| v.trim().eq_ignore_ascii_case("https")),
        _ => false,
    }
}

/// The session cookie name for this scheme. Over HTTPS, the name is the configured `__Host-` name.
/// Over HTTP, the name is the same name without the prefix.
pub fn session_cookie_name(configured: &str, https: bool) -> &str {
    if https {
        configured
    } else {
        configured.strip_prefix(HOST_PREFIX).unwrap_or(configured)
    }
}

pub fn csrf_cookie_name(https: bool) -> &'static str {
    if https {
        "__Host-lc"
    } else {
        "lc"
    }
}

/// This function sets the cookie attributes `Path`, `Max-Age`, `HttpOnly`, `SameSite`, and `Secure`.
/// The attribute `Secure` is set over HTTPS only.
/// Without `max_age_secs`, the cookie has no `Max-Age`: the browser deletes it at the end of its session.
pub fn cookie_attributes(https: bool, same_site: &str, max_age_secs: Option<u64>) -> String {
    let secure = if https { "; Secure" } else { "" };
    let max_age = max_age_secs.map_or_else(String::new, |secs| format!(" Max-Age={secs};"));
    format!("Path=/;{max_age} HttpOnly; SameSite={same_site}{secure}")
}

/// This function returns the binding of a session that is issued now. The result is a hash. The
/// session store therefore keeps no address.
pub fn binding(mode: SessionBind, headers: &HeaderMap, client_ip: Option<IpAddr>) -> Binding {
    let mut hasher = Sha256::new();
    hasher.update(b"portal-bind\0");
    match mode {
        SessionBind::None => hasher.update(b"none"),
        SessionBind::UserAgent => {
            hasher.update(b"ua\0");
            hasher.update(user_agent(headers));
        }
        SessionBind::IpUserAgent => {
            hasher.update(b"ip-ua\0");
            if let Some(ip) = client_ip {
                hasher.update(ip.to_string().as_bytes());
            }
            hasher.update(b"\0");
            hasher.update(user_agent(headers));
        }
    }
    hasher.finalize().into()
}

/// An IP network: an address and a prefix length, such as `10.42.0.0/16`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cidr {
    network: IpAddr,
    prefix: u8,
}

impl Cidr {
    /// Reads `address/prefix`. The address must be the first address of the network: the bits
    /// after the prefix are zero. The function does not change a value to make it fit.
    pub fn parse(text: &str) -> Result<Self, String> {
        let (address, prefix) = text
            .split_once('/')
            .ok_or_else(|| "expected an address, '/' and a prefix length".to_string())?;
        let network: IpAddr = address
            .parse()
            .map_err(|_| format!("{address:?} is not an IP address"))?;
        let max = if network.is_ipv4() { 32 } else { 128 };
        let prefix: u8 = match prefix.parse::<u8>() {
            Ok(n) if n <= max && (prefix == "0" || !prefix.starts_with('0')) => n,
            _ => {
                return Err(format!(
                    "the prefix length must be a number from 0 to {max}"
                ))
            }
        };
        let cidr = Self { network, prefix };
        if cidr.masked(network) != network {
            return Err(format!(
                "{address} has bits set after the prefix /{prefix}: write the first address of the network"
            ));
        }
        Ok(cidr)
    }

    /// Tells whether `ip` is in the network. An IPv4 address in IPv6 form (`::ffff:a.b.c.d`)
    /// counts as the IPv4 address.
    pub fn contains(&self, ip: IpAddr) -> bool {
        let ip = ip.to_canonical();
        ip.is_ipv4() == self.network.is_ipv4() && self.masked(ip) == self.network
    }

    fn masked(&self, ip: IpAddr) -> IpAddr {
        match ip {
            IpAddr::V4(v4) => {
                let mask = u32::MAX
                    .checked_shl(32 - u32::from(self.prefix))
                    .unwrap_or(0);
                IpAddr::V4((u32::from(v4) & mask).into())
            }
            IpAddr::V6(v6) => {
                let mask = u128::MAX
                    .checked_shl(128 - u32::from(self.prefix))
                    .unwrap_or(0);
                IpAddr::V6((u128::from(v6) & mask).into())
            }
        }
    }
}

/// The address of the client.
///
/// The server reads `X-Forwarded-For` only when the TCP peer is a trusted proxy
/// (`server.trusted_proxies`). The function then reads the header from the right. It skips the
/// addresses of trusted proxies and returns the first address that is not trusted. If every
/// address is trusted, it returns the leftmost one. In every other case it returns the peer:
/// a peer that is not trusted, no header, two headers, or an entry that is not an address.
pub fn client_ip(headers: &HeaderMap, peer: IpAddr, trusted: &[Cidr]) -> IpAddr {
    let peer = peer.to_canonical();
    let is_trusted = |ip: IpAddr| trusted.iter().any(|net| net.contains(ip));
    if !is_trusted(peer) {
        return peer;
    }
    let mut values = headers.get_all("x-forwarded-for").iter();
    let (Some(value), None) = (values.next(), values.next()) else {
        return peer;
    };
    let Ok(text) = value.to_str() else {
        return peer;
    };
    let mut leftmost = peer;
    for part in text.rsplit(',') {
        let Ok(ip) = part.trim().parse::<IpAddr>() else {
            return peer;
        };
        let ip = ip.to_canonical();
        if !is_trusted(ip) {
            return ip;
        }
        leftmost = ip;
    }
    leftmost
}

fn user_agent(headers: &HeaderMap) -> &[u8] {
    let value = headers
        .get(hyper::header::USER_AGENT)
        .map(|v| v.as_bytes())
        .unwrap_or_default();
    &value[..value.len().min(MAX_USER_AGENT_BYTES)]
}

#[cfg(test)]
mod tests {
    use super::*;
    use hyper::header::{HeaderName, HeaderValue};

    fn headers(entries: &[(&'static str, &'static str)]) -> HeaderMap {
        let mut out = HeaderMap::new();
        for (name, value) in entries {
            out.append(
                HeaderName::from_static(name),
                HeaderValue::from_static(value),
            );
        }
        out
    }

    #[test]
    fn only_a_single_https_value_counts_as_https() {
        assert!(is_https(&headers(&[("x-forwarded-proto", "https")])));
        assert!(is_https(&headers(&[("x-forwarded-proto", "HTTPS")])));
        assert!(!is_https(&headers(&[("x-forwarded-proto", "http")])));
        assert!(!is_https(&headers(&[])));
        assert!(!is_https(&headers(&[
            ("x-forwarded-proto", "https"),
            ("x-forwarded-proto", "https")
        ])));
    }

    fn nets(list: &[&str]) -> Vec<Cidr> {
        list.iter().map(|n| Cidr::parse(n).unwrap()).collect()
    }

    fn ip(text: &str) -> IpAddr {
        text.parse().unwrap()
    }

    #[test]
    fn a_network_is_written_exactly() {
        let net = Cidr::parse("10.42.0.0/16").unwrap();
        assert!(net.contains(ip("10.42.3.17")));
        assert!(net.contains(ip("::ffff:10.42.3.17")));
        assert!(!net.contains(ip("10.43.0.1")));
        assert!(Cidr::parse("0.0.0.0/0")
            .unwrap()
            .contains(ip("203.0.113.9")));
        assert!(Cidr::parse("2001:db8::/32")
            .unwrap()
            .contains(ip("2001:db8::7")));
        assert!(!Cidr::parse("2001:db8::/32")
            .unwrap()
            .contains(ip("10.0.0.1")));
        for bad in [
            "10.42.0.1/16",
            "10.42.0.0",
            "10.42.0.0/33",
            "10.42.0.0/016",
            "10.42.0.0/x",
            "host/8",
            "::/129",
        ] {
            assert!(Cidr::parse(bad).is_err(), "{bad}");
        }
    }

    /// Only a trusted proxy chooses the client address. A direct peer cannot choose it with
    /// `X-Forwarded-For`, so it cannot use the session binding or the login limiter of another
    /// address.
    #[test]
    fn x_forwarded_for_counts_only_from_a_trusted_peer() {
        let trusted = nets(&["10.42.0.0/16"]);
        let xff = |value: &'static str| headers(&[("x-forwarded-for", value)]);
        assert_eq!(
            client_ip(&xff("2001:db8::7"), ip("192.0.2.10"), &trusted),
            ip("192.0.2.10"),
            "the peer is not trusted"
        );
        assert_eq!(
            client_ip(&xff("2001:db8::7"), ip("10.42.1.5"), &trusted),
            ip("2001:db8::7")
        );
        assert_eq!(
            client_ip(
                &xff("198.51.100.1, 2001:db8::7, 10.42.0.9"),
                ip("10.42.1.5"),
                &trusted
            ),
            ip("2001:db8::7"),
            "the first address from the right that is not trusted"
        );
        assert_eq!(
            client_ip(&xff("10.42.0.8, 10.42.0.9"), ip("10.42.1.5"), &trusted),
            ip("10.42.0.8"),
            "every address is trusted: the leftmost one"
        );
        assert_eq!(
            client_ip(&xff("2001:db8::7, not-an-ip"), ip("10.42.1.5"), &trusted),
            ip("10.42.1.5"),
            "a broken chain gives the peer"
        );
        assert_eq!(
            client_ip(&headers(&[]), ip("10.42.1.5"), &trusted),
            ip("10.42.1.5")
        );
        assert_eq!(
            client_ip(
                &headers(&[
                    ("x-forwarded-for", "192.0.2.1"),
                    ("x-forwarded-for", "192.0.2.2")
                ]),
                ip("10.42.1.5"),
                &trusted
            ),
            ip("10.42.1.5"),
            "two headers give the peer"
        );
        assert_eq!(
            client_ip(&xff("2001:db8::7"), ip("10.42.1.5"), &[]),
            ip("10.42.1.5"),
            "no trusted proxy: the header never counts"
        );
    }

    #[test]
    fn cookie_names_follow_the_scheme() {
        assert_eq!(session_cookie_name("__Host-portal", true), "__Host-portal");
        assert_eq!(session_cookie_name("__Host-portal", false), "portal");
        assert_eq!(csrf_cookie_name(true), "__Host-lc");
        assert_eq!(csrf_cookie_name(false), "lc");
    }

    #[test]
    fn secure_is_set_over_https_only() {
        assert_eq!(
            cookie_attributes(true, "Strict", Some(60)),
            "Path=/; Max-Age=60; HttpOnly; SameSite=Strict; Secure"
        );
        assert_eq!(
            cookie_attributes(false, "Lax", Some(0)),
            "Path=/; Max-Age=0; HttpOnly; SameSite=Lax"
        );
    }

    #[test]
    fn a_cookie_without_a_life_has_no_max_age() {
        assert_eq!(
            cookie_attributes(true, "Strict", None),
            "Path=/; HttpOnly; SameSite=Strict; Secure"
        );
        assert_eq!(
            cookie_attributes(false, "Lax", None),
            "Path=/; HttpOnly; SameSite=Lax"
        );
    }

    #[test]
    fn binding_depends_on_the_configured_parts() {
        let ip_a: Option<IpAddr> = Some("10.0.0.20".parse().unwrap());
        let ip_b: Option<IpAddr> = Some("10.0.0.21".parse().unwrap());
        let firefox = headers(&[("user-agent", "Firefox/130")]);
        let chrome = headers(&[("user-agent", "Chrome/129")]);

        let full = |ip, h: &HeaderMap| binding(SessionBind::IpUserAgent, h, ip);
        assert_eq!(full(ip_a, &firefox), full(ip_a, &firefox));
        assert_ne!(full(ip_a, &firefox), full(ip_b, &firefox));
        assert_ne!(full(ip_a, &firefox), full(ip_a, &chrome));

        let ua = |ip, h: &HeaderMap| binding(SessionBind::UserAgent, h, ip);
        assert_eq!(ua(ip_a, &firefox), ua(ip_b, &firefox));
        assert_ne!(ua(ip_a, &firefox), ua(ip_a, &chrome));

        let none = |ip, h: &HeaderMap| binding(SessionBind::None, h, ip);
        assert_eq!(none(ip_a, &firefox), none(ip_b, &chrome));
    }

    #[test]
    fn a_missing_user_agent_still_binds_consistently() {
        let ip: Option<IpAddr> = Some("10.0.0.20".parse().unwrap());
        let empty = headers(&[]);
        assert_eq!(
            binding(SessionBind::IpUserAgent, &empty, ip),
            binding(SessionBind::IpUserAgent, &empty, ip)
        );
    }

    /// The audit lead portal/deploy/forwardauth-trust-forward-header-guidance: `/auth/verify` knows
    /// the request only from `X-Forwarded-Uri` and `X-Forwarded-Method`. The Traefik files of the
    /// repository keep `trustForwardHeader: false` on each ForwardAuth middleware. Their comments
    /// never advise `true` without the rule for the hop in front of Traefik, and they state it.
    #[test]
    fn the_traefik_examples_keep_trust_forward_header_false_and_state_the_rule_of_the_hop() {
        let read = |name: &str| {
            std::fs::read_to_string(format!("{}/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
        };
        for name in ["examples/traefik-dynamic.yaml", "demo/traefik-dynamic.yml"] {
            let text = read(name);
            let values: Vec<&str> = text
                .lines()
                .filter_map(|line| line.trim().strip_prefix("trustForwardHeader:"))
                .map(str::trim)
                .collect();
            assert!(!values.is_empty(), "{name} has no forwardAuth middleware");
            assert!(values.iter().all(|v| *v == "false"), "{name}: {values:?}");
        }
        for (name, rule) in [
            (
                "examples/traefik-dynamic.yaml",
                "Set true only when each hop in front of Traefik overwrites or removes the \
                 X-Forwarded-Uri, X-Forwarded-Method",
            ),
            (
                "examples/traefik-entrypoint-snippet.yaml",
                "Keep forwardAuth.trustForwardHeader false",
            ),
        ] {
            // The comments as one text: a sentence can go over two lines.
            let comments = read(name)
                .lines()
                .filter_map(|line| line.trim().strip_prefix('#'))
                .collect::<Vec<_>>()
                .join(" ");
            let comments = comments.split_whitespace().collect::<Vec<_>>().join(" ");
            assert!(
                !comments.contains("trustForwardHeader=true")
                    && !comments.contains("switch this to true"),
                "{name} advises trustForwardHeader true: {comments}"
            );
            assert!(comments.contains(rule), "{name}: {comments}");
        }
    }
}
