# Fable Review — 2026-07-07

> **CLOSED 2026-07-08.** Every accepted finding from both rounds shipped in PRs #63–#84.
> Round 2 re-verified all 56 round-1 verdicts against source: 54 fixed, 2 partial (both
> later closed by round 2's mock-runtime work), 0 regressed. Four findings were explicitly
> declined with rationale in [DECISIONS.md](DECISIONS.md) (D9, D10, D14, D16).
>
> The Low/Nit items still listed in the `round2/` report bodies are **accepted residual
> debt, not a backlog** — each is stamped "unbatched by triage policy" / "no action
> implied" there. Re-verified 2026-07-28: still open, still deliberate.
>
> [DECISIONS.md](DECISIONS.md) is *not* historical — it is the live rationale record for
> the CSP, the ACL trim, D3 zero-byte semantics, D5 updater behaviour, and D7 (cited by
> `.claude/hooks/check-version-sync.sh`). Keep it.

Full-codebase review by 7 parallel Fable subagents. Each report ends with its own
`## Summary` and `## Recommendations`; this index captures the cross-cutting picture.

| Report | Scope | Findings |
|---|---|---|
| [rust-core.md](rust-core.md) | converter, handbrake, probe, probe_cache, media_skip, types | 23 |
| [rust-queue-watch.md](rust-queue-watch.md) | queue commands, watcher, watch commands, db | 26 |
| [rust-app-shell.md](rust-app-shell.md) | lib/main wiring, remaining commands, tauri.conf, ACL, Cargo.toml | 31 |
| [frontend.md](frontend.md) | React components, hooks, pages, lib | 41 |
| [tests-quality.md](tests-quality.md) | Rust + Vitest suites | 30 |
| [claude-automation.md](claude-automation.md) | .claude skills/hooks/agents/settings, CLAUDE.md, docs drift | 32 |
| [ci-release.md](ci-release.md) | workflows, release.sh, version sync | 22 |

## Verification pass (2026-07-07)

Every Critical/High/Medium finding was adversarially re-verified by a second
agent instructed to refute it (verdicts appended to each report under
`## Verification pass`). Result across 55 verdicts: **49 confirmed, 5 partial,
1 refuted.** Notable corrections:

- **Refuted:** "MKV data in `.mp4`-named files on Linux" — empirically
  disproven; HandBrakeCLI auto-detects the container from the destination
  extension. But testing surfaced a **new confirmed bug**: the Linux default
  preset name `"H.265 MKV 1080p"` (db.rs:17) is invalid in current HandBrake
  (real name `"H.265 MKV 1080p30"`), so default conversions on Linux fail
  outright.
- **Downgraded:** the preset-cache lock convoy is real but the shell-out
  measured ~0.19s, not seconds — a brief UI hitch, severity Medium.
- **Narrowed:** the tray UTF-8 panic requires the opt-in
  `menubar_show_filename` setting; the Windows `fileName()` bug does not
  affect notifications (built backend-side, platform-correct); the Unix
  "flush after unlink" detail on cancel was wrong (SIGKILL — no flush),
  the Windows handle-lock core stands.

## Cross-cutting themes (corroborated by multiple agents)

1. **Main-thread blocking survives at more entry points than the 4 previously fixed.**
   `commands/handbrake.rs` runs four sync HandBrakeCLI/`which` shell-outs on the main
   thread (found independently by rust-core and rust-app-shell); `add_files_inner`
   holds the preset_cache mutex across a blocking shell-out, creating a lock convoy
   with the sync `generate_preset_suffix` command; `scan_folder`/`classify_paths`
   walk directories recursively on the main thread.

2. **ACL/plugin drift is all over-granting, none under-granting.** `fs:default` and
   `notification:default` violate the project's own "no `:default` bundles" rule
   (both plugins are Rust-side only — likely removable along with 7 more unused
   grants), and `csp: null` disables webview hardening. Found independently by
   rust-app-shell and claude-automation.

3. **Windows is the blind spot.** `format.ts:fileName()` splits on `/` only (every
   queue/history row shows the full path on Windows — and its test enshrines the
   bug); cancel-path tests are `#[cfg(unix)]` so `Child::kill()` has zero Windows
   coverage; PR CI is ubuntu-only (an advisory windows-latest PR job is cheap since
   required checks gate only `frontend` + `rust (ubuntu-22.04)`).

4. **Failures are silent in both directions.** Backend: HandBrake stderr is discarded
   so every failed encode reads "Conversion failed"; zero-byte outputs record as
   "done". Frontend: most mutations swallow rejected invokes with no UI feedback.

5. **Automation/docs have drifted from the code.** The sqlite-migration-reviewer
   agent describes a migration story that no longer exists (it would misreview);
   RECOMMENDATIONS.md lists shipped features as missing; SPEC.md contradicts
   CLAUDE.md; the Stop-hook version-sync warning is emitted where Stop hooks don't
   display it.

## Top recommendations (priority order)

1. Fix the Linux default preset name (`"H.265 MKV 1080p"` → `"H.265 MKV 1080p30"`,
   db.rs:17) — default conversions on Linux fail outright (found during verification).
2. Async-ify the remaining main-thread blockers (`commands/handbrake.rs` × 4,
   preset-cache convoy, folder scans) — same fix pattern as PR #55.
3. Capture a bounded HandBrake stderr tail into failure messages; treat zero-byte
   output as failure.
4. Strip the 9 stale ACL grants + unused fs/opener plugins; set a real CSP.
5. Fix `fileName()` separator handling (and its test) for Windows display.
6. Tag releases by the PR's `mergeCommit` oid instead of post-pull main HEAD;
   fix or remove build.yml's broken `workflow_dispatch` path.
7. Rewrite the sqlite-migration-reviewer agent against the current db.rs;
   refresh RECOMMENDATIONS.md/SPEC.md.
8. Add an advisory windows-latest PR CI job; de-`#[cfg(unix)]` the cancel tests.
9. Debounce/commit-on-blur the three SettingsPage text inputs (reuse the WatchRow
   draft pattern) to stop per-keystroke IPC races.
