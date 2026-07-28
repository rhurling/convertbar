//! Per-source login throttling: who may spend an evaluation right now.
//!
//! There is deliberately no lockout. A lockout that refuses a correct token is a
//! cheap permanent denial of service against the owner (sustaining it costs
//! ~0.07 req/s), so the gate below is temporary: after `free` failures a source
//! may be evaluated only once per spacing interval, doubling on every further
//! failure up to a cap, and a successful evaluation clears it immediately. The
//! gate closes the COMPARISON, not the response — `check` reserves the slot
//! BEFORE `token_matches` runs, so a denied request never computes a verdict at
//! all. Answering after the fact (a delay) bounds nothing: the verdict already
//! exists by then, and an attacker who abandons the connection before it
//! arrives pays nothing for the guess. See the spec's "Why no lockout" for the
//! full analysis.

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

/// Whether a source may spend an evaluation right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gate {
    /// Compare the credential. This call consumed the bucket's slot.
    Allow,
    /// Do NOT compare the credential — answer 401 without evaluating.
    Deny,
}

#[derive(Debug, Clone, Copy)]
pub struct ThrottlePolicy {
    /// Failures a source may spend before any gating begins.
    pub free: u32,
    pub base: Duration,
    pub cap: Duration,
    pub window: Duration,
}

impl Default for ThrottlePolicy {
    fn default() -> Self {
        Self {
            free: 3,
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
    /// Earliest instant at which this source may next be evaluated.
    next_at: Instant,
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

    /// Reserves this source's evaluation slot.
    ///
    /// MUST be called BEFORE comparing the credential. Comparing first would
    /// compute the verdict regardless, and the attacker only ever needed the
    /// verdict — withholding the response afterwards limits nothing.
    ///
    /// The slot is reserved inside the same critical section that reads it, so
    /// racing requests cannot all observe one open window.
    pub fn check(&self, id: ClientId, now: Instant) -> Gate {
        let mut map = self.lock();
        let Some(entry) = map.get_mut(&id) else {
            return Gate::Allow;
        };
        if now.duration_since(entry.first_at) > self.policy.window {
            map.remove(&id);
            return Gate::Allow;
        }
        if now < entry.next_at {
            return Gate::Deny;
        }
        entry.next_at = now + self.spacing_for(entry.count);
        Gate::Allow
    }

    /// Records that an allowed evaluation rejected the credential.
    pub fn record_failure(&self, id: ClientId, now: Instant) {
        let mut map = self.lock();
        if map.len() > PRUNE_THRESHOLD {
            let window = self.policy.window;
            map.retain(|_, f| now.duration_since(f.first_at) <= window);
        }
        let entry = map.entry(id).or_insert(Failures {
            count: 0,
            first_at: now,
            next_at: now,
        });
        if now.duration_since(entry.first_at) > self.policy.window {
            entry.count = 0;
            entry.first_at = now;
        }
        entry.count += 1;
        let count = entry.count;
        entry.next_at = now + self.spacing_for(count);
        let reached_cap = self.spacing_for(count) == self.policy.cap
            && self.spacing_for(count.saturating_sub(1)) != self.policy.cap;

        // Expiry alone does not bound the map under a live flood of distinct
        // sources — nothing in it is old enough to expire yet.
        if map.len() > PRUNE_THRESHOLD {
            let mut by_count: Vec<(ClientId, u32)> =
                map.iter().map(|(k, f)| (*k, f.count)).collect();
            // Lowest-count first: hot attackers are the entries worth keeping,
            // and evicting oldest-first would be gameable by flooding fresh
            // sources to shed your own bucket.
            by_count.sort_unstable_by_key(|(_, count)| *count);
            for (victim, _) in by_count
                .into_iter()
                .take(map.len().saturating_sub(PRUNE_THRESHOLD))
            {
                map.remove(&victim);
            }
        }
        drop(map);

        if reached_cap {
            tracing::warn!(
                ?id,
                "login throttle: source reached the {:?} evaluation spacing after {count} failures",
                self.policy.cap
            );
        }
    }

    /// Clears the bucket. Called when an allowed evaluation accepted the credential.
    pub fn record_success(&self, id: ClientId) {
        self.lock().remove(&id);
    }

    /// `0` while the source is inside its free allowance, then
    /// `base << (n - free)` capped. The shift is CLAMPED rather than
    /// special-cased: `1u32 << shift` panics at 32, and returning the cap there
    /// would be wrong for a zero `base` — doubling zero is still zero, and a
    /// zero-base policy means "no gating" (the integration tests rely on it).
    fn spacing_for(&self, count: u32) -> Duration {
        let Some(steps) = count.checked_sub(self.policy.free) else {
            return Duration::ZERO;
        };
        if steps == 0 {
            return Duration::ZERO;
        }
        let shift = (steps - 1).min(31);
        match self.policy.base.checked_mul(1u32 << shift) {
            Some(d) => d.min(self.policy.cap),
            None => self.policy.cap,
        }
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
            free: 3,
            base: Duration::from_millis(500),
            cap: Duration::from_secs(30),
            window: Duration::from_secs(900),
        }
    }

    fn id(s: &str) -> ClientId {
        ClientId::Addr(s.parse::<IpAddr>().unwrap())
    }

    /// Drives `n` allowed failures onto a bucket, asserting each was allowed.
    fn fail_n(t: &LoginThrottle, id: ClientId, now: Instant, n: u32) {
        for i in 0..n {
            assert_eq!(t.check(id, now), Gate::Allow, "failure {i} was denied");
            t.record_failure(id, now);
        }
    }

    #[test]
    fn the_first_free_failures_are_not_gated() {
        // Typos, a stale cookie, and the web UI's concurrent page-load fan-out
        // must never hit a denial.
        let t = LoginThrottle::new(policy());
        let now = Instant::now();
        let a = id("10.0.0.1");
        fail_n(&t, a, now, 3);
        // The 4th is the first gated one, and it is still allowed because the
        // ramp only starts spacing AFTER it.
        assert_eq!(t.check(a, now), Gate::Allow);
        t.record_failure(a, now);
        // Now the bucket is spaced: an immediate retry is refused.
        assert_eq!(t.check(a, now), Gate::Deny);
    }

    #[test]
    fn a_denied_check_does_not_advance_the_ramp() {
        // Otherwise an attacker could inflate a shared bucket's spacing for
        // free, without ever spending an evaluation.
        let t = LoginThrottle::new(policy());
        let now = Instant::now();
        let a = id("10.0.0.1");
        fail_n(&t, a, now, 4);
        for _ in 0..50 {
            assert_eq!(t.check(a, now), Gate::Deny);
        }
        // Spacing is still the first step (500ms), not 50 steps further on.
        assert_eq!(t.check(a, now + Duration::from_millis(500)), Gate::Allow);
    }

    #[test]
    fn the_slot_reopens_exactly_when_the_spacing_elapses() {
        let t = LoginThrottle::new(policy());
        let now = Instant::now();
        let a = id("10.0.0.1");
        fail_n(&t, a, now, 4);
        assert_eq!(t.check(a, now + Duration::from_millis(499)), Gate::Deny);
        assert_eq!(t.check(a, now + Duration::from_millis(500)), Gate::Allow);
    }

    #[test]
    fn spacing_doubles_per_failure_and_pins_at_the_cap() {
        let t = LoginThrottle::new(policy());
        let start = Instant::now();
        let a = id("10.0.0.1");
        fail_n(&t, a, start, 3); // free failures, no spacing yet

        // Each subsequent failure doubles the wait before the next evaluation.
        let expected = [
            Duration::from_millis(500),
            Duration::from_secs(1),
            Duration::from_secs(2),
            Duration::from_secs(4),
            Duration::from_secs(8),
            Duration::from_secs(16),
            Duration::from_secs(30), // 32s would exceed the cap
            Duration::from_secs(30),
        ];
        let mut at = start;
        for (i, want) in expected.iter().enumerate() {
            assert_eq!(t.check(a, at), Gate::Allow, "step {i} denied at its slot");
            t.record_failure(a, at);
            let just_before = at + *want - Duration::from_millis(1);
            assert_eq!(t.check(a, just_before), Gate::Deny, "step {i} opened early");
            at += *want;
        }
    }

    #[test]
    fn a_zero_base_policy_never_gates() {
        // The integration tests use this policy so they can drive many attempts
        // without modelling time.
        let t = LoginThrottle::new(ThrottlePolicy {
            base: Duration::ZERO,
            ..policy()
        });
        let now = Instant::now();
        let a = id("10.0.0.1");
        for _ in 0..100 {
            assert_eq!(t.check(a, now), Gate::Allow);
            t.record_failure(a, now);
        }
    }

    #[test]
    fn a_successful_evaluation_clears_the_gate_immediately() {
        let t = LoginThrottle::new(policy());
        let now = Instant::now();
        let a = id("10.0.0.1");
        fail_n(&t, a, now, 4);
        assert_eq!(t.check(a, now), Gate::Deny);
        t.record_success(a);
        assert_eq!(t.check(a, now), Gate::Allow);
    }

    #[test]
    fn the_window_forgets_failures_and_reopens_the_gate() {
        let t = LoginThrottle::new(policy());
        let start = Instant::now();
        let a = id("10.0.0.1");
        fail_n(&t, a, start, 4);
        assert_eq!(t.check(a, start), Gate::Deny);
        // Past the window the bucket is forgotten entirely, so the source is
        // back to its free allowance.
        let later = start + Duration::from_secs(901);
        assert_eq!(t.check(a, later), Gate::Allow);
        t.record_failure(a, later);
        assert_eq!(
            t.check(a, later),
            Gate::Allow,
            "free allowance did not reset"
        );
    }

    #[test]
    fn sources_are_gated_independently() {
        let t = LoginThrottle::new(policy());
        let now = Instant::now();
        fail_n(&t, id("10.0.0.1"), now, 4);
        assert_eq!(t.check(id("10.0.0.1"), now), Gate::Deny);
        assert_eq!(t.check(id("10.0.0.2"), now), Gate::Allow);
        assert_eq!(t.check(ClientId::Unknown, now), Gate::Allow);
        assert_eq!(t.check(ClientId::MalformedChain, now), Gate::Allow);
    }

    #[test]
    fn concurrent_checks_cannot_share_one_slot() {
        // THE property that makes this a rate limiter rather than a delay: the
        // slot is reserved inside the same lock that reads it, so N racing
        // requests cannot all observe the same open window.
        use std::sync::Arc;
        let t = Arc::new(LoginThrottle::new(policy()));
        let now = Instant::now();
        let a = id("10.0.0.1");
        fail_n(&t, a, now, 4);
        let later = now + Duration::from_secs(60); // slot is open again

        let allowed = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut handles = Vec::new();
        for _ in 0..32 {
            let t = Arc::clone(&t);
            let allowed = Arc::clone(&allowed);
            handles.push(std::thread::spawn(move || {
                if t.check(a, later) == Gate::Allow {
                    allowed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(
            allowed.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "more than one racing request claimed the same evaluation slot"
        );
    }

    #[test]
    fn map_stays_bounded_under_a_live_flood_and_keeps_the_hottest_bucket() {
        let t = LoginThrottle::new(policy());
        let now = Instant::now();
        let hot = id("192.168.1.1");
        // 4, not more: with `free: 3` only four evaluations fit at a single
        // instant before the gate shuts (the fifth is Deny). Count 4 is still
        // far hotter than the flood's count-of-1 entries, which is all this
        // test needs to prove eviction keeps the right bucket.
        fail_n(&t, hot, now, 4);

        for i in 0..(PRUNE_THRESHOLD as u32 * 2) {
            let o = i.to_be_bytes();
            let addr = IpAddr::from([10, o[1], o[2], o[3]]);
            let cold = ClientId::Addr(addr);
            t.check(cold, now);
            t.record_failure(cold, now);
        }

        assert!(
            t.len() <= PRUNE_THRESHOLD,
            "map exceeded its ceiling: {}",
            t.len()
        );
        // Lowest-count-first eviction must keep the attacker, not shed it —
        // evicting the hot bucket would silently reset the ramp that matters.
        assert_eq!(
            t.check(hot, now),
            Gate::Deny,
            "the hottest bucket was evicted and its ramp reset"
        );
    }
}
