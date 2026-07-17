# ConvertBar

Menu bar app for batch video conversion using HandBrakeCLI. Built with Tauri 2 + React + Rust.

## Version Bump Workflow

`main` is protected (changes via PR, no merge commits, signed commits). Use the `/release` skill, or run the script it wraps:

```
./scripts/release.sh <X.Y.Z|patch|minor|major> [--yes] [--dry-run] [--notes <file>]
```

The script bumps all three manifests (`tauri.conf.json`, `package.json`, `Cargo.toml`) and `package-lock.json`, rebuilds (baking the version into the binary), commits signed on a `chore/release-X.Y.Z` branch, opens a PR, admin-squash-merges it, then tags the merged commit and pushes the tag — which triggers the CI release. `--dry-run` previews and changes nothing; without `--yes` it pauses for confirmation before the merge/tag/push. It is registered as an `ask` permission, so each run prompts for approval.

Never hand-edit the version — the script keeps the manifests and lockfile in sync and rebuilds before committing.

## Merging a PR (non-release)

`main` is protected: signed commits, no merge commits, PR required. Claude cannot `git push` — ask the user to push with `! git push -u origin <branch>`. Then:

- Open PR: `gh pr create --base main`
- Merge after CI is green: `gh pr merge <n> --admin --squash` (the admin bypass of required checks is allowed)
- Cleanup: `git checkout main && git pull --ff-only`, then `git branch -d <branch>`, then delete the remote branch via `gh api -X DELETE repos/rhurling/convertbar/git/refs/heads/<branch>` (Claude can't `git push :branch`)

Required checks are `frontend` and `rust (ubuntu-22.04)`.

## Adding Tauri Plugins

Always use `npm run tauri add {plugin}` — it handles Cargo.toml, lib.rs registration, npm dependency, and capabilities in one step. Removing a whole plugin: prefer `npm run tauri remove {plugin}` for the same reason. But when only the frontend half is unused (several plugins here are Rust-side only: autostart, dialog, notification, window-state), `npm uninstall` the `@tauri-apps/plugin-*` package alone — `tauri remove` would rip out the still-needed Rust side.

## Permissions (ACL)

Explicit per-call permissions in `src-tauri/capabilities/default.json`. No `:default` bundles — each permission maps to a specific frontend API call so removing one doesn't accidentally break another.

When adding a new frontend Tauri API call or plugin, add the corresponding permission to `default.json`. Backend-only APIs (notifications, tray, window management, and window-state persistence from Rust) do not need ACL permissions. App-defined `#[tauri::command]` functions are also ACL-exempt — only `core:`/`plugin:` APIs invoked from the frontend need a grant.

## Window State

Window position is persisted across restarts via `tauri-plugin-window-state`. Screen confinement runs on every show (tray click) to handle monitor layout changes — ensures at least half the window is visible.

## Cross-Platform

- `libc` (SIGSTOP/SIGCONT) is macOS-only — gated with `cfg!(target_os = "macos")` in Cargo.toml
- Pause/resume: real process freeze on macOS, queue-level pause on other platforms
- Cancel: `Child::kill()` on all platforms
- HandBrakeCLI detection: `which` on Unix, `where` on Windows (PATH-only, no hardcoded paths)
- Default presets: VideoToolbox (macOS), NVENC (Windows), MKV (Linux)
