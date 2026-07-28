# HandBrake Locator Seam — Design

## Problem

Five tests shipped a hidden dependency on HandBrakeCLI being installed. They passed on the
developer machine (Homebrew has it at `/opt/homebrew/bin/HandBrakeCLI`) and failed on CI, which
has no HandBrake. One was caught by CI during PR #130; a stripped-PATH sweep found the other four.

Both were patched by pinning a literal suffix in the fixture so the intake path never needs to
resolve HandBrake at all (`crates/convertbar-core/src/queue_ops.rs:1643`,
`crates/convertbar-server/src/routes/mod.rs:179`). Those patches fix two instances. They do not
close the class — the next test to reach the intake path with default settings reacquires the
same dependency, and the developer machine hides it again.

### Why the tests could not simply cover both worlds

Three functions triplicate the same resolution logic:

| Function | Signature | Detection fallback | Callers |
|---|---|---|---|
| `handbrake::resolve_handbrake_path` | `&Ctx` | `handbrake.rs:322` | 8 — server routes + tauri commands |
| `queue_ops::get_handbrake_path` | `&Connection` | `queue_ops.rs:104` | 2 — `add_files_inner`, a test helper |
| `converter::get_handbrake_path` | `&Connection` | `converter.rs:452` | 1 — `process_queue` |

All three have the identical shape:

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
(`ctx.rs:12-13`):

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
triplicated. It **replaces** `queue_ops::resolve_from_configured` (`queue_ops.rs:99-105`), which is
deleted along with the two now-redundant private wrappers:

```rust
/// The configured path if it points at an existing file, otherwise the locator's answer.
/// Filesystem/subprocess work only — no DB access — so this is meant to run OUTSIDE the DB mutex.
pub fn resolve_with_locator(
    configured: Option<&str>,
    locator: &dyn HandbrakeLocator,
) -> Option<String>
```

Callers that need the current `Result<String, String>` shape keep their own
`.ok_or_else(|| "HandBrakeCLI not found".to_string())`, so no error text changes.

The two `&Connection`-shaped wrappers take the locator as a **second argument** rather than
switching to `&Ctx`. This is deliberate: `queue_ops.rs:92-97` documents that these run outside the
DB mutex precisely so a `which` spawn never happens under the lock (R3 — it previously ran per id,
under the lock, up to 2N blocking spawns per batch). Handing them `&Ctx` would put a `db` handle
back in scope at exactly the site that must not touch it. Both callers (`add_files_inner`,
`process_queue`) already have `ctx` in scope, so passing `&*ctx.handbrake` costs nothing.

`handbrake::resolve_handbrake_path` already takes `&Ctx`, so its 8 callers are unaffected.

### Test doubles

Three, mirroring the existing `TestSink` / `RecordingDisposer` idiom:

| Double | Behavior | Use |
|---|---|---|
| `PanickingLocator` | Panics when consulted, naming the expectation | **The fixture default** |
| `AbsentLocator` | Returns `None` | The CI world — newly expressible |
| `StubLocator(String)` | Returns a fixed path | HandBrake-installed world without a real binary |

`PanickingLocator` as the default is the same tactic as `LockProbeSink` in `control.rs`: make a
silent regression loud. A future test that wanders onto the intake path without declaring a world
fails immediately with a clear message, instead of quietly reading the developer's machine and
passing for the wrong reason.

The evidence says this default is cheap: the converter tests pin `handbrake_path` and never reach
the fallback, so they need no change. Only tests that genuinely fall through must declare a world —
which is exactly the coupled set.

## Testing

**Present/absent pairs on the intake path** — the path that actually broke:

- `add_files_inner` with a `{...}` suffix template and `AbsentLocator` → the documented
  `"HandBrakeCLI not found"` error, no panic, no silent success.
- The same with `StubLocator` → resolution proceeds.
- The server add-files route, both worlds, asserting the HTTP status rather than a 500.

**The two pinned-suffix workarounds are removed.** `queue_ops.rs:1643` and `routes/mod.rs:179`
pinned a literal suffix specifically to dodge resolution. With the seam, each declares a locator
instead, which restores the original intent of both tests — `add_files_emits_finished_before_queue_updated`
is about event ordering, and it should exercise the default suffix template, not a special-cased one.

**Verification, run rather than asserted:**

1. Full workspace suite with HandBrakeCLI present.
2. Full workspace suite with HandBrakeCLI stripped from PATH.
3. Results must be identical. That is the evidence the ambient dependency is gone — not the claim
   that it is.

**Mutation check** (per the practice recorded on PR #111): swap the fixture default from
`PanickingLocator` back to a real `PathLocator` and confirm the new absent-case tests go red. A
guard that cannot fail is not a guard.

## Out of scope

- **The `#[ignore]`d local-only integration tests** (`converter.rs:2591`, `probe.rs:377`) keep
  calling `detect_handbrake_path()` directly. They want a real binary and a real `ffmpeg`; they are
  `#[ignore]`d so they never run in CI. Routing them through the seam would remove the point of them.
- **A stripped-PATH script, an env-var backdoor, or a new CI job.** Once the suite has no ambient
  dependency, there is nothing left for a PATH sweep to catch. Adding one anyway would encode the
  workaround as permanent infrastructure.
- **`ffmpeg` coupling.** Only the `#[ignore]`d tests use it, and they are opt-in by construction.
- **Present/absent pairs at every resolve site.** `process_queue`'s mid-encode resolution and the 8
  command/route callers get the seam but keep their current tests. Several are already covered
  indirectly, and the intake path is where the defect actually occurred.

## Migration and compatibility

None. `Ctx::new` gains a parameter; both heads pass `PathLocator`, which calls
`detect_handbrake_path()` exactly as today. No behavior change ships to users — this is a
testability seam, and production resolution is byte-for-byte what it was.
