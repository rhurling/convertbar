# Release Script + Thin `/release` Skill — Design

**Date:** 2026-06-18
**Status:** Approved

## Problem

The `/release` skill encodes the release process as prose that Claude
interprets step-by-step. Two problems:

1. **Determinism.** Bump, lockfile sync, build, commit, and tag are pure
   mechanical steps — exactly the kind of work that should be code, not
   model-driven (the lockfile-drift bug we just fixed came from a missed
   deterministic step).
2. **Push gating.** A global `git push` **deny** in Claude's permissions blocks
   Claude from running the release pushes. Adding a scoped `allow` can't fix
   this — deny overrides allow.

## Solution

Move the deterministic mechanics into `scripts/release.sh`. Register the script
as an **ask** permission so each Claude-run release is explicitly approved by the
user — a deliberate, visible gate, not a hidden push bypass. The `/release` skill
shrinks to a thin orchestrator that supplies judgment (version choice, release
notes) and handles the unexpected.

## Components

### `scripts/release.sh` — deterministic mechanics

Pure code. Runnable by the user manually or by Claude. No model judgment inside.

**Interface:**

```
scripts/release.sh <X.Y.Z | patch | minor | major> [--yes|-y|--force] [--dry-run] [--notes <file>]
```

- **Version arg (required):** an explicit `X.Y.Z`, or a bump keyword
  (`patch`/`minor`/`major`) computed from the current `package.json` version.
- **default (no flag): interactive** — runs through "PR opened", then prompts
  `merge, tag & push v<X.Y.Z>? [y/N]` before anything irreversible.
- **`--yes` / `-y` / `--force`:** skip the prompt, run end-to-end. This is the
  flag Claude passes (Claude cannot answer an interactive stdin prompt).
- **`--dry-run`:** print the plan, mutate nothing, **build nothing**, push
  nothing. Fast and pure — this is the test seam.
- **`--notes <file>`:** PR body source. Falls back to a minimal default body.

**Stages:**

0. **Preflight (fail fast).** Clean working tree; on `main`; in sync with
   `origin/main`; required tools present (`node`, `npm`, `gh`, `git`, tauri CLI);
   `gh auth status` ok; version valid and strictly newer than current.
1. **Bump** the three manifests (`src-tauri/tauri.conf.json`, `package.json`,
   `src-tauri/Cargo.toml`); `npm version --no-git-tag-version` bumps
   `package.json` and `package-lock.json` together.
2. **Build** `npm run tauri build`. Success = output reached `Finished N
   bundles`; the trailing `TAURI_SIGNING_PRIVATE_KEY` error is expected locally
   and ignored. **Any earlier error is a hard stop** — leaves the uncommitted
   bump in place for inspection, nothing committed or pushed.
3. **Commit.** Branch `chore/release-X.Y.Z`, signed `git commit -S -am "chore:
   bump version to X.Y.Z"` (sweeps in `Cargo.lock` + `package-lock.json`).
4. **Push branch + open PR** via `gh pr create` (body from `--notes` or default).
5. **Checkpoint.** Interactive prompt, skipped with `--yes`. Declining leaves
   the open PR and exits cleanly — nothing merged.
6. **Merge** `gh pr merge --admin --squash --delete-branch`.
7. **Sync main** `git switch main && git pull --ff-only`.
8. **Tag + push** `git tag -s vX.Y.Z -m "vX.Y.Z"` on the merged commit, push the
   tag → triggers the CI release (`.github/workflows/build.yml`). Print the
   release URL.

**Failure / abort semantics:** A real build failure (stage 2) stops before any
commit or push. Aborting at the checkpoint (stage 5) leaves an open PR and an
unmerged branch, recoverable by hand. The script does not auto-revert manifest
edits — it reports state and leaves the tree for inspection.

### `.claude/settings.json` — ask permission

Add `Bash(scripts/release.sh:*)` to the project's **ask** list. Every Claude
invocation of the script prompts the user for approval; that approval is the
release gate. The global `git push` deny is untouched and still applies to every
other command.

### `/release` skill — thin orchestrator

Rewritten to:

1. Determine the target version (judgment: argument, or ask major/minor/patch).
2. Generate release notes via `/release-notes` over `<previous-tag>..HEAD`, write
   to a temp file.
3. Invoke `scripts/release.sh <version> --yes --notes <file>` (ask-gated).
4. Interpret output: treat the signing-key error as non-fatal, detect real
   failures, surface the release URL, stop loudly on any error.

The skill keeps the *why* (signing-key meaning, branch-protection rationale) as
commentary; the script enforces the actual checks.

## Testing

Reuse the existing vitest runner — no new tooling.

1. **Dry-run is pure:** invoke `release.sh <ver> --dry-run`; assert exit 0, the
   printed plan names the target version and key stages, and `git status` is
   unchanged afterward (zero mutations, no tag created, no branch created).
2. **Preflight fails fast:** invoke on a dirty tree or wrong branch; assert
   non-zero exit and a clear diagnostic — verifying intent (the guard exists to
   stop a release from a bad state), not just behavior.

## Out of scope (YAGNI)

- Resuming a half-finished release (`--resume`). v1 documents manual recovery.
- Rolling back manifest edits automatically on build failure.
- Non-`main` base branches.
