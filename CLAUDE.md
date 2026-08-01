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
queues such a job, and `update_setting` drops queued ones when the mode becomes `keep`.
Do not "simplify" this into an error recorded in `process_queue` — an `error` row is
invisible to both `queue_ops::fetch_skip_sets` and `watcher::filter_known_bad_sources`,
so a watched folder would re-queue and re-fail every file on every boot. The
`"keep" => RemoveTemp` arm in `in_place_action` covers the setting-change race and must
stay a real arm, not a `debug_assert!` — the branch it replaces permanently deletes the
user's source on the server head.

Under `keep` the source survives, so re-ingestion protection rests entirely on the
`(size, mtime)` fingerprint in completed rows. Clearing history therefore re-converts
kept sources.

## HandBrake Locator Test Fixtures

Test fixtures default to `PanickingLocator` (`crates/convertbar-core/src/handbrake.rs`): a test that reaches HandBrake resolution without declaring its world fails loud instead of silently reading whatever the host has installed. Declare the world explicitly — `AbsentLocator` for the CI world, `StubLocator` for the installed world, `PathLocator` only in `#[ignore]`d tests that genuinely want the host binary. On the queue thread (`process_queue` runs on a spawned thread), a `PanickingLocator` panic poisons `ctx.db`, so the test thread's own `.lock().unwrap()` on that mutex can surface a `PoisonError` instead of the locator's message — a confusing poison error there is a hint to check for a missing locator declaration before chasing something else.

## Version Bump Workflow

`main` is protected (changes via PR, no merge commits, signed commits). Use the `/release` skill, or run the script it wraps:

```
./scripts/release.sh <X.Y.Z|patch|minor|major> [--yes] [--dry-run] [--notes <file>]
```

The script bumps all three manifests (`tauri.conf.json`, `package.json`, `Cargo.toml`) and `package-lock.json`, rebuilds (baking the version into the binary), commits signed on a `chore/release-X.Y.Z` branch, opens a PR, admin-squash-merges it, then tags the merged commit and pushes the tag — which triggers the CI release. `--dry-run` previews and changes nothing; without `--yes` it pauses for confirmation before the merge/tag/push. It is registered as an `ask` permission, so each run prompts for approval.

Never hand-edit the version — the script keeps the manifests and lockfile in sync and rebuilds before committing.

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

`tauri.conf.json` intentionally carries no `signingIdentity`: local builds, including the rebuild inside `scripts/release.sh`, stay ad-hoc and need no private key. Only CI signs.

**Renewal:** the certificate expires **2031-07-30**. Re-run Task 1 of `docs/superpowers/plans/2026-07-29-macos-code-signing.md` and replace the first three secrets. A renewed certificate under the same Team ID does **not** cost users their permissions again — the designated requirement anchors to the team and bundle identifier, not to the particular certificate. Builds already shipped keep working after expiry; notarization tickets do not expire with the cert.

**Rotating the API key:** revoke in App Store Connect, generate a replacement, and update `APPLE_API_ISSUER`/`APPLE_API_KEY`/`APPLE_API_KEY_P8`. The preflight only checks that secrets are *present*, so a revoked-but-still-populated key passes it and fails later at the notarization submission — still loud, just further along.

**On a Command Line Tools–only machine**, the Developer ID issuing intermediate is absent from the trust store (it ships with Xcode), so a freshly installed certificate shows "not trusted" and `security find-identity -v -p codesigning` reports zero identities. Fix by fetching the intermediate named in the certificate's own Authority Information Access extension; see Task 1 Step 4 of the plan. CI is unaffected — GitHub's macOS runners have full Xcode.

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

## Cross-Platform

- `libc` (SIGSTOP/SIGCONT) is a `[target.'cfg(unix)'.dependencies]` entry in `crates/convertbar-core/Cargo.toml`, and the signal call sites are gated with `#[cfg(unix)]` attributes — never the `cfg!()` macro, which only skips code at runtime and would still require linking libc on every platform. Mid-encode pause works on macOS and Linux; Windows falls back to queue-level pause.
- Pause/resume: real process freeze (SIGSTOP/SIGCONT) on macOS and Linux, queue-level pause on Windows
- Cancel: `Child::kill()` on all platforms
- HandBrakeCLI detection: `which` on Unix, `where` on Windows (PATH-only, no hardcoded paths)
- Default presets: VideoToolbox (macOS), NVENC (Windows), MKV (Linux)
