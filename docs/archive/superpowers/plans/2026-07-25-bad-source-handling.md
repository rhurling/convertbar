# Bad-Source Handling Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Classify why a conversion failed, detect truncated source files that HandBrake reports as successes, and give the user a deliberate review list for cleaning up confirmed-bad sources.

**Architecture:** A new pure Rust module (`failure_class.rs`) holds all classification and truncation policy with zero I/O, mirroring the existing `media_skip.rs` pattern. `converter.rs` gathers facts (exit code, our own readability probe, stderr tail) and calls into it, persisting the verdict to a new additive `jobs.failure_class` column. A truncation guard runs in the success arm ahead of `decide_cleanup`, converting a falsely-successful job into an error so the source is never trashed. The frontend gains a review list whose bulk action is the only thing that ever destroys a file.

**Tech Stack:** Rust (rusqlite, tauri 2), React + TypeScript, vitest, `trash` crate.

## Global Constraints

- `declare(strict_types)` equivalent: all Rust must pass `cargo fmt` and `cargo clippy` cleanly; the repo's rustfmt hook runs on `.rs` writes.
- **Nothing is ever destroyed automatically.** Classification only labels. The only destructive code path is `purge_bad_sources`, reached solely by a user button press.
- **Uncertainty never destroys.** Every unknown, unparseable, or failed probe must route to a non-destructive outcome.
- Setting `bad_source_action` has exactly two values: `trash` | `delete`. Default `trash`. No `off`.
- `failure_class` stored strings, exactly: `'bad_source'`, `'bad_source_truncated'`, `'bad_source_purged'`, `'environment'`, `'unknown'`. `Unknown` is **never** NULL — NULL means "row predates this feature".
- `MIN_DECODED_FRACTION = 0.90`.
- `STDERR_TAIL_BYTES` becomes `8192`.
- Schema changes are **additive only** (`ALTER TABLE … ADD COLUMN`), idempotent via the `duplicate column name` check, so an auto-updating install with an existing `convertbar.db` keeps working.
- App-defined `#[tauri::command]`s are ACL-exempt — do **not** edit `src-tauri/capabilities/default.json`.
- Rust tests that hardcode path separators break on Windows; PR CI is ubuntu-only so it only reddens `main` after merge. Normalize separators in assertions.
- Run Rust tests with `cargo test --manifest-path src-tauri/Cargo.toml <filter>`; frontend with `npm test`.
- **Do not modify `clear_completed` or `remove_history_entry`.** They delete bad-source rows along with every other error row, and the spec accepts that deliberately: the review list is a view over history, so clearing history empties it. Adding a carve-out would leave rows behind after the user pressed a button that says it clears errors.

---

## File Structure

| File | Responsibility |
|---|---|
| `src-tauri/src/failure_class.rs` | **New.** Pure classification + truncation policy. No I/O. All table-testable. |
| `src-tauri/src/lib.rs` | Register the new module; register two new commands. |
| `src-tauri/src/db.rs` | Additive `failure_class` column; seed `bad_source_action`; update two count guards. |
| `src-tauri/src/converter.rs` | Readability probe; thread `FailureClass` through both error recorders and all 7 call sites; capture exit code; hoist the stderr join; add the truncation guard; bump tail bytes. |
| `src-tauri/src/types.rs` | `failure_class` on `JobInfo`; `bad_source_action` on `Settings`; `PurgeOutcome` / `PurgeResult`. |
| `src-tauri/src/commands/settings.rs` | Parse + allow the new setting key. |
| `src-tauri/src/commands/queue.rs` | `get_bad_sources`, `purge_bad_sources`; add the column to `row_to_job` and all three SELECT lists. |
| `src/lib/tauri.ts` | Mirror the type changes; wrap the two new commands. |
| `src/hooks/useBadSources.ts` | **New.** Fetch/refresh the review list. |
| `src/pages/HistoryPage.tsx` | Review-list filter, bulk button, confirm step. |
| `src/pages/SettingsPage.tsx` | Radio pair for `bad_source_action`. |

**Task dependency order:** 1 → 2 (pure, no deps) → 3 (schema) → 4 (classification wiring, needs 1+3) → 5 (truncation guard, needs 2+4) → 6 (settings, needs 3) → 7 (commands, needs 3+4) → 8 (UI, needs 6+7).

---

### Task 1: Pure failure classification module

**Files:**
- Create: `src-tauri/src/failure_class.rs`
- Modify: `src-tauri/src/lib.rs:1-10` (module declarations)
- Test: inline `#[cfg(test)] mod tests` in `src-tauri/src/failure_class.rs`

**Interfaces:**
- Consumes: nothing.
- Produces: `pub enum FailureClass { BadSource, Environment, Unknown }` with `pub fn as_str(self) -> &'static str`; `pub struct FailureFacts<'a> { pub exit_code: Option<i32>, pub source_readable: bool, pub stderr_tail: &'a str }`; `pub fn classify(facts: &FailureFacts) -> FailureClass`; `pub const CLASS_BAD_SOURCE: &str`, `CLASS_BAD_SOURCE_TRUNCATED`, `CLASS_BAD_SOURCE_PURGED`.

- [ ] **Step 1: Declare the module**

In `src-tauri/src/lib.rs`, add to the module list (keep alphabetical — it goes after `mod db;`):

```rust
mod failure_class;
```

- [ ] **Step 2: Write the failing tests**

Create `src-tauri/src/failure_class.rs` containing ONLY the test module for now:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    // Real HandBrakeCLI 1.11.2 stderr. This EXACT text is produced by all three of:
    // a zero-byte file, a directory, and a healthy file with mode 000. HandBrake cannot
    // tell them apart — which is why rule 1 exists.
    const SCAN_OPEN_FAILED: &str = r#"Opening subject.mkv...
[10:04:13] hb_scan: path=subject.mkv, title_index=1
[10:04:13] hb_stream_open: open subject.mkv failed
[10:04:13] scan: unrecognized file type
[10:04:13] libhb: scan thread found 0 valid title(s)
No title found.
"#;

    const INVALID_PRESET: &str = r#"[10:04:12] hb_init: starting libhb thread
Invalid preset No Such Preset 9000
Valid presets are:
"#;

    const WORK_RESULT_3: &str = r#"[10:01:06] libhb: work result = 3

Encode failed (error 3).
HandBrake has exited.
"#;

    fn facts<'a>(exit: Option<i32>, readable: bool, tail: &'a str) -> FailureFacts<'a> {
        FailureFacts { exit_code: exit, source_readable: readable, stderr_tail: tail }
    }

    // THE LOAD-BEARING TEST. Identical stderr, identical exit code, opposite verdict.
    // The ONLY difference is our own readability observation. If rule 1 is ever removed
    // or reordered below rule 4, the second assertion flips to BadSource and ConvertBar
    // starts offering healthy files (on a hiccuping mount, a bad permission) for deletion.
    #[test]
    fn readability_is_the_only_thing_separating_corrupt_from_unreadable() {
        assert_eq!(
            classify(&facts(Some(2), true, SCAN_OPEN_FAILED)),
            FailureClass::BadSource,
            "we opened the file ourselves, so HandBrake failing to scan it means the file is bad"
        );
        assert_eq!(
            classify(&facts(Some(2), false, SCAN_OPEN_FAILED)),
            FailureClass::Environment,
            "we could NOT open it, so the identical HandBrake output proves nothing about the file"
        );
    }

    // A preset rename in a HandBrake upgrade exits 2 and would fire on EVERY queued file.
    // Rule 2 must be evaluated before rule 4 or one broken dependency trashes a library.
    #[test]
    fn invalid_preset_is_environment_despite_exit_2() {
        assert_eq!(
            classify(&facts(Some(2), true, INVALID_PRESET)),
            FailureClass::Environment
        );
    }

    #[test]
    fn exit_3_is_always_environment() {
        // Measured for both "output dir missing" and "output dir read-only" with a VALID
        // source. HandBrake signals bad input with 2, never 3.
        assert_eq!(
            classify(&facts(Some(3), true, WORK_RESULT_3)),
            FailureClass::Environment
        );
    }

    #[test]
    fn unrecognized_failures_are_unknown_and_never_destructive() {
        // No markers, exit 1.
        assert_eq!(
            classify(&facts(Some(1), true, "something went sideways")),
            FailureClass::Unknown
        );
        // Killed by a signal — no exit code at all.
        assert_eq!(
            classify(&facts(None, true, "")),
            FailureClass::Unknown
        );
        // Exit 2 but no source marker: not enough evidence to condemn the file.
        assert_eq!(
            classify(&facts(Some(2), true, "some unfamiliar diagnostic")),
            FailureClass::Unknown
        );
        // Exit 0 with an empty output stays Unknown deliberately — the cause of that
        // state is not understood well enough to destroy on it.
        assert_eq!(
            classify(&facts(Some(0), true, SCAN_OPEN_FAILED)),
            FailureClass::Unknown
        );
    }

    #[test]
    fn stored_strings_are_pinned() {
        // These strings are persisted in SQLite and queried by the review list. Changing
        // one silently orphans every existing row.
        assert_eq!(FailureClass::BadSource.as_str(), "bad_source");
        assert_eq!(FailureClass::BadSourceTruncated.as_str(), "bad_source_truncated");
        assert_eq!(FailureClass::Environment.as_str(), "environment");
        assert_eq!(FailureClass::Unknown.as_str(), "unknown");
        assert_eq!(CLASS_BAD_SOURCE, "bad_source");
        assert_eq!(CLASS_BAD_SOURCE_TRUNCATED, "bad_source_truncated");
        assert_eq!(CLASS_BAD_SOURCE_PURGED, "bad_source_purged");
        assert_ne!(
            FailureClass::Unknown.as_str(),
            "",
            "Unknown must never serialize to NULL/empty — NULL means 'predates this feature'"
        );
    }
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test --manifest-path src-tauri/Cargo.toml failure_class`
Expected: FAIL to compile — `cannot find type FailureFacts`, `cannot find function classify`, etc.

- [ ] **Step 4: Write the implementation**

Insert ABOVE the test module in `src-tauri/src/failure_class.rs`:

```rust
//! Pure policy for deciding WHY a conversion failed, so a genuinely bad source file can be
//! told apart from a broken environment. No I/O — every function here is table-testable,
//! mirroring `media_skip.rs`. The caller gathers the facts; this module only decides.
//!
//! The governing rule is the same as the skip policy's: uncertainty is never destructive.

/// Stored `jobs.failure_class` value for a source condemned by rule 4 (HandBrake could not
/// scan a file we ourselves could read).
pub const CLASS_BAD_SOURCE: &str = "bad_source";
/// Stored value for a source condemned by the Phase 2 decode-shortfall guard. Kept distinct
/// from [`CLASS_BAD_SOURCE`] because purge re-scans the latter and must NOT re-scan this one:
/// a truncated file passes a scan by construction, so re-scanning would clear the whole list.
pub const CLASS_BAD_SOURCE_TRUNCATED: &str = "bad_source_truncated";
/// Stored value after the user's bulk action destroyed the source. Keeps the history row
/// visible while dropping it out of the review list.
pub const CLASS_BAD_SOURCE_PURGED: &str = "bad_source_purged";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureClass {
    /// The source file itself is the problem: HandBrake could not scan a file we could read.
    BadSource,
    /// The source decoded to far fewer frames than its container claimed. Distinct from
    /// [`FailureClass::BadSource`] because purge re-scans the latter and must NOT re-scan
    /// this one — a truncated file passes a scan by construction. Never returned by
    /// [`classify`]; only the Phase 2 guard constructs it.
    BadSourceTruncated,
    /// Config, disk, permissions, a missing binary — never the file's fault.
    Environment,
    /// Not enough evidence. Never destructive.
    Unknown,
}

impl FailureClass {
    /// The persisted string. Pinned by test — these values live in SQLite.
    pub fn as_str(self) -> &'static str {
        match self {
            FailureClass::BadSource => CLASS_BAD_SOURCE,
            FailureClass::BadSourceTruncated => CLASS_BAD_SOURCE_TRUNCATED,
            FailureClass::Environment => "environment",
            FailureClass::Unknown => "unknown",
        }
    }
}

/// Everything the caller observed about a failure. `source_readable` is deliberately OUR
/// observation, not HandBrake's: HandBrake emits byte-identical output for a corrupt file
/// and for a healthy file it lacks permission to open.
pub struct FailureFacts<'a> {
    /// `None` when the child was killed or signalled rather than exiting.
    pub exit_code: Option<i32>,
    pub source_readable: bool,
    pub stderr_tail: &'a str,
}

/// Diagnostics that mean the environment failed, never the file. `invalid preset` is here
/// because a HandBrake upgrade that renames a preset exits 2 — the same code as bad input —
/// and would otherwise condemn every file in the queue.
const ENVIRONMENT_MARKERS: [&str; 6] = [
    "invalid preset",
    "no space left",
    "permission denied",
    "not permitted",
    "read-only",
    "cannot create",
];

/// Diagnostics that mean HandBrake could not make sense of the input.
const SOURCE_MARKERS: [&str; 3] = ["unrecognized file type", "no title found", "0 valid title"];

/// Decide why a job failed. First matching rule wins; see the design spec for the rationale
/// behind the ordering (rule 2 MUST precede rule 4).
pub fn classify(facts: &FailureFacts) -> FailureClass {
    // 1. We could not read it ourselves, so HandBrake's verdict proves nothing about the file.
    if !facts.source_readable {
        return FailureClass::Environment;
    }
    let lower = facts.stderr_tail.to_lowercase();
    // 2. An explicit environment diagnostic. Before rule 4 so `Invalid preset` (exit 2)
    //    can never reach the BadSource branch.
    if ENVIRONMENT_MARKERS.iter().any(|m| lower.contains(m)) {
        return FailureClass::Environment;
    }
    // 3. Exit 3 is HB_ERROR_INIT / a libhb work failure — measured for a missing and a
    //    read-only output directory with a VALID source. Bad input is signalled with 2.
    if facts.exit_code == Some(3) {
        return FailureClass::Environment;
    }
    // 4. Exit 2 AND HandBrake could not parse a file we just opened successfully.
    if facts.exit_code == Some(2) && SOURCE_MARKERS.iter().any(|m| lower.contains(m)) {
        return FailureClass::BadSource;
    }
    FailureClass::Unknown
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test --manifest-path src-tauri/Cargo.toml failure_class`
Expected: PASS, 5 tests.

- [ ] **Step 6: Prove the load-bearing test can actually fail**

Temporarily delete the `if !facts.source_readable` block (rule 1) from `classify`. Re-run:

Run: `cargo test --manifest-path src-tauri/Cargo.toml failure_class`
Expected: FAIL on `readability_is_the_only_thing_separating_corrupt_from_unreadable` — the second assertion returns `BadSource` instead of `Environment`.

Then restore rule 1 and re-run to confirm PASS. **Do not commit the neutered version.**

- [ ] **Step 7: Commit**

```bash
git add src-tauri/src/failure_class.rs src-tauri/src/lib.rs
git commit -S -m "feat: add pure failure classification module"
```

---

### Task 2: Decode-shortfall parsing and truncation policy

**Files:**
- Modify: `src-tauri/src/failure_class.rs` (append to both the impl and test sections)

**Interfaces:**
- Consumes: nothing from Task 1 (same file, independent functions).
- Produces: `pub fn decode_shortfall(stderr_tail: &str) -> Option<(u64, u64)>`; `pub fn is_truncated(got: u64, expected: u64) -> bool`; `pub const MIN_DECODED_FRACTION: f64`.

- [ ] **Step 1: Write the failing tests**

Append inside the existing `mod tests` block in `src-tauri/src/failure_class.rs`:

```rust
    // Real tail from a truncated MP4 encode (HandBrakeCLI 1.11.2, exit 0).
    const TRUNCATED_TAIL: &str = r#"[10:01:21] sync: expecting 480 video frames
[10:01:21] h264-decoder done: 131 frames, 1 decoder errors
[10:01:21] sync: got 131 frames, 480 expected
[10:01:21] libhb: work result = 0
"#;

    // Real tail from a truncated MKV encode. Note ZERO decoder errors — a cleanly
    // truncated MKV decodes its available frames without error, so decoder errors must
    // NOT be a required condition or MKV truncation is missed entirely.
    const TRUNCATED_MKV_TAIL: &str = r#"[10:12:15] h264-decoder done: 155 frames, 0 decoder errors
[10:12:15] sync: got 155 frames, 480 expected
"#;

    const HEALTHY_TAIL: &str = r#"[10:01:59] h264-decoder done: 480 frames, 0 decoder errors
[10:01:59] sync: got 480 frames, 480 expected
"#;

    #[test]
    fn parses_the_frame_shortfall_marker() {
        assert_eq!(decode_shortfall(TRUNCATED_TAIL), Some((131, 480)));
        assert_eq!(decode_shortfall(TRUNCATED_MKV_TAIL), Some((155, 480)));
        assert_eq!(decode_shortfall(HEALTHY_TAIL), Some((480, 480)));
    }

    #[test]
    fn absent_or_garbled_marker_is_uncertainty_not_truncation() {
        assert_eq!(decode_shortfall(""), None);
        assert_eq!(decode_shortfall("no marker here at all"), None);
        assert_eq!(decode_shortfall("sync: got many frames, some expected"), None);
        assert_eq!(decode_shortfall("sync: got 131 frames"), None);
    }

    #[test]
    fn takes_the_last_marker_when_several_are_present() {
        // Defensive: multi-pass and subtitle-scan encodes were both checked and emit exactly
        // one line, but a Phase 2 false positive routes a HEALTHY file into the purge list,
        // so a stray earlier line must never decide the verdict.
        let two = "[00:00:01] sync: got 10 frames, 480 expected\n\
                   [00:00:09] sync: got 480 frames, 480 expected\n";
        assert_eq!(decode_shortfall(two), Some((480, 480)));
    }

    #[test]
    fn truncation_threshold_separates_real_cases_with_margin() {
        // (got, expected, expect_truncated, why)
        let cases = [
            (480u64, 480u64, false, "healthy CFR MP4 decodes every frame"),
            (150, 150, false, "healthy VFR — expected comes from sync's own accounting"),
            (131, 480, true, "truncated MP4, 27%"),
            (155, 480, true, "truncated MKV, 32%, zero decoder errors"),
            (593, 2880, true, "truncated 2-min clip, 21%"),
            (0, 0, false, "no frames expected — uncertainty, never truncated"),
            (0, 480, true, "nothing decoded at all"),
            (432, 480, false, "exactly at the 90% floor is not truncated"),
            (431, 480, true, "just past the floor is truncated"),
        ];
        for (got, expected, want, why) in cases {
            assert_eq!(is_truncated(got, expected), want, "{why}");
        }
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --manifest-path src-tauri/Cargo.toml failure_class`
Expected: FAIL to compile — `cannot find function decode_shortfall`, `cannot find function is_truncated`.

- [ ] **Step 3: Write the implementation**

Append to the non-test section of `src-tauri/src/failure_class.rs`:

```rust
/// A source counts as truncated only when it decoded to less than this fraction of the
/// frames HandBrake expected. Measured margin is wide — healthy encodes sit at exactly
/// 1.00, truncated ones at 0.21–0.32 — so 0.90 absorbs container accounting quirks
/// without approaching either cluster.
pub const MIN_DECODED_FRACTION: f64 = 0.90;

/// Parse one `sync: got N frames, M expected` line into `(got, expected)`.
fn parse_sync_line(line: &str) -> Option<(u64, u64)> {
    let rest = line.split_once("sync: got ")?.1;
    let (got, rest) = rest.split_once(" frames, ")?;
    let expected = rest.strip_suffix(" expected")?;
    Some((got.trim().parse().ok()?, expected.trim().parse().ok()?))
}

/// The frame shortfall HandBrake reported, as `(decoded, expected)`.
///
/// Takes the LAST such line in the tail. Returns `None` when the marker is absent or
/// unparseable, which the caller must treat as uncertainty — never as truncation.
pub fn decode_shortfall(stderr_tail: &str) -> Option<(u64, u64)> {
    stderr_tail.lines().filter_map(parse_sync_line).last()
}

/// Whether a decode shortfall is large enough to mean the source is truncated.
///
/// `expected == 0` is uncertainty (nothing to compare against) and is never truncated.
/// Expressed as a multiplication rather than a division so a zero denominator is impossible.
pub fn is_truncated(got: u64, expected: u64) -> bool {
    expected > 0 && (got as f64) < (expected as f64) * MIN_DECODED_FRACTION
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --manifest-path src-tauri/Cargo.toml failure_class`
Expected: PASS, 9 tests.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/failure_class.rs
git commit -S -m "feat: add decode-shortfall parsing and truncation policy"
```

---

### Task 3: Schema column and the new setting default

**Files:**
- Modify: `src-tauri/src/db.rs:150-157` (ALTER block), `:173-190` (defaults), `:252` and `:312` (count guards)
- Test: inline `#[cfg(test)] mod tests` in `src-tauri/src/db.rs`

**Interfaces:**
- Consumes: nothing.
- Produces: `jobs.failure_class TEXT` column; `settings` row `bad_source_action = 'trash'`.

- [ ] **Step 1: Write the failing tests**

Add to `mod tests` in `src-tauri/src/db.rs`:

```rust
    #[test]
    fn init_db_adds_failure_class_column() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        // Writing to the column is the real proof it exists and is TEXT-typed.
        conn.execute(
            "INSERT INTO jobs (id, source_path, output_path, preset, status, failure_class)
             VALUES ('j1', '/a.mkv', '/a.mp4', 'p', 'error', 'bad_source')",
            [],
        )
        .unwrap();
        let got: Option<String> = conn
            .query_row("SELECT failure_class FROM jobs WHERE id = 'j1'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(got.as_deref(), Some("bad_source"));
    }

    #[test]
    fn failure_class_migrates_onto_a_pre_existing_database() {
        // An auto-updating install already has a jobs table without the column. The
        // migration must add it without destroying the row that is already there.
        let conn = Connection::open_in_memory().unwrap();
        conn.execute(
            "CREATE TABLE jobs (
                id TEXT PRIMARY KEY, source_path TEXT NOT NULL, output_path TEXT NOT NULL,
                preset TEXT NOT NULL, status TEXT NOT NULL DEFAULT 'queued'
            )",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO jobs (id, source_path, output_path, preset, status)
             VALUES ('old', '/old.mkv', '/old.mp4', 'p', 'done')",
            [],
        )
        .unwrap();

        init_db(&conn).unwrap();

        let (id, class): (String, Option<String>) = conn
            .query_row("SELECT id, failure_class FROM jobs WHERE id = 'old'", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!(id, "old", "the pre-existing row must survive the migration");
        assert_eq!(
            class, None,
            "rows predating the feature are NULL — distinct from a classified 'unknown'"
        );

        // Idempotent: a second init on the same DB must not error.
        init_db(&conn).unwrap();
    }

    #[test]
    fn bad_source_action_defaults_to_trash() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        assert_eq!(
            setting(&conn, "bad_source_action").as_deref(),
            Some("trash"),
            "the review list's bulk action defaults to the recoverable option; permanent \
             deletion must be chosen deliberately"
        );
    }
```

Also change BOTH existing count assertions from `16` to `17` — `db.rs:252` in `init_db_seeds_defaults` and `db.rs:312` in `init_db_is_idempotent_and_preserves_user_changes`.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --lib db::`
Expected: FAIL — `no such column: failure_class` on the two new column tests, and the count tests fail with `left: 16, right: 17`.

- [ ] **Step 3: Write the implementation**

In `src-tauri/src/db.rs`, immediately AFTER the existing `for col in ["source_size", "source_mtime"]` ALTER loop (which ends around `:157`), add a separate block — the existing loop hardcodes `INTEGER` and cannot carry a TEXT column:

```rust
    // Older DBs predate the failure classification column. Same idempotent pattern as the
    // fingerprint columns above, but TEXT — so it needs its own ALTER rather than a new
    // entry in that INTEGER-typed loop.
    if let Err(e) = conn.execute("ALTER TABLE jobs ADD COLUMN failure_class TEXT", []) {
        if !e.to_string().contains("duplicate column name") {
            return Err(e);
        }
    }
```

Add `failure_class TEXT` to the `CREATE TABLE jobs` statement (`db.rs:79` region) so fresh databases get it directly — place it after the `error_message` column:

```rust
            failure_class   TEXT,
```

Add to the `defaults` array (`db.rs:189` region), after the `low_disk_min_gb` entry:

```rust
        ("bad_source_action", "trash"),
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --lib db::`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/db.rs
git commit -S -m "feat: add failure_class column and bad_source_action setting default"
```

---

### Task 4: Thread classification through the failure paths

**Files:**
- Modify: `src-tauri/src/converter.rs` — add `source_is_readable`; change `record_job_error_quiet` (`:585`) and `record_job_error` (`:611`); update all 7 call sites (`:721`, `:768`, `:792`, `:865`, `:984`, `:1052`, `:1190`); capture the exit code at `:1161`.
- Test: inline `#[cfg(test)] mod tests` in `src-tauri/src/converter.rs`

**Interfaces:**
- Consumes: `failure_class::{FailureClass, FailureFacts, classify}` from Task 1; the `failure_class` column from Task 3.
- Produces: `fn source_is_readable(path: &str) -> bool`; both recorders now take a trailing `class: FailureClass` parameter.

- [ ] **Step 1: Write the failing tests**

Add to `mod tests` in `src-tauri/src/converter.rs`:

```rust
    #[test]
    fn source_is_readable_reports_a_readable_file() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("clip.mkv");
        std::fs::write(&f, b"data").unwrap();
        assert!(source_is_readable(f.to_str().unwrap()));
    }

    #[test]
    fn source_is_readable_fails_safe_on_a_missing_path() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("gone.mkv");
        assert!(
            !source_is_readable(missing.to_str().unwrap()),
            "an unopenable path must report false, which routes to Environment — never destructive"
        );
    }

    #[test]
    fn source_is_readable_reports_false_for_a_directory() {
        let dir = tempfile::tempdir().unwrap();
        assert!(
            !source_is_readable(dir.path().to_str().unwrap()),
            "a directory cannot be read as a file; failing safe is correct"
        );
    }

    // A zero-byte file IS openable — readability is about access, not content. The
    // classifier, not this probe, decides the file is garbage.
    #[test]
    fn source_is_readable_reports_true_for_an_empty_file() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("empty.mkv");
        std::fs::write(&f, b"").unwrap();
        assert!(source_is_readable(f.to_str().unwrap()));
    }

    #[cfg(unix)]
    #[test]
    fn source_is_readable_reports_false_without_read_permission() {
        use std::os::unix::fs::PermissionsExt;
        // Root bypasses mode bits entirely, so this assertion is meaningless as uid 0
        // (rootful docker / `act`). GitHub's ubuntu runner is non-root, so PR CI runs it.
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("locked.mkv");
        std::fs::write(&f, b"data").unwrap();
        std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o000)).unwrap();
        let readable = source_is_readable(f.to_str().unwrap());
        // Restore so tempdir cleanup works regardless of the assertion outcome.
        let _ = std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o644));
        assert!(!readable, "an unreadable healthy file must never be credited as readable");
    }

    fn class_of(db: &Arc<Mutex<Connection>>, id: &str) -> Option<String> {
        db.lock()
            .unwrap()
            .query_row(
                "SELECT failure_class FROM jobs WHERE id = ?1",
                params![id],
                |r| r.get(0),
            )
            .unwrap()
    }

    #[test]
    fn record_job_error_persists_the_failure_class() {
        let app = mock_app();
        let db = test_db();
        queue_job(&db, "j1", "/src.mkv", "/out.mp4", 1000);
        record_job_error(
            app.handle(),
            &db,
            "j1",
            "src.mkv",
            "Conversion failed",
            crate::failure_class::FailureClass::BadSource,
        );
        assert_eq!(class_of(&db, "j1").as_deref(), Some("bad_source"));
    }

    #[test]
    fn record_job_error_quiet_persists_environment_for_a_vanished_source() {
        let app = mock_app();
        let db = test_db();
        queue_job(&db, "j2", "/gone.mkv", "/out.mp4", 1000);
        record_job_error_quiet(
            app.handle(),
            &db,
            "j2",
            "Source file no longer exists",
            crate::failure_class::FailureClass::Environment,
        );
        assert_eq!(
            class_of(&db, "j2").as_deref(),
            Some("environment"),
            "a file that vanished is never the user's corrupt-download problem"
        );
    }
```

These reuse the test module's existing helpers — `mock_app()` (`:1314`), `test_db()` (`:1320`), `queue_job()` (`:1344`). Do not add parallel helpers.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --lib converter::`
Expected: FAIL to compile — `cannot find function source_is_readable`, and the two recorder tests fail on arity (`this function takes 5 arguments but 6 were supplied`).

- [ ] **Step 3: Add the readability probe**

In `src-tauri/src/converter.rs`, next to `source_is_confirmed_missing` (`:451`):

```rust
/// Whether we can actually read `path` ourselves, right now.
///
/// This exists because HandBrake's stderr is byte-identical for a zero-byte file, a
/// directory, and a healthy file we lack permission to open — all exit 2 with
/// "No title found." Believing HandBrake alone would eventually offer good files for
/// deletion. Opening and reading one byte is our own evidence.
///
/// Every failure mode returns `false`, which the classifier routes to `Environment` and
/// therefore never destroys. That is the opposite polarity from
/// [`source_is_confirmed_missing`], which fails open because there the safe answer is
/// "let HandBrake try".
fn source_is_readable(path: &str) -> bool {
    use std::io::Read;
    match std::fs::File::open(path) {
        Ok(mut f) => {
            let mut byte = [0u8; 1];
            // Ok(0) is a legitimately empty but readable file.
            f.read(&mut byte).is_ok()
        }
        Err(_) => false,
    }
}
```

- [ ] **Step 4: Thread the class through both recorders**

Change `record_job_error_quiet` (`:585`) to accept a trailing parameter and persist it:

```rust
fn record_job_error_quiet<R: tauri::Runtime>(
    app: &AppHandle<R>,
    db: &Arc<Mutex<Connection>>,
    job_id: &str,
    err_msg: &str,
    class: crate::failure_class::FailureClass,
) {
```

and change its UPDATE to:

```rust
        let _ = db.execute(
            "UPDATE jobs SET status = 'error', error_message = ?2, completed_at = ?3, \
             failure_class = ?4 WHERE id = ?1",
            params![job_id, err_msg, now, class.as_str()],
        );
```

Change `record_job_error` (`:611`) the same way — add `class: crate::failure_class::FailureClass` as its last parameter and forward it:

```rust
    record_job_error_quiet(app, db, job_id, err_msg, class);
```

- [ ] **Step 5: Update the five statically-known call sites**

These already know structurally what went wrong; no classifier call:

- `:721` vanished source → append `, crate::failure_class::FailureClass::Environment`
- `:768` HandBrakeCLI not found → append `, crate::failure_class::FailureClass::Environment`
- `:792` DB claim failed → append `, crate::failure_class::FailureClass::Environment`
- `:865` spawn failed → append `, crate::failure_class::FailureClass::Environment`
- `:1052` in-place apply failed → append `, crate::failure_class::FailureClass::Environment`

- [ ] **Step 6: Classify at the empty-output site**

At `:984` (inside the `converted_size == 0` guard), replace the `record_job_error(...)` call with:

```rust
                    let class = crate::failure_class::classify(&crate::failure_class::FailureFacts {
                        exit_code: status.code(),
                        source_readable: source_is_readable(&job.source_path),
                        stderr_tail: &tail,
                    });
                    record_job_error(
                        app,
                        db,
                        &job.id,
                        &file_name,
                        &empty_output_error_message(&tail),
                        class,
                    );
```

- [ ] **Step 7: Capture the exit code and classify at the failure arm**

Replace the arm head at `:1161` — currently `Ok(_) | Err(_) => {` — with a bound form:

```rust
            other => {
                let exit_code = match &other {
                    Ok(s) => s.code(),
                    Err(_) => None,
                };
```

Then at `:1190`, replace the `record_job_error(...)` call with:

```rust
                    let class = crate::failure_class::classify(&crate::failure_class::FailureFacts {
                        exit_code,
                        source_readable: source_is_readable(&job.source_path),
                        stderr_tail: &tail,
                    });
                    record_job_error(
                        app,
                        db,
                        &job.id,
                        &file_name,
                        &error_message_from_tail(&tail),
                        class,
                    );
```

- [ ] **Step 8: Run tests to verify they pass**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --lib`
Expected: PASS — the whole lib suite, since the recorder signature change touches many tests.

- [ ] **Step 9: Commit**

```bash
git add src-tauri/src/converter.rs
git commit -S -m "feat: classify conversion failures and persist the verdict"
```

---

### Task 5: Truncation guard on the success path

**Files:**
- Modify: `src-tauri/src/converter.rs:478` (`STDERR_TAIL_BYTES`), `:966` (hoist the join), `:976-992` (new guard after the empty-output guard)
- Test: inline `#[cfg(test)] mod tests` in `src-tauri/src/converter.rs`

**Interfaces:**
- Consumes: `failure_class::{decode_shortfall, is_truncated, CLASS_BAD_SOURCE_TRUNCATED}` from Task 2; `record_job_error` from Task 4.
- Produces: no new public surface.

- [ ] **Step 1: Write the failing tests**

Add to `mod tests` in `src-tauri/src/converter.rs`. Model the fake-HandBrake script on the existing `successful_fake_handbrake_script` (`converter.rs:2631`) — it already writes a non-empty output and exits 0 on both unix and Windows; this variant additionally echoes a truncated-looking stderr tail:

```rust
    /// A stand-in for HandBrakeCLI that writes a small non-empty output, emits a stderr tail
    /// claiming a large frame shortfall, and exits 0 — exactly what a truncated source
    /// produces in reality.
    fn truncating_fake_handbrake_script(dir: &std::path::Path) -> std::path::PathBuf {
        #[cfg(windows)]
        {
            let p = dir.join("hb-trunc.cmd");
            std::fs::write(
                &p,
                "@echo off\r\n\
                 echo [00:00:01] sync: got 131 frames, 480 expected 1>&2\r\n\
                 for %%A in (%*) do set LAST=%%~A\r\n\
                 echo data> \"%LAST%\"\r\n\
                 exit /b 0\r\n",
            )
            .unwrap();
            p
        }
        #[cfg(not(windows))]
        {
            let p = dir.join("hb-trunc.sh");
            std::fs::write(
                &p,
                "#!/bin/sh\n\
                 echo '[00:00:01] sync: got 131 frames, 480 expected' >&2\n\
                 eval \"last=\\${$#}\"\n\
                 printf data > \"$last\"\n\
                 exit 0\n",
            )
            .unwrap();
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
            p
        }
    }

    // The data-loss regression test. Before this guard, HandBrake exiting 0 on a truncated
    // source made ConvertBar record 'done', compute an inflated space_saved, and — under the
    // DEFAULT cleanup_mode='trash' — send the user's ORIGINAL to the Trash.
    #[test]
    fn truncated_encode_errors_and_leaves_the_source_on_disk() {
        let app = mock_app();
        let db = test_db();
        let converter = ConverterState::new();

        let dir = tempfile::tempdir().unwrap();
        let script = truncating_fake_handbrake_script(dir.path());
        set_setting(&db, "handbrake_path", script.to_str().unwrap());
        set_setting(&db, "cleanup_mode", "trash");
        let source = real_source(dir.path(), "movie.mkv");
        let out = dir.path().join("movie.mp4");
        queue_job(
            &db,
            "j1",
            source.to_str().unwrap(),
            out.to_str().unwrap(),
            1000,
        );

        process_queue(app.handle(), &db, &converter);

        let (status, msg) = job_row(&db, "j1");
        assert_eq!(status, "error", "a truncated source is a failure, not a success");
        assert!(
            msg.unwrap_or_default().contains("Source appears truncated"),
            "history must say WHY, not just that it failed"
        );
        assert_eq!(class_of(&db, "j1").as_deref(), Some("bad_source_truncated"));
        assert!(
            source.exists(),
            "THE POINT OF THIS FEATURE: the user's original must still be on disk"
        );
        assert!(!out.exists(), "the short partial output must be removed");
    }

    // For an in-place job output_path IS the source. Removing output_path here would delete
    // the original outright — the same defect class as the fixed auto-resume bug.
    #[test]
    fn truncated_in_place_encode_leaves_the_source_byte_identical() {
        let app = mock_app();
        let db = test_db();
        let converter = ConverterState::new();

        let dir = tempfile::tempdir().unwrap();
        let script = truncating_fake_handbrake_script(dir.path());
        set_setting(&db, "handbrake_path", script.to_str().unwrap());
        set_setting(&db, "cleanup_mode", "trash");
        let source = real_source(dir.path(), "movie.mkv");
        let original_bytes = std::fs::read(&source).unwrap();
        // in-place: output_path == source_path
        let p = source.to_str().unwrap();
        queue_job(&db, "j1", p, p, 1000);

        process_queue(app.handle(), &db, &converter);

        assert_eq!(
            std::fs::read(&source).unwrap(),
            original_bytes,
            "an in-place truncated encode must leave the original untouched — not replaced \
             by the partial temp, and not deleted. Swapping encode_target for \
             job.output_path here destroys the user's file."
        );
        assert!(
            !in_place_temp_path(p).exists(),
            "only the temp is cleaned up"
        );
    }

    #[test]
    fn stderr_tail_window_holds_the_frame_marker_with_room_to_spare() {
        // Measured headroom from EOF: ~1.2 KB with x264, ~305 B with VideoToolbox — but each
        // extra audio track appends a mux: line AFTER the marker. 8192 is the insurance.
        assert!(
            STDERR_TAIL_BYTES >= 8192,
            "shrinking this window can silently disable truncation detection on \
             multi-track files"
        );
    }
```

These reuse the existing helpers `mock_app()`, `test_db()`, `set_setting()`, `queue_job()`, `real_source()` (`:1361`), `job_row()` (`:1367`), and the `class_of()` helper added in Task 4. `zero_byte_output_fails_with_diagnostics_and_the_queue_continues` (`:1770`) is the closest existing model — read it first.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --lib converter::truncat`
Expected: FAIL — status is `done` not `error`, and `stderr_tail_window` fails with `4096 >= 8192` false.

- [ ] **Step 3: Widen the stderr window**

At `src-tauri/src/converter.rs:478`:

```rust
const STDERR_TAIL_BYTES: usize = 8192;
```

- [ ] **Step 4: Hoist the stderr join above the match**

Immediately BEFORE `match exit_status {` at `:966`, insert:

```rust
        // Joined here rather than inside individual arms: the child has already exited, so
        // the drain thread is at EOF and this returns promptly on every path. The success
        // arm needs the tail too (truncation detection), and hoisting also removes the
        // duplicate join the two failure arms previously had.
        let tail = stderr_tail_thread
            .and_then(|t| t.join().ok())
            .unwrap_or_default();
```

Then DELETE the two now-redundant local `let tail = stderr_tail_thread…` bindings inside the arms (previously at `:981` and `:1187`) — the hoisted `tail` is in scope for both.

- [ ] **Step 5: Add the truncation guard**

In the success arm, immediately AFTER the empty-output guard's closing brace (after `:992`) and BEFORE the `let original_size = …` line:

```rust
                // HandBrake exits 0 on a truncated source: it reads the container header,
                // encodes the bytes that are actually there, and reports success. Without
                // this guard the job records 'done', space_saved is computed against the
                // full original size, and cleanup trashes the user's ORIGINAL in favour of
                // a short file. Runs unconditionally — it corrects a wrong answer rather
                // than adding a preference.
                if let Some((got, expected)) = crate::failure_class::decode_shortfall(&tail) {
                    if crate::failure_class::is_truncated(got, expected) {
                        had_errors = true;
                        // encode_target, NEVER job.output_path: for an in-place job
                        // output_path IS the source, so removing it would delete the original.
                        let _ = std::fs::remove_file(&encode_target);
                        let pct = (got as f64 / expected as f64 * 100.0).round() as u64;
                        record_job_error(
                            app,
                            db,
                            &job.id,
                            &file_name,
                            &format!(
                                "Source appears truncated: decoded {got} of {expected} frames ({pct}%)"
                            ),
                            // NOT BadSource: purge re-scans those rows, and a truncated
                            // file passes a scan by construction, so it would be cleared
                            // from the list every time.
                            crate::failure_class::FailureClass::BadSourceTruncated,
                        );
                        continue;
                    }
                }
```

- [ ] **Step 6: Run tests to verify they pass**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --lib`
Expected: PASS, whole lib suite.

- [ ] **Step 7: Commit**

```bash
git add src-tauri/src/converter.rs
git commit -S -m "fix: treat a truncated source as a failure instead of trashing the original"
```

---

### Task 6: Settings plumbing for `bad_source_action`

**Files:**
- Modify: `src-tauri/src/types.rs:21-38` (`Settings`), `src-tauri/src/commands/settings.rs:51`, `:79`, `:103`, `:107-124`, `src/lib/tauri.ts:91` region, `src/pages/SettingsPage.tsx:228-246` region
- Test: `src/pages/SettingsPage.test.tsx`, `src/hooks/useSettings.test.ts`

**Interfaces:**
- Consumes: the seeded default from Task 3.
- Produces: `Settings.bad_source_action: String` (Rust) / `bad_source_action: "trash" | "delete"` (TS).

- [ ] **Step 1: Write the failing Rust test**

Add to `mod tests` in `src-tauri/src/commands/settings.rs`:

```rust
    #[test]
    fn bad_source_action_is_writable_and_unknown_values_fall_back_to_trash() {
        assert!(
            ALLOWED_KEYS.contains(&"bad_source_action"),
            "the Settings UI writes this key via update_setting"
        );
        // Parse fallback: anything that is not exactly "delete" must read as "trash", so a
        // corrupted or future value can never silently upgrade to permanent deletion.
        assert_eq!(normalize_bad_source_action("delete"), "delete");
        assert_eq!(normalize_bad_source_action("trash"), "trash");
        assert_eq!(normalize_bad_source_action(""), "trash");
        assert_eq!(normalize_bad_source_action("DELETE"), "trash");
        assert_eq!(normalize_bad_source_action("nonsense"), "trash");
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --lib settings::`
Expected: FAIL to compile — `cannot find function normalize_bad_source_action`.

- [ ] **Step 3: Implement the Rust side**

Add to `src-tauri/src/commands/settings.rs`:

```rust
/// Coerce a stored `bad_source_action` to a known value. Anything other than an exact
/// "delete" reads as "trash": a corrupted, empty, or future value must never silently
/// escalate to permanent deletion.
pub(crate) fn normalize_bad_source_action(value: &str) -> &'static str {
    if value == "delete" {
        "delete"
    } else {
        "trash"
    }
}
```

Add the field to `Settings` in `src-tauri/src/types.rs`, after `low_disk_min_gb`:

```rust
    pub bad_source_action: String,
```

In `get_settings`, add the local (near `:51`):

```rust
    let mut bad_source_action = String::from("trash");
```

the match arm (near `:79`):

```rust
            "bad_source_action" => bad_source_action = normalize_bad_source_action(&value).to_string(),
```

the struct field in the `Ok(Settings { … })` literal (near `:103`):

```rust
        bad_source_action,
```

and the key in `ALLOWED_KEYS` (near `:123`):

```rust
    "bad_source_action",
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --lib settings::`
Expected: PASS.

- [ ] **Step 5: Write the failing frontend test**

In `src/pages/SettingsPage.test.tsx`, add `bad_source_action: "trash"` to the mock settings object, then add:

```tsx
  it("switches the bad-source action to permanent deletion", async () => {
    render(<SettingsPage />);
    const radio = await screen.findByLabelText(/delete bad source files permanently/i);
    fireEvent.click(radio);
    expect(updateSetting).toHaveBeenCalledWith("bad_source_action", "delete");
  });
```

Also add `bad_source_action: "trash"` to the mock settings in `src/hooks/useSettings.test.ts`.

**Note:** match the existing file's import style and mock-dispatcher shape rather than the illustrative `updateSetting` name above — read the surrounding tests first.

- [ ] **Step 6: Run to verify it fails**

Run: `npm test -- SettingsPage`
Expected: FAIL — unable to find a label matching `/delete bad source files permanently/i`.

- [ ] **Step 7: Implement the UI**

Add to `src/lib/tauri.ts` in the `Settings` interface:

```ts
  bad_source_action: "trash" | "delete";
```

Add a setting group to `src/pages/SettingsPage.tsx`, following the existing `cleanup_mode` radio pattern at `:228`:

```tsx
      <div className="setting-group">
        <span className="setting-label">Bad source files</span>
        <p className="setting-hint">
          Files ConvertBar could not read, or that turned out to be incomplete
          downloads, are listed in History. Nothing is removed until you choose to.
        </p>
        <label className="radio-label">
          <input
            type="radio"
            name="badSource"
            checked={settings.bad_source_action === "trash"}
            onChange={() => updateSetting("bad_source_action", "trash")}
          />
          Move bad source files to Trash
        </label>
        <label className="radio-label">
          <input
            type="radio"
            name="badSource"
            checked={settings.bad_source_action === "delete"}
            onChange={() => updateSetting("bad_source_action", "delete")}
          />
          Delete bad source files permanently
        </label>
      </div>
```

- [ ] **Step 8: Run to verify it passes**

Run: `npm test`
Expected: PASS.

- [ ] **Step 9: Commit**

```bash
git add src-tauri/src/types.rs src-tauri/src/commands/settings.rs src/lib/tauri.ts src/pages/SettingsPage.tsx src/pages/SettingsPage.test.tsx src/hooks/useSettings.test.ts
git commit -S -m "feat: add bad_source_action setting"
```

---

### Task 7: Review-list and purge commands

**Files:**
- Modify: `src-tauri/src/types.rs` (`JobInfo.failure_class`, `PurgeOutcome`, `PurgeResult`), `src-tauri/src/commands/queue.rs:56` (`row_to_job`), `:624`, `:778`, `:795` (SELECT lists), plus the two new commands; `src-tauri/src/lib.rs:69-110` (handler registration)
- Test: inline `#[cfg(test)] mod tests` in `src-tauri/src/commands/queue.rs`

**Interfaces:**
- Consumes: `failure_class::{CLASS_BAD_SOURCE, CLASS_BAD_SOURCE_TRUNCATED, CLASS_BAD_SOURCE_PURGED}`; `file_identity` (`queue.rs:77`); `get_handbrake_path` (`queue.rs:91`); `probe::probe_source`.
- Produces: `#[tauri::command] pub fn get_bad_sources(state) -> Result<Vec<JobInfo>, String>`; `#[tauri::command] pub fn purge_bad_sources(state, ids: Vec<String>) -> Result<Vec<PurgeResult>, String>`; `PurgeOutcome`, `PurgeResult`.

- [ ] **Step 1: Add the types**

In `src-tauri/src/types.rs`, add to `JobInfo` after `error_message`:

```rust
    pub failure_class: Option<String>,
```

and append:

```rust
/// What happened to one id during a bulk purge. Every variant except `Purged` means the
/// file was left alone.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PurgeOutcome {
    /// Destroyed per `bad_source_action`.
    Purged,
    /// A live job still references this path — destroying it would yank the source out
    /// from under a running or queued encode.
    InUse,
    /// The path no longer exists; nothing to do.
    AlreadyGone,
    /// The file at this path is not the one that was classified.
    Changed,
    /// A fresh scan now reads the file fine — the original verdict was a transient fault.
    Recovered,
    /// The delete/trash call itself failed.
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PurgeResult {
    pub id: String,
    pub outcome: PurgeOutcome,
}
```

- [ ] **Step 2: Write the failing tests**

Add to `mod tests` in `src-tauri/src/commands/queue.rs`:

```rust
    #[test]
    fn get_bad_sources_lists_both_bad_classes_and_excludes_everything_else() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::init_db(&conn).unwrap();
        // completed_at is set explicitly: the query orders by it, and NULLs would make the
        // assertion order-dependent on SQLite's row layout.
        for (id, status, class, done_at) in [
            ("a", "error", Some("bad_source"), "2026-07-25T10:00:00Z"),
            ("b", "error", Some("bad_source_truncated"), "2026-07-25T09:00:00Z"),
            ("c", "error", Some("environment"), "2026-07-25T08:00:00Z"),
            ("d", "error", Some("unknown"), "2026-07-25T07:00:00Z"),
            ("e", "error", Some("bad_source_purged"), "2026-07-25T06:00:00Z"),
            ("f", "error", None, "2026-07-25T05:00:00Z"),
            ("g", "done", Some("bad_source"), "2026-07-25T04:00:00Z"),
        ] {
            conn.execute(
                "INSERT INTO jobs (id, source_path, output_path, preset, status, failure_class,
                                   completed_at)
                 VALUES (?1, '/s.mkv', '/o.mp4', 'p', ?2, ?3, ?4)",
                params![id, status, class, done_at],
            )
            .unwrap();
        }
        let ids = bad_source_ids(&conn).unwrap();
        assert_eq!(
            ids,
            vec!["a".to_string(), "b".to_string()],
            "only unpurged bad-source errors belong in the review list: purged rows have been \
             handled, environment/unknown are not the file's fault, NULL predates the feature, \
             and a 'done' row is not a failure at all"
        );
    }

    // Pins the scoping the whole purge safety story depends on, without needing a real
    // HandBrake in the test. A truncated file PASSES a scan (its container header is
    // intact — that is why truncation is invisible at scan time), so re-scanning those
    // rows would report every one of them Recovered and silently empty the list.
    #[test]
    fn only_scan_failure_rows_are_rescanned_before_destruction() {
        assert!(
            should_rescan_before_purge(Some("bad_source")),
            "a scan-failure verdict can be a transient mount fault — re-verify before destroying"
        );
        assert!(
            !should_rescan_before_purge(Some("bad_source_truncated")),
            "a truncated file scans clean, so re-scanning would clear it from the list forever"
        );
        assert!(!should_rescan_before_purge(None));
        assert!(!should_rescan_before_purge(Some("environment")));
    }

    #[test]
    fn purge_skips_a_path_a_live_job_still_needs() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::init_db(&conn).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("movie.mkv");
        std::fs::write(&f, b"content").unwrap();
        let p = f.to_str().unwrap();

        // The bad-source row, plus a re-added copy of the same file now queued.
        insert_error_row(&conn, "old", p, "bad_source");
        conn.execute(
            "INSERT INTO jobs (id, source_path, output_path, preset, status)
             VALUES ('new', ?1, '/o.mp4', 'p', 'queued')",
            params![p],
        )
        .unwrap();

        let outcomes = purge_ids(&conn, &["old".to_string()], "delete").unwrap();
        assert_eq!(outcomes[0].outcome, PurgeOutcome::InUse);
        assert!(f.exists(), "a file a queued job depends on must never be destroyed");
    }

    #[test]
    fn purge_skips_a_file_whose_identity_no_longer_matches() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::init_db(&conn).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("movie.mkv");
        std::fs::write(&f, b"the replacement download").unwrap();
        let p = f.to_str().unwrap();

        insert_error_row(&conn, "old", p, "bad_source");
        // Fingerprint recorded for a DIFFERENT file that used to live at this path.
        conn.execute(
            "UPDATE jobs SET source_size = 999999, source_mtime = 1 WHERE id = 'old'",
            [],
        )
        .unwrap();

        let outcomes = purge_ids(&conn, &["old".to_string()], "delete").unwrap();
        assert_eq!(outcomes[0].outcome, PurgeOutcome::Changed);
        assert!(f.exists(), "a stale verdict must not condemn a re-downloaded file");
    }

    #[test]
    fn purge_reports_already_gone_without_failing() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::init_db(&conn).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("never-existed.mkv");
        insert_error_row(&conn, "old", missing.to_str().unwrap(), "bad_source");

        let outcomes = purge_ids(&conn, &["old".to_string()], "delete").unwrap();
        assert_eq!(outcomes[0].outcome, PurgeOutcome::AlreadyGone);
    }

    #[test]
    fn purged_rows_leave_the_list_but_stay_in_history() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::init_db(&conn).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("movie.mkv");
        std::fs::write(&f, b"garbage").unwrap();
        let p = f.to_str().unwrap();
        insert_error_row(&conn, "old", p, "bad_source_truncated");
        stamp_identity(&conn, "old", p);

        let outcomes = purge_ids(&conn, &["old".to_string()], "delete").unwrap();
        assert_eq!(outcomes[0].outcome, PurgeOutcome::Purged);
        assert!(!f.exists(), "delete mode removes the file");
        assert!(
            bad_source_ids(&conn).unwrap().is_empty(),
            "a purged row must drop out of the list or a second press just errors"
        );
        let still_there: i64 = conn
            .query_row("SELECT COUNT(*) FROM jobs WHERE id = 'old'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(still_there, 1, "the history entry itself survives");
    }
```

Add these two helpers to the same test module:

```rust
    fn insert_error_row(conn: &Connection, id: &str, path: &str, class: &str) {
        conn.execute(
            "INSERT INTO jobs (id, source_path, output_path, preset, status, failure_class,
                               completed_at)
             VALUES (?1, ?2, '/o.mp4', 'p', 'error', ?3, '2026-07-25T10:00:00Z')",
            params![id, path, class],
        )
        .unwrap();
    }

    /// Record the CURRENT on-disk fingerprint so the purge identity check passes. Without
    /// this the row looks like a pre-feature NULL-fingerprint row and purge refuses.
    fn stamp_identity(conn: &Connection, id: &str, path: &str) {
        let ident = file_identity(path).expect("file exists");
        conn.execute(
            "UPDATE jobs SET source_size = ?2, source_mtime = ?3 WHERE id = ?1",
            params![id, ident.size, ident.mtime],
        )
        .unwrap();
    }
```

Note that `purge_skips_a_path_a_live_job_still_needs` and `purge_reports_already_gone_without_failing` deliberately do **not** call `stamp_identity` — they assert outcomes that are reached before the identity check, so stamping would not change the result.

- [ ] **Step 3: Run to verify they fail**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --lib queue::`
Expected: FAIL to compile — `cannot find function bad_source_ids`, `cannot find function purge_ids`.

- [ ] **Step 4: Implement the query and purge core**

Add to `src-tauri/src/commands/queue.rs`. Note the core logic is split from the `#[tauri::command]` wrappers so it takes a plain `&Connection` and is directly unit-testable:

```rust
use crate::failure_class::{CLASS_BAD_SOURCE, CLASS_BAD_SOURCE_PURGED, CLASS_BAD_SOURCE_TRUNCATED};
use crate::types::{PurgeOutcome, PurgeResult};

/// Ids of the rows the review list shows: failures blamed on the source file that the user
/// has not already dealt with.
fn bad_source_ids(conn: &Connection) -> Result<Vec<String>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT id FROM jobs
             WHERE status = 'error' AND failure_class IN (?1, ?2)
             ORDER BY completed_at DESC",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params![CLASS_BAD_SOURCE, CLASS_BAD_SOURCE_TRUNCATED], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<Vec<String>, _>>()
        .map_err(|e| e.to_string())
}

/// Whether a row's verdict should be re-verified with a fresh scan before its file is
/// destroyed.
///
/// Only scan-failure rows qualify. A truncated source passes a scan by construction — its
/// container header is intact, which is exactly why truncation cannot be seen at scan time —
/// so re-scanning those rows would report every one of them recovered and silently empty
/// the review list.
fn should_rescan_before_purge(class: Option<&str>) -> bool {
    class == Some(CLASS_BAD_SOURCE)
}

/// Whether any live job still points at `path`.
fn path_is_in_use(conn: &Connection, path: &str) -> bool {
    conn.query_row(
        "SELECT COUNT(*) FROM jobs
         WHERE source_path = ?1 AND status IN ('queued', 'encoding', 'paused')",
        params![path],
        |r| r.get::<_, i64>(0),
    )
    .map(|n| n > 0)
    .unwrap_or(true) // a failed check means "assume in use" — never destroy on uncertainty
}

/// Decide and act for one id. `action` is the stored `bad_source_action`.
fn purge_one(conn: &Connection, id: &str, action: &str) -> PurgeOutcome {
    let row: Result<(String, Option<String>, Option<i64>, Option<i64>), _> = conn.query_row(
        "SELECT source_path, failure_class, source_size, source_mtime FROM jobs WHERE id = ?1",
        params![id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    );
    let (path, class, size, mtime) = match row {
        Ok(v) => v,
        Err(_) => return PurgeOutcome::Failed,
    };

    if path_is_in_use(conn, &path) {
        return PurgeOutcome::InUse;
    }
    if !std::path::Path::new(&path).exists() {
        return PurgeOutcome::AlreadyGone;
    }

    // Identity: the (size, mtime) fingerprint the codebase already keeps. A replacement
    // file of coincidentally identical size still fails on mtime.
    match (file_identity(&path), size, mtime) {
        (Some(current), Some(s), Some(m)) => {
            if current.size != s || current.mtime != m {
                return PurgeOutcome::Changed;
            }
        }
        // Cannot stat it now — refuse rather than guess.
        (None, _, _) => return PurgeOutcome::Changed,
        // Pre-feature row with no fingerprint: nothing to verify against, so leave it.
        _ => return PurgeOutcome::Changed,
    }

    // A scan that now succeeds means the original verdict was a transient environment fault
    // (a mount that hiccuped mid-scan), not a bad file.
    if should_rescan_before_purge(class.as_deref()) {
        if let Ok(hb) = get_handbrake_path(conn) {
            if crate::probe::probe_source(&hb, &path).is_some() {
                return PurgeOutcome::Recovered;
            }
        }
    }

    let destroyed = if action == "delete" {
        std::fs::remove_file(&path).is_ok()
    } else {
        trash::delete(&path).is_ok()
    };
    if !destroyed {
        return PurgeOutcome::Failed;
    }

    let _ = conn.execute(
        "UPDATE jobs SET failure_class = ?2 WHERE id = ?1",
        params![id, CLASS_BAD_SOURCE_PURGED],
    );
    PurgeOutcome::Purged
}

fn purge_ids(conn: &Connection, ids: &[String], action: &str) -> Result<Vec<PurgeResult>, String> {
    Ok(ids
        .iter()
        .map(|id| PurgeResult { id: id.clone(), outcome: purge_one(conn, id, action) })
        .collect())
}
```

- [ ] **Step 5: Add the command wrappers**

```rust
#[tauri::command]
pub fn get_bad_sources(state: State<'_, AppState>) -> Result<Vec<JobInfo>, String> {
    let conn = state.db.lock().map_err(|e| e.to_string())?;
    let mut stmt = conn
        .prepare(
            "SELECT id, source_path, output_path, preset, status, original_size, converted_size,
                    kept_file, space_saved, error_message, failure_class, queue_order, created_at,
                    completed_at
             FROM jobs
             WHERE status = 'error' AND failure_class IN (?1, ?2)
             ORDER BY completed_at DESC",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params![CLASS_BAD_SOURCE, CLASS_BAD_SOURCE_TRUNCATED], row_to_job)
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<Vec<JobInfo>, _>>()
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn purge_bad_sources(
    state: State<'_, AppState>,
    ids: Vec<String>,
) -> Result<Vec<PurgeResult>, String> {
    let conn = state.db.lock().map_err(|e| e.to_string())?;
    let action: String = conn
        .query_row(
            "SELECT value FROM settings WHERE key = 'bad_source_action'",
            params![],
            |r| r.get(0),
        )
        .unwrap_or_else(|_| "trash".to_string());
    let action = crate::commands::settings::normalize_bad_source_action(&action);
    purge_ids(&conn, &ids, action)
}
```

- [ ] **Step 6: Add the column to `row_to_job` and all three SELECT lists**

In `row_to_job` (`queue.rs:56`), insert `failure_class` reading at index 10 (right after `error_message`) and shift the remaining indices by one:

```rust
        failure_class: row.get(10)?,
        queue_order: row.get(11)?,
        created_at: row.get(12)?,
        completed_at: row.get(13)?,
```

Then add `failure_class,` after `error_message,` in **all three** SELECT column lists — `queue.rs:624`, `:778`, `:795`. Missing one is a runtime column-count panic, not a compile error.

Do the same for any other `row_to_job`-shaped construction in `converter.rs` (there is one around `:356-373`) — add `failure_class` to both its SELECT and its struct literal.

- [ ] **Step 7: Register the commands**

In `src-tauri/src/lib.rs`, add to `generate_handler!` after `commands::queue::clear_queue`:

```rust
            commands::queue::get_bad_sources,
            commands::queue::purge_bad_sources,
```

- [ ] **Step 8: Run to verify they pass**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --lib`
Expected: PASS.

- [ ] **Step 9: Commit**

```bash
git add src-tauri/src/types.rs src-tauri/src/commands/queue.rs src-tauri/src/converter.rs src-tauri/src/lib.rs
git commit -S -m "feat: add bad-source review list and purge commands"
```

---

### Task 8: Review-list UI

> **[Markup superseded by PR #114.]** The row layout specified below shipped and was then
> replaced: rows now stack, both halves ellipsize, and the reason is rendered from the
> `failure_class` enum rather than showing raw stderr. Read `src/pages/HistoryPage.tsx` for
> the current UI — the structural steps below no longer describe it. The hook, commands, and
> data flow (Tasks 6–7) are unchanged and did ship as written.

**Files:**
- Create: `src/hooks/useBadSources.ts`, `src/hooks/useBadSources.test.ts`
- Modify: `src/lib/tauri.ts` (types + command wrappers), `src/pages/HistoryPage.tsx`
- Test: `src/pages/HistoryPage.test.tsx`

**Interfaces:**
- Consumes: `get_bad_sources`, `purge_bad_sources` from Task 7; `bad_source_action` from Task 6.
- Produces: `useBadSources()` returning `{ badSources, refresh, purge }`.

- [ ] **Step 1: Add the TS types and command wrappers**

In `src/lib/tauri.ts`, add to `JobInfo` after `error_message`:

```ts
  failure_class: string | null;
```

and append the types plus wrappers:

```ts
export type PurgeOutcome =
  | "purged"
  | "in_use"
  | "already_gone"
  | "changed"
  | "recovered"
  | "failed";

export interface PurgeResult {
  id: string;
  outcome: PurgeOutcome;
}
```

In the `commands` object:

```ts
  getBadSources: () => invoke<JobInfo[]>("get_bad_sources"),
  purgeBadSources: (ids: string[]) =>
    invoke<PurgeResult[]>("purge_bad_sources", { ids }),
```

- [ ] **Step 2: Write the failing hook test**

Create `src/hooks/useBadSources.test.ts`:

```ts
import { describe, it, expect, vi, beforeEach } from "vitest";
import { renderHook, waitFor, act } from "@testing-library/react";
import { useBadSources } from "./useBadSources";
import { commands } from "../lib/tauri";

vi.mock("../lib/tauri", () => ({
  commands: {
    getBadSources: vi.fn(),
    purgeBadSources: vi.fn(),
  },
}));

const row = (id: string, failure_class: string) =>
  ({ id, source_path: `/m/${id}.mkv`, failure_class }) as never;

beforeEach(() => vi.resetAllMocks());

describe("useBadSources", () => {
  it("loads the list on mount", async () => {
    vi.mocked(commands.getBadSources).mockResolvedValue([
      row("a", "bad_source"),
      row("b", "bad_source_truncated"),
    ]);
    const { result } = renderHook(() => useBadSources());
    await waitFor(() => expect(result.current.badSources).toHaveLength(2));
  });

  it("refetches after a purge so handled rows disappear", async () => {
    vi.mocked(commands.getBadSources)
      .mockResolvedValueOnce([row("a", "bad_source")])
      .mockResolvedValueOnce([]);
    vi.mocked(commands.purgeBadSources).mockResolvedValue([
      { id: "a", outcome: "purged" },
    ]);

    const { result } = renderHook(() => useBadSources());
    await waitFor(() => expect(result.current.badSources).toHaveLength(1));

    await act(async () => {
      await result.current.purge(["a"]);
    });

    expect(commands.purgeBadSources).toHaveBeenCalledWith(["a"]);
    await waitFor(() => expect(result.current.badSources).toHaveLength(0));
  });

  it("surfaces outcomes so skipped files can be reported, not silently ignored", async () => {
    vi.mocked(commands.getBadSources).mockResolvedValue([row("a", "bad_source")]);
    vi.mocked(commands.purgeBadSources).mockResolvedValue([
      { id: "a", outcome: "recovered" },
    ]);
    const { result } = renderHook(() => useBadSources());
    await waitFor(() => expect(result.current.badSources).toHaveLength(1));

    let outcomes;
    await act(async () => {
      outcomes = await result.current.purge(["a"]);
    });
    expect(outcomes).toEqual([{ id: "a", outcome: "recovered" }]);
  });
});
```

- [ ] **Step 3: Run to verify it fails**

Run: `npm test -- useBadSources`
Expected: FAIL — cannot resolve `./useBadSources`.

- [ ] **Step 4: Implement the hook**

Create `src/hooks/useBadSources.ts`:

```ts
import { useState, useEffect, useCallback } from "react";
import { commands, type JobInfo, type PurgeResult } from "../lib/tauri";

export function useBadSources() {
  const [badSources, setBadSources] = useState<JobInfo[]>([]);

  const refresh = useCallback(async () => {
    try {
      setBadSources(await commands.getBadSources());
    } catch (e) {
      console.error("Failed to load bad sources:", e);
    }
  }, []);

  const purge = useCallback(
    async (ids: string[]): Promise<PurgeResult[]> => {
      const outcomes = await commands.purgeBadSources(ids);
      await refresh();
      return outcomes;
    },
    [refresh],
  );

  useEffect(() => {
    refresh();
  }, [refresh]);

  return { badSources, refresh, purge };
}
```

- [ ] **Step 5: Run to verify it passes**

Run: `npm test -- useBadSources`
Expected: PASS, 3 tests.

- [ ] **Step 6: Write the failing page test**

Add to `src/pages/HistoryPage.test.tsx` (extend the existing mock dispatcher so `get_bad_sources` returns one row and `get_settings` returns `bad_source_action: "trash"`):

```tsx
  it("shows the bad-source banner and requires a confirm before destroying", async () => {
    render(<HistoryPage />);

    const banner = await screen.findByText(/bad sources \(1\)/i);
    expect(banner).toBeTruthy();

    // First press only arms the confirm — nothing is destroyed yet.
    fireEvent.click(screen.getByRole("button", { name: /move 1 to trash/i }));
    expect(purgeBadSources).not.toHaveBeenCalled();

    fireEvent.click(screen.getByRole("button", { name: /confirm/i }));
    await waitFor(() => expect(purgeBadSources).toHaveBeenCalledWith(["a"]));
  });

  it("hides the banner entirely when there are no bad sources", async () => {
    getBadSources.mockResolvedValue([]);
    render(<HistoryPage />);
    await waitFor(() => expect(getBadSources).toHaveBeenCalled());
    expect(screen.queryByText(/bad sources/i)).toBeNull();
  });
```

- [ ] **Step 7: Run to verify it fails**

Run: `npm test -- HistoryPage`
Expected: FAIL — unable to find text `/bad sources \(1\)/i`.

- [ ] **Step 8: Implement the UI**

In `src/pages/HistoryPage.tsx`, import the hook and settings, then render the panel above `history-controls`:

```tsx
  const { badSources, purge } = useBadSources();
  const { settings } = useSettings();
  const [confirming, setConfirming] = useState(false);
  const [outcomeNote, setOutcomeNote] = useState<string | null>(null);

  // useSettings returns `AppSettings | null` while loading, so the optional chain is
  // required — and a null settings object must read as the non-destructive default.
  const destructive = settings?.bad_source_action === "delete";
  const actionLabel = destructive
    ? `Delete ${badSources.length} permanently`
    : `Move ${badSources.length} to Trash`;

  const runPurge = async () => {
    const results = await purge(badSources.map((j) => j.id));
    setConfirming(false);
    const skipped = results.filter((r) => r.outcome !== "purged");
    setOutcomeNote(
      skipped.length === 0
        ? null
        : `${skipped.length} file(s) were left alone: ${skipped
            .map((r) => r.outcome.replace(/_/g, " "))
            .join(", ")}`,
    );
  };
```

```tsx
      {badSources.length > 0 && (
        <div className="bad-sources-panel">
          <span className="bad-sources-title">
            Bad sources ({badSources.length})
          </span>
          <ul className="bad-sources-list">
            {badSources.map((job) => (
              <li key={job.id}>
                <span className="bad-sources-name">
                  {job.source_path.split(/[/\\]/).pop()}
                </span>
                <span className="bad-sources-reason">
                  {(job.error_message ?? "").split("\n")[0]}
                </span>
              </li>
            ))}
          </ul>
          {!confirming ? (
            <button className="btn btn-small" onClick={() => setConfirming(true)}>
              {actionLabel}
            </button>
          ) : (
            <div className="bad-sources-confirm">
              <span>
                {destructive
                  ? "This cannot be undone."
                  : "Files move to your Trash."}
              </span>
              <button className="btn btn-small btn-danger" onClick={runPurge}>
                Confirm
              </button>
              <button className="btn btn-small" onClick={() => setConfirming(false)}>
                Cancel
              </button>
            </div>
          )}
          {outcomeNote && <p className="setting-hint">{outcomeNote}</p>}
        </div>
      )}
```

`HistoryPage` does not currently read settings, so add `import { useSettings } from "../hooks/useSettings";` alongside the `useBadSources` import. Extend the page test's mock dispatcher to answer `get_settings` — otherwise `useSettings` rejects and `settings` stays null, which renders the Trash wording regardless of the configured action.

- [ ] **Step 9: Run the full suite**

Run: `npm test && cargo test --manifest-path src-tauri/Cargo.toml`
Expected: PASS on both. The `ipc-contract` test must pass without modification — it discovers the two new commands automatically.

- [ ] **Step 10: Commit**

```bash
git add src/hooks/useBadSources.ts src/hooks/useBadSources.test.ts src/lib/tauri.ts src/pages/HistoryPage.tsx src/pages/HistoryPage.test.tsx
git commit -S -m "feat: add bad-source review list to History"
```

---

## Final verification

- [ ] **Run everything**

```bash
npm run build
npm test
cargo test --manifest-path src-tauri/Cargo.toml
cargo fmt --manifest-path src-tauri/Cargo.toml --check
cargo clippy --manifest-path src-tauri/Cargo.toml -- -D warnings
```

- [ ] **Confirm `capabilities/default.json` is untouched**

```bash
git diff --stat origin/main..HEAD -- src-tauri/capabilities/
```

Expected: empty. Both new commands are app-defined and therefore ACL-exempt.

- [ ] **Manual smoke test** — build a truncated file and run it through the real app:

```bash
ffmpeg -y -f lavfi -i testsrc=duration=60:size=640x480:rate=24 \
  -pix_fmt yuv420p -c:v libx264 -movflags +faststart /tmp/full.mp4
SZ=$(stat -f%z /tmp/full.mp4)
dd if=/tmp/full.mp4 of=/tmp/truncated.mp4 bs=1 count=$((SZ*30/100))
```

Drop `/tmp/truncated.mp4` into ConvertBar with `cleanup_mode = trash`. Expected: the job ends in **error** with "Source appears truncated", `/tmp/truncated.mp4` is **still on disk and not in the Trash**, and History shows **Bad sources (1)**.
