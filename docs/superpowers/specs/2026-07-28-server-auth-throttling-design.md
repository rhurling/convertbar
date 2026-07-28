# Server Head — Login Throttling and a Token-Entropy Floor — Design

## Problem

The server head (`crates/convertbar-server`, shipped in PR #130) authenticates
with a single static token, and neither end of that credential is defended.

- **No floor on the token.** `ServerConfig::from_vars` (`config.rs:34`) accepts
  any non-empty `CONVERTBAR_AUTH_TOKEN`. `"1"` starts the server just as happily
  as a 32-byte random string, and the shipped `docker-compose.example.yml`
  literally suggests `"change-me"` (9 characters).
- **No cost to guessing.** `POST /api/login` (`routes/login.rs:35`) performs one
  `token_matches` and returns 401 — no delay, no counter, no state. `auth_guard`
  (`auth.rs:120`) is the same, and is arguably the softer target: an attacker
  can guess with `Authorization: Bearer <x>` against any `/api/*` route and never
  touch the login handler at all.

An authenticated session can browse the mounted filesystem, permanently delete
files (`purge_bad_sources` runs under the server's forced-`DeleteDisposer`), and
point `handbrake_path` at an arbitrary binary that the next encode executes.
The whole-branch review called this the weakest link in the auth posture. It is
acceptable for a trusted LAN and not acceptable for anything wider.

## Goals

1. Make a guessable token impossible to configure by accident.
2. Make online guessing cost enough that it is not a viable attack, without
   creating a denial-of-service against the deployment's owner.
3. Keep the failure response indistinguishable between "wrong token" and
   "currently throttled", so the throttle cannot be used as an oracle.

## Non-goals

- No frontend changes. The login screen showing a generic "unauthorized" for
  both causes **is** goal 3, not an oversight.
- No new routes. `crates/convertbar-server/routes.json` is unchanged.
- No multi-user accounts, sessions, or in-app credential management. Still one
  static token. ("Token rotation" below always means the operator changing
  `CONVERTBAR_AUTH_TOKEN` and restarting the container — the only mechanism
  there is, and one this design must not make painful.)
- `CONVERTBAR_NO_AUTH=1` keeps its current meaning: auth off entirely, no floor,
  no throttle. Opting out of authentication is a separate, deliberate choice
  and this design does not second-guess it.

## Design

### 1. Token strength floor — `config.rs`

`from_vars` gains a strength check on `CONVERTBAR_AUTH_TOKEN`:

> **at least 16 characters long, and using at least 8 distinct characters**

Length is the real defense. The distinct-character rule exists only to reject
pathological input like `aaaaaaaaaaaaaaaa` — it is not an entropy estimator, and
deliberately is not one: real entropy estimation is a rabbit hole with
false-positive risk against legitimately random tokens.

Counting is over `char`s, not bytes, so a multi-byte token is measured the way a
user would count it.

Failing the check returns a new `ConfigError::WeakToken`. `main.rs` prints the
rule and a way to satisfy it, then exits 1:

```
convertbar-server: CONVERTBAR_AUTH_TOKEN is too weak — it must be at least 16
characters long and use at least 8 distinct characters.
Generate one with:  openssl rand -base64 24
```

A hard reject rather than a warning: the server head is one day old, so there is
effectively no installed base to break, and "a warning nobody reads" is the exact
failure mode the recommendation calls out.

### 2. Trusted proxies — `config.rs`

New environment variable `CONVERTBAR_TRUSTED_PROXIES`: comma-separated CIDR
ranges or bare IP addresses, e.g. `172.18.0.0/16,10.0.0.5`. Parsed into
`Vec<IpNet>`; a bare address becomes a full-length prefix (`/32` or `/128`).

Unset (the default) means no proxy is trusted and `X-Forwarded-For` is never
consulted — correct for a direct LAN deployment.

An unparsable entry is a hard `ConfigError::BadTrustedProxy(entry)`, not a
skipped entry. Silently dropping one would collapse every client into the
proxy's single bucket, which is precisely the failure this feature exists to
prevent, and it would fail invisibly.

CIDR rather than exact IPs because Docker bridge addresses are dynamic: a proxy
container's address changes across restarts, so an exact-IP list degrades
silently over time. `ipnet` is already present in `Cargo.lock` (2.12.0) as a
transitive dependency, so promoting it to a direct dependency of
`convertbar-server` adds no new crates to the build.

### 3. Client identity — new module `throttle.rs`

```rust
pub fn client_ip(
    peer: Option<IpAddr>,
    forwarded_for: Option<&str>,
    trusted: &[IpNet],
) -> Option<IpAddr>
```

Rightmost-untrusted walk:

1. `peer` absent → `None`.
2. `peer` **not** in `trusted` → return `peer`, ignoring `X-Forwarded-For`
   entirely. A header from an untrusted hop is attacker-controlled.
3. `peer` in `trusted` → walk the `X-Forwarded-For` entries **right-to-left**,
   applying this rule to each entry in turn:
   - unparsable → stop the walk, fall back to `peer`
   - parses and is in `trusted` → continue leftward
   - parses and is not in `trusted` → return it
4. If the walk runs off the left end (every entry trusted), or the header is
   absent or empty → fall back to `peer`.

Step 3's malformed-entry rule stops the walk rather than skipping past the bad
entry, because skipping would let a client inject garbage to push the walk
further left into its own forged entries.

Entries are trimmed, and surrounding brackets plus a trailing `:port` are
stripped before parsing so both `203.0.113.7:41234` and `[2001:db8::1]:443`
resolve. Bare `2001:db8::1` also resolves.

`X-Forwarded-For` only. `X-Real-IP` is deliberately unsupported: it carries no
chain semantics, so honoring it would add a second, weaker spoofing path with no
capability the first does not already provide.

### 4. `LoginThrottle` — `throttle.rs`

```rust
pub struct ThrottlePolicy {
    pub delay: Duration,        // 500ms  — applied to every rejected attempt
    pub max_failures: u32,      // 20     — failures within `window` before locking
    pub window: Duration,       // 5 min  — failures older than this are forgotten
    pub lockout: Duration,      // 5 min  — how long a locked source stays locked
}

pub struct LoginThrottle {
    failures: Mutex<HashMap<Option<IpAddr>, Failures>>,
    policy: ThrottlePolicy,
}
```

The numbers are hardcoded constants (`ThrottlePolicy::default()`), exposed as
struct fields purely so tests can inject their own. No environment variables:
nobody tunes these, and every knob is another way to misconfigure the defense
to nothing.

`max_failures` is 20 rather than a tighter number because of the stale-credential
burst described in §5.1: a single page load fires 8–10 concurrent authenticated
requests, and all of them are already in flight before the first response can
clear a stale cookie. The threshold must sit above that burst or a token
rotation would lock the owner out on their next page load. The cost is
negligible — paired with the 500 ms delay and the 5-minute lockout, 20 failures
per 5 minutes is roughly 4 guesses a minute against a token with a 16-character
floor.

The key is `Option<IpAddr>`, so an unidentifiable source is its own bucket and
therefore still throttled. Failing *closed* matters here — in production
`ConnectInfo` is always present, so `None` only occurs if that wiring regresses,
and a regression must not silently disable the throttle.

**Every method that consults the clock takes `now: Instant` as an explicit
parameter** (`record_success` does not, since clearing a bucket is time-
independent) rather than calling `Instant::now()` internally. Callers pass `Instant::now()`; tests pass
`base + Duration::from_secs(n)`. This makes window rollover, lockout expiry, and
lockout release deterministically testable with no `sleep` anywhere in the suite.

```rust
impl LoginThrottle {
    /// True if this source is currently locked out.
    pub fn is_locked(&self, ip: Option<IpAddr>, now: Instant) -> bool;
    /// Records a rejected attempt; returns the delay the caller must apply.
    pub fn record_failure(&self, ip: Option<IpAddr>, now: Instant) -> Duration;
    /// Clears the source's failure record.
    pub fn record_success(&self, ip: Option<IpAddr>);
}
```

State machine per bucket:

- First failure records `count = 1` and `first_at = now`.
- A failure with `now - first_at > window` starts a fresh window (`count = 1`).
- Otherwise `count += 1`; on reaching `max_failures`, `locked_until = now + lockout`.
- `is_locked` is true while `now < locked_until`. Once it passes it returns
  false **and clears the bucket as a side effect** (it takes `&self` and mutates
  through the `Mutex`), so a lockout does not chain into an immediate second one.
- Attempts made *while locked* do not extend the lockout. Extending would make
  the shared-bucket case (see Risks) a permanent denial rather than a bounded one.

The map is pruned lazily: when it exceeds 1024 keys during `record_failure`,
entries that are neither locked nor inside their window are dropped. No
background task.

### 5. Enforcement — `routes/login.rs` and `auth.rs::auth_guard`

`auth_guard` already exempts `POST /api/login` (existing behaviour, `auth.rs:126`),
so a login attempt is counted at exactly one site, never both.

Both sites follow the same order and emit the identical **rejected-credential
response** — `401 {"error":"unauthorized"}` plus a cookie-clearing `Set-Cookie`
(§5.1) — after the identical fixed delay, whether the cause was a wrong token or
a lockout:

1. **Locked?** → delay, rejected-credential response. A *correct* token during
   lockout is still rejected; otherwise the lockout means nothing.
2. **Token mismatch?** → `record_failure`, delay, rejected-credential response.
3. **Success** → `login` calls `record_success` and sets the cookie; `auth_guard`
   calls `next.run(req)`.

### 5.1 Clearing the cookie on a rejected credential

Every rejected-credential response carries
`Set-Cookie: convertbar_token=; Max-Age=0; Path=/`.

Both enforcement sites build it through a **single shared constructor** in
`auth.rs` rather than assembling it independently, so the byte-identity that
goal 3 depends on holds by construction and cannot drift as either site is
edited later.

This is load-bearing, not hygiene. Without it the throttle turns a token
rotation into a self-inflicted permanent lockout:

- `src/lib/events.ts:20` opens an `EventSource("/api/events")` at module load,
  and `EventSource` **auto-reconnects indefinitely** (~3 s) on error. Nothing in
  the frontend closes it on 401 — and it cannot easily, since the `error` event
  exposes no status code.
- So any browser tab left open anywhere on the LAN after a token rotation would
  retry with its stale cookie every 3 seconds. Each retry is a credential-bearing
  failure. That reaches any threshold within a minute, and because attempts made
  while locked do not extend the lockout, the tab simply re-locks the moment each
  lockout expires — permanently, including for the owner's own machine.
- A page load has the same shape at smaller scale: 8–10 concurrent authenticated
  requests, all carrying the same stale cookie.

Clearing the cookie makes the storm self-healing: the first rejected reconnect
strips the credential, so every subsequent reconnect is credential-less and
therefore uncounted (§5's first asymmetry). The burst is bounded to the requests
already in flight, which `max_failures = 20` sits above.

This does not weaken goal 3. The indistinguishability that matters is
"wrong token" vs. "throttled" — and those two responses remain byte-identical,
because *both* clear the cookie. A client that sent only a bearer header
receives a pointless cookie-clear header, which costs nothing.

The uncredentialed 401 keeps today's plain response (no delay, no `Set-Cookie`).
It is distinguishable from a rejected credential, which is fine: the attacker
already knows whether they sent one.

Two deliberate asymmetries:

- **`auth_guard` does not throttle a request carrying no credential at all.**
  No delay, no counter, plain 401. The web UI fires several uncredentialed
  `/api/*` requests on page load specifically to trigger the login screen
  (`src/lib/transport/http.ts:30` dispatches `convertbar:unauthorized` on 401).
  Counting those would let a user lock themselves out before typing a character.
  A request is only a *guess* if it presented a credential.
- **`auth_guard` success does not reset the counter; only a successful login
  does.** This keeps the authenticated hot path (SSE, polling) free of a
  per-request map write. The lockout check on the hot path remains — one
  uncontended mutex acquisition, which is negligible next to the request itself
  and preserves the "while locked, no credential is accepted" invariant
  uniformly across both enforcement points.

Open mode (`AuthMode::Open`) short-circuits before any throttle interaction at
both sites, exactly as it does today.

### 6. Wiring

- `ServerState` gains `login_throttle: Arc<LoginThrottle>`.
- `main.rs` switches to
  `axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>())`
  so `ConnectInfo<SocketAddr>` is available to handlers.
- Handlers and middleware extract `Option<ConnectInfo<SocketAddr>>`, so the
  existing `oneshot`-based route tests — which never supply connect info —
  continue to work unchanged.
- `routes::tests::test_state()` builds the throttle with a zero-delay policy so
  the existing suite does not sleep.

## Testing

**Pure functions (no I/O, no sleeps):**

- Token strength: too short; exactly 16 accepted; 15 rejected; 16 chars with 1
  distinct rejected; exactly 8 distinct accepted; 7 distinct rejected;
  multi-byte characters counted as characters.
- `client_ip`: absent peer; untrusted peer with a forged `X-Forwarded-For`
  (header ignored); trusted peer with a single entry; trusted peer with a
  chain where the intermediate hops are trusted; **client-injected prefix**
  (`evil, real-client` where `real-client` is untrusted → `real-client`); all
  entries trusted → peer; malformed entry → peer; IPv6 with and without
  brackets/port; whitespace tolerance.
- `LoginThrottle`: first failure returns the delay and does not lock;
  `max_failures` within the window locks; a failure after the window elapses
  starts a fresh count; lockout releases exactly at `now >= locked_until` and
  resets the count; `record_success` clears; distinct IPs are independent
  buckets; `None` is independent of any address; pruning drops dead entries and
  keeps live ones.

**Integration, through `app()` (the real guard composition):**

The five load-bearing behaviours, each of which is a real bug if it regresses:

1. An `auth_guard` request with **no credential** does not advance the counter
   (repeat past `max_failures`, then confirm a valid credential still works).
2. After `max_failures` wrong credentials, the **correct** token is refused —
   at both `POST /api/login` and `auth_guard`.
3. The locked response is byte-identical to the wrong-token response (same
   status, same body, same `Set-Cookie`).
4. A successful login clears the counter (fail, succeed, fail again, confirm
   still unlocked).
5. A rejected **cookie** credential comes back with a cookie-clearing
   `Set-Cookie` — the mechanism that stops the SSE reconnect storm in §5.1, and
   the one whose absence would be invisible until a real token rotation.

Plus: open mode never engages the throttle; a missing `ConnectInfo` does not
panic; the configured delay is actually applied (one test with a small non-zero
delay asserting elapsed time, so a dropped `sleep` cannot pass silently).

**Config:** weak token rejected before the server binds; `CONVERTBAR_NO_AUTH=1`
unaffected by the floor; trusted-proxy parsing (CIDR, bare IP, invalid → error,
empty → empty vec).

**Not covered by automated tests:** the `into_make_service_with_connect_info`
change in `main.rs` — it cannot be exercised by the `oneshot` harness. Verified
by a manual run of the container against a real client, confirming two different
source addresses land in separate buckets.

## Documentation

- `README.md`: `CONVERTBAR_TRUSTED_PROXIES` row in the env-var table; the Auth
  section documents the token floor, the lockout behaviour (including that it
  refuses a correct token while locked), that rotating the token signs existing
  browsers out automatically, that a script looping on an outdated token will
  lock itself out, and the shared-bucket caveat below.
- `docker-compose.example.yml`: `"change-me"` is 9 characters and would now be
  rejected at startup — replaced with a generate-your-own instruction.
- `unraid-template.xml`: token field description gains the requirement.
- `docs/RECOMMENDATIONS.md`: item 15 moves to the shipped section.

## Risks and accepted trade-offs

- **Shared bucket behind an unconfigured proxy.** If the server sits behind a
  reverse proxy and `CONVERTBAR_TRUSTED_PROXIES` is not set, every client is
  seen as the proxy's address, so an attacker can lock the owner out for up to
  the lockout duration. This is why the variable exists; the README documents
  it at the point of use. The blast radius is bounded (5 minutes, no chaining).
- **Self-lockout.** Twenty wrong tokens in five minutes locks the owner out for
  five minutes, and the correct token will not release it early. This is
  standard lockout behaviour and is documented; the alternative (accepting a
  correct token while locked) would make the lockout decorative.
- **Distinct-character rule is not entropy.** `1234567890123456` passes. It is a
  floor against pathology, not a strength meter, and is described as such.
- **Parallel connections still bypass a per-request delay.** The fixed delay
  alone slows a serial attacker; the lockout is what bounds a parallel one. The
  combination is the defense, not either half.
- **A non-browser client looping on a stale bearer token will lock itself out.**
  Cookie clearing (§5.1) rescues browsers; a script with an outdated token in a
  retry loop has no such mechanism and is indistinguishable from an attacker.
  Correct behaviour, documented in the README rather than engineered around.
