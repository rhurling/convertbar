# Round 2 — ci-release (verification pass, 2026-07-08)

Status: in progress — findings appended incrementally by the reviewing subagent.

## Fix verification

Round-1 report had 4 Medium findings (no High/Critical). All verified against main @ daf4f8e; fix work landed in PR #72 (`69a8e48`, B9+B10). Verdicts below are from re-reading the current files and live GitHub state, not TRIAGE checkboxes.

### 1. [Medium] Windows PR coverage gap (test.yml:34) — **FIXED** (per D2 option b)

- `.github/workflows/test-windows.yml` (new file) triggers on `pull_request` with `paths: ["src-tauri/**"]` (test-windows.yml:9-12), job `rust-windows` on `windows-latest` running the full `cargo test` (test-windows.yml:19-27).
- Advisory status verified against the **live ruleset** 17085079: required contexts are still exactly `frontend` and `rust (ubuntu-22.04)` — `rust-windows` is not required, so a slow/flaky Windows run cannot block merges, exactly as the fix intended.
- Verified it actually fires and passes: `gh run list` shows `test-windows` succeeded on PRs `fix/ci-release-hardening` (2026-07-08T05:28Z) and `fix/test-backfill` (05:50Z) — both touched src-tauri — and correctly skipped frontend-only PRs (e.g. `chore/automation-docs-refresh`).
- Related B10 rider: the de-`#[cfg(unix)]`'d cancel test now runs in this Windows leg and passed both runs.

### 2. [Medium] build.yml `workflow_dispatch` half-broken (build.yml:4,108) — **FIXED** (per D4 option a)

- The diff vs eb44d4b is exactly the one-line removal of `workflow_dispatch:`; `on:` is now `push: tags: ["v*"]` only (build.yml:3-6).
- Consequence verified: `publish-release`'s `gh release edit "${{ github.ref_name }}"` (build.yml:107) can now only ever see a tag ref, so the `gh release edit main` failure mode and the stranded-draft scenario are structurally impossible. Nothing else in build.yml changed.

### 3. [Medium] release.sh merge→tag race (release.sh:155-162 old) — **FIXED**

Current `merge_and_tag` (release.sh:160-186) traced line by line:
- release.sh:165 — PR number captured **before** the merge (`gh pr view "$branch" --json number`), so `--delete-branch` can't make the PR unqueryable by branch name later. Failure here aborts via `set -e` before the merge — safe (PR untouched).
- release.sh:172 — `sha` resolved from `gh pr view "$pr" --json mergeCommit -q .mergeCommit.oid`, the authoritative API value, not local refs.
- release.sh:173 — empty-sha guard aborts before tagging.
- release.sh:174 — stale-replication case: if the fetch serves a pre-merge ref, `git pull --ff-only` succeeds trivially, but the HEAD≠sha assertion (release.sh:175-180) then aborts **without tagging** and prints the exact recovery command (`git tag -s v$target -m v$target $sha && git push origin v$target`). The old failure mode (tagging the pre-merge commit with stale manifests) is impossible.
- release.sh:176-180 — concurrent-commit case: pull fast-forwards past the merge commit, HEAD≠sha, same safe abort + hint.
- release.sh:182 — happy path tags `"$sha"` explicitly, not implicit HEAD (belt over the suspenders of the assertion).
This matches the round-1 prescribed fix exactly. Residual nit filed under New findings (#3): the sha-resolution failure path itself has no recovery hint.

### 4. [Medium] Failed build leaves dirty tree with no hint (release.sh:108-122,194-195 old) — **FIXED**

- `build_app` failure branch (release.sh:118-124) now prints the build tail, announces the restore, and runs `git checkout -- package.json package-lock.json src-tauri/tauri.conf.json src-tauri/Cargo.toml src-tauri/Cargo.lock` before `exit 1` — all five files `bump_manifests` + the build can dirty, **including both lockfiles** (the round-1 fix ask).
- Re-runnability verified by tracing: after restore, package.json holds the old version again, so a retry with the **same explicit X.Y.Z** passes the strictly-newer preflight (release.sh:73-76) — this also resolves the round-1 verification-pass "Partial" correction about the misleading retry error.
- Build artifacts can't keep the tree dirty either: `src-tauri/gen/schemas` is gitignored (src-tauri/.gitignore:7), `target/` and `dist/` likewise, so `git status --porcelain` is clean post-restore (preflight release.sh:89).
- Residual nits filed under New findings (#4, #5): the `2>/dev/null || true` masks a failed restore, and a failure *inside* `bump_manifests` (before `build_app`) still strands a dirty tree.

**Tally: 4 FIXED / 0 PARTIAL / 0 NOT FIXED / 0 REGRESSED / 0 N/A-BY-DECISION.**

### Scope riders verified

- **e2e-ignored.yml (B10/D6)** — works for real, not just green: inspected the 2026-07-08T06:09Z run log — `handbrake-cli (1.7.2+ds1-1build2)` and `ffmpeg (7:6.1.1-3ubuntu5)` installed from ubuntu-latest apt, and `cargo test -- --ignored` ran **2 tests, 2 passed** (127 filtered out), i.e. the two `#[ignore]`d e2e tests genuinely execute against real encoders. Runs on every push to main since landing (05:34, 05:56, 06:09 on 2026-07-08); first cron fire will be Mon 2026-07-13 06:00 UTC. `permissions: contents: read` present (e2e-ignored.yml:18-19).
- **IPC contract test in required job (B10)** — `src/test/ipc-contract.test.ts` exists and runs via `npm test` (vitest) inside the required `frontend` job (test.yml:24); latest frontend runs green.
- **check-version-sync.sh correctness vs manifests** — the D7 change (exit 2 + stderr, hook:17-19) landed; comparison logic is correct for the three manifests when tauri.conf.json is readable. All six version fields verified in sync at 0.13.0 (package.json, package-lock.json ×2, tauri.conf.json, Cargo.toml, Cargo.lock). Round-1 Lows remain open by design (see New findings #8).
- **shellcheck** — clean on the whole of scripts/release.sh (shellcheck 0 findings); the changed lines quote `"$branch"`, `"$pr"`, `"$sha"`, `"$target"` correctly; `local branch="chore/release-$target"` was even split onto its own line to avoid the masked-exit-status pattern.
- **Deprecated actions / pins** — no new actions introduced; test-windows.yml and e2e-ignored.yml reuse the same four SHA-pinned actions (checkout v7.0.0, dtolnay/rust-toolchain, swatinem/rust-cache v2) as test.yml. No deprecated actions anywhere.

## New findings

No Critical/High/Medium. Seven Lows, mostly hardening residue on the new files.

1. **[Low]** .github/workflows/test-windows.yml (whole file), test.yml (whole file) — neither has a `permissions:` block, so `GITHUB_TOKEN` gets the repo-default scopes (potentially write). Inconsistent within the very PR that added them: e2e-ignored.yml:18-19 sets `permissions: contents: read` and build.yml:14 sets `permissions: {}`. Failure scenario: a compromised dependency in `npm ci`/`cargo test` (both run untrusted lockfile code) holds a write-capable token in the two workflows that run most often. Fix: add `permissions: contents: read` to both.
2. **[Low]** test-windows.yml:11-12 — the paths filter doesn't include the workflow file itself. A PR that edits `.github/workflows/test-windows.yml` (toolchain bump, new step) without touching `src-tauri/**` merges with the change never exercised; breakage surfaces on the next unrelated src-tauri PR, misattributed. Fix: add `".github/workflows/test-windows.yml"` to `paths`.
3. **[Low]** release.sh:172-173 — the one failure branch in `merge_and_tag` without a recovery hint: if `gh pr view "$pr" --json mergeCommit` fails (network blip right after the merge), `set -e` aborts with only gh's own error; if it returns empty, line 173 aborts naming the PR but not the recovery command. Either way the run is stranded in the worst spot — merged but untagged — and unlike lines 174/177-178 the operator isn't told the `git tag -s ... && git push origin ...` recipe (they must also discover the sha themselves via `gh pr view N --json mergeCommit`). Cheap fix: fold the hint into line 173's message and wrap line 172 in the same `|| { hint; exit 1; }` pattern.
4. **[Low]** release.sh:123 — `git checkout -- ... 2>/dev/null || true` masks a failed restore. If checkout fails (stale index.lock is the realistic case), the script has already printed "Restoring bumped manifests to leave a clean tree..." and exits — claiming a restore that didn't happen, and the next run's clean-tree preflight error is exactly the confusion this fix was meant to remove. Violates the repo's fail-loud rule. Fix: drop `|| true` in favor of `|| echo "warning: restore failed — run: git checkout -- <files>" >&2`.
5. **[Low]** release.sh:98-107 vs 108-126 — the restore only wraps *build* failure. A failure inside `bump_manifests` itself (`npm version` dies after rewriting package.json but before package-lock.json; perl edit fails) exits via `set -e` with a partially-bumped dirty tree and no restore/hint — the exact symptom class the B9 fix addressed, one function earlier. Unlikely in practice; a `trap`-based restore armed at bump time and disarmed after commit would close it generically.
6. **[Low]** test-windows.yml required-vs-advisory trap (probed per brief) — today this is safe: `rust-windows` is not in ruleset 17085079, so a paths-skipped run blocks nothing. But if anyone later adds `rust-windows` to required checks, every frontend-only PR hangs forever on "Expected — waiting for status" because paths-filtered workflows never report a status at all. The header comment (test-windows.yml:3-8) documents the advisory intent but not this specific trap. Fix: one sentence in the comment ("never make this required — the paths filter means it reports no status on frontend-only PRs"), or convert to a job-level `dorny/paths-filter` + neutral-success pattern if it ever must gate.
7. **[Low]** e2e-ignored.yml:8-12 — GitHub auto-disables cron schedules after 60 days without repo activity. The push-to-main trigger keeps it alive while the repo is active, but a dormant repo — precisely when the weekly run is the *only* signal that ubuntu's HandBrake/ffmpeg packages still satisfy the e2e tests — silently loses the schedule (email notice only). Accept, or note it in the header comment so a future "why did the weekly run stop" hunt is short.
8. **[Info]** Still-open round-1 Lows, unbatched by design (TRIAGE.md: "Low/Nit findings ... ride along or are dropped") — none regressed, none silently blend into fixed areas: (a) required-check name embeds the runner label `rust (ubuntu-22.04)` (test.yml:34, ruleset); (b) build.yml:47-48 `PREV`/changelog edge on out-of-order tags; (c) release cargo build runs without `--locked` (build.yml:94); (d) `--dry-run` still skips preflight entirely (release.sh:212 exits before line 214); (e) check-version-sync.sh:17 still silent when tauri.conf.json is unreadable and still ignores both lockfiles — D7 fixed the warning's *visibility* (exit 2 + stderr), not these two logic gaps; (f) no signing-key preflight before `git commit -S`/`git tag -s` (release.sh:134,182).

## Summary

All four round-1 Medium findings in the ci-release scope are genuinely fixed, correctly and completely — verified against the current files, the live ruleset, and actual run logs rather than triage checkboxes. The release.sh merge→tag fix is textbook: PR number captured pre-merge, mergeCommit oid resolved from the API, HEAD asserted against it, the sha tagged explicitly, and both non-happy paths (stale replication, concurrent commit) fail safe with an exact recovery command. The failed-build restore covers all five mutated files including both lockfiles and restores re-runnability with the same version argument. workflow_dispatch is gone from build.yml per D4. The advisory Windows job and the e2e-ignored job both exist, are correctly non-required, and demonstrably work (test-windows passed on the two src-tauri PRs since landing; e2e-ignored's log shows both encoders installed and 2/2 ignored tests passing).

New issues are all Low: missing `permissions:` blocks on the two test workflows, the paths filter not covering test-windows.yml itself, three small fail-loud gaps in release.sh's error branches (unhinted mergeCommit-resolution failure, masked restore failure, unprotected bump_manifests window), the make-it-required-and-it-hangs trap inherent to paths-filtered advisory jobs, and cron auto-disable on dormancy. Nothing blocks a release today; the pipeline is in materially better shape than at round 1.

## Recommendations

1. Add `permissions: contents: read` to test.yml and test-windows.yml (finding #1) — two-line change, closes the only token-scope inconsistency across the four workflows.
2. Add the workflow's own path to test-windows.yml's `paths` filter (finding #2).
3. One more pass over release.sh's error branches (findings #3-#5): recovery hint on the mergeCommit-resolution failure, unmask the restore failure, optionally a `trap`-armed restore around bump→commit. All are message/robustness tweaks, no flow changes.
4. Document the "never make rust-windows required" trap in test-windows.yml's header comment (finding #6).
5. Leave the round-1 Lows (#8) as accepted debt or fold (a)+(c) into the next CI-touching PR: a stable rust job `name:` and `--locked` are each one line.
