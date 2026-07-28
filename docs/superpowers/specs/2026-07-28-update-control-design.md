# Update Control — Design

> Revised after an adversarial review (2026-07-28). Findings that changed the design are
> marked **[R]** at the relevant section.

## Problem

ConvertBar updates itself without ever asking, and never tells the user what changed.

**1. Both update paths install without consent.**

- **Startup** (`src-tauri/src/lib.rs:386-406`): on every launch, silently downloads *and* installs any available update, then fires an OS notification — "Updated to X — restart ConvertBar to apply". The user learns about the update only after it has already landed.
- **Settings button** (`src/pages/SettingsPage.tsx:449-486`): "Check for updates" goes straight to `check()` → `downloadAndInstall()` → `relaunch()`. Pressing a button labelled *check* commits the user to an install.

The silent-install behavior was a deliberate call — D5 in `docs/fable-review/DECISIONS.md:49`, decided 2026-07-07, chosen as the lowest-friction option. This design revisits it: not to reverse it, but to make it one of three modes the user picks between.

**2. There is nothing to show even if we asked.** `.github/workflows/build.yml:43-52` builds the GitHub release body as a single line:

```
**Full changelog**: https://github.com/rhurling/convertbar/compare/v0.19.1...v1.0.0
```

That body is what `tauri-action` writes into `latest.json`'s `notes` field, which is the only release-note text the updater plugin exposes (`Update::body`, `tauri-plugin-updater-2.10.1/src/updater.rs:602-615`).

**Verified empirically** — the published v1.0.0 artifact confirms the release body reaches `notes` verbatim:

```
$ gh release download v1.0.0 --pattern latest.json
version: 1.0.0
notes: '**Full changelog**: https://github.com/rhurling/convertbar/compare/v0.19.1...v1.0.0'
```

So "show the changelog before installing" is blocked on the release pipeline producing a real changelog first, and only on that. `release.sh --notes` does not help — it feeds the *PR* body (`scripts/release.sh:139-146`), never the release.

**3. Update state is invisible.** No last-checked time, no "what version am I on vs. what's available", and check/install failures surface only as a transient OS notification or a toast that clears after 5 seconds.

**4. The check runs once, in `setup()`.** A menu bar app stays running for weeks. An always-on user never hears about a release until they happen to restart the app.

**5. On Windows, an install mid-encode kills the app and orphans the encoder. [R]**

The install mechanism is per-platform, and only one platform is dangerous:

| Platform | `install_inner` behavior | Mid-encode risk |
|---|---|---|
| **Windows** (`updater.rs:787-865`) | Launches the NSIS/MSI installer via `ShellExecuteW`, then **`std::process::exit(0)`**. `download_and_install` never returns; the *installer* relaunches the app. | **Severe** |
| macOS (`updater.rs:1217+`) | `fs::rename` swap of the `.app`; returns normally, app keeps running | Benign |
| Linux (`updater.rs:968+`) | AppImage/deb/rpm install; returns normally | Benign |

`std::process::exit(0)` appears exactly once in the crate — line 865, Windows only. It bypasses the `RunEvent::ExitRequested` handler at `lib.rs:418-421` that calls `kill_active_child`. Per that handler's own comment, an orphaned HandBrakeCLI keeps encoding into the partial output for hours, and the next launch's auto-resume then deletes that file and starts a second encoder against the same path while the orphan still holds it.

HandBrakeCLI is an external binary resolved from `PATH`, not bundled inside the app, so the macOS bundle swap does **not** disturb a running encode. The hazard is Windows process death, not bundle replacement.

**The current Settings button does not guard this.** `SettingsPage.tsx:457` runs `await update.downloadAndInstall()` *before* the queue check at line 459. Only the **relaunch** is deferred. Today, both paths install mid-encode; one merely delays the restart.

## Decisions

Recorded here so the implementation does not relitigate them.

| # | Decision |
|---|---|
| U1 | Three modes — **Automatic / Notify / Off** — not a single on/off checkbox. |
| U2 | Release notes are **auto-generated in CI** from conventional commits, not hand-written in a CHANGELOG.md and not fetched from the GitHub API at runtime. |
| U3 | A pending update surfaces in the **Settings panel plus a dot badge on the Settings tab**. No banner over the other pages. |
| U4 | Default mode is **Automatic**, for new installs and for users upgrading from 1.0.0 alike. Nobody's updates stop because they never opened Settings. |
| U5 | **Rust owns all update policy.** The frontend is pure presentation. |
| U6 | Notes render as **plain text**, not markdown. A renderer is a new dependency and an injection surface for content that is flat grouped bullets. |
| U7 | The manual "Check now" button **never auto-installs, in any mode.** This changes today's behavior and is intentional. |
| U8 | **[R]** The install flow is specified **per platform**. Windows terminates the process on install; the design must not assume the post-install code path runs. |
| U9 | **[R]** Anything the user must still see *after* the restart that applies an update is **persisted**, not held in memory. |

## Settings and state

### Persisted

One user-facing setting, and three internal updater keys. **[R]**

| key | values | default | in `ALLOWED_KEYS` / `Settings`? |
|---|---|---|---|
| `update_mode` | `automatic` \| `notify` \| `off` | `automatic` | **yes** — user-facing |
| `update_skipped_version` | version string | unset | **no** |
| `update_notified_version` | version string | unset | **no** |
| `update_installed` | `version\|notes` of the update just installed | unset | **no** |

Only `update_mode` is a user setting: it joins `ALLOWED_KEYS` (`settings.rs:112-130`), the `Settings` struct (`types.rs:22-40`), and the `db.rs` seed block — so the seeded-settings **count guard at `db.rs:260-263` goes from 17 to 18**.

The other three are updater-internal state that happens to need durability. They are **read-with-default, never seeded**, following the `queue_paused` precedent, and are written only by `updater.rs` — never through `update_setting`. Keeping them out of `ALLOWED_KEYS` is what preserves U5: the frontend physically cannot write update policy state, it can only call `skip_update_version`. **[R]** — the earlier draft both exposed the key and added the command, giving two write paths for the same fact.

`update_mode` is normalized on read, following `normalize_bad_source_action` (`settings.rs:135`): anything not exactly `notify` or `off` reads as `automatic`. The fallback is the default rather than the most conservative value — an unreadable setting should leave the user updating, not silently strand them on an old version. (This deliberately inverts the `bad_source_action` precedent, where an unreadable value must not escalate to permanent deletion.)

### Why three keys must be persisted [R]

The first draft held all of this in memory, on the reasoning that the startup check repopulates it within seconds. That breaks three approved behaviors:

- **`update_notified_version`** — "Notify notifies once per version" only held within a single process lifetime. A user who declines an update and restarts daily gets notified daily for the same version.
- **`update_installed`** — in Automatic mode the "What's new in X" notes were wiped by the very restart that applies the update: the one moment the user is actually on version X and would read them.
- **`update_skipped_version`** — already persisted in the first draft; the asymmetry with the other two was accidental, not designed.

### In-memory

Held in `UpdaterState`, not persisted: last-checked timestamp, the available update, last error, and the deferred-install flag. These are all re-derivable, and none of them needs to survive the restart.

## `src-tauri/src/updater.rs`

New module owning every policy decision. Commands live in `src-tauri/src/commands/updater.rs`, matching the existing `commands/` layout.

```rust
pub enum UpdateMode { Automatic, Notify, Off }

pub enum UpdateStatus {
    Idle,
    Checking,
    Available,
    Downloading,
    WaitingForIdle,     // [R] downloaded, install deferred until the queue drains
    ReadyToRestart,     // macOS/Linux only — unreachable on Windows (U8)
    Error,
}

pub struct AvailableUpdate {
    version: String,
    date: Option<String>,
    notes: Option<String>,
}

pub struct UpdateState {
    mode: UpdateMode,
    status: UpdateStatus,
    current_version: String,
    available: Option<AvailableUpdate>,
    just_installed: Option<AvailableUpdate>,  // [R] from persisted `update_installed`
    last_checked: Option<i64>,                // unix seconds, wall clock
    last_error: Option<String>,
}
```

**Commands:** `get_update_state`, `check_for_update`, `install_update`, `skip_update_version`, and **`restart_app`** **[R]** — wrapping `AppHandle::restart()` (verified present, `tauri-2.11.5/src/app.rs:588`). The first draft specified a "Restart now" button while simultaneously removing `relaunch()` from the frontend and dropping `process:allow-restart`, leaving the button unimplementable.

Calling `restart()` off the main thread routes through `ExitRequested` with `RESTART_EXIT_CODE`, so the `kill_active_child` handler at `lib.rs:418` still runs.

**Event:** `update-state` carrying the full `UpdateState`, emitted on every transition. The frontend listens and re-renders; it never polls.

**Scheduler:** `updater::start(app)` replaces the inline block at `lib.rs:386-406`.

**The tick is hourly and compares wall-clock time, not a 24h sleep. [R]** A `tokio` sleep is backed by `Instant` — `CLOCK_UPTIME_RAW` on macOS — which stops while the machine is asleep. A laptop that sleeps nightly would stretch "every 24 hours" into many days of wall clock, defeating problem #4, which is the entire reason the periodic check exists. Instead: tick hourly, and check only when `now - last_checked >= 24h`. `update_mode` is re-read on each tick.

**`on_before_exit` must be registered. [R]** `tauri_plugin_updater::Builder::new().on_before_exit(...)` (`updater.rs:288-290`, invoked at `updater.rs:837-839`) runs immediately before the Windows `process::exit(0)`. Wire it to `kill_active_child`. The idle gate below is the primary defense; this is the backstop for when the gate races, and it is the only thing standing between a Windows install and an orphaned encoder.

## Policy

| | Automatic | Notify | Off |
|---|---|---|---|
| Scheduled check (startup + hourly tick, 24h min interval) | yes | yes | **no** |
| On update found | install once idle, notify after | badge + one notification, show notes | — |
| `update_skipped_version` honored | **no** | yes | — |
| Manual "Check now" | yes | yes | **yes** |

**Skip applies only in Notify mode.** In Automatic the user has delegated the decision, so a skip list there is incoherent. A version *newer* than the skipped one surfaces again — skip means "not this one", not "stop telling me".

**Notify notifies once per version**, across restarts, via persisted `update_notified_version`. The badge and panel persist until acted on; the OS notification fires only when the available version differs from the one already recorded.

**The manual button never auto-installs (U7).** In every mode, "Check now" ends at *showing* the result. In Automatic mode this means the button behaves like Notify — deliberate, since a button labelled "check" should not commit the user to anything.

**The badge follows status, not mode.** If a manual check in Off mode finds an update, status becomes `Available` and the Settings-tab dot appears. Off suppresses *automatic checking*, not the display of a result the user explicitly asked for.

### The idle gate [R]

No install runs while a job is encoding. The motivation is Windows-specific (problem #5), but the gate applies on every platform — uniform behavior is easier to reason about and to test than a `cfg`-gated invariant, and the cost on macOS/Linux is only a delayed install.

The first draft's mechanism was wrong in three ways. The corrected design:

**1. Download and install are separate steps.** Download freely; gate only the install. `Update::download()` and `Update::install()` exist separately, so this needs no new machinery.

**2. Re-check idle immediately before `install()`, under the same discipline `run_queue` uses, and hold an `installing` flag that `run_queue` and the watcher respect.** A download takes minutes, and `watcher.rs:462` starts `run_queue` whenever a watched file lands, so "idle when the download began" says nothing about idle when it finishes. Without the interlock this is a check-then-act race of exactly the kind the project's atomic-job-claim invariant exists to prevent.

**3. The retry trigger is a state re-check, not an event string.** The first draft said "retry when `menu-bar-update` reports idle". That is unreliable:

- `final_run_status` (`converter.rs:704-710`) emits `"error"`, not `"idle"`, for any run in which a job failed — so a queue that ends with one failure would never retry.
- The emit happens *inside* `process_queue`, **before** `RunningGuard` clears `is_running` (`converter.rs:712-721`) — so a listener that immediately re-checks `is_running` sees `true` and re-defers.
- `"idle"` is also emitted by `pause_after_current` (`converter.rs:1272-1284`) and the low-disk pause (`converter.rs:795-818`) with jobs still queued — the event does not mean "queue empty".

So: wake on **both** `"idle"` and `"error"`, then re-evaluate actual queue state (`is_running` plus no active child) rather than trusting the event, and re-arm if the state says otherwise.

**User-initiated install with a busy queue:** download → `pause_after_current` → wait for idle → install → restart. **[R]** — not "keep today's behavior", which installs first (see problem #5).

### Windows install flow [R]

On Windows, `install()` does not return. Therefore:

- `ReadyToRestart` is unreachable; the installer relaunches the app itself.
- The post-install notification and the "What's new in X" panel cannot be driven by code after the install call. Both must be **written to `update_installed` *before* invoking `install()`**, so the panel renders correctly after the installer-driven relaunch.
- `restart_app` is never needed on this path.

Writing `update_installed` before the install call is correct on all three platforms, so this is one code path, not a `cfg` fork. If the install fails on macOS/Linux, the key is cleared in the error branch.

## Frontend

New `src/hooks/useUpdate.ts` and `src/components/UpdatePanel.tsx`, following the existing hook/component split. `SettingsPage.tsx` is already 501 lines; this adds roughly 100 more, so it is extracted rather than grown.

**`UpdatePanel`** replaces the current Updates group (`SettingsPage.tsx:446-489`):

- Header: "Updates" + current version.
- Mode: three radios, same markup pattern as the existing `cleanup_mode` and `bad_source_action` groups. Label-to-value mapping is fixed: **Automatic** → `automatic`, **Notify me** → `notify`, **Manual only** → `off`.
- State line: "Last checked 3 minutes ago", "Checking…", or the error text. Errors render inline and persist; they do not clear on a timer.
- `Available`: version, date, a scrollable plain-text notes box with a bounded max-height, and two buttons — `Install and restart`, `Skip this version`.
- `WaitingForIdle`: "Update downloaded — will install when the queue finishes", so a deferred install is visible rather than looking like nothing happened.
- `just_installed` present: "What's new in X" with the notes, and `Restart now` (macOS/Linux; on Windows the app has already been relaunched by the installer, so the panel shows the notes without the button).
- `Check now` is always present, in every mode.

**`TabBar`** gets a dot badge on the Settings tab when status is `Available`.

Notes render as plain text in a `pre`-style box (U6) — no markdown dependency, no `dangerouslySetInnerHTML`.

**Mode changes take effect immediately. [R]** `update_setting` gets a per-key hook for `update_mode`, following the existing precedent for `launch_at_login` and `watch_skip_marker` (`settings.rs:161-173`). Without it, a user who sees "update available" and switches to Automatic would wait up to an hour for the next tick.

## CI: making the notes exist

Replace the "Build release body" step (`build.yml:43-52`) with a call to a new `scripts/release-notes.sh <prev-tag> <current-tag>`, kept as a script so it is testable — precedent: `src/test/release-script.test.ts` tests `release.sh`.

Grouping, from `git log PREV..CURRENT --pretty=format:%s`:

- `feat:` → **Features**
- `fix:` → **Fixes**
- `perf:` → **Performance**
- everything else (`chore`, `docs`, `test`, `refactor`, `ci`, and unprefixed subjects) → collapsed into a single **Maintenance** line with a count, not enumerated — otherwise every release body is dominated by dependabot commits
- footer: the existing `**Full changelog**: …compare/PREV...CURRENT` link, preserved
- the existing empty-`PREV` → `"Initial release."` branch (`build.yml:51-53`) is preserved **[R]**

**Multi-line output. [R]** The current `echo "body=..." >> $GITHUB_OUTPUT` form does not silently truncate — a continuation line without `=` makes the runner **fail the step with "Invalid format"**, and any notes line containing `=` would be parsed as a bogus additional output. The heredoc form is required:

```bash
{
  echo "body<<CONVERTBAR_NOTES_EOF"
  ./scripts/release-notes.sh "$PREV" "$CURRENT"
  echo "CONVERTBAR_NOTES_EOF"
} >> "$GITHUB_OUTPUT"
```

Consumer side needs no change: `releaseBody: ${{ steps.release_body.outputs.body }}` (`build.yml:94`) substitutes after YAML parse, so multi-line values pass through intact.

The first release built after this change is the first whose notes the app can display. Releases up to and including 1.0.0 carry only the compare link, which the panel renders as-is.

**Out of scope, documented:** `PREV=$(git tag --sort=-version:refname | grep '^v' | sed -n '2p')` assumes the tag being built is the highest version; tagging a patch *below* an existing higher tag would yield `PREV == CURRENT` and an empty range. `release.sh` already refuses a version that is not newer than the current one, so this is unreachable through the sanctioned release path and is not worth guarding against here.

## Cleanup that falls out of U5

`src/pages/SettingsPage.tsx:4-5` are the only consumers of the two JS plugins; `SettingsPage.test.tsx:11-12` the only mocks (verified repo-wide).

- Remove `updater:allow-check`, `updater:allow-download-and-install`, and `process:allow-restart` from `src-tauri/capabilities/default.json:19-21`. Restart moves to Rust's `app.restart()`, exposed as the `restart_app` command.
- `npm uninstall @tauri-apps/plugin-updater @tauri-apps/plugin-process` — the JS halves only. The Rust crates stay. This is exactly the frontend-half-unused case `CLAUDE.md` describes; `npm run tauri remove` would rip out the still-needed Rust side.
- Delete the two `vi.mock` lines in `SettingsPage.test.tsx`.

Rust-side `updater()` / `UpdaterExt` needs no ACL grant, and app-defined commands are ACL-exempt, so nothing is added to `default.json`.

## Testing

### The install seam is a design requirement, not a test detail [R]

`Update` cannot be constructed in a test — its fields are private (`updater.rs:602-644`) — and `check()` requires the network, which the mock runtime has no updater config for. A test that calls only a pure `should_install(...)` helper would stay green when the guard is deleted from the real call site: the mutation check would pass **vacuously**, which is precisely the failure mode this project has already been bitten by.

So the design mandates a seam: the scheduler takes its install action as an injected trait/closure (`trait Installer { fn install(&self, u: PendingUpdate) -> Result<()> }`), with the real implementation wrapping `Update::install()` and tests supplying a recorder. This makes the decision→action path — not a detached predicate — the thing under test.

### Rust

- `normalize_update_mode` fallback table, mirroring the `normalize_bad_source_action` test at `settings.rs:244-257`.
- Skip-version across all three modes: suppressed in Notify, a newer version resurfaces, ignored in Automatic.
- Notify fires once per version **across a simulated restart** — asserted on persisted `update_notified_version`, not by counting OS notifications, which the mock runtime cannot observe.
- **The idle gate, mutation-checked.** With the recording installer injected and the queue marked busy, no install is recorded; with the queue idle, exactly one is. Verified by deleting the guard and confirming red — the test drives the real scheduler path, so this is not vacuous.
- The interlock: a job starting between download-complete and install does not produce an install.
- `update_installed` is written *before* the install call, so the Windows no-return path still yields a correct post-restart panel.
- `db.rs` seed-count guard updated 17 → 18.

### Frontend

- `useUpdate`: state transitions driven by the `update-state` event.
- `UpdatePanel`: notes render, mode radios write through `update_setting`, Install/Skip/Restart call their commands, `WaitingForIdle` copy appears, error text persists.
- `TabBar`: badge appears on `Available`, clears on skip/install.

### CI script

- `release-notes.sh`: grouping by type, the Maintenance collapse, an empty commit range, the `Initial release.` branch, and multi-line output surviving `$GITHUB_OUTPUT`.

## Migration and compatibility

- `update_mode` is one additive seeded row; the three internal keys are read-with-default and create no rows until written. An existing `convertbar.db` needs no schema change.
- Default `automatic` (U4) reproduces today's *timing* of installs, so a 1.0.0 user sees no change in when updates land — only the added idle gate, the panel, and the notes.
- `latest.json`'s shape is unchanged; only the content of its `notes` field improves.

## Out of scope

- A pre-release / beta channel.
- A hand-maintained `CHANGELOG.md`.
- Rendering markdown in the notes box (U6).
- Download progress as a percentage — status transitions are enough for a bundle this size.
- Hardening the `PREV` tag derivation (see CI section).
