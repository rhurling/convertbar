# Skip Queued Files by Source Codec + Resolution — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Skip adding a queued file when its source codec + resolution already meet or exceed the target preset, so ConvertBar never wastes an encode that produces no benefit.

**Architecture:** A new pure-policy module (`media_skip.rs`) decides skip from `(source_codec, source_height, target_codec, target_height)` using a codec efficiency rank + resolution comparison, with "never skip on uncertainty" baked in. HandBrake source introspection (`probe_source` via `HandBrakeCLI --scan --json`) lives in `handbrake.rs` and feeds normalized slugs into that policy. The decision is wired into `add_files_inner` **outside** the DB lock, and skipped files are surfaced through the shared `AddResult`/`SkipReason` feedback channel.

**Tech Stack:** Rust (Tauri backend, `serde_json`, `rusqlite`, `cargo test`), TypeScript/React frontend (Vitest, `tsc`).

---

## Preconditions / Dependency

This plan **depends on** the in-place-reencode plan — **`docs/superpowers/plans/2026-06-21-in-place-reencode.md`** (branch `feature/in-place-reencode-spec`). The specific tasks of that plan this one builds on:

- **Its Task 1 — `AddResult` / `SkipReason` / `SkipCount` types** in `src-tauri/src/types.rs`. **Already implemented** (commit `c7b490b`). The real definitions:

  ```rust
  #[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
  #[serde(rename_all = "snake_case")]
  pub enum SkipReason { NotVideo, AlreadyQueued, AlreadyConverted, OutputExists }

  #[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
  pub struct SkipCount { pub reason: SkipReason, pub count: u32 }

  #[derive(Debug, Clone, Serialize, Deserialize)]
  pub struct AddResult { pub added: Vec<JobInfo>, pub skipped: Vec<SkipCount> }
  ```

  Note `#[serde(rename_all = "snake_case")]` — the `AlreadyAtTarget` variant my Task 9 adds serializes to the string **`"already_at_target"`** on the frontend (used in Task 10).

- **Its Task 2 — `add_files_to_db` returns `AddResult`** (`src-tauri/src/commands/queue.rs`) with per-reason skip counting, and `add_files` / `confirm_folder_add` returning `AddResult`. My Task 9 wires the media-skip into this updated signature and merges an `AlreadyAtTarget` count.
- **Its frontend tasks** — `src/lib/tauri.ts` exports `SkipReason`/`SkipCount`/`AddResult` and makes `addFiles`/`confirmFolderAdd` return `AddResult`; `src/components/DropZone.tsx` consumes `AddResult` and renders the per-reason summary. My Task 10 adds the `already_at_target` label to that summary.

**Status / ordering (as of 2026-06-21):** that plan's **Task 1 is committed** on its branch (`c7b490b`); **Task 2 + the frontend tasks are in progress**. **Tasks 1–8 below have NO dependency on it** — pure logic + probe + the setting — and can be implemented and committed now. **Tasks 9–10 are BLOCKED** until that branch's Task 2 + frontend land; the clean path is for both branches to merge to `main` (or for this branch to rebase onto the in-place branch) before starting Task 9. If priorities flip, port the in-place plan's Task 1 (types) + Task 2 (`add_files_to_db -> AddResult`) signature change into this plan first.

**No ACL changes:** the setting reuses the existing `get_settings`/`update_setting` commands, and `probe_source` is a backend-only `Command` shell-out. Neither needs a `capabilities/default.json` entry.

---

## File Structure

| File | Responsibility | Created/Modified |
|---|---|---|
| `src-tauri/src/media_skip.rs` | Pure skip policy: `SourceMedia`, `efficiency_rank`, `target_height_from_resolution`, `should_skip_by_media`, `select_media_skips` | **Create** |
| `src-tauri/src/lib.rs` | Register `mod media_skip;` | Modify (1 line) |
| `src-tauri/src/handbrake.rs` | Source introspection: `normalize_source_codec`, `parse_scan_media`, `probe_source` | Modify |
| `src-tauri/src/types.rs` | Add `skip_by_source_media` to `Settings` | Modify (1 line) |
| `src-tauri/src/db.rs` | Seed `skip_by_source_media` default `"true"` + assert it | Modify |
| `src-tauri/src/commands/settings.rs` | Read/return `skip_by_source_media`; add to `ALLOWED_KEYS` | Modify |
| `src-tauri/src/commands/queue.rs` | Wire probe + media-skip into `add_files_inner`; merge `AlreadyAtTarget` into `AddResult` | Modify (Task 9, blocked) |
| `src/lib/tauri.ts` | Add `skip_by_source_media: boolean` to `AppSettings` | Modify (1 line) |
| `src/hooks/useSettings.test.ts` | Add `skip_by_source_media: true` to the defaults helper | Modify (1 line) |
| `src/pages/SettingsPage.tsx` | Add the toggle checkbox | Modify (Task 10) |
| `src/components/DropZone.tsx` | Render the `AlreadyAtTarget` reason label | Modify (Task 10, blocked) |

**Test commands** (used throughout):
- Rust: `cargo test --manifest-path src-tauri/Cargo.toml <name>`
- Frontend type-check/build: `npm run build`
- Frontend tests: `npm test`

---

## Task 1: Create `media_skip` module with `SourceMedia` + `efficiency_rank`

**Files:**
- Create: `src-tauri/src/media_skip.rs`
- Modify: `src-tauri/src/lib.rs` (add `mod media_skip;`)
- Test: inline `#[cfg(test)]` in `media_skip.rs`

- [ ] **Step 1: Register the module**

In `src-tauri/src/lib.rs`, add `mod media_skip;` so the module list reads:

```rust
mod commands;
mod converter;
mod db;
mod handbrake;
mod media_skip;
mod types;
mod watcher;
```

- [ ] **Step 2: Write the failing test**

Create `src-tauri/src/media_skip.rs` with:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceMedia {
    /// Normalized codec slug (same vocabulary as `handbrake::classify_preset`'s codec).
    pub codec: String,
    pub height: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn efficiency_rank_buckets_codecs() {
        // Modern efficient codecs share the top bucket.
        for c in ["av1", "h265", "vp9"] {
            assert_eq!(efficiency_rank(c), Some(3), "codec {c}");
        }
        assert_eq!(efficiency_rank("h264"), Some(2));
        // Older lossy codecs rank below h264 so they always re-encode.
        for c in ["mpeg2", "mpeg4", "vc1"] {
            assert_eq!(efficiency_rank(c), Some(1), "codec {c}");
        }
        // Intermediate/lossless rank lowest — they must always re-encode to a delivery codec.
        for c in ["prores", "dnxhr", "ffv1"] {
            assert_eq!(efficiency_rank(c), Some(0), "codec {c}");
        }
        // Unknown is the uncertainty sentinel — None, never a number.
        assert_eq!(efficiency_rank("unknown"), None);
        assert_eq!(efficiency_rank("totally-made-up"), None);
    }
}
```

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test --manifest-path src-tauri/Cargo.toml efficiency_rank_buckets_codecs`
Expected: FAIL — `cannot find function efficiency_rank in this scope`.

- [ ] **Step 4: Implement `efficiency_rank`**

Add to `src-tauri/src/media_skip.rs` (above the test module):

```rust
/// Compression-efficiency rank of a codec slug. Higher = more efficient (smaller files at
/// equal quality). `None` means "unrecognized" — the caller treats that as uncertainty and
/// never skips. Intentionally coarse buckets: av1/h265/vp9 are treated as equally efficient.
pub fn efficiency_rank(codec: &str) -> Option<u8> {
    match codec {
        "av1" | "h265" | "vp9" => Some(3),
        "h264" => Some(2),
        "mpeg2" | "mpeg4" | "vc1" => Some(1),
        "prores" | "dnxhr" | "ffv1" => Some(0),
        _ => None,
    }
}
```

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo test --manifest-path src-tauri/Cargo.toml efficiency_rank_buckets_codecs`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/media_skip.rs src-tauri/src/lib.rs
git commit -m "feat: add media_skip module with codec efficiency rank"
```

---

## Task 2: `target_height_from_resolution`

**Files:**
- Modify: `src-tauri/src/media_skip.rs`
- Test: inline

- [ ] **Step 1: Write the failing test**

Add to the `tests` module in `media_skip.rs`:

```rust
#[test]
fn parses_target_height_from_resolution_slug() {
    // classify_preset emits "{height}p" or "" (when the preset keeps source resolution).
    assert_eq!(target_height_from_resolution("1080p"), 1080);
    assert_eq!(target_height_from_resolution("2160p"), 2160);
    assert_eq!(target_height_from_resolution("720p"), 720);
    // No cap / unparseable -> 0, which the policy reads as "no resolution benefit possible".
    assert_eq!(target_height_from_resolution(""), 0);
    assert_eq!(target_height_from_resolution("p"), 0);
    assert_eq!(target_height_from_resolution("source"), 0);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --manifest-path src-tauri/Cargo.toml parses_target_height_from_resolution_slug`
Expected: FAIL — `cannot find function target_height_from_resolution`.

- [ ] **Step 3: Implement it**

Add to `media_skip.rs`:

```rust
/// Parse a `classify_preset` resolution slug ("1080p", "" ...) into a numeric height.
/// "" or anything unparseable -> 0 (no downscale benefit possible).
pub fn target_height_from_resolution(resolution: &str) -> i64 {
    resolution
        .strip_suffix('p')
        .and_then(|n| n.parse::<i64>().ok())
        .unwrap_or(0)
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --manifest-path src-tauri/Cargo.toml parses_target_height_from_resolution_slug`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/media_skip.rs
git commit -m "feat: parse target height from preset resolution slug"
```

---

## Task 3: `should_skip_by_media` (the core decision)

**Files:**
- Modify: `src-tauri/src/media_skip.rs`
- Test: inline

- [ ] **Step 1: Write the failing test**

Add to the `tests` module:

```rust
#[test]
fn skip_decision_only_when_neither_dimension_helps() {
    // (source_codec, source_h, target_codec, target_h, expect_skip, why)
    let cases = [
        // Neither helps -> skip. The waste case the feature exists to prevent.
        ("av1", 1080, "h265", 1080, true, "av1 1080p -> h265 1080p is pure waste"),
        // Codec helps (h264 -> h265 at same res is a real size win) -> convert.
        ("h264", 1080, "h265", 1080, false, "h264 -> h265 saves space"),
        // Intermediate source always re-encodes (ProRes ranks lowest).
        ("prores", 1080, "h265", 1080, false, "ProRes must re-encode to a delivery codec"),
        // Resolution helps (downscale) -> convert even if codec matches.
        ("h265", 2160, "h265", 1080, false, "4K -> 1080p downscale shrinks the file"),
        // No upscale benefit + same codec -> skip.
        ("h265", 720, "h265", 1080, true, "never upscale 720p to a 1080p target"),
        // Downgrade saves nothing -> skip (compatibility users turn the toggle off).
        ("h265", 1080, "h264", 1080, true, "h265 -> h264 at same res saves nothing"),
        // Uncertainty: unknown on EITHER side -> never skip.
        ("unknown", 1080, "h265", 1080, false, "unknown source codec is never skipped"),
        ("h264", 1080, "unknown", 1080, false, "unknown target codec is never skipped"),
        // Target with no resolution cap (height 0): resolution can't help; decide on codec.
        ("h265", 1080, "h265", 0, true, "no cap + same codec -> skip"),
        ("h264", 1080, "h265", 0, false, "no cap but codec upgrade -> convert"),
    ];
    for (sc, sh, tc, th, expect, why) in cases {
        assert_eq!(should_skip_by_media(sc, sh, tc, th), expect, "{why}");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --manifest-path src-tauri/Cargo.toml skip_decision_only_when_neither_dimension_helps`
Expected: FAIL — `cannot find function should_skip_by_media`.

- [ ] **Step 3: Implement it**

Add to `media_skip.rs`:

```rust
/// Decide whether a source already meets/exceeds the target so re-encoding would not help.
/// Skip only when NEITHER downscaling NOR a more-efficient codec would shrink the file.
/// Unknown codec on either side forces "could help" so the file is queued, never skipped.
pub fn should_skip_by_media(
    source_codec: &str,
    source_height: i64,
    target_codec: &str,
    target_height: i64,
) -> bool {
    let resolution_would_help = target_height > 0 && source_height > target_height;
    let codec_would_help = match (efficiency_rank(source_codec), efficiency_rank(target_codec)) {
        (Some(source_rank), Some(target_rank)) => target_rank > source_rank,
        _ => true, // unknown on either side -> assume a conversion could help
    };
    !resolution_would_help && !codec_would_help
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --manifest-path src-tauri/Cargo.toml skip_decision_only_when_neither_dimension_helps`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/media_skip.rs
git commit -m "feat: add should_skip_by_media skip decision"
```

---

## Task 4: `select_media_skips` (pure path selection)

**Files:**
- Modify: `src-tauri/src/media_skip.rs`
- Test: inline

This isolates the "which paths to skip" selection from the I/O of probing, so it is fully unit-testable with fake probe results.

- [ ] **Step 1: Write the failing test**

Add to the `tests` module:

```rust
#[test]
fn selects_only_at_target_paths_with_known_media() {
    let candidates = vec![
        // Already at target -> selected for skip.
        ("/m/av1.mp4".to_string(), Some(SourceMedia { codec: "av1".into(), height: 1080 })),
        // Codec upgrade available -> not skipped.
        ("/m/h264.mp4".to_string(), Some(SourceMedia { codec: "h264".into(), height: 1080 })),
        // Probe failed / not introspectable -> never skipped (uncertainty).
        ("/m/unknown.mp4".to_string(), None),
    ];
    let skip = select_media_skips(&candidates, "h265", 1080);
    assert!(skip.contains("/m/av1.mp4"));
    assert!(!skip.contains("/m/h264.mp4"));
    assert!(!skip.contains("/m/unknown.mp4"), "None media must never be skipped");
    assert_eq!(skip.len(), 1);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --manifest-path src-tauri/Cargo.toml selects_only_at_target_paths_with_known_media`
Expected: FAIL — `cannot find function select_media_skips`.

- [ ] **Step 3: Implement it**

Add to the top of `media_skip.rs`:

```rust
use std::collections::HashSet;
```

Add the function:

```rust
/// From candidate `(path, probed media)` pairs, return the set of paths to skip because they
/// already meet the target. `None` media (probe failed / not a video) is never skipped.
pub fn select_media_skips(
    candidates: &[(String, Option<SourceMedia>)],
    target_codec: &str,
    target_height: i64,
) -> HashSet<String> {
    candidates
        .iter()
        .filter_map(|(path, media)| {
            let m = media.as_ref()?;
            if should_skip_by_media(&m.codec, m.height, target_codec, target_height) {
                Some(path.clone())
            } else {
                None
            }
        })
        .collect()
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --manifest-path src-tauri/Cargo.toml selects_only_at_target_paths_with_known_media`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/media_skip.rs
git commit -m "feat: add select_media_skips path selection"
```

---

## Task 5: `normalize_source_codec` (HandBrake decoder string -> slug)

**Files:**
- Modify: `src-tauri/src/handbrake.rs`
- Test: inline `#[cfg(test)]` in `handbrake.rs`

**Context (verified against HandBrake 1.11.2):** `--scan --json` reports the source's *decoder* name in `VideoCodec`, not always the codec name — e.g. AV1 shows as `libdav1d`, h265 as `hevc`. Matching must be substring-based and emit the same slugs as `classify_preset` (`h265`/`h264`/`av1`/`vp9`/`prores`/`dnxhr`/`ffv1`), plus `mpeg2`/`mpeg4`/`vc1` for older sources.

- [ ] **Step 1: Write the failing test**

Add to the `tests` module in `handbrake.rs`:

```rust
#[test]
fn normalize_source_codec_maps_handbrake_decoder_names() {
    // Real VideoCodec strings observed from HandBrakeCLI --scan --json.
    let cases = [
        ("h264", "h264"),
        ("hevc", "h265"),       // HandBrake reports HEVC as "hevc"
        ("libdav1d", "av1"),    // AV1 reports the dav1d DECODER name, not "av1"
        ("av1", "av1"),
        ("vp9", "vp9"),
        ("prores", "prores"),
        ("dnxhd", "dnxhr"),
        ("ffv1", "ffv1"),
        ("mpeg2video", "mpeg2"),
        ("mpeg4", "mpeg4"),
        ("vc1", "vc1"),
        ("", "unknown"),
        ("some-future-codec", "unknown"),
    ];
    for (input, want) in cases {
        assert_eq!(normalize_source_codec(input), want, "input {input}");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --manifest-path src-tauri/Cargo.toml normalize_source_codec_maps_handbrake_decoder_names`
Expected: FAIL — `cannot find function normalize_source_codec`.

- [ ] **Step 3: Implement it**

Add to `handbrake.rs`:

```rust
/// Map a HandBrake `--scan` `VideoCodec` string (often the libav *decoder* name) to the same
/// codec-slug vocabulary `classify_preset` emits. Substring-based because HandBrake reports
/// e.g. "libdav1d" for AV1 and "hevc" for h265. Unrecognized -> "unknown" (never skipped).
pub fn normalize_source_codec(handbrake_codec: &str) -> String {
    let c = handbrake_codec.to_lowercase();
    if c.contains("hevc") || c.contains("h265") || c.contains("x265") {
        "h265"
    } else if c.contains("av1") || c.contains("dav1d") || c.contains("aom") {
        "av1"
    } else if c.contains("h264") || c.contains("avc") || c.contains("x264") {
        "h264"
    } else if c.contains("vp9") {
        "vp9"
    } else if c.contains("prores") {
        "prores"
    } else if c.contains("dnxh") {
        "dnxhr"
    } else if c.contains("ffv1") {
        "ffv1"
    } else if c.contains("mpeg2") || c.contains("mpeg-2") {
        "mpeg2"
    } else if c.contains("mpeg4") || c.contains("mpeg-4") || c.contains("divx") || c.contains("xvid") {
        "mpeg4"
    } else if c.contains("vc1") || c.contains("vc-1") || c.contains("wmv") {
        "vc1"
    } else {
        "unknown"
    }
    .to_string()
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --manifest-path src-tauri/Cargo.toml normalize_source_codec_maps_handbrake_decoder_names`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/handbrake.rs
git commit -m "feat: normalize HandBrake scan codec names to slugs"
```

---

## Task 6: `parse_scan_media` (scan JSON -> SourceMedia)

**Files:**
- Modify: `src-tauri/src/handbrake.rs`
- Test: inline

**Context (verified):** `--scan --json` writes a `Version: {...}` block then a `JSON Title Set: {...}` block to **stdout**. Height is `TitleList[0].Geometry.Height`; codec is `TitleList[0].VideoCodec`. The title set is the last block, so everything after the marker parses as one JSON object.

- [ ] **Step 1: Write the failing test**

Add to the `tests` module in `handbrake.rs`:

```rust
const SCAN_FIXTURE_HEVC: &str = r#"Version: {
    "Name": "HandBrake",
    "VersionString": "1.11.2"
}
JSON Title Set: {
    "MainFeature": 0,
    "TitleList": [
        {
            "Geometry": { "Height": 720, "PAR": { "Den": 1, "Num": 1 }, "Width": 1280 },
            "VideoCodec": "hevc"
        }
    ]
}
"#;

const SCAN_FIXTURE_EMPTY: &str = r#"Version: { "Name": "HandBrake" }
JSON Title Set: {
    "MainFeature": 0,
    "TitleList": []
}
"#;

#[test]
fn parse_scan_media_reads_height_and_normalized_codec() {
    let media = parse_scan_media(SCAN_FIXTURE_HEVC).expect("title set present");
    assert_eq!(media.codec, "h265", "hevc normalizes to h265");
    assert_eq!(media.height, 720);
}

#[test]
fn parse_scan_media_returns_none_when_no_title() {
    // Empty TitleList (e.g. a non-video / unreadable file) -> None -> caller never skips.
    assert!(parse_scan_media(SCAN_FIXTURE_EMPTY).is_none());
    // No marker at all -> None.
    assert!(parse_scan_media("garbage output, no json").is_none());
}
```

You will also need `SourceMedia` in scope. Add near the top of `handbrake.rs`:

```rust
use crate::media_skip::SourceMedia;
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --manifest-path src-tauri/Cargo.toml parse_scan_media`
Expected: FAIL — `cannot find function parse_scan_media`.

- [ ] **Step 3: Implement it**

Add to `handbrake.rs`:

```rust
/// Parse HandBrakeCLI `--scan --json` stdout into the source's normalized codec + height.
/// Returns `None` when the title set is missing/empty or unparseable; the caller treats
/// `None` as uncertainty and does not skip the file.
pub fn parse_scan_media(stdout: &str) -> Option<SourceMedia> {
    const MARKER: &str = "JSON Title Set: ";
    let start = stdout.find(MARKER)? + MARKER.len();
    let json: serde_json::Value = serde_json::from_str(stdout[start..].trim()).ok()?;
    let title = json["TitleList"].get(0)?;
    let height = title["Geometry"]["Height"].as_i64().unwrap_or(0);
    let raw_codec = title["VideoCodec"].as_str().unwrap_or("");
    Some(SourceMedia {
        codec: normalize_source_codec(raw_codec),
        height,
    })
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --manifest-path src-tauri/Cargo.toml parse_scan_media`
Expected: PASS (both tests).

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/handbrake.rs
git commit -m "feat: parse source codec + height from HandBrake scan JSON"
```

---

## Task 7: `probe_source` (shell-out wrapper)

**Files:**
- Modify: `src-tauri/src/handbrake.rs`
- Test: an `#[ignore]`d integration test (requires local HandBrakeCLI + ffmpeg; CI skips it)

This is a thin I/O adapter over `parse_scan_media` (already unit-tested), so it has no branching logic to unit-test. The `#[ignore]`d test gives a real end-to-end check on a developer machine.

- [ ] **Step 1: Implement `probe_source`**

Add to `handbrake.rs`:

```rust
/// Run `HandBrakeCLI --scan --json` on a file and return its normalized codec + height.
/// Returns `None` if the process fails to launch or no parseable title is found — the caller
/// treats `None` as uncertainty and queues the file rather than skipping it.
pub fn probe_source(handbrake_path: &str, file: &str) -> Option<SourceMedia> {
    let output = Command::new(handbrake_path)
        .args(["--scan", "--json", "-i", file])
        .output()
        .ok()?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    parse_scan_media(&stdout)
}
```

- [ ] **Step 2: Add an ignored end-to-end test**

Add to the `tests` module in `handbrake.rs`:

```rust
// Local-only: needs ffmpeg to synthesize a clip and HandBrakeCLI to scan it.
// Run with: cargo test --manifest-path src-tauri/Cargo.toml -- --ignored probe_source_reads_real_clip
#[test]
#[ignore]
fn probe_source_reads_real_clip() {
    let hb = detect_handbrake_path().expect("HandBrakeCLI on PATH");
    let dir = tempfile::tempdir().unwrap();
    let clip = dir.path().join("probe.mp4");
    let status = std::process::Command::new("ffmpeg")
        .args(["-y", "-f", "lavfi", "-i", "testsrc=duration=0.3:size=320x240:rate=12",
               "-pix_fmt", "yuv420p", "-c:v", "libx264"])
        .arg(&clip)
        .status()
        .expect("run ffmpeg");
    assert!(status.success());
    let media = probe_source(&hb, clip.to_str().unwrap()).expect("probe returns media");
    assert_eq!(media.codec, "h264");
    assert_eq!(media.height, 240);
}
```

- [ ] **Step 3: Verify it compiles and the suite is green**

Run: `cargo test --manifest-path src-tauri/Cargo.toml`
Expected: PASS; the ignored test is listed as `ignored`, not run.

- [ ] **Step 4 (optional, local): Run the ignored test**

Run: `cargo test --manifest-path src-tauri/Cargo.toml -- --ignored probe_source_reads_real_clip`
Expected: PASS on a machine with ffmpeg + HandBrakeCLI.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/handbrake.rs
git commit -m "feat: add probe_source HandBrake scan wrapper"
```

---

## Task 8: Add the `skip_by_source_media` setting (default ON), backend + type

**Files:**
- Modify: `src-tauri/src/types.rs`, `src-tauri/src/db.rs`, `src-tauri/src/commands/settings.rs`, `src/lib/tauri.ts`, `src/hooks/useSettings.test.ts`
- Test: `db.rs` default assertion

This adds the persisted setting end-to-end (default `"true"`) but does **not** yet wire it to behavior or add a UI checkbox — those are Tasks 9–10, so we never ship an inert checkbox.

- [ ] **Step 1: Add the field to the `Settings` struct**

In `src-tauri/src/types.rs`, add after `pub skip_already_converted: bool,`:

```rust
    pub skip_by_source_media: bool,
```

- [ ] **Step 2: Write the failing default test**

In `src-tauri/src/db.rs`, locate the existing `skip_already_converted` default assertion (around line 150) and add directly after it:

```rust
        assert_eq!(
            setting(&conn, "skip_by_source_media").as_deref(),
            Some("true"),
            "skip-by-source-media defaults ON"
        );
```

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --lib db::`
Expected: FAIL — the setting is `None` (not yet seeded).

- [ ] **Step 4: Seed the default**

In `src-tauri/src/db.rs`, add to the `defaults` array after `("skip_already_converted", "false"),`:

```rust
        ("skip_by_source_media", "true"),
```

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --lib db::`
Expected: PASS.

- [ ] **Step 6: Read & return the setting in `get_settings`**

In `src-tauri/src/commands/settings.rs`:

1. Add the initializer after `let mut skip_already_converted = false;`:

```rust
    let mut skip_by_source_media = true;
```

2. Add the match arm after the `skip_already_converted` arm:

```rust
            "skip_by_source_media" => skip_by_source_media = value == "true",
```

3. Add the field to the returned `Settings { ... }` after `skip_already_converted,`:

```rust
        skip_by_source_media,
```

4. Add the key to `ALLOWED_KEYS` after `"skip_already_converted",`:

```rust
    "skip_by_source_media",
```

- [ ] **Step 7: Verify the backend compiles and is green**

Run: `cargo test --manifest-path src-tauri/Cargo.toml`
Expected: PASS (the struct now has the new field everywhere it is constructed).

- [ ] **Step 8: Add the type on the frontend**

In `src/lib/tauri.ts`, add to the `AppSettings` interface after `skip_already_converted: boolean;`:

```ts
  skip_by_source_media: boolean;
```

In `src/hooks/useSettings.test.ts`, add to the defaults helper object after `skip_already_converted: false,`:

```ts
    skip_by_source_media: true,
```

- [ ] **Step 9: Verify the frontend type-checks and tests pass**

Run: `npm run build`
Expected: PASS (no TS error about a missing `skip_by_source_media`).

Run: `npm test`
Expected: PASS.

- [ ] **Step 10: Commit**

```bash
git add src-tauri/src/types.rs src-tauri/src/db.rs src-tauri/src/commands/settings.rs src/lib/tauri.ts src/hooks/useSettings.test.ts
git commit -m "feat: add skip_by_source_media setting (default on)"
```

---

## Task 9: Wire probe + media-skip into `add_files_inner` — BLOCKED on in-place Part B

**Blocked until:** `AddResult`, `SkipCount`, `SkipReason`, and `add_files_to_db(...) -> AddResult` exist (in-place-reencode plan, Part B). If implementing this feature first, complete that scaffolding here before proceeding.

**Files:**
- Modify: wherever `SkipReason` is defined (per the in-place plan: `src-tauri/src/types.rs` or `src-tauri/src/commands/queue.rs`)
- Modify: `src-tauri/src/commands/queue.rs` (`add_files_inner`)
- Test: inline in `queue.rs`

- [ ] **Step 1: Add the `AlreadyAtTarget` reason variant**

In `src-tauri/src/types.rs`, add the variant to the `SkipReason` enum (defined by the in-place plan's Task 1, commit `c7b490b`). The enum has `#[serde(rename_all = "snake_case")]`, so this serializes to `"already_at_target"` for the frontend (Task 10):

```rust
    AlreadyAtTarget,
```

- [ ] **Step 2: Read the new setting + decide whether HandBrake is needed**

In `add_files_inner` (`queue.rs`), extend the DB-read block. Change the tuple binding and add the read for `skip_by_source_media`, and make `hb_path` also required when the new toggle is on:

```rust
    let (preset, suffix_template, hb_path, skip_already_converted, skip_by_source_media) = {
        let conn = state.db.lock().map_err(|e| e.to_string())?;

        let preset: String = conn
            .query_row("SELECT value FROM settings WHERE key = 'preset'", [], |row| row.get(0))
            .map_err(|e| e.to_string())?;

        let suffix_template: String = conn
            .query_row(
                "SELECT suffix FROM preset_suffixes WHERE preset_name = ?1",
                params![preset],
                |row| row.get(0),
            )
            .unwrap_or_default();

        let skip_already_converted: bool = conn
            .query_row(
                "SELECT value FROM settings WHERE key = 'skip_already_converted'",
                [],
                |row| row.get::<_, String>(0),
            )
            .map(|v| v == "true")
            .unwrap_or(false);

        let skip_by_source_media: bool = conn
            .query_row(
                "SELECT value FROM settings WHERE key = 'skip_by_source_media'",
                [],
                |row| row.get::<_, String>(0),
            )
            .map(|v| v == "true")
            .unwrap_or(true); // default ON

        let hb_path = if suffix_template.contains('{') || skip_by_source_media {
            get_handbrake_path(&conn).ok()
        } else {
            None
        };

        (preset, suffix_template, hb_path, skip_already_converted, skip_by_source_media)
    };
```

- [ ] **Step 3: Probe candidates and compute the media-skip set (outside the DB lock)**

After the suffix is resolved (the existing `let suffix = ...` block) and **before** re-acquiring the DB lock, add:

```rust
    // Source-media skip: probe each video file and drop those already at/below target.
    // Runs outside the DB lock; on any uncertainty (no HandBrake, probe failure, unknown
    // codec) we keep the file rather than skipping it.
    let media_skipped: std::collections::HashSet<String> = if skip_by_source_media {
        if let Some(hb) = hb_path.as_deref() {
            let metadata = {
                let mut cache = state.preset_cache.lock().map_err(|e| e.to_string())?;
                if let Some(m) = cache.get(&preset) {
                    m.clone()
                } else {
                    let m = handbrake::get_preset_metadata(hb, &preset)?;
                    cache.insert(preset.clone(), m.clone());
                    m
                }
            };
            let target_codec = metadata.codec.clone();
            let target_height = crate::media_skip::target_height_from_resolution(&metadata.resolution);

            let candidates: Vec<(String, Option<crate::media_skip::SourceMedia>)> = paths
                .iter()
                .filter(|p| is_video_file(Path::new(p))) // skip the shell-out for non-videos
                .map(|p| (p.clone(), handbrake::probe_source(hb, p)))
                .collect();

            crate::media_skip::select_media_skips(&candidates, &target_codec, target_height)
        } else {
            std::collections::HashSet::new()
        }
    } else {
        std::collections::HashSet::new()
    };

    let survivors: Vec<String> = paths
        .iter()
        .filter(|p| !media_skipped.contains(*p))
        .cloned()
        .collect();
```

- [ ] **Step 4: Pass survivors to the DB core and merge the skip count**

Replace the final `add_files_to_db(...)` call so it inserts only survivors and records the media skips:

```rust
    let conn = state.db.lock().map_err(|e| e.to_string())?;
    let mut result = add_files_to_db(&conn, &survivors, &preset, &suffix, skip_already_converted)?;
    if !media_skipped.is_empty() {
        result.skipped.push(SkipCount {
            reason: SkipReason::AlreadyAtTarget,
            count: media_skipped.len() as u32,
        });
    }
    Ok(result)
```

Ensure `SkipCount` and `SkipReason` are imported at the top of `queue.rs` (alongside the in-place Part B imports).

- [ ] **Step 5: Write a test for the merge (no HandBrake needed)**

The decision and selection are already covered (Tasks 3–4). Add a focused `queue.rs` test that the media-skip count is appended to `AddResult.skipped` given a precomputed skip set. If `add_files_inner`'s probing loop is not directly testable without HandBrake, assert the merge via a small helper or via `select_media_skips` + `add_files_to_db` composition:

```rust
#[test]
fn already_at_target_skips_are_reported_in_add_result() {
    let conn = test_conn();
    // No rows added (survivors empty), but a media skip should still surface in the result.
    let mut result = add_files_to_db(&conn, &[], "preset", "", false).unwrap();
    let media_skipped = ["/m/a.mp4".to_string()];
    if !media_skipped.is_empty() {
        result.skipped.push(SkipCount {
            reason: SkipReason::AlreadyAtTarget,
            count: media_skipped.len() as u32,
        });
    }
    assert!(result.added.is_empty());
    assert!(result
        .skipped
        .iter()
        .any(|s| s.reason == SkipReason::AlreadyAtTarget && s.count == 1));
}
```

> Note: this test mirrors the production merge step rather than driving the probe loop (which needs HandBrake). The real introspection path is exercised by the Task 7 `#[ignore]`d test plus manual verification (Task 11).

- [ ] **Step 6: Run the backend suite**

Run: `cargo test --manifest-path src-tauri/Cargo.toml`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add src-tauri/src
git commit -m "feat: skip queued files already at/below target codec + resolution"
```

---

## Task 10: User-facing UI — checkbox + reason label

**Files:**
- Modify: `src/pages/SettingsPage.tsx` (checkbox — independent of Part B)
- Modify: `src/components/DropZone.tsx` (reason label — BLOCKED on Part B's per-reason rendering)

- [ ] **Step 1: Add the settings checkbox**

In `src/pages/SettingsPage.tsx`, add a `setting-group` mirroring the `skip_already_converted` block (after it):

```tsx
      <div className="setting-group">
        <label className="setting-label">
          <input
            type="checkbox"
            checked={settings.skip_by_source_media}
            onChange={(e) =>
              updateSetting("skip_by_source_media", String(e.target.checked))
            }
          />
          Skip files already at or below the target
        </label>
        <p className="setting-hint">
          When adding files, skip any whose codec and resolution already meet the target
          preset, so they are not needlessly re-encoded. Turn this off to force a
          conversion (e.g. for device compatibility).
        </p>
      </div>
```

- [ ] **Step 2: Add the reason label (BLOCKED on Part B)**

In `src/components/DropZone.tsx`, where the in-place plan renders `SkipReason` values into the per-reason summary, add the label for the new reason so it reads e.g. `… · 3 skipped (already at target)`. The reason arrives as the serde snake_case string `"already_at_target"`:

```ts
// in the reason -> label map introduced by the in-place plan:
already_at_target: "already at target",
```

- [ ] **Step 3: Verify the frontend builds and tests pass**

Run: `npm run build`
Expected: PASS.

Run: `npm test`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add src/pages/SettingsPage.tsx src/components/DropZone.tsx
git commit -m "feat: add skip-by-source-media toggle and skip-reason label"
```

---

## Task 11: Full verification + cross-platform review

- [ ] **Step 1: Run the complete suites**

Run: `cargo test --manifest-path src-tauri/Cargo.toml`
Expected: PASS (all unit tests; the `#[ignore]`d probe test is skipped).

Run: `npm run build && npm test`
Expected: PASS.

- [ ] **Step 2: Manual end-to-end check (local, with HandBrakeCLI)**

1. Set the active preset to an h265 1080p preset; ensure the toggle is on (default).
2. Add an h265 1080p (or 720p) source → it is **not** queued; `DropZone` shows `… skipped (already at target)`.
3. Add an h264 1080p source → it **is** queued.
4. Add a 4K h265 source → it **is** queued (downscale benefit).
5. Turn the toggle off, re-add the h265 1080p source → it **is** queued.
6. Add a file while HandBrakeCLI is unavailable (rename it off PATH) → nothing is skipped by media (uncertainty default).

- [ ] **Step 3: Cross-platform review**

Dispatch the `cross-platform-reviewer` agent over the backend changes. `probe_source` uses `Command` + `HandBrakeCLI` (PATH-resolved by the existing `detect_handbrake_path`, which already branches `which`/`where`), so there is no new platform-specific code — confirm this holds.

---

## Spec Coverage Map

| Spec requirement | Task(s) |
|---|---|
| Pure rank + resolution skip rule | 1, 2, 3 |
| Efficiency rank (av1/h265/vp9 > h264 > mpeg > prores/dnxhr/ffv1) | 1 |
| Never skip on uncertainty (unknown codec / failed probe / no HandBrake) | 3, 4, 9 |
| `HandBrakeCLI --scan --json` probe + parse into shared slug vocabulary | 5, 6, 7 |
| Sequential probe outside the DB lock, beside suffix resolution | 9 |
| Setting `skip_by_source_media`, default ON | 8 |
| Settings checkbox mirroring `skip_already_converted` | 10 |
| Ephemeral feedback via shared `AddResult`/`SkipReason` (+1 variant), never persisted | 9, 10 |
| Table-tested decision; parsing tested against a captured fixture | 3, 6 |
| Dependency on in-place-reencode Part B | Preconditions, 9, 10 |
| Parallel probing deferred (follow-up) | (out of scope — noted in spec) |
</content>
