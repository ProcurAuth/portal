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
        "__Host-portal-csrf"
    } else {
        "portal-csrf"
    }
}

/// This function sets the cookie attributes `Path`, `Max-Age`, `HttpOnly`, `SameSite`, and `Secure`.
/// The attribute `Secure` is set over HTTPS only.
pub fn cookie_attributes(https: bool, same_site: &str, max_age_secs: u64) -> String {
    let secure = if https { "; Secure" } else { "" };
    format!("Path=/; Max-Age={max_age_secs}; HttpOnly; SameSite={same_site}{secure}")
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

    #[test]
    fn cookie_names_follow_the_scheme() {
        assert_eq!(session_cookie_name("__Host-portal", true), "__Host-portal");
        assert_eq!(session_cookie_name("__Host-portal", false), "portal");
        assert_eq!(csrf_cookie_name(true), "__Host-portal-csrf");
        assert_eq!(csrf_cookie_name(false), "portal-csrf");
    }

    #[test]
    fn secure_is_set_over_https_only() {
        assert_eq!(
            cookie_attributes(true, "Strict", 60),
            "Path=/; Max-Age=60; HttpOnly; SameSite=Strict; Secure"
        );
        assert_eq!(
            cookie_attributes(false, "Lax", 0),
            "Path=/; Max-Age=0; HttpOnly; SameSite=Lax"
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
}
