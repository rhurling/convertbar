# Fable Review: claude-automation

## .claude/settings.json (hooks)
Reviewed: /Users/rhurling/Sites/convertbar/.claude/settings.json (done)

PreToolUse lock-file guard:
- **[Low]** .claude/settings.json:9 — the guard fails open when `jq` is missing: `f=$(jq ...)` yields empty, the `case` falls through, and the hook exits 0, silently disabling lock-file protection. Fix: `command -v jq >/dev/null || { echo "jq missing — cannot check lock-file guard" >&2; exit 2; }` (or at least emit a warning) before the `jq` call.
- **[Nit]** .claude/settings.json:9 — the guard only covers Edit/MultiEdit/Write; lock files can still be modified via Bash (`sed -i`, `tee`, redirects). Inherent limitation, but worth a comment or a Bash-matcher companion if it ever bites.
- **[Nit]** .claude/settings.json:5,16 — matcher includes `MultiEdit`, which recent Claude Code versions merged into `Edit`. Harmless, but can be dropped.
- Quoting, exit codes, and stderr usage are otherwise correct: exit 2 blocks with the message on stderr (right channel for PreToolUse), `case "$f"` is safely quoted, and both bare (`package-lock.json`) and nested (`*/package-lock.json`) paths are matched.

PostToolUse rustfmt hook:
- **[Medium]** .claude/settings.json:20 — the hook runs `rustfmt` on the entire edited file, which reformats unrelated code on files that are not already fmt-clean (this has caused noisy diffs in practice, per project memory). Fix: either run `cargo fmt` once to make the tree fmt-clean and keep the hook, or gate it (`rustfmt --check "$f" ... || skip`) so it never introduces out-of-scope diffs.
- **[Nit]** .claude/settings.json:20 — `--edition 2021` is hardcoded; it currently matches `src-tauri/Cargo.toml` (edition = "2021") but will silently drift if the crate edition is bumped. Fix: parse the edition from Cargo.toml or use `cargo fmt -- "$f"`.
- **[Nit]** .claude/settings.json:20 — all rustfmt errors are swallowed (`>/dev/null 2>&1`), so a file rustfmt cannot parse passes silently. Acceptable for a PostToolUse hook; noting for completeness.

Stop hook wiring (line 30) correctly quotes `${CLAUDE_PROJECT_DIR:-.}` and delegates to the script below.

## .claude/hooks/check-version-sync.sh
Reviewed: /Users/rhurling/Sites/convertbar/.claude/hooks/check-version-sync.sh (done)

- **[Medium]** check-version-sync.sh:13-15 — the warning is written to stdout with exit 0. For Stop hooks, exit-0 stdout is only visible in transcript mode; it is neither shown prominently to the user nor fed back to Claude, so the mismatch warning is effectively invisible in normal use. Fix: emit JSON (`{"decision":"block","reason":"..."}`) or `exit 2` with the message on stderr if Claude should act on it; if it is meant purely for the human, document that it only shows in transcript view.
- **[Low]** check-version-sync.sh:12 — the check is anchored on `tauri.conf.json`: if that file is missing/unparseable (`tv` empty), the hook stays silent even when package.json and Cargo.toml disagree. Fix: warn when any one of the three values is empty while another is non-empty.
- Otherwise correct: `set -u`, jq-missing → exit 0 (reasonable for an advisory hook), grep/sed extraction targets the first line-anchored `version =` in Cargo.toml (matches the `[package]` section, verified at src-tauri/Cargo.toml:3), never blocks a legitimately clean state, always exits 0.

## .claude/settings.local.json (permissions)
Reviewed: /Users/rhurling/Sites/convertbar/.claude/settings.local.json (done)

No secrets or tokens present; contents are three `ask` Bash rules for the release script, `enabledMcpjsonServers: [context7]`, and a UI flag.

- **[Low]** settings.local.json:3-7 — the release `ask` gate lives in settings.local.json, which is excluded by the user's global gitignore (`**/.claude/settings.local.json`), so it exists only on this machine, while CLAUDE.md and release/SKILL.md describe it as part of the project's release gating. Fix: move the three `ask` rules into the committed `.claude/settings.json`, or note in CLAUDE.md that the gate is machine-local.
- **[Nit]** settings.local.json:4-6 — the patterns cover `./scripts/release.sh`, `scripts/release.sh`, and `bash scripts/release.sh`, but not `bash ./scripts/release.sh`, an absolute path, or `sh scripts/release.sh`. Those variants just fall back to the default permission prompt, so this is a consistency gap, not a bypass.
- Permissions hygiene is otherwise good: no over-broad `allow` rules in either settings file.

## .claude/skills/release/SKILL.md
Reviewed: /Users/rhurling/Sites/convertbar/.claude/skills/release/SKILL.md (done)

Cross-checked against scripts/release.sh — flags (`--yes`, `--dry-run`, `--notes`), the confirmation checkpoint, admin squash-merge, tag-push trigger, the ignored TAURI_SIGNING_PRIVATE_KEY error, and the CI draft-release flow (verified in .github/workflows/build.yml) all match.

- **[Low]** SKILL.md:21 — "Success is the script reaching `Finished N bundles`" conflates the internal build-success marker with overall success; the script's actual success signal is the `Released vX.Y.Z — CI build triggered` line (release.sh:163), which line 23 also states. Fix: make line 21 say the *build step's* success marker is `Finished N bundles`, and overall success is the `Released ...` line / exit 0.
- **[Low]** SKILL.md:22 — "The script makes no commit or push until the build succeeds" is true, but on build failure the working tree is left dirty with bumped manifests + lockfile (bump_manifests runs before build_app, no rollback). Fix: add "on build failure, restore with `git checkout -- .`" to the failure guidance.
- **[Nit]** SKILL.md:14 — instructs writing notes to `/tmp/release-notes.md`, which conflicts with the harness's scratchpad-directory guidance. Fix: say "a temp file in the session scratchpad".
- **[Nit]** SKILL.md:33 — the `ask` permission claim depends on the gitignored settings.local.json (see finding above).

## .claude/skills/release-notes/SKILL.md
Reviewed: /Users/rhurling/Sites/convertbar/.claude/skills/release-notes/SKILL.md (done)

Clean. Tag-selection commands, log range, grouping rules, and the compare-link format are correct and self-contained; correctly instructs print-only (no commit/release).

## .claude/skills/add-tauri-plugin/SKILL.md
Reviewed: /Users/rhurling/Sites/convertbar/.claude/skills/add-tauri-plugin/SKILL.md (done)

- **[Medium]** SKILL.md:15 — states ConvertBar uses "no `:default` bundles", but src-tauri/capabilities/default.json:28-29 currently grants `fs:default` and `notification:default` (introduced in the watched-folders PR #32). Both plugins are registered Rust-side only (src-tauri/src/lib.rs:34,40) with no `@tauri-apps/plugin-fs`/`plugin-notification` imports anywhere in src/, so by the project's own convention (backend-only APIs need no ACL) these two grants appear removable. Fix: remove the two `:default` grants (and verify notifications + watched-folder behavior still work), or update this skill, CLAUDE.md, and acl-auditor to acknowledge the exception. Same drift is flagged under CLAUDE.md and acl-auditor.
- Otherwise accurate: `npm run tauri add`, per-call permission examples, backend-only exemption, and the acl-auditor verification step all match the project setup.

## .claude/agents/acl-auditor.md
Reviewed: /Users/rhurling/Sites/convertbar/.claude/agents/acl-auditor.md (done)

Points at the correct capability file (src-tauri/capabilities/default.json) and the real frontend layout (src/lib/tauri.ts exists and is the invoke wrapper; hooks/ and pages/ exist). Permission-mapping examples match granted permissions. Read/Grep/Glob toolset is appropriate for a report-only auditor.

- **[Low]** acl-auditor.md:10 — "ConvertBar deliberately uses per-call permissions (no `:default` bundles)" is no longer true (`fs:default`, `notification:default` in default.json). The agent would correctly report them as unused, but its framing asserts a state that doesn't hold. Fix: resolve the bundle drift (preferred: remove the grants) or soften the claim.
- **[Nit]** acl-auditor.md:18 — the plugin list (updater, process, autostart, window-state, opener) omits fs and notification, which are now dependencies. They are backend-only today, but listing them would help the auditor classify them deliberately rather than by omission.

## .claude/agents/cross-platform-reviewer.md
Reviewed: /Users/rhurling/Sites/convertbar/.claude/agents/cross-platform-reviewer.md (done)

Clean. All stated platform contracts verified against current code: `libc` gated under `[target.'cfg(target_os = "macos")'.dependencies]` (src-tauri/Cargo.toml:37), `which`/`where` detection in src-tauri/src/handbrake.rs:14-23, preset defaults present. Procedure and grep targets match the src-tauri/src layout (including the commands/ subdirectory via recursive grep).

## .claude/agents/sqlite-migration-reviewer.md
Reviewed: /Users/rhurling/Sites/convertbar/.claude/agents/sqlite-migration-reviewer.md (done)

- **[High]** sqlite-migration-reviewer.md:19 — "There is currently **no migration / versioning mechanism**" is false: src-tauri/src/db.rs:70-87 now contains a data backfill (`UPDATE jobs SET completed_at ...`) and an idempotent column migration (`ALTER TABLE jobs ADD COLUMN` with "duplicate column name" ignored) for `source_size`/`source_mtime`. The agent's core-trap guidance ("adding a column will NOT apply to existing DBs", line 14) would misreview current code and push contributors toward inventing a PRAGMA user_version scheme instead of following the established idempotent-ALTER pattern. Fix: rewrite the "core trap" section to describe the existing pattern (CREATE TABLE IF NOT EXISTS + idempotent ALTER + backfill) and require new columns to follow it.
- **[Medium]** sqlite-migration-reviewer.md:14,25 — the table list is stale: db.rs now defines five tables (`jobs`, `settings`, `preset_suffixes`, `watched_directories`, `probe_cache`), not three. Fix: update the list, or better, tell the agent to derive tables from db.rs rather than hardcoding them.
- DB-path claim (`dirs::data_dir()/com.convertbar.app/convertbar.db`) verified correct (db.rs:5-6, tauri.conf.json identifier).

## CLAUDE.md (project root)
Reviewed: /Users/rhurling/Sites/convertbar/CLAUDE.md (done)

Verified claims: release workflow matches scripts/release.sh exactly (manifest bump order, signed commit on chore/release-X.Y.Z, PR, admin squash-merge, tag push → CI); `npm run tauri add` guidance is consistent with the skill; window-state plugin registered (src-tauri/src/lib.rs:36) with half-window screen confinement on show (lib.rs:157); all Cross-Platform bullets (libc gating, pause/resume split, kill-based cancel, which/where detection, preset defaults) match current code.

- **[Medium]** CLAUDE.md "Permissions (ACL)" — "No `:default` bundles" is false: src-tauri/capabilities/default.json:28-29 grants `fs:default` and `notification:default`, and both plugins are used only from Rust (no frontend imports), so per CLAUDE.md's own rule they should not need ACL entries at all. Fix: remove the two grants after verifying watched-folder + notification behavior, or amend the section to document the exception. (Same root cause as the add-tauri-plugin and acl-auditor findings.)
- **[Low]** CLAUDE.md "Version Bump Workflow" — "It is registered as an `ask` permission, so each run prompts for approval" reads as a project guarantee, but the rule lives in the machine-local, gitignored settings.local.json. Fix: move the rules to the committed settings.json or qualify the sentence.

## docs/SPEC.md
Reviewed: /Users/rhurling/Sites/convertbar/docs/SPEC.md (done)

This is the original v1 spec and much of it is intentionally historical, but several statements now directly contradict shipped behavior and other project docs:

- **[Medium]** SPEC.md:336-341 — HandBrakeCLI detection lists hardcoded fallbacks (`/usr/local/bin/HandBrakeCLI`, `/opt/homebrew/bin/HandBrakeCLI`), contradicting CLAUDE.md's "PATH-only, no hardcoded paths" and the actual implementation (src-tauri/src/handbrake.rs uses `which`/`where` only). Fix: correct or annotate as superseded.
- **[Medium]** SPEC.md:307,323 — "Platform: macOS only" / "Out of Scope: Windows/Linux support" contradicts CLAUDE.md's Cross-Platform section and the multi-platform CI build. Fix: mark superseded.
- **[Low]** SPEC.md:124-151 — data model shows three tables; db.rs now has five (`watched_directories`, `probe_cache`) and `jobs` gained `source_size`/`source_mtime` via the idempotent ALTER migration. The watched-folders and skip-by-source-media features are absent from the spec entirely. Fix: a short "superseded by" changelog note at the top would prevent readers treating this as current.
- **[Nit]** SPEC.md:227-228 — "HandBrakeCLI outputs to stderr" — RECOMMENDATIONS.md:19 itself notes progress goes to stdout when piped (fixed in v0.3.0).

Recommend adding a one-line banner: "Original design spec (v0.1) — see CLAUDE.md and code for current behavior."

## docs/OPEN_ISSUES.md
Reviewed: /Users/rhurling/Sites/convertbar/docs/OPEN_ISSUES.md (done)

Single issue (Docker/web-UI head), clearly marked "not started" — still genuinely open, no stale-fixed items.

- **[Nit]** OPEN_ISSUES.md:6 — lists `queue.rs` alongside top-level core modules; it actually lives at `src-tauri/src/commands/queue.rs` (the module named as the portable core). Cosmetic, but a reader grepping for a top-level file won't find it.

## docs/RECOMMENDATIONS.md
Reviewed: /Users/rhurling/Sites/convertbar/docs/RECOMMENDATIONS.md (done)

Header says "(v0.6.0)"; the project is at 0.13.0 and several "open"/"missing" items have since shipped:

- **[Medium]** RECOMMENDATIONS.md:156 — "Launch at login: Setting only, no actual macOS login item registration" is stale: `tauri-plugin-autostart` is registered (src-tauri/src/lib.rs:38) with `autostart:allow-enable/disable/is-enabled` ACL grants. Fix: move to Implemented.
- **[Medium]** RECOMMENDATIONS.md:164 — "No tests (unit or integration) exist for either Rust or TypeScript code" is stale: there are 10+ Vitest files across src/ and 8 `#[cfg(test)]` modules in src-tauri/src. Fix: delete the bullet.
- **[Low]** RECOMMENDATIONS.md:153 — "US2: Error state icon in menu bar — Missing" appears implemented (shared error flag for tray icon state, src-tauri/src/lib.rs:84). Fix: verify and move to Done.
- **[Low]** RECOMMENDATIONS.md:152 — "US1: Skipped files notification — Missing / silent skip" is stale: skip feedback exists (`SkipReason`/`SkipCount` in src/lib/tauri.ts, `summarizeAdds` surfaced via DropZone, plus the `skipped` job status). Fix: move to Done.
- **[Low]** RECOMMENDATIONS.md:104-113 — item 9 (file picker) says "add tauri-plugin-dialog"; the plugin is already a dependency and registered (lib.rs:35, used in commands/watch.rs for watched-folder picking). The HandBrakeCLI-path Browse button itself may still be missing, but the "How/Files" steps are outdated. Fix: re-scope the item to just the Browse button.
- **[Nit]** RECOMMENDATIONS.md:1 — retitle or date-stamp the doc; a v0.6.0 snapshot presented as current recommendations invites wrong prioritization.

## Summary

The automation layer is in good shape overall: hooks are correctly quoted with the right exit-code semantics, permissions are tight (no over-broad allows, no secrets in either settings file), the release skill matches scripts/release.sh, and release-notes / cross-platform-reviewer are fully accurate. The problems are almost all *documentation drift*, concentrated around two events the docs never caught up with: the watched-folders release (PR #32 added `fs:default`/`notification:default` bundles, two new DB tables, and an ad-hoc column-migration pattern) and general project maturation since v0.6.0 (tests, autostart, skip feedback now exist).

Highest-value issues:
1. sqlite-migration-reviewer asserts "no migration mechanism exists" — db.rs has had one since the source-fingerprint columns landed; the agent would actively misreview schema changes (High).
2. The "no `:default` bundles" invariant stated in CLAUDE.md, add-tauri-plugin, and acl-auditor is violated by capabilities/default.json (Medium, one root cause, likely fixable by deleting the two grants).
3. The Stop-hook version-sync warning goes to exit-0 stdout, which Stop hooks render only in transcript mode — it is effectively invisible (Medium).
4. The rustfmt PostToolUse hook whole-file reformatting keeps producing out-of-scope diffs on non-fmt-clean files (Medium, already a known pain point).
5. RECOMMENDATIONS.md (v0.6.0) and SPEC.md contain multiple confirmed-stale claims (tests, autostart, macOS-only, hardcoded HB paths).

## Recommendations

1. **Rewrite sqlite-migration-reviewer's "core trap" section** to document the actual pattern in db.rs (CREATE TABLE IF NOT EXISTS + idempotent `ALTER TABLE ADD COLUMN` + backfill UPDATE) and instruct the agent to derive the table list from db.rs instead of hardcoding three of five tables.
2. **Resolve the `:default` bundle drift at the source:** try removing `fs:default` and `notification:default` from capabilities/default.json (both plugins are Rust-side only); if anything breaks, document the exception in CLAUDE.md, add-tauri-plugin/SKILL.md, and acl-auditor.md instead.
3. **Make the version-sync Stop hook visible:** emit `{"decision":"block","reason":...}` JSON (or exit 2 + stderr) when manifests drift, so Claude actually sees and fixes it; also warn when only some manifests are readable.
4. **Defuse the rustfmt hook noise:** run `cargo fmt` once so the tree is fmt-clean, or change the hook to skip files that were not fmt-clean before the edit.
5. **Commit the release ask-gate:** move the three `Bash(...release.sh:*)` ask rules from the gitignored settings.local.json into the committed .claude/settings.json so the gate CLAUDE.md promises actually ships with the repo.
6. **Refresh the docs:** move the four shipped items in RECOMMENDATIONS.md to Implemented, delete the "no tests" bullet, and add a "superseded — historical spec" banner to SPEC.md (fixing the PATH-detection and macOS-only contradictions).
7. Minor hardening: fail closed (or warn) when `jq` is missing in the PreToolUse lock-file guard, and drop the obsolete `MultiEdit` matcher.

## Verification pass (2026-07-07)
- **Confirmed** — [High] sqlite-migration-reviewer.md:19 "no migration mechanism" is stale: db.rs:73-76 contains the `UPDATE jobs SET completed_at = created_at` backfill and db.rs:81-87 the idempotent `ALTER TABLE jobs ADD COLUMN` loop for `source_size`/`source_mtime` (ignoring "duplicate column name"), directly contradicting the agent's line-14 claim that added columns "will NOT apply to existing DBs". (The parenthetical "no PRAGMA user_version" is still literally true, but the blanket claim and the core-trap guidance are wrong for current code.)
- **Partial** — [Medium] sqlite-migration-reviewer.md:14,25 stale table list: the substance holds — db.rs:26-66 defines five tables (`jobs`, `settings`, `preset_suffixes`, `watched_directories`, `probe_cache`) while the agent hardcodes three — but the cited line is off by one: the list is at sqlite-migration-reviewer.md:24, not 25 (line 25 is the classify step).
- **Confirmed** — [Medium] add-tauri-plugin SKILL.md:15 "no `:default` bundles" drift: capabilities/default.json:28-29 grants `fs:default` and `notification:default`; grep of src/ finds zero `@tauri-apps/plugin-fs`/`plugin-notification` imports (only package.json deps), and both plugins are registered Rust-side at lib.rs:34,40 — matching the finding exactly.
- **Confirmed** — [Medium] CLAUDE.md "Permissions (ACL)" — "No `:default` bundles" is contradicted by default.json:28-29 (same evidence as the SKILL.md finding); per CLAUDE.md's own backend-only rule these Rust-only plugins (lib.rs:34,40, no frontend imports) should need no ACL entries.
- **Confirmed** — [Medium] check-version-sync.sh:13-15 Stop-hook warning invisible: the script writes the mismatch warning to stdout (line 13) and always `exit 0` (line 15); under the documented Stop-hook contract, exit-0 stdout is shown only in transcript mode (not surfaced to the user or fed back to Claude — that requires exit 2 + stderr or `{"decision":"block"}` JSON), so the warning is effectively invisible in normal use.
- **Confirmed** — [Medium] .claude/settings.json:20 rustfmt hook whole-file reformat: the hook runs `rustfmt --edition 2021 "$f"` on the entire edited file, and the tree is verifiably not fmt-clean today (`cargo fmt --check` in src-tauri reports a diff, e.g. src/handbrake.rs:298), so any edit to such a file produces out-of-scope reformatting — matching the documented pain point in project memory.
- **Confirmed** — [Medium] SPEC.md:336-341 hardcoded HandBrakeCLI fallbacks: lines 340-341 list `/usr/local/bin/HandBrakeCLI` and `/opt/homebrew/bin/HandBrakeCLI`, while src-tauri/src/handbrake.rs:13-21 (`detect_handbrake_path`) uses only `which`/`where` with no hardcoded paths — contradicting the spec and matching CLAUDE.md.
- **Confirmed** — [Medium] SPEC.md:307,323 macOS-only claim: line 307 says "Platform: macOS only" and line 323 lists "Windows/Linux support" as out of scope, contradicted by CLAUDE.md's Cross-Platform section and platform-gated code (e.g. `where` on Windows in handbrake.rs:15-17, Windows/Linux default presets in db.rs:11-19).
- **Confirmed** — [Medium] RECOMMENDATIONS.md:156 "Launch at login: Setting only, no actual registration" is stale: `tauri-plugin-autostart` is registered at lib.rs:38 and actively wired — commands/settings.rs:119-126 calls `app.autolaunch().enable()/disable()` when the `launch_at_login` setting changes, and settings.rs:62 reads the real plugin state. (Minor note: the autostart:allow-* ACL grants cited in the finding exist at default.json:23-25, though the calls are Rust-side.)
- **Confirmed** — [Medium] RECOMMENDATIONS.md:164 "No tests exist" is stale: 12 `*.test.*` Vitest files exist under src/ and 8 files in src-tauri/src contain `#[cfg(test)]` modules (e.g. db.rs:122-335 alone has five schema/migration tests).

**Tally:** 9 confirmed, 1 partial, 0 refuted (of 10).
