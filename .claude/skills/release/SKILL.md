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
3. **Rebuild:** `npm run tauri build` — this bakes the version into the binary. Do NOT skip or reorder this. The build will end with `Error A public key has been found, but no private key ... TAURI_SIGNING_PRIVATE_KEY` — this is **expected locally** and does not mean the build failed (see Critical).
4. **Commit:** `git commit -am "chore: bump version to X.Y.Z"`
5. **Tag:** `git tag vX.Y.Z`
6. **Push:** `git push origin main && git push origin vX.Y.Z` — the tag triggers the CI release (`.github/workflows/build.yml`).

## Critical

- **Never commit the version bump before rebuilding.** Otherwise CI builds a binary whose embedded version may not match the tag.
- All three files must match exactly. If the build updates `Cargo.lock`, the `-am` commit will include it (it is tracked).
- **Verify the build succeeded before committing.** If `npm run tauri build` fails, STOP — do not commit or tag.
- **The updater-signing error at the end of the local build is expected, not a failure.** `TAURI_SIGNING_PRIVATE_KEY` is a CI-only secret, set in `.github/workflows/build.yml` from `secrets.TAURI_SIGNING_PRIVATE_KEY`; it is intentionally absent locally. The build still compiles and produces the versioned `.app`/`.dmg` before the signing step. Treat the build as successful as long as it reaches `Finished N bundles at:` and the **only** error that follows is the missing private key. Any error *before* bundling (compile errors, version mismatches, etc.) is a real failure — STOP.
