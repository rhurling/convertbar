# Server Head — Login Throttling and a Token-Entropy Floor — Design

> **Revision 3.** An empirical review of the shipped revision-2 implementation
> found its escalating delay was not a rate limiter at all — it postponed the
> response after the verdict was already computed, so an attacker who abandoned
> the connection was unthrottled (~18,000 guesses/sec measured). Revision 3
> gates the *evaluation* instead. See **Why the delay was not enough**.
> Revisions 1 and 2 are in this file's git history.

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
2. **Bound online guess throughput per source.** Not "make guessing feel slow" —
   bound the number of times a source can learn whether a credential is correct.
3. Keep the owner's normal use unaffected, and their worst case bounded and
   self-clearing.

> **Revision 3 changed goal 2 and goal 3.** Revision 2 read goal 3 as "never deny
> the owner access" and built a post-evaluation delay to honour it. That delay is
> not a rate limiter — see **Why the delay was not enough**. Goal 3 as originally
> written is unachievable alongside goal 2, and this revision subordinates it.

## Non-goals

- No frontend changes.
- No new routes. `crates/convertbar-server/routes.json` is unchanged.
- No multi-user accounts, sessions, or in-app credential management. Still one
  static token. ("Rotating the token" always means the operator changing
  `CONVERTBAR_AUTH_TOKEN` and restarting the container.)
- `CONVERTBAR_NO_AUTH=1` keeps its current meaning: auth off entirely, no floor,
  no throttle.

## Design

### 1. Token strength floor — `config.rs`

`from_vars` gains a strength check on `CONVERTBAR_AUTH_TOKEN`:

> **at least 16 characters long, and using at least 8 distinct characters**

Length is the real defense. The distinct-character rule exists only to reject
pathological input like `aaaaaaaaaaaaaaaa` — it is not an entropy estimator and
deliberately is not one. Counting is over `char`s, not bytes.

Failing the check returns a new `ConfigError::WeakToken`. `main.rs` prints the
rule and a way to satisfy it, then exits 1:

```
convertbar-server: CONVERTBAR_AUTH_TOKEN is too weak — it must be at least 16
characters long and use at least 8 distinct characters.
Generate one with:  openssl rand -base64 24
```

**Precedence with `CONVERTBAR_NO_AUTH`:** `from_vars` checks the token *first*
(`config.rs:33-37`) and only falls through to `NO_AUTH` when no token is set.
That precedence is unchanged, so setting a weak token *and* `NO_AUTH=1` is a
startup failure, not a silent fallback to open mode. Contradictory auth config
should be surfaced, not guessed at. A test pins this.

A hard reject rather than a warning: the server head is one day old, so there is
effectively no installed base to break, and "a warning nobody reads" is the exact
failure mode the recommendation calls out.

### 2. Trusted proxies — `config.rs`

New environment variable `CONVERTBAR_TRUSTED_PROXIES`: comma-separated CIDR
ranges or bare IP addresses. Parsed into `Vec<IpNet>`; a bare address becomes a
full-length prefix (`/32` or `/128`).

Unset (the default) means no proxy is trusted and `X-Forwarded-For` is never
consulted — correct for a direct LAN deployment, and the right default.

An unparsable entry is a hard `ConfigError::BadTrustedProxy(entry)`, not a
skipped entry, with its own startup message naming the bad entry. Silently
dropping one would collapse every client into a single bucket, which is
precisely the failure this variable exists to prevent, and it would fail
invisibly.

> **Set this as narrowly as possible.** Every address in this set is trusted to
> assert who it is. A range that contains *clients* rather than only the proxy
> lets each of those clients forge a fresh identity per request and bypass the
> throttle entirely — strictly worse than leaving the variable unset. In
> particular, do **not** set it to a whole Docker bridge network
> (`172.18.0.0/16`) or a LAN range (`192.168.0.0/16`). For a proxy on a dynamic
> Docker address, pin the proxy to a compose static IP and list that `/32`.

CIDR support exists for genuinely-ranged proxy deployments, not as the
recommended shape. `ipnet` is already in `Cargo.lock` (2.12.0) via `hyper-util`,
so promoting it to a direct dependency adds no new crate to the build.

### 3. Client identity — new module `throttle.rs`

```rust
pub enum ClientId {
    Addr(IpAddr),      // IPv4 as-is; IPv6 truncated to its /64 network
    MalformedChain,    // trusted chain contained an unparsable entry
    Unknown,           // no ConnectInfo (should not occur in production)
}

pub fn client_id(
    peer: Option<IpAddr>,
    forwarded_for: &HeaderMap,
    trusted: &[IpNet],
) -> ClientId
```

**It takes the whole `HeaderMap`, not one `&str`.** `HeaderMap::get` returns only
the *first* `X-Forwarded-For` line, and HAProxy, Traefik, Caddy and Apache all
*append a new header line* rather than merging into the existing one. Reading
only the first line means reading the attacker's own line and ignoring the
proxy's — a total bypass. The implementation must concatenate `get_all` in
header order, then split on commas.

Resolution:

1. `peer` absent → `Unknown`.
2. `peer` **not** in `trusted` → `Addr(peer)`, ignoring `X-Forwarded-For`
   entirely. A header from an untrusted hop is attacker-controlled.
3. `peer` in `trusted` → walk the combined chain **right-to-left**:
   - unparsable entry → `MalformedChain` (see below)
   - parses and is in `trusted` → continue leftward
   - parses and is not in `trusted` → `Addr(that entry)`
4. Walk ran off the left end, or the header is absent/empty → `Addr(peer)`.

**Why `MalformedChain` is its own bucket, not a fallback to `peer`:** any client
can put garbage in the header. If garbage collapsed onto the proxy's address,
a client could *choose* to join the bucket shared by everyone behind that proxy
and inflate its delay for all of them. A dedicated sentinel means garbage-senders
only slow each other down.

**Normalization.** `IpAddr::to_canonical()` is applied to `peer` and to every
parsed entry before any `contains` check or bucket keying, so an IPv4-mapped
IPv6 address (`::ffff:192.168.1.5`, which is what a `CONVERTBAR_BIND=::`
dual-stack listener delivers for IPv4 clients) matches an IPv4 `trusted` entry
and shares one bucket with the same client arriving over IPv4.

**IPv6 is keyed on the /64 network, not the address.** Every SLAAC/privacy-
extensions host owns 2^64 addresses. Per-address bucketing would let any IPv6
client take a virgin bucket per guess, which is no throttle at all.

**Parsing an entry: parse first, strip second.** Try `IpAddr::from_str`; on
failure try `SocketAddr::from_str` and take `.ip()`; otherwise it is malformed.
**Do not reuse `auth.rs`'s private `strip_port`** — it strips right-to-left on
`:` and turns bare `2001:db8::1` into `2001:db8:` (verified). It is correct for
the Host headers it was written for and wrong here.

`X-Forwarded-For` only. `X-Real-IP` carries no chain semantics, so honoring it
would add a second, weaker spoofing surface with no capability the first does
not already provide.

### 4. `LoginThrottle` — `throttle.rs`

A **per-source evaluation gate**. The unit being limited is not the response —
it is the *credential comparison itself*.

```rust
pub enum Gate {
    /// Evaluate the credential. This consumed the bucket's slot.
    Allow,
    /// Do NOT evaluate. Answer 401 immediately.
    Deny,
}

pub struct ThrottlePolicy {
    pub free: u32,         // 8     — evaluations before gating begins
    pub base: Duration,    // 500ms — spacing after the first gated evaluation
    pub cap: Duration,     // 30s   — spacing ceiling
    pub window: Duration,  // 15min — attempts older than this are forgotten
}

impl LoginThrottle {
    /// Counts and reserves this source's evaluation slot. MUST be called
    /// BEFORE comparing the credential — that ordering is the entire defense.
    pub fn check(&self, id: ClientId, now: Instant) -> Gate;
    /// Clears the bucket. Called when an allowed evaluation accepted it.
    pub fn record_success(&self, id: ClientId);
}
```

`spacing(n) = 0` for `n <= free`, else `min(base << (n - free - 1), cap)` — so
attempts 1-8 are ungated, then 500 ms, 1 s, 2 s, 4 s, 8 s, 16 s, and pinned at
the 30 s cap from the 15th attempt on. The shift is clamped so a long-lived
bucket cannot overflow, and the curve stays correct for a zero `base`.

**`check` counts and reserves; it does not merely observe.** Under the lock it
compares `now` against the bucket's `next_at` and, when it allows, increments the
attempt count and sets `next_at = now + spacing(count)` before releasing.

The increment lives in `check`, not on the failure path, and that placement is
load-bearing. Counting only failures leaves every concurrent request inside the
free window observing the same open slot — measured at 5-8 evaluations against a
budget of 4 on an 8-core machine, and 64/64 with a barrier. Counting the attempt
when it is *allowed* makes the budget exact: on a virgin bucket at one instant,
exactly `free + 1` requests are admitted and the next is denied, whatever the
concurrency. A legitimate client's increments are undone by `record_success`,
which clears the bucket outright.

**The first `free` attempts are ungated** so that ordinary use is untouched: a
mistyped token, a stale cookie, and the web UI's concurrent page-load fan-out all
stay under the threshold and never see a denial. `free` is 8 rather than the
fan-out's measured 4 because `check` now counts successful attempts too, and a
threshold sitting exactly on the measured fan-out has no headroom for a fifth
concurrent call. Eight free guesses per window is irrelevant against a
16-character token.

Time is always an explicit `now: Instant` parameter, so every property above is
testable without a single `sleep` — the suite gained determinism as a side
effect of this redesign.

**Nothing sleeps.** Every response is immediate, which is why this is a rate
limiter and the previous design was not: a client learns nothing by abandoning
a connection, and holds no server resource by keeping one open.

Lock poisoning is swallowed (`.lock().unwrap_or_else(|e| e.into_inner())`) — this
lock sits on a global request path, so one panic must not 500 every subsequent
request forever. The map is bounded at `PRUNE_THRESHOLD` keys, evicting expired
entries first and then lowest-`count` entries, so hot attackers are retained and
the long tail is shed.

### 5. Enforcement — `routes/login.rs` and `auth.rs::auth_guard`

`auth_guard` already exempts `POST /api/login` (`auth.rs:126`), so an attempt is
gated at exactly one site. Both sites derive the bucket key through the same
`client_id`, so attempts against the login route and against `/api/*` share one
bucket and one rate.

**`auth_guard`, in this exact order:**

| # | Condition | Action |
|---|---|---|
| 1 | `AuthMode::Open` | pass through |
| 2 | exempt path (`POST /api/login`, any non-`/api`) | pass through |
| 3 | **no credential presented at all** | plain 401, no gate, no counter — *terminal* |
| 4 | `check` returns `Deny` | 401 **without comparing the credential** |
| 5 | credential matches | `record_success`, `next.run(req)` |
| 6 | credential mismatch | 401 (the attempt was already counted by `check`) |

**`login`:** open mode → 204; `Deny` → 401 without comparing; matches →
`record_success`, set cookie, 204; mismatch → 401.

Three properties that carry the design:

- **Step 4 precedes step 5.** This is the whole feature. If the comparison ran
  first, the attacker would learn the answer regardless of what the gate did
  afterwards, and no amount of delaying the response would change that.
- **Step 3 is terminal and ungated.** The web UI deliberately fires
  uncredentialed `/api/*` requests on load to trigger the login screen
  (`src/lib/transport/http.ts:30` dispatches `convertbar:unauthorized` on 401).
  A request is only a guess if it presented a credential; gating these would
  spend the bucket's budget on the login screen rendering itself.
- **A `Deny` and a wrong credential are the same response** —
  `401 {"error":"unauthorized"}`, no `Set-Cookie`, both immediate. The attacker
  cannot tell whether their guess was tested, so to *guarantee* a test they must
  wait out the spacing. That is what converts the spacing into a rate.

**Logging:** `tracing::warn!` once per ramp, when a bucket first reaches the
spacing cap — matching `host_guard`'s convention of logging an
operator-diagnosable rejection (`auth.rs:89`). Not per attempt, which would make
the log a flood amplifier.
### 6. Wiring

- `ServerState` gains `login_throttle: Arc<LoginThrottle>`. Only two sites
  construct it by struct literal (`main.rs:65`, `routes/mod.rs:194`).
- `main.rs` switches to
  `axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>())`.
- **The peer address is read from request extensions**
  (`req.extensions().get::<ConnectInfo<SocketAddr>>().map(|c| c.0.ip())`), not
  via an `Option<ConnectInfo<..>>` extractor — that does not satisfy axum 0.8's
  middleware trait bounds and fails to compile (verified against this
  repository's axum version). `auth_guard` already owns the `Request`; the
  `login` handler takes the peer the same way.
- Tests supply connect info with an **`Extension(ConnectInfo(addr))` layer**, not
  `MockConnectInfo`. `MockConnectInfo` inserts an extension of type
  `MockConnectInfo<T>`, and only the `ConnectInfo` *extractor* knows to fall back
  to it — so a middleware reading `extensions().get::<ConnectInfo<_>>()` sees
  nothing and every mocked request would silently land in the `Unknown` bucket,
  making the per-source tests pass for the wrong reason. `Extension(ConnectInfo(addr))`
  inserts exactly what `into_make_service_with_connect_info` inserts in
  production. Both behaviours verified empirically against this repository's axum
  version.

## Testing

**Pure functions, no sleeps:**

- Token strength: 15 rejected / 16 accepted; 16 chars 1 distinct rejected;
  exactly 8 distinct accepted, 7 rejected; multi-byte counted as characters.
- `client_id`: absent peer → `Unknown`; untrusted peer with a forged header
  (ignored); trusted peer, single entry; trusted peer, chain with trusted
  intermediates; client-injected prefix (`evil, real` where `real` is untrusted
  → `real`); all entries trusted → peer; **two separate `X-Forwarded-For` header
  lines** (the §3 bypass); malformed entry → `MalformedChain`, *not* the peer;
  IPv4-mapped IPv6 peer matches an IPv4 trusted entry; two IPv6 addresses in one
  /64 share a bucket; bare IPv6 and `[v6]:port` both parse; whitespace.
- `LoginThrottle`: delay doubles per failure and pins at `cap`; no overflow at
  high counts; window rollover resets; `record_success` clears; distinct ids are
  independent buckets; pruning drops expired and keeps live entries.

**Integration through `app()` (the real guard composition, with an
`Extension(ConnectInfo(addr))` layer — never `MockConnectInfo`, which inserts a
different extension type that the guard cannot see):**

No test asserts elapsed wall-clock time. Gating is observed by *status*, using a
policy with `free: 0` and a long `base` so the gate shuts on the first attempt
and stays shut for the test's duration.

1. An uncredentialed request returns the plain 401 and never consumes a slot —
   repeat it well past `free`, then confirm a valid credential still works.
2. Attempts at `POST /api/login` and at `/api/queue` share one gate: shut it via
   one route, then confirm the other refuses a **correct** token.
3. A successful evaluation reopens the gate for that source — and the test must
   share one `ServerState`, since a second `test_state()` builds its own
   `LoginThrottle` and would pass regardless.
4. **A shut gate refuses even a correct token.** This is the rate limit; a gate
   that still honoured a correct token would still answer every guess, which is
   exactly how revision 2 failed.
5. A `Deny` and a wrong-credential rejection are byte-identical — same status,
   same body, no `Set-Cookie` on either.
6. Exactly `free + 1` evaluations are admitted at a single instant on a virgin
   bucket, and the next is denied — the property that a read-then-write gap in
   `check` would break.
7. A different source address is unaffected by another's shut gate.
8. Open mode never gates.

**Real-listener test:** bind `127.0.0.1:0`, serve with
`into_make_service_with_connect_info`, and connect real clients. It must
*discriminate*, not merely return 401 — a 401 arrives either way. Trust
`127.0.0.1` as a proxy and drive two requests bearing different
`X-Forwarded-For` clients: with the wiring intact they occupy separate buckets
and the second client is still evaluated; without it both collapse into
`Unknown` and the second is refused. This covers the one line `oneshot` cannot
reach, and whose silent regression would turn a per-source limiter into a global
one.

**Config:** weak token rejected; weak token + `NO_AUTH=1` still rejected;
`NO_AUTH=1` alone unaffected; trusted-proxy parsing (CIDR, bare IP, invalid →
`BadTrustedProxy`, empty → empty vec).

**Existing tests this breaks — must be updated as an explicit plan task:**

- Eight `config.rs` tests pass tokens below the floor (`"secret"`, `"t"`) and
  would now fail before reaching the behaviour they test: lines 97, 118, 129,
  139, 149, 162, 172, 182. Each needs a conforming token.
- `main.rs:22-32` matches `ConfigError` exhaustively; two new variants are a
  compile error until both arms exist.
- `auth.rs:277 token_state("secret")` constructs `AuthMode::Token` directly and
  never calls `from_vars` — it is **not** affected, and neither are the two
  `*_sets_no_cookie` tests, since this revision attaches no `Set-Cookie`.

## Documentation

- `README.md`: `CONVERTBAR_TRUSTED_PROXIES` row plus the narrow-as-possible
  warning from §2; the Auth section documents the token floor and that repeated
  failures slow *that source* down while never blocking a correct token.
- `docker-compose.example.yml`: `"change-me"` is 9 characters and would now be
  rejected at startup — replaced with a generate-your-own instruction.
- `unraid-template.xml`: token field description gains the requirement.
- `docs/RECOMMENDATIONS.md`: item 15 moves to the shipped section.

## Why the delay was not enough

Revisions 1 and 2 argued about lockouts and reached a false dichotomy:

- **A lockout that refuses a correct token** is a cheap, permanent denial of
  service against the owner.
- **A lockout that accepts a correct token** provides no rate limiting at all —
  every guess is still evaluated and answered.

Revision 2 concluded these were exhaustive, rejected both, and shipped an
escalating delay applied *after* `token_matches`. That was wrong, and the error
is instructive enough to keep on the record.

**What the evaluate-then-sleep design actually achieved:** it deterred a
*sequential, response-waiting* client — one that sends a guess, waits for the
401, and only then sends the next — holding it to ~2 attempts/minute once
ramped.

**Why it was not a rate limiter.** Because the comparison ran before the sleep,
the verdict existed immediately; the sleep only postponed *writing* it, and
nothing compelled the attacker to wait. Measured against the shipped code:

- 20 simultaneous wrong guesses cost one delay, not twenty — throughput was
  `concurrency / cap`, not `1 / cap`.
- A correct token was served immediately even with the bucket pinned at the
  30 s cap, so response latency was a perfect oracle: send a guess, wait ~50 ms,
  abandon the socket if nothing came back. Measured: 8 guesses in 0.63 s against
  ~64 s of nominal delay; a tighter loop reached ~18,000 guesses/sec.

**The missing third option** — and what revision 3 implements — is to gate the
*evaluation* rather than the response: check the bucket's `next_at` **before**
`token_matches` runs, and answer 401 without comparing when the slot is not yet
open. Then abandoning a connection buys nothing (no verdict was computed to
abandon) and opening more connections buys nothing (the slot is reserved under
the same lock that reads it).

**The cost, stated plainly.** Any correct rate limiter must sometimes refuse to
answer whether a credential is valid — that refusal *is* the limit. So a correct
token is sometimes refused, not merely delayed. This is unavoidable rather than
chosen: a mechanism that always honours a correct token also always answers the
attacker, which is exactly how revision 2 failed. Revision 3 therefore
subordinates the old goal 3 and bounds the cost instead of eliminating it:

- The first `free` (3) failures are ungated, so typos, a stale cookie, and the
  web UI's concurrent page-load fan-out never see a denial.
- A successful evaluation clears the bucket outright.
- Failures are forgotten after the 15-minute window.
- A source only shares a bucket with an attacker when it shares a source
  address — the case `CONVERTBAR_TRUSTED_PROXIES` exists to eliminate.

On a normal LAN, where the attacker holds a different address, the owner's
bucket is untouched and they are never gated at all.

## Changed after review

Revision 1 was reviewed adversarially; three decisions were overturned.

1. **Lockout → escalating delay** (above). This reverses an explicit earlier
   decision and needs sign-off.
2. **Cookie-clearing on rejected credentials: dropped.** It was justified by the
   claim that `EventSource` retries a 401 indefinitely, creating a stale-cookie
   storm. That claim is false: per the HTML spec, a non-200 response causes the
   UA to *fail the connection* — `readyState` goes to `CLOSED` and it does not
   reconnect — and nothing in `src/lib/events.ts` recreates the source. A token
   rotation costs one failure per open tab, not a storm. The mechanism was also
   harmful: under a lockout it destroyed a *valid* cookie, upgrading an
   attacker-triggered lockout into a forced logout. The existing
   `convertbar:unauthorized` → `LoginScreen` flow already handles stale
   credentials.
3. **`max_failures = 20`: gone with the lockout.** It had been sized against a
   claimed 8–10 concurrent authenticated requests per page load; the real
   production fan-out is 4 (`events.ts:20`, `App.tsx:33`, `useQueue.ts:16`,
   `QueuePage.tsx:50`). The 8–10 figure reflected `React.StrictMode`'s
   double-invocation in dev, which the shipped container does not do.

Also corrected from review: multiple `X-Forwarded-For` header lines (§3), the
broad-CIDR guidance which was itself the attack (§2), `MalformedChain` bucketing
(§3), IPv6 /64 and IPv4-mapped normalization (§3), the non-compiling
`Option<ConnectInfo>` (§6), `strip_port`'s IPv6 destruction (§3), lock poisoning
(§4), the `NO_AUTH` precedence ambiguity (§1), missing lockout logging (§5), and
the ten existing tests that break (Testing).

## Risks and accepted trade-offs

- **A correct token is refused while its bucket is gated.** Inherent to rate
  limiting (see **Why the delay was not enough**), bounded by the spacing cap
  (30 s), cleared by any successful evaluation, and forgotten after the window.
  It only arises when the owner shares a source address with an attacker.

- **Shared bucket.** Where clients share a source address, one attacker's
  failures slow everyone's *failed* attempts on that address. A correct token
  still succeeds, so this is degradation, not denial. `CONVERTBAR_TRUSTED_PROXIES`
  fixes it for real proxies; it cannot help with L3 NAT, where there is no
  forwarded header to read.
- **The delay leaks a bucket's failure count** to anyone sharing it. It reveals
  nothing about the token, and the alternative (a constant delay) is a weaker
  throttle.
- **Distinct-character rule is not entropy.** `1234567890123456` passes. It is a
  floor against pathology, not a strength meter.
- **`MalformedChain` is one shared bucket**, so clients sending garbage headers
  slow each other. They are already misbehaving, and the alternative (trusting
  the garbage) is worse.
- **A stale cookie after a token rotation shares the bucket's ramp.** The old
  cookie is a credential like any other, so a rejected one is throttled the
  same as a guessed token. If the owner's source address also belongs to an
  attacker who has driven that bucket to the cap, the owner's login screen can
  take up to 30 s to render — the frontend only shows it once the 401 arrives
  (`convertbar:unauthorized`). Recovery is fast once they type the new token:
  a successful `POST /api/login` never sleeps.
