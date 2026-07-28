# HandBrake Locator Seam Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make "HandBrakeCLI is absent" expressible in tests, so the suite stops reading the developer's machine and a stripped-PATH sweep becomes unnecessary.

**Architecture:** A `HandbrakeLocator` trait injected on `Ctx` alongside the existing `events` and `disposer` collaborators. The four sites that resolve HandBrake converge on one helper, `handbrake::resolve_with_locator`, whose fallback branch is chosen by injection instead of by a `which`/`where` subprocess spawn. Test fixtures default to a locator that panics, so a test reaching resolution without declaring a world fails loudly instead of silently depending on the host.

**Tech Stack:** Rust 2021, Cargo workspace (`convertbar-core`, `convertbar-server`, `src-tauri`), `rusqlite`, inline `#[cfg(test)] mod tests`.

**Spec:** `docs/superpowers/specs/2026-07-28-handbrake-locator-seam-design.md`

## Global Constraints

- **Line numbers in this plan shift as you edit.** Locate every site by the quoted code pattern, not by line number. Re-grep before each task.
- **Test doubles must NOT be `#[cfg(test)]`-gated.** `convertbar-server`'s tests are inline `#[cfg(test)]` in *its own* crate, so `cfg(test)` on `convertbar-core` is false there. Follow the existing idiom: `events::TestSink` and `dispose::RecordingDisposer` are plain `pub`.
- **Never emit an event while holding `ctx.db`'s lock** (CLAUDE.md). This plan does not add emits, but two resolver call sites run *under* the db guard — do not add one there.
- **Do not convert the `&Connection` resolvers to `&Ctx`.** They run under the db guard; a `&Ctx` resolver that re-locks `ctx.db` self-deadlocks on a non-reentrant `std::sync::Mutex`.
- **Run tests with:** `cargo test --workspace`
- **Commits are signed:** use `git commit -S`. Do not `git push` — ask the user.
- The `handbrake_path` DB default is `""` (`db.rs`), and `DEFAULT_SUFFIX_TEMPLATE` is `".{resolution}-{codec}"` (`settings_ops.rs`). Both matter: an unconfigured path plus a templated suffix is exactly what reaches the fallback.

---

### Task 1: The `HandbrakeLocator` trait, `PathLocator`, and the three test doubles

**Files:**
- Modify: `crates/convertbar-core/src/handbrake.rs` (add near `detect_handbrake_path`)

**Interfaces:**
- Produces: `handbrake::HandbrakeLocator` (trait, method `locate(&self) -> Option<String>`), `handbrake::PathLocator`, `handbrake::PanickingLocator`, `handbrake::AbsentLocator`, `handbrake::StubLocator(pub String)`, and `handbrake::resolve_with_locator(configured: Option<&str>, locator: &dyn HandbrakeLocator) -> Option<String>`.

- [ ] **Step 1: Write the failing tests**

Add to the existing `#[cfg(test)] mod tests` in `crates/convertbar-core/src/handbrake.rs`:

```rust
#[test]
fn resolve_with_locator_prefers_an_existing_configured_path() {
    // A configured path that exists must win outright — the locator is the *fallback*, and
    // consulting it anyway would spawn a subprocess the user already told us to skip.
    let f = tempfile::NamedTempFile::new().unwrap();
    let configured = f.path().to_str().unwrap();
    let got = resolve_with_locator(Some(configured), &StubLocator("/never/used".into()));
    assert_eq!(got.as_deref(), Some(configured));
}

#[test]
fn resolve_with_locator_falls_back_when_the_configured_path_is_gone() {
    // A stale setting (HandBrake uninstalled/moved since it was saved) must not be trusted.
    let got = resolve_with_locator(
        Some("/nonexistent/HandBrakeCLI"),
        &StubLocator("/found/here".into()),
    );
    assert_eq!(got.as_deref(), Some("/found/here"));
}

#[test]
fn resolve_with_locator_falls_back_when_configured_is_empty() {
    // "" is the shipped DB default, so this is the unconfigured-user path, not an edge case.
    let got = resolve_with_locator(Some(""), &StubLocator("/found/here".into()));
    assert_eq!(got.as_deref(), Some("/found/here"));
}

#[test]
fn absent_locator_reports_handbrake_missing() {
    // The CI world, expressible for the first time.
    assert_eq!(resolve_with_locator(Some(""), &AbsentLocator), None);
}

#[test]
#[should_panic(expected = "Declare the world explicitly")]
fn panicking_locator_fails_loud_rather_than_reading_the_machine() {
    // The guard itself must be able to fail; a fixture default that silently succeeded
    // would reintroduce exactly the machine-coupling this seam removes.
    let _ = resolve_with_locator(Some(""), &PanickingLocator);
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p convertbar-core --lib handbrake::tests::resolve_with_locator`
Expected: FAIL to compile — `cannot find function resolve_with_locator`, `cannot find type StubLocator`.

- [ ] **Step 3: Add `tempfile` as a dev-dependency if absent**

Run: `grep -n "tempfile" crates/convertbar-core/Cargo.toml`
If there is no `tempfile` under `[dev-dependencies]`, add it. If it is already there, change nothing.

- [ ] **Step 4: Write the implementation**

In `crates/convertbar-core/src/handbrake.rs`, directly below `detect_handbrake_path`:

```rust
/// How the engine discovers HandBrakeCLI when no usable path is configured.
///
/// Injected on [`crate::ctx::Ctx`] rather than called directly so that tests can state which
/// world they are in. `detect_handbrake_path` shells out to `which`/`where`, i.e. it reads the
/// machine the test happens to run on — which let five tests pass locally and fail in CI.
pub trait HandbrakeLocator: Send + Sync {
    fn locate(&self) -> Option<String>;
}

/// Production: PATH detection via `which` / `where`.
pub struct PathLocator;

impl HandbrakeLocator for PathLocator {
    fn locate(&self) -> Option<String> {
        detect_handbrake_path()
    }
}

/// The test-harness default. Fails loud when a test reaches HandBrake resolution without
/// saying whether HandBrake is installed — the same tactic as `LockProbeSink` in `control.rs`.
/// Reaching this means the test's outcome would otherwise depend on the host.
pub struct PanickingLocator;

impl HandbrakeLocator for PanickingLocator {
    fn locate(&self) -> Option<String> {
        panic!(
            "HandBrake resolution was reached with the default test locator, so this test's \
             result would depend on whether HandBrakeCLI is installed on this machine. \
             Declare the world explicitly: `Arc::new(AbsentLocator)` for the CI world (no \
             HandBrake), or `Arc::new(StubLocator(path))` for the installed world."
        );
    }
}

/// The CI world: HandBrakeCLI is not installed.
pub struct AbsentLocator;

impl HandbrakeLocator for AbsentLocator {
    fn locate(&self) -> Option<String> {
        None
    }
}

/// The installed world, without requiring a real binary on the host.
pub struct StubLocator(pub String);

impl HandbrakeLocator for StubLocator {
    fn locate(&self) -> Option<String> {
        Some(self.0.clone())
    }
}

/// The configured path if it points at an existing file, otherwise the locator's answer.
///
/// Filesystem work only — no DB access — so callers can hold or release the db guard as their
/// own call site requires.
pub fn resolve_with_locator(
    configured: Option<&str>,
    locator: &dyn HandbrakeLocator,
) -> Option<String> {
    if let Some(path) = configured {
        if !path.is_empty() && std::path::Path::new(path).exists() {
            return Some(path.to_string());
        }
    }
    locator.locate()
}
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p convertbar-core --lib handbrake::tests`
Expected: PASS, including `panicking_locator_fails_loud_rather_than_reading_the_machine`.

- [ ] **Step 6: Commit**

```bash
git add crates/convertbar-core/src/handbrake.rs crates/convertbar-core/Cargo.toml
git commit -S -m "feat: add HandbrakeLocator seam and test doubles

detect_handbrake_path shells out to which/where, so any test reaching it
reads the host. The trait makes both worlds expressible; PanickingLocator
is the fixture default so a test that reaches resolution without declaring
a world fails instead of silently depending on the machine."
```

---

### Task 2: Wire the locator into `Ctx` across all 14 construction sites

Nothing calls `locate()` yet, so the suite must stay green through this task. That is the point of doing it separately: a compile-only change with no behavior change isolates the mechanical churn from the behavioral one.

**Files:**
- Modify: `crates/convertbar-core/src/ctx.rs`
- Modify (production, pass `PathLocator`): `crates/convertbar-server/src/main.rs`, `src-tauri/src/lib.rs`
- Modify (test fixtures, pass `PanickingLocator`): `crates/convertbar-core/src/{converter,settings_ops,queue_ops,control,watcher,handbrake}.rs`, `crates/convertbar-server/src/{routes/mod,startup}.rs`, `src-tauri/src/{commands/updater,updater}.rs`

**Interfaces:**
- Consumes: `handbrake::{HandbrakeLocator, PathLocator, PanickingLocator}` from Task 1.
- Produces: `Ctx.handbrake: Arc<dyn HandbrakeLocator>`; `Ctx::new(conn, events, disposer, handbrake)`.

- [ ] **Step 1: Add the field and parameter**

In `crates/convertbar-core/src/ctx.rs`, add the field after `disposer`:

```rust
    pub disposer: Arc<dyn crate::dispose::FileDisposer>,
    /// How HandBrakeCLI is discovered when no usable path is configured. Injected so tests can
    /// declare whether HandBrake is installed instead of inheriting the host's answer.
    pub handbrake: Arc<dyn crate::handbrake::HandbrakeLocator>,
```

and the matching parameter on `Ctx::new`:

```rust
    pub fn new(
        conn: rusqlite::Connection,
        events: Arc<dyn crate::events::EventSink>,
        disposer: Arc<dyn crate::dispose::FileDisposer>,
        handbrake: Arc<dyn crate::handbrake::HandbrakeLocator>,
    ) -> Arc<Self> {
        Arc::new(Self {
            db: Arc::new(Mutex::new(conn)),
            converter: Arc::new(crate::converter::ConverterState::new()),
            events,
            disposer,
            handbrake,
            preset_cache: Mutex::new(HashMap::new()),
            watcher: crate::watcher::WatcherState::new(),
        })
    }
```

- [ ] **Step 2: Run the build to enumerate every broken call site**

Run: `cargo build --workspace --all-targets --keep-going 2>&1 | grep -E "^\s+--> " | sort -u`

Expected: FAIL with `this function takes 4 arguments but 3 arguments were supplied`.

**Do not count error lines — count unique `file:line` sites.** `--keep-going` is required because cargo
is fail-fast by default and would otherwise report only the first compilation unit's errors. Even
then the error *count* will exceed the site count: `lib.rs:92` and `main.rs:40` each compile in two
units (lib/bin plus their test target), so they are reported twice. The **14 unique sites** are the
figure to reconcile — 2 production plus 12 fixtures, listed under Files above.

- [ ] **Step 3: Update the 2 production sites to `PathLocator`**

`crates/convertbar-server/src/main.rs`:

```rust
    let ctx = convertbar_core::ctx::Ctx::new(
        conn,
        Arc::new(ServerSink(events_tx.clone())),
        Arc::new(DeleteDisposer),
        Arc::new(convertbar_core::handbrake::PathLocator),
    );
```

`src-tauri/src/lib.rs`:

```rust
            let ctx = Ctx::new(
                conn,
                events,
                Arc::new(sink::TrashDisposer),
                Arc::new(convertbar_core::handbrake::PathLocator),
            );
```

- [ ] **Step 4: Update the 12 test fixtures to `PanickingLocator`**

Each remaining site gains a fourth argument. In `convertbar-core` the path is
`crate::handbrake::PanickingLocator`; from `convertbar-server` and `src-tauri` it is
`convertbar_core::handbrake::PanickingLocator`. Example — the `test_ctx` helper in
`crates/convertbar-core/src/queue_ops.rs`:

```rust
    fn test_ctx(conn: Connection) -> (Arc<Ctx>, Arc<TestSink>, Arc<RecordingDisposer>) {
        let sink = Arc::new(TestSink::default());
        let disposer = Arc::new(RecordingDisposer::default());
        let ctx = Ctx::new(
            conn,
            sink.clone(),
            disposer.clone(),
            Arc::new(crate::handbrake::PanickingLocator),
        );
        (ctx, sink, disposer)
    }
```

Apply the same fourth argument at each of the other 11 fixture sites.

- [ ] **Step 5: Add a locator-aware fixture to the server routes**

In `crates/convertbar-server/src/routes/mod.rs`, the `test_state*` helpers build the `Ctx`. Later
tasks need per-test control, so parameterize now. Keep the existing helper names working by
delegating:

```rust
    pub(crate) fn test_state_with_locator(
        locator: Arc<dyn convertbar_core::handbrake::HandbrakeLocator>,
    ) -> (ServerState, tokio::sync::watch::Sender<bool>) {
        // ... identical body to test_state_with_shutdown, but passing `locator` to Ctx::new
    }
```

and have `test_state_with_shutdown()` call
`test_state_with_locator(Arc::new(convertbar_core::handbrake::PanickingLocator))`.

- [ ] **Step 6: Verify the whole workspace builds and the suite is still green**

Run: `cargo test --workspace`
Expected: PASS — 389 passed, 0 failed, 4 ignored. Nothing calls `locate()` yet, so no test can
panic. **If any test fails here, the field wiring is wrong — do not proceed.**

- [ ] **Step 7: Commit**

```bash
git add -A
git commit -S -m "refactor: thread a HandbrakeLocator through Ctx

Compile-only change: production passes PathLocator (today's behavior
exactly), test fixtures pass PanickingLocator. Nothing consults the
locator yet, so the suite is unchanged."
```

---

### Task 3: Converge the four resolvers onto the locator

This is the behavioral change. The moment it lands, the four tests listed in Step 5 start
reaching `PanickingLocator`. They are fixed in this same task so the suite ends green.

**Files:**
- Modify: `crates/convertbar-core/src/handbrake.rs` (`resolve_handbrake_path` body)
- Modify: `crates/convertbar-core/src/queue_ops.rs` (delete `resolve_from_configured`; `get_handbrake_path`; `purge_bad_sources`; the `resolve_handbrake_for_test` helper)
- Modify: `crates/convertbar-core/src/converter.rs` (`get_handbrake_path` and its `process_queue` call site)
- Modify: `crates/convertbar-core/src/queue_ops.rs`, `crates/convertbar-server/src/routes/mod.rs` (the four tests)

**Interfaces:**
- Consumes: `handbrake::resolve_with_locator`, `Ctx.handbrake` from Tasks 1–2.
- Produces: `queue_ops::get_handbrake_path(conn: &Connection, locator: &dyn HandbrakeLocator) -> Result<String, String>`; `converter::get_handbrake_path(db: &Connection, locator: &dyn HandbrakeLocator) -> Option<String>`. Both remain private to their modules.

- [ ] **Step 1: Switch `handbrake::resolve_handbrake_path`'s fallback**

Replace the trailing existence-check and `Ok(detect_handbrake_path())` with the shared helper.
**This body change is load-bearing** — leaving it alone keeps the ambient read alive at the detect
route and defeats the whole change:

```rust
pub fn resolve_handbrake_path(ctx: &Ctx) -> Result<Option<String>, String> {
    let configured: Option<String> = {
        let conn = ctx.db.lock().map_err(|e| e.to_string())?;
        conn.query_row(
            "SELECT value FROM settings WHERE key = 'handbrake_path'",
            params![],
            |row| row.get(0),
        )
        .ok()
    };

    Ok(resolve_with_locator(configured.as_deref(), &*ctx.handbrake))
}
```

- [ ] **Step 2: Converge `queue_ops`**

Delete `resolve_from_configured` entirely (its doc comment's R3 note moves to `purge_bad_sources`,
which is the only caller that honored it). Rewrite the wrapper:

```rust
fn get_handbrake_path(
    conn: &rusqlite::Connection,
    locator: &dyn handbrake::HandbrakeLocator,
) -> Result<String, String> {
    handbrake::resolve_with_locator(read_configured_handbrake_path(conn).as_deref(), locator)
        .ok_or_else(|| "HandBrakeCLI not found".to_string())
}
```

In `add_files_inner`, the call site inside the db-guard block becomes:

```rust
            get_handbrake_path(&conn, &*ctx.handbrake).ok()
```

In `purge_bad_sources`, replace the `resolve_from_configured(...)` line — keeping its
`Result<String, String>` shape, which `purge_one_locked` expects:

```rust
    // R3: resolved ONCE for the whole batch, OUTSIDE the lock, and passed to every
    // `purge_one_locked` call below — this used to run per id, under the DB mutex, in both
    // purge phases, i.e. up to 2N blocking spawns under the lock for a batch of N ids.
    let handbrake_path = handbrake::resolve_with_locator(
        configured_handbrake_path.as_deref(),
        &*ctx.handbrake,
    )
    .ok_or_else(|| "HandBrakeCLI not found".to_string());
```

- [ ] **Step 3: Converge `converter`**

```rust
fn get_handbrake_path(
    db: &Connection,
    locator: &dyn crate::handbrake::HandbrakeLocator,
) -> Option<String> {
    let configured: Option<String> = db
        .query_row(
            "SELECT value FROM settings WHERE key = 'handbrake_path'",
            [],
            |row| row.get(0),
        )
        .ok();

    crate::handbrake::resolve_with_locator(configured.as_deref(), locator)
}
```

and in `process_queue`, inside the db-guard block:

```rust
            handbrake_path_opt = get_handbrake_path(&db, &*ctx.handbrake);
```

- [ ] **Step 4: Fix the `resolve_handbrake_for_test` helper**

Run: `grep -n "resolve_handbrake_for_test" crates/convertbar-core/src/queue_ops.rs`

Change it to take the `Ctx` (so it can reach the locator):

```rust
    fn resolve_handbrake_for_test(ctx: &Arc<Ctx>) -> Result<String, String> {
        let conn = ctx.db.lock().unwrap();
        get_handbrake_path(&conn, &*ctx.handbrake)
    }
```

**This is five-test surgery, not a signature tweak.** All five call sites currently build a bare
`Arc<Mutex<Connection>>` and never construct a `Ctx` at all. Each needs restructuring: build the
`Ctx` first, apply the test's settings through it, then pass `&ctx.db` to `purge_one_locked`. All
five already pin `handbrake_path` to an existing path, so they will not reach the guard and need no
world declaration — budget the time for the restructuring, not for debugging panics.

- [ ] **Step 5: Run the suite to see exactly which tests now reach the guard**

Run: `cargo test --workspace 2>&1 | grep -E "^(test .* FAILED|failures:|panicked)" | head -20`

Expected: FAIL — **exactly three visible failures.** Five tests reach `PanickingLocator`, but two are
`#[ignore]`d and do not run under `cargo test --workspace`; they are fixed in Step 7 and would
otherwise only surface later, during a local `--ignored` run, as a poisoned-mutex panic.

| Test | File | Runs here? |
|---|---|---|
| `purge_bad_sources_destroys_through_the_ctx_disposer` | `convertbar-core/src/queue_ops.rs` | yes |
| `purge_bad_sources_with_no_ids_returns_an_empty_array` | `convertbar-server/src/routes/mod.rs` | yes |
| `detect_handbrake_smoke_returns_200_with_valid_json` | `convertbar-server/src/routes/mod.rs` | yes |
| `add_files_inner_skips_at_target_source_end_to_end` | `convertbar-core/src/queue_ops.rs` | **no — `#[ignore]`d** |
| `process_queue_drives_a_real_encode_from_queued_to_done` | `convertbar-core/src/converter.rs` | **no — `#[ignore]`d** |

Do not go hunting for a fourth visible failure — there isn't one. If a genuinely *new* test fails,
that is a real discovery: it was machine-coupled too. Give it `AbsentLocator` and say so in the
commit message.

- [ ] **Step 6: Declare a world in each newly-failing test**

For `purge_bad_sources_destroys_through_the_ctx_disposer`: the assertion is about the disposer, not
about HandBrake, so build its `Ctx` with `Arc::new(crate::handbrake::AbsentLocator)` instead of the
fixture default.

For `purge_bad_sources_with_no_ids_returns_an_empty_array`: use the new
`test_state_with_locator(Arc::new(convertbar_core::handbrake::AbsentLocator))`.

For `detect_handbrake_smoke_returns_200_with_valid_json`: this test currently tolerates either
answer (its own comment concedes "CI has no HandBrakeCLI, but the test host might"). Replace that
vagueness with two precise tests:

**The route returns `Json(Option<String>)`** — a bare JSON `null` or a bare JSON string, with **no
`path` field** (`crates/convertbar-server/src/routes/handbrake.rs`, `detect_handbrake`). Assert on
the bare value; an assertion like `body.get("path")` would be vacuously true and pass in both worlds:

```rust
    #[tokio::test]
    async fn detect_handbrake_reports_absent_when_handbrake_is_not_installed() {
        // Pins the CI world: 200 with a null body, not a 500. This test previously accepted
        // either answer because it reported whatever the host happened to have installed.
        let (state, _tx) =
            test_state_with_locator(Arc::new(convertbar_core::handbrake::AbsentLocator));
        let app = router(state);
        let (status, body) = request_json(app, "GET", "/api/handbrake/detect", None).await;
        assert_eq!(status, 200);
        assert!(body.is_null(), "absent HandBrake must report null, got {body:?}");
    }

    #[tokio::test]
    async fn detect_handbrake_reports_the_located_path_when_handbrake_is_installed() {
        let (state, _tx) = test_state_with_locator(Arc::new(
            convertbar_core::handbrake::StubLocator("/opt/fake/HandBrakeCLI".into()),
        ));
        let app = router(state);
        let (status, body) = request_json(app, "GET", "/api/handbrake/detect", None).await;
        assert_eq!(status, 200);
        assert_eq!(body.as_str(), Some("/opt/fake/HandBrakeCLI"));
    }
```

- [ ] **Step 7: Give BOTH `#[ignore]`d e2e tests the real world**

Two `#[ignore]`d tests reach the fallback *through the seam* rather than by calling
`detect_handbrake_path()` directly, so neither can be left alone. Both genuinely want the host's
binary, which is the one place reading the real machine is correct. Build each one's `Ctx` with
`Arc::new(crate::handbrake::PathLocator)`:

- `add_files_inner_skips_at_target_source_end_to_end` — `crates/convertbar-core/src/queue_ops.rs`
- `process_queue_drives_a_real_encode_from_queued_to_done` — `crates/convertbar-core/src/converter.rs`
  (builds its ctx via `test_ctx(test_conn())` and never pins `handbrake_path`)

**Neither is caught by any automated check in this plan.** Ignored tests do not run in Task 6's
sweeps, and Task 6's grep only finds *direct* `detect`/`which`/`where` calls. Missing one leaves the
documented local command — `cargo test -- --ignored process_queue_drives_a_real_encode` — panicking
inside the db-guard block and poisoning the mutex.

- [ ] **Step 7b: Confirm both ignored tests still run**

Run: `cargo test -p convertbar-core --lib -- --ignored process_queue_drives_a_real_encode`
Expected: PASS on a machine with HandBrakeCLI and ffmpeg installed. If HandBrake is absent locally,
the test is expected to fail on its own terms — but it must **not** panic with the
`PanickingLocator` message, which would mean Step 7 was missed.

- [ ] **Step 8: Run the suite to verify it is green again**

Run: `cargo test --workspace`
Expected: PASS. Test count rises by one (the smoke test split into two).

- [ ] **Step 9: Commit**

```bash
git add -A
git commit -S -m "refactor: resolve HandBrake through the injected locator

All four resolution sites now consult ctx.handbrake instead of spawning
which/where directly. Four tests were reaching the real PATH and are now
explicit: three declare AbsentLocator, and the ignored local-only e2e
declares PathLocator because it genuinely wants the host's binary.

The detect-route smoke test tolerated either answer depending on the host;
it is now two tests that each pin one world."
```

---

### Task 4: Remove the three pinned-suffix workarounds and add intake present/absent pairs

**Files:**
- Modify: `crates/convertbar-core/src/queue_ops.rs` (the `add_files_emits_finished_before_queue_updated` pin; new pairs)
- Modify: `crates/convertbar-server/src/routes/mod.rs` (the fixture pin)
- Modify: `crates/convertbar-core/src/watcher.rs` (the third pin)

**Interfaces:**
- Consumes: `handbrake::{AbsentLocator, StubLocator}`, `Ctx.preset_cache`.

- [ ] **Step 1: Write the failing absent-world intake test**

In `crates/convertbar-core/src/queue_ops.rs`'s test module:

```rust
#[test]
fn add_files_inner_reports_handbrake_missing_when_the_suffix_needs_a_probe() {
    // The default suffix template contains {...} placeholders, so intake must resolve HandBrake
    // to expand them. With HandBrake absent the caller gets a named error — not a panic, and
    // not a silent success that would write files with an unexpanded literal suffix.
    let (ctx, _sink, _d) = test_ctx_with_locator(test_conn(), Arc::new(AbsentLocator));
    let err = add_files_inner(&ctx, &["/tmp/whatever.mkv".to_string()], None)
        .expect_err("intake must fail when the suffix template needs HandBrake and it is absent");
    assert!(err.contains("HandBrakeCLI not found"), "got: {err}");
}
```

- [ ] **Step 2: Add the `test_ctx_with_locator` helper it needs**

Alongside the existing `test_ctx` in `crates/convertbar-core/src/queue_ops.rs`:

```rust
    fn test_ctx_with_locator(
        conn: Connection,
        locator: Arc<dyn crate::handbrake::HandbrakeLocator>,
    ) -> (Arc<Ctx>, Arc<TestSink>, Arc<RecordingDisposer>) {
        let sink = Arc::new(TestSink::default());
        let disposer = Arc::new(RecordingDisposer::default());
        let ctx = Ctx::new(conn, sink.clone(), disposer.clone(), locator);
        (ctx, sink, disposer)
    }
```

and make the existing `test_ctx` delegate to it with `Arc::new(crate::handbrake::PanickingLocator)`
so the strict default is stated in exactly one place.

- [ ] **Step 3: Run to verify the new test passes and the ordering test now fails**

Run: `cargo test -p convertbar-core --lib queue_ops`
Expected: the new absent test PASSES immediately (Task 3 already wired the behavior). Leave
`add_files_emits_finished_before_queue_updated` untouched for now — it still passes on its pin.

- [ ] **Step 4: Remove the pin from the event-ordering test**

Delete the pinned-suffix block from `add_files_emits_finished_before_queue_updated` (the comment
beginning "Pin a literal suffix so add_files_inner never needs to resolve HandBrakeCLI" and the
`set_preset_suffix` call beneath it). Build its `Ctx` via `test_ctx_with_locator` with a
`StubLocator`, **and pre-populate the preset cache** — without that, `cached_preset_metadata` shells
out to the stub path, returns `Err`, and the `queue-updated` emit never happens:

```rust
    let (ctx, sink, _d) = test_ctx_with_locator(
        test_conn(),
        Arc::new(StubLocator("/opt/fake/HandBrakeCLI".into())),
    );
    // Pre-seed the metadata cache so suffix expansion short-circuits before the shell-out to
    // the stub path. This is what makes the "HandBrake installed" world expressible without a
    // real binary — a bare StubLocator alone would make add_files_inner return Err.
    let preset: String = ctx
        .db
        .lock()
        .unwrap()
        .query_row("SELECT value FROM settings WHERE key = 'preset'", [], |r| r.get(0))
        .unwrap();
    ctx.preset_cache.lock().unwrap().insert(
        preset,
        crate::handbrake::PresetMetadata {
            codec: "h265".into(),
            resolution: "1080p".into(),
            quality: "22".into(),
            preset: "Fast 1080p30".into(),
            device: "VideoToolbox".into(),
        },
    );
```

- [ ] **Step 5: Run to verify the ordering test still passes without its pin**

Run: `cargo test -p convertbar-core --lib add_files_emits_finished_before_queue_updated`
Expected: PASS — now exercising the real default suffix template, which is what the test was always
meant to cover.

- [ ] **Step 6: Remove the routes fixture pin**

In `crates/convertbar-server/src/routes/mod.rs`, delete the pinned-suffix block from the shared
fixture (the comment beginning "Pin a literal suffix for the default preset" and the
`set_preset_suffix` call). Any route test that then reaches the guard must declare a world via
`test_state_with_locator`.

Run: `cargo test -p convertbar-server`
Fix each failure by giving that test `AbsentLocator` (if it does not care about HandBrake) or
`StubLocator` plus a seeded `preset_cache` (if it adds files and expects success).

- [ ] **Step 6b: Add the server add-files route present/absent pair**

Required by the spec's Testing section ("The server add-files route, both worlds, asserting the HTTP
status rather than a 500"). The route is `POST /api/queue/files` with body `{"paths": [...]}`
(`routes/queue.rs`, `add_files`), and it maps a core `Err` to `core_err(e)` — so the absent world
must produce a *deliberate* status, not an accidental 500 from a panic:

```rust
    #[tokio::test]
    async fn add_files_route_reports_the_error_when_handbrake_is_absent() {
        // The default suffix template needs HandBrake to expand. Absent, the route must return
        // the core error deliberately — not a 500 from a panicking locator, and not a silent 200.
        let (state, _tx) =
            test_state_with_locator(Arc::new(convertbar_core::handbrake::AbsentLocator));
        let app = router(state);
        let (status, body) = request_json(
            app,
            "POST",
            "/api/queue/files",
            Some(json!({"paths": ["/tmp/clip.mp4"]})),
        )
        .await;
        assert_ne!(status, 200, "absent HandBrake must not report success, got {body:?}");
        assert!(
            format!("{body}").contains("HandBrakeCLI not found"),
            "the response must name the missing binary so the UI can tell the user, got {body:?}"
        );
    }
```

For the present world, add the mirror test using `StubLocator` **plus a seeded `preset_cache`** on
`state.ctx` (same reason as Step 4 — otherwise `cached_preset_metadata` shells out to the stub path
and returns `Err`), asserting `status == 200`.

Reconcile `assert_ne!(status, 200)` against `core_err`'s actual status code once you have read it;
prefer asserting the exact code over `assert_ne!` if it is stable.

- [ ] **Step 7: Remove the watcher pin**

In `crates/convertbar-core/src/watcher.rs`, delete the `UPDATE preset_suffixes SET suffix = '.conv'`
workaround and its comment.

**Use `StubLocator` plus a seeded `preset_cache` — `AbsentLocator` will not work here.** That test
asserts a `queue-updated` event was emitted, but on an `add_files_inner` error `enqueue_and_start`
returns before emitting anything, so the absent world guarantees a red test. Seed the cache exactly
as in Step 4.

Run: `cargo test -p convertbar-core --lib watcher`
Expected: PASS.

- [ ] **Step 8: Run the full suite**

Run: `cargo test --workspace`
Expected: PASS.

- [ ] **Step 9: Commit**

```bash
git add -A
git commit -S -m "test: replace pinned-suffix workarounds with explicit locators

Three tests pinned a literal suffix purely to dodge HandBrake resolution
(queue_ops, routes fixture, watcher). Each now declares its world instead,
so the event-ordering test exercises the real default suffix template it
was always meant to cover.

The installed world needs StubLocator plus a seeded preset_cache:
cached_preset_metadata shells out to the resolved path on a cache miss, so
a bare stub path returns Err and swallows the queue-updated emit."
```

---

### Task 5: Cover the mid-encode absent arm

The one place the queue actually consumes a missing-HandBrake answer, untested until now.

**Files:**
- Modify: `crates/convertbar-core/src/converter.rs` (test module)

- [ ] **Step 1: Write the failing test**

**The source file must really exist on disk.** `process_queue` stats the source at its
vanished-source gate *before* it reaches the missing-HandBrake arm, so a fixture naming a path that
was never created records `"Source file no longer exists"` and never touches the code under test —
the `status` and `failure_class` assertions would still pass, making this a green test of the wrong
branch. `real_source`'s own doc comment in `converter.rs` warns about exactly this. Use the module's
existing helpers (`real_source`, `queue_job`, `job_row`, `class_of`) — note `queue_job` takes
`&ctx.db`, so the `Ctx` must be built first:

```rust
#[test]
fn a_queued_job_fails_as_environment_when_handbrake_is_missing() {
    // process_queue resolves HandBrake per job. Absent, the job must be recorded as an
    // Environment failure and the queue must move on — not hang, not retry forever, and not be
    // mistaken for a bad source file. Before the locator seam this arm was unreachable in tests,
    // because "HandBrake is not installed" could not be expressed.
    let (ctx, _sink, _d) =
        test_ctx_with_locator(test_conn(), Arc::new(crate::handbrake::AbsentLocator));

    let dir = tempfile::tempdir().unwrap();
    // A real file: the vanished-source gate runs first and would otherwise claim this job.
    let src = real_source(dir.path(), "in.mp4");
    queue_job(&ctx.db, "j1", src.to_str().unwrap(), "/nowhere/out.mp4", 1000);

    process_queue(&ctx);

    let (status, msg) = job_row(&ctx.db, "j1");
    assert_eq!(status, "error");
    assert!(
        msg.clone().unwrap_or_default().contains("HandBrakeCLI not found"),
        "the failure must name the missing binary, not blame the source file — got {msg:?}"
    );
    assert_eq!(class_of(&ctx.db, "j1").as_deref(), Some("environment"));
}
```

- [ ] **Step 2: Add the `test_ctx_with_locator` helper to `converter.rs`**

Mirror the helper added in Task 4 Step 2, and make `converter.rs`'s existing `test_ctx` delegate to
it with `PanickingLocator`.

- [ ] **Step 3: Run the test**

Run: `cargo test -p convertbar-core --lib a_queued_job_fails_as_environment_when_handbrake_is_missing`
Expected: PASS.

**If it fails on the message assertion, do not weaken that assertion** — it means the job stopped at
a different gate and the mid-encode arm was never reached, which is the whole point of the test.
Fix the fixture instead. (`FailureClass::Environment` serializes as `"environment"`, and the job
error column is `error_message` — both already handled by `class_of` and `job_row`.)

- [ ] **Step 4: Commit**

```bash
git add crates/convertbar-core/src/converter.rs
git commit -S -m "test: cover the mid-encode missing-HandBrake arm

process_queue's absent branch records an Environment failure and continues.
It was unreachable in tests before the locator seam, because absence could
not be expressed."
```

---

### Task 6: Verify the ambient dependency is gone

The claim this whole change exists to make is checkable. Check it — do not assert it.

**Files:** none modified (except a possible fix if verification fails).

- [ ] **Step 1: Confirm HandBrakeCLI is present on this machine**

Run: `which HandBrakeCLI`
Expected: a path (e.g. `/opt/homebrew/bin/HandBrakeCLI`). If it is absent, this verification proves
nothing — install it or run on a machine that has it.

- [ ] **Step 2: Full suite with HandBrake present**

Run: `cargo test --workspace 2>&1 | grep -E "^test result"`
Record every line.

- [ ] **Step 3: Full suite with HandBrake stripped from PATH**

Strip only the directories containing the binary, so the Rust toolchain still resolves:

```bash
HB_DIR=$(dirname "$(which HandBrakeCLI)")
CLEAN_PATH=$(echo "$PATH" | tr ':' '\n' | grep -vx "$HB_DIR" | paste -sd: -)
env PATH="$CLEAN_PATH" sh -c 'which HandBrakeCLI && echo "STILL PRESENT - strip failed" || echo "confirmed absent"'
env PATH="$CLEAN_PATH" cargo test --workspace 2>&1 | grep -E "^test result"
```

Expected: `confirmed absent`, then test-result lines **identical** to Step 2.

- [ ] **Step 4: If the two runs differ, stop**

A difference means an ambient read survives. Find it before continuing:
`grep -rn "detect_handbrake_path\|Command::new(\"which\")\|Command::new(\"where\")" crates src-tauri`
Every non-`#[ignore]`d hit must route through a locator.

- [ ] **Step 5: Mutation-check both claims — separately**

A guard that cannot fail is not a guard. There are **two** distinct claims here, and one mutation
cannot test both. Mutating the `test_ctx` *default* alone proves nothing: after this plan, every
queue_ops test either declares its world explicitly or pins `handbrake_path` to an existing file, so
no test consults the default at resolution time and the suite stays green whatever the default is.

**Commit all real work before this step** — each revert below discards uncommitted changes in that
file.

**Mutation A — does the absent-world test actually depend on absence?**
In the new `add_files_inner_reports_handbrake_missing_when_the_suffix_needs_a_probe`, change its
`Arc::new(AbsentLocator)` to `Arc::new(PathLocator)`.

Run: `cargo test -p convertbar-core --lib add_files_inner_reports_handbrake_missing`
Expected on a machine **with** HandBrake: FAIL. If it passes, the test is not testing absence.
Revert: `git checkout crates/convertbar-core/src/queue_ops.rs`

**Mutation B — does `PanickingLocator` actually fire?**
Pick one test that currently declares a world — `purge_bad_sources_destroys_through_the_ctx_disposer`
is the cheapest — and remove its declaration so it falls back to the fixture default.

Run: `cargo test -p convertbar-core --lib purge_bad_sources_destroys_through_the_ctx_disposer`
Expected: FAIL with the `PanickingLocator` message ("Declare the world explicitly"). If it passes,
the strict default is not wired into that fixture and the guard is decorative.
Revert: `git checkout crates/convertbar-core/src/queue_ops.rs`

- [ ] **Step 6: Confirm the frontend is untouched**

Run: `git diff --stat main -- src/ && npm test`
Expected: no frontend files changed; frontend suite passes (206 tests).

- [ ] **Step 7: Record the verification result**

Append a short "Verification" note to the spec stating both suite runs matched, with the counts.
Then commit:

```bash
git add docs/superpowers/specs/2026-07-28-handbrake-locator-seam-design.md
git commit -S -m "docs: record the dual-PATH verification result"
```

---

## Self-Review

**Spec coverage:**

| Spec requirement | Task |
|---|---|
| `HandbrakeLocator` trait + `PathLocator` | 1 |
| Three test doubles, not `cfg(test)`-gated | 1 |
| `resolve_with_locator` replaces `queue_ops::resolve_from_configured` | 1 (add), 3 (delete old) |
| `Ctx.handbrake` field, 14 `Ctx::new` sites | 2 |
| `resolve_handbrake_path` body switches to the locator | 3 Step 1 |
| `&Connection` wrappers keep their shape, gain a locator param | 3 Steps 2–3 |
| 4th site: `purge_bad_sources` | 3 Step 2 |
| Four tests must declare a world | 3 Steps 5–7 |
| Three pinned-suffix workarounds removed | 4 Steps 4, 6, 7 |
| Present world = `StubLocator` + seeded `preset_cache` | 4 Step 4 |
| **Server add-files route, both worlds** | **4 Step 6b** |
| Mid-encode absent arm test | 5 |
| Dual-PATH verification | 6 Steps 2–4 |
| Mutation check | 6 Step 5 |

No spec requirement is unassigned. (The server add-files route pair was missing from the first draft
of this plan while the table still claimed full coverage — an adversarial review caught it. The
lesson generalizes: a self-review that reports its own completeness is worth less than one that
someone else checks.)

**Type consistency:** `locate()` returns `Option<String>` everywhere. `resolve_with_locator` returns
`Option<String>`; `queue_ops::get_handbrake_path` adapts to `Result<String, String>` via
`.ok_or_else`, `converter::get_handbrake_path` returns `Option<String>` directly, and
`handbrake::resolve_handbrake_path` wraps in `Ok(...)` for `Result<Option<String>, String>`. All
three current return types are preserved, so no caller outside these files changes.

**Known soft spots, called out rather than hidden:**

- Task 4 Step 6 cannot enumerate in advance which route tests reach the guard once the fixture pin is
  removed — that set depends on the pin's removal. The step says to run the suite and fix what
  breaks, with the decision rule for each case (`AbsentLocator` vs `StubLocator` + seeded cache).
- Task 3 Step 7's two ignored-test fixes are **not covered by any automated check in this plan**.
  Ignored tests do not run in Task 6's sweeps, and its grep only finds direct `detect`/`which`/`where`
  calls. Step 7b is a manual confirmation; if it is skipped, the breakage surfaces later as a
  poisoned mutex during a local `--ignored` run.
- Task 4 Step 6b's `assert_ne!(status, 200)` is deliberately loose because `core_err`'s status code
  has not been read. The step says to tighten it to the exact code once known.

**Verified against the codebase while correcting this plan** (no longer soft): `PresetMetadata`'s
five `String` fields, `FailureClass::Environment` → `"environment"`, the job error column
`error_message`, `tempfile` already a dev-dependency, `purge_one_locked` taking
`&Result<String, String>`, and `resolve_with_locator` being semantically identical to all three
originals (same empty-check, same `exists()`, same error text, all three return shapes preserved).
