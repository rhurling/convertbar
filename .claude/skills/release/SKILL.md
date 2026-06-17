---
name: release
description: Use when cutting a new ConvertBar release or bumping its version — bumps the version across all three manifests, rebuilds so the version is baked into the binary, then commits, tags, and pushes to trigger the CI release.
disable-model-invocation: true
---

# Release ConvertBar

Cut a new release. The version lives in **three** files that must stay in sync, and the binary embeds the version at build time — so the rebuild MUST happen before the commit.

## Steps

1. **Determine the target version** `X.Y.Z`. Use the argument if given; otherwise read the current version from `package.json` and ask which part to bump (major/minor/patch).
2. **Bump all three manifests** to `X.Y.Z`:
   - `src-tauri/tauri.conf.json` (`"version"`)
   - `package.json` (`"version"`)
   - `src-tauri/Cargo.toml` (`[package]` `version`)
3. **Rebuild:** `npm run tauri build` — this bakes the version into the binary. Do NOT skip or reorder this.
4. **Commit:** `git commit -am "chore: bump version to X.Y.Z"`
5. **Tag:** `git tag vX.Y.Z`
6. **Push:** `git push origin main && git push origin vX.Y.Z` — the tag triggers the CI release (`.github/workflows/build.yml`).

## Critical

- **Never commit the version bump before rebuilding.** Otherwise CI builds a binary whose embedded version may not match the tag.
- All three files must match exactly. If the build updates `Cargo.lock`, the `-am` commit will include it (it is tracked).
- **Verify the build succeeded before committing.** If `npm run tauri build` fails, STOP — do not commit or tag.
