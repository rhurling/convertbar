# ConvertBar

Menu bar app for batch video conversion using HandBrakeCLI. Built with Tauri 2 + React + Rust.

## Version Bump Workflow

`main` is protected (changes via PR, no merge commits, signed commits), so the bump lands through a PR and the release is triggered by the tag — never by a direct push to `main`. Use the `/release` skill, which automates this:

1. Edit version in `src-tauri/tauri.conf.json`, `package.json`, `src-tauri/Cargo.toml`, then run `npm install --package-lock-only` to sync `package-lock.json` (the build refreshes `Cargo.lock` but never the npm lockfile)
2. Rebuild: `npm run tauri build` (bakes the version into the binary)
3. Branch + signed commit: `git switch -c chore/release-X.Y.Z && git commit -S -am "chore: bump version to X.Y.Z"`
4. Open a PR (use `/release-notes` for the body) and **squash-merge** it — never push to `main` directly
5. Tag the merged commit and push the tag: `git tag -s vX.Y.Z && git push origin vX.Y.Z` (tag triggers CI release)

Never commit a version bump before rebuilding — the version is baked into the binary.

## Adding Tauri Plugins

Always use `npm run tauri add {plugin}` — it handles Cargo.toml, lib.rs registration, npm dependency, and capabilities in one step.

## Permissions (ACL)

Explicit per-call permissions in `src-tauri/capabilities/default.json`. No `:default` bundles — each permission maps to a specific frontend API call so removing one doesn't accidentally break another.

When adding a new frontend Tauri API call or plugin, add the corresponding permission to `default.json`. Backend-only APIs (notifications, opener, tray, window management from Rust) do not need ACL permissions.

## Window State

Window position is persisted across restarts via `tauri-plugin-window-state`. Screen confinement runs on every show (tray click) to handle monitor layout changes — ensures at least half the window is visible.

## Cross-Platform

- `libc` (SIGSTOP/SIGCONT) is macOS-only — gated with `cfg!(target_os = "macos")` in Cargo.toml
- Pause/resume: real process freeze on macOS, queue-level pause on other platforms
- Cancel: `Child::kill()` on all platforms
- HandBrakeCLI detection: `which` on Unix, `where` on Windows (PATH-only, no hardcoded paths)
- Default presets: VideoToolbox (macOS), NVENC (Windows), MKV (Linux)
