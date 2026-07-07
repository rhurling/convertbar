# Fable Review — 2026-07-07

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

1. Async-ify the remaining main-thread blockers (`commands/handbrake.rs` × 4,
   preset-cache convoy, folder scans) — same fix pattern as PR #55.
2. Capture a bounded HandBrake stderr tail into failure messages; treat zero-byte
   output as failure.
3. Strip the 9 stale ACL grants + unused fs/opener plugins; set a real CSP.
4. Fix `fileName()` separator handling (and its test) for Windows display.
5. Tag releases by the PR's `mergeCommit` oid instead of post-pull main HEAD;
   fix or remove build.yml's broken `workflow_dispatch` path.
6. Rewrite the sqlite-migration-reviewer agent against the current db.rs;
   refresh RECOMMENDATIONS.md/SPEC.md.
7. Add an advisory windows-latest PR CI job; de-`#[cfg(unix)]` the cancel tests.
8. Debounce/commit-on-blur the three SettingsPage text inputs (reuse the WatchRow
   draft pattern) to stop per-keystroke IPC races.
