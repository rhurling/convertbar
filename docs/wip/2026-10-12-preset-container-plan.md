# Preset import file and container from the preset — plan

Ephemeral: deleted by /finish. Executes docs/wip/2026-10-12-preset-container-spec.md (ADRs 0002, 0004). Progress is the commit history; ledger lines below point at commits.

Baseline (2026-10-12, at 3d5b9a7's tree plus spec fixes): `npm test` 38 files / 359 tests green, `npm run build` clean, `cargo test --workspace` 636 passed, 4 ignored, 0 failed. CI gates on nothing else.

## Slices

**1. Container from the preset (tracer).** Blocked by: nothing.
The preset metadata gains the container read from `FileFormat`, and every add uses it. Under an MKV preset a source gets a `.mkv` output. An `.mkv` source with an empty suffix is in place, and its in-place temp keeps the source's extension through encode, crash recovery and cancel. An unknown format refuses the add. A literal-suffix add now resolves HandBrake and fails with the not-found wording when it is absent. The frontend's metadata type carries the field, which defaults to mp4. Accepted when the intake tests show those names and in-place decisions, and the queue-thread tests show `-o` and recovery using the right extension.

— Checkpoint 1 after slice 1 —

**2. Preset import in export and encode.** Blocked by: 1.
With an import path on the shared context, the metadata fetch and the encode both pass `--preset-import-file`. Each first runs a pre-flight that re-reads the file and refuses with an Environment error naming the file when the file is missing or invalid, the preset is in neither the file nor the built-ins, or an imported name collides with a built-in. A refused encode books the job and the queue moves on; a refused add fails. Accepted when the queue-thread tests show the flag exactly when configured, and a deleted file or removed preset books Environment without spawning HandBrake.

**3. Server startup and environment.** Blocked by: 2.
The server reads `CONVERTBAR_PRESET_FILE`. Between building the context and booting, it refuses to start, naming the cause, for a missing file, invalid JSON, no presets, an unresolvable HandBrake, or a collision. A valid file starts with the import path and the built-in names set on the context. Accepted when the config test reads the variable and the startup tests (declared locator worlds) cover each cause and the valid start.

**4. Listing presets.** Blocked by: 3.
Both heads' preset list returns the imported names first, read from the JSON (folders recursed, both entry types), then the built-ins listed without the flag, with no duplicates. A collision is an error. Accepted when the list-route test against a real file and a stub list shows that order and the error.

— Checkpoint 2 after slice 4 —

**5. User-visible text and docs.** Blocked by: 1, 3.
The settings page's naming preview and in-place hint follow the selected preset's container instead of saying `.mp4`. The README environment table, the compose example and the Unraid template document `CONVERTBAR_PRESET_FILE`. Accepted when the frontend tests show the preview with an MKV container.

**6. Live end-to-end run.** Blocked by: 4.
An ignored test drives the server head with a real preset file and the host's HandBrakeCLI. An MKV preset that keeps every track replaces an `.mkv` source in place and writes an `.mkv` beside an `.mp4` source, and ffprobe shows the track counts kept. Its output is the recorded artifact.

— Final review after slice 6 —

## Ledger
- Slice 1: done at ac89b3b
- Checkpoint 1: BASE 21a5ebb, reviewed HEAD ac89b3b; next interval BASE ac89b3b
- Checkpoint 1 fix pass at 80e8c7f: fixed 5 (mapping table test, serde-default test, dead media-skip branch, stale temp comment, resolver error propagation); rejected 6 per spec/ADR 0004 (cache per name, unknown FileFormat refuses, watcher export cost, old-extension naming, extensionless fallback, serde default by design); both new tests mutation-proven
- Slice 2: done at 2794473
- Slice 3: done at b551e33 (live: real binary refuses a missing file and a real built-in collision)
- Slice 4: done at ae0e76a
- Checkpoint 2: BASE ac89b3b, reviewed HEAD ae0e76a; next interval BASE ae0e76a
- Checkpoint 2 fix pass at 8e002d6: fixed 7 (shared preset-file reader + one collision message, listing judged against the startup snapshot, de-duplicated names, add-side test proves HandBrake not asked, per-cause refusal assertions, trimmed + canonical env path, spec line on list refresh); rejected 7 per spec (per-job error rows, cache vs edits, Type 0 collision refusal, stale built-in snapshot, per-job parse, ignored real-HB test args, cross-module stub copies); snapshot and dedupe mutation-proven
- Slice 5: done at 43b3d90
- Slice 6: done at 7a5fed2 (live, HandBrakeCLI 1.11.2: in-place mkv and mkv beside mp4, both h264→hevc with 1v/2a/2s kept; dropping the import args from the spawn turns it red)
