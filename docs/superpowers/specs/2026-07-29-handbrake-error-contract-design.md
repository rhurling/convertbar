# Empty Intake and the HandBrake-Missing Error Contract

**Date:** 2026-07-29
**Status:** Approved, ready for planning
**Ships as:** two independent PRs, in order

## Why now

The HandbrakeLocator seam (PR #133) did not create either problem below. It made both
*visible* for the first time by letting a test state "HandBrake is absent" instead of
inheriting whatever the host machine happened to have installed. Two things surfaced:

1. `add_files_inner` fails on an empty intake when HandBrake is absent. Pre-existing —
   confirmed against the code as it stood before the seam, not assumed.
2. The literal `"HandBrakeCLI not found"` is now asserted as an HTTP response body across a
   crate boundary. It became a contract during that work without anyone deciding it should
   be one.

They are independent and ship separately. PR 1 changes intake semantics. PR 2 is a pure
refactor with no behavior change.

---

## PR 1 — An empty intake is a no-op

### Current behavior

`add_files_inner` (`crates/convertbar-core/src/queue_ops.rs:775-836`) reads settings,
resolves HandBrakeCLI, and expands the output-suffix template *before* it ever looks at
`paths`. The default suffix template is `.{resolution}-{codec}`, which contains `{`, so the
HandBrake path is required on every call.

With `paths = []` and HandBrake absent, the function returns
`Err("HandBrakeCLI not found")`. With HandBrake present and a cold preset cache, it pays a
`which` spawn plus a `HandBrakeCLI --preset-export` spawn to compute a suffix that will
never be applied to anything.

### Blast radius

Four callers funnel through `add_files_inner`:

| Caller | Reaches it with an empty slice when… |
|---|---|
| `queue_ops::add_files` → desktop drop / `POST /api/queue/files` | the client posts `{"paths": []}` |
| `queue_ops::confirm_folder_add` | the chosen folder contains no video files |
| `watcher::enqueue_and_start` | **never** — guarded |
| (server route, via `add_files`) | as above |

`watcher::enqueue_and_start` (`crates/convertbar-core/src/watcher.rs:422-434`) already
guards `paths.is_empty()` three times before calling in. The rule "an empty intake is a
no-op" already exists in this codebase; it is implemented at one caller instead of at the
function that needs it.

The user-visible bug is therefore not limited to the HTTP head: **"add a folder containing
no video files" errors with "HandBrakeCLI not found" on the desktop too**, when HandBrake
is not installed.

### Decision

An intake with zero paths is a no-op that returns success and never reaches HandBrake
resolution.

Rejected alternatives:

- *Defer suffix resolution until first use.* No gain. `add_files_to_db`'s skip rules
  (`fetch_skip_sets`) compare candidate output names, so the suffix is forced for any
  non-empty intake anyway. All this buys is the empty case, which the early return already
  covers, at the cost of threading a lazy resolver through the skip rules.
- *Make missing HandBrake a `SkipReason` so a non-empty intake returns 200 with
  `skipped: [{HandbrakeMissing, N}]`.* Rejected: files the user explicitly dragged in would
  vanish into a skip badge instead of producing a clear, actionable error. A missing
  encoder is an environment fault worth reporting as one.

### Design

One early return at the top of `add_files_inner`, before the settings read:

```rust
pub fn add_files_inner(
    ctx: &Ctx,
    paths: &[String],
    progress: Option<&dyn Fn(u32, u32)>,
) -> Result<AddResult, String> {
    // Nothing to add decides nothing: there is no output name to build, so there is no
    // reason to reach HandBrake. Resolving the suffix first meant an empty intake failed
    // outright when HandBrakeCLI was absent — including "add a folder with no videos in
    // it". The watcher already guards this at its own call site; the other callers did not.
    if paths.is_empty() {
        return Ok(AddResult::default());
    }
    // ... existing settings read, suffix resolution, media skip, insert
}
```

`AddResult` (`crates/convertbar-core/src/types.rs:112`) gains `Default` in its derive list.
Both fields are `Vec`, so the derive is correct without a manual impl.

**Placement is in `add_files_inner`, not in the callers.** That covers all four entry points
at once, including any future one.

**The watcher's three existing guards stay.** They short-circuit *before* constructing
`AddOp`, so a no-op watcher batch still emits no `add-started`/`add-finished` bracket.
Removing them would change event behavior; this PR changes intake semantics only.

### Behavior delta

| Case | Before | After |
|---|---|---|
| empty intake, HandBrake absent | `Err("HandBrakeCLI not found")` | `Ok({added: [], skipped: []})` |
| empty intake, HandBrake present | `Ok` after 2 subprocess spawns (cold cache) | `Ok`, zero subprocesses |
| non-empty intake, HandBrake absent | `Err("HandBrakeCLI not found")` | **unchanged** |
| non-empty intake, HandBrake present | queues | **unchanged** |

One deliberate side effect: an empty intake through `add_files` now emits
`add-started` → `add-finished` → `queue-updated`, where previously it emitted
`add-started` → `add-finished` → *error* when HandBrake was absent. The extra
`queue-updated` is a redundant UI refresh, harmless, and already the behavior when
HandBrake is installed.

`add_files_inner` stops validating the environment as a side effect of being called with no
work. No caller relies on that; none of the four call it as a probe.

**The same shape exists in `purge_bad_sources` and is deliberately left alone.**
`routes/mod.rs:449` documents it: a purge with `ids: []` still resolves HandBrake up front,
spawning a `which` for a batch it will never consume. It is not fixed here, because the
resolution there is genuinely per-batch state threaded into `purge_one_locked` for each id —
a different shape from intake, where the suffix is a value computed for paths that do not
exist. Recording the asymmetry so a later reader does not "finish the job" and assume it was
an oversight. If it is worth fixing, it is its own change with its own argument.

### Testing

**New core test** — the seam pays for itself; no new fixture is needed, because
`queue_ops`'s `test_ctx` defaults to `PanickingLocator`:

```rust
#[test]
fn add_files_inner_with_no_paths_never_reaches_handbrake_resolution() {
    // Asserts the negative directly. The fixture's default PanickingLocator means that if
    // the early return regresses, resolution is reached and this panics — rather than
    // quietly passing on any machine that happens to have HandBrakeCLI installed.
    let (ctx, _sink, _disposer) = test_ctx(test_conn());
    let result = add_files_inner(&ctx, &[], None).expect("an empty intake cannot fail");
    assert!(result.added.is_empty());
    assert!(result.skipped.is_empty());
}
```

**Mutation check** (per the project's load-bearing-test bar): delete the early return,
confirm the test goes red with the locator's "Declare the world explicitly" message,
restore.

**Three existing tests to update.** Each currently declares an *installed* world only because
of the bug, and each carries a comment that documents the bug as though it were the design.
After the fix all three move to the panicking default, which converts every one of them into
an additional regression guard — a test that says "this must not reach resolution" is
strictly stronger than one that quietly tolerates it:

| Test | Today | Its comment today |
|---|---|---|
| `queue_ops.rs:1683` `add_files_emits_finished_before_queue_updated` — calls `add_files(&ctx, &[])` | `StubLocator` + `seed_preset_cache` | *"Without the seed the stub path would be shelled out to and intake would return Err, swallowing the queue-updated emit this test asserts on."* |
| `routes/mod.rs:258` `add_files_with_empty_paths_returns_empty_added_and_skipped` | `test_state_installed()` | *"Even an empty add resolves the suffix template first, so the world must be declared."* |
| `routes/mod.rs:471` `confirm_folder_add_on_an_empty_tempdir_adds_nothing` | `test_state_installed()` | *"confirm_folder_add routes into the same intake, so it resolves the suffix template too."* |

The third is the exact user-facing scenario this PR fixes — "add a folder that turned out to
hold no videos" — and it needed an installed HandBrake to pass. That is the bug, written down
as a fixture.

---

## PR 2 — One named HandBrake-missing error

### Current state

The literal `"HandBrakeCLI not found"` is constructed at eight production sites across three
crates, and asserted at three test sites. It simultaneously plays three roles that nobody
chose for it:

- an **internal diagnostic** returned from core functions,
- a **user-visible string** — `converter.rs:884` persists it as a job's `error_message` via
  `record_job_error`, and the UI renders that verbatim,
- a **cross-crate HTTP contract** — `routes/mod.rs:288` asserts the exact 500 body.

Not coupled: the frontend never parses it. `src/pages/QueuePage.tsx:81` renders its own copy
of those words, driven by `hbStatus.found`, and `src/pages/SettingsPage.tsx:464` alerts its
own. Both stay as they are.

### Decision

Dedupe structurally first, then name what remains. Sites that hold a `&Ctx` and are not
under the DB guard call a new core function that owns the `None` → `Err` mapping. Sites that
structurally cannot reference one exported constant.

Renaming the message becomes a compile error rather than a silent cross-crate test break.

Rejected alternative: *just add the constant, change nothing else.* It dedupes the literal
but leaves eight copies of the decision that `None` means an error — the pattern, not just
the words.

### Design

In `crates/convertbar-core/src/handbrake.rs`:

```rust
/// The message for "HandBrakeCLI could not be located". Exported because it is asserted
/// across a crate boundary (`convertbar-server`'s route tests read it as an HTTP body) and
/// is persisted verbatim as a job's `error_message`, so the wording is a contract, not an
/// incidental string. One definition means renaming it is a compile error, not a silent
/// test break in another crate.
pub const HANDBRAKE_NOT_FOUND: &str = "HandBrakeCLI not found";

/// [`resolve_handbrake_path`], with "not found" folded into the error. The single production
/// site of the `None` -> `Err` mapping.
///
/// Locks `ctx.db` (via `resolve_handbrake_path`) and may spawn `which`/`where`, so it must
/// never be called while the DB guard is held — see the three constant-only sites below.
pub fn require_handbrake_path(ctx: &Ctx) -> Result<String, String> {
    resolve_handbrake_path(ctx)?.ok_or_else(|| HANDBRAKE_NOT_FOUND.to_string())
}
```

Both heads already have the module in scope — `convertbar-server` via
`use convertbar_core::handbrake as hb`, `src-tauri` via the `pub(crate) use` re-export in
`src-tauri/src/lib.rs:1`. No new plumbing.

#### Five sites collapse into `require_handbrake_path`

| Site | Today | After |
|---|---|---|
| `crates/convertbar-server/src/routes/handbrake.rs:32` | `match resolve_handbrake_path(&ctx)? { Some(p) => list_presets(&p), None => Err("…") }` | `hb::list_presets(&hb::require_handbrake_path(&ctx)?)` |
| `crates/convertbar-server/src/routes/handbrake.rs:81` | `resolve_handbrake_path(&ctx)?.ok_or("…")?` | `hb::require_handbrake_path(&ctx)?` |
| `src-tauri/src/commands/handbrake.rs:26` | as server:32 | as server:32 |
| `src-tauri/src/commands/handbrake.rs:47` | as server:81 | as server:81 |
| `crates/convertbar-core/src/queue_ops.rs:1256` | `resolve_with_locator(configured.as_deref(), &*ctx.handbrake).ok_or_else(…)` | `handbrake::require_handbrake_path(ctx)` (no `?` — the `Result` is passed on as-is) |

The `queue_ops.rs:1256` site needs care. `purge_bad_sources` currently reads
`bad_source_action` and `handbrake_path` in a single lock acquisition, then resolves outside
the lock. After the change, the lock block reads only `bad_source_action`, and
`require_handbrake_path` takes its own brief lock.

**The R3 invariant is preserved**: the comment at `queue_ops.rs:1250-1253` requires the path
to be resolved *once per batch, outside the lock*, because the fallback can spawn a blocking
`which`/`where` and the original code did it per id under the mutex. `require_handbrake_path`
locks only to read the setting, releases, then runs the locator unlocked — still once per
batch. The cost is one extra lock acquisition per purge batch (not per id), on a
user-initiated path. `read_configured_handbrake_path` stays; `get_handbrake_path` still uses
it.

#### Three sites reference the constant

Each of these structurally cannot call `require_handbrake_path`, and the spec records why so
a future reader does not "finish the job" and reintroduce a deadlock or a duplicate probe:

| Site | Why not the function |
|---|---|
| `crates/convertbar-core/src/queue_ops.rs:100` (`get_handbrake_path`) | Runs **under** `add_files_inner`'s DB guard. Its own doc comment already states that a `&Ctx`-taking resolver would invite re-locking the non-reentrant `ctx.db`. Takes `&Connection` + `&dyn HandbrakeLocator` by design. |
| `crates/convertbar-core/src/queue_ops.rs:831` | Already holds a resolved `Option<String>` from line 814. Calling `require_handbrake_path` here would resolve a second time — an extra `which` spawn per intake. |
| `crates/convertbar-core/src/converter.rs:884` | Already holds a resolved `Option` (`handbrake_path_opt`) and passes a `&str` into `record_job_error`. |

Three test assertions switch to the constant: `converter.rs:1698`, `queue_ops.rs:1649`,
`crates/convertbar-server/src/routes/mod.rs:288`.

### What is explicitly not changed

`add_files_inner` currently builds the error at line 814 via `get_handbrake_path(...).ok()`,
discards it, and rebuilds it at line 831. Threading the `Result` through instead would
require `hb_path: Option<Result<String, String>>`, because the resolution is itself
conditional and line 850 consumes it as an `Option`. That is a worse shape than the single
duplicated literal it removes. Line 831 uses the constant and the structure stays.

### Testing

PR 2 is a refactor: no behavior changes, so the existing suite is the safety net and every
current assertion must still hold.

**New unit tests** — `require_handbrake_path` is now the single production site of the
mapping, so it gets a direct test in both declared worlds. `handbrake.rs`'s test module
currently has `fn test_ctx()` with `PanickingLocator` hardcoded; it is generalized to
`fn test_ctx_with(locator: Arc<dyn HandbrakeLocator>) -> Arc<Ctx>`, with `test_ctx()` kept as
a delegating wrapper so existing callers in that module are untouched.

```rust
#[test]
fn require_handbrake_path_reports_missing_in_the_absent_world() {
    let ctx = test_ctx_with(Arc::new(AbsentLocator));
    assert_eq!(require_handbrake_path(&ctx).unwrap_err(), HANDBRAKE_NOT_FOUND);
}

#[test]
fn require_handbrake_path_returns_the_located_path_in_the_installed_world() {
    // StubLocator alone is enough here: require_handbrake_path resolves a path and stops —
    // it never runs the binary, so no preset-cache seeding is needed.
    let ctx = test_ctx_with(Arc::new(StubLocator("/opt/HandBrakeCLI".into())));
    assert_eq!(require_handbrake_path(&ctx).unwrap(), "/opt/HandBrakeCLI");
}
```

**Single-sourcing check** (the refactor's actual invariant): temporarily change
`HANDBRAKE_NOT_FOUND` to a different string and confirm `cargo test --workspace` stays
green. Any test that independently hardcodes the words will fail, which is exactly what the
check is for. Restore afterwards.

**The panic-detection property must survive.** The server test at `routes/mod.rs:288` was
written to prove the 500 was deliberate rather than a panic unwinding inside
`spawn_blocking`. `assert_eq!(json, json!({"error": hb::HANDBRAKE_NOT_FOUND}))` still fails
on `{"error": "task panicked: ..."}`, so nothing is lost. Verify by temporarily making the
route's blocking closure `panic!()` and confirming that test goes red. This is what makes
deferring the item below safe.

---

## Deferred — filed, not built

The reason the server test pinned the exact wording is that a deliberate core error and a
panic inside `spawn_blocking` are indistinguishable by status: all ten join-error sites in
`crates/convertbar-server/src/routes/` — `queue.rs:29,44,53,67,129`, `fs.rs:89`,
`handbrake.rs:24,38,63,88` — map to `core_err(format!("task panicked: {join}"))`, i.e. the
same 500 as any ordinary failure. A client cannot tell a bug from an expected failure.

Counting these is fiddlier than it looks, and two plausible methods both give the wrong answer.
Grepping the phrase `task panicked` over-reports — the surplus hits are comments inside
`routes/mod.rs` tests. Grepping `core_err(format!(...))` under-reports at nine, because
`fs.rs:89` routes through that module's own `json_err` helper instead. The real count is ten
join-error sites, and the property they share is the response *shape* (500 +
`{"error": ...}`), not the helper that builds it.

That is a third, larger idea — a transport error taxonomy touching every route — and it is
out of scope here. It is added to `docs/RECOMMENDATIONS.md` under **Open — Polish** as:

> ### 16. Server: panics masquerade as deliberate errors
> All ten `spawn_blocking` join-error sites in `crates/convertbar-server/src/routes/`
> map to `core_err(format!("task panicked: {join}"))` — HTTP 500 with an `error` string,
> identical in shape to an ordinary core failure. A client cannot distinguish a server bug
> from an expected condition such as a missing HandBrakeCLI, and tests can only tell them
> apart by matching on the message text. Consider a distinct status or body shape for join
> failures.

Deferring is safe because PR 2 preserves the property the test relies on (see above).

---

## Sequencing and verification

Two PRs, in order. Both touch `queue_ops.rs`, but in non-overlapping regions
(PR 1 at 775-780; PR 2 at 100, 831, 1256), so sequential landing avoids any conflict.

**Outstanding collision.** A locked worktree at
`.claude/worktrees/feature+server-auth-throttling` holds an unmerged
`feature/server-auth-throttling` branch (RECOMMENDATIONS item 15) with pre-locator-seam copies
of `routes/handbrake.rs` and `queue_ops.rs` — its `queue_ops.rs:104` still calls
`detect_handbrake_path` directly. PR 2 rewrites those exact regions. Whoever lands second
rebases through conflicts. Not a blocker, but land that branch first if it is close.

**Branch topology.** PR 2's branch was cut from PR 1's tip rather than from `main`, because
`main` is protected and PR 1 could not be merged first. Consequence, which must not be
forgotten: **after PR 1 squash-merges, PR 2 needs `git rebase --onto main fix/empty-intake-noop`**
before it is opened. A squash rewrites history, so PR 1's commits will not drop out of PR 2's
range on their own — without the rebase, PR 2's diff shows both changesets.

1. **PR 1** — `fix: an empty intake is a no-op instead of an environment error`
   Touches `queue_ops.rs`, `types.rs`, one new core test, and three existing tests whose
   declared worlds and comments were artifacts of the bug (`queue_ops.rs:1683`,
   `routes/mod.rs:258`, `routes/mod.rs:471`).
2. **PR 2** — `refactor: one named HandBrake-missing error instead of eight literals`
   Touches `handbrake.rs`, `queue_ops.rs`, `converter.rs`, both heads' `handbrake` command
   modules, three test assertions, two new unit tests, `docs/RECOMMENDATIONS.md`.

Each PR must pass, before it is opened:

```
cargo test --workspace
cargo fmt --all -- --check
cargo clippy --workspace --all-targets
```

Neither PR touches TypeScript, so no frontend change is expected; CI's `frontend` check runs
`npm run build` regardless. Required checks are `frontend` and `rust (ubuntu-22.04)`.

`main` is protected: branch, commit signed, ask the user to push, then
`gh pr create --base main` and `gh pr merge <n> --admin --squash`.
