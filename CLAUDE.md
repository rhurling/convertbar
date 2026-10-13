# ConvertBar

Menu bar app for batch video conversion using HandBrakeCLI. Built with Tauri 2 + React + Rust.

## Workspace Layout

Cargo workspace: `crates/convertbar-core` (head-agnostic engine: converter, watcher, queue_ops, control, settings_ops, db — zero tauri deps, enforced by the crate graph) + `src-tauri` (desktop shell: thin `#[tauri::command]` adapters, tray, updater, dialogs, `TauriSink`/`TrashDisposer`) + `crates/convertbar-server` (headless HTTP/SSE head; routes.json is the route contract). The version lives in the root `Cargo.toml` `[workspace.package]`; `release.sh` bumps it there. Run tests with `cargo test --workspace`.

## Emitting Events Under the DB Lock

Never emit a Tauri event while holding `ctx.db`'s lock (`crates/convertbar-core/src/control.rs:80-82` and sibling call sites): the desktop tray listener re-locks `ctx.db` synchronously on the same thread to read settings, and `std::sync::Mutex` is not reentrant, so holding the guard across an emit self-deadlocks. Pause/resume/cancel drop the `db` guard before emitting; `LockProbeSink` (a test double in `control.rs`) fails loud instead of hanging if this regresses. Two shipped deadlocks came from violating it.

## Cleanup Modes and the In-Place Rule

`cleanup_mode` is `trash | delete | keep`, always read through
`settings_ops::read_cleanup_mode` (never a raw column compare); an unrecognized value
normalizes to `trash`.

`keep` and an in-place job (empty suffix, so `output_path == source_path`) are mutually
exclusive, and that is enforced by PREVENTION, not refusal: `add_files_to_db` never
queues such a job, and `update_setting` drops `queued` ones when the mode becomes `keep`.
Only `queued` ones: a `paused` row has a SIGSTOPped child behind it and a converter thread
parked in `child.wait()`, so deleting it strands a live encode whose completion then writes
to nothing — no history row and no fingerprint, which is exactly the re-ingestion loop below.
`process_queue`'s backstop re-reads `cleanup_mode` at the cleanup decision and handles it.
Do not "simplify" this into an error recorded in `process_queue` — an `error` row is
invisible to both `queue_ops::fetch_skip_sets` and `watcher::filter_known_bad_sources`,
so a watched folder would re-queue and re-fail every file on every boot. The
`"keep" => RemoveTemp` arm in `in_place_action` covers the setting-change race and must
stay a real arm, not a `debug_assert!` — the branch it replaces permanently deletes the
user's source on the server head.

Under `keep` the source survives, so re-ingestion protection rests on the `(size, mtime)`
fingerprint in completed rows — and, when a row has none (`file_identity` failed at add
time), on `fetch_skip_sets` falling back to the source path for every unfingerprinted row
while the mode is `keep`, not just for in-place ones. Without that fallback the file has
nothing left to be recognized by and a watched folder re-queues it every pass. Clearing
history removes both signals, so it re-converts kept sources.

## Preset File and Output Container

HandBrakeCLI picks the container from the output's **extension**, never from the preset, so the
extension is the container (ADR 0004): it comes from the preset's `FileFormat` at add time and is
stored in `output_path`. Never pass `--format` and never hard-code `.mp4` again — an MKV preset
named `.mp4` silently writes MP4 and drops every track MP4 cannot carry. The in-place temp keeps
the source's extension for the same reason.

The server's `CONVERTBAR_PRESET_FILE` (env only, ADR 0002) is imported with
`--preset-import-file` on every export and encode, after `handbrake::preset_import_args` re-reads
the file. Do not drop that pre-flight as redundant with the startup check. HandBrake re-reads the
file on every run, skips an invalid one with **exit 0**, and lets a built-in silently win a name
collision. Its "Invalid preset" lands ~21 KB into stderr, outside the classified tail, so without
the pre-flight these become `Unknown` errors or a wrong-preset encode. Preset names come from the
file's JSON, never from `--preset-list`, whose layout depends on the preset's `Type`. The path is
stored absolute but not canonical, so a symlinked file is followed on every read.
`crates/convertbar-server/tests/live_preset_file.rs` is the `#[ignore]`d live check against a real
HandBrake.

## HandBrake Locator Test Fixtures

Test fixtures default to `PanickingLocator` (`crates/convertbar-core/src/handbrake.rs`): a test that reaches HandBrake resolution without declaring its world fails loud instead of silently reading whatever the host has installed. Declare the world explicitly — `AbsentLocator` for the CI world, `StubLocator` for the installed world, `PathLocator` only in `#[ignore]`d tests that genuinely want the host binary. On the queue thread (`process_queue` runs on a spawned thread), a `PanickingLocator` panic poisons `ctx.db`, so the test thread's own `.lock().unwrap()` on that mutex can surface a `PoisonError` instead of the locator's message — a confusing poison error there is a hint to check for a missing locator declaration before chasing something else.

## Version Bump Workflow

`main` is protected (changes via PR, no merge commits, signed commits). Use the `/release` skill, or run the script it wraps:

```
./scripts/release.sh <X.Y.Z|patch|minor|major> [--yes] [--dry-run] [--notes <file>]
```

The script bumps all three manifests (`tauri.conf.json`, `package.json`, `Cargo.toml`) and `package-lock.json`, rebuilds (baking the version into the binary), commits signed on a `chore/release-X.Y.Z` branch, opens a PR, admin-squash-merges it, then tags the merged commit and pushes the tag — which triggers the CI release. `--dry-run` previews and changes nothing; without `--yes` it pauses for confirmation before the merge/tag/push. It is registered as an `ask` permission, so each run prompts for approval.

Never hand-edit the version — the script keeps the manifests and lockfile in sync and rebuilds before committing.

Published releases are **immutable**: a missing asset can never be added, only shipped in the next
version. So in `build.yml` the `create-release` job makes the one draft and every other job uploads
to it by id. Never let a matrix leg find or create the release by tag again — two legs racing did
that on v2.7.1 and split it across two drafts while every job stayed green. `publish-release`
refuses a draft whose `latest.json` lacks a platform or that has no darwin server asset. That also
catches the legs' read-modify-write of the shared `latest.json` losing an update. When it fails,
re-run the leg whose platform is missing, then `publish-release`.

## Code Signing (macOS)

Release builds are Developer ID–signed and notarized in CI. This is not cosmetic: an ad-hoc signature's designated requirement is the binary's cdhash, so macOS revoked every TCC permission grant on each new build. A Developer ID signature anchors the requirement to the team and bundle identifier instead, so grants survive version bumps.

Six repository secrets drive it, all consumed in `.github/workflows/build.yml`:

| Secret | What it is |
|---|---|
| `APPLE_CERTIFICATE` | base64 of the Developer ID Application `.p12` |
| `APPLE_CERTIFICATE_PASSWORD` | that `.p12`'s export password |
| `APPLE_SIGNING_IDENTITY` | `Developer ID Application: … (TEAMID)` |
| `APPLE_API_ISSUER` | App Store Connect API issuer UUID |
| `APPLE_API_KEY` | App Store Connect API key ID |
| `APPLE_API_KEY_P8` | base64 of `AuthKey_<KEYID>.p8` |

Notarization authenticates with the App Store Connect API key — deliberately never `APPLE_ID`/`APPLE_PASSWORD`, so no personal Apple ID or app-specific password exists in CI. The key is team-scoped, holds only the Developer role, and is revocable on its own.

The preflight step exists because both failure modes are silent: the bundler skips notarization with only a *warning* when the API-key variables are absent, and falls back to an ad-hoc signature when the certificate is absent. Either would ship a release that looks fine and breaks every user's permission grants. A missing secret fails the macOS legs instead, which leaves the release an unpublished draft (`publish-release` needs every matrix leg green).

The darwin-arm64 server asset (`publish-server-darwin` job, `scripts/package-server-macos.sh`)
is signed with the same identity and secrets, under the fixed identifier `com.convertbar.server`,
and notarized through `notarytool` (ADR 0001). The identifier is load-bearing: the designated
requirement is identifier + team, which is what keeps the LaunchAgent's local-network grant
(TN3179) across releases. Changing it costs every install that grant once. The script refuses
an ad-hoc or non-Developer-ID signature and smoke-tests that the signed binary serves the
embedded web UI. Run it locally with the installed identity to rehearse everything except
notarization.

`tauri.conf.json` intentionally carries no `signingIdentity`: local builds, including the rebuild inside `scripts/release.sh`, stay ad-hoc and need no private key. Only CI signs.

**Renewal:** the certificate expires **2031-07-30**. Re-run Task 1 of `docs/superpowers/plans/2026-07-29-macos-code-signing.md` and replace the first three secrets. A renewed certificate under the same Team ID does **not** cost users their permissions again — the designated requirement anchors to the team and bundle identifier, not to the particular certificate. Builds already shipped keep working after expiry; notarization tickets do not expire with the cert.

**Rotating the API key:** revoke in App Store Connect, generate a replacement, and update `APPLE_API_ISSUER`/`APPLE_API_KEY`/`APPLE_API_KEY_P8`. The preflight only checks that secrets are *present*, so a revoked-but-still-populated key passes it and fails later at the notarization submission — still loud, just further along.

**On a Command Line Tools–only machine**, the Developer ID issuing intermediate is absent from the trust store (it ships with Xcode), so a freshly installed certificate shows "not trusted" and `security find-identity -v -p codesigning` reports zero identities. Fix by fetching the intermediate named in the certificate's own Authority Information Access extension; see Task 1 Step 4 of the plan. CI is unaffected — GitHub's macOS runners have full Xcode.

## Restarting After an Auto-Update

A successful install MUST restart the app. `updater.rs`'s `InstallAttempt::Installed` arm calls
`restart_after_install`; there is no safe "keep running the old binary until the user gets around
to it" state on macOS or Linux, and it must not be softened back into one.

tauri-plugin-updater's `install_inner` moves the **running** bundle into a `tempfile::TempDir`
and drops that TempDir the moment `install()` returns — deleting it. Since 2.13.2 the move is an
atomic `renamex_np(RENAME_SWAP)` into the `tauri_updated_app*` extraction dir (two renames on a
volume that cannot swap); the outcome for the running process is unchanged. From that instant the
process executes an unlinked bundle, so macOS can no longer validate its code identity against
the TCC grants it holds (see the designated-requirement note above), and every `open()` of a
protected path fails for it **and** for the HandBrakeCLI children it spawns. This is not the
periodic `/private/tmp` cleaner; it is immediate and happens on every macOS auto-install.

The failure is silent and reads as data corruption: HandBrake logs only `hb_stream_open: open
<path> failed` / `scan: unrecognized file type`. **The tell is the missing ffmpeg line** — a
genuinely bad file logs `[mov,mp4,... @ 0x...] moov atom not found` first, because libavformat
actually read bytes. No ffmpeg line means the file was never opened, so look at the process, not
the file: `lsof -p <pid> | grep -i "txt.*convertbar"` pointing into a `tauri_*` temp dir
instead of `/Applications` is an orphaned process, and the fix is to relaunch from `/Applications`. v2.5.0 shipped the old
behaviour and failed eight jobs this way against files that were fine.

Restarting is free, which is why the old `resume_queue_after_install` is not worth reviving:
`try_install_now` refuses to install unless the queue is idle, `restart_app` kills and reaps the
encoder before exiting, and `recover_interrupted_jobs` requeues anything that raced into the gap.
`should_resume_queue_at_launch` then restarts the queue on the new binary and lifts an
updater-caused drain pause — logic that only ever runs *because* the app restarts.

Unreachable on Windows: `install()` exits the process and the installer relaunches the app.

## Merging a PR (non-release)

`main` is protected: signed commits, no merge commits, PR required. Claude cannot `git push` — ask the user to push with `! git push -u origin <branch>`. Then:

- Open PR: `gh pr create --base main`
- Merge after CI is green: `gh pr merge <n> --admin --squash` (the admin bypass of required checks is allowed)
- Cleanup: `git checkout main && git pull --ff-only`, then `git branch -d <branch>`, then delete the remote branch via `gh api -X DELETE repos/rhurling/convertbar/git/refs/heads/<branch>` (Claude can't `git push :branch`)

Required checks are `frontend` and `rust (ubuntu-22.04)`.

Work that starts from a GitHub issue goes through `/ship-issue`, which wraps the whole path — chunk selection, worktree, TDD, PR, red-CI triage, issue bookkeeping, cleanup. It stops for approval before merging unless told otherwise.

## Adding Tauri Plugins

Always use `npm run tauri add {plugin}` — it handles Cargo.toml, lib.rs registration, npm dependency, and capabilities in one step. Removing a whole plugin: prefer `npm run tauri remove {plugin}` for the same reason. But when only the frontend half is unused (several plugins here are Rust-side only: autostart, dialog, notification, window-state), `npm uninstall` the `@tauri-apps/plugin-*` package alone — `tauri remove` would rip out the still-needed Rust side.

## Permissions (ACL)

Explicit per-call permissions in `src-tauri/capabilities/default.json`. No `:default` bundles — each permission maps to a specific frontend API call so removing one doesn't accidentally break another.

When adding a new frontend Tauri API call or plugin, add the corresponding permission to `default.json`. Backend-only APIs (notifications, tray, window management, and window-state persistence from Rust) do not need ACL permissions. App-defined `#[tauri::command]` functions are also ACL-exempt — only `core:`/`plugin:` APIs invoked from the frontend need a grant.

## Window State

Window position is persisted across restarts via `tauri-plugin-window-state`. Screen confinement runs on every show (tray click) to handle monitor layout changes — ensures at least half the window is visible.

## Post-Convert Hooks

`crates/convertbar-core/src/hooks.rs` fires a webhook and/or a command on two events —
`post-convert` (per file) and `queue-drained` (once per true drain) — through an injected
`HookRunner` seam, same pattern as `FileDisposer`/`HandbrakeLocator`. Six invariants a future
change is most likely to break:

- `post-convert` has three fire points; the error one must stay on `record_job_error_quiet`, not
  the `record_job_error` wrapper, or the direct `record_job_error_quiet` call in the
  vanished-source gate (the one that bypasses the `record_job_error` wrapper) is missed. The
  third is `book_without_encode`, which books a remote job that never reaches the encoder
  (already at the target after the pull, or published by a finalize an outage cut off); a new
  no-encode booking must go through it, or its job ends silently.
- `queue-drained` fires only on a true drain. The queue-done block is also reached by
  `pause_after_current`, which is every pause on Windows.
- `post_convert_command` / `queue_drained_command` are absent from `ALLOWED_KEYS` and from the
  `Settings` struct on purpose. That pair of absences is the entire boundary keeping the server
  head's HTTP API from being a remote shell.
- `drain_mechanism` takes two **disjoint** `ctx.db` guards per iteration — one to read the
  batch, one to write the watermark after dispatching. A mutation that kept the first guard
  alive past the second deadlocked the function against *itself*. No `LockProbeRunner` can catch
  that: `try_lock` only helps once the hook is reached, and this hangs before that. The
  disjoint-statement structure is the only thing preventing it.
- **Each mechanism has its own watermark** (`Mechanism::watermark_key`:
  `last_queue_drained_at_webhook` / `last_queue_drained_at_command`) and its own batch loop.
  Do not re-merge them. A shared watermark advances only when every configured mechanism
  delivers, so a working webhook is pinned behind a failing command — replaying the same oldest
  batch forever, and never delivering anything past the first `QUEUE_DRAINED_BATCH` until the
  command is fixed. Both keys are absent from `ALLOWED_KEYS`/`Settings` like the key they
  replaced; `db::init_db` seeds them from the pre-split `last_queue_drained_at` (`INSERT OR
  IGNORE`, and the legacy row is kept so a rollback still has a watermark).
- **A drain hook can block for minutes**, and every start is refused for that whole time —
  silently. `claim_queue_slot` must stay the ONE place that decides what a refusal means:
  `control::start_queue` (the path both heads use after an add, and the watcher's too) claims
  through it, rather than short-circuiting on its own `is_running` read, which is how that path
  stayed broken after the watcher path was fixed. It claims BEFORE clearing `queue_paused`: a
  SIGSTOP pause keeps the slot held, so clear-then-claim recorded "not paused" over a frozen
  encoder (#190). `process_queue` runs another pass when `work_arrived_while_busy`
  was set *and* the DB really holds a `queued` job; **no branch may loop on the flag alone** (a
  refusal can race a `clear_queue`, and an empty pass re-enters `fire_queue_drained`, which
  re-dispatches a pinned mechanism's whole backlog at up to 300s a batch — a hook whose side
  effect starts the queue would livelock). Only a `PassOutcome::Drained` may be followed by
  another pass, since `get_next_job` does not consult `queue_paused`. The update interlock's
  refusal must not set the flag. The final release goes through
  `release_queue_slot_unless_work_arrived`, which reads the flag and clears `is_running` in ONE
  critical section — a read followed by `RunningGuard`'s drop loses any refusal landing between
  the two — and deliberately LEAVES the flag set when it refuses to release, so that refusal is
  settled by the same DB-backed check as every other. The guard is disarmed after a successful
  release so its drop cannot free a slot another run has since claimed.

## Remote Roots over ssh

The server head re-encodes files on other machines (`CONVERTBAR_REMOTE_ROOTS`, ADRs 0003, 0005,
0006): pull into staging, encode, push to a marked temp, then finalize on the host. What a
change here is most likely to break:

- **`remote.rs` owns all ssh I/O.** Scripts travel on stdin to `sh -s`, their arguments in a
  leading `set --` line, never on the command line: sshd hands the remote command to the login
  shell as one argument, which Linux caps at 128 KiB — a few hundred paths of a folder add. The
  ssh shim enforces that cap. Each script is a function called on its last line, so a cut-off
  stream runs nothing. ssh's exit 255 means unreachable, so no script may exit 255.
- **A remote row's path names a file on its host, never here.** Every site that stats, trashes
  or deletes a job's local path excludes remote rows (`remote_host IS NULL`): recovery, cancel,
  the bad-source lookup, and purge, which answers `remote` from the row before any local rung. A
  new local-file site must do the same, or it destroys an unrelated local file of the same path.
  Dedupe and the name-taken check key on host + path.
- **`RemoteTurn`'s drop ends every remote turn** however it ends: it clears `current_job_id`,
  `finalizing_on`, the outage re-queue marker and the phase, and empties staging. It empties only
  `source/` and `result/`: `CONVERTBAR_STAGING_DIR` may name a directory holding anything.
- **Cancel is refused while `finalizing_on` is set**, every done or no-encode booking writes only
  a row still `encoding`/`paused`, and a cancel lifts the persisted pause only when it booked a
  row. A failure of a job a Cancel already booked is the Cancel's (`pull_source` and the publish
  arm turn it into `Killed`): `record_job_error_quiet` is unguarded, and an outage booking over a
  Cancel would have the next Start take the cancelled job back.
- **Finalize re-reads `cleanup_mode`** (`begin_finalize`, under the lock a Cancel books under):
  a push can take many minutes, and a switch to `keep` during it must still keep the original.
- **Containment compares resolved paths.** A root may itself be a symlink (a share reached
  through one); anything below it must resolve to the resolved root plus the rest of the path as
  named, or it is a symlink and refused, so finalize never renames over a link. Listings use
  `find -H` and show symlinks as nothing, and answer in paths as named, as `/api/info`
  advertises the roots as configured.
- The shim tests need GNU coreutils ≥ 9.5 and findutils (Homebrew's `gnubin` locally; CI
  installs them). `crates/convertbar-server/tests/live_remote_roots.rs` is the `#[ignore]`d live
  run against a real host (default `unraid`), in a scratch directory it removes.

## Status Endpoint (`GET /api/status`)

A flat `{state, percent, queued, errors, queue_paused}` snapshot for pollers outside the app —
the NAS dashboard's Homepage `customapi` tile, configured in the dotfiles repo, which this repo
cannot see. The first four names and the `state` values are that config's contract: change them
only as a breaking change. `percent` is 0, never null, because Homepage renders null as "NaN%".
`queue_paused` is the dotfiles `send` script's contract: it starts the queue after queueing work
only while this is false, so it must stay the persisted `queue_paused` flag alone (a stop by hand
or after an unreachable host), never folding in the in-memory low-disk pause or `state`.

- Live progress comes from `progress::spawn_progress_cache`, a task on the SSE broadcast. Do
  not "simplify" it into `ServerSink` writing a slot: the sink runs on the converter thread
  under core locks and must never lock (see Emitting Events Under the DB Lock). Its `Lagged`
  arm must keep listening — a `break` there freezes progress for the life of the process.
- The cache keeps the last job's value after it ends; `percent` is shown only for the job id
  of the current `encoding`/`paused` row, so the next job never inherits it.
- The route needs a `getStatus` member in the HTTP transport although no UI calls it: the
  frontend contract test requires every `routes.json` command there.

## Cross-Platform

- `libc` (SIGSTOP/SIGCONT) is a `[target.'cfg(unix)'.dependencies]` entry in `crates/convertbar-core/Cargo.toml`, and the signal call sites are gated with `#[cfg(unix)]` attributes — never the `cfg!()` macro, which only skips code at runtime and would still require linking libc on every platform. Mid-encode pause works on macOS and Linux; Windows falls back to queue-level pause.
- Pause/resume: real process freeze (SIGSTOP/SIGCONT) on macOS and Linux, queue-level pause on Windows
- Cancel: `Child::kill()` on all platforms
- HandBrakeCLI detection: `which` on Unix, `where` on Windows (PATH-only, no hardcoded paths)
- Default presets: VideoToolbox (macOS), NVENC (Windows), MKV (Linux)
