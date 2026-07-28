# Archive

Completed history. Nothing here is a task list — every document describes work that
shipped, or a decision that was made and is not being relitigated. Archived 2026-07-28,
after each document was verified against v1.0.0 source rather than against its own
checkboxes.

Live documents stay in `docs/`: `RECOMMENDATIONS.md` is now the only one — the backlog,
including larger unstarted ideas. `OPEN_ISSUES.md` was archived here on 2026-07-28 when
its single item (the Docker/web-UI server head) shipped.

## Read the code, not these docs, for current behaviour

Where an archived document and the code disagree, the code is right. Known divergences
are flagged inline in the documents themselves; the notable ones:

- **`superpowers/plans/2026-07-23-low-disk-pause-and-resume.md`** — states the pause
  reason is surfaced "event-only, no new command, no durable backend state". The shipped
  design does the opposite (`ConverterState.low_disk_pause` + `get_low_disk_pause`).
- **`superpowers/specs/2026-06-21-skip-by-source-media-design.md`** — mandates the setting
  default ON. It ships OFF, deliberately, because the check shells out to HandBrake per file.
- **`superpowers/plans/2026-07-25-bad-source-handling.md`** — Task 8's row markup was
  replaced wholesale by PR #114.

## A note on the checkboxes

Every plan here is full of unticked `- [ ]` boxes, and every one of them shipped anyway —
roughly 430 boxes, zero ticked. Plans were never updated during execution; each was
committed in the same commit as its own feature. **The merge commit is the completion
record, not the checkbox.** Treat checkbox state in these files as noise.

## Contents

| Path | What | Status |
|---|---|---|
| `SPEC.md` | Original design spec, pre-v0.1 | Largely superseded; several sections factually wrong vs. shipped code (progress is read from **stdout**, not stderr; 5 DB tables not 3; cross-platform `Child::kill()` not `SIGTERM`; ~20 commands missing from its list) |
| `fable-review/` | Two rounds of full-codebase adversarial review, 2026-07-07/08 | Closed — all accepted findings shipped in PRs #63–#84. `DECISIONS.md` inside is **live**, not historical |
| `superpowers/specs/` | Design specs, one per feature | All shipped |
| `superpowers/plans/` | Phased implementation plans | All shipped, v0.13–v0.19 |

### Shipped features by document

| Feature | Plan / spec date | Shipped |
|---|---|---|
| Test setup (Vitest + Rust) | 2026-06-17 | Phases 0–3; Phase 4 later covered by PR #78's mock-runtime harness |
| Release script | 2026-06-18 | `scripts/release.sh` |
| In-place re-encode | 2026-06-21 | v0.13 era |
| Skip by source media | 2026-06-21 | Ships OFF by default (see above) |
| Probe-once cache | 2026-06-22 | `probe_cache.rs` |
| Add-progress indicator | 2026-07-17 | v0.15.0 (#99) |
| Serialized folder intake | 2026-07-22 | v0.16.0 (#103) |
| Low-disk pause and resume | 2026-07-23 | v0.17.0 (#105) — no design spec; over-delivered vs. its plan |
| Queue-pause persistence | 2026-07-23 | v0.18.0 (#108) |
| Bad-source handling | 2026-07-25 | v0.19.0 (#111), hardened by #110 and #114. Phase 3 deliberately deferred |
