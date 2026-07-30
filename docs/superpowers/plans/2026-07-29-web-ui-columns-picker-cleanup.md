# Web UI Columns, File-Picker Overhaul, and a Third Cleanup Mode — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give the Docker web UI a real multi-column layout, a file picker that can express "these folders and that file", and a `keep` cleanup mode that leaves originals on disk while the user evaluates their encodes.

**Architecture:** Four independent slices, each its own PR. The only core change is a third `cleanup_mode` value, enforced so an impossible in-place+keep job is never created rather than refused after the fact. Everything else is frontend: a `matchMedia` hook that decides which pages mount, a rewritten selection model in `FileBrowserModal`, and an intake surface that stops advertising drag-and-drop.

**Tech Stack:** Rust (rusqlite, axum), React 19 + TypeScript, vitest + React Testing Library, Vite. Cargo workspace: `crates/convertbar-core` (head-agnostic), `src-tauri` (desktop), `crates/convertbar-server` (headless).

**Spec:** `docs/superpowers/specs/2026-07-29-web-ui-columns-picker-cleanup-design.md`

## Global Constraints

- **Never emit a Tauri/sink event while holding `ctx.db`'s lock.** The desktop tray listener re-locks `ctx.db` synchronously on the same thread and `std::sync::Mutex` is not reentrant. Drop the guard first. Two shipped deadlocks came from violating this.
- **Test fixtures must declare their HandBrake world.** `PanickingLocator` is the default; use `StubLocator` for the installed world, `AbsentLocator` for the CI world. A `PanickingLocator` panic on the queue thread poisons `ctx.db` and surfaces as a confusing `PoisonError`.
- **`cargo fmt` before every Rust commit.** CI does not gate it, but the repo is fmt-clean.
- **Strict TypeScript.** No `any`. Run `npm run build` (type-check) before any frontend commit.
- **Commands:** `cargo test --workspace`, `npm test`, `npm run build`.
- **Commit style:** conventional commits (`feat:`, `fix:`, `test:`, `docs:`, `refactor:`).
- **Claude cannot `git push`.** Ask the user to run `! git push -u origin <branch>`. Merge with `gh pr merge <n> --admin --squash`.
- **Branch:** all work lands on `feature/web-ui-columns-picker-cleanup` unless a PR boundary says otherwise.

---

## File Structure

**Created:**

| File | Responsibility |
|---|---|
| `src/lib/pathSelection.ts` | One pure function: the inclusive slice of a listing between two paths. No React, no I/O. |
| `src/lib/pathSelection.test.ts` | Its tests. |
| `src/hooks/useLayoutMode.ts` | Wraps `matchMedia`; returns `"tabs" \| "two-col" \| "three-col"`. The single source of truth for which pages mount. |
| `src/hooks/useLayoutMode.test.ts` | Its tests. |

**Modified:**

| File | Change |
|---|---|
| `crates/convertbar-core/src/settings_ops.rs` | `normalize_cleanup_mode`, `read_cleanup_mode`, and the `cleanup_mode → keep` post-write hook. |
| `crates/convertbar-core/src/converter.rs` | `"keep"` arms in the cleanup match and `in_place_action`; `get_cleanup_mode` delegates to `settings_ops`. |
| `crates/convertbar-core/src/queue_ops.rs` | Add-time in-place+keep block; `drop_queued_in_place_jobs`. |
| `crates/convertbar-core/src/types.rs` | `SkipReason::InPlaceKeepBlocked`. |
| `crates/convertbar-server/src/startup.rs` | Test only: `'keep'` survives boot normalization. |
| `src/lib/transport/types.ts` | The new `SkipReason` member. |
| `src/lib/addSummary.ts` | Its label and sort position. |
| `src/pages/SettingsPage.tsx` | Three/two cleanup radios plus the Keep lifecycle copy. |
| `src/components/FileBrowserModal.tsx` | The whole selection model. |
| `src/components/FileBrowserModal.test.tsx` | Existing navigation tests move to the `→` button. |
| `src/App.tsx` | Column rendering driven by `useLayoutMode`. |
| `src/components/TabBar.tsx` | Renders a tab subset. |
| `src/components/DropZone.tsx` | Optional `onPick` click surface. |
| `src/pages/QueuePage.tsx` | Drops the standalone intake button; head-aware empty state. |
| `src/App.css` | Column layout, picker rows, pick surface. |
| `README.md`, `CLAUDE.md`, `unraid-template.xml` | Docs for the keep mode and the picker. |

---

# PR 1 — `cleanup_mode = "keep"`

Land and merge this before starting PR 2. It is the only change on a destructive path.

---

### Task 1: Normalize `cleanup_mode` through one function

**Files:**
- Modify: `crates/convertbar-core/src/settings_ops.rs`
- Modify: `crates/convertbar-core/src/converter.rs:455-462`

**Interfaces:**
- Produces: `settings_ops::normalize_cleanup_mode(&str) -> &'static str`, `settings_ops::read_cleanup_mode(&rusqlite::Connection) -> String`. Tasks 2, 3 and 4 both call `read_cleanup_mode`.

- [ ] **Step 1: Write the failing test**

Append to the `tests` module in `crates/convertbar-core/src/settings_ops.rs`:

```rust
#[test]
fn cleanup_mode_normalizes_to_three_known_values() {
    // Exact matches pass through; everything else reads as "trash". The fallback is
    // deliberately NOT "keep": it preserves the behavior every existing row already has,
    // and mirrors normalize_bad_source_action's convention.
    assert_eq!(normalize_cleanup_mode("keep"), "keep");
    assert_eq!(normalize_cleanup_mode("delete"), "delete");
    assert_eq!(normalize_cleanup_mode("trash"), "trash");
    assert_eq!(normalize_cleanup_mode(""), "trash");
    assert_eq!(normalize_cleanup_mode("KEEP"), "trash");
    assert_eq!(normalize_cleanup_mode("nonsense"), "trash");
}

#[test]
fn read_cleanup_mode_normalizes_what_it_reads() {
    let conn = test_conn();
    // init_db seeds 'trash'.
    assert_eq!(read_cleanup_mode(&conn), "trash");

    conn.execute(
        "UPDATE settings SET value = 'keep' WHERE key = 'cleanup_mode'",
        [],
    )
    .unwrap();
    assert_eq!(read_cleanup_mode(&conn), "keep");

    // A corrupted row must never reach a call site as a raw string.
    conn.execute(
        "UPDATE settings SET value = 'garbage' WHERE key = 'cleanup_mode'",
        [],
    )
    .unwrap();
    assert_eq!(read_cleanup_mode(&conn), "trash");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p convertbar-core -- normalize_cleanup_mode read_cleanup_mode`
Expected: FAIL — `cannot find function 'normalize_cleanup_mode' in this scope`

- [ ] **Step 3: Write minimal implementation**

Add to `crates/convertbar-core/src/settings_ops.rs`, next to `normalize_bad_source_action`:

```rust
/// Coerce a stored `cleanup_mode` to a known value. Exactly `"keep"` or `"delete"` pass
/// through; anything else — corrupted, empty, or written by a newer version — reads as
/// `"trash"`, which is what every pre-existing row already means. Sibling of
/// [`normalize_bad_source_action`].
pub fn normalize_cleanup_mode(value: &str) -> &'static str {
    match value {
        "keep" => "keep",
        "delete" => "delete",
        _ => "trash",
    }
}

/// The stored `cleanup_mode`, normalized. The single read path — `converter` and
/// `queue_ops` both go through this so no call site ever string-compares a raw column.
pub fn read_cleanup_mode(conn: &rusqlite::Connection) -> String {
    let raw: String = conn
        .query_row(
            "SELECT value FROM settings WHERE key = 'cleanup_mode'",
            [],
            |row| row.get(0),
        )
        .unwrap_or_default();
    normalize_cleanup_mode(&raw).to_string()
}
```

- [ ] **Step 4: Point the converter at it**

Replace the body of `get_cleanup_mode` in `crates/convertbar-core/src/converter.rs:455-462` with a delegation:

```rust
fn get_cleanup_mode(db: &Connection) -> String {
    crate::settings_ops::read_cleanup_mode(db)
}
```

- [ ] **Step 5: Run the full core suite**

Run: `cargo test -p convertbar-core`
Expected: PASS — every existing `trash`/`delete` converter test still green, proving the delegation is behavior-preserving.

- [ ] **Step 6: Commit**

```bash
cargo fmt
git add crates/convertbar-core/src/settings_ops.rs crates/convertbar-core/src/converter.rs
git commit -m "refactor(core): one normalized read path for cleanup_mode"
```

---

### Task 2: `keep` disposes nothing

**Files:**
- Modify: `crates/convertbar-core/src/converter.rs:116-127` (`in_place_action`)
- Modify: `crates/convertbar-core/src/converter.rs:1185-1203` (the cleanup match)
- Test: `crates/convertbar-core/src/converter.rs` tests module

**Interfaces:**
- Consumes: `settings_ops::read_cleanup_mode` (Task 1), via the existing `get_cleanup_mode`.
- Produces: nothing new — behavior only.

**Harness already in the file** (`converter.rs` tests module) — use these, do not invent new ones:

| Helper | Line | What it gives you |
|---|---|---|
| `test_ctx(test_conn())` | `:1510` | `(Arc<Ctx>, Arc<TestSink>, Arc<RecordingDisposer>)` |
| `set_setting(&ctx.db, key, value)` | `:1526` | writes one settings row |
| `queue_job(&ctx.db, id, source, output, original_size)` | `:1536` | inserts a `queued` row |
| `real_source(dir, name)` | `:1553` | a real 10-byte file on disk |
| `job_row(&ctx.db, id)` | `:1559` | `(status, error_message)` |
| `successful_fake_handbrake_script(dir)` | `:3468` | a shell script that writes 5 bytes to the output path and exits 0 |

Note the locator: these tests pin `handbrake_path` to the fake script, and
`resolve_with_locator` short-circuits on a configured path, so the fixture default
`PanickingLocator` is never consulted. That is why `test_ctx` is correct here and
`StubLocator` is not needed.

Sizes are chosen so `decide_cleanup` lands deterministically: the script always writes a
5-byte output, so passing `original_size = 1000` to `queue_job` gives "converted won"
and `original_size = 3` gives "converted lost".

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn in_place_action_never_disposes_under_keep() {
    // Release-mode backstop for the race where a job flips to 'encoding' between the
    // setting write and Task 4's dequeue. The `else` branch this replaces is
    // TrashSourceThenRename, which on the server routes through DeleteDisposer and
    // permanently removes the user's source. debug_assert! would compile out.
    assert_eq!(
        in_place_action(KeptFile::Converted, "keep"),
        InPlaceAction::RemoveTemp
    );
    assert_eq!(
        in_place_action(KeptFile::Original, "keep"),
        InPlaceAction::RemoveTemp
    );
    // Unchanged for the two shipping modes.
    assert_eq!(
        in_place_action(KeptFile::Converted, "delete"),
        InPlaceAction::RenameTempOverSource
    );
    assert_eq!(
        in_place_action(KeptFile::Converted, "trash"),
        InPlaceAction::TrashSourceThenRename
    );
}

/// `(status, space_saved)` for a finished job — `job_row` only returns the error message.
fn job_outcome(db: &Arc<Mutex<Connection>>, id: &str) -> (String, Option<i64>) {
    db.lock()
        .unwrap()
        .query_row(
            "SELECT status, space_saved FROM jobs WHERE id = ?1",
            params![id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap()
}

#[test]
fn keep_leaves_both_files_and_records_the_normal_space_saved() {
    // Keep is an evaluation mode: the user verifies the encode, deletes originals by
    // hand, then switches to Delete. So space_saved keeps its usual value — it records
    // how much the encode OPTIMIZED, not how many bytes were freed. Zeroing it would
    // blank the one number the user is evaluating.
    let (ctx, _sink, disposer) = test_ctx(test_conn());

    let dir = tempfile::tempdir().unwrap();
    let script = successful_fake_handbrake_script(dir.path());
    set_setting(&ctx.db, "handbrake_path", script.to_str().unwrap());
    set_setting(&ctx.db, "cleanup_mode", "keep");

    let source = real_source(dir.path(), "movie.mkv");
    let out = dir.path().join("movie.mp4");
    // The script writes 5 bytes, so 1000 makes the re-encode the winner.
    queue_job(&ctx.db, "j1", source.to_str().unwrap(), out.to_str().unwrap(), 1000);

    process_queue(&ctx);

    assert!(source.exists(), "keep must not remove the source");
    assert!(out.exists(), "keep must not remove the output");
    assert!(
        disposer.0.lock().unwrap().is_empty(),
        "nothing may be routed to the disposer under keep"
    );

    let (status, space_saved) = job_outcome(&ctx.db, "j1");
    assert_eq!(status, "done");
    assert_eq!(space_saved, Some(995), "1000 - 5, exactly as delete would record");
}

#[test]
fn keep_with_a_larger_output_keeps_both_and_still_records_skipped() {
    let (ctx, _sink, disposer) = test_ctx(test_conn());

    let dir = tempfile::tempdir().unwrap();
    let script = successful_fake_handbrake_script(dir.path());
    set_setting(&ctx.db, "handbrake_path", script.to_str().unwrap());
    set_setting(&ctx.db, "cleanup_mode", "keep");

    let source = real_source(dir.path(), "movie.mkv");
    let out = dir.path().join("movie.mp4");
    // 3 < the script's 5-byte output, so the re-encode loses.
    queue_job(&ctx.db, "j1", source.to_str().unwrap(), out.to_str().unwrap(), 3);

    process_queue(&ctx);

    assert!(source.exists());
    assert!(out.exists(), "even a losing output survives under keep");
    assert!(disposer.0.lock().unwrap().is_empty());

    let (status, space_saved) = job_outcome(&ctx.db, "j1");
    assert_eq!(status, "skipped");
    // The negative delta, identical to what delete records for the same sizes: keep
    // changed the disposal and nothing else.
    assert_eq!(space_saved, Some(-2));
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p convertbar-core -- keep_leaves_both in_place_action_never_disposes keep_with_a_larger`
Expected: FAIL — the source file is gone (the `_` arm disposed it) and `in_place_action` returns `TrashSourceThenRename`.

- [ ] **Step 3: Add the `keep` arm to `in_place_action`**

```rust
fn in_place_action(kept: KeptFile, cleanup_mode: &str) -> InPlaceAction {
    match kept {
        // "keep" is prevented at add time and at setting-change time (queue_ops), so this
        // arm only covers the race where a job flips to 'encoding' in between. Discarding
        // the temp is the non-destructive outcome: a wasted encode, never a lost original.
        KeptFile::Converted if cleanup_mode == "keep" => InPlaceAction::RemoveTemp,
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
```

- [ ] **Step 4: Add the `keep` arm to the distinct-file cleanup**

In `crates/convertbar-core/src/converter.rs:1185-1203`, guard the whole match:

```rust
} else if cleanup_mode == "keep" {
    // Keep both files. decide_cleanup still ran, so status and space_saved are
    // unchanged — only the disposal is skipped.
    false
} else {
    match kept {
        KeptFile::Converted => match cleanup_mode.as_str() {
            "delete" => {
                let _ = std::fs::remove_file(&job.source_path);
            }
            _ => {
                let _ = ctx.disposer.dispose(&job.source_path);
            }
        },
        KeptFile::Original => match cleanup_mode.as_str() {
            "delete" => {
                let _ = std::fs::remove_file(&job.output_path);
            }
            _ => {
                let _ = ctx.disposer.dispose(&job.output_path);
            }
        },
        KeptFile::Neither => {}
    }
    false
};
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p convertbar-core`
Expected: PASS, including every pre-existing `trash`/`delete` test.

- [ ] **Step 6: Commit**

```bash
cargo fmt
git add crates/convertbar-core/src/converter.rs
git commit -m "feat(core): a keep cleanup mode that disposes nothing"
```

---

### Task 3: Block in-place jobs at add time

**Files:**
- Modify: `crates/convertbar-core/src/types.rs:96-103`
- Modify: `crates/convertbar-core/src/queue_ops.rs:911` and `:924-1042`
- Modify: `src/lib/transport/types.ts:20-27`
- Modify: `src/lib/addSummary.ts:3-18`

**Interfaces:**
- Consumes: `settings_ops::read_cleanup_mode` (Task 1); `converter::is_in_place` (already `pub(crate)`).
- Produces: `SkipReason::InPlaceKeepBlocked` (serialized as `in_place_keep_blocked`), and `add_files_to_db`'s new `cleanup_mode: &str` parameter.

- [ ] **Step 1: Write the failing Rust test**

Add to the `add_files_to_db skip rules` test section in `crates/convertbar-core/src/queue_ops.rs` (near `:1542`):

```rust
#[test]
fn an_in_place_source_is_not_queued_under_keep() {
    let conn = test_conn();
    // An empty suffix makes the output path equal the source path: an in-place
    // re-encode. "Keep both files" has no meaning when there is only one file, so the
    // job must never be created — an error row here would be invisible to
    // fetch_skip_sets and filter_known_bad_sources, and a watched folder would
    // re-queue and re-fail it on every single boot.
    let source = "/movies/clip.mp4".to_string();

    let blocked = add_files_to_db(&conn, &[source.clone()], "preset", "", false, "keep").unwrap();
    assert!(blocked.added.is_empty(), "no in-place job may be queued under keep");
    assert_eq!(
        blocked.skipped,
        vec![SkipCount { reason: SkipReason::InPlaceKeepBlocked, count: 1 }]
    );

    // The same source under delete still queues: the block is scoped to keep, not to
    // in-place encoding in general.
    let queued = add_files_to_db(&conn, &[source], "preset", "", false, "delete").unwrap();
    assert_eq!(queued.added.len(), 1);
}

#[test]
fn a_distinct_output_still_queues_under_keep() {
    let conn = test_conn();
    // Non-empty suffix -> output != source -> keep is perfectly meaningful.
    let result =
        add_files_to_db(&conn, &["/movies/clip.mp4".to_string()], "preset", "-conv", false, "keep")
            .unwrap();
    assert_eq!(result.added.len(), 1);
    assert!(result.skipped.is_empty());
}

#[test]
fn a_kept_source_is_not_requeued_while_its_history_row_survives() {
    // THE load-bearing test for keep. Under trash/delete the source is gone after a
    // successful conversion, so a watched-folder rescan cannot re-ingest it. Under keep
    // the source is still sitting there, and the ONLY thing preventing an infinite
    // re-convert loop is the (size, mtime) fingerprint on the completed row. Nothing
    // else in the suite pins that.
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("clip.mp4");
    std::fs::write(&source, b"0123456789").unwrap();
    let source_str = source.to_string_lossy().into_owned();

    let conn = test_conn();
    let id = crate::probe_cache::file_identity(&source_str).expect("stat the source");
    conn.execute(
        "INSERT INTO jobs (id, source_path, output_path, preset, status, source_size, source_mtime, queue_order, created_at)
         VALUES ('j1', ?1, ?2, 'preset', 'done', ?3, ?4, 0, '2020-01-01T00:00:00Z')",
        params![
            source_str,
            format!("{source_str}-conv.mp4"),
            id.size,
            id.mtime
        ],
    )
    .unwrap();

    let result =
        add_files_to_db(&conn, &[source_str], "preset", "-conv", false, "keep").unwrap();

    assert!(result.added.is_empty(), "a kept source must not be re-queued");
    assert_eq!(
        result.skipped,
        vec![SkipCount { reason: SkipReason::AlreadyConverted, count: 1 }]
    );
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p convertbar-core -- an_in_place_source_is_not_queued a_distinct_output_still_queues a_kept_source_is_not_requeued`
Expected: FAIL — `add_files_to_db` takes 5 arguments, and `SkipReason::InPlaceKeepBlocked` does not exist. (The third test will pass once it compiles; it is a characterization test. Verify it can fail by temporarily blanking `source_mtime` in its INSERT — that drops the row into the legacy bucket, which `skip_already_converted = false` does not honor, and the source gets re-queued. Revert after confirming RED.)

- [ ] **Step 3: Add the enum variant**

In `crates/convertbar-core/src/types.rs`, extend `SkipReason`:

```rust
pub enum SkipReason {
    NotVideo,
    AlreadyQueued,
    AlreadyConverted,
    OutputExists,
    /// Source codec + resolution already meet/exceed the target preset (skip-by-source-media).
    AlreadyAtTarget,
    /// The output path equals the source (empty suffix) while `cleanup_mode` is `keep`.
    /// Keeping "both" files is meaningless when there is one file, so the job is never
    /// created — see `queue_ops::add_files_to_db`.
    InPlaceKeepBlocked,
}
```

- [ ] **Step 4: Thread `cleanup_mode` into `add_files_to_db`**

Add the parameter to the signature at `crates/convertbar-core/src/queue_ops.rs:924`:

```rust
fn add_files_to_db(
    conn: &rusqlite::Connection,
    paths: &[String],
    preset: &str,
    suffix: &str,
    skip_already_converted: bool,
    cleanup_mode: &str,
) -> Result<AddResult, String> {
```

Add a counter beside the existing four:

```rust
let (mut n_not_video, mut n_queued, mut n_converted, mut n_output_exists) =
    (0u32, 0u32, 0u32, 0u32);
let mut n_in_place_keep = 0u32;
```

**Then fix the exhaustive match at `queue_ops.rs:952-959`, or the crate will not compile.**
It matches every `SkipReason` variant with no wildcard arm, so adding one to the enum is an
E0004 that blocks the entire test binary — not just this module's tests. `cheap_skip_reason`
can never return the new variant (the block lives outside it, after the output path is
resolved), so the arm is unreachable-but-required:

```rust
match reason {
    SkipReason::NotVideo => n_not_video += 1,
    SkipReason::AlreadyQueued => n_queued += 1,
    SkipReason::AlreadyConverted => n_converted += 1,
    SkipReason::OutputExists => n_output_exists += 1,
    // The cheap checks never produce AlreadyAtTarget — that needs a probe.
    SkipReason::AlreadyAtTarget => {}
    // Nor InPlaceKeepBlocked: that decision needs the resolved output path, so it is
    // counted at its own site below rather than here.
    SkipReason::InPlaceKeepBlocked => {}
}
```

Insert the block immediately after `output_str` is computed and **before** `assigned.insert(output_str.clone())` (around `:982`), so a blocked path never claims an output name:

```rust
// In-place under keep is impossible by construction: output_path == source_path, so
// there is no "both" to keep. Blocked here rather than refused in process_queue —
// an error row is invisible to both re-ingestion guards, so a watched folder would
// re-queue it forever (see the design doc, Part 3).
if cleanup_mode == "keep" && crate::converter::is_in_place(path_str, &output_str) {
    n_in_place_keep += 1;
    continue;
}
assigned.insert(output_str.clone());
```

Add it to the summary loop at `:1031`:

```rust
for (reason, count) in [
    (SkipReason::NotVideo, n_not_video),
    (SkipReason::AlreadyQueued, n_queued),
    (SkipReason::AlreadyConverted, n_converted),
    (SkipReason::OutputExists, n_output_exists),
    (SkipReason::InPlaceKeepBlocked, n_in_place_keep),
] {
```

- [ ] **Step 5: Update the call site**

At `crates/convertbar-core/src/queue_ops.rs:911`, read the mode from the connection already in hand:

```rust
let conn = ctx.db.lock().map_err(|e| e.to_string())?;
let cleanup_mode = crate::settings_ops::read_cleanup_mode(&conn);
let mut result =
    add_files_to_db(&conn, &survivors, &preset, &suffix, skip_already_converted, &cleanup_mode)?;
```

Every other `add_files_to_db` call is in the test module; pass `"trash"` for those, which is the mode they already ran under.

- [ ] **Step 6: Run the core suite**

Run: `cargo test -p convertbar-core`
Expected: PASS

- [ ] **Step 7: Add the frontend member and label**

`src/lib/transport/types.ts` — extend the union:

```ts
export type SkipReason =
  | "not_video"
  | "already_queued"
  | "already_converted"
  | "output_exists"
  | "already_at_target"
  | "in_place_keep_blocked";
```

`src/lib/addSummary.ts` — both maps. Missing either renders `undefined` in the status line:

```ts
const SKIP_LABELS: Record<SkipReason, string> = {
  output_exists: "output exists",
  already_converted: "already converted",
  already_queued: "already queued",
  not_video: "not a video",
  already_at_target: "already at target",
  in_place_keep_blocked: "in-place encode needs Delete",
};

// Fixed order so the rendered summary is deterministic regardless of backend ordering.
const REASON_ORDER: SkipReason[] = [
  "in_place_keep_blocked",
  "output_exists",
  "already_converted",
  "already_at_target",
  "already_queued",
  "not_video",
];
```

`in_place_keep_blocked` leads the order deliberately: it is the only reason the user must change a setting to resolve.

- [ ] **Step 8: Write the frontend test**

Append to `src/lib/addSummary.test.ts`:

```ts
it("labels an in-place-blocked skip and sorts it first", () => {
  const summary = summarizeAdds([
    {
      added: [],
      skipped: [
        { reason: "not_video", count: 2 },
        { reason: "in_place_keep_blocked", count: 3 },
      ],
    },
  ]);
  expect(summary).toBe("3 skipped (in-place encode needs Delete) · 2 skipped (not a video)");
});
```

- [ ] **Step 9: Run the frontend tests and the type-check**

Run: `npm test -- addSummary && npm run build`
Expected: PASS, no type errors. A missing `SKIP_LABELS` entry is a compile error because the map is `Record<SkipReason, string>` — that is the point of typing it that way.

- [ ] **Step 10: Commit**

```bash
cargo fmt
git add crates/convertbar-core/src/types.rs crates/convertbar-core/src/queue_ops.rs \
        src/lib/transport/types.ts src/lib/addSummary.ts src/lib/addSummary.test.ts
git commit -m "feat(core): never queue an in-place job while cleanup_mode is keep"
```

---

### Task 4: Drop queued in-place jobs when switching to keep

**Files:**
- Modify: `crates/convertbar-core/src/queue_ops.rs` (new `drop_queued_in_place_jobs`)
- Modify: `crates/convertbar-core/src/settings_ops.rs:161-181` (`update_setting`)

**Interfaces:**
- Consumes: `converter::is_in_place`.
- Produces: `queue_ops::drop_queued_in_place_jobs(&rusqlite::Connection) -> usize` — the number of rows deleted.

- [ ] **Step 1: Write the failing test**

Add to `crates/convertbar-core/src/settings_ops.rs`'s tests module:

```rust
#[test]
fn switching_to_keep_drops_queued_in_place_jobs() {
    // Run on a worker thread with a bounded join, exactly like the sibling deadlock test
    // above (`update_setting_hands_the_connection_back_before_the_hooks_run`). This hook
    // re-locks ctx.db, so if the write scope's guard ever stops being dropped first, the
    // failure mode is a SELF-DEADLOCK — an unbounded hang that would freeze the whole
    // suite rather than fail. The timeout turns that into a legible failure.
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let (ctx, sink, _d) = test_ctx(test_conn());
        {
            let conn = ctx.db.lock().unwrap();
            // One in-place job (source == output) and one normal job.
            conn.execute(
                "INSERT INTO jobs (id, source_path, output_path, preset, status, queue_order, created_at)
                 VALUES ('inplace', '/m/a.mp4', '/m/a.mp4', 'p', 'queued', 0, '2020-01-01T00:00:00Z')",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO jobs (id, source_path, output_path, preset, status, queue_order, created_at)
                 VALUES ('normal', '/m/b.mp4', '/m/b-conv.mp4', 'p', 'queued', 1, '2020-01-01T00:00:00Z')",
                [],
            )
            .unwrap();
        }

        let result = update_setting(&ctx, "cleanup_mode", "keep");

        let ids: Vec<String> = ctx
            .db
            .lock()
            .unwrap()
            .prepare("SELECT id FROM jobs ORDER BY id")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        let emits = sink.payloads("queue-updated").len();
        let lock_free = ctx.db.try_lock().is_ok();
        let _ = tx.send((result, ids, emits, lock_free));
    });

    let (result, ids, emits, lock_free) = rx
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("update_setting deadlocked against its own post-write hook");
    result.unwrap();
    assert_eq!(ids, vec!["normal".to_string()], "only the in-place job is dropped");
    // The Queue panel must learn about the removal.
    assert_eq!(emits, 1);
    assert!(lock_free, "a hook running after the write must not strand the connection");
}

#[test]
fn switching_to_delete_leaves_queued_in_place_jobs_alone() {
    let (ctx, _sink, _d) = test_ctx(test_conn());
    {
        let conn = ctx.db.lock().unwrap();
        conn.execute(
            "INSERT INTO jobs (id, source_path, output_path, preset, status, queue_order, created_at)
             VALUES ('inplace', '/m/a.mp4', '/m/a.mp4', 'p', 'queued', 0, '2020-01-01T00:00:00Z')",
            [],
        )
        .unwrap();
    }

    update_setting(&ctx, "cleanup_mode", "delete").unwrap();

    let conn = ctx.db.lock().unwrap();
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM jobs", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1, "in-place jobs are only impossible under keep");
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p convertbar-core -- switching_to_keep switching_to_delete`
Expected: FAIL on the assertion — both jobs still present. (Note the test name changed from an earlier draft: it no longer claims to pin "without holding the lock". `TestSink` does not re-lock `ctx.db`, so it cannot observe an emit made under the guard; `LockProbeSink` in `control.rs` is the double built for that property, and the real protection here is the bounded join, which converts a self-deadlock from a hang into a failure.)

- [ ] **Step 3: Write `drop_queued_in_place_jobs`**

Add to `crates/convertbar-core/src/queue_ops.rs`:

```rust
/// Deletes every `queued` job whose output path is its own source (an in-place re-encode),
/// returning how many rows went. Called when `cleanup_mode` becomes `keep`, where such a
/// job is impossible: there is no second file to keep.
///
/// Filtered in Rust rather than SQL because in-place-ness is a *normalized* path
/// comparison (`converter::is_in_place` collapses `//` and `/./`), which a `WHERE
/// source_path = output_path` would miss.
pub fn drop_queued_in_place_jobs(conn: &rusqlite::Connection) -> usize {
    let rows: Vec<(String, String, String)> = match conn
        .prepare("SELECT id, source_path, output_path FROM jobs WHERE status = 'queued'")
    {
        Ok(mut stmt) => match stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))) {
            Ok(rows) => rows.flatten().collect(),
            Err(_) => return 0,
        },
        Err(_) => return 0,
    };

    let mut dropped = 0;
    for (id, source, output) in rows {
        if crate::converter::is_in_place(&source, &output) {
            if conn
                .execute("DELETE FROM jobs WHERE id = ?1", rusqlite::params![id])
                .is_ok()
            {
                dropped += 1;
            }
        }
    }
    dropped
}
```

- [ ] **Step 4: Hook it into `update_setting`**

First add the trait import at the top of `crates/convertbar-core/src/settings_ops.rs` — `emit_t` is on the `EventSinkExt` extension trait (`events.rs:13-23`), not on `EventSink` itself, and this file imports neither today. Without it the hook below is an E0599, "items from traits can only be used if the trait is in scope":

```rust
use rusqlite::params;

use crate::ctx::Ctx;
use crate::events::EventSinkExt;
use crate::types::Settings;
```

Then extend the existing post-write hook block. The write scope already drops its guard before the hooks run — keep it that way:

```rust
    } // conn must be dropped before the hooks below: they re-lock ctx.db on this same
      // thread, and std::sync::Mutex is not reentrant — holding the guard self-deadlocks.

    // Let the running watcher pick up a changed skip-marker name without a restart.
    if key == "watch_skip_marker" {
        crate::watcher::refresh_skip_marker(ctx);
    }

    // Switching to keep makes any queued in-place job impossible (its output IS its
    // source). Drop them here, at the moment the user makes the choice, so no such job
    // can reach the converter and no error row is ever written.
    if key == "cleanup_mode" && normalize_cleanup_mode(value) == "keep" {
        let dropped = {
            let conn = ctx.db.lock().map_err(|e| e.to_string())?;
            crate::queue_ops::drop_queued_in_place_jobs(&conn)
        }; // guard released before the emit below — see the comment above.
        if dropped > 0 {
            ctx.events.emit_t("queue-updated", ());
        }
    }

    Ok(())
}
```

- [ ] **Step 5: Run to verify it passes**

Run: `cargo test -p convertbar-core`
Expected: PASS, including the pre-existing `update_setting_hands_the_connection_back_before_the_hooks_run` deadlock test.

- [ ] **Step 6: Commit**

```bash
cargo fmt
git add crates/convertbar-core/src/queue_ops.rs crates/convertbar-core/src/settings_ops.rs
git commit -m "feat(core): switching to keep drops queued in-place jobs"
```

---

### Task 5: `keep` survives a server boot

**Files:**
- Modify: `crates/convertbar-server/src/startup.rs` (tests module only)

**Interfaces:**
- Consumes: nothing new. This task adds no production code — `FORCED_DELETE_KEYS` already rewrites only rows equal to `'trash'`.

- [ ] **Step 1: Write the test**

Add beside `normalize_leaves_delete_untouched` in `crates/convertbar-server/src/startup.rs`:

```rust
#[test]
fn normalize_leaves_keep_untouched() {
    // The server forces trash -> delete because the `trash` crate litters .Trash-<uid>
    // on NAS mounts. It must NOT touch 'keep', which is a deliberate user choice the
    // web UI now offers. No production code enforces this — only the fact that the
    // rewrite is scoped to the exact string 'trash'. This test is what keeps it scoped.
    let conn = test_conn();
    conn.execute(
        "UPDATE settings SET value = 'keep' WHERE key = 'cleanup_mode'",
        [],
    )
    .unwrap();
    let ctx = test_ctx(conn);

    normalize_server_settings(&ctx);

    let conn = ctx.db.lock().unwrap();
    assert_eq!(setting(&conn, "cleanup_mode").as_deref(), Some("keep"));
}
```

- [ ] **Step 2: Run it**

Run: `cargo test -p convertbar-server normalize_leaves_keep_untouched`
Expected: PASS immediately — it is a characterization test, pinning behavior that already holds.

- [ ] **Step 3: Verify it can fail**

Temporarily rewrite the condition at `startup.rs:30` to also catch `keep`, re-run, confirm RED, then revert. A test that cannot fail is not protecting anything.

The line is an `==` comparison, not a match, so an or-pattern does **not** compile there (`Some("trash") | Some("keep")` parses as `BitOr` on `Option<&str>`) — and this repo has been bitten before by a non-compiling mutation reading as "survived". Use:

```rust
if matches!(value.as_deref(), Some("trash") | Some("keep")) {
```

Run: `cargo test -p convertbar-server normalize_leaves_keep_untouched`
Expected: FAIL while mutated, PASS after reverting.

- [ ] **Step 4: Commit**

```bash
cargo fmt
git add crates/convertbar-server/src/startup.rs
git commit -m "test(server): pin that boot normalization leaves keep alone"
```

---

### Task 6: Settings UI, copy, and docs

**Files:**
- Modify: `src/pages/SettingsPage.tsx:231-260` (the "After conversion" group) and `:221-226` (the empty-suffix note)
- Modify: `src/pages/SettingsPage.test.tsx`
- Modify: `README.md`, `CLAUDE.md`, `unraid-template.xml:27-31`

**Interfaces:**
- Consumes: `settings.cleanup_mode` (already `string` in `AppSettings`), `updateSetting` from `useSettings`.

- [ ] **Step 1: Write the failing tests**

`src/pages/SettingsPage.test.tsx` mocks `@tauri-apps/api/core`'s `invoke` and builds settings from a local `makeSettings()` (`:17-38`), which currently takes no arguments. Give it an override parameter first — the two new tests need to vary `cleanup_mode`:

```ts
function makeSettings(overrides: Partial<AppSettings> = {}): AppSettings {
  return {
    // ...every existing field unchanged...
    update_mode: "automatic",
    ...overrides,
  };
}
```

Then add the tests. The desktop case renders directly; the server case reuses the file's existing `stubEnv` + `resetModules` + `fetch` pattern (`:90-129`), because `isServerHead` is a module-level const that must be re-evaluated on a fresh module graph:

```ts
it("offers three cleanup modes on desktop", async () => {
  render(<SettingsPage />);

  expect(await screen.findByLabelText("Move original to Trash")).toBeInTheDocument();
  expect(screen.getByLabelText("Delete original permanently")).toBeInTheDocument();
  expect(screen.getByLabelText("Keep both files")).toBeInTheDocument();
});

it("writes cleanup_mode=keep when Keep is chosen", async () => {
  render(<SettingsPage />);

  fireEvent.click(await screen.findByLabelText("Keep both files"));

  await waitFor(() =>
    expect(invokeMock).toHaveBeenCalledWith("update_setting", {
      key: "cleanup_mode",
      value: "keep",
    }),
  );
});

it("hides the Trash option on the server head", async () => {
  // A headless deployment has no Trash, and the `trash` crate litters .Trash-<uid>
  // directories on the NAS mounts these servers run against.
  vi.stubEnv("VITE_HEAD", "server");
  const fetchMock = vi.fn((path: string) => {
    if (path === "/api/settings") {
      return Promise.resolve({ ok: true, status: 200, json: () => Promise.resolve(makeSettings()) });
    }
    if (path === "/api/handbrake/presets") {
      return Promise.resolve({ ok: true, status: 200, json: () => Promise.resolve(["Fast 1080p30"]) });
    }
    if (path === "/api/info") {
      return Promise.resolve({
        ok: true,
        status: 200,
        json: () =>
          Promise.resolve({
            version: "1.2.3",
            head: "server",
            can_pause_process: true,
            auth_required: false,
            browse_roots: [],
          }),
      });
    }
    if (path.includes("/suffix/generate")) {
      return Promise.resolve({ ok: true, status: 200, json: () => Promise.resolve(META) });
    }
    if (path.includes("/suffix")) {
      return Promise.resolve({ ok: true, status: 200, json: () => Promise.resolve("-conv") });
    }
    return Promise.resolve({ ok: false, status: 404, json: () => Promise.resolve({ error: "not mocked" }) });
  });
  vi.stubGlobal("fetch", fetchMock);
  vi.resetModules();

  const { default: FreshSettingsPage } = await import("./SettingsPage");
  render(<FreshSettingsPage />);

  expect(await screen.findByLabelText("Delete original permanently")).toBeInTheDocument();
  expect(screen.getByLabelText("Keep both files")).toBeInTheDocument();
  expect(screen.queryByLabelText("Move original to Trash")).not.toBeInTheDocument();
});

it("warns only when keep is selected AND the resolved suffix is empty", async () => {
  const warning = /cannot keep the original/i;

  // An empty resolved suffix comes from resolve_suffix_template returning "".
  const withMode = (cleanup_mode: string, suffix: string) => {
    invokeMock.mockImplementation(((cmd: string) => {
      switch (cmd) {
        case "get_settings":
          return Promise.resolve(makeSettings({ cleanup_mode }));
        case "list_handbrake_presets":
          return Promise.resolve(["Fast 1080p30"]);
        case "get_preset_suffix":
          return Promise.resolve(suffix);
        case "generate_preset_suffix":
          return Promise.resolve(META);
        case "resolve_suffix_template":
          return Promise.resolve(suffix);
        default:
          return Promise.resolve(null);
      }
    }) as typeof invoke);
  };

  withMode("keep", "-conv");
  const a = render(<SettingsPage />);
  expect(await screen.findByLabelText("Keep both files")).toBeInTheDocument();
  expect(screen.queryByText(warning)).not.toBeInTheDocument();
  a.unmount();

  withMode("delete", "");
  const b = render(<SettingsPage />);
  expect(await screen.findByLabelText("Keep both files")).toBeInTheDocument();
  expect(screen.queryByText(warning)).not.toBeInTheDocument();
  b.unmount();

  // Only the combination warns — the setting alone or the empty suffix alone must not.
  withMode("keep", "");
  render(<SettingsPage />);
  expect(await screen.findByText(warning)).toBeInTheDocument();
});
```

The suffix preview is debounced by 250ms (`SUFFIX_PREVIEW_DEBOUNCE_MS`), so if the last assertion is flaky, wrap the render in `vi.useFakeTimers()` / `act(() => vi.advanceTimersByTime(300))` as the file's existing suffix tests do.

- [ ] **Step 2: Run to verify failure**

Run: `npm test -- SettingsPage`
Expected: FAIL — "Keep both files" is not in the document.

- [ ] **Step 3: Replace the "After conversion" group**

```tsx
<div className="setting-group">
  <label className="setting-label">After conversion</label>
  <div className="setting-radios">
    {/* No Trash on the server head: the `trash` crate litters .Trash-<uid>
        directories on the NAS mounts a headless deployment runs against. */}
    {!isServerHead && (
      <label className="radio-label">
        <input
          type="radio"
          name="cleanup"
          checked={settings.cleanup_mode === "trash"}
          onChange={() => updateSetting("cleanup_mode", "trash")}
        />
        Move original to Trash
      </label>
    )}
    <label className="radio-label">
      <input
        type="radio"
        name="cleanup"
        checked={settings.cleanup_mode === "delete"}
        onChange={() => updateSetting("cleanup_mode", "delete")}
      />
      Delete original permanently
    </label>
    <label className="radio-label">
      <input
        type="radio"
        name="cleanup"
        checked={settings.cleanup_mode === "keep"}
        onChange={() => updateSetting("cleanup_mode", "keep")}
      />
      Keep both files
    </label>
  </div>
  <p className="setting-hint">
    Keep both files deletes nothing. Use it to check the encodes are good on this
    machine, remove the originals yourself, then switch to Delete once you trust the
    results.
  </p>
</div>
```

- [ ] **Step 4: Extend the empty-suffix note**

Replace the block at `SettingsPage.tsx:221-226`:

```tsx
{resolvedSuffix.trim() === "" && (
  <div className="suffix-inplace-note">
    Empty suffix: mp4 files are re-encoded in place, replacing the original. The fast
    &quot;already converted&quot; skip-by-suffix is also disabled.
    {settings.cleanup_mode === "keep" && (
      <>
        {" "}
        <strong>
          An in-place re-encode cannot keep the original — there is only one file. These
          files will be skipped until you choose Delete or set a suffix.
        </strong>
      </>
    )}
  </div>
)}
```

- [ ] **Step 5: Run the tests and type-check**

Run: `npm test -- SettingsPage && npm run build`
Expected: PASS

- [ ] **Step 6: Update the docs**

`README.md` — extend the "How a conversion works" section (`:95`), which is where the current Trash behaviour is described. Document the three modes and the two caveats the design accepted:

```markdown
**After conversion** — what happens to the file that loses on size:

- **Move original to Trash** (desktop only) — recoverable from the OS Trash.
- **Delete original permanently** — the default on the server head; a headless
  deployment has no Trash, and the `trash` crate litters `.Trash-<uid>` folders on
  NAS mounts.
- **Keep both files** — nothing is deleted. This is an evaluation mode: run a batch,
  check the encodes are good on your hardware, delete the originals yourself, then
  switch to Delete. History still shows how much each encode saved, so you can judge
  the result before committing to it.

Two things to know about Keep:

- An empty output suffix re-encodes in place, so there is no second file to keep.
  Those files are skipped with a note until you choose Delete or set a suffix.
- While originals are kept, ConvertBar avoids re-converting them by remembering each
  file's size and modification time in History. Clearing History forgets that, and a
  watched folder will convert those files again into renumbered outputs
  (`movie (1).1080p-h265.mp4`).
```

`unraid-template.xml:27-31` — the current text says the server build "always deletes replaced files", which Part 3 makes false. Change that sentence to:

```
    This server build deletes replaced files by default rather than moving them to a
    trash folder; the Settings page can switch that to keeping both files while you
    evaluate the results. Its Settings page hides the desktop-only options (menu bar,
    notifications, auto-update, launch at login).
```

`CLAUDE.md` — add a section after "Emitting Events Under the DB Lock":

```markdown
## Cleanup Modes and the In-Place Rule

`cleanup_mode` is `trash | delete | keep`, always read through
`settings_ops::read_cleanup_mode` (never a raw column compare); an unrecognized value
normalizes to `trash`.

`keep` and an in-place job (empty suffix, so `output_path == source_path`) are mutually
exclusive, and that is enforced by PREVENTION, not refusal: `add_files_to_db` never
queues such a job, and `update_setting` drops queued ones when the mode becomes `keep`.
Do not "simplify" this into an error recorded in `process_queue` — an `error` row is
invisible to both `queue_ops::fetch_skip_sets` and `watcher::filter_known_bad_sources`,
so a watched folder would re-queue and re-fail every file on every boot. The
`"keep" => RemoveTemp` arm in `in_place_action` covers the setting-change race and must
stay a real arm, not a `debug_assert!` — the branch it replaces permanently deletes the
user's source on the server head.

Under `keep` the source survives, so re-ingestion protection rests entirely on the
`(size, mtime)` fingerprint in completed rows. Clearing history therefore re-converts
kept sources.
```

- [ ] **Step 7: Full suite and commit**

```bash
cargo test --workspace && npm test && npm run build
cargo fmt
git add src/pages/SettingsPage.tsx src/pages/SettingsPage.test.tsx README.md CLAUDE.md unraid-template.xml
git commit -m "feat: offer the keep cleanup mode in Settings"
```

- [ ] **Step 8: Open PR 1**

Ask the user to run `! git push -u origin feature/web-ui-columns-picker-cleanup`, then:

```bash
gh pr create --base main --title "feat: a keep cleanup mode that leaves originals on disk" \
  --body "Implements Part 3 of docs/superpowers/specs/2026-07-29-web-ui-columns-picker-cleanup-design.md"
```

Wait for `frontend` and `rust (ubuntu-22.04)` to be green, then `gh pr merge <n> --admin --squash`.

---

# PR 2 — File-picker overhaul

Branch from an updated `main` after PR 1 merges: `git checkout main && git pull --ff-only && git checkout -b feature/file-picker-selection`.

---

### Task 7: `rangeBetween`

**Files:**
- Create: `src/lib/pathSelection.ts`
- Create: `src/lib/pathSelection.test.ts`

**Interfaces:**
- Produces: `rangeBetween(entries: { path: string }[], anchorPath: string, targetPath: string): string[]`. Task 9 calls it.

- [ ] **Step 1: Write the failing test**

```ts
import { describe, it, expect } from "vitest";
import { rangeBetween } from "./pathSelection";

const listing = [
  { path: "/m/2024" },
  { path: "/m/archive" },
  { path: "/m/a.mkv" },
  { path: "/m/b.mp4" },
  { path: "/m/c.mp4" },
];

describe("rangeBetween", () => {
  it("returns the inclusive slice between two paths", () => {
    expect(rangeBetween(listing, "/m/archive", "/m/b.mp4")).toEqual([
      "/m/archive",
      "/m/a.mkv",
      "/m/b.mp4",
    ]);
  });

  it("is order-agnostic — a shift-click above the anchor works the same", () => {
    expect(rangeBetween(listing, "/m/b.mp4", "/m/archive")).toEqual([
      "/m/archive",
      "/m/a.mkv",
      "/m/b.mp4",
    ]);
  });

  it("spans folders and files alike", () => {
    // The whole reason the row model puts a checkbox on every row: a range must not
    // stop at the folder/file boundary.
    expect(rangeBetween(listing, "/m/2024", "/m/a.mkv")).toEqual([
      "/m/2024",
      "/m/archive",
      "/m/a.mkv",
    ]);
  });

  it("returns just the row when anchor and target are the same", () => {
    expect(rangeBetween(listing, "/m/b.mp4", "/m/b.mp4")).toEqual(["/m/b.mp4"]);
  });

  it("returns nothing when either end is not in the listing", () => {
    // The anchor is cleared on navigation, so a stale anchor from a previous
    // directory must degrade to "no range", never to a wrong range.
    expect(rangeBetween(listing, "/other/x.mp4", "/m/b.mp4")).toEqual([]);
    expect(rangeBetween(listing, "/m/b.mp4", "/other/x.mp4")).toEqual([]);
  });
});
```

- [ ] **Step 2: Run to verify it fails**

Run: `npm test -- pathSelection`
Expected: FAIL — cannot resolve `./pathSelection`

- [ ] **Step 3: Implement**

```ts
/**
 * The inclusive slice of `entries` between two paths, in listing order. Order-agnostic:
 * the anchor may sit above or below the target. Returns `[]` when either endpoint is
 * absent, so a stale anchor left over from another directory degrades to "no range"
 * rather than to a wrong one.
 *
 * Compares paths as opaque strings, so it is separator-agnostic and holds if a server
 * head ever runs on Windows.
 */
export function rangeBetween(
  entries: { path: string }[],
  anchorPath: string,
  targetPath: string,
): string[] {
  const anchor = entries.findIndex((e) => e.path === anchorPath);
  const target = entries.findIndex((e) => e.path === targetPath);
  if (anchor === -1 || target === -1) return [];
  const [from, to] = anchor <= target ? [anchor, target] : [target, anchor];
  return entries.slice(from, to + 1).map((e) => e.path);
}
```

- [ ] **Step 4: Run to verify it passes**

Run: `npm test -- pathSelection && npm run build`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/lib/pathSelection.ts src/lib/pathSelection.test.ts
git commit -m "feat: a pure range-between helper for picker shift-selection"
```

---

### Task 8: Checkbox rows and select-all

**Files:**
- Modify: `src/components/FileBrowserModal.tsx:83-98` and `:142-167`
- Modify: `src/components/FileBrowserModal.test.tsx:48-69`
- Modify: `src/App.css:1249-1281`

**Interfaces:**
- Produces: the `mode: "files"` row contract — every row toggles selection on click; folder rows carry a separate navigate button with accessible name `Open <name>`. Tasks 9 and 10 build on it.

- [ ] **Step 1: Update the four existing tests this task breaks**

Changing the row model breaks **four** files-mode tests, not one. Fix them all in this step, before writing anything new — otherwise Step 5's suite run is red for reasons that have nothing to do with the new code:

| Test | Line | Why it breaks | Fix |
|---|---|---|---|
| "navigates into a directory on click" | `:48` | clicks the folder row to navigate | retarget to the Open button (rewritten below) |
| "multi-selects files and calls onSelect…" | `:71` | asserts `/add 2 files/i` | label is now `Add 2 items` |
| "navigates back up via the breadcrumb" | `:107` | clicks the folder row as a *setup* step to get deeper | retarget that one click to the Open button; the breadcrumb assertions are unchanged |
| "disables the Add-files confirm button…" | `:145` | asserts `/add 0 files/i` | label is now `Add 0 items` |
| "stops breadcrumb up-navigation at the containing root…" | `:170` | same setup-step click | same fix as `:107` |

`:86` ("directory mode confirms the current directory") is **not** affected — directory mode keeps row-click navigation.

⚠️ `:107` and `:170` are the dangerous ones. They only use the folder click to *get somewhere*, so the tempting fix is to make row text navigate again — which silently reverts this entire task. Change the setup click, never the component.

The rewritten navigation test:

```ts
it("navigates into a directory via its open button, not by clicking the row", async () => {
  fsListMock.mockImplementation((path: string) => {
    if (path === "/") {
      return Promise.resolve({
        entries: [entry({ name: "Movies", path: "/Movies", is_dir: true })],
      });
    }
    if (path === "/Movies") {
      return Promise.resolve({
        entries: [entry({ name: "clip.mp4", path: "/Movies/clip.mp4" })],
      });
    }
    return Promise.reject(new Error(`unexpected path: ${path}`));
  });

  render(<FileBrowserModal mode="files" onSelect={vi.fn()} onClose={vi.fn()} />);

  // Clicking the row selects the folder rather than entering it — that uniformity is
  // what lets a shift-range span folders and files.
  fireEvent.click(await screen.findByText("Movies"));
  expect(fsListMock).toHaveBeenCalledTimes(1);

  fireEvent.click(screen.getByRole("button", { name: "Open Movies" }));
  await waitFor(() => expect(fsListMock).toHaveBeenCalledWith("/Movies"));
  expect(await screen.findByText("clip.mp4")).toBeInTheDocument();
});

it("selects every row in the listing from the select-all control", async () => {
  fsListMock.mockResolvedValue({
    entries: [
      entry({ name: "2024", path: "/2024", is_dir: true }),
      entry({ name: "a.mp4", path: "/a.mp4" }),
    ],
  });
  const onSelect = vi.fn();

  render(<FileBrowserModal mode="files" onSelect={onSelect} onClose={vi.fn()} />);

  fireEvent.click(await screen.findByLabelText("Select all"));
  fireEvent.click(screen.getByRole("button", { name: /^Add 2 items/ }));

  expect(onSelect).toHaveBeenCalledWith(["/2024", "/a.mp4"]);
});

it("shows the select-all box as indeterminate when only some rows are selected", async () => {
  fsListMock.mockResolvedValue({
    entries: [
      entry({ name: "a.mp4", path: "/a.mp4" }),
      entry({ name: "b.mp4", path: "/b.mp4" }),
    ],
  });

  render(<FileBrowserModal mode="files" onSelect={vi.fn()} onClose={vi.fn()} />);

  const selectAll = (await screen.findByLabelText("Select all")) as HTMLInputElement;
  expect(selectAll.checked).toBe(false);
  expect(selectAll.indeterminate).toBe(false);

  fireEvent.click(screen.getByText("a.mp4"));
  // Partial selection reads as indeterminate, not unchecked — otherwise the header
  // claims nothing is selected while a row is ticked right below it.
  expect(selectAll.indeterminate).toBe(true);

  fireEvent.click(screen.getByText("b.mp4"));
  expect(selectAll.checked).toBe(true);
  expect(selectAll.indeterminate).toBe(false);
});

it("keeps directory mode free of checkboxes", async () => {
  fsListMock.mockResolvedValue({
    entries: [entry({ name: "Movies", path: "/Movies", is_dir: true })],
  });

  render(<FileBrowserModal mode="directory" onSelect={vi.fn()} onClose={vi.fn()} />);

  expect(await screen.findByText("Movies")).toBeInTheDocument();
  expect(screen.queryByLabelText("Select all")).not.toBeInTheDocument();
  // Directory mode still navigates on a row click, as it always has.
  fireEvent.click(screen.getByText("Movies"));
  await waitFor(() => expect(fsListMock).toHaveBeenCalledWith("/Movies"));
});
```

- [ ] **Step 2: Run to verify they fail**

Run: `npm test -- FileBrowserModal`
Expected: FAIL — no "Open Movies" button, no "Select all" control. The four retargeted tests above also fail at this point, which is correct: they are asserting the new behavior before it exists.

- [ ] **Step 3: Replace the click handler and the row renderer**

In `FileBrowserModal.tsx`, replace `handleEntryClick`:

```tsx
const selectable = mode === "files";

const handleRowClick = (entry: FsEntry) => {
  // Directory mode keeps its original behavior: a folder click navigates, a file is inert.
  if (!selectable) {
    if (entry.is_dir) load(entry.path);
    return;
  }
  toggleSelect(entry.path);
};

const allSelected = entries.length > 0 && entries.every((e) => selected.has(e.path));
const someSelected = entries.some((e) => selected.has(e.path));

const toggleSelectAll = () => {
  setSelected((prev) => {
    const next = new Set(prev);
    for (const e of entries) {
      if (allSelected) next.delete(e.path);
      else next.add(e.path);
    }
    return next;
  });
};
```

Add the select-all header directly above `.file-browser-list`, rendered only in files mode:

```tsx
{selectable && !loading && entries.length > 0 && (
  <label className="file-browser-select-all">
    <input
      type="checkbox"
      aria-label="Select all"
      checked={allSelected}
      ref={(el) => {
        if (el) el.indeterminate = !allSelected && someSelected;
      }}
      onChange={toggleSelectAll}
    />
    <span>Select all ({entries.length})</span>
  </label>
)}
```

Replace the row renderer. The row is a `<div>`, not a `<button>`, because it now contains a nested button — a button inside a button is invalid HTML and React will warn:

```tsx
entries.map((entry) => {
  const isSelected = selected.has(entry.path);
  return (
    <div
      key={entry.path}
      className={`file-browser-entry${isSelected ? " file-browser-entry-selected" : ""}${
        !selectable && !entry.is_dir ? " file-browser-entry-disabled" : ""
      }`}
      onClick={() => handleRowClick(entry)}
    >
      {selectable && (
        {/* readOnly, not onChange: the row's click handler owns the state transition
            (it needs the shiftKey modifier, which a change event does not carry). Without
            readOnly React warns on every row about a checked input with no onChange. */}
        <input
          type="checkbox"
          className="file-browser-entry-check"
          checked={isSelected}
          readOnly
          aria-label={entry.name}
          onClick={(e) => e.stopPropagation()}
        />
      )}
      <span className="file-browser-entry-icon">{entry.is_dir ? "📁" : "📄"}</span>
      <span className="file-browser-entry-name">{entry.name}</span>
      {selectable && entry.is_dir && (
        <button
          type="button"
          className="file-browser-entry-open"
          aria-label={`Open ${entry.name}`}
          onClick={(e) => {
            e.stopPropagation();
            load(entry.path);
          }}
        >
          →
        </button>
      )}
    </div>
  );
});
```

Update the confirm label — the selection is no longer files-only:

```tsx
const confirmLabel =
  mode === "directory"
    ? "Choose this folder"
    : `Add ${selected.size} item${selected.size === 1 ? "" : "s"}`;
```

- [ ] **Step 4: Add the CSS**

Append to `src/App.css` beside the existing `.file-browser-entry` rules:

```css
.file-browser-select-all {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 6px 12px;
  border-bottom: 1px solid var(--border);
  font-size: 12px;
  color: var(--text-secondary);
  cursor: pointer;
}

.file-browser-entry-disabled {
  color: var(--text-dim);
  cursor: default;
}

.file-browser-entry-check {
  flex-shrink: 0;
}

/* Full-height hit area: the row itself now selects, so this is the only way into a
   folder and must not be a small target. */
.file-browser-entry-open {
  margin-left: auto;
  padding: 4px 10px;
  background: none;
  border: none;
  color: var(--accent);
  font-size: 14px;
  cursor: pointer;
  align-self: stretch;
}

.file-browser-entry-open:hover {
  background: var(--bg-tertiary);
}
```

- [ ] **Step 5: Run to verify they pass**

Run: `npm test -- FileBrowserModal && npm run build`
Expected: PASS

- [ ] **Step 6: Commit**

```bash
git add src/components/FileBrowserModal.tsx src/components/FileBrowserModal.test.tsx src/App.css
git commit -m "feat: selectable folder rows and a select-all in the file picker"
```

---

### Task 9: Cross-directory selection and shift-ranges

**Files:**
- Modify: `src/components/FileBrowserModal.tsx` (the `load` callback, the row click, the footer)
- Modify: `src/components/FileBrowserModal.test.tsx`

**Interfaces:**
- Consumes: `rangeBetween` from Task 7; the row contract from Task 8.

- [ ] **Step 1: Write the failing tests**

```ts
it("keeps the selection when navigating between directories", async () => {
  fsListMock.mockImplementation((path: string) => {
    if (path === "/") {
      return Promise.resolve({
        entries: [
          entry({ name: "Movies", path: "/Movies", is_dir: true }),
          entry({ name: "root.mp4", path: "/root.mp4" }),
        ],
      });
    }
    return Promise.resolve({
      entries: [entry({ name: "inner.mp4", path: "/Movies/inner.mp4" })],
    });
  });
  const onSelect = vi.fn();

  render(<FileBrowserModal mode="files" onSelect={onSelect} onClose={vi.fn()} />);

  fireEvent.click(await screen.findByText("root.mp4"));
  fireEvent.click(screen.getByRole("button", { name: "Open Movies" }));
  fireEvent.click(await screen.findByText("inner.mp4"));

  // The whole point of persistence: gather from more than one folder in one pass.
  expect(screen.getByText("2 selected")).toBeInTheDocument();
  fireEvent.click(screen.getByRole("button", { name: /^Add 2 items/ }));
  expect(onSelect).toHaveBeenCalledWith(["/root.mp4", "/Movies/inner.mp4"]);
});

it("shift-clicking selects the range and never deselects", async () => {
  fsListMock.mockResolvedValue({
    entries: [
      entry({ name: "2024", path: "/2024", is_dir: true }),
      entry({ name: "a.mp4", path: "/a.mp4" }),
      entry({ name: "b.mp4", path: "/b.mp4" }),
      entry({ name: "c.mp4", path: "/c.mp4" }),
    ],
  });
  const onSelect = vi.fn();

  render(<FileBrowserModal mode="files" onSelect={onSelect} onClose={vi.fn()} />);

  fireEvent.click(await screen.findByText("c.mp4"));      // an unrelated earlier pick
  fireEvent.click(screen.getByText("2024"));               // anchor, and a folder
  fireEvent.click(screen.getByText("b.mp4"), { shiftKey: true });

  // Additive: c.mp4 survives even though it is outside the range. A mis-aimed
  // shift-click must never silently drop earlier work.
  fireEvent.click(screen.getByRole("button", { name: /^Add 4 items/ }));
  expect(onSelect).toHaveBeenCalledWith(["/c.mp4", "/2024", "/a.mp4", "/b.mp4"]);
});

it("clears the selection from the footer", async () => {
  fsListMock.mockResolvedValue({ entries: [entry({ name: "a.mp4", path: "/a.mp4" })] });

  render(<FileBrowserModal mode="files" onSelect={vi.fn()} onClose={vi.fn()} />);

  fireEvent.click(await screen.findByText("a.mp4"));
  expect(screen.getByText("1 selected")).toBeInTheDocument();
  fireEvent.click(screen.getByRole("button", { name: "Clear" }));
  expect(screen.queryByText("1 selected")).not.toBeInTheDocument();
});

it("passes a folder and a file inside it through untouched", async () => {
  // Deliberately NOT deduped here. The backend already does it: useFileIntake enqueues
  // files before folders, and add_files_to_db skips anything already queued. Deduping
  // in the picker would silently lose the ticked file if the user skips the folder's
  // >5-file confirm prompt.
  fsListMock.mockResolvedValue({
    entries: [
      entry({ name: "Movies", path: "/Movies", is_dir: true }),
      entry({ name: "clip.mp4", path: "/Movies/clip.mp4" }),
    ],
  });
  const onSelect = vi.fn();

  render(<FileBrowserModal mode="files" onSelect={onSelect} onClose={vi.fn()} />);

  fireEvent.click(await screen.findByText("Movies"));
  fireEvent.click(screen.getByText("clip.mp4"));
  fireEvent.click(screen.getByRole("button", { name: /^Add 2 items/ }));

  expect(onSelect).toHaveBeenCalledWith(["/Movies", "/Movies/clip.mp4"]);
});
```

- [ ] **Step 2: Run to verify they fail**

Run: `npm test -- FileBrowserModal`
Expected: FAIL — navigation clears the selection; there is no "2 selected" text.

- [ ] **Step 3: Stop clearing the selection on navigation**

In the `load` callback, delete `setSelected(new Set())` and reset the shift anchor instead — ranges are per-listing:

```tsx
const [anchor, setAnchor] = useState<string | null>(null);

const load = useCallback(async (target: string) => {
  setLoading(true);
  setError(null);
  try {
    const result = await httpCommands.fsList(target);
    setEntries(result.entries);
    setPath(target);
    // The selection deliberately survives navigation — gathering from several folders
    // in one pass is the point. The shift anchor does NOT: a range only means
    // something within one listing.
    setAnchor(null);
  } catch (e) {
    setError(errorText(e));
  } finally {
    setLoading(false);
  }
}, []);
```

- [ ] **Step 4: Wire shift-ranges into the row click**

```tsx
const handleRowClick = (entry: FsEntry, shiftKey: boolean) => {
  if (!selectable) {
    if (entry.is_dir) load(entry.path);
    return;
  }
  if (shiftKey && anchor) {
    const range = rangeBetween(entries, anchor, entry.path);
    if (range.length > 0) {
      // Additive by design: a range never deselects, so a mis-aimed shift-click
      // cannot silently drop work the user did earlier.
      setSelected((prev) => new Set([...prev, ...range]));
      return;
    }
  }
  setAnchor(entry.path);
  toggleSelect(entry.path);
};
```

Pass the modifier through from the row: `onClick={(e) => handleRowClick(entry, e.shiftKey)}`. The checkbox (already `readOnly` from Task 8) routes to the same handler instead of merely swallowing the click: `onClick={(e) => { e.stopPropagation(); handleRowClick(entry, e.shiftKey); }}`. One click path, one state transition — clicking the box and clicking the row must not diverge.

Import it: `import { rangeBetween } from "../lib/pathSelection";`

- [ ] **Step 5: Add the footer count and Clear**

Replace the footer's left side:

```tsx
<div className="file-browser-footer">
  {mode === "files" && selected.size > 0 && (
    <span className="file-browser-count">
      {selected.size} selected
      <button type="button" className="btn btn-small btn-dim" onClick={() => setSelected(new Set())}>
        Clear
      </button>
    </span>
  )}
  <button type="button" className="btn btn-small" onClick={onClose}>
    Cancel
  </button>
  <button type="button" className="btn btn-small" disabled={confirmDisabled} onClick={handleConfirm}>
    {confirmLabel}
  </button>
</div>
```

Add to `src/App.css`:

```css
.file-browser-count {
  display: flex;
  align-items: center;
  gap: 8px;
  margin-right: auto;
  font-size: 12px;
  color: var(--text-secondary);
}
```

- [ ] **Step 6: Run to verify they pass**

Run: `npm test -- FileBrowserModal && npm run build`
Expected: PASS

- [ ] **Step 7: Commit**

```bash
git add src/components/FileBrowserModal.tsx src/components/FileBrowserModal.test.tsx src/App.css
git commit -m "feat: persistent cross-directory selection and shift-ranges in the picker"
```

---

### Task 10: Jump-to-path and an inert backdrop

**Files:**
- Modify: `src/components/FileBrowserModal.tsx:117-138`
- Modify: `src/components/FileBrowserModal.test.tsx`
- Modify: `src/App.css`

- [ ] **Step 1: Write the failing tests**

```ts
it("navigates to a typed path", async () => {
  fsListMock.mockImplementation((path: string) =>
    path === "/media/movies"
      ? Promise.resolve({ entries: [entry({ name: "deep.mp4", path: "/media/movies/deep.mp4" })] })
      : Promise.resolve({ entries: [] }),
  );

  render(<FileBrowserModal mode="files" onSelect={vi.fn()} onClose={vi.fn()} />);

  const input = await screen.findByLabelText("Go to path");
  fireEvent.change(input, { target: { value: "/media/movies" } });
  fireEvent.submit(input.closest("form")!);

  expect(await screen.findByText("deep.mp4")).toBeInTheDocument();
});

it("shows the server's error for a forbidden path and keeps the current listing", async () => {
  fsListMock.mockImplementation((path: string) =>
    path === "/"
      ? Promise.resolve({ entries: [entry({ name: "visible.mp4", path: "/visible.mp4" })] })
      : Promise.reject(new Error("path outside allowed roots")),
  );

  render(<FileBrowserModal mode="files" onSelect={vi.fn()} onClose={vi.fn()} />);
  expect(await screen.findByText("visible.mp4")).toBeInTheDocument();

  const input = screen.getByLabelText("Go to path");
  fireEvent.change(input, { target: { value: "/etc" } });
  fireEvent.submit(input.closest("form")!);

  expect(await screen.findByText("path outside allowed roots")).toBeInTheDocument();
  // A rejected jump must not blank the listing the user was looking at.
  expect(screen.getByText("visible.mp4")).toBeInTheDocument();
});

it("does not close when the backdrop is clicked", async () => {
  fsListMock.mockResolvedValue({ entries: [] });
  const onClose = vi.fn();

  const { container } = render(
    <FileBrowserModal mode="files" onSelect={vi.fn()} onClose={onClose} />,
  );
  await waitFor(() => expect(fsListMock).toHaveBeenCalled());

  fireEvent.click(container.querySelector(".modal-overlay")!);

  // A stray backdrop click used to discard the whole selection. The accident class is
  // removed outright rather than guarded with a confirm dialog.
  expect(onClose).not.toHaveBeenCalled();
  fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
  expect(onClose).toHaveBeenCalled();
});
```

- [ ] **Step 2: Run to verify they fail**

Run: `npm test -- FileBrowserModal`
Expected: FAIL — no "Go to path" input; the backdrop click calls `onClose`.

- [ ] **Step 3: Make the backdrop inert**

The inner `stopPropagation` exists only to protect against the overlay handler, so it goes too:

```tsx
<div className="modal-overlay">
  <div className="file-browser-modal">
```

- [ ] **Step 4: Add the jump-to-path form**

Below the breadcrumb block. `error` already renders underneath, so a 403/404 needs no new surface:

```tsx
<form
  className="file-browser-goto"
  onSubmit={(e) => {
    e.preventDefault();
    const target = gotoDraft.trim();
    if (target) load(target);
  }}
>
  <input
    className="setting-input"
    type="text"
    aria-label="Go to path"
    placeholder="/media/movies"
    value={gotoDraft}
    onChange={(e) => setGotoDraft(e.target.value)}
  />
  <button type="submit" className="btn btn-small">
    Go
  </button>
</form>
```

With `const [gotoDraft, setGotoDraft] = useState("");` beside the other state.

- [ ] **Step 5: Add the CSS**

```css
.file-browser-goto {
  display: flex;
  gap: 6px;
  padding: 8px 12px;
  border-bottom: 1px solid var(--border);
}

.file-browser-goto .setting-input {
  flex: 1;
  min-width: 0;
}
```

- [ ] **Step 6: Run the full frontend suite**

Run: `npm test && npm run build`
Expected: PASS

- [ ] **Step 7: Commit and open PR 2**

```bash
git add src/components/FileBrowserModal.tsx src/components/FileBrowserModal.test.tsx src/App.css
git commit -m "feat: jump-to-path and an inert backdrop in the file picker"
```

Ask the user to push, then `gh pr create --base main --title "feat: file-picker selection overhaul"`, wait for green, `gh pr merge <n> --admin --squash`.

---

# PR 3 — Multi-column layout

Branch from updated `main`: `git checkout -b feature/multi-column-layout`.

---

### Task 11: `useLayoutMode`

**Files:**
- Create: `src/hooks/useLayoutMode.ts`
- Create: `src/hooks/useLayoutMode.test.ts`

**Interfaces:**
- Produces: `type LayoutMode = "tabs" | "two-col" | "three-col"` and `useLayoutMode(): LayoutMode`. Task 12 consumes both.

- [ ] **Step 1: Write the failing test**

```ts
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { renderHook, act } from "@testing-library/react";
import { useLayoutMode } from "./useLayoutMode";

type Listener = () => void;
const listeners = new Map<string, Set<Listener>>();
let matching: string[] = [];

function installMatchMedia() {
  window.matchMedia = ((query: string) => ({
    matches: matching.includes(query),
    media: query,
    addEventListener: (_: string, fn: Listener) => {
      if (!listeners.has(query)) listeners.set(query, new Set());
      listeners.get(query)!.add(fn);
    },
    removeEventListener: (_: string, fn: Listener) => listeners.get(query)?.delete(fn),
  })) as unknown as typeof window.matchMedia;
}

beforeEach(() => {
  listeners.clear();
  matching = [];
  installMatchMedia();
});

afterEach(() => {
  vi.restoreAllMocks();
});

describe("useLayoutMode", () => {
  it("is tabs below the first breakpoint", () => {
    const { result } = renderHook(() => useLayoutMode());
    expect(result.current).toBe("tabs");
  });

  it("is two-col at the 900px breakpoint", () => {
    matching = ["(min-width: 900px)"];
    const { result } = renderHook(() => useLayoutMode());
    expect(result.current).toBe("two-col");
  });

  it("is three-col when both breakpoints match", () => {
    matching = ["(min-width: 900px)", "(min-width: 1300px)"];
    const { result } = renderHook(() => useLayoutMode());
    expect(result.current).toBe("three-col");
  });

  it("updates when a query changes", () => {
    const { result } = renderHook(() => useLayoutMode());
    expect(result.current).toBe("tabs");

    act(() => {
      matching = ["(min-width: 900px)"];
      for (const set of listeners.values()) for (const fn of set) fn();
    });

    expect(result.current).toBe("two-col");
  });

  it("falls back to tabs when matchMedia is unavailable", () => {
    // Some jsdom configurations have no matchMedia. Throwing here would take down the
    // whole app shell, so the narrowest layout is the safe default.
    // @ts-expect-error deliberately removing the API
    window.matchMedia = undefined;
    const { result } = renderHook(() => useLayoutMode());
    expect(result.current).toBe("tabs");
  });
});
```

- [ ] **Step 2: Run to verify it fails**

Run: `npm test -- useLayoutMode`
Expected: FAIL — cannot resolve `./useLayoutMode`

- [ ] **Step 3: Implement**

```ts
import { useEffect, useState } from "react";

/** Which panels are pinned into their own column. `tabs` is the menu-bar popover layout. */
export type LayoutMode = "tabs" | "two-col" | "three-col";

const WIDE = "(min-width: 900px)";
const WIDER = "(min-width: 1300px)";

function currentMode(): LayoutMode {
  // matchMedia is absent in some jsdom configurations; the narrowest layout is the safe
  // fallback, and it is also what the desktop head always resolves to (fixed 400x500).
  if (typeof window.matchMedia !== "function") return "tabs";
  if (window.matchMedia(WIDER).matches) return "three-col";
  if (window.matchMedia(WIDE).matches) return "two-col";
  return "tabs";
}

/**
 * The layout decision, as state rather than CSS: which pages *mount* changes with width,
 * and CSS cannot mount an unmounted component.
 */
export function useLayoutMode(): LayoutMode {
  const [mode, setMode] = useState<LayoutMode>(currentMode);

  useEffect(() => {
    if (typeof window.matchMedia !== "function") return;
    const update = () => setMode(currentMode());
    const queries = [window.matchMedia(WIDE), window.matchMedia(WIDER)];
    for (const q of queries) q.addEventListener("change", update);
    update();
    return () => {
      for (const q of queries) q.removeEventListener("change", update);
    };
  }, []);

  return mode;
}
```

- [ ] **Step 4: Run to verify it passes**

Run: `npm test -- useLayoutMode && npm run build`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/hooks/useLayoutMode.ts src/hooks/useLayoutMode.test.ts
git commit -m "feat: a layout-mode hook driven by viewport width"
```

---

### Task 12: Render columns

**Files:**
- Modify: `src/App.tsx:15-74`
- Modify: `src/components/TabBar.tsx`
- Modify: `src/components/TabBar.test.tsx`
- Create: `src/App.test.tsx` (does not exist yet)
- Modify: `src/App.css`

**Interfaces:**
- Consumes: `useLayoutMode` and `LayoutMode` (Task 11).
- Produces: `TabBar`'s new required `tabs: Tab[]` prop.

- [ ] **Step 1: Write the failing tests**

Create `src/App.test.tsx`. Every page is stubbed to a bare `data-testid`, so this file tests composition — which panels mount at which width — and nothing about page internals:

```tsx
import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen } from "@testing-library/react";
import type { LayoutMode } from "./hooks/useLayoutMode";

let layoutMode: LayoutMode = "tabs";
vi.mock("./hooks/useLayoutMode", () => ({ useLayoutMode: () => layoutMode }));

vi.mock("./pages/QueuePage", () => ({ default: () => <div data-testid="queue-page" /> }));
vi.mock("./pages/HistoryPage", () => ({ default: () => <div data-testid="history-page" /> }));
vi.mock("./pages/WatchedFoldersPage", () => ({ default: () => <div data-testid="watch-page" /> }));
vi.mock("./pages/SettingsPage", () => ({ default: () => <div data-testid="settings-page" /> }));

// App's own hooks reach for IPC on mount; stub them to inert values.
vi.mock("./hooks/useAddProgress", () => ({ useAddProgress: () => ({ isAdding: false, activity: null }) }));
vi.mock("./hooks/useUpdate", () => ({ useUpdate: () => ({ state: null }) }));
vi.mock("./hooks/useFileIntake", () => ({
  useFileIntake: () => ({
    pendingConfirm: null,
    onAdd: vi.fn(),
    onSkip: vi.fn(),
    status: null,
    isDragOver: false,
    addPaths: vi.fn(),
  }),
}));
vi.mock("./lib/tauri", () => ({
  commands: {
    validateHandbrake: () => Promise.resolve({ found: true, path: "/usr/bin/HandBrakeCLI" }),
    hideWindow: vi.fn(),
  },
}));

import App from "./App";

beforeEach(() => {
  vi.clearAllMocks();
  layoutMode = "tabs";
});

describe("App layout", () => {
it("pins Queue and tabs the rest at two-col", async () => {
  layoutMode = "two-col";
  render(<App />);

  expect(await screen.findByTestId("queue-page")).toBeInTheDocument();
  // Queue is always visible, so its tab button is gone.
  expect(screen.queryByRole("button", { name: "Queue" })).not.toBeInTheDocument();
  expect(screen.getByRole("button", { name: "History" })).toBeInTheDocument();
  // activeTab defaults to "queue", which is pinned — the derived fallback must land on
  // the first tab still in the bar rather than rendering an empty column.
  expect(screen.getByTestId("history-page")).toBeInTheDocument();
  // The pinned panel names itself; the tabbed one is already named by its tab button.
  expect(screen.getByRole("heading", { name: "Queue" })).toBeInTheDocument();
});

it("renders every panel and no tab buttons at three-col", async () => {
  layoutMode = "three-col";
  render(<App />);

  expect(await screen.findByTestId("queue-page")).toBeInTheDocument();
  expect(screen.getByTestId("history-page")).toBeInTheDocument();
  expect(screen.getByTestId("watch-page")).toBeInTheDocument();
  expect(screen.getByTestId("settings-page")).toBeInTheDocument();
  for (const label of ["Queue", "History", "Watch", "Settings"]) {
    expect(screen.queryByRole("button", { name: label })).not.toBeInTheDocument();
  }
  // Each pinned panel names itself, so a four-panel view is self-describing.
  expect(screen.getByRole("heading", { name: "Watch" })).toBeInTheDocument();
  expect(screen.getByRole("heading", { name: "Settings" })).toBeInTheDocument();
});

it("keeps the classic tab bar below the first breakpoint", async () => {
  layoutMode = "tabs";
  render(<App />);

  expect(await screen.findByTestId("queue-page")).toBeInTheDocument();
  expect(screen.getByRole("button", { name: "Queue" })).toBeInTheDocument();
  expect(screen.queryByTestId("history-page")).not.toBeInTheDocument();
});
});
```

- [ ] **Step 2: Run to verify they fail**

Run: `npm test -- App`
Expected: FAIL — only one page ever renders.

- [ ] **Step 3: Give `TabBar` a tab subset**

`Tab` is currently declared **twice** — `App.tsx:15` and `TabBar.tsx:4`. Make `TabBar` the single owner: export the type and the labels from there, and delete the duplicate declaration in `App.tsx` in favour of `import TabBar, { type Tab, TAB_LABELS } from "./components/TabBar";`.

Note `activeTab` becomes optional. In `three-col` no tab is active at all, and typing it `Tab` while passing `tabbed[0]` from an empty array would be a lie that only survives because `noUncheckedIndexedAccess` is off:

```tsx
export type Tab = "queue" | "history" | "watch" | "settings";

interface TabBarProps {
  tabs: Tab[];
  /** Undefined in three-col, where every panel is pinned and nothing is tabbed. */
  activeTab: Tab | undefined;
  onTabChange: (tab: Tab) => void;
  isAdding: boolean;
  updateAvailable: boolean;
}

export const TAB_LABELS: Record<Tab, string> = {
  queue: "Queue",
  history: "History",
  watch: "Watch",
  settings: "Settings",
};

export default function TabBar({ tabs, activeTab, onTabChange, isAdding, updateAvailable }: TabBarProps) {
  return (
    <div className="tab-bar" data-tauri-drag-region>
      {tabs.map((tab) => (
        <button
          key={tab}
          className={`tab-btn ${activeTab === tab ? "active" : ""}`}
          onClick={() => onTabChange(tab)}
        >
          {TAB_LABELS[tab]}
          {tab === "settings" && updateAvailable && (
            <span className="tab-badge" aria-label="Update available" />
          )}
        </button>
      ))}
      <div className="tab-spacer" data-tauri-drag-region />
      {isAdding && (
        <span className="tab-spinner" title="Adding files to the queue…" aria-label="Adding files" />
      )}
      {!isServerHead && (
        <button className="tab-btn close-tab-btn" onClick={() => commands.hideWindow()} title="Close">
          &times;
        </button>
      )}
    </div>
  );
}
```

The bar stays mounted in every mode even when `tabs` is empty: it carries `data-tauri-drag-region`, the adding spinner, and the desktop close button, none of which have another home.

- [ ] **Step 4: Restructure `App`**

```tsx
const PINNED: Record<LayoutMode, Tab[]> = {
  tabs: [],
  "two-col": ["queue"],
  "three-col": ["queue", "history", "watch", "settings"],
};

const ALL_TABS: Tab[] = ["queue", "history", "watch", "settings"];

function App() {
  const layout = useLayoutMode();
  // Deliberately still named setActiveTab: `useFileIntake({ onDrop: () => setActiveTab("queue") })`
  // at App.tsx:22 stays exactly as it is. Renaming the setter here would break that line,
  // and the derived `activeTab` below already absorbs a request for a pinned tab.
  const [requestedTab, setActiveTab] = useState<Tab>("queue");
  // ... existing hbStatus / unauthorized / intake / update state, unchanged

  const pinned = PINNED[layout];
  const tabbed = ALL_TABS.filter((t) => !pinned.includes(t));
  // Derived, never stored: selecting a pinned tab resolves to a visible one instead of
  // blanking the tabbed column. This also covers useFileIntake's drop-to-Queue switch.
  // Undefined only in three-col, where `tabbed` is empty and nothing is tabbed at all.
  const activeTab: Tab | undefined = tabbed.includes(requestedTab) ? requestedTab : tabbed[0];

  const panel = (tab: Tab) => {
    switch (tab) {
      case "queue":
        return <QueuePage hbStatus={hbStatus} adding={activity} isAdding={isAdding} intake={intake} />;
      case "history":
        return <HistoryPage />;
      case "watch":
        return <WatchedFoldersPage />;
      case "settings":
        return <SettingsPage onHbPathChanged={refreshHbStatus} />;
    }
  };

  if (unauthorized) return <LoginScreen />;

  return (
    <div className={`app app-${layout}`}>
      <TabBar
        tabs={tabbed}
        activeTab={activeTab}
        onTabChange={setActiveTab}
        isAdding={isAdding}
        updateAvailable={updateState?.status === "available"}
      />
      <div className="app-columns">
        {/* three-col groups Watch and Settings into one column: Settings is by far the
            longest panel, and pairing it with the shortest balances the row. */}
        {layout === "three-col" ? (
          <>
            <section className="app-column">
              <h2 className="app-column-title">Queue</h2>
              {panel("queue")}
            </section>
            <section className="app-column">
              <h2 className="app-column-title">History</h2>
              {panel("history")}
            </section>
            <section className="app-column">
              <h2 className="app-column-title">Watch</h2>
              {panel("watch")}
              <h2 className="app-column-title">Settings</h2>
              {panel("settings")}
            </section>
          </>
        ) : (
          <>
            {pinned.map((tab) => (
              <section className="app-column" key={tab}>
                <h2 className="app-column-title">{TAB_LABELS[tab]}</h2>
                {panel(tab)}
              </section>
            ))}
            <section className="app-column page">{activeTab && panel(activeTab)}</section>
          </>
        )}
      </div>
    </div>
  );
}
```

The `{activeTab && panel(activeTab)}` guard in the else-branch is what makes the optional type safe: in `two-col` `tabbed` is never empty so it always renders, and the branch is unreachable in `three-col`.

- [ ] **Step 5: Add the CSS**

```css
.app-columns {
  display: flex;
  flex: 1;
  min-height: 0;
}

.app-column {
  flex: 1 1 0;
  min-width: 0;
  overflow-y: auto;
  border-right: 1px solid var(--border);
}

.app-column:last-child {
  border-right: none;
}

/* The tabs layout keeps a single scroll container, exactly as before. */
.app-tabs .app-columns {
  display: block;
  overflow-y: auto;
}

.app-column-title {
  position: sticky;
  top: 0;
  z-index: 1;
  margin: 0;
  padding: 8px 12px;
  background: var(--bg-secondary);
  border-bottom: 1px solid var(--border);
  font-size: 11px;
  font-weight: 600;
  letter-spacing: 0.04em;
  text-transform: uppercase;
  color: var(--text-secondary);
}

/* Only ever visible in the browser: the desktop window is a fixed 400x500. */
.app-tabs .app-column-title {
  display: none;
}
```

- [ ] **Step 6: Run everything**

Run: `npm test && npm run build`
Expected: PASS. `TabBar.test.tsx` needs the new `tabs` prop — update its render calls to pass `tabs={["queue", "history", "watch", "settings"]}`.

- [ ] **Step 7: Commit and open PR 3**

```bash
git add src/App.tsx src/App.test.tsx src/components/TabBar.tsx src/components/TabBar.test.tsx src/App.css
git commit -m "feat: multi-column layout on wide displays"
```

Push, `gh pr create --base main --title "feat: multi-column web UI layout"`, wait for green, `gh pr merge <n> --admin --squash`.

---

# PR 4 — Intake without drag-and-drop

Branch from updated `main`: `git checkout -b feature/web-intake-picker`.

---

### Task 13: A click-to-pick intake surface

**Files:**
- Modify: `src/components/DropZone.tsx`
- Modify: `src/components/DropZone.test.tsx`
- Modify: `src/pages/QueuePage.tsx:86-113` and `:172-177`
- Modify: `src/App.css`
- Modify: `README.md`

**Interfaces:**
- Produces: `DropZone`'s optional `onPick?: () => void`.

- [ ] **Step 1: Write the failing tests**

`DropZone.test.tsx` drives clicks with `userEvent`, not `fireEvent` (its imports are `render, screen` plus `userEvent`). Match that — do not add a `fireEvent` import for these three:

```ts
it("renders a pick button instead of the drop label when onPick is given", async () => {
  const onPick = vi.fn();
  render(
    <DropZone pendingConfirm={null} onAdd={vi.fn()} onSkip={vi.fn()} status={null} isDragOver={false} onPick={onPick} />,
  );

  // There is no OS drag-drop event in a browser tab, so advertising one is a lie.
  expect(screen.queryByText(/Drop video files/)).not.toBeInTheDocument();
  await userEvent.click(screen.getByRole("button", { name: /Add files or folders/ }));
  expect(onPick).toHaveBeenCalled();
});

it("keeps the drop label when onPick is absent", () => {
  render(
    <DropZone pendingConfirm={null} onAdd={vi.fn()} onSkip={vi.fn()} status={null} isDragOver={false} />,
  );
  expect(screen.getByText(/Drop video files/)).toBeInTheDocument();
});

it("shows the folder confirm prompt even when onPick is given", () => {
  render(
    <DropZone
      pendingConfirm={{ folder_path: "/m", folder_name: "m", file_count: 9 }}
      onAdd={vi.fn()}
      onSkip={vi.fn()}
      status={null}
      isDragOver={false}
      onPick={vi.fn()}
    />,
  );
  // onPick must not shadow the confirm branch — that would strand the intake pipeline.
  expect(screen.getByText(/Add 9 files/)).toBeInTheDocument();
  expect(screen.queryByRole("button", { name: /Add files or folders/ })).not.toBeInTheDocument();
});
```

- [ ] **Step 2: Run to verify they fail**

Run: `npm test -- DropZone`
Expected: FAIL — `onPick` is not a prop.

- [ ] **Step 3: Add the branch**

```tsx
interface DropZoneProps {
  pendingConfirm: FolderScanResult | null;
  onAdd: () => void;
  onSkip: () => void;
  status: string | null;
  isDragOver: boolean;
  /** Server head: there is no OS drag-drop in a browser tab, so the surface opens the
   *  file browser instead of advertising a drop that cannot happen. */
  onPick?: () => void;
}
```

Replace only the final label branch:

```tsx
) : onPick ? (
  <button type="button" className="drop-zone-pick" onClick={onPick}>
    Add files or folders…
  </button>
) : (
  <span className="drop-zone-label">Drop video files or folders here</span>
)}
```

- [ ] **Step 4: Pin the QueuePage side in its own test**

`QueuePage.test.tsx` currently only covers the desktop empty state, which survives this change untouched. Add the server-head assertion, using the same `stubEnv` + `resetModules` pattern `SettingsPage.test.tsx` uses (`isServerHead` is a module-level const):

```ts
it("has no separate intake button on the server head — the drop surface is the picker", async () => {
  vi.stubEnv("VITE_HEAD", "server");
  vi.resetModules();
  const { default: FreshQueuePage } = await import("./QueuePage");

  render(
    <FreshQueuePage hbStatus={{ found: true, path: "/usr/bin/HandBrakeCLI" }} adding={null} isAdding={false} intake={stubIntake()} />,
  );

  expect(await screen.findByRole("button", { name: /Add files or folders/ })).toBeInTheDocument();
  // The old standalone "Add files…" button is gone: two controls for one action was the
  // thing this task removes.
  expect(screen.queryByRole("button", { name: /^Add files…$/ })).not.toBeInTheDocument();
});
```

`stubIntake()` is a local helper returning the same inert `FileIntake` shape the file's existing tests already build; reuse theirs rather than adding a second one.

- [ ] **Step 5: Wire up `QueuePage`**

Pass `onPick` only on the server head, and delete the now-redundant `.intake-actions` block at `:96-102`:

```tsx
<DropZone
  pendingConfirm={intake.pendingConfirm}
  onAdd={intake.onAdd}
  onSkip={intake.onSkip}
  status={intake.status}
  isDragOver={intake.isDragOver}
  onPick={isServerHead ? () => setShowBrowser(true) : undefined}
/>
```

And the empty state at `:172-177`:

```tsx
{!isAdding && !activeJob && pendingJobs.length === 0 && (
  <div className="empty-state">
    <span className="empty-state-icon">&#128194;</span>
    <span>
      {isServerHead
        ? "Add files or folders to get started"
        : "Drag video files or folders here to get started"}
    </span>
  </div>
)}
```

- [ ] **Step 6: Add the CSS**

```css
.drop-zone-pick {
  width: 100%;
  padding: 0;
  background: none;
  border: none;
  color: var(--accent);
  font-size: 13px;
  cursor: pointer;
}
```

- [ ] **Step 7: Document it**

Add to the README under `## Server (Docker)` (there is no `settings` heading; the nearest neighbours are `## Server (Docker)` at :100 and "How a conversion works" at :95):

```markdown
The web UI takes files through the picker, not by dragging them onto the page — a
browser tab receives no OS drag-drop event. Click the intake panel on the Queue tab
to browse. Inside the picker, every row has a checkbox (folders included, added
recursively), the header selects everything in the current folder, shift-click selects
a range, and the selection survives moving between folders. Reordering the queue by
dragging still works.
```

- [ ] **Step 8: Run everything**

Run: `npm test && npm run build && cargo test --workspace`
Expected: PASS

- [ ] **Step 9: Commit and open PR 4**

```bash
git add src/components/DropZone.tsx src/components/DropZone.test.tsx src/pages/QueuePage.tsx src/App.css README.md
git commit -m "feat: a click-to-pick intake surface on the web UI"
```

Push, `gh pr create --base main --title "feat: web intake without drag-and-drop"`, wait for green, `gh pr merge <n> --admin --squash`.

---

## Final verification

Once all four PRs are merged, from an updated `main`:

- [ ] `cargo test --workspace` — green
- [ ] `npm test` — green
- [ ] `npm run build` — green
- [ ] `cargo fmt --check` — clean
- [ ] Walk the spec's Acceptance Criteria 1-10 by hand against a running server head
      (`CONVERTBAR_DATA_DIR=/tmp/cb-manual cargo run -p convertbar-server`), resizing the
      browser through both breakpoints, and confirm criterion 7 by converting a file
      under keep and restarting the server with the source still in a watched folder.
