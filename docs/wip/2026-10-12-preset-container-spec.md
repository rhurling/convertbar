# Preset import file and container from the preset — spec

Ephemeral: deleted by /finish. Cycle 2 of 3 for the Mac mini's re-encoding (dotfiles spec "ConvertBar moves from Unraid to the Mac mini", contract item 2). Cycle 1 is the signed server asset (a PR, no spec); cycle 3 is remote roots over ssh (its own spec), which builds on this cycle's naming.

## Problem

ConvertBar passes HandBrakeCLI only `-Z <preset>` and names every output `.mp4`. Two consequences, both verified against HandBrake 1.11.2 on 2026-10-12:

- A custom preset cannot be used. `-Z <custom name>` alone answers "Invalid preset"; a custom preset loads only together with `--preset-import-file`. The built-in presets keep only the first audio track (as stereo AAC) and drop every subtitle, which the mini's re-encodes must not do (dotfiles ADR 0119).
- HandBrake picks the container from the output's extension, so an MKV preset writes MP4 when the output is named `.mp4`, losing whatever MP4 cannot carry (ASS/PGS subtitles, DTS/TrueHD audio).

Further facts from probing the same CLI:

- How `--preset-list` prints an imported preset depends on the file. A custom preset (`Type: 1`) appears under a `My Presets/` heading at four spaces, the shape ConvertBar's list parser keeps. A preset exported from a built-in (`Type: 0`) appears at column 0 with its description at four spaces, so the parser would offer the description as a preset and drop the preset itself.
- When an imported preset has a built-in's name, the built-in wins silently, at export and at encode: an imported "Fast 1080p30" set to MKV still exports as x264/MP4.
- HandBrake reads the import file on every invocation. A file that has become invalid JSON is skipped with "Preset import failed" and **exit 0**. HandBrake then carries on with the built-ins, so a colliding name encodes the built-in and an imported name fails as "Invalid preset". That message sits early in a stderr of about 21 KB, outside the 8 KB tail ConvertBar classifies, so today it would land as `Unknown` with the preset list as its message.
- A preset without `FileFormat` is rejected at import ("invalid preset format"), so the field is always present in practice.
- `-O` (MP4 fast start), which ConvertBar always passes, is harmless for MKV output: exit 0, Matroska written.

## Solution

The server head can import one preset file, named by an environment variable (ADR 0002). Every HandBrake call that resolves a preset name passes that file, after ConvertBar has checked the file and the name itself. A preset's `FileFormat` decides the output extension, which is fixed when the job is added (ADR 0004). Output naming and the in-place check follow that extension, in both heads.

## Behavior

### The preset file

- The server reads `CONVERTBAR_PRESET_FILE`. Unset means no import, which is exactly today's behaviour. The desktop head never imports a file.
- Imported preset names are read from the file's own JSON (each entry's `PresetName` in `PresetList`, recursing into folders' `ChildrenArray`), never parsed out of `--preset-list`.
- At startup, when the variable is set, the server refuses to start, naming the cause, if:
  - the file is missing or unreadable;
  - the file is not valid JSON;
  - the file defines no presets;
  - HandBrake cannot be resolved (with a preset file configured there is nothing useful to run, and the collision check below needs it);
  - an imported name equals a built-in's (the built-in names come from `--preset-list` without the import flag; this list is kept for the life of the process).
- ConvertBar reads the file's names once at startup. HandBrake reads the file on every call. So before every `--preset-export` and every encode, ConvertBar re-reads the file and refuses with an Environment error naming the file and the cause when:
  - the file is missing, unreadable or not valid JSON;
  - the preset in use is neither in the file nor a built-in;
  - the preset in use is in the file and is also a built-in's name.

  An encode refused this way books the job as an Environment error with that message and the queue moves on. An add refused this way fails the add with that message. This keeps the failure out of HandBrake's 21 KB stderr and out of `Unknown`.
- The preset list re-reads the file on every request, so a preset added to the file is listed at once. Changing what metadata (suffix, container) an already-cached preset yields, or HandBrake's own built-ins, needs a restart. The pre-flight above keeps an edited file from silently encoding something else.

### Listing presets

- The preset list (both heads' "list HandBrake presets" command and the HTTP route) returns the imported names first, then the built-ins. That is two sources, not two HandBrake calls with the flag: the names come from the file, and the built-ins from `--preset-list` run without the import flag, so no name appears twice.
- If the file changed after startup so that a name now collides, the list fails with the same wording as the startup refusal.

### Preset metadata and the container

- `--preset-export` (the metadata fetch behind suffix templates and the source-media skip) passes `--preset-import-file` when a file is configured.
- The metadata gains the container, read from the preset's `FileFormat`:

  | `FileFormat` | Extension |
  |---|---|
  | `av_mp4` | `mp4` |
  | `av_mkv` | `mkv` |
  | `av_webm` | `webm` |
  | missing | `mp4` (defensive; HandBrake rejects such a file at import) |
  | anything else | the add fails with an error naming the preset and the value |

- The metadata crosses the frontend boundary: the settings page sends it back to resolve a suffix preview. The container field defaults to `mp4` when absent, so an older payload still deserialises, and the frontend's metadata type gains it. The test helper that seeds the metadata cache seeds `mp4`, so existing suffix assertions stay as they are.
- The container is not a suffix-template variable.

### Adding a job

- Every add resolves the preset's metadata, not only adds with a templated suffix or the source-media skip, because the extension needs it. An add of zero paths still decides nothing and never reaches HandBrake. An add when HandBrake is missing fails with today's "HandBrake not found" wording.
- The output name is `{stem}{suffix}.{ext}`. Renumbering on a taken name keeps the same shape: `{stem} ({n}){suffix}.{ext}`.
- A job is in place exactly when that name equals the source path. An `.mkv` source under an MKV preset with an empty suffix is now in place; under an MP4 preset it gets a distinct `.mp4` output, as today. The existing in-place rules apply unchanged: never queued under `keep`, and dropped from the queue when the mode becomes `keep`.
- The comparison stays byte-wise, as it is for `.MP4` today. An upper-case `.MKV` source under an MKV preset is therefore not in place. On a case-insensitive volume its default name already exists, so it is renumbered to `{stem} (1).mkv`, and the original is removed after a winning encode.
- The extension is fixed in the job's stored output path, like the preset name. A queued job keeps it even if the preset file later changes the container.
- Consequences, accepted:
  - A watched folder's intake fails the same way when HandBrake is missing, and the watcher drops the settled files until its next startup scan. That already happens for a templated suffix and now applies to every suffix.
  - The first add per preset name per process shells out once for metadata, even when every path ends up skipped.

### Encoding

- After the pre-flight, HandBrake is spawned with `-Z <job's preset>`, plus `--preset-import-file <file>` when one is configured, `-O` as today, and `-o` with the stored output path, or for an in-place job with its temp path.
- The in-place temp path is derived from the source path and keeps its extension, `.{stem}.convertbar-tmp.{ext}`. For an in-place job that extension equals the container. Crash recovery, cancel, and the pre-encode clearing of a stale temp all derive it the same way, so a `.mkv` in-place job's partial is cleaned up rather than looked for under `.mp4`.
- No `--format` flag. The extension already selects the container, and adding one would let the stored name and the container disagree.

### User-visible text

- The settings page's naming preview and in-place hint stop saying `.mp4` and use the selected preset's container.
- The server documentation (README environment table, the compose example, the Unraid template) gains `CONVERTBAR_PRESET_FILE`.

## Implementation decisions

- The context shared by both heads carries an optional preset-import path and the built-in name list, both empty by default, so the existing construction sites need no change. The server sets them after building the context and before boot. Nothing reads the path from the settings table, and it is not in the allowed settings keys (ADR 0002).
- The import path is threaded into the export and encode calls only. The built-in listing deliberately runs without it, and the source scan behind probing and purge needs no preset.
- The container is part of the preset metadata and cached with it. The output-name helper takes an extension instead of the hard-coded `mp4`; the in-place-temp helper derives it from the source path.
- The "literal suffix never resolves HandBrake" guarantee is reversed on purpose (ADR 0004). Its test changes intent: a literal suffix now resolves HandBrake, and an add with HandBrake absent fails with the not-found wording.
- No schema change: the stored output path already carries the full name.

## Testing decisions

The seams, the only places tests drive this cycle:

- **Core intake** (the add entry point, with the preset cache seeded with a container, or a stub HandBrake that answers `--preset-export`):
  - output names for `.mp4`, `.mkv` and `.wmv` sources under MP4 and MKV presets, with a literal and an empty suffix;
  - renumbering keeps the extension;
  - the in-place decision (`.mkv` + MKV + empty suffix is in place; under `keep` it is not queued);
  - an unknown `FileFormat` fails the add;
  - a literal-suffix add with HandBrake absent fails with the not-found wording;
  - a cache-miss add whose preset file has gone missing fails with the pre-flight's wording.
- **The queue thread** (`process_queue` with the existing recording fake-HandBrake scripts):
  - the spawned arguments carry `--preset-import-file` exactly when one is configured;
  - `-o` carries the stored extension, or `.{stem}.convertbar-tmp.mkv` for an in-place MKV job;
  - an interrupted in-place MKV job's temp is removed by recovery;
  - a job whose preset file was deleted, or whose preset was removed from it, after startup is booked as Environment with the file named, and HandBrake is never spawned.
- **Server config**, through the injected-environment parsing: the variable is read; unset changes nothing.
- **Server startup**, after the context is built and before boot, with the world declared (`StubLocator` plus a stub HandBrake script that answers `--preset-list`, and `AbsentLocator` for the unresolvable case): each refusal cause (missing, invalid JSON, no presets, HandBrake unresolvable, collision) stops startup with its cause named, and a valid file starts.
- **The preset-list route**, against a real preset file and a stub HandBrake list: imported names first, no duplicates, folders recursed, both `Type 0` and `Type 1` entries listed.

Medium-hard end-to-end case, kept as a recorded artifact: the server head with a real preset file defining an MKV preset that keeps every audio and subtitle track, queuing an `.mkv` source with an empty suffix and an `.mp4` source. The first is replaced in place as MKV and the second gets an `.mkv` beside it, and ffprobe shows the track counts preserved. This is `#[ignore]`d, since it needs the host's HandBrakeCLI.

## Out of scope

- The preset file's contents (the dotfiles side writes it).
- Remote roots, staging, and hook payload changes (cycle 3).
- The signed darwin-arm64 server asset (cycle 1).
- A UI for choosing or uploading a preset file, and importing a preset file in the desktop head.
- Re-deriving the container for jobs already queued when the preset file changes.
- Making the in-place comparison case-insensitive (pre-existing behaviour).

## Open questions

- None blocking.

## ADRs

- 0002: remote roots and the preset file are configured by env, not the API
- 0004: the output container comes from the preset and is fixed at add time
- (0001, 0003, 0005 and 0006 belong to cycles 1 and 3; they were recorded in the same session.)
