# Empty Intake and the HandBrake-Missing Error Contract — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make an intake with zero paths a no-op that never reaches HandBrake resolution, then replace eight hand-duplicated `"HandBrakeCLI not found"` literals with one named constant and one shared resolver.

**Architecture:** Two independent, sequential PRs against a Cargo workspace. PR 1 (Tasks 1-3) adds a single early return in `queue_ops::add_files_inner` — a behavior change to intake semantics. PR 2 (Tasks 4-8) is a pure refactor: `convertbar-core::handbrake` gains `HANDBRAKE_NOT_FOUND` and `require_handbrake_path`; five call sites collapse into the function, three reference the constant, and three test assertions stop hardcoding the words.

**Tech Stack:** Rust (Cargo workspace: `crates/convertbar-core`, `crates/convertbar-server`, `src-tauri`), `rusqlite`, `axum` + `tokio` (server head), Tauri 2 (desktop head). Tests are `#[test]` / `#[tokio::test]` inside `mod tests` in the same file as the code.

**Source spec:** `docs/superpowers/specs/2026-07-29-handbrake-error-contract-design.md`

## Global Constraints

- Every task ends green on: `cargo test --workspace`, `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets`.
- **Never hold `ctx.db`'s lock across an event emit.** `std::sync::Mutex` is not reentrant and the desktop tray listener re-locks synchronously on the same thread. Two shipped deadlocks came from violating this.
- **`ctx.db` is not reentrant, full stop.** `require_handbrake_path` locks it. It must never be called while a `ctx.db` guard is held.
- **Test fixtures default to `PanickingLocator`.** A test that reaches HandBrake resolution without declaring its world must fail loud. Use `AbsentLocator` for the CI world, `StubLocator` for the installed world, never `PathLocator` outside `#[ignore]`d tests.
- The exact message string is `HandBrakeCLI not found` — unchanged by this work, only relocated to one definition.
- `main` is protected: signed commits, no merge commits, PR required. Claude cannot `git push`; ask the user to run `! git push -u origin <branch>`.
- Required CI checks: `frontend` and `rust (ubuntu-22.04)`.
- No TypeScript changes in either PR. `src/pages/QueuePage.tsx:81` and `src/pages/SettingsPage.tsx:464` hold their own copies of the words, driven by `hbStatus.found` rather than by parsing an error — they stay exactly as they are.

---

# PR 1 — An empty intake is a no-op

Branch: `fix/empty-intake-noop`

### Task 1: Empty intake returns success without resolving HandBrake

**Files:**
- Modify: `crates/convertbar-core/src/types.rs:111-116`
- Modify: `crates/convertbar-core/src/queue_ops.rs:775-781`
- Test: `crates/convertbar-core/src/queue_ops.rs` (the `mod tests` block, near `add_files_inner_with_a_literal_suffix_never_resolves_handbrake` at ~line 1653)

**Interfaces:**
- Consumes: nothing from earlier tasks.
- Produces: `AddResult: Default`. `add_files_inner(&Ctx, &[String], Option<&dyn Fn(u32,u32)>) -> Result<AddResult, String>` keeps its exact signature; only its behavior on an empty slice changes.

- [ ] **Step 1: Write the failing test**

Add to the `mod tests` block in `crates/convertbar-core/src/queue_ops.rs`, immediately after `add_files_inner_with_a_literal_suffix_never_resolves_handbrake`:

```rust
    #[test]
    fn add_files_inner_with_no_paths_never_reaches_handbrake_resolution() {
        // Built with the plain `test_ctx` (the `PanickingLocator` default) on purpose: it
        // asserts the negative directly. If the empty-intake guard regresses, resolution is
        // reached and the fixture panics — rather than this quietly passing on any machine that
        // happens to have HandBrakeCLI installed, which is exactly how the bug survived until
        // the locator seam made the absent world expressible.
        let (ctx, _sink, _disposer) = test_ctx(test_conn());
        let result = add_files_inner(&ctx, &[], None).expect("an empty intake cannot fail");
        assert!(
            result.added.is_empty(),
            "nothing was offered, so nothing can be added"
        );
        assert!(
            result.skipped.is_empty(),
            "nothing was offered, so nothing can be skipped"
        );
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p convertbar-core add_files_inner_with_no_paths_never_reaches_handbrake_resolution`

Expected: FAIL. The panic message is the locator's — `"HandBrake resolution was reached with the default test locator..."`. Note that the panic may surface instead as a `PoisonError` on `ctx.db` if it unwinds while the guard is held; either failure mode confirms the test is red for the right reason.

- [ ] **Step 3: Add `Default` to `AddResult`**

In `crates/convertbar-core/src/types.rs`, change the derive on `AddResult` (line 112). Before:

```rust
/// Result of an add operation: the jobs actually queued, plus per-reason counts of paths skipped.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AddResult {
    pub added: Vec<JobInfo>,
    pub skipped: Vec<SkipCount>,
}
```

After:

```rust
/// Result of an add operation: the jobs actually queued, plus per-reason counts of paths skipped.
/// `Default` is the empty result — `add_files_inner` returns it for an intake with no paths.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AddResult {
    pub added: Vec<JobInfo>,
    pub skipped: Vec<SkipCount>,
}
```

- [ ] **Step 4: Add the early return**

In `crates/convertbar-core/src/queue_ops.rs`, insert at the very top of `add_files_inner`'s body, before the `// First, read preset and suffix template from DB` comment. Before:

```rust
pub fn add_files_inner(
    ctx: &Ctx,
    paths: &[String],
    progress: Option<&dyn Fn(u32, u32)>,
) -> Result<AddResult, String> {
    // First, read preset and suffix template from DB
    let (preset, suffix_template, hb_path, skip_already_converted, skip_by_source_media) = {
```

After:

```rust
pub fn add_files_inner(
    ctx: &Ctx,
    paths: &[String],
    progress: Option<&dyn Fn(u32, u32)>,
) -> Result<AddResult, String> {
    // Nothing to add decides nothing: there is no output name to build, so there is no reason to
    // reach HandBrake. Without this, intake resolved the suffix template first and an empty
    // intake failed outright when HandBrakeCLI was absent — including "add a folder that turned
    // out to hold no videos". `watcher::enqueue_and_start` already guards emptiness at its own
    // call site; `add_files`, `confirm_folder_add`, and the server route did not.
    if paths.is_empty() {
        return Ok(AddResult::default());
    }

    // First, read preset and suffix template from DB
    let (preset, suffix_template, hb_path, skip_already_converted, skip_by_source_media) = {
```

- [ ] **Step 5: Run the test to verify it passes**

Run: `cargo test -p convertbar-core add_files_inner_with_no_paths_never_reaches_handbrake_resolution`

Expected: PASS.

- [ ] **Step 6: Retire the installed-world scaffolding from `add_files_emits_finished_before_queue_updated`**

This existing test at `crates/convertbar-core/src/queue_ops.rs:1683` calls `add_files(&ctx, &[])`
— an empty intake. It declares an installed world and seeds the preset cache *only* because of
the bug this task fixes. It stays green either way, but its comment now documents behavior that
no longer exists, and the panicking default turns it into a second regression guard. Before:

```rust
    fn add_files_emits_finished_before_queue_updated() {
        // The installed world, exercising the real default (templated) suffix — the pinned
        // literal suffix this test used to carry meant it never expanded a template at all.
        let (ctx, sink, _d) = test_ctx_with_locator(
            test_conn(),
            Arc::new(StubLocator("/opt/fake/HandBrakeCLI".into())),
        );
        // Without the seed the stub path would be shelled out to and intake would return Err,
        // swallowing the queue-updated emit this test asserts on.
        crate::handbrake::seed_preset_cache(&ctx);

        // an empty add still brackets: add-started → add-finished → queue-updated
        let _ = add_files(&ctx, &[]);
```

After:

```rust
    fn add_files_emits_finished_before_queue_updated() {
        // The plain `test_ctx` (PanickingLocator) default is load-bearing here: an empty add
        // returns before it reaches HandBrake resolution, so this asserts the event bracketing
        // AND that the early return is intact. It previously needed a StubLocator plus a seeded
        // preset cache purely because intake resolved the suffix template before looking at
        // `paths` — scaffolding for a bug, not for this test's subject.
        let (ctx, sink, _d) = test_ctx(test_conn());

        // an empty add still brackets: add-started → add-finished → queue-updated
        let _ = add_files(&ctx, &[]);
```

Check afterwards whether `StubLocator` and `seed_preset_cache` are still referenced elsewhere in
this test module (`grep -n "StubLocator\|seed_preset_cache" crates/convertbar-core/src/queue_ops.rs`).
If either import is now unused, remove the import — clippy will fail otherwise. If they are still
used, change nothing.

- [ ] **Step 7: Run the whole core suite**

Run: `cargo test -p convertbar-core`

Expected: all pass. If any existing test fails, stop and read it before changing anything — a test that expected an empty intake to error is a real signal, not noise, and must be reported rather than silently updated.

- [ ] **Step 8: Mutation check — prove the tests are load-bearing**

Delete the four-line `if paths.is_empty() { return Ok(AddResult::default()); }` block, run
`cargo test -p convertbar-core`, and confirm BOTH
`add_files_inner_with_no_paths_never_reaches_handbrake_resolution` and
`add_files_emits_finished_before_queue_updated` go RED. Then restore the block and confirm both
go GREEN again. Record all four outcomes in the task report. A test that stays green through
this deletion is not a test.

- [ ] **Step 9: Commit**

```bash
git add crates/convertbar-core/src/queue_ops.rs crates/convertbar-core/src/types.rs
git commit -m "fix: an empty intake is a no-op instead of an environment error"
```

If the commit fails with `1Password: failed to fill whole buffer`, the signing key is locked.
Do not work around it — ask the user to run `! op signin`, then retry the commit once.

---

### Task 2: The server's two empty-intake route tests assert the negative

**Files:**
- Modify: `crates/convertbar-server/src/routes/mod.rs:258-270`
- Modify: `crates/convertbar-server/src/routes/mod.rs:471-484`

**Interfaces:**
- Consumes: `add_files_inner`'s new empty-intake behavior from Task 1.
- Produces: nothing later tasks depend on.

- [ ] **Step 1: Rewrite the test**

The existing test declares an installed world and carries a comment that documents the bug as
though it were the design. Both change. Before:

```rust
    #[tokio::test]
    async fn add_files_with_empty_paths_returns_empty_added_and_skipped() {
        // Even an empty add resolves the suffix template first, so the world must be declared.
        let (status, json) = request_json(
            api_router(test_state_installed()),
            "POST",
            "/api/queue/files",
            Some(json!({"paths": []})),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json, json!({"added": [], "skipped": []}));
    }
```

After:

```rust
    #[tokio::test]
    async fn add_files_with_empty_paths_never_reaches_handbrake_resolution() {
        // `test_state()`'s PanickingLocator is the assertion: an empty add must return before it
        // reaches HandBrake resolution. Declaring an installed world here would hide a
        // regression — the route would resolve, succeed, and return this same body either way.
        // A panic inside `spawn_blocking` would surface as a 500 with `{"error": "task
        // panicked: ..."}`, so the status assertion below catches it.
        let (status, json) = request_json(
            api_router(test_state()),
            "POST",
            "/api/queue/files",
            Some(json!({"paths": []})),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json, json!({"added": [], "skipped": []}));
    }
```

- [ ] **Step 2: Rewrite the empty-folder route test**

This is the exact user-facing scenario PR 1 fixes — "add a folder that turned out to hold no
videos" — and it currently needs an installed HandBrake to pass. Before:

```rust
    #[tokio::test]
    async fn confirm_folder_add_on_an_empty_tempdir_adds_nothing() {
        let dir = tempfile::tempdir().expect("create tempdir");
        // confirm_folder_add routes into the same intake, so it resolves the suffix template too.
        let (status, json) = request_json(
            api_router(test_state_installed()),
```

After:

```rust
    #[tokio::test]
    async fn confirm_folder_add_on_an_empty_tempdir_never_reaches_handbrake_resolution() {
        // The scenario this fix exists for: a folder with no videos in it reaches intake with
        // zero paths. It used to require an installed HandBrake to succeed, because intake
        // expanded the suffix template before looking at `paths`. `test_state()`'s
        // PanickingLocator now asserts that it does not.
        let dir = tempfile::tempdir().expect("create tempdir");
        let (status, json) = request_json(
            api_router(test_state()),
```

- [ ] **Step 3: Run both tests to verify they pass**

```bash
cargo test -p convertbar-server add_files_with_empty_paths_never_reaches_handbrake_resolution
cargo test -p convertbar-server confirm_folder_add_on_an_empty_tempdir_never_reaches_handbrake_resolution
```

Expected: both PASS.

- [ ] **Step 4: Mutation check — prove the route-level tests are load-bearing**

Delete the early return in `add_files_inner` again, run both tests, and confirm each goes RED
with a 500 (`{"error": "task panicked: ..."}`) rather than the expected 200. Restore, confirm
green. This proves the route tests independently cover the fix, not just the core unit test.

- [ ] **Step 5: Check `test_state_installed` is still used**

Run: `grep -n "test_state_installed" crates/convertbar-server/src/routes/mod.rs`

Expected: several remaining callers (the non-empty add tests, `remove_job`, and others). If the
count is zero, the helper is now dead and clippy will fail — report it rather than deleting the
helper unilaterally.

- [ ] **Step 6: Run the full workspace suite and the lints**

```bash
cargo test --workspace
cargo fmt --all -- --check
cargo clippy --workspace --all-targets
```

Expected: all green, zero warnings introduced.

- [ ] **Step 7: Commit**

```bash
git add crates/convertbar-server/src/routes/mod.rs
git commit -m "test: assert an empty intake never reaches HandBrake resolution"
```

---

### Task 3: Ship PR 1

**Files:** none modified.

- [ ] **Step 1: Confirm the branch contains exactly the intended diff**

```bash
git diff main --stat
```

Expected: `crates/convertbar-core/src/queue_ops.rs`, `crates/convertbar-core/src/types.rs`,
`crates/convertbar-server/src/routes/mod.rs`, plus the spec and plan documents. Nothing else.

- [ ] **Step 2: Final verification**

```bash
cargo test --workspace
cargo fmt --all -- --check
cargo clippy --workspace --all-targets
```

Paste the actual test summary line into the task report. Do not claim green without it.

- [ ] **Step 3: Hand off the push**

Claude cannot `git push`. Ask the user to run:

```
! git push -u origin fix/empty-intake-noop
```

- [ ] **Step 4: Open the PR**

```bash
gh pr create --base main \
  --title "fix: an empty intake is a no-op instead of an environment error" \
  --body "See docs/superpowers/specs/2026-07-29-handbrake-error-contract-design.md.

Adding zero paths resolved the output-suffix template before looking at \`paths\`, so an empty
intake failed with \"HandBrakeCLI not found\" when HandBrake was absent — including \"add a
folder that turned out to hold no videos\" on the desktop head. \`watcher::enqueue_and_start\`
already guarded emptiness at its own call site; the other three callers did not.

Non-empty intake with HandBrake absent still errors, unchanged."
```

- [ ] **Step 5: Merge once CI is green**

```bash
gh pr merge <n> --admin --squash
git checkout main && git pull --ff-only
git branch -d fix/empty-intake-noop
gh api -X DELETE repos/rhurling/convertbar/git/refs/heads/fix/empty-intake-noop
```

---

# PR 2 — One named HandBrake-missing error

Branch: `refactor/handbrake-error-contract`, cut from `main` **after PR 1 merges**.

### Task 4: Add the constant and the shared resolver

**Files:**
- Modify: `crates/convertbar-core/src/handbrake.rs` (add near `resolve_handbrake_path`, ~line 380; generalize `test_ctx` in the `mod tests` block)

**Interfaces:**
- Consumes: nothing from earlier tasks.
- Produces:
  - `convertbar_core::handbrake::HANDBRAKE_NOT_FOUND: &'static str`
  - `convertbar_core::handbrake::require_handbrake_path(ctx: &Ctx) -> Result<String, String>`
  - test-only `test_ctx_with_locator(locator: Arc<dyn HandbrakeLocator>) -> Arc<Ctx>` in `handbrake.rs`'s `mod tests` — named to match the identically-purposed helper already in `queue_ops.rs`'s test module (which additionally takes a `Connection`, since its tests seed rows)

- [ ] **Step 1: Generalize the test fixture**

In `crates/convertbar-core/src/handbrake.rs`'s `mod tests` block, replace `test_ctx`. Before:

```rust
    fn test_ctx() -> std::sync::Arc<Ctx> {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::db::init_db(&conn).unwrap();
        Ctx::new(
            conn,
            std::sync::Arc::new(crate::events::TestSink::default()),
            std::sync::Arc::new(crate::dispose::DeleteDisposer),
            std::sync::Arc::new(crate::handbrake::PanickingLocator),
        )
    }
```

After:

```rust
    fn test_ctx() -> std::sync::Arc<Ctx> {
        test_ctx_with_locator(std::sync::Arc::new(crate::handbrake::PanickingLocator))
    }

    /// `test_ctx` for tests that actually reach HandBrake resolution and must therefore say
    /// which world they are in, rather than inheriting whatever the host has installed.
    fn test_ctx_with_locator(locator: std::sync::Arc<dyn HandbrakeLocator>) -> std::sync::Arc<Ctx> {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::db::init_db(&conn).unwrap();
        Ctx::new(
            conn,
            std::sync::Arc::new(crate::events::TestSink::default()),
            std::sync::Arc::new(crate::dispose::DeleteDisposer),
            locator,
        )
    }
```

- [ ] **Step 2: Write the failing tests**

Add to the same `mod tests` block, after `absent_locator_reports_handbrake_missing`:

```rust
    #[test]
    fn require_handbrake_path_reports_missing_in_the_absent_world() {
        // The single production site of the `None` -> `Err` mapping. Eight call sites used to
        // each spell this out; if any of them drifts, it is now a compile error rather than a
        // silent cross-crate test break.
        let ctx = test_ctx_with_locator(std::sync::Arc::new(AbsentLocator));
        assert_eq!(
            require_handbrake_path(&ctx).unwrap_err(),
            HANDBRAKE_NOT_FOUND
        );
    }

    #[test]
    fn require_handbrake_path_returns_the_located_path_in_the_installed_world() {
        // `StubLocator` alone is enough here, with no preset-cache seeding: this function
        // resolves a path and stops — it never runs the binary.
        let ctx = test_ctx_with_locator(std::sync::Arc::new(StubLocator("/opt/HandBrakeCLI".into())));
        assert_eq!(require_handbrake_path(&ctx).unwrap(), "/opt/HandBrakeCLI");
    }
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test -p convertbar-core require_handbrake_path`

Expected: FAIL to compile — `cannot find function require_handbrake_path` and `cannot find value HANDBRAKE_NOT_FOUND`.

- [ ] **Step 4: Add the constant and the function**

In `crates/convertbar-core/src/handbrake.rs`, immediately after `resolve_handbrake_path`:

```rust
/// The message for "HandBrakeCLI could not be located". Exported because it is asserted across
/// a crate boundary (`convertbar-server`'s route tests read it as an HTTP response body) and is
/// persisted verbatim as a job's `error_message` by `converter::process_queue`, so the wording
/// is a contract rather than an incidental string. One definition means renaming it is a compile
/// error instead of a silent test break in another crate.
pub const HANDBRAKE_NOT_FOUND: &str = "HandBrakeCLI not found";

/// [`resolve_handbrake_path`] with "not found" folded into the error — the single production
/// site of the `None` -> `Err` mapping.
///
/// Locks `ctx.db` (via `resolve_handbrake_path`) and may spawn `which`/`where`, so it must never
/// be called while a `ctx.db` guard is held: the mutex is not reentrant. Call sites that already
/// hold the guard (`queue_ops::get_handbrake_path`) or that already hold a resolved path
/// (`queue_ops::add_files_inner`, `converter::process_queue`) reference [`HANDBRAKE_NOT_FOUND`]
/// directly instead — calling this there would deadlock or re-spawn `which`.
pub fn require_handbrake_path(ctx: &Ctx) -> Result<String, String> {
    resolve_handbrake_path(ctx)?.ok_or_else(|| HANDBRAKE_NOT_FOUND.to_string())
}
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p convertbar-core require_handbrake_path`

Expected: both PASS.

- [ ] **Step 6: Mutation check**

Change `require_handbrake_path`'s body to `Ok(resolve_handbrake_path(ctx)?.unwrap_or_default())`.
Run `cargo test -p convertbar-core require_handbrake_path` and confirm
`require_handbrake_path_reports_missing_in_the_absent_world` goes RED. Restore, confirm GREEN.

- [ ] **Step 7: Commit**

```bash
git add crates/convertbar-core/src/handbrake.rs
git commit -m "refactor: name the HandBrake-missing error and give it one resolver"
```

---

### Task 5: Collapse the four head call sites

**Files:**
- Modify: `crates/convertbar-server/src/routes/handbrake.rs:29-40` and `:81`
- Modify: `src-tauri/src/commands/handbrake.rs:21-30` and `:47`

**Interfaces:**
- Consumes: `hb::require_handbrake_path` from Task 4.
- Produces: nothing later tasks depend on.

Neither head needs a new import: `crates/convertbar-server/src/routes/handbrake.rs:12` already
has `use convertbar_core::handbrake as hb;` and `src-tauri/src/lib.rs:1` already re-exports
`handbrake` via `pub(crate) use convertbar_core::{converter, db, handbrake, types, watcher};`.

Both of these run inside `spawn_blocking` with no `ctx.db` guard held, so calling a function that
locks `ctx.db` is safe here.

- [ ] **Step 1: Rewrite the server's `list_handbrake_presets`**

In `crates/convertbar-server/src/routes/handbrake.rs`. Before:

```rust
pub async fn list_handbrake_presets(State(s): State<ServerState>) -> Response {
    let ctx = s.ctx.clone();
    match tokio::task::spawn_blocking(move || match hb::resolve_handbrake_path(&ctx)? {
        Some(p) => hb::list_presets(&p),
        None => Err("HandBrakeCLI not found".to_string()),
    })
    .await
    {
```

After:

```rust
pub async fn list_handbrake_presets(State(s): State<ServerState>) -> Response {
    let ctx = s.ctx.clone();
    match tokio::task::spawn_blocking(move || {
        let path = hb::require_handbrake_path(&ctx)?;
        hb::list_presets(&path)
    })
    .await
    {
```

- [ ] **Step 2: Rewrite the server's `generate_preset_suffix`**

Same file, inside the `spawn_blocking` closure. Before:

```rust
        let handbrake_path = hb::resolve_handbrake_path(&ctx)?.ok_or("HandBrakeCLI not found")?;
```

After:

```rust
        let handbrake_path = hb::require_handbrake_path(&ctx)?;
```

- [ ] **Step 3: Rewrite the desktop's `list_handbrake_presets`**

In `src-tauri/src/commands/handbrake.rs`. Before:

```rust
    tauri::async_runtime::spawn_blocking(move || match hb::resolve_handbrake_path(&ctx)? {
        Some(p) => hb::list_presets(&p),
        None => Err("HandBrakeCLI not found".to_string()),
    })
    .await
    .map_err(|e| e.to_string())?
```

After:

```rust
    tauri::async_runtime::spawn_blocking(move || {
        let path = hb::require_handbrake_path(&ctx)?;
        hb::list_presets(&path)
    })
    .await
    .map_err(|e| e.to_string())?
```

- [ ] **Step 4: Rewrite the desktop's `generate_preset_suffix`**

Same file. Before:

```rust
        let handbrake_path = hb::resolve_handbrake_path(&ctx)?.ok_or("HandBrakeCLI not found")?;
```

After:

```rust
        let handbrake_path = hb::require_handbrake_path(&ctx)?;
```

- [ ] **Step 5: Verify all four compile and the suite is green**

```bash
cargo test --workspace
cargo clippy --workspace --all-targets
```

Expected: green. In particular, `validate_handbrake` in both heads is deliberately untouched — it
maps `None` to `HandbrakeStatus { found: false, .. }` rather than an error, which is correct and
is not one of the eight sites.

- [ ] **Step 6: Commit**

```bash
git add crates/convertbar-server/src/routes/handbrake.rs src-tauri/src/commands/handbrake.rs
git commit -m "refactor: both heads resolve HandBrake through require_handbrake_path"
```

---

### Task 6: Collapse the purge call site

**Files:**
- Modify: `crates/convertbar-core/src/queue_ops.rs:1236-1258`

**Interfaces:**
- Consumes: `handbrake::require_handbrake_path` from Task 4.
- Produces: nothing later tasks depend on.

This is the delicate one. `purge_bad_sources` currently reads `bad_source_action` and the
configured HandBrake path in a single lock acquisition, then resolves outside the lock. Two
invariants must survive:

1. **The guard must be dropped before `require_handbrake_path` is called** — it locks `ctx.db`,
   and the mutex is not reentrant. The existing `let (...) = { ... };` block already drops it at
   the closing brace; keep that shape.
2. **R3** (the comment at `queue_ops.rs:1250-1253`): the path is resolved ONCE per batch,
   OUTSIDE the lock, because `PathLocator` spawns a blocking `which`/`where` and this used to run
   per id under the mutex. `require_handbrake_path` locks only to read the setting, releases, then
   runs the locator unlocked — so R3 holds, at the cost of one extra lock acquisition per batch
   (not per id).

- [ ] **Step 1: Rewrite the head of `purge_bad_sources`**

Before:

```rust
pub fn purge_bad_sources(ctx: &Arc<Ctx>, ids: Vec<String>) -> Result<Vec<PurgeResult>, String> {
    let (action, configured_handbrake_path) = {
        let conn = ctx.db.lock().map_err(|e| e.to_string())?;
        let action: String = conn
            .query_row(
                "SELECT value FROM settings WHERE key = 'bad_source_action'",
                params![],
                |r| r.get(0),
            )
            .unwrap_or_else(|_| "trash".to_string());
        (action, read_configured_handbrake_path(&conn))
    };
    let action = PurgeAction::from_setting(&action);
    // R3: resolved ONCE for the whole batch, OUTSIDE the lock, and passed to every
    // `purge_one_locked` call below — the fallback can spawn a blocking `which`/`where`
    // subprocess (`PathLocator`), and this used to run per id, under the DB mutex, in both
    // purge phases, i.e. up to 2N blocking spawns under the lock for a batch of N ids.
    let handbrake_path =
        handbrake::resolve_with_locator(configured_handbrake_path.as_deref(), &*ctx.handbrake)
            .ok_or_else(|| "HandBrakeCLI not found".to_string());
```

After:

```rust
pub fn purge_bad_sources(ctx: &Arc<Ctx>, ids: Vec<String>) -> Result<Vec<PurgeResult>, String> {
    let action: String = {
        let conn = ctx.db.lock().map_err(|e| e.to_string())?;
        conn.query_row(
            "SELECT value FROM settings WHERE key = 'bad_source_action'",
            params![],
            |r| r.get(0),
        )
        .unwrap_or_else(|_| "trash".to_string())
    }; // guard dropped here — `require_handbrake_path` below takes `ctx.db` itself, and the
       // mutex is not reentrant.
    let action = PurgeAction::from_setting(&action);
    // R3: resolved ONCE for the whole batch, OUTSIDE the lock, and passed to every
    // `purge_one_locked` call below — the fallback can spawn a blocking `which`/`where`
    // subprocess (`PathLocator`), and this used to run per id, under the DB mutex, in both
    // purge phases, i.e. up to 2N blocking spawns under the lock for a batch of N ids.
    // `require_handbrake_path` locks only to read the setting, releases, then runs the locator
    // unlocked, so R3 still holds — at the cost of one extra acquisition per batch, not per id.
    let handbrake_path = handbrake::require_handbrake_path(ctx);
```

- [ ] **Step 2: Confirm `read_configured_handbrake_path` is still used**

Run: `grep -n "read_configured_handbrake_path" crates/convertbar-core/src/queue_ops.rs`

Expected: its definition (~line 82) plus its call inside `get_handbrake_path` (~line 99). If the
only remaining hit is the definition, it is dead code, clippy will fail, and that is a signal the
edit went further than intended — report rather than delete.

- [ ] **Step 3: Run the purge tests specifically**

Run: `cargo test -p convertbar-core purge`

Expected: all pass, and none hang. A hang here means the guard was not dropped before
`require_handbrake_path` — kill it and re-read Step 1.

- [ ] **Step 4: Run the full workspace suite**

```bash
cargo test --workspace
cargo clippy --workspace --all-targets
```

Expected: green.

- [ ] **Step 5: Commit**

```bash
git add crates/convertbar-core/src/queue_ops.rs
git commit -m "refactor: purge_bad_sources resolves HandBrake through require_handbrake_path"
```

---

### Task 7: Point the remaining sites at the constant

**Files:**
- Modify: `crates/convertbar-core/src/queue_ops.rs:100` and `:831`
- Modify: `crates/convertbar-core/src/converter.rs:884`
- Modify: `crates/convertbar-core/src/converter.rs:1698` (test assertion)
- Modify: `crates/convertbar-core/src/queue_ops.rs:1649` (test assertion)
- Modify: `crates/convertbar-server/src/routes/mod.rs:288` (test assertion)

**Interfaces:**
- Consumes: `handbrake::HANDBRAKE_NOT_FOUND` from Task 4.
- Produces: nothing later tasks depend on.

These three production sites use the constant rather than `require_handbrake_path` because each
structurally cannot call it. Do not "finish the job" by converting them.

- [ ] **Step 1: `queue_ops::get_handbrake_path` (runs under the DB guard)**

Before:

```rust
    handbrake::resolve_with_locator(read_configured_handbrake_path(conn).as_deref(), locator)
        .ok_or_else(|| "HandBrakeCLI not found".to_string())
```

After:

```rust
    handbrake::resolve_with_locator(read_configured_handbrake_path(conn).as_deref(), locator)
        .ok_or_else(|| handbrake::HANDBRAKE_NOT_FOUND.to_string())
```

Its existing doc comment already explains why it takes `&Connection` + `&dyn HandbrakeLocator`
rather than `&Ctx`: it runs under `add_files_inner`'s guard, and a `&Ctx`-taking resolver would
invite re-locking the non-reentrant mutex. Leave that comment intact.

- [ ] **Step 2: `queue_ops::add_files_inner` (already holds a resolved `Option`)**

Before:

```rust
        let hb = hb_path.clone().ok_or("HandBrakeCLI not found")?;
```

After:

```rust
        let hb = hb_path.clone().ok_or(handbrake::HANDBRAKE_NOT_FOUND)?;
```

`ok_or` yields `Result<String, &'static str>`; `?` converts via `impl From<&str> for String`, so
the function's `Result<_, String>` is satisfied without an explicit `.to_string()`.

- [ ] **Step 3: `converter::process_queue` (already holds a resolved `Option`; passes `&str`)**

Before:

```rust
                record_job_error(
                    ctx,
                    &job.id,
                    &file_name,
                    "HandBrakeCLI not found",
                    crate::failure_class::FailureClass::Environment,
                );
```

After:

```rust
                record_job_error(
                    ctx,
                    &job.id,
                    &file_name,
                    crate::handbrake::HANDBRAKE_NOT_FOUND,
                    crate::failure_class::FailureClass::Environment,
                );
```

- [ ] **Step 4: The three test assertions**

`crates/convertbar-core/src/converter.rs` (~line 1698), before:

```rust
                .contains("HandBrakeCLI not found"),
```

after:

```rust
                .contains(crate::handbrake::HANDBRAKE_NOT_FOUND),
```

`crates/convertbar-core/src/queue_ops.rs` (~line 1649), before:

```rust
        assert!(err.contains("HandBrakeCLI not found"), "got: {err}");
```

after:

```rust
        assert!(
            err.contains(crate::handbrake::HANDBRAKE_NOT_FOUND),
            "got: {err}"
        );
```

`crates/convertbar-server/src/routes/mod.rs` (~line 288), before:

```rust
        assert_eq!(json, json!({"error": "HandBrakeCLI not found"}));
```

after:

```rust
        assert_eq!(
            json,
            json!({"error": convertbar_core::handbrake::HANDBRAKE_NOT_FOUND})
        );
```

- [ ] **Step 5: Confirm no literal survives**

Run: `grep -rn "HandBrakeCLI not found" crates src-tauri/src --include="*.rs"`

Expected: exactly ONE hit — the `HANDBRAKE_NOT_FOUND` definition in
`crates/convertbar-core/src/handbrake.rs`. Any other hit is a missed site; list it in the report.

(`src/pages/QueuePage.tsx` and `src/pages/SettingsPage.tsx` are excluded by the `--include` filter
and stay as they are — they render their own copy of the words driven by `hbStatus.found`, and
never parse an error.)

- [ ] **Step 6: Run the full workspace suite and the lints**

```bash
cargo test --workspace
cargo fmt --all -- --check
cargo clippy --workspace --all-targets
```

Expected: green.

- [ ] **Step 7: Commit**

```bash
git add crates/convertbar-core/src/queue_ops.rs crates/convertbar-core/src/converter.rs \
        crates/convertbar-server/src/routes/mod.rs
git commit -m "refactor: the remaining HandBrake-missing sites reference one constant"
```

---

### Task 8: Verify the refactor's invariants, file the deferred item, ship

**Files:**
- Modify: `docs/RECOMMENDATIONS.md` (the `## Open — Polish` section, after item 14)

**Interfaces:**
- Consumes: everything from Tasks 4-7.
- Produces: the shipped PR.

- [ ] **Step 1: Single-sourcing check**

This is the refactor's actual invariant: the words exist in exactly one place. Temporarily change
the constant in `crates/convertbar-core/src/handbrake.rs`:

```rust
pub const HANDBRAKE_NOT_FOUND: &str = "XXX PLACEHOLDER XXX";
```

Run `cargo test --workspace`. Expected: **green**. Any failure names a test that still hardcodes
the words — fix that test to use the constant, then re-run. Restore the real value afterwards and
confirm green again. Record both runs in the report.

- [ ] **Step 2: Panic-detection check**

The server test at `routes/mod.rs:288` exists to prove its 500 is deliberate rather than a panic
unwinding inside `spawn_blocking`. Swapping its assertion to the constant must not weaken that.

Temporarily add `panic!("forced");` as the first line of the `spawn_blocking` closure in
`crates/convertbar-server/src/routes/queue.rs`'s `add_files` handler. Run:

`cargo test -p convertbar-server add_files_route_reports_the_error_when_handbrake_is_absent`

Expected: RED — the body is `{"error": "task panicked: ..."}`, not the constant. Remove the
`panic!` and confirm GREEN. This is what makes deferring the item in Step 3 safe; record the
result.

- [ ] **Step 3: File the deferred item**

Append to `docs/RECOMMENDATIONS.md` in the `## Open — Polish` section, after `### 14. Drop on Tray Icon`
and before the `---` that closes the section:

```markdown
### 16. Server: panics masquerade as deliberate errors
- All ten `spawn_blocking` join-error sites in `crates/convertbar-server/src/routes/`
  (`queue.rs:29,44,53,67,129`, `fs.rs:89`, `handbrake.rs:24,38,63,88`) map to
  `core_err(format!("task panicked: {join}"))` — HTTP 500 with an `error` string, identical in
  shape to an ordinary core failure.
- A client cannot distinguish a server bug from an expected condition such as a missing
  HandBrakeCLI, and tests can only tell them apart by matching on the message text.
- Consider a distinct status or body shape for join failures. Surfaced 2026-07-29 while giving
  the HandBrake-missing error one definition (`HANDBRAKE_NOT_FOUND`).
```

Before writing, run `grep -n "^### 1[5-9]\." docs/RECOMMENDATIONS.md` to confirm `16.` is not
already taken; if it is, use the next free number and adjust the heading.

- [ ] **Step 4: Final verification**

```bash
cargo test --workspace
cargo fmt --all -- --check
cargo clippy --workspace --all-targets
```

Paste the actual test summary line into the report. Do not claim green without it.

- [ ] **Step 5: Commit**

```bash
git add docs/RECOMMENDATIONS.md
git commit -m "docs: file the server panic-vs-deliberate-error gap as a backlog item"
```

- [ ] **Step 6: Hand off the push and open the PR**

Ask the user to run:

```
! git push -u origin refactor/handbrake-error-contract
```

Then:

```bash
gh pr create --base main \
  --title "refactor: one named HandBrake-missing error instead of eight literals" \
  --body "See docs/superpowers/specs/2026-07-29-handbrake-error-contract-design.md.

\`\"HandBrakeCLI not found\"\` was constructed at eight production sites across three crates and
asserted at three test sites, including as a cross-crate HTTP response body. It played three
roles at once — internal diagnostic, user-visible \`error_message\`, and HTTP contract — without
anyone deciding it should be any of them.

Five sites now call \`handbrake::require_handbrake_path\`; three reference
\`handbrake::HANDBRAKE_NOT_FOUND\` because they hold the DB guard or an already-resolved path.
No behavior change.

Related backlog item filed: server \`spawn_blocking\` panics are indistinguishable from
deliberate errors (RECOMMENDATIONS item 16)."
```

- [ ] **Step 7: Merge once CI is green**

```bash
gh pr merge <n> --admin --squash
git checkout main && git pull --ff-only
git branch -d refactor/handbrake-error-contract
gh api -X DELETE repos/rhurling/convertbar/git/refs/heads/refactor/handbrake-error-contract
```
