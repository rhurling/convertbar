# Fable Review — Round 2 (verification pass), 2026-07-08

Re-review at main @ daf4f8e by 7 parallel Fable subagents, one per round-1
report. Each agent (a) adversarially verified every High/Medium round-1
finding against current source — not trusting TRIAGE.md checkboxes, (b)
scanned the fix diffs `eb44d4b..HEAD` (PRs #63–#74) for newly introduced
issues, and (c) did a fresh pass over its area.

| Report | Prior fixes verified | New findings |
|---|---|---|
| [rust-core.md](rust-core.md) | 4 FIXED | 1 Medium, 3 Low |
| [rust-queue-watch.md](rust-queue-watch.md) | 5 FIXED | 4 Low |
| [rust-app-shell.md](rust-app-shell.md) | 15 FIXED | 2 Low |
| [frontend.md](frontend.md) | 9 FIXED | 5 Low |
| [tests-quality.md](tests-quality.md) | 7 FIXED, **2 PARTIAL** | **3 Medium**, 3 Low |
| [claude-automation.md](claude-automation.md) | 10 FIXED | 5 Low |
| [ci-release.md](ci-release.md) | 4 FIXED | 7 Low |
| **Total** | **54 FIXED, 2 PARTIAL, 0 not fixed, 0 regressed** | **0 Critical, 0 High, 4 Medium, 29 Low** |

Suites at daf4f8e: `cargo test` 127 passed / 0 failed / 2 ignored; vitest
15 files / 73 tests all passing. Both hooks and the live CI ruleset were
verified by execution, not just reading.

## Verdict

The B1–B12 campaign held up: every code fix is in place and correct, nothing
regressed. The residue is concentrated in **test coverage claims** (both
PARTIALs and 3 of the 4 Mediums are gaps in what TRIAGE said was covered,
not broken code) and one **teardown race** in the code itself.

## The 4 Mediums

1. **Quit can still orphan an encoder** (rust-core; converter.rs:135):
   `kill_active_child` reaps the current child, but `process_queue` has no
   shutdown signal — with more jobs queued, the loop can spawn the *next*
   HandBrakeCLI during app teardown, re-creating the bug B4 fixed. Fix
   shape: a shutdown `AtomicBool` checked at the loop head.
2. **TRIAGE B11's e2e-coverage claim is false** (tests-quality): neither
   `#[ignore]` test in e2e-ignored runs a conversion, so `process_queue`'s
   queued→encoding→done/error DB transitions remain untested despite the
   triage note saying otherwise.
3. **D3 zero-byte guard untested** (tests-quality): no test exercises the
   zero-byte-output→failure path, and the `decide_cleanup` matrix still
   pins superseded pre-D3 "done for 0-byte" rows.
4. **B4 cancel ordering untested** (tests-quality): the kill→wait→
   delete-partial sequence has no test; reordering it silently breaks
   Windows only (file-handle lock), which PR CI wouldn't catch.

## PARTIALs (both tests-quality)

- **process_queue coverage (round-1 High #3)**: B11 extracted only two
  micro-decisions; the loop body itself is still integration-untested
  (see Medium 2).
- **pause/resume coverage (round-1 Medium #4)**: flag mechanics tested;
  `can_pause_process`/SIGSTOP paths uncovered, commands/converter.rs has
  no test module.

## Low-severity themes (29)

- Unguarded in-flight async races: frontend response races (suffix preview,
  preset switch, updateSetting restore), uncancellable background scans.
- Failure-path polish: silent updater install failure, release.sh
  `|| true`-masked restore, unhinted merged-but-untagged recovery branch.
- Drift: acl-auditor agent behind B6's grant trim, 4 dead
  `@tauri-apps/plugin-*` npm deps, RECOMMENDATIONS.md residuals.
- CI hygiene: missing `permissions:` blocks, paths filter omits the
  workflow file itself, cron auto-disable after 60 days dormancy.
- Missing `stop_hook_active` loop guard in the now-blocking Stop hook.

## Recommended next steps

1. Fix Medium 1 (shutdown flag in `process_queue`) — the only new
   code-behavior defect; small, with a test per repo rules.
2. Close the three test-coverage Mediums together (one test batch): a real
   e2e conversion test in e2e-ignored, a zero-byte-failure test +
   decide_cleanup matrix refresh, and a cancel-ordering test.
3. Sweep the Lows opportunistically — the drift/hygiene items (acl-auditor,
   dead npm deps, workflow permissions, stop-hook guard) are one small
   chore PR; the async-race Lows can ride along with future frontend work.
