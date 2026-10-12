# Preset import file and container from the preset — spec

Ephemeral: deleted by /finish. Cycle 2 of 3 for the Mac mini's re-encoding (dotfiles spec "ConvertBar moves from Unraid to the Mac mini", contract item 2). Cycle 1 is the signed server asset (a PR, no spec); cycle 3 is remote roots over ssh (its own spec), which builds on this cycle's naming.

## Problem

ConvertBar passes HandBrakeCLI only `-Z <preset>` and names every output `.mp4`. Two consequences, both verified against HandBrake 1.11.2 on 2026-10-12:

- A custom preset cannot be used. `-Z <custom name>` alone answers "Invalid preset"; a custom preset loads only together with `--preset-import-file`. The built-in presets keep only the first audio track (as stereo AAC) and drop every subtitle, which the mini's re-encodes must not do (dotfiles ADR 0119).
- HandBrake picks the container from the output's extension, so an MKV preset writes MP4 when the output is named `.mp4`, losing whatever MP4 cannot carry (ASS/PGS subtitles, DTS/TrueHD audio).

Two further facts from probing the CLI:

- `--preset-list` with an import file prints an imported, uncategorised preset at column 0 and its description at four spaces. ConvertBar's list parser keeps exactly the four-space lines, so it would offer the description text as presets and never the preset itself.
- When an imported preset has a built-in's name, the built-in wins silently: an imported "Fast 1080p30" set to MKV still exports as x264/MP4.

## Solution

The server head can import one preset file, named by an environment variable (ADR 0002). Every HandBrake call that resolves a preset name passes that file. A preset's `FileFormat` decides the output extension, which is fixed when the job is added (ADR 0004). Output naming and the in-place check follow that extension, in both heads.

## Behavior

### The preset file

- The server reads `CONVERTBAR_PRESET_FILE`. Unset means no import, which is exactly today's behaviour. The desktop head never imports a file.
- At startup, when the variable is set, the server refuses to start, naming the cause, if the file:
  - is missing or unreadable;
  - is not valid JSON;
  - defines no presets;
  - defines a preset whose name equals a built-in's. That check runs when HandBrake can be resolved at startup; without HandBrake nothing can encode, and the collision is checked again at listing time (below).
- Imported preset names are read from the file's own JSON (`PresetList`, recursing into folders' `ChildrenArray`), never parsed out of `--preset-list`.
- Edits to the file take effect after a restart. Preset metadata is cached per name for the life of the process.

### Listing presets

- The preset list (both heads' "list HandBrake presets" command and the HTTP route) returns the imported names first, then the built-ins from `--preset-list` run without the import flag, so no name appears twice.
- An imported name that collides with a built-in is a listing error with the same wording as the startup refusal. This only happens if the file changed after startup.

### Preset metadata and the container

- `--preset-export` (the metadata fetch behind suffix templates and the source-media skip) passes `--preset-import-file` when a file is configured.
- The metadata gains the container, read from the preset's `FileFormat`:

  | `FileFormat` | Extension |
  |---|---|
  | `av_mp4` | `mp4` |
  | `av_mkv` | `mkv` |
  | `av_webm` | `webm` |
  | missing | `mp4` |
  | anything else | the add fails with an error naming the preset and the value |

### Adding a job

- Every add resolves the preset's metadata, not only adds with a templated suffix or the source-media skip, because the extension needs it. An add of zero paths still decides nothing and never reaches HandBrake. An add when HandBrake is missing fails with today's "HandBrake not found" wording.
- The output name is `{stem}{suffix}.{ext}`. Renumbering on a taken name keeps the same shape: `{stem} ({n}){suffix}.{ext}`.
- A job is in place exactly when that name equals the source path. An `.mkv` source under an MKV preset with an empty suffix is now in place; under an MP4 preset it gets a distinct `.mp4` output, as today. The existing in-place rules apply unchanged: never queued under `keep`, and dropped from the queue when the mode becomes `keep`.
- The extension is fixed in the job's stored output path, like the preset name. A queued job keeps it even if the preset file later changes the container.

### Encoding

- HandBrake is spawned with `-Z <job's preset>`, plus `--preset-import-file <file>` when one is configured, and `-o` with the stored output path, or for an in-place job with its temp path.
- The in-place temp path keeps the source's own extension, `.{stem}.convertbar-tmp.{ext}`. For an in-place job that extension equals the container. Crash recovery and the pre-encode clearing of a stale temp use the same path, so a `.mkv` in-place job's partial is cleaned up rather than looked for under `.mp4`.
- No `--format` flag. The extension already selects the container, and adding one would let the stored name and the container disagree.

## Implementation decisions

- The context shared by both heads carries an optional preset-import path. The server sets it from the environment and the desktop passes none. Nothing reads it from the settings table, and it is not in the allowed settings keys (ADR 0002).
- The import path is threaded into the three preset-resolving HandBrake calls (list, export, encode) and nowhere else. The source scan behind probing and purge needs no preset.
- The container is part of the preset metadata and cached with it. The output-name and in-place-temp helpers take an extension instead of the hard-coded `mp4`.
- The "literal suffix never resolves HandBrake" guarantee is reversed on purpose (ADR 0004). Its test changes intent: a literal suffix now resolves HandBrake, and an add with HandBrake absent fails with the not-found wording.
- No schema change: the stored output path already carries the full name.

## Testing decisions

The seams, the only places tests drive this cycle:

- **Core intake** (the add entry point, with the preset cache seeded with a container, or a stub HandBrake that answers `--preset-export`):
  - output names for `.mp4`, `.mkv` and `.wmv` sources under MP4 and MKV presets, with a literal and an empty suffix;
  - renumbering keeps the extension;
  - the in-place decision (`.mkv` + MKV + empty suffix is in place; under `keep` it is not queued);
  - an unknown `FileFormat` fails the add;
  - a literal-suffix add with HandBrake absent fails with the not-found wording.
- **The queue thread** (`process_queue` with the existing recording fake-HandBrake scripts):
  - the spawned arguments carry `--preset-import-file` exactly when one is configured;
  - `-o` carries the stored extension, or `.{stem}.convertbar-tmp.mkv` for an in-place MKV job;
  - an interrupted in-place MKV job's temp is removed by recovery.
- **Server startup**, through the injected-environment config path: each refusal cause (missing, invalid JSON, no presets, collision) stops startup with its cause named; unset starts as today.
- **The preset-list route**, against a real preset file and a stub HandBrake list: imported names first, no duplicates, folders recursed.

Medium-hard end-to-end case, kept as a recorded artifact: the server head with a real preset file defining an MKV preset that keeps every audio and subtitle track, queuing an `.mkv` source with an empty suffix and an `.mp4` source. The first is replaced in place as MKV and the second gets an `.mkv` beside it, and ffprobe shows the track counts preserved. This is `#[ignore]`d, since it needs the host's HandBrakeCLI.

## Out of scope

- The preset file's contents (the dotfiles side writes it).
- Remote roots, staging, and hook payload changes (cycle 3).
- The signed darwin-arm64 server asset (cycle 1).
- A UI for choosing or uploading a preset file, and importing a preset file in the desktop head.
- Re-deriving the container for jobs already queued when the preset file changes.

## Open questions

- None blocking. The collision check at startup needs HandBrake to list its built-ins; when HandBrake is absent at startup it is deferred to listing time, which is accepted because nothing can encode then anyway.

## ADRs

- 0002: remote roots and the preset file are configured by env, not the API
- 0004: the output container comes from the preset and is fixed at add time
- (0001 and 0003 belong to cycles 1 and 3; they were recorded in the same session.)
