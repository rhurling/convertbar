# Skip queued files by source codec + resolution — Design

**Date:** 2026-06-21
**Status:** Approved design, ready for implementation plan
**Source issue:** `docs/OPEN_ISSUES.md` → "Skip queued files by source codec + resolution"
**Depends on:** `2026-06-21-in-place-reencode-design.md` (Part B — Skip-reason feedback).
This feature's user feedback rides on the `AddResult` / `SkipReason` scaffolding that spec
introduces and cannot be built independently of it — see **Dependency** below.

## Problem

When files are added to the queue, ConvertBar always converts them — even when the
source is already at or below the target preset's codec and resolution, so the
conversion wastes CPU and produces no benefit (`decide_cleanup` would just keep the
original and mark the job "skipped" *after* burning the encode). The app does not
probe sources today: it only classifies the *target preset* via `classify_preset`
(`src-tauri/src/handbrake.rs`). This feature adds source introspection so wasteful
conversions are skipped *before* they start.

## The skip rule (core, pure, table-testable)

A new pure function decides skip from four inputs:
`(source_codec, source_height, target_codec, target_height)`. It mirrors the style of
`decide_cleanup` (`src-tauri/src/converter.rs:56`) — no I/O, exhaustively unit-tested.

```
resolution_would_help = target_height > 0 AND source_height > target_height
codec_would_help      = efficiency_rank(target) > efficiency_rank(source)
skip  ⟺  NOT resolution_would_help AND NOT codec_would_help
```

We skip only when **neither** dimension would benefit from re-encoding.

Worked cases:

| Source | Target | resolution_would_help | codec_would_help | Result |
|---|---|---|---|---|
| AV1 1080p | h265 1080p | no | no | **skip** (the waste case the feature targets) |
| h264 1080p | h265 1080p | no | yes | convert (common, real size win) |
| ProRes 1080p | h265 1080p | no | yes | convert (ProRes ranks lowest) |
| h265 720p | h265 1080p (cap) | no (no upscale) | no | **skip** |
| h265 4K | h265 1080p (cap) | yes (downscale) | no | convert |
| h265 1080p | h264 1080p | no | no | **skip** (downgrade saves nothing) |

### Efficiency rank

| Rank | Codecs |
|---|---|
| 3 | av1, h265 / hevc, vp9 |
| 2 | h264 |
| 1 | mpeg2, mpeg4, other lossy |
| 0 | prores, dnxhr, ffv1 (intermediate / lossless — always re-encode to a delivery codec) |

Codec slugs reuse the **same vocabulary** `classify_preset` already emits
(`h265`/`h264`/`av1`/`vp9`/`prores`/`dnxhr`/`ffv1`/`unknown`), so target and source
compare in one namespace.

### Resolution semantics

`target_height` comes from the preset's `PictureHeight` (already read by
`classify_preset`). A preset with no cap reports `PictureHeight == 0`; we treat that as
"no downscale benefit possible" (`resolution_would_help = false`), so the decision falls
to codec alone. `target_height > 0 AND source_height > target_height` is the only case
where downscaling shrinks the file.

## Safety default: never skip on uncertainty

If the probe fails, the source codec is unrecognized (`unknown`), or the source height
is unavailable, the corresponding `*_would_help` is forced to **true** so the file is
**queued, never skipped**. We only skip when we are confident the conversion is
wasteful. This is the single most important correctness property and is encoded
directly in the tests (a skip must never drop a file that could have benefited).

## Compatibility posture

The efficiency-rank rule means a deliberate "re-encode an already-efficient codec for an
old device" (e.g. AV1 → h265) would be skipped. The escape hatch is the toggle itself:
turn **Skip files already at/below target** off (globally or for that batch). We are
**not** building a separate "force codec" override — one toggle, YAGNI.

## Default ON — conscious behavior change

The setting defaults to `"true"`. After an update, existing users' adds — including
watched-folder auto-adds — begin probing and skipping automatically, and a
compatibility-driven user must actively turn it off. Accepted as the right default for a
space-saving app.

## Architecture & data flow

- **Probe** — new `probe_source(hb_path, file) -> Option<SourceMedia { codec, height }>`
  in `handbrake.rs`, shelling out `HandBrakeCLI --scan --json -i <file>` and parsing the
  scanned title's video codec + height into the shared codec-slug vocabulary.
- **Comparison** — pure `should_skip_by_media(...)` + `efficiency_rank(...)` (in
  `handbrake.rs` or a small sibling module). No I/O. Exhaustively tested.
- **Wiring** — in `add_files_inner` (`queue.rs`), **outside the DB lock**, beside the
  existing suffix-resolution shell-out (`queue.rs:122`): read the new setting and the
  target preset's codec/height (from the already-fetched / cached `PresetMetadata`),
  then probe each candidate **sequentially**. Files that should-skip are filtered out
  before `add_files_to_db` inserts them. The DB-only core stays pure / in-memory testable.
- **Feedback** — ephemeral, never persisted. This rides on the shared per-reason skip
  feedback introduced by `2026-06-21-in-place-reencode-design.md` (Part B): the add core
  returns `AddResult { added, skipped: Vec<SkipCount> }`, and this feature contributes one
  new `SkipReason` variant (e.g. `AlreadyAtTarget`) emitted at its probe-skip point.
  `DropZone` renders it in the same per-reason summary line
  (`… · N skipped (already at target)`); watched-folder adds ignore `.skipped` (or log
  it). We do **not** add a parallel count-only return — see **Dependency** below.

## Settings & UI

- New setting key `skip_by_source_media` (boolean), default `"true"`, seeded in the
  `db.rs` settings defaults.
- One checkbox in `SettingsPage.tsx` mirroring the `skip_already_converted` block.
- The boolean added to the settings type in `src/lib/tauri.ts` and the defaults in
  `src/hooks/useSettings.test.ts`.

## Testing

- Table tests for `should_skip_by_media` over every rank pairing × resolution
  combination, plus the uncertainty cases (unknown codec / failed probe / missing height
  → never skip). Each assertion encodes *why*: skipping must never drop a file that would
  have benefited.
- `efficiency_rank` ranks intermediate codecs (ProRes/DNxHR/FFV1) below h264.
- Probe parsing tested against a **captured `--scan` JSON fixture** — no live HandBrake
  in the test suite.

## Risks / to verify during planning

- **Source codec availability from HandBrake scan.** The plan must confirm that
  `HandBrakeCLI --scan --json` reliably exposes the source video codec name (and height)
  in a parseable field. The whole feature leans on this to avoid adding a new `ffprobe`
  dependency. If the scan JSON does not expose codec cleanly, revisit before building —
  this is the one open implementation unknown.

## Dependency: the in-place-reencode spec

This spec **requires** `2026-06-21-in-place-reencode-design.md` (branch
`feature/in-place-reencode-spec`). Its **Part B — Skip-reason feedback** changes
`add_files_to_db`'s return from `Vec<JobInfo>` to
`AddResult { added: Vec<JobInfo>, skipped: Vec<SkipCount> }` with
`enum SkipReason { NotVideo, AlreadyQueued, AlreadyConverted, OutputExists }`, surfaced
per-reason in `DropZone` and never written to history.

This feature **extends that enum with one variant** (`AlreadyAtTarget`) and emits it at its
probe-skip point — it does not define its own feedback channel. Both specs also edit the
same `add_files_to_db` skip loop and the same return type, so they cannot be built
independently without conflict.

**Build order:** land the in-place-reencode spec's Part B scaffolding first
(`AddResult` / `SkipReason`), then this feature rebases onto it and adds its reason variant
plus the probe/skip logic. If priorities flip, this spec's plan must carry the
`AddResult` / `SkipReason` introduction itself and the in-place spec rebases instead.

## Follow-ups (out of scope for this spec)

- **Parallel probing.** Probing is sequential in this design. Large manual folder adds
  pay one `--scan` per file in series. Bounded-concurrency parallel probing is a
  deferred optimization — revisit if big-batch add latency becomes a complaint.
</content>
</invoke>
