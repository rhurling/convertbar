//! Per-source login throttling: who is attempting, and how long they must wait.
//!
//! There is deliberately no lockout. A lockout that refuses a correct token is a
//! cheap permanent denial of service against the owner (sustaining it costs
//! ~0.07 req/s), and one that honours a correct token rate-limits nothing —
//! every guess is still evaluated. The escalating delay below makes guessing
//! expensive without ever creating denial state an attacker can trigger.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::Mutex;
use std::time::{Duration, Instant};

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

/// Prune when the map exceeds this many keys. The map is attacker-influenced
/// (one key per source), so it needs a ceiling; a few thousand entries is a few
/// hundred KB, which is the right order for a LAN appliance.
const PRUNE_THRESHOLD: usize = 4096;

#[derive(Debug, Clone, Copy)]
pub struct ThrottlePolicy {
    pub base: Duration,
    pub cap: Duration,
    pub window: Duration,
}

impl Default for ThrottlePolicy {
    fn default() -> Self {
        Self {
            base: Duration::from_millis(500),
            cap: Duration::from_secs(30),
            window: Duration::from_secs(900),
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Failures {
    count: u32,
    first_at: Instant,
}

pub struct LoginThrottle {
    failures: Mutex<HashMap<ClientId, Failures>>,
    policy: ThrottlePolicy,
}

impl LoginThrottle {
    pub fn new(policy: ThrottlePolicy) -> Self {
        Self {
            failures: Mutex::new(HashMap::new()),
            policy,
        }
    }

    /// Records a rejected attempt and returns how long the caller must wait
    /// before responding.
    ///
    /// The count is incremented HERE, under the lock, and the delay derives from
    /// the post-increment value — so N concurrent attempts get N escalating
    /// delays rather than N copies of the first one. Computing the delay before
    /// incrementing would let an attacker bypass the whole ramp by opening more
    /// connections.
    pub fn record_failure(&self, id: ClientId, now: Instant) -> Duration {
        let mut map = self.lock();
        if map.len() > PRUNE_THRESHOLD {
            let window = self.policy.window;
            map.retain(|_, f| now.duration_since(f.first_at) <= window);
        }
        let entry = map.entry(id).or_insert(Failures {
            count: 0,
            first_at: now,
        });
        if now.duration_since(entry.first_at) > self.policy.window {
            entry.count = 0;
            entry.first_at = now;
        }
        entry.count += 1;
        let count = entry.count;
        drop(map);

        let delay = self.delay_for(count);
        if delay == self.policy.cap && count == self.cap_reached_at() {
            tracing::warn!(
                ?id,
                "login throttle: source reached the {:?} delay cap after {count} failed attempts",
                self.policy.cap
            );
        }
        delay
    }

    /// Clears a source's ramp. Called only on a successful login.
    pub fn record_success(&self, id: ClientId) {
        self.lock().remove(&id);
    }

    /// `base << (n-1)`, capped. The shift is CLAMPED rather than special-cased:
    /// `1u32 << shift` panics once shift reaches 32, and returning the cap
    /// directly at that point would be wrong for a zero `base` — doubling zero
    /// is still zero, and a zero-base policy means "no throttling" (the tests
    /// rely on it). Clamping keeps one code path correct for every policy.
    fn delay_for(&self, count: u32) -> Duration {
        let shift = count.saturating_sub(1).min(31);
        match self.policy.base.checked_mul(1u32 << shift) {
            Some(d) => d.min(self.policy.cap),
            None => self.policy.cap,
        }
    }

    /// The lowest failure count whose delay is the cap — used so the cap warning
    /// logs once per ramp instead of on every subsequent attempt. Note "per ramp",
    /// not "per source": after a window rollover or a successful login the source
    /// starts over and will warn again if it climbs back to the cap. That is the
    /// intended reading — a fresh ramp reaching the cap is a fresh event.
    fn cap_reached_at(&self) -> u32 {
        (1..=32)
            .find(|n| self.delay_for(*n) == self.policy.cap)
            .unwrap_or(u32::MAX)
    }

    /// Poisoning must not take the server down: this lock sits on a global
    /// request path, so a single panic would otherwise 500 every subsequent
    /// request forever. A possibly-stale counter is the better failure mode.
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<ClientId, Failures>> {
        self.failures.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.lock().len()
    }
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

#[cfg(test)]
mod throttle_tests {
    use super::*;
    use std::net::IpAddr;
    use std::time::{Duration, Instant};

    fn policy() -> ThrottlePolicy {
        ThrottlePolicy {
            base: Duration::from_millis(500),
            cap: Duration::from_secs(30),
            window: Duration::from_secs(900),
        }
    }

    fn id(s: &str) -> ClientId {
        ClientId::Addr(s.parse::<IpAddr>().unwrap())
    }

    #[test]
    fn delay_doubles_with_each_failure_then_pins_at_the_cap() {
        let t = LoginThrottle::new(policy());
        let now = Instant::now();
        let a = id("10.0.0.1");
        assert_eq!(t.record_failure(a, now), Duration::from_millis(500));
        assert_eq!(t.record_failure(a, now), Duration::from_secs(1));
        assert_eq!(t.record_failure(a, now), Duration::from_secs(2));
        assert_eq!(t.record_failure(a, now), Duration::from_secs(4));
        assert_eq!(t.record_failure(a, now), Duration::from_secs(8));
        assert_eq!(t.record_failure(a, now), Duration::from_secs(16));
        // 32s would exceed the cap.
        assert_eq!(t.record_failure(a, now), Duration::from_secs(30));
        assert_eq!(t.record_failure(a, now), Duration::from_secs(30));
    }

    #[test]
    fn a_very_long_run_of_failures_does_not_overflow() {
        // `base << (n-1)` overflows once n-1 reaches 32, so the shift must be
        // guarded. Iterate well past that point — and only assert the cap from
        // failure 7 on, because failures 1-6 are still climbing the ramp
        // (500ms, 1s, 2s, 4s, 8s, 16s) and asserting the cap on those would make
        // the test die at iteration 1, never reaching the code it exists to pin.
        let t = LoginThrottle::new(policy());
        let now = Instant::now();
        let a = id("10.0.0.1");
        let mut last = Duration::ZERO;
        for i in 1..=200u32 {
            last = t.record_failure(a, now);
            if i >= 7 {
                assert_eq!(last, Duration::from_secs(30), "failure #{i} left the cap");
            }
        }
        assert_eq!(last, Duration::from_secs(30));
    }

    #[test]
    fn failures_older_than_the_window_start_a_fresh_count() {
        let t = LoginThrottle::new(policy());
        let start = Instant::now();
        let a = id("10.0.0.1");
        t.record_failure(a, start);
        t.record_failure(a, start);
        assert_eq!(t.record_failure(a, start), Duration::from_secs(2));
        // Past the window: the ramp resets to base.
        let later = start + Duration::from_secs(901);
        assert_eq!(t.record_failure(a, later), Duration::from_millis(500));
    }

    #[test]
    fn a_successful_login_clears_the_ramp() {
        let t = LoginThrottle::new(policy());
        let now = Instant::now();
        let a = id("10.0.0.1");
        t.record_failure(a, now);
        t.record_failure(a, now);
        t.record_success(a);
        assert_eq!(t.record_failure(a, now), Duration::from_millis(500));
    }

    #[test]
    fn distinct_sources_have_independent_ramps() {
        let t = LoginThrottle::new(policy());
        let now = Instant::now();
        for _ in 0..5 {
            t.record_failure(id("10.0.0.1"), now);
        }
        assert_eq!(
            t.record_failure(id("10.0.0.2"), now),
            Duration::from_millis(500)
        );
        assert_eq!(
            t.record_failure(ClientId::Unknown, now),
            Duration::from_millis(500)
        );
        assert_eq!(
            t.record_failure(ClientId::MalformedChain, now),
            Duration::from_millis(500)
        );
    }

    #[test]
    fn pruning_drops_expired_entries_and_keeps_live_ones() {
        let t = LoginThrottle::new(policy());
        let start = Instant::now();
        // Fill past the prune threshold with entries that will expire.
        for i in 0..5000u32 {
            let octets = i.to_be_bytes();
            let addr = IpAddr::from([10, octets[1], octets[2], octets[3]]);
            t.record_failure(ClientId::Addr(addr), start);
        }
        let live = id("192.168.1.1");
        t.record_failure(live, start);
        // Well past the window, so everything above is prunable. This failure
        // triggers a prune; the map must not retain the dead entries.
        let later = start + Duration::from_secs(901);
        t.record_failure(id("192.168.1.2"), later);
        assert!(
            t.len() < 5000,
            "expired entries were not pruned: {}",
            t.len()
        );
        // The live entry expired too (same window), so it also resets.
        assert_eq!(t.record_failure(live, later), Duration::from_millis(500));
    }

    #[test]
    fn a_zero_base_policy_never_delays_no_matter_how_high_the_count_climbs() {
        // Tasks 5 and 6 use a zero-base policy so the suite does not sleep. A
        // guard that returns the cap once the shift saturates would silently
        // reintroduce 30-second sleeps into exactly those tests.
        let t = LoginThrottle::new(ThrottlePolicy {
            base: Duration::ZERO,
            ..Default::default()
        });
        let now = Instant::now();
        let a = id("10.0.0.1");
        for _ in 0..100 {
            assert_eq!(t.record_failure(a, now), Duration::ZERO);
        }
    }
}
