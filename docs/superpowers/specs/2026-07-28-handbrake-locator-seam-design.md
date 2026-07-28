# HandBrake Locator Seam — Design

> Revised after an adversarial review (2026-07-28). Findings that changed the design are
> marked **[R]** at the relevant section.

## Problem

Five tests shipped a hidden dependency on HandBrakeCLI being installed. They passed on the
developer machine (Homebrew has it at `/opt/homebrew/bin/HandBrakeCLI`) and failed on CI, which
has no HandBrake. One was caught by CI during PR #130; a stripped-PATH sweep found the other four.

Both were patched by pinning a literal suffix in the fixture so the intake path never needs to
resolve HandBrake at all (`crates/convertbar-core/src/queue_ops.rs:1643`,
`crates/convertbar-server/src/routes/mod.rs:179`) — and a third instance of the same dodge already
existed at `crates/convertbar-core/src/watcher.rs:1153-1156`. Those patches fix specific instances.
They do not close the class — the next test to reach the intake path with default settings reacquires the
same dependency, and the developer machine hides it again.

### Why the tests could not simply cover both worlds

Four call sites resolve HandBrake, three of them triplicating the same logic: **[R]**

| Function | Signature | Detection fallback | Callers |
|---|---|---|---|
| `handbrake::resolve_handbrake_path` | `&Ctx` | `handbrake.rs:322` | 8 — server routes + tauri commands |
| `queue_ops::get_handbrake_path` | `&Connection` | `queue_ops.rs:104` | 2 — `add_files_inner`, a test helper |
| `converter::get_handbrake_path` | `&Connection` | `converter.rs:452` | 1 — `process_queue` |
| `queue_ops::purge_bad_sources` | `&Arc<Ctx>` | calls `resolve_from_configured` direct, `queue_ops.rs:1263` | — resolves inline |

The fourth was missed in the first draft. `purge_bad_sources` resolves **unconditionally**, before
iterating ids — even for an empty list — and the DB default for `handbrake_path` is `""`
(`db.rs:196`), so it reaches the fallback. It already takes `&Arc<Ctx>`, so wiring it to the seam is
trivial; the cost is that it pulls four more existing tests into the "must declare a world" set
(enumerated under Testing).

The first three have the identical shape:

```rust
// read the `handbrake_path` setting from the DB
if configured points at an existing file { return it }
detect_handbrake_path()          // ← shells out to `which` / `where`
```

That last line reads ambient machine state. The consequence is **one-sided controllability**:

- **"HandBrakeCLI is present"** is already expressible — point the `handbrake_path` setting at any
  existing file. The converter tests do exactly this at 18+ sites, pinning a fake script
  (`converter.rs:1673`, `:1730`, `:1763`, …). Those tests are already hermetic.
- **"HandBrakeCLI is absent"** is **not expressible**. That branch always falls through to the real
  `which`, so the only way to produce it is to mutate the process environment.

That asymmetry is the defect. A stripped-PATH script (`PATH=/usr/bin:/bin cargo test`) is a
workaround for it, not a fix: it fakes the uncontrollable branch from the outside, must be
remembered before every push, and leaves the ambient dependency in place.

### Why the existing tests survive

`process_queue` calls `get_handbrake_path(&db)` **unconditionally** on every loop iteration
(`converter.rs:807`) — there is no guard. The converter tests survive because they pin the
`handbrake_path` setting, so resolution returns the configured path and never reaches detection.
This is load-bearing for the change below: a seam placed at the *fallback* point leaves every one
of those tests untouched, and fires only where a test actually falls through to the machine.

## Design

Add a locator collaborator to `Ctx`, beside the two injected collaborators already there
(`ctx.rs:10-11`):

```rust
pub trait HandbrakeLocator: Send + Sync {
    /// The HandBrakeCLI path discovered from the environment, or None if not installed.
    fn locate(&self) -> Option<String>;
}

/// Production: PATH detection via `which` / `where`.
pub struct PathLocator;

impl HandbrakeLocator for PathLocator {
    fn locate(&self) -> Option<String> {
        detect_handbrake_path()
    }
}
```

`Ctx` gains one field:

```rust
pub struct Ctx {
    pub db: Arc<Mutex<rusqlite::Connection>>,
    pub converter: Arc<crate::converter::ConverterState>,
    pub events: Arc<dyn crate::events::EventSink>,
    pub disposer: Arc<dyn crate::dispose::FileDisposer>,
    pub handbrake: Arc<dyn crate::handbrake::HandbrakeLocator>,   // new
    pub preset_cache: Mutex<HashMap<String, crate::handbrake::PresetMetadata>>,
    pub watcher: crate::watcher::WatcherState,
}
```

`Ctx::new` gains a `locator` parameter. That touches **14 call sites: 2 production**
(`crates/convertbar-server/src/main.rs:40`, `src-tauri/src/lib.rs:92`, both passing `PathLocator`,
preserving today's behavior exactly) **and 12 test fixtures**, most of which are the per-module
`test_ctx` helpers — so the churn concentrates in ~7 helper functions rather than per test.

An alternative was considered and rejected: leave `Ctx::new` alone and default it to `PathLocator`,
adding a separate `new_with_locator` for tests. That is less churn, but a test fixture that forgets
to opt in silently gets real PATH detection — which is the exact failure mode this design exists to
remove. The explicit parameter makes forgetting impossible.

### Converging the three resolvers

One shared helper in `handbrake.rs`, next to the trait, owns the logic that is currently
triplicated. It **replaces** `queue_ops::resolve_from_configured` (`queue_ops.rs:98-105`), which is
deleted:

```rust
/// The configured path if it points at an existing file, otherwise the locator's answer.
pub fn resolve_with_locator(
    configured: Option<&str>,
    locator: &dyn HandbrakeLocator,
) -> Option<String>
```

Callers that need the current `Result<String, String>` shape keep their own
`.ok_or_else(|| "HandBrakeCLI not found".to_string())`, so no error text changes.

Explicitly, so an implementer cannot satisfy the letter and miss the point: **[R]**

- `queue_ops::get_handbrake_path` and `converter::get_handbrake_path` are **kept**, gaining a
  locator parameter. (An earlier draft said these were deleted *and* kept — they are kept.)
- `handbrake::resolve_handbrake_path`'s **body changes**: its fallback at `handbrake.rs:322` stops
  calling `detect_handbrake_path()` and calls `ctx.handbrake.locate()` instead. Its 8 callers are
  unaffected, but leaving the body alone would keep the ambient read alive at the detect route and
  falsify the entire thesis.
- `purge_bad_sources` (`queue_ops.rs:1263`) resolves through the helper, passing `&*ctx.handbrake`.

### Why the `&Connection` wrappers keep that shape **[R]**

The two `&Connection` wrappers take the locator as a **second argument** rather than switching to
`&Ctx`. The first draft justified this by claiming they run outside the DB mutex. **That is
backwards, and the corrected reason matters more than the original one.**

Both call sites run *under* the guard: `queue_ops.rs:790` takes the lock, `:823` resolves, `:835`
releases it; `converter.rs:802` takes it, `:807` resolves, `:812` releases. Handing them `&Ctx`
invites a resolver that re-locks `ctx.db` — and `std::sync::Mutex` is not reentrant, so that
self-deadlocks. This is the same hazard class as the emit-under-db-lock invariant documented in
CLAUDE.md, which cost two shipped deadlocks. **Correction:** the `&Connection` shape only
constrains the two resolver *functions* — they never see `ctx.db`, so they cannot re-lock it. It
does not constrain the `&dyn HandbrakeLocator` trait object they call into: a locator can close
over anything, including `Arc<Mutex<Connection>>`, independent of that parameter. The invariant is
enforced by the doc comment on `HandbrakeLocator::locate` (`handbrake.rs`), not by the type
system.

Two consequences follow, both accepted rather than fixed here:

- `locate()` runs under the db lock at those two sites. That is exactly what
  `detect_handbrake_path()` does there today, so the seam neither improves nor worsens it. The R3
  comment at `queue_ops.rs:92-97` expresses the intent to avoid this, and `purge_bad_sources`
  honors it while these two do not — a pre-existing inconsistency, out of scope for a testability
  change.
- A `PanickingLocator` firing under that lock **poisons the mutex**. Loud and messy, but loud is
  the point, and the alternative is silent machine-coupling.

### Test doubles

Three, mirroring the existing `TestSink` / `RecordingDisposer` idiom:

| Double | Behavior | Use |
|---|---|---|
| `PanickingLocator` | Panics when consulted, naming the expectation | **The fixture default** |
| `AbsentLocator` | Returns `None` | The CI world — newly expressible |
| `StubLocator(String)` | Returns a fixed path | HandBrake-installed world without a real binary |

`PanickingLocator` as the default is the same tactic as `LockProbeSink` in `control.rs`: make a
silent regression loud. A test that reaches a resolver without declaring a world fails instead of
quietly reading the developer's machine and passing for the wrong reason.

**How loudly, precisely — it varies by thread. [R]** On a direct call (`add_files_inner` from a
test body) the panic fails that test outright. But `process_queue` runs on a spawned thread
(`converter.rs:1442`, started from `control.rs:42`, `watcher.rs:473`, `src-tauri/src/updater.rs:915`),
where a panic does not fail the test process — it surfaces indirectly as missing events. On a
server route it degrades to a 500 via the `JoinError` arm (`routes/handbrake.rs:24`). So the guard
is strongest exactly where the defect occurred (the intake path) and weaker on the queue thread.
No non-ignored test currently reaches `process_queue` unpinned, so this is a limit on the guard's
reach, not a present gap.

The default is still cheap, but not free: the converter tests pin `handbrake_path` and never reach
the fallback, so they need no change. Four other existing tests do reach a fallback and must
declare a world — they are enumerated below rather than left for the implementer to discover.

## Testing

**Present/absent pairs on the intake path** — the path that actually broke:

- `add_files_inner` with a `{...}` suffix template and `AbsentLocator` → the documented
  `"HandBrakeCLI not found"` error, no panic, no silent success.
- The same in the present world → resolution proceeds (see the caveat immediately below).
- The server add-files route, both worlds, asserting the HTTP status rather than a 500.

**Expressing the present world needs more than `StubLocator`. [R]** A bare stub path is *not*
sufficient on the intake path: past resolution, `add_files_inner` calls `cached_preset_metadata`
(`queue_ops.rs:838-840`), which on a cache miss shells out to the resolved path and propagates the
failure (`handbrake.rs:341`). A stub pointing at a non-executable path therefore returns `Err`, and
`add_files_emits_finished_before_queue_updated` would lose its `queue-updated` emit entirely
(emitted only on `Ok`, `queue_ops.rs:1053-1055`) — i.e. the restored test would fail.

The present world is therefore expressed as `StubLocator` **plus a pre-populated
`ctx.preset_cache`**, which short-circuits before the shell-out. `preset_cache` is already a public
field on `Ctx`, so this needs no new API. (The `#[ignore]`d e2e test at `queue_ops.rs:2355-2365`
solves the same problem with a real fake executable; the cache is cheaper and needs no temp files.)

**Three pinned-suffix workarounds are removed, not two. [R]** `queue_ops.rs:1643`,
`routes/mod.rs:179`, and — missed in the first draft — `watcher.rs:1153-1156`, which carries the
same comment and the same dodge. Each pinned a literal suffix specifically to avoid resolution;
with the seam, each declares a locator instead. That restores the original intent:
`add_files_emits_finished_before_queue_updated` is about event ordering and should exercise the
default suffix template, not a special-cased one.

**Four existing tests must declare a world. [R]** These reach a fallback today and pass in both
environments by luck; under the strict default each needs an explicit locator:

| Test | Location | Route to the fallback |
|---|---|---|
| `purge_bad_sources_destroys_through_the_ctx_disposer` | `queue_ops.rs:2820` | `purge_bad_sources` at `:2835` |
| `purge_bad_sources_with_no_ids_returns_an_empty_array` | `routes/mod.rs:396` | route → `queue.rs:126` → unconditional resolve |
| `detect_handbrake_smoke_returns_200_with_valid_json` | `routes/mod.rs:605-618` | `/api/handbrake/detect` → `resolve_handbrake_path` |
| `add_files_inner_skips_at_target_source_end_to_end` | `queue_ops.rs:2343` (`#[ignore]`d) | `skip_by_source_media` → `get_handbrake_path` at `:823` |

The last one is `#[ignore]`d but reaches the fallback *through the seam* rather than by calling
`detect_handbrake_path()` directly, so unlike the other ignored tests it cannot simply be left
alone — it would panic under the fixture default when run locally.

**One test is added outside the intake path. [R]** The mid-encode absent arm
(`converter.rs:876-889`: "HandBrakeCLI not found" → `record_job_error`, Environment class) is
untested today and is the one place the queue actually consumes a missing-HandBrake answer.
`AbsentLocator` makes it expressible for the first time; skipping it would leave the spec's own
framing ("that asymmetry is the defect") unhonored at the site that matters most.

**Verification, run rather than asserted:**

1. Full workspace suite with HandBrakeCLI present.
2. Full workspace suite with HandBrakeCLI stripped from PATH.
3. Results must be identical. That is the evidence the ambient dependency is gone — not the claim
   that it is.

**Mutation check** (per the practice recorded on PR #111): swap the fixture default from
`PanickingLocator` back to a real `PathLocator` and confirm the new absent-case tests go red. A
guard that cannot fail is not a guard.

## Out of scope

- **The `#[ignore]`d tests that call `detect_handbrake_path()` directly** (`converter.rs:2591`,
  `probe.rs:377`). They want a real binary and a real `ffmpeg`; they are `#[ignore]`d so they never
  run in CI. Routing them through the seam would remove the point of them. Note this does **not**
  cover `queue_ops.rs:2343`, which is also `#[ignore]`d but reaches the fallback *through* the seam
  and so must declare a world (see Testing). **[R]**
- **A stripped-PATH script, an env-var backdoor, or a new CI job.** Once the suite has no ambient
  dependency, there is nothing left for a PATH sweep to catch. Adding one anyway would encode the
  workaround as permanent infrastructure.
- **`ffmpeg` coupling.** Only the `#[ignore]`d tests use it, and they are opt-in by construction.
- **Present/absent pairs at the 8 command/route callers.** They get the seam but keep their current
  tests; they are thin adapters over `resolve_handbrake_path`, already covered indirectly. The
  intake path is where the defect occurred, and mid-encode gets one test (see Testing) because it
  is the only site that consumes a missing-HandBrake answer.
- **The lock-discipline inconsistency.** `purge_bad_sources` resolves outside the DB mutex per the
  R3 intent at `queue_ops.rs:92-97`; `add_files_inner` and `process_queue` resolve under it. The
  seam preserves both behaviors exactly as they are. Reconciling them is a real cleanup but not a
  testability change. **[R]**

## Migration and compatibility

None. `Ctx::new` gains a parameter; both heads pass `PathLocator`, which calls
`detect_handbrake_path()` exactly as today. No behavior change ships to users — this is a
testability seam, and production resolution is byte-for-byte what it was.

## Verification (Task 6, 2026-07-28)

Ran `cargo test --workspace` twice at `af6f90b`: once with HandBrakeCLI present at
`/opt/homebrew/bin/HandBrakeCLI`, once with that directory stripped from `PATH` (confirmed absent
via `which HandBrakeCLI` in the stripped environment). Every crate's `test result` line was
identical between the two runs:

- `convertbar-core` (unit): `259 passed; 0 failed; 4 ignored` — both runs
- `convertbar-desktop` / `src-tauri` (unit): `47 passed; 0 failed; 0 ignored` — both runs
- `convertbar-server`: `93 passed; 0 failed; 0 ignored` — both runs
- three doctest/integration harnesses: `0 passed; 0 failed; 0 ignored` — both runs (unchanged)

Total: **399 passed / 0 failed / 4 ignored**, matching in both worlds. That identity is the
evidence the ambient PATH read is gone from every test that runs in CI.

Mutation check, both claims exercised separately (each reverted with `git checkout` immediately
after, nothing committed):

- **Mutation A** (does the absent-world test depend on absence?): swapped
  `Arc::new(AbsentLocator)` for `Arc::new(PathLocator)` in
  `add_files_inner_reports_handbrake_missing_when_the_suffix_needs_a_probe`. Result: **FAILED**, as
  predicted — with HandBrake genuinely installed, the real locator resolves it and the file gets
  queued instead of erroring, so the `expect_err` panics.
- **Mutation B** (does `PanickingLocator` actually fire?): removed the explicit
  `Arc::new(AbsentLocator)` world declaration from
  `purge_bad_sources_destroys_through_the_ctx_disposer`, falling back to the `test_ctx` fixture
  default. Result: **FAILED**, as predicted — panicked with the `PanickingLocator` message
  ("Declare the world explicitly: `Arc::new(AbsentLocator)` ... or `Arc::new(StubLocator(path))`
  ...").

Both mutations produced the predicted failure, so the guard is not decorative.

**Static confirmation, stronger than the two runtime suite runs above:**
`grep -rn "PathLocator\|detect_handbrake_path" crates src-tauri/src --include=*.rs` shows every
test-side use of either name inside an `#[ignore]`d test —
`converter.rs:2537,2540` (`process_queue_drives_a_real_encode_from_queued_to_done`),
`converter.rs:2639` (`real_handbrake_flags_a_truncated_source_and_spares_the_original`),
`queue_ops.rs:2394,2398` (`add_files_inner_skips_at_target_source_end_to_end`), and
`probe.rs:377` (`probe_source_reads_real_clip`) — plus `PathLocator`'s own definition/impl in
`handbrake.rs` and the two production injection sites (`convertbar-server/src/main.rs:44`,
`src-tauri/src/lib.rs:96`). No non-`#[ignore]`d test can reach `detect_handbrake_path()` by any
route.

Frontend: `git diff --stat f3ca8b4 -- src/` (merge base with `origin/main`) is empty — no frontend
files touched by this branch. `npm test` passes **206/206**.
