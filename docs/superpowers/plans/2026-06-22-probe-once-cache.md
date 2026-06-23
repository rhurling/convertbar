# Probe-Once Cache Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Memoize `probe_source` results in a `(path, size, mtime)`-keyed SQLite cache so an already-at-target file is probed once, not on every scan/launch.

**Architecture:** A new `probe_cache` table stores raw `SourceMedia` keyed by file path, validated by size+mtime. A new `probe_cache.rs` module holds the DB layer (`lookup_batch`/`store_batch`) and a pure, closure-driven `resolve_media` orchestrator. `add_files_inner` stamps each probe-candidate with its filesystem identity, reuses cached media for unchanged files, and probes only the misses — all outside the DB lock. The pure skip policy (`media_skip`) is unchanged and still decides skip/queue each scan. `converter.rs` is untouched (an in-place re-encode changes the file's size+mtime, so the next scan misses and re-probes once — see the spec's trap-3 resolution).

**Tech Stack:** Rust, rusqlite (SQLite, UPSERT via `INSERT OR REPLACE`), chrono, Tauri 2.

**Spec:** `docs/superpowers/specs/2026-06-22-probe-once-cache-design.md`

---

## Conventions for this plan

- **Strict types, match surrounding style.** No `declare`; typed returns.
- **rustfmt / format hook.** `probe_cache.rs` is a new file — keep it `cargo fmt`-clean. `queue.rs` and `db.rs` are not repo-fmt-clean and the editor's format hook reformats whole files on save; keep edits surgical and **review the diff** so only intended lines change. Do **not** run a repo-wide `cargo fmt` that reformats unrelated code.
- **DB lock discipline.** The expensive `probe_source` shell-out must never run while the `state.db` mutex is held. `resolve_media` enforces this by calling its `lookup` closure (locks briefly), then `probe` (no lock), then `store` (locks briefly), in that order.
- Run Rust tests with `cargo test --lib` from `src-tauri/`.

## File structure

- **Create** `src-tauri/src/probe_cache.rs` — the cache: `FileIdentity`, DB `lookup_batch`/`store_batch`, and the pure `resolve_media` orchestrator. One responsibility: "probe once, remember the answer."
- **Modify** `src-tauri/src/lib.rs:6` — register `mod probe_cache;`.
- **Modify** `src-tauri/src/db.rs` — add the `probe_cache` table to `init_db`'s `execute_batch`.
- **Modify** `src-tauri/src/commands/queue.rs` — add a `file_identity` helper and replace the eager probe loop in `add_files_inner` with the cache-aware `resolve_media` call; extend the ignored end-to-end test with a second-pass (cache-hit) assertion.

Untouched: `probe.rs`, `media_skip.rs`, `converter.rs`, `types.rs`, `capabilities/default.json`, all frontend.

---

## Task 1: Add the `probe_cache` table to `init_db`

**Files:**
- Modify: `src-tauri/src/db.rs` (the `execute_batch` in `init_db`, and the `tests` module)

- [ ] **Step 1: Write the failing test**

Add to the `tests` module in `src-tauri/src/db.rs`:

```rust
    #[test]
    fn init_db_creates_probe_cache_table() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        // The table accepts a full cache row with the documented columns.
        conn.execute(
            "INSERT INTO probe_cache (path, size, mtime, codec, height, probed_at)
             VALUES ('/m/a.mp4', 100, 5000, 'h265', 1080, '2026-06-22T00:00:00Z')",
            [],
        )
        .unwrap();

        let (size, mtime, codec, height): (i64, i64, String, i64) = conn
            .query_row(
                "SELECT size, mtime, codec, height FROM probe_cache WHERE path = '/m/a.mp4'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!((size, mtime, codec.as_str(), height), (100, 5000, "h265", 1080));

        // `path` is the primary key, so a second row for the same path is rejected.
        let dup = conn.execute(
            "INSERT INTO probe_cache (path, size, mtime, codec, height, probed_at)
             VALUES ('/m/a.mp4', 1, 1, 'h264', 1, '2026-06-22T00:00:00Z')",
            [],
        );
        assert!(dup.is_err(), "duplicate path should violate the PRIMARY KEY");
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd src-tauri && cargo test --lib db::tests::init_db_creates_probe_cache_table`
Expected: FAIL — `no such table: probe_cache`.

- [ ] **Step 3: Add the table to `init_db`**

In `src-tauri/src/db.rs`, inside the `conn.execute_batch("...")` in `init_db`, add this table definition immediately after the `watched_directories` table block (before the closing `");`):

```sql
        CREATE TABLE IF NOT EXISTS probe_cache (
            path      TEXT PRIMARY KEY,
            size      INTEGER NOT NULL,
            mtime     INTEGER NOT NULL,
            codec     TEXT NOT NULL,
            height    INTEGER NOT NULL,
            probed_at TEXT NOT NULL
        );
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cd src-tauri && cargo test --lib db::tests`
Expected: PASS — including the existing `init_db_seeds_defaults` (still asserts **14** settings; no new setting was added) and `init_db_is_idempotent_and_preserves_user_changes`.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/db.rs
git commit -m "feat: add probe_cache table to init_db"
```

---

## Task 2: `probe_cache.rs` DB layer — `FileIdentity`, `lookup_batch`, `store_batch`

**Files:**
- Create: `src-tauri/src/probe_cache.rs`
- Modify: `src-tauri/src/lib.rs:6` (register the module)

- [ ] **Step 1: Register the module**

In `src-tauri/src/lib.rs`, add after `mod probe;` (line 6):

```rust
mod probe_cache;
```

- [ ] **Step 2: Write the failing tests**

Create `src-tauri/src/probe_cache.rs` with the module doc + imports + types and the test module (implementation bodies come in Step 4):

```rust
//! Persistent memoization of `probe_source` results, keyed by file identity
//! `(path, size, mtime)`. A cached `SourceMedia` is reused only when the file still has the
//! same size AND mtime, so any content change — including our own in-place re-encode —
//! forces an honest re-probe. The pure skip policy (`media_skip`) still decides each scan.

use crate::media_skip::SourceMedia;
use rusqlite::{params, Connection};

/// A file's content identity: its byte size and last-modified time (epoch millis). A cached
/// probe is reusable only when BOTH match the file's current stat.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileIdentity {
    pub size: i64,
    pub mtime: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn media(codec: &str, height: i64) -> SourceMedia {
        SourceMedia {
            codec: codec.into(),
            height,
        }
    }

    fn test_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::init_db(&conn).unwrap();
        conn
    }

    #[test]
    fn store_then_lookup_returns_a_hit_on_matching_identity() {
        let conn = test_conn();
        let id = FileIdentity { size: 100, mtime: 5000 };
        store_batch(&conn, &[("/m/a.mp4".to_string(), id, media("h265", 1080))]);

        let (hits, misses) = lookup_batch(&conn, &[("/m/a.mp4".to_string(), id)]);
        assert_eq!(hits, vec![("/m/a.mp4".to_string(), media("h265", 1080))]);
        assert!(misses.is_empty(), "a matching identity is a hit, not a miss");
    }

    #[test]
    fn lookup_misses_when_size_or_mtime_differs() {
        let conn = test_conn();
        let id = FileIdentity { size: 100, mtime: 5000 };
        store_batch(&conn, &[("/m/a.mp4".to_string(), id, media("h265", 1080))]);

        // Changed size — e.g. a different file was dropped at this path.
        let (h1, m1) = lookup_batch(
            &conn,
            &[("/m/a.mp4".to_string(), FileIdentity { size: 101, mtime: 5000 })],
        );
        assert!(h1.is_empty());
        assert_eq!(m1.len(), 1, "a size change must re-probe");

        // Changed mtime — e.g. our in-place re-encode rewrote the file.
        let (h2, m2) = lookup_batch(
            &conn,
            &[("/m/a.mp4".to_string(), FileIdentity { size: 100, mtime: 6000 })],
        );
        assert!(h2.is_empty());
        assert_eq!(m2.len(), 1, "an mtime change must re-probe");
    }

    #[test]
    fn lookup_misses_when_path_absent() {
        let conn = test_conn();
        let (hits, misses) = lookup_batch(
            &conn,
            &[("/m/never.mp4".to_string(), FileIdentity { size: 1, mtime: 1 })],
        );
        assert!(hits.is_empty());
        assert_eq!(misses.len(), 1, "an unseen path is always a miss");
    }

    #[test]
    fn store_upserts_a_stale_row() {
        let conn = test_conn();
        let path = "/m/a.mp4".to_string();
        store_batch(&conn, &[(path.clone(), FileIdentity { size: 100, mtime: 5000 }, media("h264", 1080))]);
        // Re-probe after the file changed: new identity + new media replace the row.
        store_batch(&conn, &[(path.clone(), FileIdentity { size: 200, mtime: 9000 }, media("h265", 720))]);

        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM probe_cache WHERE path = ?1",
                params![path],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1, "upsert keeps exactly one row per path");

        let (hits, _) = lookup_batch(&conn, &[(path.clone(), FileIdentity { size: 200, mtime: 9000 })]);
        assert_eq!(hits, vec![(path, media("h265", 720))], "the row reflects the latest probe");
    }
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cd src-tauri && cargo test --lib probe_cache::tests`
Expected: FAIL — `cannot find function lookup_batch` / `store_batch` in this scope.

- [ ] **Step 4: Implement `lookup_batch` and `store_batch`**

In `src-tauri/src/probe_cache.rs`, between the `FileIdentity` struct and the `#[cfg(test)]` module, add:

```rust
/// Split `candidates` into cache hits and misses. A hit is `(path, media)` whose stored row
/// matches BOTH the supplied size and mtime; everything else (no row, or a stale row) is a
/// miss carrying the identity to re-probe and re-store.
pub fn lookup_batch(
    conn: &Connection,
    candidates: &[(String, FileIdentity)],
) -> (Vec<(String, SourceMedia)>, Vec<(String, FileIdentity)>) {
    let mut hits = Vec::new();
    let mut misses = Vec::new();
    for (path, id) in candidates {
        match cached_media(conn, path, *id) {
            Some(media) => hits.push((path.clone(), media)),
            None => misses.push((path.clone(), *id)),
        }
    }
    (hits, misses)
}

/// Read the cached `SourceMedia` for `path`, but only if the stored size AND mtime equal
/// `id`. Returns `None` on no row, an identity mismatch, or any query error.
fn cached_media(conn: &Connection, path: &str, id: FileIdentity) -> Option<SourceMedia> {
    conn.query_row(
        "SELECT size, mtime, codec, height FROM probe_cache WHERE path = ?1",
        params![path],
        |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
            ))
        },
    )
    .ok()
    .filter(|(size, mtime, _, _)| *size == id.size && *mtime == id.mtime)
    .map(|(_, _, codec, height)| SourceMedia { codec, height })
}

/// Upsert each freshly probed `(path, identity, media)`, replacing any stale row for that
/// path. Cache writes are best-effort: a failure must never break adding files, so errors
/// are swallowed.
pub fn store_batch(conn: &Connection, probed: &[(String, FileIdentity, SourceMedia)]) {
    let now = chrono::Utc::now().to_rfc3339();
    for (path, id, media) in probed {
        let _ = conn.execute(
            "INSERT OR REPLACE INTO probe_cache (path, size, mtime, codec, height, probed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![path, id.size, id.mtime, media.codec, media.height, now],
        );
    }
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cd src-tauri && cargo test --lib probe_cache::tests`
Expected: PASS (4 tests).

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/lib.rs src-tauri/src/probe_cache.rs
git commit -m "feat: probe_cache DB layer (lookup_batch/store_batch)"
```

---

## Task 3: Pure `resolve_media` orchestrator

**Files:**
- Modify: `src-tauri/src/probe_cache.rs` (add `resolve_media` + tests)

- [ ] **Step 1: Write the failing tests**

Add these tests inside the existing `#[cfg(test)] mod tests` in `src-tauri/src/probe_cache.rs` (each test imports `RefCell` locally — no module-level import needed):

```rust
    #[test]
    fn resolve_media_probes_only_misses_and_never_reprobes_a_hit() {
        use std::cell::RefCell;
        // The whole point: a cached hit costs ZERO probes; a miss costs exactly one.
        let probe_calls = RefCell::new(Vec::<String>::new());
        let stored = RefCell::new(Vec::<(String, FileIdentity, SourceMedia)>::new());

        let candidates = vec![
            ("/m/hit.mp4".to_string(), Some(FileIdentity { size: 10, mtime: 100 })),
            ("/m/miss.mp4".to_string(), Some(FileIdentity { size: 20, mtime: 200 })),
        ];

        let lookup = |ids: &[(String, FileIdentity)]| {
            let mut hits = Vec::new();
            let mut misses = Vec::new();
            for (p, id) in ids {
                if p == "/m/hit.mp4" {
                    hits.push((p.clone(), media("h265", 1080)));
                } else {
                    misses.push((p.clone(), *id));
                }
            }
            (hits, misses)
        };
        let probe = |p: &str| {
            probe_calls.borrow_mut().push(p.to_string());
            Some(media("h264", 1080))
        };
        let store = |items: &[(String, FileIdentity, SourceMedia)]| {
            stored.borrow_mut().extend_from_slice(items);
        };

        let out = resolve_media(&candidates, lookup, probe, store);

        assert_eq!(
            probe_calls.borrow().as_slice(),
            ["/m/miss.mp4"],
            "only the miss is probed; the hit is never re-probed"
        );
        assert_eq!(stored.borrow().len(), 1, "the fresh probe is cached");
        assert_eq!(stored.borrow()[0].0, "/m/miss.mp4");

        // Both files come back carrying media for the skip policy.
        let hit = out.iter().find(|(p, _)| p == "/m/hit.mp4").unwrap();
        assert_eq!(hit.1, Some(media("h265", 1080)));
        let miss = out.iter().find(|(p, _)| p == "/m/miss.mp4").unwrap();
        assert_eq!(miss.1, Some(media("h264", 1080)));
    }

    #[test]
    fn resolve_media_probes_identityless_files_but_never_caches_them() {
        use std::cell::RefCell;
        // No readable size/mtime -> no stable key: probe it, but never store it.
        let stored = RefCell::new(Vec::<(String, FileIdentity, SourceMedia)>::new());
        let candidates = vec![("/m/no-id.mp4".to_string(), None)];
        let lookup = |_: &[(String, FileIdentity)]| (Vec::new(), Vec::new());
        let probe = |_: &str| Some(media("h265", 1080));
        let store = |items: &[(String, FileIdentity, SourceMedia)]| {
            stored.borrow_mut().extend_from_slice(items)
        };

        let out = resolve_media(&candidates, lookup, probe, store);

        assert_eq!(out, vec![("/m/no-id.mp4".to_string(), Some(media("h265", 1080)))]);
        assert!(stored.borrow().is_empty(), "an identity-less file is never cached");
    }

    #[test]
    fn resolve_media_does_not_cache_a_failed_probe() {
        use std::cell::RefCell;
        // Uncertainty (None) is never stored, so it is re-evaluated next scan.
        let stored = RefCell::new(Vec::<(String, FileIdentity, SourceMedia)>::new());
        let candidates = vec![("/m/bad.mp4".to_string(), Some(FileIdentity { size: 1, mtime: 1 }))];
        let lookup = |ids: &[(String, FileIdentity)]| (Vec::new(), ids.to_vec());
        let probe = |_: &str| None;
        let store = |items: &[(String, FileIdentity, SourceMedia)]| {
            stored.borrow_mut().extend_from_slice(items)
        };

        let out = resolve_media(&candidates, lookup, probe, store);

        assert_eq!(out, vec![("/m/bad.mp4".to_string(), None)]);
        assert!(stored.borrow().is_empty(), "a failed probe must not be cached");
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cd src-tauri && cargo test --lib probe_cache::tests::resolve_media`
Expected: FAIL — `cannot find function resolve_media`.

- [ ] **Step 3: Implement `resolve_media`**

In `src-tauri/src/probe_cache.rs`, add after `store_batch` (before the `#[cfg(test)]` module):

```rust
/// Memoize probes for `candidates`: reuse cached media for unchanged files, probe only the
/// rest, persist the fresh probes, and return the `(path, Option<SourceMedia>)` list that
/// `media_skip::select_media_skips` consumes. Generic over the three side effects so the
/// memoization behavior is unit-testable without a database or HandBrake.
///
/// Each candidate carries its `FileIdentity`, or `None` when its size/mtime couldn't be
/// read — those are forced misses: probed every time, never cached (no stable key).
///
/// Call order matters: `lookup` runs first (one batch), then `probe` per miss, then `store`
/// once. The wiring relies on this so the expensive probe never holds the DB lock.
pub fn resolve_media<L, P, S>(
    candidates: &[(String, Option<FileIdentity>)],
    lookup: L,
    probe: P,
    store: S,
) -> Vec<(String, Option<SourceMedia>)>
where
    L: Fn(&[(String, FileIdentity)]) -> (Vec<(String, SourceMedia)>, Vec<(String, FileIdentity)>),
    P: Fn(&str) -> Option<SourceMedia>,
    S: Fn(&[(String, FileIdentity, SourceMedia)]),
{
    // Files with no readable identity can't be cached — set them aside to always probe.
    let mut identified = Vec::new();
    let mut forced = Vec::new();
    for (path, id) in candidates {
        match id {
            Some(id) => identified.push((path.clone(), *id)),
            None => forced.push(path.clone()),
        }
    }

    let (hits, misses) = lookup(&identified);

    // Probe every cache miss and every identity-less file. Cache only the successes.
    let mut to_store = Vec::new();
    let mut probed: Vec<(String, Option<SourceMedia>)> = Vec::new();
    for (path, id) in &misses {
        let media = probe(path);
        if let Some(m) = &media {
            to_store.push((path.clone(), *id, m.clone()));
        }
        probed.push((path.clone(), media));
    }
    for path in &forced {
        probed.push((path.clone(), probe(path)));
    }
    store(&to_store);

    // Hits + freshly probed. Order is irrelevant — select_media_skips collects into a set.
    let mut out: Vec<(String, Option<SourceMedia>)> =
        hits.into_iter().map(|(p, m)| (p, Some(m))).collect();
    out.extend(probed);
    out
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cd src-tauri && cargo test --lib probe_cache::tests`
Expected: PASS (7 tests total).

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/probe_cache.rs
git commit -m "feat: pure resolve_media probe memoizer"
```

---

## Task 4: Wire the cache into `add_files_inner`

**Files:**
- Modify: `src-tauri/src/commands/queue.rs` (add `file_identity`; replace the eager probe loop; extend the ignored e2e test)

- [ ] **Step 1: Add the `file_identity` helper**

In `src-tauri/src/commands/queue.rs`, add this free function just above `pub(crate) fn add_files_inner` (after `get_handbrake_path`):

```rust
/// The cache key for a probe: the file's byte size + last-modified time (epoch millis).
/// `None` when the file can't be stat'd or has no readable mtime — such a file has no
/// stable identity and is probed every scan (handled by `resolve_media`'s forced-miss path).
fn file_identity(path: &str) -> Option<crate::probe_cache::FileIdentity> {
    let meta = std::fs::metadata(path).ok()?;
    let mtime = meta
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_millis() as i64;
    Some(crate::probe_cache::FileIdentity {
        size: meta.len() as i64,
        mtime,
    })
}
```

- [ ] **Step 2: Replace the eager probe loop with the cache-aware call**

In `add_files_inner`, inside the `if let Some(hb) = hb_path.as_deref() {` arm of the `media_skipped` block, replace **only** this:

```rust
            let probed: Vec<(String, Option<crate::media_skip::SourceMedia>)> = candidates_to_probe
                .iter()
                .map(|p| (p.clone(), crate::probe::probe_source(hb, p)))
                .collect();
```

with:

```rust
            // Stamp each candidate with its filesystem identity (stat outside the DB lock),
            // then reuse cached media for unchanged files and probe only the misses.
            // resolve_media calls lookup (brief lock) -> probe (no lock) -> store (brief
            // lock), so the HandBrake shell-out never runs while the DB mutex is held.
            let with_identity: Vec<(String, Option<crate::probe_cache::FileIdentity>)> =
                candidates_to_probe
                    .iter()
                    .map(|p| (p.clone(), file_identity(p)))
                    .collect();
            let probed = crate::probe_cache::resolve_media(
                &with_identity,
                |ids| {
                    let conn = state.db.lock().expect("db mutex poisoned");
                    crate::probe_cache::lookup_batch(&conn, ids)
                },
                |p| crate::probe::probe_source(hb, p),
                |items| {
                    let conn = state.db.lock().expect("db mutex poisoned");
                    crate::probe_cache::store_batch(&conn, items);
                },
            );
```

The surrounding `target_codec`, `target_height`, and `select_media_skips(&probed, &target_codec, target_height)` lines stay exactly as they are. (`select_media_skips` consumes `&[(String, Option<SourceMedia>)]`, which `resolve_media` returns.)

- [ ] **Step 3: Verify it compiles cleanly**

Run: `cd src-tauri && cargo build 2>&1 | tail -5`
Expected: builds with **no warnings** (no unused imports; `crate::probe::probe_source` is still referenced inside the probe closure).

- [ ] **Step 4: Extend the ignored end-to-end test with a cache-hit second pass**

In `src-tauri/src/commands/queue.rs`, in the test `add_files_inner_skips_at_target_source_end_to_end`, change the single add call:

```rust
        let result = add_files_inner(&state, &[at_target, upgrade]).unwrap();
```

to bind the inputs so they can be reused:

```rust
        let inputs = vec![at_target, upgrade];
        let result = add_files_inner(&state, &inputs).unwrap();
```

Then, immediately before the test's closing brace (after the existing `reported` assertion), add:

```rust
        // Second pass over the same inputs: the at-target source's identity is unchanged, so
        // its media is served from probe_cache (zero re-probe), and the codec-upgrade source
        // is now already queued. The at-target source must STILL be reported skipped —
        // proving the cached media drives the same decision as a live probe.
        let again = add_files_inner(&state, &inputs).unwrap();
        assert!(again.added.is_empty(), "nothing new to queue on a repeat add");
        let at_target_again = again
            .skipped
            .iter()
            .find(|c| c.reason == SkipReason::AlreadyAtTarget);
        assert_eq!(
            at_target_again.map(|c| c.count),
            Some(1),
            "the cached at-target source is still recognized on re-scan"
        );
```

- [ ] **Step 5: Run the full lib suite + the ignored e2e**

Run: `cd src-tauri && cargo test --lib`
Expected: PASS — all unit tests (including the new `probe_cache` and `db` tests), zero failures.

Run (local only — needs `ffmpeg` + `HandBrakeCLI` on PATH):
`cd src-tauri && cargo test --lib -- --ignored add_files_inner_skips_at_target_source_end_to_end`
Expected: PASS — including the new second-pass assertions. If `ffmpeg`/`HandBrakeCLI` are unavailable, note it as skipped (do not claim it passed).

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/commands/queue.rs
git commit -m "feat: serve source-media probes from probe_cache in add_files_inner"
```

---

## Task 5: Reviewer-agent verification sweep

**Files:** none (verification only; commit only if a reviewer surfaces a required fix)

- [ ] **Step 1: SQLite migration review**

Dispatch the `sqlite-migration-reviewer` agent on the `db.rs` change.
Expected conclusion: the additive `CREATE TABLE IF NOT EXISTS probe_cache` is backward-compatible — an existing `convertbar.db` gains an empty table on next launch, no `jobs` rewrite, no settings-count change.

- [ ] **Step 2: Cross-platform review**

Dispatch the `cross-platform-reviewer` agent on `probe_cache.rs` + the `queue.rs` wiring.
Expected conclusion: `SystemTime`/`metadata.modified()` and the SQLite table are platform-neutral; no `cfg` gating needed; mtime granularity differences are tolerated because the same value is written and compared.

- [ ] **Step 3: ACL audit**

Dispatch the `acl-auditor` agent.
Expected conclusion: no new frontend Tauri API call → `capabilities/default.json` unchanged → no new permission required.

- [ ] **Step 4: Final formatting + full test gate**

Run: `cd src-tauri && cargo fmt -- --check src/probe_cache.rs` (the new file must be clean).
Run: `cd src-tauri && cargo test --lib`
Expected: clean format on the new file; all tests pass.

- [ ] **Step 5: Commit any required fixes**

Only if a reviewer surfaced a genuine fix:

```bash
git add -A
git commit -m "fix: address probe-cache review feedback"
```

---

## Definition of done

- `probe_cache` table created additively in `init_db`; `init_db_seeds_defaults` still asserts 14 settings.
- `probe_cache.rs`: `lookup_batch`/`store_batch` (identity-validated hit, upsert) + pure `resolve_media` (probe once per miss, never re-probe a hit, never cache `None` or identity-less files), all unit-tested.
- `add_files_inner` serves unchanged files from the cache and probes only misses, with the HandBrake shell-out never holding the DB lock.
- `converter.rs`, `media_skip.rs`, `probe.rs`, and the frontend are untouched.
- `cargo test --lib` green; new file `rustfmt`-clean; three reviewer agents report no required changes.
```
