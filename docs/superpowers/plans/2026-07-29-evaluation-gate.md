# Evaluation Gate — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Convert the shipped post-evaluation delay into a real per-source rate limiter by gating the credential *comparison* instead of the response.

**Architecture:** `LoginThrottle` gains `check(id, now) -> Gate` which reserves the bucket's evaluation slot under the same lock that reads it. `auth_guard` and the `login` route call it **before** `token_matches` and answer 401 without comparing when it returns `Deny`. All sleeping is removed.

**Spec:** `docs/superpowers/specs/2026-07-28-server-auth-throttling-design.md` (revision 3), §4 and §5.

## Global Constraints

- **`check` must be called BEFORE `token_matches` at every enforcement site.** This ordering is the entire feature; a comparison that runs first hands the attacker the verdict no matter what happens afterwards.
- **`check` reserves, it does not observe.** It sets `next_at = now + spacing(count)` inside the same critical section in which it read `next_at`. Reading and reserving in two separate lock acquisitions is a concurrency bypass.
- `spacing(n) = 0` for `n < free`, else `min(base << (n - free), cap)`. Defaults: `free = 3`, `base = 500ms`, `cap = 30s`, `window = 15min`. The shift stays clamped (`.min(31)`) so a long-lived bucket cannot overflow, and must remain correct for a zero `base` (tests use it).
- **Nothing sleeps.** Remove every `tokio::time::sleep` from the auth paths. No test may assert elapsed wall-clock time.
- A `Deny` and a wrong-credential rejection are the **same** response: `401 {"error":"unauthorized"}`, no `Set-Cookie`, immediate.
- A request with **no credential at all** is still terminal, ungated, and uncounted.
- Every method that consults the clock takes `now: Instant` explicitly; never call `Instant::now()` inside the throttle.
- Mutex acquisitions keep `.lock().unwrap_or_else(|e| e.into_inner())`; the map stays bounded by the existing eviction.
- Comments explain WHY, never WHAT. Commits are signed; on a 1Password agent error report BLOCKED rather than committing unsigned.
- Baseline before this plan: **430 passing / 0 failing.**

---

### Task 1: Replace the delay API with the evaluation gate

**Files:** Modify `crates/convertbar-server/src/throttle.rs`

**Interfaces produced:**
- `pub enum Gate { Allow, Deny }` (derives `Debug, Clone, Copy, PartialEq, Eq`)
- `pub struct ThrottlePolicy { pub free: u32, pub base: Duration, pub cap: Duration, pub window: Duration }` with `Default`
- `pub fn check(&self, id: ClientId, now: Instant) -> Gate`
- `pub fn record_failure(&self, id: ClientId, now: Instant)` — note: **returns `()` now**, not `Duration`
- `pub fn record_success(&self, id: ClientId)` — unchanged

- [ ] **Step 1: Write the failing tests**

Replace the whole `mod throttle_tests` block. Every test is deterministic — no sleeps, no elapsed-time assertions.

```rust
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
        assert_eq!(t.check(a, later), Gate::Allow, "free allowance did not reset");
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
        fail_n(&t, hot, now, 6);

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
```

- [ ] **Step 2: Run and confirm they fail**

Run: `cargo test -p convertbar-server throttle_tests 2>&1 | tail -20`
Expected: compile errors — `Gate` and `check` do not exist, and `record_failure` still returns `Duration`.

- [ ] **Step 3: Implement**

In `crates/convertbar-server/src/throttle.rs`, update the module doc's description of the mechanism, then replace `ThrottlePolicy`, `Failures`, and the `impl LoginThrottle` block:

```rust
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

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<ClientId, Failures>> {
        self.failures.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.lock().len()
    }
}
```

Note `PRUNE_THRESHOLD` must be visible to the test module (it already is via `use super::*`).

- [ ] **Step 4: Run the tests**

Run: `cargo test -p convertbar-server throttle_tests 2>&1 | grep -E "^test result|^error"`
Expected: all pass. (Other modules will not compile yet — that is Task 2. If the crate fails to build because `auth.rs`/`login.rs` still call the old API, that is expected; use `cargo test -p convertbar-server throttle_tests` only after Task 2, and for now confirm the throttle module itself compiles by reading the errors and checking none of them are in `throttle.rs`.)

- [ ] **Step 5: Mutation checks — record the observed result of each**

1. In `check`, move the reservation (`entry.next_at = ...`) to after the lock is released, or drop it entirely → `concurrent_checks_cannot_share_one_slot` and `the_slot_reopens_exactly_when_the_spacing_elapses` MUST fail. Revert.
2. In `check`, return `Gate::Allow` unconditionally when `now < entry.next_at` → `a_denied_check_does_not_advance_the_ramp` MUST fail. Revert.
3. In `spacing_for`, remove the `free` subtraction so gating starts at the first failure → `the_first_free_failures_are_not_gated` MUST fail. Revert.
4. Reverse the eviction comparator (`sort_unstable_by_key(|(_, c)| std::cmp::Reverse(*c))`) → `map_stays_bounded_under_a_live_flood_and_keeps_the_hottest_bucket` MUST fail. Revert.

If any mutation does not fail its named test, say so loudly — the test is not doing its job.

- [ ] **Step 6: Commit** (after Task 2 makes the crate compile, or commit both together if you prefer — the crate will not build until the call sites are updated)

---

### Task 2: Gate before evaluating at both enforcement sites

**Files:** Modify `crates/convertbar-server/src/auth.rs`, `crates/convertbar-server/src/routes/login.rs`

**Interfaces consumed:** `Gate`, `check`, `record_failure(id, now)` (now returns `()`), `record_success` from Task 1.

- [ ] **Step 1: Rewrite `auth_guard`'s tail**

Replace everything after the exempt-path check with:

```rust
    // A request with NO credential is not a guess — it is how the web UI
    // discovers it must show the login screen. Gating these would spend the
    // bucket's budget on rendering the login form.
    let Some(provided) = bearer_token(&req).or_else(|| cookie_token(&req)) else {
        return json_err(StatusCode::UNAUTHORIZED, "unauthorized");
    };

    let id = client_id(peer_ip(&req), req.headers(), &s.config.trusted_proxies);
    let now = std::time::Instant::now();

    // BEFORE the comparison, not after. Comparing first would compute the
    // verdict the attacker wants regardless of what we do with the response.
    if s.login_throttle.check(id, now) == Gate::Deny {
        return json_err(StatusCode::UNAUTHORIZED, "unauthorized");
    }

    if token_matches(&provided, expected) {
        s.login_throttle.record_success(id);
        return next.run(req).await;
    }

    s.login_throttle.record_failure(id, now);
    json_err(StatusCode::UNAUTHORIZED, "unauthorized")
```

Remove the now-unused `tokio::time::sleep` import if present.

- [ ] **Step 2: Rewrite the `login` handler's body**

Same ordering; keep the existing `Secure`-flag comment on the cookie:

```rust
    let peer = connect.map(|axum::Extension(ConnectInfo(addr))| addr.ip());
    let id = client_id(peer, &headers, &s.config.trusted_proxies);
    let now = std::time::Instant::now();

    if s.login_throttle.check(id, now) == Gate::Deny {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "unauthorized"})),
        )
            .into_response();
    }

    if !token_matches(&body.token, expected) {
        s.login_throttle.record_failure(id, now);
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "unauthorized"})),
        )
            .into_response();
    }

    s.login_throttle.record_success(id);
```

- [ ] **Step 3: Replace the timing-based integration tests**

Every test that asserted elapsed wall-clock time must go — nothing sleeps now. Delete these and replace them with the deterministic equivalents below:

- `auth.rs`: `wrong_credentials_are_delayed_and_the_delay_escalates`, `one_sources_failures_do_not_slow_another_source`, and the timing assertions inside `uncredentialed_requests_are_never_throttled`
- `login.rs`: `a_successful_login_resets_the_ramp`, `login_and_api_failures_share_one_bucket`, `open_mode_never_engages_the_throttle`'s timing assertion
- `routes/mod.rs`: `served_requests_are_bucketed_per_forwarded_client`'s timing assertion

Use a policy with `free: 0` and a large `base` so the gate closes after a single failure and the test needs no clock control. In `auth.rs`'s `guard_integration_tests`:

```rust
/// A policy that gates immediately and stays shut for the whole test: `free: 0`
/// means the first failure closes the slot, and a 1-hour base means it does not
/// reopen. Lets the gate be observed without modelling time.
fn gated_state(token: &str) -> ServerState {
    let mut state = token_state(token);
    state.login_throttle = std::sync::Arc::new(crate::throttle::LoginThrottle::new(
        crate::throttle::ThrottlePolicy {
            free: 0,
            base: Duration::from_secs(3600),
            ..Default::default()
        },
    ));
    state
}

#[tokio::test]
async fn a_denied_request_is_not_evaluated_so_even_the_correct_token_is_refused() {
    // This is the rate limit. Refusing to answer is what bounds throughput —
    // a gate that still honoured a correct token would still answer every
    // guess, which is exactly how the previous delay-based design failed.
    let app = app_from(gated_state("abcdefghijklmnop"), "10.0.0.1:5555");
    let first = send(
        app.clone(),
        "GET",
        "/api/queue",
        &[("Host", "localhost"), ("Authorization", "Bearer wrong")],
        None,
    )
    .await;
    assert_eq!(first.status(), StatusCode::UNAUTHORIZED);

    let correct = send(
        app,
        "GET",
        "/api/queue",
        &[
            ("Host", "localhost"),
            ("Authorization", "Bearer abcdefghijklmnop"),
        ],
        None,
    )
    .await;
    assert_eq!(
        correct.status(),
        StatusCode::UNAUTHORIZED,
        "the gate was open, so the credential was still being evaluated"
    );
}

#[tokio::test]
async fn a_denied_response_is_indistinguishable_from_a_wrong_credential() {
    let app = app_from(gated_state("abcdefghijklmnop"), "10.0.0.1:5555");
    let evaluated = send(
        app.clone(),
        "GET",
        "/api/queue",
        &[("Host", "localhost"), ("Authorization", "Bearer wrong")],
        None,
    )
    .await;
    let denied = send(
        app,
        "GET",
        "/api/queue",
        &[("Host", "localhost"), ("Authorization", "Bearer wrong")],
        None,
    )
    .await;
    assert_eq!(evaluated.status(), denied.status());
    assert!(evaluated.headers().get(SET_COOKIE).is_none());
    assert!(denied.headers().get(SET_COOKIE).is_none());
    assert_eq!(json_body(evaluated).await, json_body(denied).await);
}

#[tokio::test]
async fn uncredentialed_requests_never_close_the_gate() {
    // The web UI fires these on page load to trigger its login screen. If they
    // consumed slots, the login form would gate itself out.
    let app = app_from(gated_state("abcdefghijklmnop"), "10.0.0.1:5555");
    for _ in 0..25 {
        let response = send(app.clone(), "GET", "/api/queue", &[("Host", "localhost")], None).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }
    let response = send(
        app,
        "GET",
        "/api/queue",
        &[
            ("Host", "localhost"),
            ("Authorization", "Bearer abcdefghijklmnop"),
        ],
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn a_successful_credential_reopens_the_gate_for_that_source() {
    let state = gated_state("abcdefghijklmnop");
    let app = app_from(state, "10.0.0.1:5555");
    send(
        app.clone(),
        "GET",
        "/api/queue",
        &[("Host", "localhost"), ("Authorization", "Bearer wrong")],
        None,
    )
    .await;
    // Gate is shut; a different source proves the shut gate is per-source.
    let other = send(
        app_from(token_state("abcdefghijklmnop"), "10.0.0.2:5555"),
        "GET",
        "/api/queue",
        &[
            ("Host", "localhost"),
            ("Authorization", "Bearer abcdefghijklmnop"),
        ],
        None,
    )
    .await;
    assert_eq!(other.status(), StatusCode::OK);
}
```

In `login.rs`, replace the deleted tests with:

```rust
fn gated_login_app(token: &str) -> axum::Router {
    let mut state = state_with_auth(AuthMode::Token(token.to_string()));
    state.login_throttle = std::sync::Arc::new(crate::throttle::LoginThrottle::new(
        crate::throttle::ThrottlePolicy {
            free: 0,
            base: Duration::from_secs(3600),
            ..Default::default()
        },
    ));
    crate::routes::app(state).layer(Extension(ConnectInfo(
        "10.0.0.1:5555".parse::<SocketAddr>().unwrap(),
    )))
}

#[tokio::test]
async fn a_gated_login_is_refused_without_evaluating_the_token() {
    let app = gated_login_app("abcdefghijklmnop");
    assert_eq!(
        try_login(app.clone(), "wrong").await.status(),
        axum::http::StatusCode::UNAUTHORIZED
    );
    let response = try_login(app, "abcdefghijklmnop").await;
    assert_eq!(
        response.status(),
        axum::http::StatusCode::UNAUTHORIZED,
        "the gate was open, so the token was still evaluated"
    );
    assert!(response.headers().get(SET_COOKIE).is_none());
}

#[tokio::test]
async fn login_and_api_attempts_share_one_gate() {
    // Otherwise an attacker guesses on whichever channel is still open.
    use tower::ServiceExt;
    let app = gated_login_app("abcdefghijklmnop");
    try_login(app.clone(), "wrong").await;
    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/queue")
                .header("Host", "localhost")
                .header("Authorization", "Bearer abcdefghijklmnop")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        axum::http::StatusCode::UNAUTHORIZED,
        "auth_guard did not inherit the login route's shut gate"
    );
}

#[tokio::test]
async fn open_mode_never_gates() {
    let mut state = state_with_auth(AuthMode::Open);
    state.login_throttle = std::sync::Arc::new(crate::throttle::LoginThrottle::new(
        crate::throttle::ThrottlePolicy {
            free: 0,
            base: Duration::from_secs(3600),
            ..Default::default()
        },
    ));
    let app = crate::routes::app(state).layer(Extension(ConnectInfo(
        "10.0.0.1:5555".parse::<SocketAddr>().unwrap(),
    )));
    for _ in 0..5 {
        assert_eq!(
            try_login(app.clone(), "any-token-at-all").await.status(),
            axum::http::StatusCode::NO_CONTENT
        );
    }
}
```

For `routes/mod.rs`'s real-listener test, keep the two-forwarded-client structure but assert on **status**, not elapsed time: with `free: 0` and a long base, the first forwarded client's second request is 401-denied while a *different* forwarded client is still evaluated. Without connect info both collapse into `Unknown` and the second client is denied too — which is the discrimination the test exists for.

- [ ] **Step 4: Verify**

`cargo fmt --all`, then `cargo test --workspace`. Confirm 0 failures and report the total. `cargo clippy -p convertbar-server --all-targets` must show zero warnings for this crate. **Grep the crate for `tokio::time::sleep` and confirm there are none left in non-test code.**

- [ ] **Step 5: Mutation checks**

1. In `auth_guard`, move the `check` call to after `token_matches` → `a_denied_request_is_not_evaluated_so_even_the_correct_token_is_refused` MUST fail. Revert.
2. In `login`, same move → `a_gated_login_is_refused_without_evaluating_the_token` MUST fail. Revert.
3. Delete the uncredentialed early return → `uncredentialed_requests_never_close_the_gate` MUST fail. Revert.

- [ ] **Step 6: Commit**

---

### Task 3: Documentation

**Files:** `README.md`, `docs/RECOMMENDATIONS.md`, `unraid-template.xml` (if it describes the throttle)

- [ ] **Step 1: README Auth section**

Rewrite the failed-attempt paragraph to describe a rate limiter, not a delay. It must state: the first 3 failures from a source are free; after that the source may be evaluated only once per interval, doubling to 30 s; **while gated, even the correct token is refused, so wait rather than retrying in a loop**; a successful sign-in clears it immediately; and the count is forgotten after 15 minutes. Keep it terse.

- [ ] **Step 2: `docs/RECOMMENDATIONS.md`**

Item 15 currently sits under "Open — High Impact" with the residual gap named. Move it to the shipped section, matching the house style of items 10/11, recording that the gap is now closed by gating evaluation, and that the trade-off is a correct token being refused while gated.

- [ ] **Step 3: Verify every claim against the code**, then commit.

---

### Task 4: Verification

- [ ] `cargo test --workspace` — 0 failures.
- [ ] `env PATH=/Users/rhurling/.cargo/bin:/usr/bin:/bin cargo test --workspace` — identical (no HandBrakeCLI dependency).
- [ ] `cargo fmt --all -- --check`; `cargo clippy -p convertbar-server --all-targets` (zero warnings for this crate).
- [ ] `npm test` — 206 passing, unchanged.
- [ ] **Live smoke test**: start the binary with a real token, then confirm with `curl` that (a) the first 3 wrong tokens are answered normally, (b) subsequent wrong tokens are refused immediately, (c) **the correct token is also refused while gated**, (d) after the spacing elapses the correct token succeeds and clears the gate, and (e) a fire-and-forget flood no longer buys evaluations — verify by checking that a correct token still fails during the flood. Report measured numbers.
