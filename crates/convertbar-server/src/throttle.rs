//! Per-source login throttling: who is attempting, and how long they must wait.
//!
//! There is deliberately no lockout. A lockout that refuses a correct token is a
//! cheap permanent denial of service against the owner (sustaining it costs
//! ~0.07 req/s), and one that honours a correct token rate-limits nothing —
//! every guess is still evaluated. The escalating delay below makes guessing
//! expensive without ever creating denial state an attacker can trigger.

use std::net::{IpAddr, SocketAddr};

use axum::http::{header::HeaderMap, HeaderValue};
use ipnet::IpNet;

/// A throttling bucket key. IPv6 is keyed on its /64 network, not the address:
/// a SLAAC host owns 2^64 addresses and would otherwise get a fresh bucket per guess.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ClientId {
    Addr(IpAddr),
    /// A trusted chain containing an unparsable entry. Its own bucket, so clients
    /// sending garbage slow only each other rather than the proxy's real clients.
    MalformedChain,
    /// No connect info. Should not occur in production; fails closed (throttled).
    Unknown,
}

const XFF: &str = "x-forwarded-for";

/// Canonicalizes (unwrapping IPv4-mapped IPv6) and, for IPv6, truncates to /64.
fn bucket_addr(addr: IpAddr) -> IpAddr {
    match addr.to_canonical() {
        IpAddr::V4(v4) => IpAddr::V4(v4),
        IpAddr::V6(v6) => {
            let mut octets = v6.octets();
            octets[8..].fill(0);
            IpAddr::V6(octets.into())
        }
    }
}

/// Parses one forwarded-chain entry. Parse first, strip second: `auth.rs`'s
/// `strip_port` splits on the last colon and would turn `2001:db8::1` into
/// `2001:db8:`.
fn parse_entry(entry: &str) -> Option<IpAddr> {
    let entry = entry.trim();
    if let Ok(addr) = entry.parse::<IpAddr>() {
        return Some(addr);
    }
    if let Ok(sock) = entry.parse::<SocketAddr>() {
        return Some(sock.ip());
    }
    None
}

fn is_trusted(addr: IpAddr, trusted: &[IpNet]) -> bool {
    let canonical = addr.to_canonical();
    trusted.iter().any(|net| net.contains(&canonical))
}

/// Resolves the throttling identity of a request. See the spec's §3 for the
/// rightmost-untrusted walk and why each fallback is what it is.
pub fn client_id(peer: Option<IpAddr>, headers: &HeaderMap, trusted: &[IpNet]) -> ClientId {
    let Some(peer) = peer else {
        return ClientId::Unknown;
    };
    if !is_trusted(peer, trusted) {
        return ClientId::Addr(bucket_addr(peer));
    }

    // ALL header lines, in order: proxies append a new line rather than merging,
    // so `get` alone would read only the attacker's own line.
    let chain: Vec<&str> = headers
        .get_all(XFF)
        .iter()
        .filter_map(|v: &HeaderValue| v.to_str().ok())
        .flat_map(|line| line.split(','))
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();

    for entry in chain.iter().rev() {
        match parse_entry(entry) {
            None => return ClientId::MalformedChain,
            Some(addr) if is_trusted(addr, trusted) => continue,
            Some(addr) => return ClientId::Addr(bucket_addr(addr)),
        }
    }
    ClientId::Addr(bucket_addr(peer))
}

#[cfg(test)]
mod client_id_tests {
    use super::*;
    use axum::http::HeaderMap;

    fn nets(entries: &[&str]) -> Vec<IpNet> {
        entries
            .iter()
            .map(|e| {
                e.parse::<IpNet>()
                    .or_else(|_| e.parse::<IpAddr>().map(IpNet::from))
                    .unwrap()
            })
            .collect()
    }

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    /// Builds a HeaderMap with one `X-Forwarded-For` line per element — NOT one
    /// comma-joined line. Real proxies (HAProxy, Traefik, Caddy, Apache) append a
    /// new line rather than merging, and `HeaderMap::get` only sees the first.
    fn xff(lines: &[&str]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for line in lines {
            h.append("x-forwarded-for", line.parse().unwrap());
        }
        h
    }

    #[test]
    fn absent_peer_is_unknown() {
        assert_eq!(client_id(None, &HeaderMap::new(), &[]), ClientId::Unknown);
    }

    #[test]
    fn untrusted_peer_ignores_a_forged_forwarded_header() {
        // The header comes from an untrusted hop, so it is attacker-controlled.
        assert_eq!(
            client_id(
                Some(ip("203.0.113.9")),
                &xff(&["1.2.3.4"]),
                &nets(&["172.18.0.5"])
            ),
            ClientId::Addr(ip("203.0.113.9"))
        );
    }

    #[test]
    fn trusted_peer_takes_the_single_forwarded_entry() {
        assert_eq!(
            client_id(
                Some(ip("172.18.0.5")),
                &xff(&["203.0.113.9"]),
                &nets(&["172.18.0.5"])
            ),
            ClientId::Addr(ip("203.0.113.9"))
        );
    }

    #[test]
    fn client_injected_prefix_is_skipped_by_the_rightmost_walk() {
        // Client sent "evil"; the proxy appended the real address to its right.
        // The rightmost untrusted entry is the real client.
        assert_eq!(
            client_id(
                Some(ip("172.18.0.5")),
                &xff(&["9.9.9.9, 203.0.113.9"]),
                &nets(&["172.18.0.5"])
            ),
            ClientId::Addr(ip("203.0.113.9"))
        );
    }

    #[test]
    fn multiple_forwarded_header_lines_are_all_considered() {
        // THE bypass this guards: an attacker sends their own X-Forwarded-For line,
        // the proxy APPENDS a second line with the real address. Reading only the
        // first line would return the attacker-chosen value and hand them a fresh
        // bucket per request, disabling the throttle completely.
        assert_eq!(
            client_id(
                Some(ip("172.18.0.5")),
                &xff(&["9.9.9.9", "203.0.113.9"]),
                &nets(&["172.18.0.5"])
            ),
            ClientId::Addr(ip("203.0.113.9"))
        );
    }

    #[test]
    fn chain_of_trusted_hops_walks_through_to_the_client() {
        assert_eq!(
            client_id(
                Some(ip("172.18.0.5")),
                &xff(&["203.0.113.9, 172.18.0.7, 172.18.0.6"]),
                &nets(&["172.18.0.0/24"])
            ),
            ClientId::Addr(ip("203.0.113.9"))
        );
    }

    #[test]
    fn all_entries_trusted_falls_back_to_the_peer() {
        assert_eq!(
            client_id(
                Some(ip("172.18.0.5")),
                &xff(&["172.18.0.7"]),
                &nets(&["172.18.0.0/24"])
            ),
            ClientId::Addr(ip("172.18.0.5"))
        );
    }

    #[test]
    fn malformed_entry_gets_its_own_bucket_not_the_proxys() {
        // Any client can send garbage. If garbage collapsed onto the proxy's
        // address, a client could CHOOSE to join the bucket shared by everyone
        // behind that proxy and inflate the delay for all of them.
        assert_eq!(
            client_id(
                Some(ip("172.18.0.5")),
                &xff(&["!!!, 172.18.0.7"]),
                &nets(&["172.18.0.0/24"])
            ),
            ClientId::MalformedChain
        );
    }

    #[test]
    fn missing_or_empty_header_falls_back_to_the_peer() {
        assert_eq!(
            client_id(
                Some(ip("172.18.0.5")),
                &HeaderMap::new(),
                &nets(&["172.18.0.5"])
            ),
            ClientId::Addr(ip("172.18.0.5"))
        );
        assert_eq!(
            client_id(
                Some(ip("172.18.0.5")),
                &xff(&["   "]),
                &nets(&["172.18.0.5"])
            ),
            ClientId::Addr(ip("172.18.0.5"))
        );
    }

    #[test]
    fn ipv4_mapped_ipv6_peer_matches_an_ipv4_trusted_entry() {
        // A CONVERTBAR_BIND=:: dual-stack listener delivers IPv4 clients as
        // ::ffff:a.b.c.d. Without canonicalization the trusted entry silently
        // stops matching and the proxy is no longer recognised.
        assert_eq!(
            client_id(
                Some(ip("::ffff:172.18.0.5")),
                &xff(&["203.0.113.9"]),
                &nets(&["172.18.0.5"])
            ),
            ClientId::Addr(ip("203.0.113.9"))
        );
    }

    #[test]
    fn ipv4_mapped_and_plain_ipv4_share_one_bucket() {
        assert_eq!(
            client_id(Some(ip("::ffff:203.0.113.9")), &HeaderMap::new(), &[]),
            client_id(Some(ip("203.0.113.9")), &HeaderMap::new(), &[])
        );
    }

    #[test]
    fn ipv6_addresses_in_one_slash64_share_a_bucket() {
        // Every SLAAC/privacy-extensions host owns 2^64 addresses. Per-address
        // bucketing would hand an IPv6 attacker a virgin bucket per guess.
        let a = client_id(Some(ip("2001:db8:1:2::1")), &HeaderMap::new(), &[]);
        let b = client_id(
            Some(ip("2001:db8:1:2:ffff:ffff:ffff:ffff")),
            &HeaderMap::new(),
            &[],
        );
        assert_eq!(a, b);
        // A different /64 must NOT collide.
        let c = client_id(Some(ip("2001:db8:1:3::1")), &HeaderMap::new(), &[]);
        assert_ne!(a, c);
    }

    #[test]
    fn forwarded_entries_with_ports_and_brackets_parse() {
        assert_eq!(
            client_id(
                Some(ip("172.18.0.5")),
                &xff(&["203.0.113.9:41234"]),
                &nets(&["172.18.0.5"])
            ),
            ClientId::Addr(ip("203.0.113.9"))
        );
        // Bare IPv6 must survive — auth.rs's strip_port would turn this into
        // "2001:db8:" by splitting on the last colon.
        assert_eq!(
            client_id(
                Some(ip("172.18.0.5")),
                &xff(&["2001:db8:1:2::1"]),
                &nets(&["172.18.0.5"])
            ),
            client_id(Some(ip("2001:db8:1:2::9")), &HeaderMap::new(), &[])
        );
        assert_eq!(
            client_id(
                Some(ip("172.18.0.5")),
                &xff(&["[2001:db8:1:2::1]:443"]),
                &nets(&["172.18.0.5"])
            ),
            client_id(Some(ip("2001:db8:1:2::9")), &HeaderMap::new(), &[])
        );
    }
}
