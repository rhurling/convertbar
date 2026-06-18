---
name: release
description: Use when cutting a new ConvertBar release or bumping its version — picks the version and release notes, then hands the mechanical bump/build/commit/PR/merge/tag/push to scripts/release.sh.
disable-model-invocation: true
---

# Release ConvertBar

The deterministic mechanics live in `scripts/release.sh`. This skill supplies the judgment around it and interprets the result.

## Steps

1. **Determine the target version.** Use the argument if given. Otherwise read the current version from `package.json` and ask which part to bump (major/minor/patch).
2. **Draft release notes.** Run the `/release-notes` skill over `<previous-tag>..HEAD` and write its markdown to a temp file, e.g. `/tmp/release-notes.md`.
3. **Run the release script.** You will be prompted to approve it — that approval is the release gate:
   ```
   ./scripts/release.sh <X.Y.Z|patch|minor|major> --yes --notes /tmp/release-notes.md
   ```
   `--yes` runs it non-interactively. The script bumps all manifests + the lockfile, rebuilds (baking the version into the binary), commits signed on a release branch, opens a PR, admin-squash-merges it, then tags the merged commit and pushes the tag — which triggers the CI release.
4. **Interpret the result.**
   - Success is the script reaching `Finished N bundles`; it already ignores the trailing `TAURI_SIGNING_PRIVATE_KEY` error (a CI-only secret, absent locally). Do not flag that error.
   - On any non-zero exit, STOP and report what failed. The script makes no commit or push until the build succeeds; if it aborts after pushing, it leaves an open PR.
   - On success the script prints `Released vX.Y.Z — CI build triggered`. CI creates the GitHub release as a **draft** and finalizes it once the multi-platform build completes, so a release URL may not resolve immediately — point the user to the Actions run and `https://github.com/rhurling/convertbar/releases/tag/vX.Y.Z` to watch.

## Preview / manual use

- `./scripts/release.sh <version> --dry-run` prints the plan and changes nothing.
- Run without `--yes` to get a `[y/N]` confirmation before the merge/tag/push (for a human running it directly).

## Notes

- `main` is protected (PR-only, no merge commits, signed commits). The script honours this: it lands the bump via an admin squash-merge and tags the merged commit; the **tag** push (not a branch push) triggers `.github/workflows/build.yml`.
- The script is registered as an `ask` permission in `.claude/settings.local.json`, so every run prompts for approval before anything happens.
