# In-place re-encode + skip-reason feedback — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Re-encode an `.mp4` onto itself safely (temp file + atomic rename) when the resolved output path equals the source path, and replace silent add-time skips with per-reason counts surfaced in the UI.

**Architecture:** "In-place" is *derived* from `source_path == output_path` (no schema change). At add time, that case is queued instead of skipped; at encode time the converter writes to a same-directory marked temp file, reuses `decide_cleanup`'s decision, and either atomically renames the temp over the source (respecting `cleanup_mode`) or deletes the temp and keeps the original. The add core returns structured skip counts instead of dropping files silently.

**Tech Stack:** Rust (Tauri 2 backend, rusqlite, `trash` crate), React + TypeScript (Vite), Vitest, `cargo test`.

**Spec:** `docs/superpowers/specs/2026-06-21-in-place-reencode-design.md`

---

## Working context

- **All work happens in the worktree** `/Users/rhurling/Sites/convertbar/.worktrees/in-place-reencode` on branch `feature/in-place-reencode-spec`.
- **First run only:** `npm install` (frontend deps) and `cargo build --manifest-path src-tauri/Cargo.toml` (Rust deps) — node_modules and target/ are per-worktree.
- **Rust format hook:** the Rust sources are not `cargo fmt`-clean, and the Edit/Write format hook reformats whole files, producing noisy diffs. Prefer surgical `sed`/Edit, run `git diff` before each commit, and keep each commit to the intended change only.
- **Test commands:**
  - Rust (single test): `cargo test --manifest-path src-tauri/Cargo.toml <test_name> -- --nocapture`
  - Rust (all): `cargo test --manifest-path src-tauri/Cargo.toml`
  - Frontend (single file): `npx vitest run <path>`
  - Frontend (all): `npm test`
  - Type check: `npx tsc --noEmit`

## File structure

**Backend (`src-tauri/src`)**
- `types.rs` — add `SkipReason`, `SkipCount`, `AddResult`.
- `commands/queue.rs` — `add_files_to_db` returns `AddResult` (in-place detection + per-reason tallies); `is_video_file` excludes the temp marker; `add_files_inner`, `add_files`, `confirm_folder_add` thread `AddResult`.
- `converter.rs` — in-place helpers (`is_in_place`, `in_place_temp_path`, `IN_PLACE_TEMP_MARKER`, `InPlaceAction`, `in_place_action`, `apply_in_place_action`); `process_queue` encodes in-place jobs to the temp and acts on the decision; failure cleanup targets the temp.
- `commands/converter.rs` — `cancel_conversion` removes the temp (not the source) for in-place jobs.
- `watcher.rs` — `enqueue_and_start` uses `result.added`.

**Frontend (`src`)**
- `lib/tauri.ts` — `SkipReason`/`SkipCount`/`AddResult` types; `addFiles`/`confirmFolderAdd` return `AddResult`.
- `lib/addSummary.ts` (new) + `lib/addSummary.test.ts` (new) — pure `summarizeAdds`.
- `components/DropZone.tsx` — consume `AddResult`, show the summary in the status line.
- `components/DropZone.test.tsx` — update mocks to `AddResult`; assert the summary renders.
- `components/QueueItem.tsx` + `components/QueueItem.test.tsx` (new) — "In place" badge.
- `pages/SettingsPage.tsx` + `App.css` — empty-suffix info note.

---

## Task 1: Backend `AddResult` / `SkipReason` / `SkipCount` types

**Files:**
- Modify: `src-tauri/src/types.rs`

- [ ] **Step 1: Add the types**

Append to `src-tauri/src/types.rs`:

```rust
/// Why a dropped/scanned path was not queued. Surfaced at add time and counted per reason;
/// never written to history.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum SkipReason {
    NotVideo,
    AlreadyQueued,
    AlreadyConverted,
    OutputExists,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SkipCount {
    pub reason: SkipReason,
    pub count: u32,
}

/// Result of an add operation: the jobs actually queued, plus per-reason counts of paths skipped.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AddResult {
    pub added: Vec<JobInfo>,
    pub skipped: Vec<SkipCount>,
}
```

- [ ] **Step 2: Verify it compiles**

Run: `cargo build --manifest-path src-tauri/Cargo.toml`
Expected: builds (warnings about unused types are fine until later tasks).

- [ ] **Step 3: Commit**

```bash
git add src-tauri/src/types.rs
git commit -m "feat: add AddResult/SkipReason/SkipCount types"
```

---

## Task 2: `add_files_to_db` — in-place detection + per-reason skip counts

**Files:**
- Modify: `src-tauri/src/commands/queue.rs` (`is_video_file`, imports, `add_files_to_db`, `add_files_inner`, `add_files`, `confirm_folder_add`, tests)
- Modify: `src-tauri/src/watcher.rs:210-221` (`enqueue_and_start`)

- [ ] **Step 1: Update the failing tests first (TDD)**

Replace the four existing `add_files_to_db` skip-rule tests in `src-tauri/src/commands/queue.rs` and add the new in-place test. The existing tests currently assert on a returned `Vec<JobInfo>`; they must assert on `result.added` and the new `result.skipped`.

```rust
#[test]
fn add_files_skips_paths_already_in_queue() {
    let conn = test_conn();
    insert_queued(&conn, "j1", "/movies/a.mp4", "queued", 1);

    let result =
        add_files_to_db(&conn, &["/movies/a.mp4".to_string()], "preset", "", false).unwrap();

    assert!(result.added.is_empty(), "an already-queued source must be skipped");
    assert_eq!(
        result.skipped,
        vec![SkipCount { reason: SkipReason::AlreadyQueued, count: 1 }],
        "the skip is reported as AlreadyQueued"
    );
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM jobs", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1, "no duplicate row inserted");
}

#[test]
fn add_files_skips_when_output_already_exists() {
    let conn = test_conn();
    let dir = tempfile::tempdir().unwrap();
    // With suffix "-conv", source clip.mov -> output clip-conv.mp4. Pre-create the output.
    std::fs::write(dir.path().join("clip-conv.mp4"), b"x").unwrap();
    let source = dir.path().join("clip.mov").to_string_lossy().to_string();

    let result = add_files_to_db(&conn, &[source], "preset", "-conv", false).unwrap();

    assert!(result.added.is_empty(), "must skip when the converted output already exists");
    assert_eq!(
        result.skipped,
        vec![SkipCount { reason: SkipReason::OutputExists, count: 1 }]
    );
}

#[test]
fn add_files_skips_source_that_already_has_suffix() {
    let conn = test_conn();
    let result = add_files_to_db(
        &conn,
        &["/movies/clip-conv.mov".to_string()],
        "preset",
        "-conv",
        false,
    )
    .unwrap();

    assert!(result.added.is_empty(), "must skip a source whose stem already carries the suffix");
    assert_eq!(
        result.skipped,
        vec![SkipCount { reason: SkipReason::AlreadyConverted, count: 1 }]
    );
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM jobs", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 0);
}

#[test]
fn add_files_skip_already_converted_union_respects_flag() {
    let conn = test_conn();
    insert_history(&conn, "h1", "/movies/done.mkv", "done", 100, 1000, "2020-01-01T00:00:00Z");

    // Flag on: a previously-converted (done) source is skipped and reported as AlreadyConverted.
    let with_flag =
        add_files_to_db(&conn, &["/movies/done.mkv".to_string()], "preset", "", true).unwrap();
    assert!(with_flag.added.is_empty());
    assert_eq!(
        with_flag.skipped,
        vec![SkipCount { reason: SkipReason::AlreadyConverted, count: 1 }]
    );

    // Flag off: history is ignored, the file is queued again.
    let without_flag =
        add_files_to_db(&conn, &["/movies/done.mkv".to_string()], "preset", "", false).unwrap();
    assert_eq!(without_flag.added.len(), 1, "without the flag, a done source is re-added");
}

#[test]
fn add_files_reencodes_mp4_in_place_instead_of_skipping() {
    let conn = test_conn();
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("clip.mp4");
    std::fs::write(&src, b"x").unwrap();
    let src_str = src.to_string_lossy().to_string();

    // Empty suffix on an mp4 -> output path == source path. Must QUEUE an in-place job, not skip.
    let result = add_files_to_db(&conn, &[src_str.clone()], "preset", "", false).unwrap();

    assert_eq!(result.added.len(), 1, "mp4 + empty suffix must queue an in-place job");
    assert_eq!(
        result.added[0].output_path, src_str,
        "an in-place job stores output_path == source_path"
    );
    assert!(result.skipped.is_empty(), "an in-place job is not a skip");
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --manifest-path src-tauri/Cargo.toml add_files`
Expected: FAIL to compile (`AddResult`/`result.added` unknown; `add_files_to_db` still returns `Vec`).

- [ ] **Step 3: Define the temp marker constant (used by both modules)**

`is_video_file` (this task) and `in_place_temp_path` (Task 3) both reference the marker, so define it once in `converter.rs` now. Add near the top of `src-tauri/src/converter.rs` (after the imports, above `ConverterState`):

```rust
/// Filename marker for an in-flight in-place encode. A recognizable, non-suffix token so
/// `is_video_file` can exclude it — a folder scan or watched folder must never enqueue a temp.
pub(crate) const IN_PLACE_TEMP_MARKER: &str = ".convertbar-tmp.";
```

- [ ] **Step 4: Update imports and `is_video_file`**

In `src-tauri/src/commands/queue.rs`, extend the types import and exclude the temp marker:

```rust
use crate::handbrake;
use crate::types::{
    AddResult, ClassifiedPaths, FolderScanResult, HistoryPage, HistorySummary, JobInfo, SkipCount,
    SkipReason,
};
use crate::AppState;
use crate::converter::IN_PLACE_TEMP_MARKER;
```

Replace `is_video_file`:

```rust
pub(crate) fn is_video_file(path: &Path) -> bool {
    // An in-flight in-place temp must never be treated as a queueable video, or a folder scan
    // or watched folder could enqueue it mid-encode.
    if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
        if name.contains(IN_PLACE_TEMP_MARKER) {
            return false;
        }
    }
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| VIDEO_EXTENSIONS.contains(&ext.to_lowercase().as_str()))
        .unwrap_or(false)
}
```

- [ ] **Step 5: Rewrite `add_files_to_db`**

Replace the whole function body (signature returns `AddResult`):

```rust
fn add_files_to_db(
    conn: &rusqlite::Connection,
    paths: &[String],
    preset: &str,
    suffix: &str,
    skip_already_converted: bool,
) -> Result<AddResult, String> {
    // Active queue (always) and finished history (only when skip_already_converted) are tracked
    // separately so a history match is reported as AlreadyConverted, not AlreadyQueued.
    let queued_paths: HashSet<String> = {
        let mut stmt = conn
            .prepare("SELECT source_path FROM jobs WHERE status IN ('queued', 'encoding', 'paused')")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|e| e.to_string())?;
        rows.filter_map(|r| r.ok()).collect()
    };
    let history_paths: HashSet<String> = if skip_already_converted {
        let mut stmt = conn
            .prepare("SELECT source_path FROM jobs WHERE status IN ('done', 'skipped')")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|e| e.to_string())?;
        rows.filter_map(|r| r.ok()).collect()
    } else {
        HashSet::new()
    };

    let mut queue_order = get_next_queue_order(conn)?;
    let mut added = Vec::new();
    let (mut n_not_video, mut n_queued, mut n_converted, mut n_output_exists) = (0u32, 0, 0, 0);

    for path_str in paths {
        let path = Path::new(path_str);

        if !is_video_file(path) {
            n_not_video += 1;
            continue;
        }
        if queued_paths.contains(path_str) {
            n_queued += 1;
            continue;
        }
        if history_paths.contains(path_str) {
            n_converted += 1;
            continue;
        }

        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("output");
        let parent = path.parent().unwrap_or(Path::new("."));

        // Source already carries the suffix -> it's an already-converted output.
        if !suffix.is_empty() && stem.ends_with(suffix) {
            n_converted += 1;
            continue;
        }

        let output_path = parent.join(format!("{}{}.mp4", stem, suffix));
        // In-place when the forced-.mp4 output resolves back onto the source itself (mp4 + empty
        // suffix). That used to be silently skipped; now it queues and the converter re-encodes
        // via a temp file. A *distinct* pre-existing output is still a real skip.
        let in_place = output_path.as_path() == path;
        if !in_place && output_path.exists() {
            n_output_exists += 1;
            continue;
        }

        let id = uuid::Uuid::new_v4().to_string();
        let now = chrono::Utc::now().to_rfc3339();
        let original_size = std::fs::metadata(path).map(|m| m.len() as i64).ok();

        conn.execute(
            "INSERT INTO jobs (id, source_path, output_path, preset, status, original_size, queue_order, created_at)
             VALUES (?1, ?2, ?3, ?4, 'queued', ?5, ?6, ?7)",
            params![
                id,
                path_str,
                output_path.to_string_lossy().to_string(),
                preset,
                original_size,
                queue_order,
                now,
            ],
        )
        .map_err(|e| e.to_string())?;

        added.push(JobInfo {
            id,
            source_path: path_str.clone(),
            output_path: output_path.to_string_lossy().to_string(),
            preset: preset.to_string(),
            status: "queued".to_string(),
            original_size,
            converted_size: None,
            kept_file: None,
            space_saved: None,
            error_message: None,
            queue_order,
            created_at: now,
            completed_at: None,
        });

        queue_order += 1;
    }

    let mut skipped = Vec::new();
    for (reason, count) in [
        (SkipReason::NotVideo, n_not_video),
        (SkipReason::AlreadyQueued, n_queued),
        (SkipReason::AlreadyConverted, n_converted),
        (SkipReason::OutputExists, n_output_exists),
    ] {
        if count > 0 {
            skipped.push(SkipCount { reason, count });
        }
    }

    Ok(AddResult { added, skipped })
}
```

- [ ] **Step 6: Update `add_files_inner` and the two commands**

In `add_files_inner`, the final line already returns the call result — only its return type changes:

```rust
pub(crate) fn add_files_inner(state: &AppState, paths: &[String]) -> Result<AddResult, String> {
```
(the body is unchanged; it still ends with `add_files_to_db(&conn, paths, &preset, &suffix, skip_already_converted)`).

Update the two command signatures:

```rust
#[tauri::command]
pub fn add_files(state: State<'_, AppState>, paths: Vec<String>) -> Result<AddResult, String> {
    add_files_inner(&state, &paths)
}
```
```rust
#[tauri::command]
pub fn confirm_folder_add(
    state: State<'_, AppState>,
    path: String,
) -> Result<AddResult, String> {
    // ... unchanged body; still ends with `add_files_inner(&state, &paths)`
}
```

- [ ] **Step 7: Update the watcher caller**

In `src-tauri/src/watcher.rs`, `enqueue_and_start`:

```rust
    let result = match queue::add_files_inner(&app_state, &paths) {
        Ok(result) => result,
        Err(err) => {
            eprintln!("watcher: failed to enqueue {paths:?}: {err}");
            return;
        }
    };
    if result.added.is_empty() {
        return;
    }
```

- [ ] **Step 8: Run the tests to verify they pass**

Run: `cargo test --manifest-path src-tauri/Cargo.toml`
Expected: PASS (all queue tests including the new in-place test).

- [ ] **Step 9: Commit**

```bash
git add src-tauri/src/commands/queue.rs src-tauri/src/watcher.rs src-tauri/src/types.rs src-tauri/src/converter.rs
git commit -m "feat: queue mp4 in-place jobs and return per-reason skip counts"
```

---

## Task 3: In-place helpers in `converter.rs`

**Files:**
- Modify: `src-tauri/src/converter.rs` (helpers + tests)

- [ ] **Step 1: Add the path/decision helpers**

`IN_PLACE_TEMP_MARKER` was added to `src-tauri/src/converter.rs` in Task 2 Step 3. Add the remaining helpers near it (above `ConverterState`):

```rust
/// A job re-encodes a file onto itself exactly when its stored output path equals its source.
pub(crate) fn is_in_place(source_path: &str, output_path: &str) -> bool {
    source_path == output_path
}

/// Temp output path for an in-place encode: a hidden, marked sibling in the SAME directory so the
/// final `rename` is atomic (same filesystem). Keeps `.mp4` so HandBrake's container matches the
/// distinct-file path.
pub(crate) fn in_place_temp_path(source_path: &str) -> std::path::PathBuf {
    let p = std::path::Path::new(source_path);
    let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("output");
    let parent = p.parent().unwrap_or_else(|| std::path::Path::new("."));
    parent.join(format!(".{stem}{IN_PLACE_TEMP_MARKER}mp4"))
}

/// Filesystem action for an in-place job once the keep/discard decision is made. Pure mapping so
/// it can be table-tested apart from the side effects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InPlaceAction {
    /// Re-encode won — overwrite the source with the temp (cleanup_mode = delete).
    RenameTempOverSource,
    /// Re-encode won — move the source to Trash first, then put the temp in its place (trash mode).
    TrashSourceThenRename,
    /// Re-encode lost or produced nothing usable — drop the temp, keep the source.
    RemoveTemp,
}

fn in_place_action(kept: KeptFile, cleanup_mode: &str) -> InPlaceAction {
    match kept {
        KeptFile::Converted => {
            if cleanup_mode == "delete" {
                InPlaceAction::RenameTempOverSource
            } else {
                InPlaceAction::TrashSourceThenRename
            }
        }
        KeptFile::Original | KeptFile::Neither => InPlaceAction::RemoveTemp,
    }
}

fn apply_in_place_action(
    action: InPlaceAction,
    temp: &std::path::Path,
    source: &std::path::Path,
) -> std::io::Result<()> {
    match action {
        InPlaceAction::RenameTempOverSource => std::fs::rename(temp, source),
        InPlaceAction::TrashSourceThenRename => {
            let _ = trash::delete(source);
            std::fs::rename(temp, source)
        }
        InPlaceAction::RemoveTemp => std::fs::remove_file(temp),
    }
}
```

- [ ] **Step 2: Write the failing tests**

Add to the `tests` module in `src-tauri/src/converter.rs`:

```rust
#[test]
fn is_in_place_only_when_paths_match() {
    assert!(is_in_place("/m/clip.mp4", "/m/clip.mp4"));
    assert!(!is_in_place("/m/clip.mkv", "/m/clip.mp4"));
    assert!(!is_in_place("/m/clip.mp4", "/m/clip-conv.mp4"));
}

#[test]
fn in_place_temp_path_is_marked_hidden_sibling() {
    let temp = in_place_temp_path("/movies/clip.mp4");
    assert_eq!(temp, std::path::Path::new("/movies/.clip.convertbar-tmp.mp4"));
    // The marker must round-trip so is_video_file can exclude it.
    assert!(temp.to_string_lossy().contains(IN_PLACE_TEMP_MARKER));
}

#[test]
fn in_place_action_maps_decision_to_filesystem_op() {
    // Re-encode won.
    assert_eq!(in_place_action(KeptFile::Converted, "delete"), InPlaceAction::RenameTempOverSource);
    assert_eq!(in_place_action(KeptFile::Converted, "trash"), InPlaceAction::TrashSourceThenRename);
    // Re-encode lost / nothing usable -> keep original, drop temp, regardless of cleanup mode.
    assert_eq!(in_place_action(KeptFile::Original, "delete"), InPlaceAction::RemoveTemp);
    assert_eq!(in_place_action(KeptFile::Neither, "trash"), InPlaceAction::RemoveTemp);
}

#[test]
fn apply_rename_replaces_source_with_temp() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("clip.mp4");
    let temp = dir.path().join(".clip.convertbar-tmp.mp4");
    std::fs::write(&source, b"original").unwrap();
    std::fs::write(&temp, b"reencoded").unwrap();

    apply_in_place_action(InPlaceAction::RenameTempOverSource, &temp, &source).unwrap();

    assert_eq!(std::fs::read(&source).unwrap(), b"reencoded", "source now holds the re-encode");
    assert!(!temp.exists(), "temp was consumed by the rename");
}

#[test]
fn apply_remove_temp_keeps_source_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("clip.mp4");
    let temp = dir.path().join(".clip.convertbar-tmp.mp4");
    std::fs::write(&source, b"original").unwrap();
    std::fs::write(&temp, b"bigger-reencode").unwrap();

    apply_in_place_action(InPlaceAction::RemoveTemp, &temp, &source).unwrap();

    assert!(!temp.exists(), "temp was removed");
    assert_eq!(std::fs::read(&source).unwrap(), b"original", "source is left exactly as it was");
}
```

- [ ] **Step 3: Run the tests**

Run: `cargo test --manifest-path src-tauri/Cargo.toml in_place`
Expected: PASS (5 new tests). Also verify the `is_video_file` exclusion test path: `cargo test --manifest-path src-tauri/Cargo.toml is_video` still passes from Task 2.

- [ ] **Step 4: Add the `is_video_file` temp-marker regression test**

In `src-tauri/src/commands/queue.rs` tests module:

```rust
#[test]
fn rejects_in_place_temp_files() {
    // A lingering in-place temp must never be picked up by a folder scan or watched folder.
    assert!(!is_video_file(Path::new("/movies/.clip.convertbar-tmp.mp4")));
}
```

Run: `cargo test --manifest-path src-tauri/Cargo.toml rejects_in_place_temp_files`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/converter.rs src-tauri/src/commands/queue.rs
git commit -m "feat: add in-place temp-path and cleanup-action helpers"
```

---

## Task 4: `process_queue` — encode in-place jobs to the temp

**Files:**
- Modify: `src-tauri/src/converter.rs` (`process_queue`: encode target, success cleanup, failure cleanup)

- [ ] **Step 1: Select the encode target before spawning HandBrake**

In `process_queue`, immediately before the `// Spawn HandBrakeCLI` block, insert:

```rust
        let in_place = is_in_place(&job.source_path, &job.output_path);
        let encode_target = if in_place {
            in_place_temp_path(&job.source_path)
        } else {
            std::path::PathBuf::from(&job.output_path)
        };
        if in_place {
            // Clear any stale temp left by a previous crash so HandBrake writes a fresh file.
            let _ = std::fs::remove_file(&encode_target);
        }
```

Change the spawn's output argument from `&job.output_path` to `&encode_target`:

```rust
        let child = Command::new(&handbrake_path)
            .arg("-Z")
            .arg(&job.preset)
            .arg("-O")
            .arg("-i")
            .arg(&job.source_path)
            .arg("-o")
            .arg(&encode_target)
            .stderr(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn();
```

- [ ] **Step 2: Use the temp for size + cleanup in the success branch**

In the `Ok(status) if status.success()` branch, replace the size computation and the `match kept { ... }` cleanup block:

```rust
                let converted_size = std::fs::metadata(&encode_target)
                    .map(|m| m.len() as i64)
                    .ok();
                // For in-place, the source is unchanged during the temp encode, so re-stat it now.
                let original_size = if in_place {
                    std::fs::metadata(&job.source_path)
                        .map(|m| m.len() as i64)
                        .unwrap_or(job.original_size.unwrap_or(0))
                } else {
                    job.original_size.unwrap_or(0)
                };
                let conv_size = converted_size.unwrap_or(0);

                let (kept, space_saved, status_str) = decide_cleanup(original_size, conv_size);

                // Act on the decision. In-place replaces/keeps the source via the temp; the
                // distinct-file path keeps both names and trashes/deletes the loser as before.
                if in_place {
                    let action = in_place_action(kept, &cleanup_mode);
                    let _ = apply_in_place_action(
                        action,
                        &encode_target,
                        std::path::Path::new(&job.source_path),
                    );
                } else {
                    match kept {
                        KeptFile::Converted => match cleanup_mode.as_str() {
                            "delete" => {
                                let _ = std::fs::remove_file(&job.source_path);
                            }
                            _ => {
                                let _ = trash::delete(&job.source_path);
                            }
                        },
                        KeptFile::Original => match cleanup_mode.as_str() {
                            "delete" => {
                                let _ = std::fs::remove_file(&job.output_path);
                            }
                            _ => {
                                let _ = trash::delete(&job.output_path);
                            }
                        },
                        KeptFile::Neither => {}
                    }
                }
```

Everything after this (the `kept_file` string, the DB `UPDATE`, the emits, and notifications) is unchanged: for in-place, `output_path` stays equal to `source_path`, and `converted_size`/`space_saved` were computed from the temp before the action ran.

- [ ] **Step 3: Target the temp in the failure branch (the safety fix)**

In the `Ok(_) | Err(_)` branch, change the partial-output cleanup so a failed/cancelled in-place encode never deletes the source:

```rust
            Ok(_) | Err(_) => {
                had_errors = true;
                // Remove the partial encode output (the temp for in-place jobs), never the source.
                let _ = std::fs::remove_file(&encode_target);
```
(the rest of the branch is unchanged.)

- [ ] **Step 4: Build and run the full Rust suite**

Run: `cargo test --manifest-path src-tauri/Cargo.toml`
Expected: PASS (existing tests + the helper tests from Task 3). `process_queue` itself has no unit test (it shells out to HandBrake); it is covered by the helper tests and the manual verification step below.

- [ ] **Step 5: Manual verification (in-place happy path)**

With a built app and HandBrakeCLI installed, set an empty output suffix, drop a single `.mp4`, and confirm:
- a "In place" job appears (Task 8 adds the badge),
- on completion the file keeps its exact name + `.mp4` extension,
- if the re-encode is smaller the original is replaced (trash mode → original recoverable from Trash); if larger, the file is unchanged and the job is "kept original"/skipped,
- no `.<name>.convertbar-tmp.mp4` file is left behind.

Document the result in the commit body.

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/converter.rs
git commit -m "feat: re-encode mp4 in place via temp file and atomic rename"
```

---

## Task 5: `cancel_conversion` — remove the temp, not the source

**Files:**
- Modify: `src-tauri/src/commands/converter.rs:214-266`

- [ ] **Step 1: Fetch source_path alongside output_path**

Replace the `let (output_path, update_result) = match job_id_val { ... }` block so it also reads `source_path`:

```rust
    let (paths, update_result) = match job_id_val {
        Some(ref job_id) => {
            let db = state.db.lock().map_err(|e| e.to_string())?;
            let paths: Option<(String, String)> = db
                .query_row(
                    "SELECT source_path, output_path FROM jobs WHERE id = ?1",
                    rusqlite::params![job_id],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                )
                .ok();
            let update_result = db.execute(
                "UPDATE jobs SET status = 'error', error_message = 'Cancelled by user' WHERE id = ?1",
                rusqlite::params![job_id],
            );
            (paths, Some(update_result))
        }
        None => (None, None),
    };
```

- [ ] **Step 2: Remove the correct partial file**

Replace the removal block (currently `if let Some(path) = output_path { let _ = std::fs::remove_file(&path); }`):

```rust
    if let Some(ref job_id) = job_id_val {
        if let Some((ref source_path, ref output_path)) = paths {
            // For an in-place job output_path == source_path, so deleting output_path would delete
            // the user's original. Remove the temp instead; otherwise remove the partial output.
            let target = if crate::converter::is_in_place(source_path, output_path) {
                crate::converter::in_place_temp_path(source_path)
            } else {
                std::path::PathBuf::from(output_path)
            };
            let _ = std::fs::remove_file(&target);
        }
```
(keep the existing `app.emit(...)` calls that follow, unchanged.)

- [ ] **Step 3: Build and run the suite**

Run: `cargo test --manifest-path src-tauri/Cargo.toml`
Expected: PASS. Also `cargo build --manifest-path src-tauri/Cargo.toml` clean.

- [ ] **Step 4: Manual verification (cancel safety)**

Start an in-place `.mp4` encode, cancel it mid-run, and confirm the **original file still exists** and only the `.convertbar-tmp.mp4` is gone.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/commands/converter.rs
git commit -m "fix: cancelling an in-place encode removes the temp, not the source"
```

---

## Task 6: Frontend `AddResult` types + command return types

**Files:**
- Modify: `src/lib/tauri.ts`

- [ ] **Step 1: Add the types**

In `src/lib/tauri.ts`, after the `JobInfo` interface, add:

```ts
export type SkipReason =
  | "not_video"
  | "already_queued"
  | "already_converted"
  | "output_exists";

export interface SkipCount {
  reason: SkipReason;
  count: number;
}

export interface AddResult {
  added: JobInfo[];
  skipped: SkipCount[];
}
```

- [ ] **Step 2: Update the two command return types**

```ts
  addFiles: (paths: string[]) => invoke<AddResult>("add_files", { paths }),
  confirmFolderAdd: (path: string) =>
    invoke<AddResult>("confirm_folder_add", { path }),
```

- [ ] **Step 3: Type-check**

Run: `npx tsc --noEmit`
Expected: errors in `DropZone.tsx` (it still treats the results as arrays) — that is fixed in Task 8. No errors originating in `tauri.ts`.

- [ ] **Step 4: Commit**

```bash
git add src/lib/tauri.ts
git commit -m "feat: type add commands as AddResult"
```

---

## Task 7: Pure `summarizeAdds` helper

**Files:**
- Create: `src/lib/addSummary.ts`
- Test: `src/lib/addSummary.test.ts`

- [ ] **Step 1: Write the failing test**

`src/lib/addSummary.test.ts`:

```ts
import { describe, it, expect } from "vitest";
import { summarizeAdds } from "./addSummary";
import type { AddResult } from "./tauri";

describe("summarizeAdds", () => {
  it("returns null when nothing was added or skipped", () => {
    expect(summarizeAdds([{ added: [], skipped: [] }])).toBeNull();
  });

  it("reports only the added count when there are no skips", () => {
    const r: AddResult = {
      added: [{} as never, {} as never],
      skipped: [],
    };
    expect(summarizeAdds([r])).toBe("Added 2");
  });

  it("sums added and merges skip reasons across results in a stable order", () => {
    const a: AddResult = {
      added: [{} as never],
      skipped: [{ reason: "output_exists", count: 1 }],
    };
    const b: AddResult = {
      added: [{} as never, {} as never],
      skipped: [
        { reason: "output_exists", count: 2 },
        { reason: "already_converted", count: 1 },
      ],
    };
    expect(summarizeAdds([a, b])).toBe(
      "Added 3 · 3 skipped (output exists) · 1 skipped (already converted)",
    );
  });

  it("renders a skips-only summary when nothing was added", () => {
    const r: AddResult = {
      added: [],
      skipped: [{ reason: "not_video", count: 2 }],
    };
    expect(summarizeAdds([r])).toBe("2 skipped (not a video)");
  });
});
```

- [ ] **Step 2: Run it to verify it fails**

Run: `npx vitest run src/lib/addSummary.test.ts`
Expected: FAIL (`summarizeAdds` not found).

- [ ] **Step 3: Implement the helper**

`src/lib/addSummary.ts`:

```ts
import type { AddResult, SkipReason } from "./tauri";

const SKIP_LABELS: Record<SkipReason, string> = {
  output_exists: "output exists",
  already_converted: "already converted",
  already_queued: "already queued",
  not_video: "not a video",
};

// Fixed order so the rendered summary is deterministic regardless of backend ordering.
const REASON_ORDER: SkipReason[] = [
  "output_exists",
  "already_converted",
  "already_queued",
  "not_video",
];

/** Aggregate one or more AddResults into a single human-readable status line, or null if empty. */
export function summarizeAdds(results: AddResult[]): string | null {
  const added = results.reduce((n, r) => n + r.added.length, 0);
  const counts = new Map<SkipReason, number>();
  for (const r of results) {
    for (const s of r.skipped) {
      counts.set(s.reason, (counts.get(s.reason) ?? 0) + s.count);
    }
  }

  const parts: string[] = [];
  if (added > 0) parts.push(`Added ${added}`);
  for (const reason of REASON_ORDER) {
    const count = counts.get(reason);
    if (count) parts.push(`${count} skipped (${SKIP_LABELS[reason]})`);
  }

  return parts.length > 0 ? parts.join(" · ") : null;
}
```

- [ ] **Step 4: Run it to verify it passes**

Run: `npx vitest run src/lib/addSummary.test.ts`
Expected: PASS (4 tests).

- [ ] **Step 5: Commit**

```bash
git add src/lib/addSummary.ts src/lib/addSummary.test.ts
git commit -m "feat: add summarizeAdds for add-time skip feedback"
```

---

## Task 8: DropZone shows the skip summary

**Files:**
- Modify: `src/components/DropZone.tsx`
- Modify: `src/components/DropZone.test.tsx`

- [ ] **Step 1: Update the existing test mocks and add a summary test**

In `src/components/DropZone.test.tsx`, change the `add_files` and `confirm_folder_add` mocks in `beforeEach` to return `AddResult` shape:

```ts
      case "add_files":
        return Promise.resolve({ added: [], skipped: [] });
      case "confirm_folder_add":
        return Promise.resolve({ added: [], skipped: [] });
```

Add a test asserting the summary renders (place inside the existing `describe("DropZone", ...)`):

```ts
  it("shows a per-reason skip summary after an add", async () => {
    classified = { files: ["/movies/a.mp4", "/movies/b.txt"], folders: [] };
    invokeMock.mockImplementation(((cmd: string) => {
      switch (cmd) {
        case "classify_paths":
          return Promise.resolve(classified);
        case "add_files":
          return Promise.resolve({
            added: [{ id: "1" }],
            skipped: [{ reason: "not_video", count: 1 }],
          });
        case "start_queue":
          return Promise.resolve(undefined);
        default:
          return Promise.reject(new Error(`unexpected invoke: ${cmd}`));
      }
    }) as typeof invoke);

    render(<DropZone onFilesAdded={() => {}} />);
    fireDrop(["/movies/a.mp4", "/movies/b.txt"]);

    await waitFor(() =>
      expect(screen.getByText("Added 1 · 1 skipped (not a video)")).toBeInTheDocument(),
    );
  });
```

- [ ] **Step 2: Run it to verify the new test fails**

Run: `npx vitest run src/components/DropZone.test.tsx`
Expected: the new test FAILS (no summary rendered yet); pre-existing tests should still pass after the mock-shape update.

- [ ] **Step 3: Consume AddResult in `handlePaths`**

In `src/components/DropZone.tsx`, add the import:

```tsx
import { commands, FolderScanResult, AddResult } from "../lib/tauri";
import { summarizeAdds } from "../lib/addSummary";
```

Rewrite `handlePaths` to collect results and show the summary:

```tsx
  const handlePaths = useCallback(
    async (paths: string[]) => {
      setStatus("Adding files...");
      try {
        const classified = await commands.classifyPaths(paths);
        const results: AddResult[] = [];

        if (classified.files.length > 0) {
          results.push(await commands.addFiles(classified.files));
        }

        const toConfirm: FolderScanResult[] = [];
        for (const folder of classified.folders) {
          if (folder.file_count === 0) continue;
          if (folder.file_count <= 5) {
            results.push(await commands.confirmFolderAdd(folder.folder_path));
          } else {
            toConfirm.push(folder);
          }
        }

        if (toConfirm.length > 0) {
          setPendingFolders(toConfirm);
          setStatus(summarizeAdds(results));
        } else {
          await commands.startQueue();
          onFilesAdded();
          const summary = summarizeAdds(results);
          setStatus(summary);
          if (summary) setTimeout(() => setStatus(null), 4000);
        }
      } catch (e) {
        setStatus(`Error: ${e}`);
        setTimeout(() => setStatus(null), 3000);
      }
    },
    [onFilesAdded],
  );
```

- [ ] **Step 4: Show a summary for the manual folder-confirm path**

In the "Add" button `onClick` for `pendingFolders`, capture the result and surface it when the last folder is confirmed:

```tsx
                <button className="btn btn-small" onClick={async () => {
                  const res = await commands.confirmFolderAdd(folder.folder_path);
                  const remaining = pendingFolders.filter((_, j) => j !== i);
                  setPendingFolders(remaining);
                  if (remaining.length === 0) {
                    await commands.startQueue();
                    onFilesAdded();
                    const summary = summarizeAdds([res]);
                    setStatus(summary);
                    if (summary) setTimeout(() => setStatus(null), 4000);
                  }
                }}>Add</button>
```

(The "Skip" button is unchanged.)

- [ ] **Step 5: Run the DropZone tests**

Run: `npx vitest run src/components/DropZone.test.tsx`
Expected: PASS (existing + new summary test).

- [ ] **Step 6: Commit**

```bash
git add src/components/DropZone.tsx src/components/DropZone.test.tsx
git commit -m "feat: surface per-reason skip counts in the drop zone"
```

---

## Task 9: QueueItem "In place" badge

**Files:**
- Modify: `src/components/QueueItem.tsx`
- Create: `src/components/QueueItem.test.tsx`

- [ ] **Step 1: Write the failing test**

`src/components/QueueItem.test.tsx`:

```tsx
import { describe, it, expect, vi } from "vitest";
import { render, screen } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

import QueueItem from "./QueueItem";
import type { JobInfo } from "../lib/tauri";

function job(overrides: Partial<JobInfo>): JobInfo {
  return {
    id: "1",
    source_path: "/m/clip.mp4",
    output_path: "/m/clip-conv.mp4",
    preset: "p",
    status: "queued",
    original_size: null,
    converted_size: null,
    kept_file: null,
    space_saved: null,
    error_message: null,
    queue_order: 0,
    created_at: "",
    completed_at: null,
    ...overrides,
  };
}

describe("QueueItem", () => {
  it("shows an In place badge when output equals source", () => {
    render(<QueueItem job={job({ output_path: "/m/clip.mp4" })} onRemoved={() => {}} />);
    expect(screen.getByText("In place")).toBeInTheDocument();
  });

  it("shows no In place badge for a distinct output", () => {
    render(<QueueItem job={job({ output_path: "/m/clip-conv.mp4" })} onRemoved={() => {}} />);
    expect(screen.queryByText("In place")).not.toBeInTheDocument();
  });
});
```

- [ ] **Step 2: Run it to verify it fails**

Run: `npx vitest run src/components/QueueItem.test.tsx`
Expected: FAIL (no "In place" text).

- [ ] **Step 3: Render the badge**

In `src/components/QueueItem.tsx`, compute the flag and render the badge before the "Queued" badge:

```tsx
export default function QueueItem({ job, onRemoved, onDragStart, onDragOver, onDrop, isDragOver }: QueueItemProps) {
  const isInPlace = job.source_path === job.output_path;

  const handleRemove = async () => {
```

```tsx
      <span className="queue-item-name" title={job.source_path}>
        {fileName(job.source_path)}
      </span>
      {isInPlace && (
        <span className="badge badge-dim" title="Re-encoded in place, replacing the original">
          In place
        </span>
      )}
      <span className="badge badge-dim">Queued</span>
```

- [ ] **Step 4: Run it to verify it passes**

Run: `npx vitest run src/components/QueueItem.test.tsx`
Expected: PASS (2 tests).

- [ ] **Step 5: Commit**

```bash
git add src/components/QueueItem.tsx src/components/QueueItem.test.tsx
git commit -m "feat: badge in-place jobs in the queue"
```

---

## Task 10: SettingsPage empty-suffix note + style

**Files:**
- Modify: `src/pages/SettingsPage.tsx`
- Modify: `src/App.css`

- [ ] **Step 1: Add the note under the suffix preview**

In `src/pages/SettingsPage.tsx`, inside the `<>...</>` that renders the suffix input, immediately after the `<div className="suffix-preview">...</div>` block, add:

```tsx
            {resolvedSuffix.trim() === "" && (
              <div className="suffix-inplace-note">
                Empty suffix: mp4 files are re-encoded in place, replacing the original. The fast
                &quot;already converted&quot; skip-by-suffix is also disabled.
              </div>
            )}
```

(`resolvedSuffix` is already computed at `SettingsPage.tsx:84`.)

- [ ] **Step 2: Add the style**

In `src/App.css`, after the `.suffix-preview span { ... }` rule (around line 630), add:

```css
.suffix-inplace-note {
  margin-top: 6px;
  font-size: 12px;
  line-height: 1.4;
  color: var(--text-dim, #888);
}
```

(If `--text-dim` is not a defined variable in this file, match the color used by `.suffix-preview`; check the existing rule and reuse its color token.)

- [ ] **Step 3: Type-check and verify the full frontend suite**

Run: `npx tsc --noEmit && npm test`
Expected: no type errors; all Vitest tests pass.

- [ ] **Step 4: Manual verification**

Open Settings, clear the output suffix template, and confirm the note appears; type a non-empty template and confirm it disappears.

- [ ] **Step 5: Commit**

```bash
git add src/pages/SettingsPage.tsx src/App.css
git commit -m "feat: explain in-place + lost suffix-skip when output suffix is empty"
```

---

## Final verification

- [ ] **Full Rust suite:** `cargo test --manifest-path src-tauri/Cargo.toml` — all green.
- [ ] **Full frontend suite + types:** `npx tsc --noEmit && npm test` — all green.
- [ ] **Frontend build (release type-check parity with CI):** `npm run build` — succeeds.
- [ ] **Cross-platform review:** dispatch the `cross-platform-reviewer` agent over `converter.rs` and `commands/converter.rs` — confirm `std::fs::rename` is relied on for an atomic same-filesystem replace (Unix + Windows `MOVEFILE_REPLACE_EXISTING`) and nothing is gated incorrectly.
- [ ] **ACL check:** no new frontend Tauri command names were introduced (only return types changed), so `capabilities/default.json` needs no edits — confirm with the `acl-auditor` agent.

## Spec coverage map

| Spec section | Task |
|---|---|
| A1 in-place detection (output == source queues) | Task 2 |
| A2 temp encode + decide_cleanup actions (rename / trash+rename / remove-temp) | Tasks 3, 4 |
| A2 temp not re-enqueued by scans/watcher | Tasks 2 (`is_video_file`), 3 (`IN_PLACE_TEMP_MARKER`) |
| A3 failure deletes temp not source | Task 4 |
| A3/cancel safety (second deletion site) | Task 5 |
| A4 cross-platform atomic rename | Final verification |
| B1 AddResult/SkipReason + reason mapping | Tasks 1, 2 |
| B1 watcher uses `.added` | Task 2 |
| B2 DropZone summary | Tasks 7, 8 |
| C QueueItem in-place badge | Task 9 |
| C SettingsPage empty-suffix note (both consequences) | Task 10 |
| Size guard = "kept original" | reuses `decide_cleanup` (unchanged), exercised in Task 4 |
