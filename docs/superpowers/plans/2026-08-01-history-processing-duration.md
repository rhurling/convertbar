# History Processing Duration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Show each history entry's encode duration under its status badge, behind a setting that defaults on.

**Architecture:** A new additive `jobs.started_at` column is stamped when a job is atomically claimed into `encoding`, and cleared by any non-terminal transition back out (crash recovery, mid-encode pause). The frontend computes the delta against the existing `completed_at` and renders it right-aligned on the entry's bottom row. One implementation serves both heads — desktop and the headless server render the same React history list.

**Tech Stack:** Rust (rusqlite, chrono), React + TypeScript, Vitest + React Testing Library, Cargo test.

**Spec:** `docs/superpowers/specs/2026-08-01-history-processing-duration-design.md`

## Global Constraints

- Conventional commit messages (`feat:`, `fix:`, `test:`, `docs:`). Commits are signed; if signing fails with a 1Password error, unlock and retry once.
- Rust must stay `cargo fmt` clean. Run `cargo fmt --all` before each Rust commit.
- **Never emit a Tauri event while holding the `ctx.db` lock.** The tray listener re-locks `ctx.db` synchronously on the same thread and `std::sync::Mutex` is not reentrant. Two shipped deadlocks came from violating this. No task here adds an emit, but do not move existing ones.
- Any Rust test that reaches HandBrake resolution must declare its locator world explicitly — `AbsentLocator` for the CI world, `StubLocator` for the installed world. The default `PanickingLocator` fails loud on purpose. A confusing `PoisonError` on `ctx.db` in a queue-thread test usually means a missing locator declaration.
- `started_at` is appended as the **last** column in every SELECT list (index 14). `row_to_job` reads by positional index; inserting it anywhere else silently shifts every field after it.
- Do not add `AND status = 'encoding'` to the pause UPDATE. The spec records that pre-existing race as knowingly inherited; narrowing it is a separate change.
- Run `npx tsc --noEmit` before frontend commits — several plumbing errors here are compiler-caught by design.

---

## File Structure

**Rust — `crates/convertbar-core/src/`**

| File | Change |
|---|---|
| `db.rs` | `started_at` in `CREATE TABLE`; idempotent `ALTER`; `history_show_duration` default; count guard 18 → 19 |
| `types.rs` | `JobInfo.started_at`; `Settings.history_show_duration` |
| `queue_ops.rs` | `row_to_job` + four SELECT lists; the `JobInfo` literal in the add path |
| `converter.rs` | `get_next_job` SELECT; stamp in `claim_job`; clear in `recover_interrupted_jobs` |
| `control.rs` | Clear in `pause_conversion` |
| `settings_ops.rs` | Allowlist, parser arm + `true` fallback, struct field |

**Frontend — `src/`**

| File | Change |
|---|---|
| `lib/transport/types.ts` | `JobInfo.started_at`; settings interface field |
| `lib/format.ts` | `durationSeconds`, `formatDuration` |
| `components/HistoryItem.tsx` | `showDuration` prop; duration span; error-row restructure |
| `App.css` | `.history-item-duration`; `.history-item-error-row`; `.history-item-error-msg` becomes a flex child |
| `pages/HistoryPage.tsx` | Pass `showDuration` from `useSettings()` |
| `pages/SettingsPage.tsx` | "History" setting group (both heads) |

---

## Task 1: Add the `started_at` column

**Files:**
- Modify: `crates/convertbar-core/src/db.rs:83-100` (CREATE TABLE), `:171-175` (ALTER block)
- Test: `crates/convertbar-core/src/db.rs` (the `mod tests` block)

**Interfaces:**
- Consumes: nothing.
- Produces: a nullable `jobs.started_at TEXT` column present on both fresh and upgraded databases.

- [ ] **Step 1: Write the failing test**

Add to `mod tests` in `db.rs`, next to `init_db_adds_failure_class_column`:

```rust
    #[test]
    fn init_db_adds_started_at_column() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        // Writing through the column is the real proof it exists and is TEXT-typed.
        conn.execute(
            "INSERT INTO jobs (id, source_path, output_path, preset, status, queue_order, created_at, started_at)
             VALUES ('j1', '/a.mkv', '/a.mp4', 'p', 'encoding', 0, '2026-01-01T00:00:00Z', '2026-01-01T00:00:05Z')",
            [],
        )
        .unwrap();
        let got: Option<String> = conn
            .query_row("SELECT started_at FROM jobs WHERE id = 'j1'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(got.as_deref(), Some("2026-01-01T00:00:05Z"));
    }

    #[test]
    fn init_db_adds_started_at_to_an_existing_database() {
        // An old DB predating the column. init_db must ALTER it in, leave existing rows
        // NULL, and stay idempotent on a second run — users upgrade in place.
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE jobs (
                id              TEXT PRIMARY KEY,
                source_path     TEXT NOT NULL,
                output_path     TEXT NOT NULL,
                preset          TEXT NOT NULL,
                status          TEXT NOT NULL DEFAULT 'queued',
                original_size   INTEGER,
                converted_size  INTEGER,
                kept_file       TEXT,
                space_saved     INTEGER,
                error_message   TEXT,
                queue_order     INTEGER NOT NULL,
                created_at      TEXT NOT NULL,
                completed_at    TEXT
            );",
        )
        .unwrap();
        insert_job(&conn, "old", "done", "2020-01-01T00:00:00Z", Some("2020-01-01T00:10:00Z"));

        init_db(&conn).unwrap();
        // Second run must not error on the duplicate column.
        init_db(&conn).unwrap();

        let started: Option<String> = conn
            .query_row("SELECT started_at FROM jobs WHERE id = 'old'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(
            started, None,
            "a pre-upgrade row has no start time and must render no duration, not a wrong one"
        );
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p convertbar-core started_at`
Expected: FAIL — `no such column: started_at`.

- [ ] **Step 3: Add the column to CREATE TABLE**

In `db.rs`, in the `CREATE TABLE IF NOT EXISTS jobs` block, add `started_at` immediately after `created_at`:

```sql
            queue_order     INTEGER NOT NULL,
            created_at      TEXT NOT NULL,
            started_at      TEXT,
            completed_at    TEXT
```

- [ ] **Step 4: Add the idempotent migration**

Directly below the existing `failure_class` ALTER block (`db.rs:171-175`), add:

```rust
    // Older DBs predate the encode-start timestamp. Same idempotent pattern as
    // failure_class above. No backfill: a row written before this column existed has no
    // knowable start time, and NULL renders no duration.
    if let Err(e) = conn.execute("ALTER TABLE jobs ADD COLUMN started_at TEXT", []) {
        if !e.to_string().contains("duplicate column name") {
            return Err(e);
        }
    }
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p convertbar-core started_at`
Expected: PASS (2 tests).

- [ ] **Step 6: Run the full core suite for regressions**

Run: `cargo test -p convertbar-core`
Expected: all pass. The column is additive, so nothing should move.

- [ ] **Step 7: Commit**

```bash
cargo fmt --all
git add crates/convertbar-core/src/db.rs
git commit -m "feat(db): add additive jobs.started_at column"
```

---

## Task 2: Surface `started_at` on `JobInfo`

**Files:**
- Modify: `crates/convertbar-core/src/types.rs:17-18`
- Modify: `crates/convertbar-core/src/queue_ops.rs` — `row_to_job` (`:62-79`), `get_queue` SELECT (`~:1188`), the add-path `JobInfo` literal (`~:1084-1099`), `get_bad_sources_inner` SELECT (`~:1291`), both `get_history` SELECTs (`~:1413`, `~:1430`)
- Modify: `crates/convertbar-core/src/converter.rs:433-457` (`get_next_job`)
- Modify: `src/lib/transport/types.ts:3-17`
- Test: `crates/convertbar-core/src/queue_ops.rs` (the `mod tests` block)

**Interfaces:**
- Consumes: the `started_at` column from Task 1.
- Produces: `JobInfo.started_at: Option<String>` (Rust) / `started_at: string | null` (TS), reaching both heads through `get_history`.

- [ ] **Step 1: Write the failing test**

Add to `mod tests` in `queue_ops.rs`. Match the existing fixture style in that module for creating a `Ctx`; if a helper like `test_ctx` already exists there, use it rather than building one.

```rust
    #[test]
    fn get_history_carries_started_at_through_to_the_frontend() {
        // The whole feature is unreachable if the column is not in the SELECT list that
        // feeds row_to_job — and because row_to_job reads by positional index, a column
        // appended anywhere but last would silently shift every field after it.
        let (ctx, _sink, _disposer) = test_ctx(test_conn());
        ctx.db
            .lock()
            .unwrap()
            .execute(
                "INSERT INTO jobs (id, source_path, output_path, preset, status, original_size,
                                   queue_order, created_at, started_at, completed_at)
                 VALUES ('j1', '/a.mkv', '/a.mp4', 'p', 'done', 1000, 0,
                         '2026-08-01T10:00:00+00:00',
                         '2026-08-01T10:00:05+00:00',
                         '2026-08-01T10:12:39+00:00')",
                [],
            )
            .unwrap();

        let page = get_history(&ctx, 10, 0, None, None).unwrap();
        let job = &page.jobs[0];
        assert_eq!(job.started_at.as_deref(), Some("2026-08-01T10:00:05+00:00"));
        assert_eq!(
            job.completed_at.as_deref(),
            Some("2026-08-01T10:12:39+00:00"),
            "the fields after started_at must not have shifted"
        );
        assert_eq!(job.original_size, Some(1000), "nor the fields before it");
    }
```

If `get_history`'s signature in this codebase differs, adapt the call — do not change the function.

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p convertbar-core get_history_carries_started_at`
Expected: FAIL to compile — `no field `started_at` on type `JobInfo``.

- [ ] **Step 3: Add the field to the Rust struct**

In `types.rs`, in `struct JobInfo`, between `created_at` and `completed_at`:

```rust
    pub created_at: String,
    /// When the encode was claimed and HandBrake launched. NULL for a job that never
    /// reached the claim, and cleared by any non-terminal transition back out of
    /// `encoding` (crash recovery, mid-encode pause) — see the invariant in
    /// converter::claim_job.
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
```

- [ ] **Step 4: Update `row_to_job` and every SELECT**

In `queue_ops.rs`, `row_to_job` — note `started_at` is read at index **14**, after `completed_at` at 13, because it is appended last in the SQL:

```rust
        created_at: row.get(12)?,
        completed_at: row.get(13)?,
        started_at: row.get(14)?,
    })
```

Append `, started_at` to the end of the column list in all four SELECTs in this file (`get_queue`, `get_bad_sources_inner`, and both branches of `get_history`). Each currently ends `... created_at, completed_at`; make it `... created_at, completed_at, started_at`.

Add to the add-path `JobInfo` literal (`~:1084`), which builds a freshly queued job:

```rust
            created_at: now,
            started_at: None,
            completed_at: None,
        });
```

- [ ] **Step 5: Update `get_next_job` in `converter.rs`**

Append `started_at` to its SELECT list (after `completed_at`) and add to its literal:

```rust
            created_at: row.get(12)?,
            completed_at: row.get(13)?,
            started_at: row.get(14)?,
        })
```

- [ ] **Step 6: Add the field to the TypeScript interface**

In `src/lib/transport/types.ts`, in `interface JobInfo`:

```ts
  created_at: string;
  started_at: string | null;
  completed_at: string | null;
```

- [ ] **Step 7: Run the tests to verify they pass**

Run: `cargo test --workspace && npx tsc --noEmit`
Expected: Rust passes. `tsc` may report missing `started_at` in frontend `JobInfo` test fixtures — add `started_at: null` to each such literal, then re-run until clean.

- [ ] **Step 8: Commit**

```bash
cargo fmt --all
git add -A
git commit -m "feat(jobs): carry started_at on JobInfo through both heads"
```

---

## Task 3: Stamp `started_at` on the claim

**Files:**
- Modify: `crates/convertbar-core/src/converter.rs:659-668` (`claim_job`)
- Test: `crates/convertbar-core/src/converter.rs` (the `mod tests` block)

**Interfaces:**
- Consumes: `JobInfo.started_at` from Task 2.
- Produces: a non-NULL `started_at` on every job claimed into `encoding`.

- [ ] **Step 1: Write the failing test**

Add to `mod tests` in `converter.rs`, near the other `claim_job` tests:

```rust
    fn started_at_of(db: &Arc<Mutex<Connection>>, id: &str) -> Option<String> {
        db.lock()
            .unwrap()
            .query_row(
                "SELECT started_at FROM jobs WHERE id = ?1",
                params![id],
                |r| r.get(0),
            )
            .unwrap()
    }

    #[test]
    fn claiming_a_job_stamps_the_encode_start_time() {
        let (ctx, _sink, _disposer) = test_ctx(test_conn());
        queue_job(&ctx.db, "j1", "/src/a.mkv", "/out/a.mp4", 1000);

        let outcome = claim_job(&ctx.db.lock().unwrap(), "j1");

        assert_eq!(outcome, ClaimOutcome::Claimed);
        assert!(
            started_at_of(&ctx.db, "j1").is_some(),
            "the duration is measured from the claim; without a stamp the encode time is \
             unknowable after the fact"
        );
    }

    #[test]
    fn a_claim_that_loses_the_race_stamps_nothing() {
        // clear_queue/remove_job can delete or re-status a job during the pre-spawn window.
        // The conditional claim must not stamp a job it did not win, or a later attempt
        // would measure from a start it never had.
        let (ctx, _sink, _disposer) = test_ctx(test_conn());
        queue_job(&ctx.db, "j1", "/src/a.mkv", "/out/a.mp4", 1000);
        ctx.db
            .lock()
            .unwrap()
            .execute("UPDATE jobs SET status = 'done' WHERE id = 'j1'", [])
            .unwrap();

        let outcome = claim_job(&ctx.db.lock().unwrap(), "j1");

        assert_eq!(outcome, ClaimOutcome::Gone);
        assert_eq!(started_at_of(&ctx.db, "j1"), None);
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p convertbar-core claim`
Expected: `claiming_a_job_stamps_the_encode_start_time` FAILS (`started_at` is None). `a_claim_that_loses_the_race_stamps_nothing` passes already — that is correct; it is a guard against the fix over-reaching.

- [ ] **Step 3: Stamp in `claim_job`**

Replace the body of `claim_job` in `converter.rs`:

```rust
/// Atomically claim `job_id` for encoding iff it is still queued. Distinguishes a genuine DB
/// error from "row no longer queued" so a failing UPDATE can't spin the queue on the same job.
///
/// Also stamps `started_at`, the anchor for the encode duration shown in History. The
/// invariant: `started_at` is set here and cleared by any NON-terminal transition back out
/// of `encoding` — `recover_interrupted_jobs` and `pause_conversion`. Terminal transitions
/// (done/skipped/error) leave it alone. A new transition out of `encoding` must answer this
/// question or it will report a stale duration.
fn claim_job(db: &Connection, job_id: &str) -> ClaimOutcome {
    let now = chrono::Utc::now().to_rfc3339();
    match db.execute(
        "UPDATE jobs SET status = 'encoding', started_at = ?2 WHERE id = ?1 AND status = 'queued'",
        params![job_id, now],
    ) {
        Ok(0) => ClaimOutcome::Gone,
        Ok(_) => ClaimOutcome::Claimed,
        Err(e) => ClaimOutcome::Failed(e.to_string()),
    }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p convertbar-core claim`
Expected: PASS.

- [ ] **Step 5: Run the full workspace suite**

Run: `cargo test --workspace`
Expected: all pass. The trigger-based claim-failure test keys on `NEW.status = 'encoding'` and still fires; the `Ok(0)`/`Ok(_)` row-count semantics are unchanged.

- [ ] **Step 6: Commit**

```bash
cargo fmt --all
git add crates/convertbar-core/src/converter.rs
git commit -m "feat(converter): stamp started_at when a job is claimed"
```

---

## Task 4: Clear the stamp on every non-terminal exit from `encoding`

**Files:**
- Modify: `crates/convertbar-core/src/converter.rs:57-60` (`recover_interrupted_jobs`)
- Modify: `crates/convertbar-core/src/control.rs:84-87` (`pause_conversion`)
- Test: `crates/convertbar-core/src/converter.rs`, `crates/convertbar-core/src/control.rs`

**Interfaces:**
- Consumes: the stamp from Task 3.
- Produces: the full invariant — `started_at` is non-NULL only for an attempt that ran through to a terminal state.

This is the task that fixes the review's headline defect. Without the recovery clause, a crash followed by a deleted source yields a 12-hour "encode duration" on an error row.

- [ ] **Step 1: Write the failing recovery test**

Add to `mod tests` in `converter.rs`:

```rust
    #[test]
    fn recovery_clears_the_stamp_so_a_pre_claim_error_reports_no_duration() {
        // The regression this exists for: an encode stamped Monday 22:00, a crash, a source
        // the user then deletes, a relaunch Tuesday. Recovery re-queues the job, and the
        // vanished-source gate errors it BEFORE the claim — so nothing re-stamps. A stale
        // stamp plus a fresh completed_at reads as a 12-hour encode that never happened.
        //
        // AbsentLocator, not the default PanickingLocator: process_queue resolves the
        // HandBrake path before reaching the vanished-source gate, so this test must
        // declare that it lives in the no-HandBrake world.
        let (ctx, _sink, _disposer) =
            test_ctx_with_locator(test_conn(), Arc::new(crate::handbrake::AbsentLocator));
        let dir = tempfile::tempdir().unwrap();
        // Deliberately never created on disk — this is the vanished source.
        let src = dir.path().join("gone.mp4");
        let out = dir.path().join("gone-conv.mp4");
        queue_job(
            &ctx.db,
            "j1",
            src.to_str().unwrap(),
            out.to_str().unwrap(),
            1000,
        );
        // The interrupted first attempt: stamped, left 'encoding' by the crash.
        ctx.db
            .lock()
            .unwrap()
            .execute(
                "UPDATE jobs SET status = 'encoding', started_at = '2026-08-01T22:00:00+00:00'
                 WHERE id = 'j1'",
                [],
            )
            .unwrap();

        recover_interrupted_jobs(&ctx.db.lock().unwrap());

        assert_eq!(
            started_at_of(&ctx.db, "j1"),
            None,
            "recovery returns the job to 'queued', so the abandoned attempt's start time \
             must not survive into the next one"
        );

        *ctx.converter.is_running.lock().unwrap() = true;
        process_queue(&ctx);

        let (status, _msg) = job_row(&ctx.db, "j1");
        assert_eq!(status, "error", "the vanished source fails the job");
        assert_eq!(
            started_at_of(&ctx.db, "j1"),
            None,
            "a job that errored before ever being claimed has no encode duration to report"
        );
    }

    #[test]
    fn a_recovered_job_restamps_when_it_is_claimed_again() {
        // The other half: clearing on recovery must not leave the retry unmeasured.
        let (ctx, _sink, _disposer) = test_ctx(test_conn());
        queue_job(&ctx.db, "j1", "/src/a.mkv", "/out/a.mp4", 1000);
        ctx.db
            .lock()
            .unwrap()
            .execute(
                "UPDATE jobs SET status = 'encoding', started_at = '2026-08-01T22:00:00+00:00'
                 WHERE id = 'j1'",
                [],
            )
            .unwrap();

        recover_interrupted_jobs(&ctx.db.lock().unwrap());
        claim_job(&ctx.db.lock().unwrap(), "j1");

        let stamped = started_at_of(&ctx.db, "j1").expect("the re-claim stamps a fresh start");
        assert_ne!(
            stamped, "2026-08-01T22:00:00+00:00",
            "the retry is measured from its own start, not the abandoned attempt's"
        );
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p convertbar-core recovery_clears_the_stamp a_recovered_job_restamps`
Expected: `recovery_clears_the_stamp_...` FAILS on the first assertion (the stale `2026-08-01T22:00:00+00:00` survives). `a_recovered_job_restamps...` passes already.

- [ ] **Step 3: Clear the stamp in `recover_interrupted_jobs`**

In `converter.rs`, in the `for` loop:

```rust
        let _ = db.execute(
            // started_at is cleared with the status: the abandoned attempt's start time must
            // not survive into the next one. Three error paths in process_queue (vanished
            // source, HandBrake-not-found, ClaimOutcome::Failed) write completed_at WITHOUT
            // re-claiming, so a surviving stamp would report the whole downtime as encode time.
            "UPDATE jobs SET status = 'queued', started_at = NULL WHERE id = ?1",
            params![id],
        );
```

- [ ] **Step 4: Run to verify they pass**

Run: `cargo test -p convertbar-core recovery_clears_the_stamp a_recovered_job_restamps`
Expected: PASS.

- [ ] **Step 5: Write the failing pause test**

Add to `mod tests` in `control.rs`, modelled on the existing pause test that spawns a throwaway child (see the SIGSTOP/SIGCONT cleanup comments around `control.rs:604-650` — reuse that fixture's approach so the test never signals its own process). Note `control.rs`'s `test_ctx` returns a bare `Arc<Ctx>`, not the 3-tuple that `converter.rs`/`queue_ops.rs` use, and this module has no `test_conn()` helper:

```rust
    #[cfg(unix)]
    #[test]
    fn a_mid_encode_pause_discards_the_duration_measurement() {
        // Wall clock cannot tell a 5-minute encode from a 5-minute encode paused for eight
        // hours. Rather than accumulate paused time, the pause throws the measurement away:
        // a blank duration is honest, an eight-hour one is a lie.
        let conn = Connection::open_in_memory().unwrap();
        crate::db::init_db(&conn).unwrap();
        let ctx = test_ctx(conn);
        // Reuse the existing pause test's throwaway-child fixture to get a real, live PID.
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let pid = child.id();
        *ctx.converter.current_pid.lock().unwrap() = Some(pid);
        *ctx.converter.current_job_id.lock().unwrap() = Some("j1".to_string());
        ctx.db
            .lock()
            .unwrap()
            .execute(
                "INSERT INTO jobs (id, source_path, output_path, preset, status, queue_order,
                                   created_at, started_at)
                 VALUES ('j1', '/a.mkv', '/a.mp4', 'p', 'encoding', 0,
                         '2026-08-01T10:00:00+00:00', '2026-08-01T10:00:05+00:00')",
                [],
            )
            .unwrap();

        pause_conversion(&ctx).unwrap();

        let started: Option<String> = ctx
            .db
            .lock()
            .unwrap()
            .query_row("SELECT started_at FROM jobs WHERE id = 'j1'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(
            started, None,
            "a paused encode reports no duration rather than one inflated by the pause"
        );

        // The child is SIGSTOPed by pause_conversion — CONT before kill so it can be reaped.
        unsafe {
            libc::kill(pid as i32, libc::SIGCONT);
        }
        let _ = child.kill();
        let _ = child.wait();
    }

    #[cfg(unix)]
    #[test]
    fn resuming_does_not_restore_the_discarded_start_time() {
        // Resume returns the job to 'encoding' without re-stamping: the encode it is
        // resuming began before the pause, so any stamp written now would measure a
        // fraction of the real work. Once discarded, the attempt stays unmeasured.
        let conn = Connection::open_in_memory().unwrap();
        crate::db::init_db(&conn).unwrap();
        let ctx = test_ctx(conn);
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let pid = child.id();
        *ctx.converter.current_pid.lock().unwrap() = Some(pid);
        *ctx.converter.current_job_id.lock().unwrap() = Some("j1".to_string());
        ctx.db
            .lock()
            .unwrap()
            .execute(
                "INSERT INTO jobs (id, source_path, output_path, preset, status, queue_order,
                                   created_at, started_at)
                 VALUES ('j1', '/a.mkv', '/a.mp4', 'p', 'encoding', 0,
                         '2026-08-01T10:00:00+00:00', '2026-08-01T10:00:05+00:00')",
                [],
            )
            .unwrap();

        pause_conversion(&ctx).unwrap();
        resume_conversion(&ctx).unwrap();

        let (status, started): (String, Option<String>) = ctx
            .db
            .lock()
            .unwrap()
            .query_row(
                "SELECT status, started_at FROM jobs WHERE id = 'j1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(status, "encoding", "resume returns the job to encoding");
        assert_eq!(started, None, "but does not invent a new start time");

        let _ = child.kill();
        let _ = child.wait();
    }
```

- [ ] **Step 6: Run it to verify it fails**

Run: `cargo test -p convertbar-core a_mid_encode_pause_discards`
Expected: FAIL — `started_at` is still `2026-08-01T10:00:05+00:00`.

- [ ] **Step 7: Clear the stamp in `pause_conversion`**

In `control.rs`, amend the existing UPDATE inside the `if let Some(ref job_id)` block. Do **not** move it relative to the `db` guard's scope — the guard must still drop before the emits below it:

```rust
                    let _ = db.execute(
                        // started_at is cleared with the status: wall clock cannot exclude the
                        // paused interval, so the measurement is discarded rather than inflated.
                        // resume_conversion deliberately does not re-stamp.
                        "UPDATE jobs SET status = 'paused', started_at = NULL WHERE id = ?1",
                        params![job_id],
                    );
```

- [ ] **Step 8: Run to verify it passes**

Run: `cargo test -p convertbar-core a_mid_encode_pause_discards`
Expected: PASS.

- [ ] **Step 9: Run the full workspace suite**

Run: `cargo test --workspace`
Expected: all pass. Pay attention to the `LockProbeSink` deadlock-probe test — if it hangs or fails, the `db` guard's scope was changed in step 7.

- [ ] **Step 10: Commit**

```bash
cargo fmt --all
git add crates/convertbar-core/src/converter.rs crates/convertbar-core/src/control.rs
git commit -m "fix(jobs): clear started_at on recovery and pause

Recovery returned a job to 'queued' without clearing the stamp, and three
error paths in process_queue write completed_at before ever re-claiming. A
crash plus a deleted source reported the whole downtime as encode time."
```

---

## Task 5: The `history_show_duration` setting

**Files:**
- Modify: `crates/convertbar-core/src/db.rs` (defaults list; both `assert_eq!(count, 18)` at `:273` and `:334`)
- Modify: `crates/convertbar-core/src/settings_ops.rs` (`ALLOWED_KEYS` `:39-58`; parser `:110-181`)
- Modify: `crates/convertbar-core/src/types.rs` (`struct Settings`)
- Modify: `src/lib/transport/types.ts` (settings interface)
- Modify: `src/pages/SettingsPage.tsx`
- Test: `crates/convertbar-core/src/db.rs`, `crates/convertbar-core/src/settings_ops.rs`, `src/pages/SettingsPage.test.tsx`

**Interfaces:**
- Consumes: nothing from earlier tasks — independent, may be done in parallel with Tasks 1-4.
- Produces: `Settings.history_show_duration: bool` (Rust) / `history_show_duration: boolean` (TS), default `true`.

- [ ] **Step 1: Write the failing tests**

In `db.rs` `mod tests`, add:

```rust
    #[test]
    fn history_duration_defaults_on() {
        // The setting's whole rationale is that Docker users get the duration without
        // hunting through settings. A default flip would be invisible in every other test.
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        assert_eq!(
            setting(&conn, "history_show_duration").as_deref(),
            Some("true")
        );
    }
```

In `settings_ops.rs` `mod tests`, add. That module already has `test_conn()` (in-memory + `init_db`) and `test_ctx(conn)` returning the `(Arc<Ctx>, Arc<TestSink>, Arc<RecordingDisposer>)` tuple; `get_settings` and `update_setting` both take `&Ctx`, not `&Connection`:

```rust
    #[test]
    fn history_show_duration_falls_back_to_true_when_the_row_is_absent() {
        // A DB whose settings row is missing must not silently invert the default. init_db
        // seeds the row, so only deleting it exposes the parser's fallback — and the
        // surrounding initializers are a mix of true and false, so the right value has to
        // be chosen deliberately rather than copied from a neighbour.
        let conn = test_conn();
        conn.execute(
            "DELETE FROM settings WHERE key = 'history_show_duration'",
            [],
        )
        .unwrap();
        let (ctx, _sink, _disposer) = test_ctx(conn);

        assert!(get_settings(&ctx).unwrap().history_show_duration);
    }

    #[test]
    fn history_show_duration_is_writable() {
        let (ctx, _sink, _disposer) = test_ctx(test_conn());

        update_setting(&ctx, "history_show_duration", "false").unwrap();

        assert!(!get_settings(&ctx).unwrap().history_show_duration);
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p convertbar-core history_show_duration history_duration_defaults_on`
Expected: FAIL to compile (no such field) and/or the default assertion fails.

- [ ] **Step 3: Add the seeded default and bump the count guard**

In `db.rs`, add to the `defaults` array after `("update_mode", "automatic"),`:

```rust
        ("history_show_duration", "true"),
```

Change **both** `assert_eq!(count, 18);` (at `:273` and `:334`) to `assert_eq!(count, 19);`.

- [ ] **Step 4: Add the key to the allowlist, the struct, and the parser**

`settings_ops.rs`, end of `ALLOWED_KEYS`:

```rust
    "update_mode",
    "history_show_duration",
];
```

`types.rs`, end of `struct Settings`:

```rust
    pub update_mode: String,
    pub history_show_duration: bool,
```

`settings_ops.rs`, with the other initializers — **`true`**, matching the seeded default:

```rust
    let mut history_show_duration = true;
```

In the `match key.as_str()` block, after the `"update_mode"` arm:

```rust
            "history_show_duration" => history_show_duration = value == "true",
```

And in the returned `Settings { .. }` literal:

```rust
        update_mode,
        history_show_duration,
    })
```

- [ ] **Step 5: Add the TypeScript field**

In `src/lib/transport/types.ts`, in the settings interface (the one containing `notifications_queue_done`), add:

```ts
  history_show_duration: boolean;
```

Then add `history_show_duration: true,` to the settings fixture in each of the four test files: `src/App.layoutTransition.test.tsx`, `src/hooks/useSettings.test.ts`, `src/pages/HistoryPage.test.tsx`, `src/pages/SettingsPage.test.tsx`. Each has a `makeSettings` factory (HistoryPage's takes a positional `badSourceAction`; SettingsPage's takes a `Partial<AppSettings>` overrides object) — add the field inside the returned literal in each.

- [ ] **Step 6: Write the failing settings-UI test**

In `src/pages/SettingsPage.test.tsx`, inside the existing `describe("SettingsPage")`. Copy the server-head harness from the neighbouring `"hides the Trash option on the server head"` test (`:366`) — `isServerHead` is a module-level const, so the env must be stubbed and the module graph reloaded before importing the component:

```tsx
  it("offers the history duration toggle on the server head, where it matters most", async () => {
    // Unlike the menu bar and notification groups, this one is deliberately NOT wrapped in
    // !isServerHead: the Docker web UI is the feature's primary audience. Asserting on the
    // desktop render would pass even if it were gated to desktop only.
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

    expect(await screen.findByLabelText("Show processing time")).toBeInTheDocument();
  });
```

- [ ] **Step 7: Add the settings group**

In `SettingsPage.tsx`, insert **before** the `{/* Menu bar display ... */}` block (so it is not inside a `!isServerHead` guard):

```tsx
      <div className="setting-group">
        <label className="setting-label">History</label>
        <div className="setting-toggles">
          <label className="toggle-label">
            <input
              type="checkbox"
              checked={settings.history_show_duration}
              onChange={(e) =>
                updateSetting("history_show_duration", String(e.target.checked))
              }
            />
            Show processing time
          </label>
        </div>
      </div>
```

- [ ] **Step 8: Run all tests**

Run: `cargo test --workspace && npm test && npx tsc --noEmit`
Expected: all pass.

- [ ] **Step 9: Commit**

```bash
cargo fmt --all
git add -A
git commit -m "feat(settings): add history_show_duration, defaulting on"
```

---

## Task 6: Duration formatting helpers

**Files:**
- Modify: `src/lib/format.ts`
- Test: `src/lib/format.test.ts`

**Interfaces:**
- Consumes: nothing.
- Produces: `durationSeconds(startedAt: string | null, completedAt: string | null): number | null` and `formatDuration(seconds: number): string`.

- [ ] **Step 1: Write the failing tests**

Append to `src/lib/format.test.ts`:

```ts
describe("durationSeconds", () => {
  it("parses the backend's own timestamp format, not a hand-typed one", () => {
    // chrono's to_rfc3339() emits up to nine fractional digits and a +00:00 offset.
    // Nothing else in src/ parses timestamps, so this assumption is otherwise unproven.
    expect(
      durationSeconds(
        "2026-08-01T10:00:00.123456789+00:00",
        "2026-08-01T10:12:34.123456789+00:00",
      ),
    ).toBeCloseTo(754, 3);
  });

  it("returns null when either stamp is missing", () => {
    expect(durationSeconds(null, "2026-08-01T10:00:00+00:00")).toBeNull();
    expect(durationSeconds("2026-08-01T10:00:00+00:00", null)).toBeNull();
  });

  it("returns null for an unparseable stamp rather than NaN", () => {
    expect(durationSeconds("not a date", "2026-08-01T10:00:00+00:00")).toBeNull();
  });

  it("returns null for a non-positive delta, so a clock jump shows nothing", () => {
    // An NTP correction between the two stamps must not render a negative duration.
    expect(
      durationSeconds("2026-08-01T10:05:00+00:00", "2026-08-01T10:00:00+00:00"),
    ).toBeNull();
    expect(
      durationSeconds("2026-08-01T10:00:00+00:00", "2026-08-01T10:00:00+00:00"),
    ).toBeNull();
  });
});

describe("formatDuration", () => {
  it.each([
    [0.3, "<1s"],
    [1, "1s"],
    [59, "59s"],
    [60, "1m 00s"],
    [754, "12m 34s"],
    [3599, "59m 59s"],
    [3600, "1h 00m"],
    [90000, "25h 00m"],
  ])("formats %ss as %s", (seconds, expected) => {
    expect(formatDuration(seconds)).toBe(expected);
  });

  it("never renders a sub-second encode as 0s", () => {
    // An instantly-failing encode is real and must stay distinguishable from "no data",
    // which renders nothing at all.
    expect(formatDuration(0.2)).toBe("<1s");
  });
});
```

Add `durationSeconds` and `formatDuration` to the file's existing import from `./format`.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `npx vitest run src/lib/format.test.ts`
Expected: FAIL — `durationSeconds is not a function`.

- [ ] **Step 3: Implement both helpers**

Append to `src/lib/format.ts`:

```ts
/// Seconds between a job's encode start and its completion, or null when there is no
/// duration to show: a missing stamp (never claimed, paused mid-encode, or a row predating
/// the column), an unparseable stamp, or a non-positive delta from a clock adjustment.
export function durationSeconds(
  startedAt: string | null,
  completedAt: string | null,
): number | null {
  if (!startedAt || !completedAt) return null;
  const start = Date.parse(startedAt);
  const end = Date.parse(completedAt);
  if (Number.isNaN(start) || Number.isNaN(end)) return null;
  const delta = (end - start) / 1000;
  return delta > 0 ? delta : null;
}

// Deliberately not formatEta: the menu bar's ETA format is load-bearing elsewhere and
// must not shift to suit History.
export function formatDuration(seconds: number): string {
  const total = Math.round(seconds);
  if (total < 1) return "<1s";
  if (total < 60) return `${total}s`;
  if (total < 3600) {
    return `${Math.floor(total / 60)}m ${String(total % 60).padStart(2, "0")}s`;
  }
  const h = Math.floor(total / 3600);
  const m = Math.floor((total % 3600) / 60);
  return `${h}h ${String(m).padStart(2, "0")}m`;
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `npx vitest run src/lib/format.test.ts`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/lib/format.ts src/lib/format.test.ts
git commit -m "feat(format): add durationSeconds and formatDuration"
```

---

## Task 7: Render the duration in `HistoryItem`

**Files:**
- Modify: `src/components/HistoryItem.tsx`
- Modify: `src/App.css:329-353`
- Test: `src/components/HistoryItem.test.tsx`

**Interfaces:**
- Consumes: `JobInfo.started_at` (Task 2), `durationSeconds`/`formatDuration` (Task 6).
- Produces: `HistoryItem` accepting `showDuration?: boolean` (default `false`).

The error-row change is a **markup restructure, not just a CSS change**. Making `.history-item-error-msg` a flex container turns its text into an anonymous flex item, which kills `text-overflow: ellipsis` and pushes the duration out past `overflow: hidden`. The message must move into a child element that owns the ellipsis rules.

- [ ] **Step 1: Write the failing tests**

Add to `src/components/HistoryItem.test.tsx`. First add `started_at: null,` to the `job()` fixture's defaults, then:

```tsx
  const timed = { started_at: "2026-08-01T10:00:00+00:00", completed_at: "2026-08-01T10:12:34+00:00" };

  it("shows the encode duration when the setting is on", () => {
    render(<HistoryItem job={job(timed)} showDuration />);
    expect(screen.getByText("12m 34s")).toBeInTheDocument();
  });

  it("shows nothing when the setting is off", () => {
    render(<HistoryItem job={job(timed)} showDuration={false} />);
    expect(screen.queryByText("12m 34s")).toBeNull();
  });

  it("shows nothing for a job with no start time", () => {
    // A row predating the column, or one whose encode was paused. Blank is honest;
    // a fabricated time is not.
    render(<HistoryItem job={job({ started_at: null, completed_at: "2026-08-01T10:12:34+00:00" })} showDuration />);
    expect(screen.queryByText(/\ds/)).toBeNull();
  });

  it("shows nothing when the stamps run backwards", () => {
    // A clock adjustment between the two stamps. Rendering "-3m 00s" or "0s" would look
    // like a bug in the encode rather than in the clock.
    render(
      <HistoryItem
        job={job({ started_at: "2026-08-01T10:12:34+00:00", completed_at: "2026-08-01T10:00:00+00:00" })}
        showDuration
      />,
    );
    expect(screen.queryByTitle("Encode time")).toBeNull();
  });

  it("shows the duration of a skipped encode, which is the point of the feature", () => {
    // 'skipped' is a POST-encode status: the encode ran to completion and the output came
    // out no smaller. That wasted time is exactly what the user wants to see.
    render(<HistoryItem job={job({ ...timed, status: "skipped", kept_file: "original" })} showDuration />);
    expect(screen.getByText("12m 34s")).toBeInTheDocument();
  });

  it("shows the duration on an error row without breaking the message ellipsis", () => {
    // The message and the duration share a flex row. If the message keeps the ellipsis
    // rules on the flex container itself they stop applying, and the duration is pushed
    // out of the clipped box — invisible at every width.
    const full = "Conversion failed: moov atom not found\n[mov] moov atom not found";
    render(<HistoryItem job={job({ ...timed, status: "error", error_message: full })} showDuration />);

    expect(screen.getByText("12m 34s")).toBeInTheDocument();
    const msg = screen.getByText(/moov atom not found/);
    expect(msg.title).toBe(full);
    expect(msg.className).toContain("history-item-error-msg");
  });

  it("renders a bottom row for the duration when there are no sizes to show", () => {
    // original_size is null when both stat fallbacks failed at add time. The duration
    // still has to land somewhere.
    render(<HistoryItem job={job({ ...timed, original_size: null, converted_size: null, space_saved: null })} showDuration />);
    expect(screen.getByText("12m 34s")).toBeInTheDocument();
  });
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `npx vitest run src/components/HistoryItem.test.tsx`
Expected: FAIL — no duration is rendered; `showDuration` is not a prop.

- [ ] **Step 3: Rewrite `HistoryItem.tsx`**

```tsx
import type { JobInfo } from "../lib/tauri";
import {
  durationSeconds,
  fileName,
  formatBytes,
  formatDuration,
  formatPercent,
} from "../lib/format";

interface HistoryItemProps {
  job: JobInfo;
  showDuration?: boolean;
  onContextMenu?: (e: React.MouseEvent, job: JobInfo) => void;
}

export default function HistoryItem({
  job,
  showDuration = false,
  onContextMenu,
}: HistoryItemProps) {
  const isError = job.status === "error";
  const keptOriginal = job.kept_file === "original";

  let badgeClass = "badge-green";
  let badgeLabel = "Saved";
  if (isError) {
    badgeClass = "badge-red";
    badgeLabel = "Error";
  } else if (keptOriginal) {
    badgeClass = "badge-amber";
    badgeLabel = "Kept original";
  } else if (job.status === "skipped") {
    badgeClass = "badge-dim";
    badgeLabel = "Skipped";
  }

  const secs = showDuration
    ? durationSeconds(job.started_at, job.completed_at)
    : null;
  const duration =
    secs !== null ? (
      <span className="history-item-duration" title="Encode time">
        {formatDuration(secs)}
      </span>
    ) : null;

  return (
    <div
      className={`history-item ${isError ? "history-item-error" : ""}`}
      onContextMenu={(e) => {
        e.preventDefault();
        onContextMenu?.(e, job);
      }}
    >
      <div className="history-item-top">
        <span className="history-item-name" title={job.source_path}>
          {fileName(job.source_path)}
        </span>
        <span className={`badge ${badgeClass}`}>{badgeLabel}</span>
      </div>
      {!isError && (job.original_size !== null || duration) && (
        <div className="history-item-sizes">
          {job.original_size !== null && (
            <>
              <span>{formatBytes(job.original_size)}</span>
              <span className="arrow">&rarr;</span>
              <span>
                {job.converted_size !== null
                  ? formatBytes(job.converted_size)
                  : "—"}
              </span>
              {job.space_saved !== null && job.space_saved > 0 && (
                <span className="saved-pct">
                  -{formatPercent(job.space_saved, job.original_size)}
                </span>
              )}
            </>
          )}
          {duration}
        </div>
      )}
      {isError && (job.error_message || duration) && (
        <div className="history-item-error-row">
          {job.error_message && (
            <span className="history-item-error-msg" title={job.error_message}>
              {job.error_message}
            </span>
          )}
          {duration}
        </div>
      )}
    </div>
  );
}
```

- [ ] **Step 4: Update the CSS**

In `src/App.css`, replace the `.history-item-error-msg` rule and add the two new ones:

```css
.history-item-duration {
  margin-left: auto;
  white-space: nowrap;
  color: var(--text-dim);
}

.history-item-error-row {
  display: flex;
  align-items: center;
  gap: 8px;
  margin-top: 2px;
}

/* A flex CHILD, not the flex container: text-overflow only ellipsizes a block
   container's own text, so leaving these rules on the row would stop the message
   truncating and push the duration outside the clipped box. */
.history-item-error-msg {
  flex: 1;
  min-width: 0;
  font-size: 11px;
  color: var(--red);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
```

The duration inside `.history-item-error-row` needs the row's font size; add `font-size: 11px;` to `.history-item-error-row` so the duration matches the message.

- [ ] **Step 5: Run the tests to verify they pass**

Run: `npx vitest run src/components/HistoryItem.test.tsx`
Expected: PASS, including the two pre-existing tests (the error-message `title` assertion still resolves, because `getByText` now returns the span that carries it).

- [ ] **Step 6: Commit**

```bash
git add src/components/HistoryItem.tsx src/components/HistoryItem.test.tsx src/App.css
git commit -m "feat(history): render encode duration under the status badge"
```

---

## Task 8: Wire the setting through `HistoryPage`

**Files:**
- Modify: `src/pages/HistoryPage.tsx:320-323`
- Test: `src/pages/HistoryPage.test.tsx`

**Interfaces:**
- Consumes: `HistoryItem`'s `showDuration` prop (Task 7), `history_show_duration` (Task 5).
- Produces: the finished feature.

Without this task the feature ships dead — and every test in Tasks 6 and 7 still passes, because they hand `showDuration` in directly. That is exactly why this task has its own test.

- [ ] **Step 1: Write the failing test**

In `src/pages/HistoryPage.test.tsx`. The file drives the component through module-level mutable fixtures (`page`, `settings`) that its mocked `invoke` reads, reset in `beforeEach`, and renders with a bare `render(<HistoryPage />)`. Add a job helper next to the existing `doneJob`:

```tsx
function timedJob(id: string): JobInfo {
  return {
    ...doneJob(id),
    started_at: "2026-08-01T10:00:00+00:00",
    completed_at: "2026-08-01T10:12:34+00:00",
  };
}
```

Then the two tests:

```tsx
  it("hides the duration when history_show_duration is off", async () => {
    // The gap this closes: every HistoryItem and formatDuration test passes the flag in
    // directly, so a HistoryPage that never reads the setting — or hardcodes it on —
    // leaves the whole suite green while the shipped feature ignores the toggle.
    settings = { ...makeSettings("trash"), history_show_duration: false };
    page = { jobs: [timedJob("1")], total: 1 };

    render(<HistoryPage />);

    expect(await screen.findByText("Saved")).toBeInTheDocument();
    expect(screen.queryByText("12m 34s")).toBeNull();
  });

  it("shows the duration when history_show_duration is on", async () => {
    settings = { ...makeSettings("trash"), history_show_duration: true };
    page = { jobs: [timedJob("1")], total: 1 };

    render(<HistoryPage />);

    expect(await screen.findByText("12m 34s")).toBeInTheDocument();
  });
```

`findByText("Saved")` is the badge on a `doneJob`, and proves the row rendered at all — without it, the negative assertion would also pass on an empty list.

- [ ] **Step 2: Run the test to verify it fails**

Run: `npx vitest run src/pages/HistoryPage.test.tsx`
Expected: FAIL — the `true` case renders no duration.

- [ ] **Step 3: Pass the prop**

In `HistoryPage.tsx`:

```tsx
        {history.map((job) => (
          <HistoryItem
            key={job.id}
            job={job}
            showDuration={settings?.history_show_duration === true}
            onContextMenu={handleItemContextMenu}
          />
        ))}
```

`=== true` rather than a truthy check: `settings` is null while loading, and a nullish value must render nothing rather than flash a duration in.

- [ ] **Step 4: Run the test to verify it passes**

Run: `npx vitest run src/pages/HistoryPage.test.tsx`
Expected: PASS.

- [ ] **Step 5: Run everything**

Run: `cargo test --workspace && npm test && npx tsc --noEmit && npm run build`
Expected: all green. `npm run build` is what CI's `frontend` check runs.

- [ ] **Step 6: Commit**

```bash
git add src/pages/HistoryPage.tsx src/pages/HistoryPage.test.tsx
git commit -m "feat(history): wire the duration setting through HistoryPage"
```

---

## Task 9: Verify against a real database

**Files:** none — this is a manual verification gate before the PR.

Automated tests cannot prove the upgrade path or the visual result. Both failure modes here are silent.

- [ ] **Step 1: Verify the migration against a pre-upgrade database**

Copy the real database aside first, then run the app against the copy:

```bash
cp ~/Library/Application\ Support/com.convertbar.app/convertbar.db /tmp/convertbar-upgrade-test.db
CONVERTBAR_DATA_DIR=/tmp npm run tauri dev
```

Confirm: the app starts, History lists prior conversions, and those rows show **no** duration (they have no `started_at`).

- [ ] **Step 2: Verify a real encode end to end**

Queue a small file and let it finish. Confirm the new row shows a plausible duration, right-aligned under the badge.

- [ ] **Step 3: Verify the pause behavior**

Queue a longer file, pause mid-encode, wait ~30s, resume, let it finish. Confirm the finished row shows **no** duration.

- [ ] **Step 4: Verify the toggle**

Turn "Show processing time" off in Settings, return to History, confirm the durations disappear without a reload. Turn it back on.

- [ ] **Step 5: Verify the error row**

Point a job at a file HandBrake cannot read (an empty `.mp4` works) and confirm the error row shows both a truncated message and a duration, and that the message still ellipsizes rather than pushing the duration off the edge. Narrow the window to check.

- [ ] **Step 6: Record the result**

Note in the PR body which of these were run and on which platform. If any were skipped, say so explicitly rather than implying full coverage.

---

## Definition of Done

- [ ] `cargo test --workspace` green
- [ ] `npm test` green
- [ ] `npx tsc --noEmit` clean
- [ ] `npm run build` succeeds
- [ ] `cargo fmt --all --check` clean
- [ ] Task 9's manual checks run, with any skips stated in the PR body
- [ ] The spec's Known Gaps still accurately describe the shipped behavior
