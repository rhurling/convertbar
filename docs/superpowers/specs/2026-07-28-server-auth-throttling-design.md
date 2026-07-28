# Server Head — Login Throttling and a Token-Entropy Floor — Design

> Revision 2, after an adversarial review of revision 1. The review overturned
> three of revision 1's decisions; see **Changed after review** at the end for
> what and why. Revision 1 is in the git history of this file.

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
2. Make online guessing cost enough that it is not a viable attack.
3. **Never deny the owner access.** No input from an unauthenticated party may
   prevent a client holding the correct token from getting in.

Goal 3 is the constraint that shapes everything below, and it is why this design
has no lockout. See **Why no lockout**.

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

An **escalating per-bucket delay**. No lockout, no threshold, no denial.

```rust
pub struct ThrottlePolicy {
    pub base: Duration,    // 500ms — delay after the 1st failure
    pub cap: Duration,     // 30s   — ceiling
    pub window: Duration,  // 15min — failures older than this are forgotten
}

pub struct LoginThrottle {
    failures: Mutex<HashMap<ClientId, Failures>>,
    policy: ThrottlePolicy,
}

impl LoginThrottle {
    /// Records a rejected attempt; returns the delay the caller must apply.
    pub fn record_failure(&self, id: ClientId, now: Instant) -> Duration;
    /// Clears the bucket. Called only on a successful login.
    pub fn record_success(&self, id: ClientId);
}
```

`delay(n) = min(base << (n-1), cap)` — 500 ms, 1 s, 2 s, 4 s, 8 s, 16 s, then
pinned at the 30 s cap from the 7th failure on. The shift must be guarded
(`checked_shl` or an early `n > 20 → cap`) so a long-running bucket cannot
overflow.

Sustained *sequential* guessing — one guess, wait for the response, repeat —
therefore costs ~2 attempts/minute per bucket. The owner is never locked out —
their worst case is a 30-second wait, after which a correct token succeeds and
resets the bucket to zero. **This figure does not hold for a concurrent or
response-abandoning attacker; see the correction filed under "Why no lockout"
below.**

**The counter increments under the lock *before* the sleep, and the delay is
computed from the post-increment count.** This prevents an attacker from
resetting the ramp by opening more connections: concurrent attempts still draw
escalating counter values (500 ms, 1 s, 2 s … 30 s, 30 s, 30 s) rather than N
copies of the first delay. **It does not bound the attacker's throughput** —
evaluation happens before the sleep, so those escalating delays run
concurrently rather than serializing the guesses. See the correction below for
what this ordering does and does not achieve.

State per bucket: `count` and `first_at`. A failure with `now - first_at >
window` starts a fresh window at `count = 1`. `record_success` removes the entry.

Time is always an explicit `now: Instant` parameter rather than an internal
`Instant::now()`, so window rollover is deterministically testable with no
`sleep` in the unit suite. (`record_success` takes no `now`; clearing a bucket
is time-independent.)

**Lock poisoning:** all call sites use `.lock().unwrap_or_else(|e| e.into_inner())`.
This is the only new lock on a global request path — `auth_guard` layers over
every request including static assets — so a poisoned mutex would turn one panic
into a 500 on every request forever. A possibly-stale counter is the better
failure mode.

The map is pruned lazily inside `record_failure` once it exceeds 4096 keys:
first by dropping entries whose window has expired, then — if a live flood of
distinct sources leaves it still over the threshold, since nothing expired —
by evicting the lowest-`count` entries until it is back at 4096. Eviction
prefers count over age because a hot attacker's bucket is the one worth
keeping, and count-based eviction is harder to game: flooding fresh sources to
force an eviction only spends the flood's own low-count buckets. The map is
therefore bounded at ~4096 entries (a few hundred KB) regardless of how many
distinct sources attack concurrently.

### 5. Enforcement — `routes/login.rs` and `auth.rs::auth_guard`

`auth_guard` already exempts `POST /api/login` (`auth.rs:126`), so a login
attempt is counted at exactly one site. Both sites derive the bucket key through
the same `client_id`, so failures against the login route and against `/api/*`
accumulate in the **same** bucket.

**`auth_guard`, in this exact order:**

| # | Condition | Action |
|---|---|---|
| 1 | `AuthMode::Open` | pass through |
| 2 | exempt path (`POST /api/login`, any non-`/api`) | pass through |
| 3 | **no credential presented at all** | plain 401, no delay, no counter — *terminal* |
| 4 | credential matches | `next.run(req)` — no lock taken |
| 5 | credential mismatch | `record_failure`, sleep that long, 401 |

**`login`:** open mode → 204; token matches → `record_success`, set cookie, 204;
mismatch → `record_failure`, sleep, 401.

Two properties worth stating explicitly, because both were wrong in revision 1:

- **Step 3 is terminal and must come before any throttle interaction.** The web
  UI deliberately fires uncredentialed `/api/*` requests on load to trigger the
  login screen (`src/lib/transport/http.ts:30` dispatches
  `convertbar:unauthorized` on 401). A request is only a *guess* if it presented
  a credential. Charging those a delay would make the login screen slow exactly
  when the user needs it.
- **Step 4 precedes any throttle work, so the authenticated hot path (SSE,
  polling) takes no lock at all.** With no lockout there is no state to consult
  on success. Only failures touch the throttle.

The 401 body and status are unchanged from today (`{"error":"unauthorized"}`),
and **no `Set-Cookie` is attached** — see **Changed after review**.

**Logging:** `tracing::warn!` once per bucket, when it first reaches the delay
cap — matching `host_guard`'s convention of logging an operator-diagnosable
rejection (`auth.rs:89`). Not per attempt, which would make the log a flood
amplifier.

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

**Integration through `app()` (the real guard composition, with `MockConnectInfo`):**

1. An uncredentialed request returns the **plain, undelayed** 401 and does not
   advance the counter — repeat it many times, then confirm a valid credential
   still works immediately.
2. Failures at `POST /api/login` and at `/api/queue` accumulate in the **same**
   bucket (fail N at one, assert the delay continues escalating at the other).
3. A successful login clears the counter.
4. **A correct token always succeeds, no matter how many failures precede it** —
   the goal-3 regression test, and the one that would have caught revision 1.
5. A different source address is unaffected by another's failures.
6. The delay is actually applied and actually escalates: with a small non-zero
   `base`, assert elapsed time grows between successive failures, so a dropped
   `sleep` or a delay computed pre-increment cannot pass silently.
7. Open mode never engages the throttle.

**Real-listener test:** bind `127.0.0.1:0`, serve with
`into_make_service_with_connect_info`, and connect real clients. It must
*discriminate*, not merely return 401 — a 401 arrives either way, since the
`Unknown` bucket at a zero delay is behaviourally identical. Trust `127.0.0.1`
as a proxy, use a non-zero base delay, and drive two requests bearing different
`X-Forwarded-For` clients: with the wiring intact they occupy separate buckets
and the second is fast; without it both collapse into `Unknown` and the second is
delayed. This covers the one line `oneshot` cannot reach, and whose silent
regression would turn a per-source throttle into a global one.

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

## Why no lockout

The recommendation and the first revision both assumed a lockout. It cannot be
made to satisfy goal 3, because the two possible behaviours are exhaustive and
both fail:

- **A lockout that refuses a correct token** is a cheap, permanent denial of
  service. Sustaining it costs 20 requests per 5 minutes — 0.07 req/s — and the
  owner cannot escape it by knowing the password. In any deployment where
  clients share a source address (Docker Desktop's userland forwarding, NAT,
  VPN, or a reverse proxy without `CONVERTBAR_TRUSTED_PROXIES`), a single
  unauthenticated LAN device permanently bricks the owner's access.
- **A lockout that accepts a correct token** provides no rate limiting at all.
  Every guess is still evaluated and still answered; the attacker's throughput is
  unchanged. It is pure ceremony.

Revision 1 chose the first and justified it with "otherwise the lockout means
nothing." That reasoning was backwards: a lockout exists to stop *guessing*, and
a guesser does not hold the correct token. Honoring a correct token costs
essentially zero anti-guessing value and removes 100% of the DoS.

The escalating delay deters casual, sequential guessing with no denial state
to abuse, and is less code than a lockout. It does **not** make guessing
expensive in the sense originally claimed here — see the correction below,
filed after empirical review of the shipped implementation.

### Correction, filed after the fix shipped (empirical review)

The dichotomy above is false. It considers only two lockout behaviours and
misses a third: **gate evaluation behind the delay**, checking a per-bucket
`next_allowed_at` *before* `token_matches` runs, instead of sleeping after a
verdict is already known. That option rate-limits genuinely — no answer is
produced until the wait elapses, so an attacker cannot get a faster verdict by
opening more connections or abandoning ones it doesn't want to wait on — while
still never permanently denying the owner, since the wait is bounded by the
same 30 s cap. This option was not identified during design, is not what
shipped, and is a deliberate future decision rather than part of this
correction — see `docs/RECOMMENDATIONS.md` item 15.

**What the shipped delay (evaluate-then-sleep) actually achieves:** it deters
a *sequential, response-waiting* client — one that sends a guess, waits for
the 401, and only then sends the next. That client is genuinely held to
~2 attempts/minute once ramped.

**What it does not do:** rate-limit a client that parallelises or abandons the
connection. Because `token_matches` runs before the sleep, the verdict exists
immediately; the sleep only delays *when* the 401 is written, and nothing
forces the attacker to wait for it. Measured against this implementation: 20
simultaneous wrong guesses cost one delay, not twenty — throughput is
`concurrency / cap`, not `1 / cap`. A correct token is also served immediately
even at the cap, so response latency is a perfect oracle: send a guess, wait
~50 ms, abandon the socket if nothing came back. Measured: 8 guesses in 0.63 s
(versus ~64 s had the client waited out each delay sequentially); a tighter
loop reached ~18,000 guesses/sec.

**Consequence:** the escalating delay deters naive/sequential scanners and is
worth keeping for that reason — it is cheap and harmless to a legitimate
owner — but it is not the server's rate limit. **The 16-character/8-distinct
token floor (§1) is the actual defense**; at that length, throughput against
the delay is irrelevant to whether guessing is viable at all.

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
