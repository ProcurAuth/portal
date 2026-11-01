//! The address of the client behind a trusted reverse proxy.

use std::net::{IpAddr, SocketAddr};

use hyper::header::HeaderMap;

use crate::transport::Cidr;

/// Gives the address of the peer, or the address that a trusted proxy announces.
///
/// The function reads `X-Forwarded-For` from the right and skips the trusted proxies. A client
/// therefore cannot choose its address with a header of its own.
pub fn client_ip(peer: SocketAddr, headers: &HeaderMap, trusted: &[Cidr]) -> IpAddr {
    let peer_ip = canonical(peer.ip());
    let is_trusted = |ip: &IpAddr| trusted.iter().any(|n| n.contains(*ip));
    if !is_trusted(&peer_ip) {
        return peer_ip;
    }
    if let Some(xff) = headers.get("x-forwarded-for").and_then(|v| v.to_str().ok()) {
        let mut candidate = None;
        for part in xff.rsplit(',') {
            match part.trim().parse::<IpAddr>() {
                Ok(ip) => {
                    let ip = canonical(ip);
                    candidate = Some(ip);
                    if !is_trusted(&ip) {
                        return ip;
                    }
                }
                Err(_) => break,
            }
        }
        if let Some(ip) = candidate {
            return ip;
        }
    }
    if let Some(ip) = headers
        .get("x-real-ip")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<IpAddr>().ok())
    {
        return canonical(ip);
    }
    peer_ip
}

fn canonical(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => v6
            .to_ipv4_mapped()
            .map(IpAddr::V4)
            .unwrap_or(IpAddr::V6(v6)),
        v4 => v4,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(xff: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert("x-forwarded-for", xff.parse().unwrap());
        h
    }

    #[test]
    fn untrusted_peer_headers_are_ignored() {
        let peer: SocketAddr = "203.0.113.9:1234".parse().unwrap();
        let trusted = vec![Cidr::parse("10.0.0.0/8").unwrap()];
        assert_eq!(client_ip(peer, &headers("1.2.3.4"), &trusted), peer.ip());
    }

    #[test]
    fn trusted_proxy_chain_is_walked_from_the_right() {
        let peer: SocketAddr = "10.0.0.2:1234".parse().unwrap();
        let trusted = vec![Cidr::parse("10.0.0.0/8").unwrap()];
        let ip = client_ip(peer, &headers("6.6.6.6, 198.51.100.7, 10.0.0.5"), &trusted);
        assert_eq!(ip, "198.51.100.7".parse::<IpAddr>().unwrap());
    }
}
