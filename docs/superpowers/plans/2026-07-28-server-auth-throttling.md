# Server-Head Login Throttling and Token-Entropy Floor — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make a guessable `CONVERTBAR_AUTH_TOKEN` impossible to configure, and make online token guessing against the server head expensive — without ever letting an unauthenticated party deny the owner access.

**Architecture:** A startup strength check in `config.rs` rejects weak tokens. A new `throttle.rs` module resolves a request to a `ClientId` (peer address, or a forwarded address when the peer is a configured trusted proxy) and holds an escalating per-source failure delay. Both credential-checking sites — `auth_guard` and the `login` route — record failures into that shared throttle and sleep before returning the existing 401. There is no lockout: a correct token always succeeds.

**Tech Stack:** Rust, axum 0.8, `ipnet` 2.12 (already in `Cargo.lock` via `hyper-util`), tokio, `constant_time_eq`.

**Spec:** `docs/superpowers/specs/2026-07-28-server-auth-throttling-design.md`

## Global Constraints

- Token floor: **at least 16 characters, using at least 8 distinct characters**, counted over `char`s not bytes.
- Delay curve: `delay(n) = min(base << (n-1), cap)` with `base = 500ms`, `cap = 30s`, `window = 15min`. Guard the shift against overflow.
- **The failure counter increments under the lock BEFORE the sleep, and the delay is computed from the post-increment count.** Without this, opening more connections bypasses the escalation entirely.
- **No lockout.** A correct token succeeds regardless of preceding failures.
- **No `Set-Cookie` on any rejection path.** The 401 body stays exactly `{"error":"unauthorized"}`.
- A request presenting **no credential at all** is not a guess: plain 401, no delay, no counter.
- All throttle mutex acquisitions use `.lock().unwrap_or_else(|e| e.into_inner())` — this lock sits on a global request path.
- Never use an `Option<ConnectInfo<SocketAddr>>` extractor; it does not satisfy axum 0.8's middleware trait bounds. Read the peer from `req.extensions()`.
- Never reuse `auth.rs`'s private `strip_port` for forwarded addresses; it turns bare `2001:db8::1` into `2001:db8:`.
- Commits are signed automatically. If one fails with a 1Password agent error, ask the user to unlock and retry **once**; never commit unsigned.
- Run the full suite with `cargo test --workspace`. Baseline before this plan: **389 passing, 0 failing.**

---

## File Structure

| File | Responsibility |
|---|---|
| `crates/convertbar-server/src/throttle.rs` | **New.** `ClientId`, `client_id()`, `ThrottlePolicy`, `LoginThrottle`. All throttle state and all forwarded-address parsing. |
| `crates/convertbar-server/src/config.rs` | Token strength check, `CONVERTBAR_TRUSTED_PROXIES` parsing, two new `ConfigError` variants. |
| `crates/convertbar-server/src/auth.rs` | `auth_guard` enforcement; a `peer_ip(&Request)` helper. |
| `crates/convertbar-server/src/routes/login.rs` | `login` enforcement. |
| `crates/convertbar-server/src/routes/mod.rs` | `ServerState.login_throttle`; test-state wiring. |
| `crates/convertbar-server/src/main.rs` | `ConfigError` match arms; `into_make_service_with_connect_info`. |
| `crates/convertbar-server/Cargo.toml` | `ipnet` direct dependency. |
| `README.md`, `docker-compose.example.yml`, `unraid-template.xml`, `docs/RECOMMENDATIONS.md` | Documentation. |

Task order is dependency order: 1 (config) and 2–4 (throttle) are independent of each other; 5 wires state; 6–7 enforce; 8 covers the serve wiring; 9 documents.

---

### Task 1: Token strength floor and trusted-proxy config

**Files:**
- Modify: `crates/convertbar-server/Cargo.toml` (add `ipnet`)
- Modify: `crates/convertbar-server/src/config.rs`
- Modify: `crates/convertbar-server/src/main.rs:22-32` (exhaustive `ConfigError` match)

**Interfaces:**
- Consumes: nothing.
- Produces: `ServerConfig.trusted_proxies: Vec<IpNet>`; `ConfigError::WeakToken`; `ConfigError::BadTrustedProxy(String)`; `pub fn token_is_strong(token: &str) -> bool`.

- [ ] **Step 1: Add the dependency**

In `crates/convertbar-server/Cargo.toml`, under `[dependencies]`, after the `constant_time_eq = "0.3"` line:

```toml
ipnet = "2"
```

- [ ] **Step 2: Write the failing tests**

Add to the `mod tests` block in `crates/convertbar-server/src/config.rs`. Note `vars()` already exists in that module.

```rust
#[test]
fn token_at_the_floor_is_accepted() {
    // Exactly 16 chars, 16 distinct — the boundary must pass, not just clear it.
    let cfg = ServerConfig::from_vars(&vars(&[("CONVERTBAR_AUTH_TOKEN", "abcdefghijklmnop")]))
        .unwrap();
    assert_eq!(cfg.auth, AuthMode::Token("abcdefghijklmnop".to_string()));
}

#[test]
fn token_one_char_below_the_floor_is_rejected() {
    let err = ServerConfig::from_vars(&vars(&[("CONVERTBAR_AUTH_TOKEN", "abcdefghijklmno")]))
        .unwrap_err();
    assert_eq!(err, ConfigError::WeakToken);
}

#[test]
fn long_token_with_too_few_distinct_chars_is_rejected() {
    // 32 chars but only 2 distinct: length alone must not be sufficient.
    let err = ServerConfig::from_vars(&vars(&[(
        "CONVERTBAR_AUTH_TOKEN",
        "abababababababababababababababab",
    )]))
    .unwrap_err();
    assert_eq!(err, ConfigError::WeakToken);
}

#[test]
fn exactly_eight_distinct_chars_is_accepted_and_seven_is_not() {
    // 16 chars, 8 distinct — the distinct boundary, from the other side.
    assert!(ServerConfig::from_vars(&vars(&[("CONVERTBAR_AUTH_TOKEN", "abcdefghabcdefgh")])).is_ok());
    // 16 chars, 7 distinct.
    assert_eq!(
        ServerConfig::from_vars(&vars(&[("CONVERTBAR_AUTH_TOKEN", "abcdefgabcdefgaa")])).unwrap_err(),
        ConfigError::WeakToken
    );
}

#[test]
fn token_length_counts_characters_not_bytes() {
    // 16 distinct multi-byte chars: 48 bytes but 16 chars. A bytes-based
    // implementation would wrongly accept a 6-char version of this too, so the
    // rejection case below is the one that pins it.
    assert!(ServerConfig::from_vars(&vars(&[("CONVERTBAR_AUTH_TOKEN", "日本語表示試験用文字列拡張確認済")])).is_ok());
    // 6 chars / 18 bytes — over the byte floor, under the char floor.
    assert_eq!(
        ServerConfig::from_vars(&vars(&[("CONVERTBAR_AUTH_TOKEN", "日本語表示試")])).unwrap_err(),
        ConfigError::WeakToken
    );
}

#[test]
fn weak_token_is_rejected_even_when_no_auth_is_also_set() {
    // from_vars checks the token first, so a weak token does NOT silently fall
    // through to open mode. Contradictory auth config must be surfaced.
    let err = ServerConfig::from_vars(&vars(&[
        ("CONVERTBAR_AUTH_TOKEN", "weak"),
        ("CONVERTBAR_NO_AUTH", "1"),
    ]))
    .unwrap_err();
    assert_eq!(err, ConfigError::WeakToken);
}

#[test]
fn trusted_proxies_parse_cidr_and_bare_addresses() {
    let cfg = ServerConfig::from_vars(&vars(&[
        ("CONVERTBAR_AUTH_TOKEN", "abcdefghijklmnop"),
        ("CONVERTBAR_TRUSTED_PROXIES", "172.18.0.5,10.0.0.0/8,2001:db8::/32"),
    ]))
    .unwrap();
    assert_eq!(
        cfg.trusted_proxies,
        vec![
            "172.18.0.5/32".parse::<ipnet::IpNet>().unwrap(),
            "10.0.0.0/8".parse::<ipnet::IpNet>().unwrap(),
            "2001:db8::/32".parse::<ipnet::IpNet>().unwrap(),
        ]
    );
}

#[test]
fn unparsable_trusted_proxy_is_a_hard_error_not_a_skipped_entry() {
    // Silently dropping an entry would collapse every client into one bucket —
    // exactly the failure this variable exists to prevent, and invisibly.
    let err = ServerConfig::from_vars(&vars(&[
        ("CONVERTBAR_AUTH_TOKEN", "abcdefghijklmnop"),
        ("CONVERTBAR_TRUSTED_PROXIES", "172.18.0.5,not-an-ip"),
    ]))
    .unwrap_err();
    assert_eq!(err, ConfigError::BadTrustedProxy("not-an-ip".to_string()));
}

#[test]
fn trusted_proxies_defaults_to_empty() {
    let cfg = ServerConfig::from_vars(&vars(&[("CONVERTBAR_AUTH_TOKEN", "abcdefghijklmnop")]))
        .unwrap();
    assert!(cfg.trusted_proxies.is_empty());
}
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test -p convertbar-server config::tests 2>&1 | tail -20`
Expected: compile errors — no `trusted_proxies` field, no `WeakToken`/`BadTrustedProxy` variants.

- [ ] **Step 4: Implement**

In `crates/convertbar-server/src/config.rs`, add the import at the top:

```rust
use ipnet::IpNet;
```

Extend the error enum:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigError {
    MissingAuth,
    WeakToken,
    BadTrustedProxy(String),
    BadBind(String),
}
```

Add the field to `ServerConfig`:

```rust
pub trusted_proxies: Vec<IpNet>,
```

Add the strength check above `impl ServerConfig`:

```rust
/// Minimum viable token: long enough that guessing is hopeless, and not a single
/// character repeated. This is a floor against pathological input, not an entropy
/// estimator — `1234567890123456` passes, deliberately.
pub fn token_is_strong(token: &str) -> bool {
    token.chars().count() >= 16
        && token.chars().collect::<std::collections::HashSet<_>>().len() >= 8
}
```

Replace the `auth` match in `from_vars` (currently `config.rs:33-37`):

```rust
let auth = match vars.get("CONVERTBAR_AUTH_TOKEN") {
    Some(token) if !token.is_empty() => {
        if !token_is_strong(token) {
            return Err(ConfigError::WeakToken);
        }
        AuthMode::Token(token.clone())
    }
    _ if vars.get("CONVERTBAR_NO_AUTH").map(String::as_str) == Some("1") => AuthMode::Open,
    _ => return Err(ConfigError::MissingAuth),
};
```

Add the trusted-proxy parsing next to the existing `allowed_hosts` block, and include `trusted_proxies` in the returned `Self { .. }`:

```rust
// A bare address means "exactly this host", i.e. a full-length prefix.
let mut trusted_proxies = Vec::new();
if let Some(raw) = vars.get("CONVERTBAR_TRUSTED_PROXIES").filter(|s| !s.is_empty()) {
    for entry in raw.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        let net = entry
            .parse::<IpNet>()
            .or_else(|_| entry.parse::<std::net::IpAddr>().map(IpNet::from))
            .map_err(|_| ConfigError::BadTrustedProxy(entry.to_string()))?;
        trusted_proxies.push(net);
    }
}
```

- [ ] **Step 5: Fix the eight existing tests the floor breaks**

These pass tokens below the floor and would now fail before reaching the behaviour they test. In `crates/convertbar-server/src/config.rs`, replace the token value in each:

| Line | Test | Change |
|---|---|---|
| 97 | `defaults_when_only_token_set` | `"secret"` → `"abcdefghijklmnop"` (also update its `assert_eq!` on `AuthMode::Token`) |
| 118 | `custom_bind_and_port_are_parsed` | `"t"` → `"abcdefghijklmnop"` |
| 129 | `bad_bind_host_is_rejected` | `"t"` → `"abcdefghijklmnop"` |
| 139 | `bad_port_is_rejected` | `"t"` → `"abcdefghijklmnop"` |
| 149 | `browse_roots_split_on_colon` | `"t"` → `"abcdefghijklmnop"` |
| 162 | `empty_browse_roots_falls_back_to_default` | `"t"` → `"abcdefghijklmnop"` |
| 172 | `empty_allowed_hosts_falls_back_to_default` | `"t"` → `"abcdefghijklmnop"` |
| 182 | `allowed_hosts_split_on_comma_only` | `"t"` → `"abcdefghijklmnop"` |

Also add `trusted_proxies: Vec::new()` expectations only where a test asserts the whole struct — none currently do, so no further change.

- [ ] **Step 6: Add the new `main.rs` match arms**

`crates/convertbar-server/src/main.rs:22-32` matches `ConfigError` exhaustively and will not compile until both new variants are handled. Add:

```rust
Err(ConfigError::WeakToken) => {
    eprintln!(
        "convertbar-server: CONVERTBAR_AUTH_TOKEN is too weak — it must be at least 16 \
         characters long and use at least 8 distinct characters.\n\
         Generate one with:  openssl rand -base64 24"
    );
    std::process::exit(1);
}
Err(ConfigError::BadTrustedProxy(entry)) => {
    eprintln!(
        "convertbar-server: invalid CONVERTBAR_TRUSTED_PROXIES entry: {entry} \
         (expected an IP address or CIDR range, e.g. 172.18.0.5 or 10.0.0.0/8)"
    );
    std::process::exit(1);
}
```

- [ ] **Step 7: Run the tests to verify they pass**

Run: `cargo test -p convertbar-server 2>&1 | grep -E "^test result|^error"`
Expected: all pass, no compile errors.

- [ ] **Step 8: Commit**

```bash
git add crates/convertbar-server/Cargo.toml crates/convertbar-server/src/config.rs crates/convertbar-server/src/main.rs Cargo.lock
git commit -m "feat(server): reject weak auth tokens and parse trusted proxies"
```

---

### Task 2: `ClientId` resolution from peer + `X-Forwarded-For`

**Files:**
- Create: `crates/convertbar-server/src/throttle.rs`
- Modify: `crates/convertbar-server/src/main.rs` (add `mod throttle;`)

**Interfaces:**
- Consumes: `ServerConfig.trusted_proxies` (Task 1).
- Produces: `pub enum ClientId { Addr(IpAddr), MalformedChain, Unknown }` (derives `Debug, Clone, Copy, PartialEq, Eq, Hash`); `pub fn client_id(peer: Option<IpAddr>, headers: &HeaderMap, trusted: &[IpNet]) -> ClientId`.

- [ ] **Step 1: Register the module**

Add `mod throttle;` to the module list at the top of `crates/convertbar-server/src/main.rs`, keeping alphabetical order — that is **after `mod startup;`** (line 6), which is the last entry.

- [ ] **Step 2: Write the failing tests**

Create `crates/convertbar-server/src/throttle.rs` containing only this test module for now:

```rust
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
            client_id(Some(ip("203.0.113.9")), &xff(&["1.2.3.4"]), &nets(&["172.18.0.5"])),
            ClientId::Addr(ip("203.0.113.9"))
        );
    }

    #[test]
    fn trusted_peer_takes_the_single_forwarded_entry() {
        assert_eq!(
            client_id(Some(ip("172.18.0.5")), &xff(&["203.0.113.9"]), &nets(&["172.18.0.5"])),
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
            client_id(Some(ip("172.18.0.5")), &HeaderMap::new(), &nets(&["172.18.0.5"])),
            ClientId::Addr(ip("172.18.0.5"))
        );
        assert_eq!(
            client_id(Some(ip("172.18.0.5")), &xff(&["   "]), &nets(&["172.18.0.5"])),
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
        let b = client_id(Some(ip("2001:db8:1:2:ffff:ffff:ffff:ffff")), &HeaderMap::new(), &[]);
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
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test -p convertbar-server client_id_tests 2>&1 | tail -20`
Expected: compile errors — `client_id` and `ClientId` are not defined.

- [ ] **Step 4: Implement**

Prepend to `crates/convertbar-server/src/throttle.rs` (above the test module):

```rust
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
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p convertbar-server client_id_tests 2>&1 | grep -E "^test result|^error"`
Expected: `test result: ok. 13 passed`.

- [ ] **Step 6: Mutation-check the header bypass**

These tests are the only defense against the `X-Forwarded-For` bypass, so prove they can catch it. Change the chain collection from `headers.get_all(XFF)` to `headers.get(XFF).into_iter()`. Run `cargo test -p convertbar-server client_id_tests`.

`multiple_forwarded_header_lines_are_all_considered` MUST fail. Revert.

Also change `.rev()` to a forward walk: `client_injected_prefix_is_skipped_by_the_rightmost_walk` MUST fail. Revert.

- [ ] **Step 7: Commit**

```bash
git add crates/convertbar-server/src/throttle.rs crates/convertbar-server/src/main.rs
git commit -m "feat(server): resolve client identity from peer and trusted forwarded headers"
```

---

### Task 3: The escalating-delay `LoginThrottle`

**Files:**
- Modify: `crates/convertbar-server/src/throttle.rs`

**Interfaces:**
- Consumes: `ClientId` (Task 2).
- Produces: `pub struct ThrottlePolicy { pub base: Duration, pub cap: Duration, pub window: Duration }` (with `Default`); `pub struct LoginThrottle` with `pub fn new(policy: ThrottlePolicy) -> Self`, `pub fn record_failure(&self, id: ClientId, now: Instant) -> Duration`, `pub fn record_success(&self, id: ClientId)`.

- [ ] **Step 1: Write the failing tests**

Append to `crates/convertbar-server/src/throttle.rs`:

```rust
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
        assert_eq!(t.record_failure(id("10.0.0.2"), now), Duration::from_millis(500));
        assert_eq!(t.record_failure(ClientId::Unknown, now), Duration::from_millis(500));
        assert_eq!(t.record_failure(ClientId::MalformedChain, now), Duration::from_millis(500));
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
        assert!(t.len() < 5000, "expired entries were not pruned: {}", t.len());
        // The live entry expired too (same window), so it also resets.
        assert_eq!(t.record_failure(live, later), Duration::from_millis(500));
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p convertbar-server throttle_tests 2>&1 | tail -20`
Expected: compile errors — `LoginThrottle`, `ThrottlePolicy` undefined.

- [ ] **Step 3: Implement**

Add to `crates/convertbar-server/src/throttle.rs` (after `client_id`, before the test modules). Extend the top-of-file imports with `use std::collections::HashMap; use std::sync::Mutex; use std::time::{Duration, Instant};`.

```rust
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
        (1..=32).find(|n| self.delay_for(*n) == self.policy.cap).unwrap_or(u32::MAX)
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
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p convertbar-server throttle_tests 2>&1 | grep -E "^test result|^error"`
Expected: `test result: ok. 6 passed`.

- [ ] **Step 5: Mutation-check the two load-bearing behaviours**

This project has been bitten by tests that could not fail (see the
`mutation-check-load-bearing-tests` precedent). Verify these two catch their bug:

1. In `record_failure`, move `let count = entry.count;` to **before** `entry.count += 1`. Run `cargo test -p convertbar-server throttle_tests`. `delay_doubles_with_each_failure_then_pins_at_the_cap` MUST fail. Revert.
2. In `delay_for`, delete the `if shift >= 32` guard. Run the tests. `a_very_long_run_of_failures_does_not_overflow` MUST fail (panic or wrong value). Revert.

If either mutation passes, the test is not doing its job — fix the test before continuing.

- [ ] **Step 6: Commit**

```bash
git add crates/convertbar-server/src/throttle.rs
git commit -m "feat(server): add escalating per-source login failure delay"
```

---

### Task 4: Thread the throttle through `ServerState`

**Files:**
- Modify: `crates/convertbar-server/src/routes/mod.rs:39-44` (struct), `:194-204` (test state)
- Modify: `crates/convertbar-server/src/main.rs:65-70`

**Interfaces:**
- Consumes: `LoginThrottle`, `ThrottlePolicy` (Task 3).
- Produces: `ServerState.login_throttle: Arc<LoginThrottle>`; `routes::tests::test_state()` unchanged in signature but now carrying a zero-delay throttle.

- [ ] **Step 1: Add the field**

In `crates/convertbar-server/src/routes/mod.rs`, add to `ServerState`:

```rust
    /// Per-source failed-credential ramp, shared by `auth_guard` and the login route
    /// so failures at either accumulate together.
    pub login_throttle: Arc<crate::throttle::LoginThrottle>,
```

- [ ] **Step 2: Construct it in production**

In `crates/convertbar-server/src/main.rs`, add this field to the `ServerState { .. }` literal at line 65:

```rust
        login_throttle: Arc::new(throttle::LoginThrottle::new(
            throttle::ThrottlePolicy::default(),
        )),
```

- [ ] **Step 3: Construct it in tests with a zero delay**

In `crates/convertbar-server/src/routes/mod.rs`, in `test_state_with_shutdown`'s `ServerState { .. }` literal:

```rust
            // Zero base delay so the existing suite does not sleep. Tests that
            // exercise the ramp construct their own policy.
            login_throttle: Arc::new(crate::throttle::LoginThrottle::new(
                crate::throttle::ThrottlePolicy {
                    base: std::time::Duration::ZERO,
                    ..Default::default()
                },
            )),
```

- [ ] **Step 4: Verify the whole suite still passes**

Run: `cargo test --workspace 2>&1 | grep -E "^test result|^error"`
Expected: all green, same count as before plus the tests added in Tasks 1–3.

- [ ] **Step 5: Commit**

```bash
git add crates/convertbar-server/src/routes/mod.rs crates/convertbar-server/src/main.rs
git commit -m "feat(server): thread the login throttle through ServerState"
```

---

### Task 5: Enforce in `auth_guard`

**Files:**
- Modify: `crates/convertbar-server/src/auth.rs:120-139`

**Interfaces:**
- Consumes: `client_id`, `ClientId`, `LoginThrottle` (Tasks 2–4).
- Produces: `pub fn peer_ip(req: &Request) -> Option<IpAddr>` (used by Task 6).

- [ ] **Step 1: Write the failing tests**

Add to `guard_integration_tests` in `crates/convertbar-server/src/auth.rs`. Add these imports to that module: `use std::net::SocketAddr; use std::time::Duration; use axum::extract::ConnectInfo; use axum::Extension;`.

```rust
/// Wraps `app` so requests carry a peer address, the way a real listener does.
/// `oneshot` supplies no connect info, so without this every test would land in
/// the single `Unknown` bucket and prove nothing about per-source behaviour.
///
/// NOT `MockConnectInfo` — that inserts an extension of type `MockConnectInfo<T>`,
/// and only the `ConnectInfo` *extractor* falls back to it. Code reading
/// `extensions().get::<ConnectInfo<_>>()` sees nothing, so every request would
/// silently resolve to `Unknown` and these tests would pass for the wrong reason.
/// `Extension(ConnectInfo(..))` inserts exactly what
/// `into_make_service_with_connect_info` inserts in production. (Verified
/// empirically; see the spec's §6.)
fn app_from(state: ServerState, peer: &str) -> axum::Router {
    app(state).layer(Extension(ConnectInfo(peer.parse::<SocketAddr>().unwrap())))
}

fn throttled_state(token: &str, base: Duration) -> ServerState {
    let mut state = token_state(token);
    state.login_throttle = std::sync::Arc::new(crate::throttle::LoginThrottle::new(
        crate::throttle::ThrottlePolicy {
            base,
            ..Default::default()
        },
    ));
    state
}

#[tokio::test]
async fn uncredentialed_requests_are_never_throttled() {
    // The web UI fires several uncredentialed /api/* requests on page load
    // specifically to trigger the login screen. If those counted, a user would
    // lock themselves into a slow login before typing a character.
    let state = throttled_state("abcdefghijklmnop", Duration::from_millis(80));
    let app = app_from(state, "10.0.0.1:5555");
    for _ in 0..12 {
        // Assert the elapsed time of EACH uncredentialed request. Asserting only
        // the status would let a "count them as failures" regression pass: the
        // loop would just get slow, and a later authenticated request still
        // returns fast because the success path never sleeps.
        let start = std::time::Instant::now();
        let response = send(app.clone(), "GET", "/api/queue", &[("Host", "localhost")], None).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert!(
            start.elapsed() < Duration::from_millis(80),
            "uncredentialed request was delayed: {:?}",
            start.elapsed()
        );
    }
    // And the ramp must still be at zero: a wrong credential now should cost the
    // FIRST step (80ms), not the thirteenth.
    let start = std::time::Instant::now();
    send(
        app.clone(),
        "GET",
        "/api/queue",
        &[("Host", "localhost"), ("Authorization", "Bearer wrong")],
        None,
    )
    .await;
    let first_real_failure = start.elapsed();
    assert!(
        first_real_failure < Duration::from_millis(300),
        "counter advanced on uncredentialed requests: first real failure cost {first_real_failure:?}"
    );

    // A correct credential must still be served immediately.
    let start = std::time::Instant::now();
    let response = send(
        app,
        "GET",
        "/api/queue",
        &[("Host", "localhost"), ("Authorization", "Bearer abcdefghijklmnop")],
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        start.elapsed() < Duration::from_millis(80),
        "authenticated request was delayed: {:?}",
        start.elapsed()
    );
}

#[tokio::test]
async fn a_correct_token_always_succeeds_no_matter_how_many_failures_precede_it() {
    // The goal-3 regression test. There is no lockout by design: no sequence of
    // unauthenticated requests may deny a client holding the right token.
    let state = throttled_state("abcdefghijklmnop", Duration::ZERO);
    let app = app_from(state, "10.0.0.1:5555");
    for _ in 0..40 {
        let response = send(
            app.clone(),
            "GET",
            "/api/queue",
            &[("Host", "localhost"), ("Authorization", "Bearer wrong")],
            None,
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }
    let response = send(
        app,
        "GET",
        "/api/queue",
        &[("Host", "localhost"), ("Authorization", "Bearer abcdefghijklmnop")],
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn wrong_credentials_are_delayed_and_the_delay_escalates() {
    // Pins BOTH that the sleep happens at all and that it derives from the
    // post-increment count — a delay computed before incrementing would make
    // every attempt cost the same, which is bypassable by opening connections.
    let state = throttled_state("abcdefghijklmnop", Duration::from_millis(60));
    let app = app_from(state, "10.0.0.1:5555");

    let first = std::time::Instant::now();
    send(
        app.clone(),
        "GET",
        "/api/queue",
        &[("Host", "localhost"), ("Authorization", "Bearer wrong")],
        None,
    )
    .await;
    let first = first.elapsed();

    let second = std::time::Instant::now();
    send(
        app,
        "GET",
        "/api/queue",
        &[("Host", "localhost"), ("Authorization", "Bearer wrong")],
        None,
    )
    .await;
    let second = second.elapsed();

    // Absolute bounds, not `second >= first * 2`: that form doubles the first
    // request's scheduling overhead into the bound and is flaky on a loaded
    // machine. These are still discriminating — a delay computed BEFORE the
    // increment gives second ~= 60ms and fails, and a dropped sleep fails the
    // first assertion.
    assert!(first >= Duration::from_millis(60), "first not delayed: {first:?}");
    assert!(
        second >= Duration::from_millis(120),
        "delay did not escalate: {first:?} then {second:?}"
    );
}

#[tokio::test]
async fn one_sources_failures_do_not_slow_another_source() {
    let state = throttled_state("abcdefghijklmnop", Duration::from_millis(60));
    let noisy = app_from(state.clone(), "10.0.0.1:5555");
    for _ in 0..6 {
        send(
            noisy.clone(),
            "GET",
            "/api/queue",
            &[("Host", "localhost"), ("Authorization", "Bearer wrong")],
            None,
        )
        .await;
    }
    let quiet = app_from(state, "10.0.0.2:5555");
    let start = std::time::Instant::now();
    send(
        quiet,
        "GET",
        "/api/queue",
        &[("Host", "localhost"), ("Authorization", "Bearer wrong")],
        None,
    )
    .await;
    // Second source is at ramp step 1, not step 7.
    assert!(
        start.elapsed() < Duration::from_millis(200),
        "unrelated source inherited the ramp: {:?}",
        start.elapsed()
    );
}

#[tokio::test]
async fn rejection_body_and_headers_are_unchanged_and_carry_no_cookie() {
    let state = throttled_state("abcdefghijklmnop", Duration::ZERO);
    let response = send(
        app_from(state, "10.0.0.1:5555"),
        "GET",
        "/api/queue",
        &[("Host", "localhost"), ("Authorization", "Bearer wrong")],
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert!(response.headers().get(SET_COOKIE).is_none());
    assert_eq!(json_body(response).await, json!({"error": "unauthorized"}));
}
```

- [ ] **Step 2: Run the tests and record which are red**

Run: `cargo test -p convertbar-server guard_integration 2>&1 | tail -30`

**Do not expect everything to be red — that would be a lie, and noticing it is the point.** Tasks 3 and 4 already added the types and the field, so this compiles. Only two of these are true red→green drivers; the rest are regression pins that are green from the start and are validated by the mutation checks in Step 5 instead.

| Test | Pre-implementation | Why |
|---|---|---|
| `wrong_credentials_are_delayed_and_the_delay_escalates` | **RED** | No delay exists yet — the only true driver here |
| `uncredentialed_requests_are_never_throttled` | green (pin) | Nothing throttles yet, so nothing is delayed |
| `a_correct_token_always_succeeds...` | green (pin) | No lockout exists yet to break it |
| `one_sources_failures_do_not_slow_another_source` | green (pin) | No ramp exists yet to leak |
| `rejection_body_and_headers_are_unchanged_and_carry_no_cookie` | green (pin) | Asserts today's behaviour survives |

Confirm `wrong_credentials_are_delayed_and_the_delay_escalates` is failing before continuing. If any *other* test is failing, stop — something from Tasks 1–4 is wrong.

- [ ] **Step 3: Implement**

In `crates/convertbar-server/src/auth.rs`, add imports: `use std::net::{IpAddr, SocketAddr}; use axum::extract::ConnectInfo; use crate::throttle::client_id;`.

Add the peer helper next to `bearer_token`:

```rust
/// The connecting peer's address, from request extensions. NOT an
/// `Option<ConnectInfo<..>>` extractor — that does not satisfy axum 0.8's
/// middleware trait bounds and will not compile.
pub fn peer_ip(req: &Request) -> Option<IpAddr> {
    req.extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|c| c.0.ip())
}
```

Replace the body of `auth_guard` after the exempt-path check (currently `auth.rs:131-138`):

```rust
    // Order matters. A request with NO credential is not a guess: it is how the
    // web UI discovers it needs to show the login screen. Charging it a delay
    // would make the login screen slow exactly when it is needed.
    let Some(provided) = bearer_token(&req).or_else(|| cookie_token(&req)) else {
        return json_err(StatusCode::UNAUTHORIZED, "unauthorized");
    };

    // Checked before any throttle work, so the authenticated hot path (SSE,
    // polling) takes no lock at all.
    if token_matches(&provided, expected) {
        return next.run(req).await;
    }

    let id = client_id(peer_ip(&req), req.headers(), &s.config.trusted_proxies);
    let delay = s.login_throttle.record_failure(id, std::time::Instant::now());
    tokio::time::sleep(delay).await;
    json_err(StatusCode::UNAUTHORIZED, "unauthorized")
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p convertbar-server 2>&1 | grep -E "^test result|^error"`
Expected: all pass.

- [ ] **Step 5: Mutation-check the guards that are pins, not drivers**

Four of the five tests above were green before the implementation, so only a mutation proves they can fail at all. Run each, confirm the named test goes red, then revert.

1. Delete the `let Some(provided) = ... else { return ... };` early return, treating a missing credential as a mismatch that records a failure. → `uncredentialed_requests_are_never_throttled` MUST fail (its per-request elapsed assertions, and its "first real failure" assertion).
2. Change the bucket key in `auth_guard` to a constant (e.g. always `ClientId::Unknown`). → `one_sources_failures_do_not_slow_another_source` MUST fail. This is the test most at risk of passing vacuously, because a broken connect-info path collapses every source into one bucket and the test would never notice.
3. Attach a `Set-Cookie` clearing header to the 401. → `rejection_body_and_headers_are_unchanged_and_carry_no_cookie` MUST fail.
4. Add a lockout: after 5 failures return 401 without checking the token. → `a_correct_token_always_succeeds...` MUST fail. This is the goal-3 pin and the single most important test in the plan.

If any mutation passes, the test is not doing its job — fix the test before continuing.

- [ ] **Step 6: Commit**

```bash
git add crates/convertbar-server/src/auth.rs
git commit -m "feat(server): throttle failed credentials in auth_guard"
```

---

### Task 6: Enforce in the login route

**Files:**
- Modify: `crates/convertbar-server/src/routes/login.rs`

**Interfaces:**
- Consumes: `client_id`, `LoginThrottle` (Tasks 2–4). Note it does **not** use `auth.rs`'s `peer_ip` — that helper takes a `&Request`, and this is a handler with an extractor signature.
- Produces: nothing new.

- [ ] **Step 1: Write the failing tests**

Add to `mod tests` in `crates/convertbar-server/src/routes/login.rs`. The existing `post_login` helper uses `api_router`, which has no guards; these new tests need the full `app()` plus connect info, so add a second helper.

```rust
use axum::extract::ConnectInfo;
use axum::Extension;
use std::net::SocketAddr;
use std::time::Duration;

fn login_app(token: &str, base: Duration) -> axum::Router {
    let mut state = state_with_auth(AuthMode::Token(token.to_string()));
    state.login_throttle = std::sync::Arc::new(crate::throttle::LoginThrottle::new(
        crate::throttle::ThrottlePolicy {
            base,
            ..Default::default()
        },
    ));
    // Extension(ConnectInfo(..)), NOT MockConnectInfo — see the note in Task 5.
    crate::routes::app(state).layer(Extension(ConnectInfo(
        "10.0.0.1:5555".parse::<SocketAddr>().unwrap(),
    )))
}

async fn try_login(app: axum::Router, token: &str) -> axum::http::Response<Body> {
    use tower::ServiceExt;
    app.oneshot(
        Request::builder()
            .method("POST")
            .uri("/api/login")
            .header("Host", "localhost")
            .header("Content-Type", "application/json")
            .body(Body::from(json!({ "token": token }).to_string()))
            .unwrap(),
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn the_correct_token_is_accepted_after_many_failed_logins() {
    // No lockout, by design: repeated failures must never deny the owner.
    let app = login_app("abcdefghijklmnop", Duration::ZERO);
    for _ in 0..40 {
        let response = try_login(app.clone(), "wrong").await;
        assert_eq!(response.status(), axum::http::StatusCode::UNAUTHORIZED);
    }
    let response = try_login(app, "abcdefghijklmnop").await;
    assert_eq!(response.status(), axum::http::StatusCode::NO_CONTENT);
    assert!(response.headers().get(SET_COOKIE).is_some());
}

#[tokio::test]
async fn a_successful_login_resets_the_ramp() {
    let app = login_app("abcdefghijklmnop", Duration::from_millis(60));
    for _ in 0..4 {
        try_login(app.clone(), "wrong").await;
    }
    try_login(app.clone(), "abcdefghijklmnop").await;
    // Back to ramp step 1, so well under the step-5 delay of ~960ms.
    let start = std::time::Instant::now();
    try_login(app, "wrong").await;
    assert!(
        start.elapsed() < Duration::from_millis(300),
        "ramp was not reset by the successful login: {:?}",
        start.elapsed()
    );
}

#[tokio::test]
async fn login_and_api_failures_share_one_bucket() {
    // Otherwise an attacker guesses via `Authorization: Bearer` to keep the
    // login route's ramp at zero, or vice versa.
    let app = login_app("abcdefghijklmnop", Duration::from_millis(60));
    for _ in 0..5 {
        try_login(app.clone(), "wrong").await;
    }
    use tower::ServiceExt;
    let start = std::time::Instant::now();
    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/queue")
                .header("Host", "localhost")
                .header("Authorization", "Bearer wrong")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), axum::http::StatusCode::UNAUTHORIZED);
    assert!(
        start.elapsed() >= Duration::from_millis(500),
        "auth_guard did not inherit the login route's ramp: {:?}",
        start.elapsed()
    );
}

#[tokio::test]
async fn a_failed_login_still_sets_no_cookie() {
    let app = login_app("abcdefghijklmnop", Duration::ZERO);
    let response = try_login(app, "wrong").await;
    assert_eq!(response.status(), axum::http::StatusCode::UNAUTHORIZED);
    assert!(response.headers().get(SET_COOKIE).is_none());
}
```

- [ ] **Step 2: Run the tests and record which are red**

Run: `cargo test -p convertbar-server routes::login 2>&1 | tail -30`

As in Task 5, most of these are pins rather than drivers. Only `a_successful_login_resets_the_ramp` is a true driver here (nothing resets anything yet — though note it is also trivially green if nothing ramps either, so its real proof is the mutation in Step 5). `login_and_api_failures_share_one_bucket` is **RED** because Task 5's `auth_guard` now ramps but the login route does not yet feed the same bucket. The other two are green pins.

Confirm `login_and_api_failures_share_one_bucket` is failing before continuing.

- [ ] **Step 3: Implement**

In `crates/convertbar-server/src/routes/login.rs`, add `headers` and an optional connect-info parameter to the handler.

`Option<Extension<ConnectInfo<SocketAddr>>>` is the correct form here and **has been verified to compile and to work at runtime against this repository's axum version**. It compiles because `Extension` implements `OptionalFromRequestParts` (`ConnectInfo` does not, which is why the bare `Option<ConnectInfo<..>>` fails to satisfy the trait bound). It resolves at runtime because `into_make_service_with_connect_info` — and the `Extension(ConnectInfo(..))` test layer — store a `ConnectInfo<SocketAddr>` extension, which is exactly what `Extension<ConnectInfo<SocketAddr>>` looks up.

**`MockConnectInfo` does NOT work here** and must not be substituted: it stores a `MockConnectInfo<T>` extension, which only the `ConnectInfo` *extractor* knows to fall back to. Verified empirically — under `MockConnectInfo` this handler receives `None`.

**Import note:** `login.rs:7` already has `use axum::extract::State;`. **Replace that line** rather than adding a second import — a duplicate `State` import is E0252, a hard error, not a warning.

```rust
// REPLACES the existing `use axum::extract::State;` at line 7:
use axum::extract::{ConnectInfo, State};
use axum::http::HeaderMap;
use std::net::SocketAddr;

use crate::throttle::client_id;

pub async fn login(
    State(s): State<ServerState>,
    connect: Option<axum::Extension<ConnectInfo<SocketAddr>>>,
    headers: HeaderMap,
    jar: CookieJar,
    Json(body): Json<LoginBody>,
) -> Response {
    let expected = match &s.config.auth {
        AuthMode::Open => return StatusCode::NO_CONTENT.into_response(),
        AuthMode::Token(t) => t,
    };

    let peer = connect.map(|axum::Extension(ConnectInfo(addr))| addr.ip());
    let id = client_id(peer, &headers, &s.config.trusted_proxies);

    if !token_matches(&body.token, expected) {
        let delay = s.login_throttle.record_failure(id, std::time::Instant::now());
        tokio::time::sleep(delay).await;
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "unauthorized"})),
        )
            .into_response();
    }

    s.login_throttle.record_success(id);

    // No `Secure` flag: this is a plain-HTTP LAN server by design (see CLAUDE.md's
    // threat model), so requiring HTTPS here would just break the cookie entirely.
    let cookie = Cookie::build((TOKEN_COOKIE, body.token))
        .http_only(true)
        .same_site(SameSite::Strict)
        .path("/")
        .build();

    (jar.add(cookie), StatusCode::NO_CONTENT).into_response()
}
```

**Note for the implementer:** `Json<LoginBody>` must stay the **last** parameter — it consumes the request body, so any extractor after it will not compile.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p convertbar-server 2>&1 | grep -E "^test result|^error"`
Expected: all pass.

- [ ] **Step 5: Mutation-check the reset**

Delete the `s.login_throttle.record_success(id);` line. `a_successful_login_resets_the_ramp` MUST fail. Revert. Without this check that test is green in every world where the ramp is small, and proves nothing.

- [ ] **Step 6: Add the open-mode test**

The spec requires proving open mode never engages the throttle. Add:

```rust
#[tokio::test]
async fn open_mode_never_engages_the_throttle() {
    let mut state = state_with_auth(AuthMode::Open);
    state.login_throttle = std::sync::Arc::new(crate::throttle::LoginThrottle::new(
        crate::throttle::ThrottlePolicy {
            base: Duration::from_millis(200),
            ..Default::default()
        },
    ));
    let app = crate::routes::app(state).layer(Extension(ConnectInfo(
        "10.0.0.1:5555".parse::<SocketAddr>().unwrap(),
    )));
    for _ in 0..5 {
        let start = std::time::Instant::now();
        let response = try_login(app.clone(), "any-token-at-all").await;
        assert_eq!(response.status(), axum::http::StatusCode::NO_CONTENT);
        assert!(
            start.elapsed() < Duration::from_millis(200),
            "open mode was throttled: {:?}",
            start.elapsed()
        );
    }
}
```

- [ ] **Step 7: Commit**

```bash
git add crates/convertbar-server/src/routes/login.rs
git commit -m "feat(server): throttle failed logins and reset the ramp on success"
```

---

### Task 7: Real-listener connect-info wiring

**Files:**
- Modify: `crates/convertbar-server/src/main.rs:75`
- Modify: `crates/convertbar-server/src/routes/mod.rs` (new test)

**Interfaces:**
- Consumes: everything above.
- Produces: nothing.

- [ ] **Step 1: Write the failing test**

`oneshot` cannot exercise the serve wiring, and its silent regression would put every client in the single `Unknown` bucket — a global throttle instead of a per-source one.

**The test must discriminate, and asserting a 401 does not.** A 401 comes back whether the request is bucketed by address or lands in `Unknown`; at a zero delay the two are indistinguishable. So: trust `127.0.0.1` as a proxy, use a non-zero delay, and send two requests bearing *different* forwarded clients. With the wiring intact each gets its own bucket and the second is fast. Without it, the peer is `None`, the forwarded header is never consulted (an untrusted peer's header is ignored), both collapse into `Unknown`, and the second inherits the first's ramp.

Add to `mod tests` in `crates/convertbar-server/src/routes/mod.rs`:

```rust
/// The one line `oneshot` cannot cover. Without
/// `into_make_service_with_connect_info` there is no `ConnectInfo`, so
/// `client_id` cannot recognise 127.0.0.1 as a trusted proxy, never reads
/// `X-Forwarded-For`, and every client collapses into one shared bucket.
#[tokio::test]
async fn served_requests_are_bucketed_per_forwarded_client() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let (state, _shutdown_tx) = test_state_with_shutdown();
    let mut config = (*state.config).clone();
    config.auth = crate::config::AuthMode::Token("abcdefghijklmnop".to_string());
    config.trusted_proxies = vec!["127.0.0.1".parse::<ipnet::IpNet>().unwrap_or_else(|_| {
        ipnet::IpNet::from("127.0.0.1".parse::<std::net::IpAddr>().unwrap())
    })];
    let mut state = state;
    state.config = Arc::new(config);
    state.login_throttle = Arc::new(crate::throttle::LoginThrottle::new(
        crate::throttle::ThrottlePolicy {
            base: std::time::Duration::from_millis(150),
            ..Default::default()
        },
    ));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(
            listener,
            app(state).into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .unwrap();
    });

    async fn wrong_credential_from(addr: std::net::SocketAddr, forwarded: &str) -> String {
        let request = format!(
            "GET /api/queue HTTP/1.1\r\nHost: localhost\r\n\
             Authorization: Bearer wrong\r\nX-Forwarded-For: {forwarded}\r\n\
             Connection: close\r\n\r\n"
        );
        let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).await.unwrap();
        response
    }

    // Ramp up one forwarded client.
    for _ in 0..4 {
        let response = wrong_credential_from(addr, "203.0.113.1").await;
        assert!(response.starts_with("HTTP/1.1 401"), "unexpected: {response}");
    }

    // A DIFFERENT forwarded client must start at step 1 (~150ms), not inherit
    // the first client's ramp (~1.2s at step 4).
    let start = std::time::Instant::now();
    let response = wrong_credential_from(addr, "203.0.113.2").await;
    assert!(response.starts_with("HTTP/1.1 401"), "unexpected: {response}");
    assert!(
        start.elapsed() < std::time::Duration::from_millis(600),
        "second forwarded client inherited the first's ramp ({:?}) — connect info \
         is missing, so every client shares the Unknown bucket",
        start.elapsed()
    );
}
```

**Note:** `"127.0.0.1".parse::<IpNet>()` fails (no prefix), so the `unwrap_or_else` above converts the bare address. If Task 1's config parsing is reachable from this test, prefer building the config through `ServerConfig::from_vars` with `CONVERTBAR_TRUSTED_PROXIES=127.0.0.1` instead — it is the same code path production uses and avoids duplicating the parse logic.

- [ ] **Step 2: Verify the test can fail**

This test builds its own server, so it passes as soon as it is written — `main.rs` is not what it exercises. Its value is entirely in the mutation, so run that now:

Change the test's own `axum::serve(listener, app(state).into_make_service_with_connect_info::<SocketAddr>())` to plain `axum::serve(listener, app(state))`. Run:

`cargo test -p convertbar-server served_requests_are_bucketed_per_forwarded_client 2>&1 | tail -20`

Expected: **FAIL** on the elapsed-time assertion — with no connect info the peer is `None`, so both forwarded clients land in `Unknown` and the second inherits the ramp. Revert the mutation and confirm it passes.

If it passes under the mutation, the test is not discriminating and must be fixed before continuing.

- [ ] **Step 3: Update `main.rs`**

Change `crates/convertbar-server/src/main.rs:75`:

```rust
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
```

`app` is currently bound at line 72 (`let app = routes::app(state);`) — keep that binding and apply the conversion at the `serve` call.

- [ ] **Step 4: Verify**

Run: `cargo test --workspace 2>&1 | grep -E "^test result|^error"`
Expected: all pass.

- [ ] **Step 5: Commit**

```bash
git add crates/convertbar-server/src/main.rs crates/convertbar-server/src/routes/mod.rs
git commit -m "feat(server): serve with connect info so throttling is per-source"
```

---

### Task 8: Documentation

**Files:**
- Modify: `README.md` (env-var table ~line 135, Auth section ~line 137)
- Modify: `docker-compose.example.yml:7`
- Modify: `unraid-template.xml` (the `CONVERTBAR_AUTH_TOKEN` config block, ~line 82)
- Modify: `docs/RECOMMENDATIONS.md` (item 15)

- [ ] **Step 1: Add the env-var row**

In `README.md`, after the `CONVERTBAR_ALLOWED_HOSTS` row:

```markdown
| `CONVERTBAR_TRUSTED_PROXIES` | *(none)* | Comma-separated IPs or CIDR ranges whose `X-Forwarded-For` header is believed, so login throttling counts real client addresses instead of the proxy's. **Set this as narrowly as possible** — see [Auth](#auth). |
```

- [ ] **Step 2: Rewrite the Auth section**

Replace the Auth section body in `README.md`:

```markdown
### Auth

The server refuses to start unless `CONVERTBAR_AUTH_TOKEN` or
`CONVERTBAR_NO_AUTH=1` is set — there is no unauthenticated-by-default mode.
`CONVERTBAR_NO_AUTH=1` is only for a trusted LAN or a deployment where a reverse
proxy already gates access.

**Token requirements.** `CONVERTBAR_AUTH_TOKEN` must be at least 16 characters
long and use at least 8 distinct characters; anything weaker is refused at
startup rather than warned about. Generate one with:

```sh
openssl rand -base64 24
```

**Failed-attempt throttling.** Each failed credential — at `/api/login` or via
an `Authorization` header — makes the *next* failure from that source slower:
500 ms, then 1 s, 2 s, 4 s, and so on to a 30-second ceiling. A successful login
clears it. There is deliberately no lockout, so no amount of guessing by anyone
else can stop you getting in with the right token; the ramp only ever delays
attempts that are already wrong.

Rotating the token means changing the variable and restarting the container.
Open browser tabs will be signed out and can log in again immediately. A script
looping on an outdated token will ramp itself to the 30-second delay — that is
working as intended, and is indistinguishable from an attacker.

**Behind a reverse proxy**, every request appears to come from the proxy, so all
clients share one throttling ramp. Set `CONVERTBAR_TRUSTED_PROXIES` to the
proxy's address to have `X-Forwarded-For` believed instead:

```
CONVERTBAR_TRUSTED_PROXIES=172.18.0.5
```

> Set it as narrowly as possible. Every address listed is trusted to assert who
> it is, so a range that contains *clients* rather than only the proxy lets each
> of them forge a fresh identity per request and skip throttling entirely —
> worse than leaving it unset. Do not use a whole Docker bridge network
> (`172.18.0.0/16`) or a LAN range; pin the proxy to a static address and list
> that. This cannot help behind plain NAT, where there is no forwarded header.
```

- [ ] **Step 3: Fix the compose example**

`docker-compose.example.yml:7` suggests `"change-me"`, a 9-character token the
server now refuses at startup. **Keep it refusing.** Replace the line with a
placeholder that also fails the floor:

```yaml
      # Generate with: openssl rand -base64 24
      # This placeholder is deliberately too weak: >=16 chars using >=8 distinct
      # characters is required, so the server refuses to start until you replace it.
      CONVERTBAR_AUTH_TOKEN: "CHANGE_ME"
```

**Do not use a descriptive placeholder like `"REPLACE_ME_WITH_A_GENERATED_TOKEN"`** —
at 33 characters and 17 distinct it *passes* `token_is_strong`, so a user who
copies the file verbatim gets a booting server protected by a string published
in a public repository. The old `"change-me"` failed loudly at startup; the
replacement must preserve that fail-safe, not remove it. Verify whatever you
write against `token_is_strong` rather than eyeballing it, and check the other
template files (`unraid-template.xml`) for the same trap.

- [ ] **Step 4: Update the Unraid template**

In `unraid-template.xml`, extend the `CONVERTBAR_AUTH_TOKEN` field's description
to state: at least 16 characters with at least 8 distinct characters; generate
with `openssl rand -base64 24`.

- [ ] **Step 5: Move the recommendation to shipped**

In `docs/RECOMMENDATIONS.md`, move item 15 out of "Open — High Impact" into the
shipped section, following the format of the neighbouring shipped entries
(e.g. item 10/11), noting: token floor in `config.rs`, escalating per-source
delay in `throttle.rs`, enforcement in `auth.rs` and `routes/login.rs`, and that
the lockout in the original recommendation was deliberately not implemented —
with a one-line pointer to the spec's "Why no lockout".

- [ ] **Step 6: Verify docs match reality**

Re-read each claim in the new Auth section against the code as implemented.
Specifically confirm: the delay values match `ThrottlePolicy::default()`, the
floor numbers match `token_is_strong`, and the "no lockout" claim matches
`LoginThrottle`'s API (no `is_locked`, no denial path).

- [ ] **Step 7: Commit**

```bash
git add README.md docker-compose.example.yml unraid-template.xml docs/RECOMMENDATIONS.md
git commit -m "docs: document the token floor and login throttling"
```

---

### Task 9: Full verification

- [ ] **Step 1: Full workspace suite**

Run: `cargo test --workspace 2>&1 | grep -E "^test result|^error"`
Expected: 0 failures. Compare the total against the 389 baseline plus this plan's additions.

- [ ] **Step 2: Restricted-PATH run**

This project has repeatedly shipped tests that depend on HandBrakeCLI being
installed and then failed in CI. Run:

`env PATH=/usr/bin:/bin cargo test --workspace 2>&1 | grep -E "^test result|^error"`
Expected: identical results.

- [ ] **Step 3: Formatting and lints**

```bash
cargo fmt --all -- --check
cargo clippy -p convertbar-server --all-targets
```

**Do not use `-D warnings`.** `convertbar-core` carries 10 pre-existing clippy
warnings on `main`, so a workspace-wide `-D warnings` gate fails regardless of
this work and is not part of CI. The bar here is: **no new warnings in
`convertbar-server`**. Verify by reading the warning list and confirming every
`-->` path outside `convertbar-core` belongs to code this plan did not add.

Watch specifically for `clippy::useless_format` in the new tests — a `format!`
with no interpolated arguments. Use a plain `&str` literal where there is
nothing to interpolate.

Note: a `field is never read` warning for `trusted_proxies` is expected between
Tasks 1 and 2 and must be gone once `client_id` consumes it.

- [ ] **Step 4: Frontend suite (unchanged, but prove it)**

Run: `npm test`
Expected: passing; this plan makes no frontend changes.

- [ ] **Step 5: Manual smoke test**

Start the server with a conforming token, confirm: a wrong token at the login
screen is rejected and visibly slower on repeat; the correct token then logs in
immediately; a weak token makes the binary exit 1 with the generated-token hint.

- [ ] **Step 6: Report**

Summarize: tests added, tests changed, mutation checks performed and their
outcome, anything deferred.
