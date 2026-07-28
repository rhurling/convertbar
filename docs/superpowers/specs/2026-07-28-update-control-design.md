# Update Control — Design

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

That body is what `tauri-action` writes into `latest.json`'s `notes` field, which is the only release-note text the updater plugin exposes (`Update::body`). So "show the changelog before installing" is blocked on the release pipeline producing a real changelog first. `release.sh --notes` does not help — it feeds the *PR* body (`scripts/release.sh:139-146`), never the release.

**3. Update state is invisible.** No last-checked time, no "what version am I on vs. what's available", and check/install failures surface only as a transient OS notification or a toast that clears after 5 seconds.

**4. The check runs once, in `setup()`.** A menu bar app stays running for weeks. An always-on user never hears about a release until they happen to restart the app.

**5. An install can land mid-encode.** The startup path calls `download_and_install` unconditionally — it will swap the `.app` bundle while HandBrakeCLI is running underneath it. The Settings button *does* guard this (`SettingsPage.tsx:459-473`: `pause_after_current`, wait for idle, then relaunch); the startup path does not.

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

## Settings and state

Two new rows in the `settings` table. Both additive, so an existing `convertbar.db` upgrades safely.

| key | values | default |
|---|---|---|
| `update_mode` | `automatic` \| `notify` \| `off` | `automatic` |
| `update_skipped_version` | version string, or `""` | `""` |

`update_mode` is normalized on read, following the existing `normalize_bad_source_action` pattern (`src-tauri/src/commands/settings.rs:135`): anything not exactly `notify` or `off` reads as `automatic`. The fallback is the default rather than the most conservative value — an unreadable setting should leave the user updating, not silently strand them on an old version.

Both keys go in `ALLOWED_KEYS` (`settings.rs:112-130`), the `Settings` struct (`src-tauri/src/types.rs:22-40`), and the seed block in `db.rs`. The seeded-settings **count guard** at `db.rs:258-261` must be updated — it exists precisely to catch a key added without thinking about migration.

Everything else is in-memory in an `UpdaterState`, not persisted:

- last-checked timestamp
- the available update (version, date, notes)
- last error
- whether an install is deferred waiting for the queue to go idle

The startup check populates all of it within seconds of launch, so persisting buys nothing and avoids a schema change.

## `src-tauri/src/updater.rs`

New module owning every policy decision. Commands live in `src-tauri/src/commands/updater.rs`, matching the existing `commands/` layout.

```rust
pub enum UpdateMode { Automatic, Notify, Off }

pub enum UpdateStatus { Idle, Checking, Available, Downloading, ReadyToRestart, Error }

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
    last_checked: Option<i64>,   // unix seconds
    last_error: Option<String>,
}
```

**Commands:** `get_update_state`, `check_for_update`, `install_update`, `skip_update_version`. Mode changes go through the existing `update_setting` — no new command for that.

**Event:** `update-state` carrying the full `UpdateState`, emitted on every transition. The frontend listens and re-renders; it never polls.

**Scheduler:** `updater::start(app)` replaces the inline block at `lib.rs:386-406`. It checks at startup, then every 24 hours, **re-reading `update_mode` on each tick** so a mode change takes effect without restarting the app.

App-defined `#[tauri::command]` functions are ACL-exempt, so none of these need a `capabilities/default.json` entry.

## Policy

| | Automatic | Notify | Off |
|---|---|---|---|
| Scheduled check (startup + 24h) | yes | yes | **no** |
| On update found | install once idle, notify after | badge + one notification, show notes | — |
| `update_skipped_version` honored | **no** | yes | — |
| Manual "Check now" | yes | yes | **yes** |

Four rules that are easy to get wrong:

**Skip applies only in Notify mode.** In Automatic the user has delegated the decision, so a skip list there is incoherent. A version *newer* than the skipped one surfaces again — skip means "not this one", not "stop telling me".

**Notify notifies once per version.** The badge and panel persist until acted on; the OS notification does not repeat on every 24h tick. Re-notify only when the available version differs from the one already notified about.

**The manual button never auto-installs (U7).** In every mode, "Check now" ends at *showing* the result. In Automatic mode this means the button behaves like Notify — deliberate, since a button labelled "check" should not commit the user to anything.

**The badge follows status, not mode.** If a manual check in Off mode finds an update, status becomes `Available` and the Settings-tab dot appears. Off suppresses *automatic checking*, not the display of a result the user explicitly asked for.

**No install ever runs mid-encode.** Automatic or user-initiated, the install is gated on the queue being idle:

- *Automatic, queue busy*: hold the install. Retry when the queue next reaches idle, hooked into the `menu-bar-update` listener already running at `lib.rs:239` — not on the next 24h tick, which is far too coarse.
- *User-initiated, queue busy*: keep today's behavior — `pause_after_current`, wait for idle, then install and relaunch — but say so in the UI rather than silently pausing the user's queue.

In Automatic mode the notes of the version just installed are retained in `UpdateState`, so the panel can show "What's new in X" after the fact. The changelog is not lost in Automatic mode, just shown after the install rather than before.

## Frontend

New `src/hooks/useUpdate.ts` and `src/components/UpdatePanel.tsx`, following the existing hook/component split. `SettingsPage.tsx` is already 501 lines; this adds roughly 100 more, so it is extracted rather than grown.

**`UpdatePanel`** replaces the current Updates group (`SettingsPage.tsx:446-489`):

- Header: "Updates" + current version.
- Mode: three radios, using the same markup pattern as the existing `cleanup_mode` and `bad_source_action` radio groups. Label-to-value mapping is fixed: **Automatic** → `automatic`, **Notify me** → `notify`, **Manual only** → `off`.
- State line: "Last checked 3 minutes ago", "Checking…", or the error text. Errors render inline and persist; they do not clear on a timer.
- When an update is available: version, date, a scrollable plain-text notes box with a bounded max-height, and two buttons — `Install and restart`, `Skip this version`.
- When Automatic has just installed: "What's new in X" with the notes and a `Restart now` button.
- `Check now` is always present, in every mode.

**`TabBar`** gets a dot badge on the Settings tab when status is `Available`, so a missed OS notification is not the only signal.

Notes are rendered as plain text in a `pre`-style box (U6) — no markdown dependency, no `dangerouslySetInnerHTML`.

## CI: making the notes exist

Replace the "Build release body" step (`build.yml:43-52`) with a call to a new `scripts/release-notes.sh <prev-tag> <current-tag>`, kept as a script so it is testable — precedent: `src/test/release-script.test.ts` tests `release.sh`.

Grouping, from `git log PREV..CURRENT --pretty=format:%s`:

- `feat:` → **Features**
- `fix:` → **Fixes**
- `perf:` → **Performance**
- everything else (`chore`, `docs`, `test`, `refactor`, `ci`, and unprefixed subjects) → collapsed into a single **Maintenance** line with a count, not enumerated
- footer: the existing `**Full changelog**: …compare/PREV...CURRENT` link, preserved

**Gotcha:** the current `echo "body=..." >> $GITHUB_OUTPUT` form silently truncates multi-line values. Multi-line output requires the heredoc delimiter form:

```bash
{
  echo "body<<EOF"
  ./scripts/release-notes.sh "$PREV" "$CURRENT"
  echo "EOF"
} >> "$GITHUB_OUTPUT"
```

The first release built after this change is the first one whose notes the app can display. Releases up to and including 1.0.0 have only the compare link, which the panel renders as-is.

## Cleanup that falls out of U5

With policy in Rust, the frontend stops importing the updater and process plugins entirely. `src/pages/SettingsPage.tsx:4-5` are the only consumers; `SettingsPage.test.tsx:11-12` the only mocks.

- Remove `updater:allow-check`, `updater:allow-download-and-install`, and `process:allow-restart` from `src-tauri/capabilities/default.json`. Restart moves to Rust's `app.restart()`.
- `npm uninstall @tauri-apps/plugin-updater @tauri-apps/plugin-process` — the JS halves only. The Rust crates stay. This is exactly the frontend-half-unused case `CLAUDE.md` describes; `npm run tauri remove` would rip out the still-needed Rust side.
- Delete the two `vi.mock` lines in `SettingsPage.test.tsx`.

## Testing

**Rust**

- `normalize_update_mode` fallback table, mirroring the `normalize_bad_source_action` test at `settings.rs:244-257`: known values map through, unknown/empty/wrong-case fall back to `automatic`.
- Skip-version behavior across all three modes: skipped version suppressed in Notify; a newer version resurfaces; skip ignored in Automatic.
- Notify fires one notification per version, not one per tick.
- **The idle gate is load-bearing.** An install must not start while a job is encoding. Per the project's mutation-check rule, this test is verified by deleting the guard and confirming it goes red — a passing test that cannot fail when the guard is removed is worthless here, and this is the path that can corrupt a running encode.
- `db.rs` seed-count guard updated for the two new keys.

**Frontend**

- `useUpdate`: state transitions driven by the `update-state` event.
- `UpdatePanel`: notes render, mode radios write through `update_setting`, Install and Skip call their commands, error text persists rather than clearing.
- `TabBar`: badge appears on `Available` and clears on skip/install.

**CI script**

- `release-notes.sh`: grouping by type, the Maintenance collapse, an empty commit range, and multi-line output surviving `$GITHUB_OUTPUT`.

## Migration and compatibility

- Both new settings are additive rows; an existing `convertbar.db` needs no schema change.
- Default `automatic` (U4) reproduces today's behavior, so a 1.0.0 user upgrading sees no change in *when* updates install — only the added mid-encode guard, the panel, and the notes.
- `latest.json`'s shape is unchanged; only the content of its `notes` field improves.

## Out of scope

- A pre-release / beta channel.
- A hand-maintained `CHANGELOG.md` in the repo.
- Rendering markdown in the notes box (U6).
- Download progress as a percentage — status transitions (`Downloading` → `ReadyToRestart`) are enough for a bundle this size.
