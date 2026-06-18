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

   Then sync the npm lockfile: `npm install --package-lock-only` — this updates `package-lock.json`'s version to match `package.json`. The rebuild in step 3 updates `Cargo.lock` but never touches `package-lock.json`, so without this it silently drifts behind.
3. **Rebuild:** `npm run tauri build` — this bakes the version into the binary. Do NOT skip or reorder this. The build will end with `Error A public key has been found, but no private key ... TAURI_SIGNING_PRIVATE_KEY` — this is **expected locally** and does not mean the build failed (see Critical).
4. **Branch & commit (signed).** `main` is protected — changes must land via a PR, with no merge commits and verified signatures. Never commit or push to `main` directly:
   ```
   git switch -c chore/release-X.Y.Z
   git commit -S -am "chore: bump version to X.Y.Z"
   git push -u origin chore/release-X.Y.Z
   ```
5. **Draft release notes** for the PR body: run the `/release-notes` skill over the range `<previous-tag>..HEAD` and keep its markdown output.
6. **Open and squash-merge the PR:**
   ```
   gh pr create --title "Release X.Y.Z" --body "<release notes from step 5>"
   gh pr merge --squash --delete-branch
   git switch main && git pull --ff-only
   ```
   Squash keeps history linear (no merge commit) and GitHub re-signs the landed commit (verified). The repo has merge commits disabled, so only squash/rebase are offered.
7. **Tag & push** the merged commit — this triggers the release:
   ```
   git tag -s vX.Y.Z -m "vX.Y.Z"
   git push origin vX.Y.Z
   ```
   The tag triggers the CI release (`.github/workflows/build.yml`). Tags aren't covered by the branch ruleset, so this push succeeds even though direct pushes to `main` are blocked.

## Critical

- **Never commit the version bump before rebuilding.** Otherwise CI builds a binary whose embedded version may not match the tag.
- All three files must match exactly. The `-am` commit also sweeps in `Cargo.lock` (refreshed by the build) and `package-lock.json` (refreshed by the lockfile sync in step 2) — both are tracked.
- **The squash merge changes the commit SHA but not the tree** — the tagged commit still contains `X.Y.Z` in all three manifests, so CI rebuilds from the tag with the correct embedded version.
- **Verify the build succeeded before committing.** If `npm run tauri build` fails, STOP — do not commit or tag.
- **The updater-signing error at the end of the local build is expected, not a failure.** `TAURI_SIGNING_PRIVATE_KEY` is a CI-only secret, set in `.github/workflows/build.yml` from `secrets.TAURI_SIGNING_PRIVATE_KEY`; it is intentionally absent locally. The build still compiles and produces the versioned `.app`/`.dmg` before the signing step. Treat the build as successful as long as it reaches `Finished N bundles at:` and the **only** error that follows is the missing private key. Any error *before* bundling (compile errors, version mismatches, etc.) is a real failure — STOP.
