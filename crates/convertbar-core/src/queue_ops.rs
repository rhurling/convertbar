use rusqlite::params;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::converter::IN_PLACE_TEMP_MARKER;
use crate::ctx::Ctx;
use crate::dispose::FileDisposer;
use crate::events::EventSinkExt;
use crate::failure_class::{
    CLASS_BAD_SOURCE, CLASS_BAD_SOURCE_PURGED, CLASS_BAD_SOURCE_RECOVERED,
    CLASS_BAD_SOURCE_TRUNCATED,
};
use crate::handbrake;
use crate::types::{
    AddResult, ClassifiedPaths, FolderScanResult, HistoryPage, HistorySummary, JobInfo,
    PurgeOutcome, PurgeResult, SkipCount, SkipReason,
};

const VIDEO_EXTENSIONS: &[&str] = &[
    "mp4", "mkv", "avi", "mov", "wmv", "flv", "webm", "m4v", "ts",
];

pub fn is_video_file(path: &Path) -> bool {
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

pub fn scan_video_files(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                files.extend(scan_video_files(&path));
            } else if is_video_file(&path) {
                files.push(path);
            }
        }
    }
    files
}

fn get_next_queue_order(conn: &rusqlite::Connection) -> Result<i32, String> {
    conn.query_row(
        "SELECT COALESCE(MAX(queue_order), 0) + 1 FROM jobs WHERE status IN ('queued', 'encoding', 'paused')",
        [],
        |row| row.get(0),
    )
    .map_err(|e| e.to_string())
}

fn row_to_job(row: &rusqlite::Row) -> rusqlite::Result<JobInfo> {
    Ok(JobInfo {
        id: row.get(0)?,
        source_path: row.get(1)?,
        output_path: row.get(2)?,
        preset: row.get(3)?,
        status: row.get(4)?,
        original_size: row.get(5)?,
        converted_size: row.get(6)?,
        kept_file: row.get(7)?,
        space_saved: row.get(8)?,
        error_message: row.get(9)?,
        failure_class: row.get(10)?,
        queue_order: row.get(11)?,
        created_at: row.get(12)?,
        completed_at: row.get(13)?,
    })
}

/// Reads the configured `handbrake_path` setting, if any. DB-only — no filesystem or subprocess
/// work — so this is safe to call under the mutex.
fn read_configured_handbrake_path(conn: &rusqlite::Connection) -> Option<String> {
    conn.query_row(
        "SELECT value FROM settings WHERE key = 'handbrake_path'",
        params![],
        |row| row.get(0),
    )
    .ok()
}

/// Takes the locator as a parameter rather than a `&Ctx`: this runs *under* the DB guard at its
/// `add_files_inner` call site, and a `&Ctx`-taking resolver would invite re-locking the
/// non-reentrant `ctx.db` mutex there.
fn get_handbrake_path(
    conn: &rusqlite::Connection,
    locator: &dyn handbrake::HandbrakeLocator,
) -> Result<String, String> {
    handbrake::resolve_with_locator(read_configured_handbrake_path(conn).as_deref(), locator)
        .ok_or_else(|| "HandBrakeCLI not found".to_string())
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

/// Canonicalize `path` for the in-use comparison, falling back to the raw path unchanged when
/// canonicalization fails (e.g. a dead mount, or a path that no longer resolves) — a failure
/// here must never make a live job's path silently drop out of the comparison.
fn canonical_or_raw(path: &str) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| PathBuf::from(path))
}

/// The source paths of every currently-live job (queued/encoding/paused), or `None` if the
/// lookup itself failed. Shared by both in-use comparisons below.
fn live_job_paths(conn: &rusqlite::Connection) -> Option<Vec<String>> {
    let mut stmt = conn
        .prepare("SELECT source_path FROM jobs WHERE status IN ('queued', 'encoding', 'paused')")
        .ok()?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(0)).ok()?;
    Some(rows.flatten().collect())
}

/// Whether any live job still points at `path`, by exact string comparison — no filesystem
/// access (`canonicalize` is a blocking syscall; see `path_is_in_use_canonical` below). Used by
/// phase 1 (`quick_db_check`), which runs entirely under the DB mutex and must therefore stay
/// filesystem-free (R2) even when `path` or a live job's source lives on a dead/hung mount.
///
/// A raw-string match can miss a live job recorded under a different spelling of the SAME file
/// (a case-insensitive filesystem, `/tmp` vs `/private/tmp`, a symlinked watched directory). That
/// false negative is deliberate and safe: it only lets a candidate proceed to later rungs, where
/// phase 3's `path_is_in_use_canonical` — the check that actually authorizes destruction — still
/// catches it. It can never falsely block a legitimate purge.
fn path_is_in_use_raw(conn: &rusqlite::Connection, path: &str) -> bool {
    match live_job_paths(conn) {
        // a failed check means "assume in use" — never destroy on uncertainty
        None => true,
        Some(paths) => paths.iter().any(|p| p == path),
    }
}

/// Whether any live job still points at `path`. Compares canonicalized paths, not raw strings:
/// an exact-string match misses a case-insensitive filesystem (macOS/Windows default), `/tmp`
/// vs `/private/tmp`, or a symlinked watched directory — any of which would let a live job's
/// source look "free" to purge under a different spelling of the same file.
///
/// `canonicalize` is a blocking syscall, so this must only run where that cost is acceptable:
/// phase 3, the FINAL re-verify immediately before destroying, where `purge_one_locked`'s
/// earlier, unlocked phase 2 has already proven the mount responsive (see its doc comment).
/// Phase 1 (`quick_db_check`) uses `path_is_in_use_raw` instead — see R2 there.
fn path_is_in_use_canonical(conn: &rusqlite::Connection, path: &str) -> bool {
    match live_job_paths(conn) {
        None => true,
        Some(paths) => {
            let target = canonical_or_raw(path);
            paths.iter().any(|p| canonical_or_raw(p) == target)
        }
    }
}

/// Outcome of the DB-only part of the ladder: eligibility (folded into the row lookup),
/// InUse, AlreadyGone, Changed, and — for scan-failure rows — the rung-4 rescan decision.
/// Touches the filesystem only via `Path::exists`/`file_identity`; never destroys anything.
enum PreDestroy {
    /// A final, non-destructive verdict. Nothing more to do.
    Stop(PurgeOutcome),
    /// A rescan is required for this row's class. When `pre_destroy_check` runs as the FINAL
    /// re-verify (`purge_one_locked`'s only caller), the scan already ran in phase 2 and
    /// confirmed `RescanVerdict::Destroy` — this variant just means "every DB-side fact still
    /// holds," not "run the scan (again)". No `handbrake_path` is carried: nothing here needs it.
    NeedsScan { path: String },
    /// Everything passed and no rescan is required (a `bad_source_truncated` row, whose
    /// verdict does not depend on a scan) — safe to destroy `path`.
    ReadyToDestroy { path: String },
}

/// The row facts the ladder needs about a candidate id, before any filesystem or scan work. A
/// named struct instead of a raw tuple — clippy's `type_complexity` lint flagged the previous
/// `(String, Option<String>, Option<i64>, Option<i64>)`.
struct BadSourceRow {
    path: String,
    class: Option<String>,
    size: Option<i64>,
    mtime: Option<i64>,
}

/// Eligibility lookup: the id must be a live, unpurged bad-source error row (right status, right
/// failure_class). An id that fails this (wrong status, wrong/absent failure_class, or simply
/// nonexistent) matches no row — the UI is expected to only ever pass ids from the review list,
/// but a wiring mistake (e.g. passing a History row's id) must never be able to reach a live
/// `done`/`queued` row. DB-only: no filesystem I/O.
fn lookup_bad_source_row(conn: &rusqlite::Connection, id: &str) -> Option<BadSourceRow> {
    conn.query_row(
        "SELECT source_path, failure_class, source_size, source_mtime FROM jobs
         WHERE id = ?1 AND status = 'error' AND failure_class IN (?2, ?3)",
        params![id, CLASS_BAD_SOURCE, CLASS_BAD_SOURCE_TRUNCATED],
        |r| {
            Ok(BadSourceRow {
                path: r.get(0)?,
                class: r.get(1)?,
                size: r.get(2)?,
                mtime: r.get(3)?,
            })
        },
    )
    .ok()
}

/// FS-only verdict for a source path against its stored `(size, mtime)` fingerprint. No DB
/// access, so this can run with the mutex released — see `purge_one_locked`'s doc comment for
/// why that split matters (a dead SMB/NFS mount can block a stat for 30-60s).
///
/// Uses `try_exists`, NOT `exists` (which collapses every stat error — a Windows ACL denial, an
/// EIO on a half-broken mount — into a bare `false`). An error here proves nothing about whether
/// the file exists, so it must route to `Unverifiable`, never be misread as "deleted" and get the
/// row permanently stamped. Mirrors `converter::source_is_confirmed_missing`'s convention.
fn verify_source_identity_on_disk(
    path: &str,
    size: Option<i64>,
    mtime: Option<i64>,
) -> Result<(), PurgeOutcome> {
    match Path::new(path).try_exists() {
        Ok(true) => {}
        Ok(false) => {
            // The path itself being missing is ambiguous: an unplugged external/network volume
            // makes every path under it look identical to "the file was deleted" — `try_exists`
            // reads `Ok(false)` either way. Only treat this as AlreadyGone when the PARENT
            // directory is confirmed reachable, which positively attributes the absence to the
            // file itself rather than to the whole volume being offline.
            let parent_reachable = Path::new(path)
                .parent()
                .is_some_and(|p| matches!(p.try_exists(), Ok(true)));
            return Err(if parent_reachable {
                PurgeOutcome::AlreadyGone
            } else {
                PurgeOutcome::Unverifiable
            });
        }
        Err(_) => return Err(PurgeOutcome::Unverifiable),
    }

    // Identity: the (size, mtime) fingerprint the codebase already keeps. A replacement file
    // of coincidentally identical size still fails on mtime. Anything short of a PROVEN match
    // — a current stat failure, or a stored NULL fingerprint (pre-feature row, or a source
    // that was itself unstattable at add time) — is treated as a mismatch: there is nothing to
    // verify against, so purge refuses rather than guess.
    let identity_matches = matches!(
        (crate::probe_cache::file_identity(path), size, mtime),
        (Some(current), Some(s), Some(m)) if current.size == s && current.mtime == m
    );
    if !identity_matches {
        return Err(PurgeOutcome::Changed);
    }
    Ok(())
}

/// Rungs 1-3 of the ladder plus the rung-4 rescan decision, against a single already-acquired
/// connection. This is the FINAL re-verify immediately before destroying, run under the
/// re-acquired lock in `purge_one_locked`'s phase 3 — where the FS re-stat here is cheap because
/// phase 2's earlier, unlocked pass already proved the mount responsive, and where the in-use
/// check is the canonicalized one (`path_is_in_use_canonical`), since this is the rung that
/// actually authorizes destruction. `handbrake_path` is the caller's already-resolved value (see
/// R3 in `purge_bad_sources`'s doc comment) — never re-detected here.
fn pre_destroy_check(
    conn: &rusqlite::Connection,
    id: &str,
    handbrake_path: &Result<String, String>,
) -> PreDestroy {
    let BadSourceRow {
        path,
        class,
        size,
        mtime,
    } = match lookup_bad_source_row(conn, id) {
        Some(v) => v,
        None => return PreDestroy::Stop(PurgeOutcome::Failed),
    };

    if path_is_in_use_canonical(conn, &path) {
        return PreDestroy::Stop(PurgeOutcome::InUse);
    }

    if let Err(outcome) = verify_source_identity_on_disk(&path, size, mtime) {
        // Nothing left to destroy, but the user's intent (this file should be gone) is already
        // satisfied — stamp it purged so the row leaves the review list instead of reappearing
        // (and re-reporting AlreadyGone) on every future press. Every other Stop outcome must
        // leave failure_class untouched so the row survives in the review list for a retry.
        if outcome == PurgeOutcome::AlreadyGone {
            mark_purged(conn, id);
        }
        return PreDestroy::Stop(outcome);
    }

    if should_rescan_before_purge(class.as_deref()) {
        return match handbrake_path {
            // Cannot even attempt the rescan (e.g. HandBrakeCLI moved since classification) —
            // indistinguishable from "the scan ran and failed", so this must not fall through
            // to destruction.
            Err(_) => PreDestroy::Stop(PurgeOutcome::Unverifiable),
            Ok(_) => PreDestroy::NeedsScan { path },
        };
    }
    PreDestroy::ReadyToDestroy { path }
}

/// What rung 4 concludes from a rescan's outcome. Kept as a pure mapping — separate both from
/// `ScanOutcome` (owned by probe.rs) and from actually running the scan — so the
/// `NoTitle -> Destroy` / `CouldNotRun -> Unverifiable` split can be pinned by a unit test
/// regardless of whether a real scan can run in the test environment. If someone later collapses
/// these two back together, a test on this function alone catches it.
#[derive(Debug, PartialEq, Eq)]
enum RescanVerdict {
    /// A transient environment fault (e.g. a mount that hiccuped mid-scan), not a bad file.
    Recovered,
    /// The scan ran to completion and still found nothing — a real, re-confirmed verdict that
    /// the file is bad. Proceed to destroy.
    Destroy,
    /// The scan could not be run at all (spawn failure, HandBrakeCLI moved, timeout) — says
    /// nothing about the file, so never treat it as confirmation.
    Unverifiable,
}

fn rescan_verdict(outcome: crate::probe::ScanOutcome) -> RescanVerdict {
    match outcome {
        crate::probe::ScanOutcome::Titled(_) => RescanVerdict::Recovered,
        crate::probe::ScanOutcome::NoTitle => RescanVerdict::Destroy,
        crate::probe::ScanOutcome::CouldNotRun => RescanVerdict::Unverifiable,
    }
}

/// Stamp a row purged so it drops out of the review list (`get_bad_sources_inner` filters on
/// `failure_class`) while the history entry itself survives.
fn mark_purged(conn: &rusqlite::Connection, id: &str) {
    let _ = conn.execute(
        "UPDATE jobs SET failure_class = ?2 WHERE id = ?1",
        params![id, CLASS_BAD_SOURCE_PURGED],
    );
}

/// Stamp a row recovered so it drops out of the review list too (F12) — without this, a file
/// the rescan just PROVED healthy stays listed under "Bad sources" forever, costing a fresh
/// ~30s rescan on every subsequent purge press. See `CLASS_BAD_SOURCE_RECOVERED` for why this
/// uses a distinct value rather than NULL or `CLASS_BAD_SOURCE_PURGED`.
fn mark_recovered(conn: &rusqlite::Connection, id: &str) {
    let _ = conn.execute(
        "UPDATE jobs SET failure_class = ?2 WHERE id = ?1",
        params![id, CLASS_BAD_SOURCE_RECOVERED],
    );
}

/// The two ways a condemned source can be disposed of, mapped from the persisted
/// `bad_source_action` setting via `normalize_bad_source_action` (so "trash"/"delete" stay the
/// single source of truth for what counts as which). A typed enum instead of comparing
/// `action == "delete"` inline so the setting -> dispatch mapping is a pure, unit-testable
/// function: mutation testing found this routing completely unverified — every purge test
/// passed `"delete"` literally, so the DEFAULT `"trash"` arm, sold to users as recoverable, was
/// never distinguished from a permanent delete.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PurgeAction {
    Trash,
    Delete,
}

impl PurgeAction {
    fn from_setting(value: &str) -> Self {
        match crate::settings_ops::normalize_bad_source_action(value) {
            "delete" => PurgeAction::Delete,
            _ => PurgeAction::Trash,
        }
    }
}

/// Destroy `path` per `action` and stamp the row purged. Called only once every earlier rung
/// has passed. The actual filesystem primitive is injected (`remove_file`/`trash_delete`) so the
/// trash-vs-delete DISPATCH — not just the pure `PurgeAction::from_setting` mapping above — can
/// be pinned by a test without ever deleting a real file or touching the OS Trash.
fn destroy_and_record_with(
    conn: &rusqlite::Connection,
    id: &str,
    action: PurgeAction,
    path: &str,
    remove_file: impl FnOnce(&str) -> bool,
    trash_delete: impl FnOnce(&str) -> bool,
) -> PurgeOutcome {
    let destroyed = match action {
        PurgeAction::Delete => remove_file(path),
        PurgeAction::Trash => trash_delete(path),
    };
    if !destroyed {
        return PurgeOutcome::Failed;
    }

    mark_purged(conn, id);
    PurgeOutcome::Purged
}

// R4: `destroy_and_record` itself has no DI seam — unlike `destroy_and_record_with`, whose two
// closures a test can swap in freely, these two are hardcoded to the REAL primitives. A test
// that only calls `destroy_and_record_with` (as `purge_action_from_setting_maps_...` and the two
// `destroy_and_record_routes_..._never_..._with` tests below do) can never catch a transposition
// of these two arguments here — that mistake would still pass every one of those tests, while
// silently making every Trash-configured purge (the DEFAULT) permanently unlink instead of
// recoverably trash. Named functions (not inline closures) with `#[cfg(test)]` call-count
// instrumentation are the seam: `destroy_and_record_binds_..._not_swapped` below calls the real
// `destroy_and_record` and asserts which primitive it invoked, without ever touching the real OS
// Trash — the trash arm is itself replaced with a safe stand-in under `#[cfg(test)]`.

#[cfg(test)]
thread_local! {
    static REMOVE_FILE_PRIMITIVE_CALLS: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

fn remove_file_primitive(path: &str) -> bool {
    #[cfg(test)]
    REMOVE_FILE_PRIMITIVE_CALLS.with(|c| c.set(c.get() + 1));
    std::fs::remove_file(path).is_ok()
}

/// The recoverable-delete primitive: the head-injected `FileDisposer` (OS trash on desktop,
/// permanent delete on a server head). Tests inject `RecordingDisposer`, which records the path
/// and then deletes it — same observable effect the old `#[cfg(test)]` stand-in provided,
/// without a parallel test-only code path here.
fn trash_delete_primitive(disposer: &dyn FileDisposer, path: &str) -> bool {
    disposer.dispose(path)
}

fn destroy_and_record(
    conn: &rusqlite::Connection,
    id: &str,
    action: PurgeAction,
    path: &str,
    disposer: &dyn FileDisposer,
) -> PurgeOutcome {
    destroy_and_record_with(conn, id, action, path, remove_file_primitive, |p| {
        trash_delete_primitive(disposer, p)
    })
}

/// DB-only phase of the ladder: row lookup, eligibility, and InUse — via `path_is_in_use_raw`
/// (R2: a raw-string comparison, not `path_is_in_use_canonical`'s `canonicalize` syscall) — plus
/// threading through the CALLER's already-resolved `handbrake_path` (R3: resolved once for the
/// whole batch in `purge_bad_sources`, not re-detected per row) when a rescan will be needed.
/// Nothing here touches the SOURCE path's filesystem state or spawns a subprocess, so this lock
/// is always brief even when that path lives on a dead mount.
struct QuickCheck {
    path: String,
    size: Option<i64>,
    mtime: Option<i64>,
    /// `Some` iff the row's class requires a rescan before destruction — a clone of the batch's
    /// single resolved `handbrake_path`, not a fresh per-row detection.
    handbrake_path: Option<Result<String, String>>,
}

enum QuickOutcome {
    Stop(PurgeOutcome),
    Proceed(QuickCheck),
}

fn quick_db_check(
    conn: &rusqlite::Connection,
    id: &str,
    handbrake_path: &Result<String, String>,
) -> QuickOutcome {
    let row = match lookup_bad_source_row(conn, id) {
        Some(v) => v,
        None => return QuickOutcome::Stop(PurgeOutcome::Failed),
    };
    if path_is_in_use_raw(conn, &row.path) {
        return QuickOutcome::Stop(PurgeOutcome::InUse);
    }
    let handbrake_path = if should_rescan_before_purge(row.class.as_deref()) {
        Some(handbrake_path.clone())
    } else {
        None
    };
    QuickOutcome::Proceed(QuickCheck {
        path: row.path,
        size: row.size,
        mtime: row.mtime,
        handbrake_path,
    })
}

/// Same ladder as `pre_destroy_check` + `destroy_and_record`, for the production async command:
/// the shared DB mutex is released around EVERY step that can block on the filesystem or a
/// subprocess — the identity re-stat, the rung-4 scan (`PROBE_TIMEOUT`, ~30s), and the destroy
/// call itself — so none of them can stall the converter thread's progress writes, or any other
/// command, for the duration. On a dead SMB/NFS mount a single stat can block 30-60s; doing that
/// under this mutex used to freeze the whole UI (every command takes this same lock) and stall
/// the queue thread's next DB write — exactly the hardware this feature targets.
///
/// The lock is held only for: the initial DB-only lookup (`quick_db_check`), a brief re-acquire
/// to stamp `AlreadyGone`, and the final re-verify + destroy. Phase 2's filesystem checks and the
/// rescan run with the mutex fully released.
///
/// Before the async/lock restructure, identity -> (scan) -> destroy all ran under ONE held
/// lock, so nothing else could touch this row or path in between (the converter itself must
/// take this same lock to flip a job to 'encoding'). Releasing the lock around the scan reopens
/// that window: in the up-to-30s gap, a job for the same path could be added, claimed, encoded
/// IN PLACE, and complete (landing back at `status = 'done'`, so a same-path InUse check alone
/// would read as free again) before the scan result comes back. Re-checking only `InUse` after
/// re-acquiring the lock — the original round-1 fix — misses exactly this: the row's
/// eligibility and the file's identity fingerprint are not re-verified, so a freshly converted
/// file (the user's only copy, for an in-place job) could be destroyed as if it were still the
/// original bad source.
///
/// The fix: re-run the FULL `pre_destroy_check` (eligibility, InUse, AlreadyGone, identity —
/// everything but the scan itself) under the freshly re-acquired lock, and only destroy if it
/// still passes. A row that needed a rescan will report `NeedsScan` again here (its
/// `failure_class` hasn't changed) rather than `ReadyToDestroy` — that's expected, and is
/// treated as "every DB-side fact still holds," not as a signal to scan a second time: the
/// `RescanVerdict::Destroy` obtained outside the lock already stands as the scan's answer. This
/// final re-stat is cheap: phase 2 already proved the mount responsive, so re-acquiring the lock
/// here cannot stall.
///
/// R2: the two locked phases use different in-use checks on purpose. Phase 1 (`quick_db_check`)
/// uses `path_is_in_use_raw` — a plain string comparison, no `canonicalize` syscall — so it stays
/// filesystem-free and brief even when a path lives on a dead mount. Phase 3 (`pre_destroy_check`)
/// uses `path_is_in_use_canonical`, because phase 3 is the rung that actually authorizes
/// destruction and by then phase 2 has already proven the mount responsive, making the
/// `canonicalize` cost safe to pay. R3: `handbrake_path` is resolved ONCE per purge batch by the
/// caller (`purge_bad_sources`), outside any lock, and threaded through both phases unchanged —
/// neither phase re-detects it.
fn purge_one_locked(
    db: &Arc<Mutex<rusqlite::Connection>>,
    id: &str,
    action: PurgeAction,
    handbrake_path: &Result<String, String>,
    disposer: &dyn FileDisposer,
) -> PurgeOutcome {
    // Phase 1: DB-only.
    let check = {
        let conn = db.lock().unwrap();
        match quick_db_check(&conn, id, handbrake_path) {
            QuickOutcome::Stop(outcome) => return outcome,
            QuickOutcome::Proceed(c) => c,
        }
    };

    // Phase 2: filesystem work OUTSIDE the lock — see the doc comment above.
    if let Err(outcome) = verify_source_identity_on_disk(&check.path, check.size, check.mtime) {
        if outcome == PurgeOutcome::AlreadyGone {
            let conn = db.lock().unwrap();
            mark_purged(&conn, id);
        }
        return outcome;
    }

    if let Some(handbrake_path_result) = check.handbrake_path {
        let handbrake_path = match handbrake_path_result {
            Ok(p) => p,
            Err(_) => return PurgeOutcome::Unverifiable,
        };
        // Deliberately outside any lock — see the doc comment above.
        match rescan_verdict(crate::probe::scan_outcome(&handbrake_path, &check.path)) {
            RescanVerdict::Recovered => {
                // F12: a file just PROVED healthy must exit the review list, not sit there
                // forever costing a fresh rescan on every future purge press. Nothing is
                // destroyed, so a brief re-acquired lock to stamp the row is all this needs.
                let conn = db.lock().unwrap();
                mark_recovered(&conn, id);
                return PurgeOutcome::Recovered;
            }
            RescanVerdict::Unverifiable => return PurgeOutcome::Unverifiable,
            RescanVerdict::Destroy => {
                // Rule-1 evidence (failure_class.rs): HandBrake's diagnostics are byte-identical
                // for a genuinely corrupt file and a healthy one we merely can't open (a
                // hiccuping mount, a permission fluke). The rescan proves HandBrake still can't
                // parse it; only WE opening the file ourselves proves it is truly unreadable and
                // not just transiently so.
                if !crate::converter::source_is_readable(&check.path) {
                    return PurgeOutcome::Unverifiable;
                }
            }
        }
    }

    // Phase 3: re-acquire the lock and re-verify EVERYTHING — not just InUse — before
    // destroying. See the doc comment above.
    //
    // A note on a boundary this relies on: phase 1 decided whether a rescan was needed from the
    // row's failure_class, and if it wasn't (a bad_source_truncated row), phase 2 above ran no
    // scan and no `source_is_readable` gate at all. Phase 3 below re-reads failure_class fresh —
    // if it had somehow changed to bad_source between phase 1 and phase 3, `NeedsScan` here would
    // still be treated as "ready" (see the match below) and destruction would proceed with
    // neither a rescan nor a readability check ever having run. This is unreachable today —
    // nothing in this codebase rewrites failure_class or source_path on an existing error row —
    // but a future retry feature that did would need to close this gap.
    let conn = db.lock().unwrap();
    match pre_destroy_check(&conn, id, handbrake_path) {
        PreDestroy::Stop(outcome) => outcome,
        // Either never needed a rescan (bad_source_truncated), or needed one and every other
        // rung still passes — in the latter case the scan already ran above and confirmed
        // Destroy, so this is not a second rescan, just re-confirmation of the DB-side facts.
        PreDestroy::ReadyToDestroy { path } | PreDestroy::NeedsScan { path, .. } => {
            destroy_and_record(&conn, id, action, &path, disposer)
        }
    }
}

/// The probe-free skip decision for one path: `Some(reason)` to skip it, `None` to keep it.
/// Shared by `probe_candidates` (which paths are worth the expensive HandBrake probe) and
/// `add_files_to_db` (authoritative accounting + insert) so the two can never disagree about what
/// counts as a skip. Never returns `AlreadyAtTarget` — that decision requires a source probe.
fn cheap_skip_reason(
    path_str: &str,
    suffix: &str,
    identity: Option<(i64, i64)>,
    queued_paths: &HashSet<String>,
    legacy_history_paths: &HashSet<String>,
    converted_identities: &HashSet<(i64, i64)>,
) -> Option<SkipReason> {
    let path = Path::new(path_str);
    if !is_video_file(path) {
        return Some(SkipReason::NotVideo);
    }
    if queued_paths.contains(path_str) {
        return Some(SkipReason::AlreadyQueued);
    }
    // A file whose (size, mtime) matches a completed conversion is genuinely already done,
    // whatever it is named. This replaces the old output-filename-exists heuristic, which
    // wrongly skipped a different video that recycled a converted file's name.
    if let Some(id) = identity {
        if converted_identities.contains(&id) {
            return Some(SkipReason::AlreadyConverted);
        }
    }
    // Pre-migration completed rows carry no fingerprint; fall back to the old source_path
    // match (see `fetch_skip_sets` for how this set is scoped).
    if legacy_history_paths.contains(path_str) {
        return Some(SkipReason::AlreadyConverted);
    }
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("output");
    if !suffix.is_empty() && stem.ends_with(suffix) {
        return Some(SkipReason::AlreadyConverted);
    }
    None
}

/// Output path for a source, renumbering the BASE name to avoid clobbering an existing file:
/// `movie.mp4` -> `movie.h265.mp4`, then `movie (1).h265.mp4`, `movie (2).h265.mp4`, ...
/// In-place jobs (default output == source) are returned verbatim and never renumbered, so the
/// converter's in-place path stays intact. `is_taken` reports whether a candidate name is already
/// claimed (on disk, by another job row, or earlier in the same batch).
fn choose_output_path(source_path: &str, suffix: &str, is_taken: &dyn Fn(&str) -> bool) -> String {
    let path = Path::new(source_path);
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("output");
    let parent = path.parent().unwrap_or(Path::new("."));
    let default = parent.join(format!("{}{}.mp4", stem, suffix));
    // In-place guard first: an in-place source's own on-disk presence must never trigger a rename.
    if default.as_path() == path {
        return default.to_string_lossy().to_string();
    }
    let default_str = default.to_string_lossy().to_string();
    if !is_taken(&default_str) {
        return default_str;
    }
    let mut n = 1;
    loop {
        let candidate = parent
            .join(format!("{} ({}){}.mp4", stem, n, suffix))
            .to_string_lossy()
            .to_string();
        if !is_taken(&candidate) {
            return candidate;
        }
        n += 1;
    }
}

/// Reads the three sets the cheap skip checks test against: active-queue source paths, the
/// pre-migration legacy history paths (scoped by `skip_already_converted`, plus in-place rows),
/// and the always-on `(size, mtime)` fingerprints of completed conversions.
fn fetch_skip_sets(
    conn: &rusqlite::Connection,
    skip_already_converted: bool,
) -> Result<(HashSet<String>, HashSet<String>, HashSet<(i64, i64)>), String> {
    let queued_paths: HashSet<String> = {
        let mut stmt = conn
            .prepare(
                "SELECT source_path FROM jobs WHERE status IN ('queued', 'encoding', 'paused')",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|e| e.to_string())?;
        rows.filter_map(|r| r.ok()).collect()
    };

    // One pass over completed jobs feeds two skip signals:
    // - `converted_identities`: (size, mtime) fingerprints for the always-on identity check.
    // - `legacy_history_paths`: source paths of PRE-MIGRATION rows that carry no fingerprint.
    //   In-place rows (source_path == output_path) are always included to prevent a re-encode
    //   cascade; other legacy rows are included only when `skip_already_converted` is on, which
    //   preserves the historical history-skip behavior for upgraded databases.
    let mut converted_identities: HashSet<(i64, i64)> = HashSet::new();
    let mut legacy_history_paths: HashSet<String> = HashSet::new();
    {
        let mut stmt = conn
            .prepare(
                "SELECT source_path, output_path, source_size, source_mtime
                 FROM jobs WHERE status IN ('done', 'skipped')",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<i64>>(2)?,
                    row.get::<_, Option<i64>>(3)?,
                ))
            })
            .map_err(|e| e.to_string())?;
        for (source_path, output_path, size, mtime) in rows.flatten() {
            match (size, mtime) {
                (Some(s), Some(m)) => {
                    converted_identities.insert((s, m));
                }
                _ => {
                    let in_place = Path::new(&source_path) == Path::new(&output_path);
                    if in_place || skip_already_converted {
                        legacy_history_paths.insert(source_path);
                    }
                }
            }
        }
    }

    Ok((queued_paths, legacy_history_paths, converted_identities))
}

/// The subset of `paths` worth probing for the source-media skip: those that survive the
/// probe-free skip checks. A re-scan of an already-queued/converted folder yields an empty set, so
/// the source-media probe shells out to HandBrake zero times.
fn probe_candidates(
    conn: &rusqlite::Connection,
    paths: &[String],
    suffix: &str,
    skip_already_converted: bool,
) -> Result<Vec<String>, String> {
    let (queued_paths, legacy_history_paths, converted_identities) =
        fetch_skip_sets(conn, skip_already_converted)?;
    Ok(paths
        .iter()
        .filter(|p| {
            let identity = crate::probe_cache::file_identity(p).map(|i| (i.size, i.mtime));
            cheap_skip_reason(
                p,
                suffix,
                identity,
                &queued_paths,
                &legacy_history_paths,
                &converted_identities,
            )
            .is_none()
        })
        .cloned()
        .collect())
}

pub fn add_files_inner(
    ctx: &Ctx,
    paths: &[String],
    progress: Option<&dyn Fn(u32, u32)>,
) -> Result<AddResult, String> {
    // Nothing to add decides nothing: there is no output name to build, so there is no reason to
    // reach HandBrake. Without this, intake resolved the suffix template first and an empty
    // intake failed outright when HandBrakeCLI was absent — including "add a folder that turned
    // out to hold no videos". `watcher::enqueue_and_start` already guards emptiness at its own
    // call site; `add_files`, `confirm_folder_add`, and the server route did not.
    if paths.is_empty() {
        return Ok(AddResult::default());
    }

    // First, read preset and suffix template from DB
    let (preset, suffix_template, hb_path, skip_already_converted, skip_by_source_media) = {
        let conn = ctx.db.lock().map_err(|e| e.to_string())?;

        let preset: String = conn
            .query_row(
                "SELECT value FROM settings WHERE key = 'preset'",
                [],
                |row| row.get(0),
            )
            .map_err(|e| e.to_string())?;

        let suffix_template = crate::settings_ops::read_suffix_template(&conn, &preset);

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
            .unwrap_or(false);

        // Source-media skip also needs the target preset metadata, so fetch HandBrake when
        // either the suffix template or the skip toggle requires it.
        let hb_path = if suffix_template.contains('{') || skip_by_source_media {
            get_handbrake_path(&conn, &*ctx.handbrake).ok()
        } else {
            None
        };

        (
            preset,
            suffix_template,
            hb_path,
            skip_already_converted,
            skip_by_source_media,
        )
    }; // db lock released

    // Resolve template if needed
    let suffix = if suffix_template.contains('{') {
        let hb = hb_path.clone().ok_or("HandBrakeCLI not found")?;
        let metadata = crate::handbrake::cached_preset_metadata(ctx, &hb, &preset)?;
        handbrake::resolve_suffix_template(&suffix_template, &metadata)
    } else {
        suffix_template
    };

    // Source-media skip: probe candidate files and drop those already at/below the target. Only
    // files that survive the probe-free skip checks are probed, so a re-scan of an already-handled
    // folder shells out to HandBrake zero times. Probing runs outside the DB lock; on any
    // uncertainty (no HandBrake, probe failure/timeout, unknown codec) the file is kept.
    let candidates_to_probe: Vec<String> = if skip_by_source_media {
        let conn = ctx.db.lock().map_err(|e| e.to_string())?;
        probe_candidates(&conn, paths, &suffix, skip_already_converted)?
    } else {
        Vec::new()
    };

    let media_skipped: HashSet<String> = if !candidates_to_probe.is_empty() {
        if let Some(hb) = hb_path.as_deref() {
            let metadata = crate::handbrake::cached_preset_metadata(ctx, hb, &preset)?;
            let target_codec = metadata.codec.clone();
            let target_height =
                crate::media_skip::target_height_from_resolution(&metadata.resolution);
            // Stamp each candidate with its filesystem identity (stat outside the DB lock),
            // then reuse cached media for unchanged files and probe only the misses.
            // resolve_media calls lookup (brief lock) -> probe (no lock) -> store (brief
            // lock), so the HandBrake shell-out never runs while the DB mutex is held.
            let with_identity: Vec<(String, Option<crate::probe_cache::FileIdentity>)> =
                candidates_to_probe
                    .iter()
                    .map(|p| (p.clone(), crate::probe_cache::file_identity(p)))
                    .collect();
            let total = candidates_to_probe.len() as u32;
            let probe_count = std::cell::Cell::new(0u32);
            let probed = crate::probe_cache::resolve_media(
                &with_identity,
                |ids| {
                    let conn = ctx.db.lock().expect("db mutex poisoned");
                    crate::probe_cache::lookup_batch(&conn, ids)
                },
                |p| {
                    let media = crate::probe::probe_source(hb, p);
                    let done = probe_count.get() + 1;
                    probe_count.set(done);
                    if let Some(report) = progress {
                        report(done, total);
                    }
                    media
                },
                |items| {
                    let conn = ctx.db.lock().expect("db mutex poisoned");
                    crate::probe_cache::store_batch(&conn, items);
                },
            );
            crate::media_skip::select_media_skips(&probed, &target_codec, target_height)
        } else {
            HashSet::new()
        }
    } else {
        HashSet::new()
    };

    let survivors: Vec<String> = paths
        .iter()
        .filter(|p| !media_skipped.contains(*p))
        .cloned()
        .collect();

    // Re-acquire db lock and hand the resolved suffix to the DB-only core.
    let conn = ctx.db.lock().map_err(|e| e.to_string())?;
    let mut result = add_files_to_db(&conn, &survivors, &preset, &suffix, skip_already_converted)?;
    if !media_skipped.is_empty() {
        result.skipped.push(SkipCount {
            reason: SkipReason::AlreadyAtTarget,
            count: media_skipped.len() as u32,
        });
    }
    Ok(result)
}

/// DB-only core of `add_files_inner`: applies the skip rules and inserts queued jobs given an
/// already-resolved output suffix. Separated from suffix resolution (which may shell out to
/// HandBrakeCLI) so the skip-rule matrix can be tested against an in-memory database.
fn add_files_to_db(
    conn: &rusqlite::Connection,
    paths: &[String],
    preset: &str,
    suffix: &str,
    skip_already_converted: bool,
) -> Result<AddResult, String> {
    let (queued_paths, legacy_history_paths, converted_identities) =
        fetch_skip_sets(conn, skip_already_converted)?;

    let mut queue_order = get_next_queue_order(conn)?;
    let mut added = Vec::new();
    // Output names claimed earlier in THIS batch, so two new sources never resolve to one name.
    let mut assigned: HashSet<String> = HashSet::new();
    let (mut n_not_video, mut n_queued, mut n_converted, mut n_output_exists) =
        (0u32, 0u32, 0u32, 0u32);

    for path_str in paths {
        let identity = crate::probe_cache::file_identity(path_str);
        let id_tuple = identity.map(|i| (i.size, i.mtime));
        if let Some(reason) = cheap_skip_reason(
            path_str,
            suffix,
            id_tuple,
            &queued_paths,
            &legacy_history_paths,
            &converted_identities,
        ) {
            match reason {
                SkipReason::NotVideo => n_not_video += 1,
                SkipReason::AlreadyQueued => n_queued += 1,
                SkipReason::AlreadyConverted => n_converted += 1,
                SkipReason::OutputExists => n_output_exists += 1,
                // The cheap checks never produce AlreadyAtTarget — that needs a probe.
                SkipReason::AlreadyAtTarget => {}
            }
            continue;
        }

        let path = Path::new(path_str);
        // Never clobber an existing output: if the default name is taken (on disk, by another job
        // row, or earlier in this batch) the base name is renumbered. In-place jobs are returned
        // verbatim by `choose_output_path` and never renumbered. The closure's borrow of
        // `assigned` ends with this block, before the `assigned.insert` below.
        let output_str = {
            let is_taken = |name: &str| -> bool {
                assigned.contains(name)
                    || Path::new(name).exists()
                    || conn
                        .query_row(
                            "SELECT 1 FROM jobs WHERE output_path = ?1 LIMIT 1",
                            params![name],
                            |_| Ok(()),
                        )
                        .is_ok()
            };
            choose_output_path(path_str, suffix, &is_taken)
        };
        assigned.insert(output_str.clone());

        let id = uuid::Uuid::new_v4().to_string();
        let now = chrono::Utc::now().to_rfc3339();
        // Preserve original_size for the space-saved display even when only mtime is unreadable.
        let original_size = identity
            .map(|i| i.size)
            .or_else(|| std::fs::metadata(path).map(|m| m.len() as i64).ok());
        let source_size = identity.map(|i| i.size);
        let source_mtime = identity.map(|i| i.mtime);

        conn.execute(
            "INSERT INTO jobs (id, source_path, output_path, preset, status, original_size, source_size, source_mtime, queue_order, created_at)
             VALUES (?1, ?2, ?3, ?4, 'queued', ?5, ?6, ?7, ?8, ?9)",
            params![
                id,
                path_str,
                output_str,
                preset,
                original_size,
                source_size,
                source_mtime,
                queue_order,
                now,
            ],
        )
        .map_err(|e| e.to_string())?;

        added.push(JobInfo {
            id,
            source_path: path_str.clone(),
            output_path: output_str,
            preset: preset.to_string(),
            status: "queued".to_string(),
            original_size,
            converted_size: None,
            kept_file: None,
            space_saved: None,
            error_message: None,
            failure_class: None,
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

/// Adds `paths`, bracketing the intake with `add-started`/`add-progress`/`add-finished` events
/// (via `AddOp`) and, on success, an unlabeled `queue-updated` emit so `useQueue` refreshes
/// without a dedicated frontend callback (mirrors the watcher's `enqueue_and_start`).
pub fn add_files(ctx: &Arc<Ctx>, paths: &[String]) -> Result<AddResult, String> {
    let result = {
        let op = crate::add_progress::AddOp::new(ctx.events.clone(), String::new());
        let reporter = |done: u32, total: u32| op.report(done, total);
        add_files_inner(ctx, paths, Some(&reporter as &dyn Fn(u32, u32)))
        // `op` drops here → add-finished, before the queue-updated emit below.
    };
    if result.is_ok() {
        ctx.events.emit_t("queue-updated", ());
    }
    result
}

pub fn scan_folder(path: String) -> Result<FolderScanResult, String> {
    let dir = Path::new(&path);
    if !dir.is_dir() {
        return Err("Path is not a directory".to_string());
    }

    let files = scan_video_files(dir);
    let folder_name = dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("Unknown")
        .to_string();

    Ok(FolderScanResult {
        file_count: files.len(),
        folder_name,
        folder_path: path,
    })
}

/// Recursively scans `path` for video files and adds them, same event bracketing as
/// [`add_files`] but labeled with the folder's own name.
pub fn confirm_folder_add(ctx: &Arc<Ctx>, path: String) -> Result<AddResult, String> {
    if !Path::new(&path).is_dir() {
        return Err("Path is not a directory".to_string());
    }

    let label = Path::new(&path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_string();
    let result = {
        let op = crate::add_progress::AddOp::new(ctx.events.clone(), label);
        let files = scan_video_files(Path::new(&path));
        let paths: Vec<String> = files
            .into_iter()
            .filter_map(|p| p.to_str().map(|s| s.to_string()))
            .collect();
        let reporter = |done: u32, total: u32| op.report(done, total);
        add_files_inner(ctx, &paths, Some(&reporter as &dyn Fn(u32, u32)))
        // `op` drops here → add-finished, before the queue-updated emit below.
    };
    if result.is_ok() {
        ctx.events.emit_t("queue-updated", ());
    }
    result
}

pub fn get_queue(ctx: &Ctx) -> Result<Vec<JobInfo>, String> {
    let conn = ctx.db.lock().map_err(|e| e.to_string())?;
    let mut stmt = conn
        .prepare(
            "SELECT id, source_path, output_path, preset, status, original_size, converted_size,
                    kept_file, space_saved, error_message, failure_class, queue_order, created_at, completed_at
             FROM jobs
             WHERE status IN ('queued', 'encoding', 'paused', 'error')
             ORDER BY queue_order ASC",
        )
        .map_err(|e| e.to_string())?;

    let jobs = stmt
        .query_map([], |row| row_to_job(row))
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;

    Ok(jobs)
}

pub fn remove_job(ctx: &Ctx, id: &str) -> Result<(), String> {
    let conn = ctx.db.lock().map_err(|e| e.to_string())?;
    conn.execute(
        "DELETE FROM jobs WHERE id = ?1 AND status = 'queued'",
        params![id],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

pub fn remove_history_entry(ctx: &Ctx, id: &str) -> Result<(), String> {
    let conn = ctx.db.lock().map_err(|e| e.to_string())?;
    remove_history_entry_inner(&conn, id)
}

fn remove_history_entry_inner(conn: &rusqlite::Connection, id: &str) -> Result<(), String> {
    // Terminal rows only — mirrors remove_job's queued-only guard so the History
    // tab can never delete a job that is queued or mid-encode.
    conn.execute(
        "DELETE FROM jobs WHERE id = ?1 AND status IN ('done', 'error', 'skipped')",
        params![id],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

pub fn reorder_queue(ctx: &Ctx, job_ids: &[String]) -> Result<(), String> {
    let conn = ctx.db.lock().map_err(|e| e.to_string())?;
    reorder_queue_inner(&conn, job_ids)
}

fn reorder_queue_inner(conn: &rusqlite::Connection, job_ids: &[String]) -> Result<(), String> {
    conn.execute("BEGIN", []).map_err(|e| e.to_string())?;
    for (i, id) in job_ids.iter().enumerate() {
        if let Err(e) = conn.execute(
            "UPDATE jobs SET queue_order = ?1 WHERE id = ?2",
            params![i as i32, id],
        ) {
            let _ = conn.execute("ROLLBACK", []);
            return Err(e.to_string());
        }
    }
    conn.execute("COMMIT", []).map_err(|e| e.to_string())?;
    Ok(())
}

pub fn clear_completed(ctx: &Ctx, mode: &str) -> Result<(), String> {
    let conn = ctx.db.lock().map_err(|e| e.to_string())?;
    match mode {
        "errors" => {
            conn.execute("DELETE FROM jobs WHERE status = 'error'", [])
                .map_err(|e| e.to_string())?;
        }
        _ => {
            // "all" - clear everything in history
            conn.execute(
                "DELETE FROM jobs WHERE status IN ('done', 'skipped', 'error')",
                [],
            )
            .map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

pub fn clear_queue(ctx: &Ctx) -> Result<(), String> {
    let conn = ctx.db.lock().map_err(|e| e.to_string())?;
    conn.execute("DELETE FROM jobs WHERE status = 'queued'", [])
        .map_err(|e| e.to_string())?;
    // A cleared queue has no job to justify a low-disk pause reason; drop it so the banner
    // can't be re-seeded over an empty queue after a remount.
    *ctx.converter
        .low_disk_pause
        .lock()
        .map_err(|e| e.to_string())? = None;
    // A cleared queue has no jobs to stay paused for.
    crate::converter::set_queue_paused(&conn, false);
    Ok(())
}

/// The review-list query itself. Extracted so both the command and its test exercise the same
/// SQL — a WHERE clause or SELECT column list that drifted between two copies would leave the
/// column-count hazard (and the review-list scoping) untested.
fn get_bad_sources_inner(conn: &rusqlite::Connection) -> Result<Vec<JobInfo>, String> {
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
        .query_map(
            params![CLASS_BAD_SOURCE, CLASS_BAD_SOURCE_TRUNCATED],
            row_to_job,
        )
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<Vec<JobInfo>, _>>()
        .map_err(|e| e.to_string())
}

pub fn get_bad_sources(ctx: &Ctx) -> Result<Vec<JobInfo>, String> {
    let conn = ctx.db.lock().map_err(|e| e.to_string())?;
    get_bad_sources_inner(&conn)
}

/// Purges a batch of bad-source review-list ids per the persisted `bad_source_action`. Rung 4
/// can block per id for up to `PROBE_TIMEOUT` (~30s) scanning a stalled/offline source, so this
/// is meant to be called from a blocking thread by callers (the desktop wrapper offloads via
/// `spawn_blocking`, same as `add_files`/`scan_folder`/`confirm_folder_add`/`classify_paths`);
/// `purge_one_locked` additionally releases the DB mutex around each scan so a slow purge can't
/// stall the converter thread too.
pub fn purge_bad_sources(ctx: &Arc<Ctx>, ids: Vec<String>) -> Result<Vec<PurgeResult>, String> {
    let action: String = {
        let conn = ctx.db.lock().map_err(|e| e.to_string())?;
        conn.query_row(
            "SELECT value FROM settings WHERE key = 'bad_source_action'",
            params![],
            |r| r.get(0),
        )
        .unwrap_or_else(|_| "trash".to_string())
    }; // guard dropped here: `require_handbrake_path` takes `ctx.db` itself, and it is not
       // reentrant. The two settings are no longer read in one acquisition — they are
       // independent, and nothing here depends on seeing a consistent snapshot of both.
    let action = PurgeAction::from_setting(&action);
    // R3: resolved ONCE for the whole batch, OUTSIDE the lock, and passed to every
    // `purge_one_locked` call below — the fallback can spawn a blocking `which`/`where`
    // subprocess (`PathLocator`), and this used to run per id, under the DB mutex, in both
    // purge phases, i.e. up to 2N blocking spawns under the lock for a batch of N ids.
    // `require_handbrake_path` locks only to read the setting, releases, then runs the locator
    // unlocked, so R3 still holds — at the cost of one extra acquisition per batch, not per id.
    // A lock failure inside it now lands in this `Err` and reaches `purge_one_locked` per id
    // (which maps any `Err` to `Unverifiable`, destroying nothing) rather than failing the whole
    // call; on a poisoned mutex the `action` read above would already have returned `Err` first.
    let handbrake_path = handbrake::require_handbrake_path(ctx);
    Ok(ids
        .iter()
        .map(|id| PurgeResult {
            id: id.clone(),
            outcome: purge_one_locked(&ctx.db, id, action, &handbrake_path, &*ctx.disposer),
        })
        .collect())
}

pub fn get_history(
    ctx: &Ctx,
    limit: u32,
    offset: u32,
    search: Option<String>,
    sort_by: Option<String>,
) -> Result<HistoryPage, String> {
    let conn = ctx.db.lock().map_err(|e| e.to_string())?;
    get_history_inner(&conn, limit, offset, search, sort_by)
}

fn get_history_inner(
    conn: &rusqlite::Connection,
    limit: u32,
    offset: u32,
    search: Option<String>,
    sort_by: Option<String>,
) -> Result<HistoryPage, String> {
    let search_param = search
        .as_ref()
        .filter(|s| !s.is_empty())
        .map(|s| format!("%{}%", s));
    let has_search = search_param.is_some();

    let order_clause = match sort_by.as_deref() {
        Some("space_saved") => "space_saved DESC",
        Some("original_size") => "original_size DESC",
        Some("source_path") => "source_path ASC",
        _ => "completed_at DESC",
    };

    // Count query
    let total: i64 = if has_search {
        let count_sql = "SELECT COUNT(*) FROM jobs WHERE status IN ('done', 'error', 'skipped') AND (source_path LIKE ?1 OR output_path LIKE ?1)";
        conn.query_row(count_sql, params![search_param], |row| row.get(0))
            .map_err(|e| e.to_string())?
    } else {
        conn.query_row(
            "SELECT COUNT(*) FROM jobs WHERE status IN ('done', 'error', 'skipped')",
            [],
            |row| row.get(0),
        )
        .map_err(|e| e.to_string())?
    };

    // Data query
    let jobs = if has_search {
        let sql = format!(
            "SELECT id, source_path, output_path, preset, status, original_size, converted_size,
                    kept_file, space_saved, error_message, failure_class, queue_order, created_at, completed_at
             FROM jobs
             WHERE status IN ('done', 'error', 'skipped') AND (source_path LIKE ?1 OR output_path LIKE ?1)
             ORDER BY {}
             LIMIT ?2 OFFSET ?3",
            order_clause
        );
        let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
        let result = stmt
            .query_map(params![search_param, limit, offset], |row| row_to_job(row))
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        result
    } else {
        let sql = format!(
            "SELECT id, source_path, output_path, preset, status, original_size, converted_size,
                    kept_file, space_saved, error_message, failure_class, queue_order, created_at, completed_at
             FROM jobs
             WHERE status IN ('done', 'error', 'skipped')
             ORDER BY {}
             LIMIT ?1 OFFSET ?2",
            order_clause
        );
        let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
        let result = stmt
            .query_map(params![limit, offset], |row| row_to_job(row))
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        result
    };

    Ok(HistoryPage { jobs, total })
}

pub fn get_history_summary(ctx: &Ctx, search: Option<String>) -> Result<HistorySummary, String> {
    let conn = ctx.db.lock().map_err(|e| e.to_string())?;
    get_history_summary_inner(&conn, search)
}

fn get_history_summary_inner(
    conn: &rusqlite::Connection,
    search: Option<String>,
) -> Result<HistorySummary, String> {
    let search_param = search
        .as_ref()
        .filter(|s| !s.is_empty())
        .map(|s| format!("%{}%", s));

    let (total_saved_bytes, total_files): (i64, i64) = if search_param.is_some() {
        conn.query_row(
            "SELECT COALESCE(SUM(space_saved), 0), COUNT(*) FROM jobs WHERE status = 'done' AND (source_path LIKE ?1 OR output_path LIKE ?1)",
            params![search_param],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(|e| e.to_string())?
    } else {
        conn.query_row(
            "SELECT COALESCE(SUM(space_saved), 0), COUNT(*) FROM jobs WHERE status = 'done'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(|e| e.to_string())?
    };

    Ok(HistorySummary {
        total_saved_bytes,
        total_files,
    })
}

pub fn classify_paths(paths: Vec<String>) -> Result<ClassifiedPaths, String> {
    let mut files = Vec::new();
    let mut folders = Vec::new();
    for path_str in paths {
        let path = Path::new(&path_str);
        if path.is_dir() {
            let video_files = scan_video_files(path);
            let folder_name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("Unknown")
                .to_string();
            folders.push(FolderScanResult {
                file_count: video_files.len(),
                folder_name,
                folder_path: path_str,
            });
        } else if path.is_file() {
            files.push(path_str);
        }
    }
    Ok(ClassifiedPaths { files, folders })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispose::{DeleteDisposer, RecordingDisposer};
    use crate::events::TestSink;
    use crate::handbrake::AbsentLocator;
    use rusqlite::Connection;
    use std::path::Path;

    fn test_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::init_db(&conn).unwrap();
        conn
    }

    /// Same 6-line harness as `converter.rs`'s tests: a `Ctx` backed by an in-memory DB, a
    /// `TestSink` for event assertions, and a `RecordingDisposer` (records then deletes) so
    /// destructive-path tests can assert dispose calls without touching the real OS Trash.
    fn test_ctx(conn: Connection) -> (Arc<Ctx>, Arc<TestSink>, Arc<RecordingDisposer>) {
        test_ctx_with_locator(conn, Arc::new(crate::handbrake::PanickingLocator))
    }

    /// `test_ctx` for tests that actually reach HandBrake resolution and must therefore say
    /// which world they are in, rather than inheriting whatever the host has installed.
    fn test_ctx_with_locator(
        conn: Connection,
        locator: Arc<dyn crate::handbrake::HandbrakeLocator>,
    ) -> (Arc<Ctx>, Arc<TestSink>, Arc<RecordingDisposer>) {
        let sink = Arc::new(TestSink::default());
        let disposer = Arc::new(RecordingDisposer::default());
        let ctx = Ctx::new(conn, sink.clone(), disposer.clone(), locator);
        (ctx, sink, disposer)
    }

    fn insert_queued(conn: &Connection, id: &str, source: &str, status: &str, order: i32) {
        conn.execute(
            "INSERT INTO jobs (id, source_path, output_path, preset, status, queue_order, created_at)
             VALUES (?1, ?2, ?3, 'preset', ?4, ?5, '2020-01-01T00:00:00Z')",
            params![id, source, format!("{source}.out"), status, order],
        )
        .unwrap();
    }

    fn insert_history(
        conn: &Connection,
        id: &str,
        source: &str,
        status: &str,
        space_saved: i64,
        original_size: i64,
        completed_at: &str,
    ) {
        conn.execute(
            "INSERT INTO jobs (id, source_path, output_path, preset, status, original_size, space_saved, queue_order, created_at, completed_at)
             VALUES (?1, ?2, ?3, 'preset', ?4, ?5, ?6, 0, ?7, ?7)",
            params![
                id,
                source,
                format!("{source}.out.mp4"),
                status,
                original_size,
                space_saved,
                completed_at
            ],
        )
        .unwrap();
    }

    /// A completed job carrying an explicit source-identity fingerprint (post-migration row).
    fn insert_done_with_identity(
        conn: &Connection,
        id: &str,
        source: &str,
        output: &str,
        source_size: i64,
        source_mtime: i64,
    ) {
        conn.execute(
            "INSERT INTO jobs (id, source_path, output_path, preset, status, original_size, source_size, source_mtime, queue_order, created_at)
             VALUES (?1, ?2, ?3, 'preset', 'done', ?4, ?4, ?5, 0, '2020-01-01T00:00:00Z')",
            params![id, source, output, source_size, source_mtime],
        )
        .unwrap();
    }

    #[test]
    fn accepts_known_video_extensions_case_insensitively() {
        assert!(is_video_file(Path::new("movie.mp4")));
        assert!(is_video_file(Path::new("movie.MKV")));
        assert!(is_video_file(Path::new("/a/b/c.MoV")));
    }

    #[test]
    fn rejects_non_video_and_extensionless() {
        assert!(!is_video_file(Path::new("notes.txt")));
        assert!(!is_video_file(Path::new("README")));
    }

    #[test]
    fn rejects_in_place_temp_files() {
        // A lingering in-place temp must never be picked up by a folder scan or watched folder,
        // even though it carries a valid .mp4 extension.
        assert!(!is_video_file(Path::new(
            "/movies/.clip.convertbar-tmp.mp4"
        )));
        assert!(!is_video_file(Path::new("clip.convertbar-tmp.mp4")));
    }

    // ---- add_files_to_db skip rules ----

    #[test]
    fn add_files_skips_paths_already_in_queue() {
        let conn = test_conn();
        insert_queued(&conn, "j1", "/movies/a.mp4", "queued", 1);

        let result =
            add_files_to_db(&conn, &["/movies/a.mp4".to_string()], "preset", "", false).unwrap();

        assert!(
            result.added.is_empty(),
            "an already-queued source must be skipped"
        );
        assert_eq!(
            result.skipped,
            vec![SkipCount {
                reason: SkipReason::AlreadyQueued,
                count: 1
            }],
            "the skip is reported as AlreadyQueued"
        );
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM jobs", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1, "no duplicate row inserted");
    }

    #[test]
    fn add_files_renumbers_when_output_name_is_taken() {
        // A pre-existing output with no completed-job fingerprint to prove it belongs to this
        // source must NOT cause a skip (that was the recycled-filename bug). The source is queued
        // to a renumbered output so the unrelated existing file is never clobbered.
        let conn = test_conn();
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("clip-conv.mp4"), b"x").unwrap();
        let source = dir.path().join("clip.mov").to_string_lossy().to_string();

        let result = add_files_to_db(&conn, &[source], "preset", "-conv", false).unwrap();

        assert_eq!(
            result.added.len(),
            1,
            "a taken output name renumbers, not skips"
        );
        assert!(
            result.added[0].output_path.ends_with("clip (1)-conv.mp4"),
            "output must be renumbered on the base name, got {}",
            result.added[0].output_path
        );
        assert!(result.skipped.is_empty(), "renumbering is not a skip");
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

        assert!(
            result.added.is_empty(),
            "must skip a source whose stem already carries the suffix"
        );
        assert_eq!(
            result.skipped,
            vec![SkipCount {
                reason: SkipReason::AlreadyConverted,
                count: 1
            }]
        );
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM jobs", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn add_files_skip_already_converted_union_respects_flag() {
        let conn = test_conn();
        insert_history(
            &conn,
            "h1",
            "/movies/done.mkv",
            "done",
            100,
            1000,
            "2020-01-01T00:00:00Z",
        );

        let with_flag =
            add_files_to_db(&conn, &["/movies/done.mkv".to_string()], "preset", "", true).unwrap();
        assert!(with_flag.added.is_empty());
        assert_eq!(
            with_flag.skipped,
            vec![SkipCount {
                reason: SkipReason::AlreadyConverted,
                count: 1
            }]
        );

        let without_flag = add_files_to_db(
            &conn,
            &["/movies/done.mkv".to_string()],
            "preset",
            "",
            false,
        )
        .unwrap();
        assert_eq!(
            without_flag.added.len(),
            1,
            "without the flag, a done source is re-added"
        );
    }

    #[test]
    fn add_files_inner_reports_handbrake_missing_when_the_suffix_needs_a_probe() {
        // The default suffix template contains {...} placeholders, so intake must resolve HandBrake
        // to expand them. With HandBrake absent the caller gets a named error — not a panic, and
        // not a silent success that would write files with an unexpanded literal suffix.
        let (ctx, _sink, _disposer) = test_ctx_with_locator(test_conn(), Arc::new(AbsentLocator));
        let err = add_files_inner(&ctx, &["/tmp/whatever.mkv".to_string()], None).expect_err(
            "intake must fail when the suffix template needs HandBrake and it is absent",
        );
        assert!(err.contains("HandBrakeCLI not found"), "got: {err}");
    }

    #[test]
    fn add_files_inner_with_a_literal_suffix_never_resolves_handbrake() {
        // Before this branch, three tests incidentally covered the "literal suffix -> don't
        // resolve HandBrake at all" branch (the `suffix_template.contains('{') ||
        // skip_by_source_media` guard above) via a pinned `.conv`-style suffix; all three pins are
        // gone now (see the locator-seam design doc) and nothing else exercises it. Built with the
        // plain `test_ctx` (the `PanickingLocator` default) rather than a declared world on
        // purpose: if this guard were ever removed or broken, resolution would be reached and the
        // fixture default would panic instead of this assertion quietly passing for the wrong
        // reason — the cleanest demonstration in the suite that the guard is a real guard.
        let (ctx, _sink, _disposer) = test_ctx(test_conn());
        let preset: String = ctx
            .db
            .lock()
            .unwrap()
            .query_row("SELECT value FROM settings WHERE key = 'preset'", [], |r| {
                r.get(0)
            })
            .unwrap();
        crate::settings_ops::set_preset_suffix(&ctx, &preset, "-conv").unwrap();

        let result = add_files_inner(&ctx, &["/movies/clip.mp4".to_string()], None);
        assert!(
            result.is_ok(),
            "a literal suffix must never reach HandBrake resolution, got: {result:?}"
        );
    }

    #[test]
    fn add_files_inner_with_no_paths_never_reaches_handbrake_resolution() {
        // Built with the plain `test_ctx` (the `PanickingLocator` default) on purpose: it
        // asserts the negative directly. If the empty-intake guard regresses, resolution is
        // reached and the fixture panics — rather than this quietly passing on any machine that
        // happens to have HandBrakeCLI installed, which is exactly how the bug survived until
        // the locator seam made the absent world expressible.
        let (ctx, _sink, _disposer) = test_ctx(test_conn());
        let result = add_files_inner(&ctx, &[], None).expect("an empty intake cannot fail");
        assert!(
            result.added.is_empty(),
            "nothing was offered, so nothing can be added"
        );
        assert!(
            result.skipped.is_empty(),
            "nothing was offered, so nothing can be skipped"
        );
    }

    // Pins the RAII bracketing Plan 1 preserved by hand: `AddOp`'s `add-finished` fires on Drop
    // (before the `queue-updated` emit that follows it in `add_files`), so the UI spinner always
    // clears before the queue refetch signal — even for a trivially empty add.
    #[test]
    fn add_files_emits_finished_before_queue_updated() {
        // The plain `test_ctx` (PanickingLocator) default is load-bearing here: an empty add
        // returns before it reaches HandBrake resolution, so this asserts the event bracketing
        // AND that the early return is intact. It previously needed a StubLocator plus a seeded
        // preset cache purely because intake resolved the suffix template before looking at
        // `paths` — scaffolding for a bug, not for this test's subject.
        let (ctx, sink, _d) = test_ctx(test_conn());

        // an empty add still brackets: add-started → add-finished → queue-updated
        let _ = add_files(&ctx, &[]);
        let names: Vec<String> = sink
            .events
            .lock()
            .unwrap()
            .iter()
            .map(|(n, _)| n.clone())
            .collect();
        let fin = names
            .iter()
            .position(|n| n == "add-finished")
            .expect("add-finished emitted");
        let upd = names
            .iter()
            .position(|n| n == "queue-updated")
            .expect("queue-updated emitted");
        assert!(
            fin < upd,
            "spinner must clear before the queue refetch signal"
        );
    }

    #[test]
    fn add_files_reencodes_mp4_in_place_instead_of_skipping() {
        let conn = test_conn();
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("clip.mp4");
        std::fs::write(&src, b"x").unwrap();
        let src_str = src.to_string_lossy().to_string();

        let result = add_files_to_db(&conn, &[src_str.clone()], "preset", "", false).unwrap();

        assert_eq!(
            result.added.len(),
            1,
            "mp4 + empty suffix must queue an in-place job"
        );
        assert_eq!(
            result.added[0].output_path, src_str,
            "an in-place job stores output_path == source_path"
        );
        assert!(result.skipped.is_empty(), "an in-place job is not a skip");
    }

    // ---- cheap_skip_reason: identity is the authoritative "already converted" signal ----

    fn ids(pairs: &[(i64, i64)]) -> HashSet<(i64, i64)> {
        pairs.iter().copied().collect()
    }

    #[test]
    fn cheap_skip_reason_skips_on_matching_identity_regardless_of_name() {
        // The point of the fix: a file whose (size, mtime) matches a completed conversion is
        // already done even though its name matches nothing we recorded.
        let converted = ids(&[(500, 42)]);
        let reason = cheap_skip_reason(
            "/m/whatever.mp4",
            ".h265",
            Some((500, 42)),
            &HashSet::new(),
            &HashSet::new(),
            &converted,
        );
        assert_eq!(reason, Some(SkipReason::AlreadyConverted));
    }

    #[test]
    fn cheap_skip_reason_keeps_a_different_identity_at_a_recycled_path() {
        // A different video (different size/mtime) that recycled a converted file's name must NOT
        // be skipped — this is the exact bug. Neither the identity set nor legacy paths match.
        let converted = ids(&[(500, 42)]);
        let reason = cheap_skip_reason(
            "/m/recycled.mp4",
            ".h265",
            Some((999, 7)),
            &HashSet::new(),
            &HashSet::new(),
            &converted,
        );
        assert_eq!(reason, None);
    }

    #[test]
    fn cheap_skip_reason_never_matches_identity_when_stat_failed() {
        // Unreadable identity (None) must never match — uncertainty keeps the file (it converts).
        let converted = ids(&[(500, 42)]);
        let reason = cheap_skip_reason(
            "/m/gone.mp4",
            ".h265",
            None,
            &HashSet::new(),
            &HashSet::new(),
            &converted,
        );
        assert_eq!(reason, None);
    }

    #[test]
    fn cheap_skip_reason_legacy_path_still_skips_but_never_output_exists() {
        // Pre-migration fallback: a source_path in the legacy set skips as AlreadyConverted.
        let legacy: HashSet<String> = ["/m/old.mp4".to_string()].into_iter().collect();
        assert_eq!(
            cheap_skip_reason(
                "/m/old.mp4",
                ".h265",
                Some((1, 1)),
                &HashSet::new(),
                &legacy,
                &HashSet::new(),
            ),
            Some(SkipReason::AlreadyConverted),
        );
        // The suffix-ends guard is untouched.
        assert_eq!(
            cheap_skip_reason(
                "/m/clip.h265.mp4",
                ".h265",
                None,
                &HashSet::new(),
                &HashSet::new(),
                &HashSet::new(),
            ),
            Some(SkipReason::AlreadyConverted),
        );
        // OutputExists is gone: a bare unknown video is kept, never skipped by a filename check.
        assert_eq!(
            cheap_skip_reason(
                "/m/fresh.mp4",
                ".h265",
                Some((5, 5)),
                &HashSet::new(),
                &HashSet::new(),
                &HashSet::new(),
            ),
            None,
        );
    }

    // ---- choose_output_path: renumber the base name, never clobber, never renumber in-place ----

    /// Normalizes path separators so assertions written with `/` match the values
    /// `choose_output_path` builds with the platform separator (`\` on Windows).
    fn norm(path: &str) -> String {
        path.replace('\\', "/")
    }

    #[test]
    fn choose_output_path_renumbers_the_base_name_when_taken() {
        let free = |_: &str| false;
        assert_eq!(
            norm(&choose_output_path("/m/clip.mov", ".h265", &free)),
            "/m/clip.h265.mp4",
            "an untaken default name is used as-is"
        );

        let taken1: HashSet<String> = ["/m/clip.h265.mp4".to_string()].into_iter().collect();
        let is_taken1 = |n: &str| taken1.contains(&norm(n));
        assert_eq!(
            norm(&choose_output_path("/m/clip.mov", ".h265", &is_taken1)),
            "/m/clip (1).h265.mp4"
        );

        let taken2: HashSet<String> = ["/m/clip.h265.mp4", "/m/clip (1).h265.mp4"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let is_taken2 = |n: &str| taken2.contains(&norm(n));
        assert_eq!(
            norm(&choose_output_path("/m/clip.mov", ".h265", &is_taken2)),
            "/m/clip (2).h265.mp4"
        );
    }

    #[test]
    fn choose_output_path_never_renumbers_an_in_place_job() {
        // An in-place source's own on-disk presence must never trigger a rename, or the converter
        // would route it through the distinct-file overwrite path.
        let always_taken = |_: &str| true;
        assert_eq!(
            norm(&choose_output_path("/m/clip.mp4", "", &always_taken)),
            "/m/clip.mp4"
        );
    }

    #[test]
    fn choose_output_path_dedupes_within_a_batch() {
        let mut assigned: HashSet<String> = HashSet::new();
        let first = {
            let is_taken = |n: &str| assigned.contains(n);
            choose_output_path("/m/clip.mov", ".h265", &is_taken)
        };
        assigned.insert(first.clone());
        let second = {
            let is_taken = |n: &str| assigned.contains(n);
            choose_output_path("/m/clip.mov", ".h265", &is_taken)
        };
        assert_eq!(norm(&first), "/m/clip.h265.mp4");
        assert_eq!(
            norm(&second),
            "/m/clip (1).h265.mp4",
            "a batch never assigns one name twice"
        );
    }

    // ---- fetch_skip_sets: always-on identity vs. flag-gated legacy fallback ----

    #[test]
    fn fetch_skip_sets_fingerprinted_row_is_identity_only_and_flag_independent() {
        let conn = test_conn();
        insert_done_with_identity(&conn, "h1", "/m/a.mp4", "/m/a.h265.mp4", 500, 42);

        for flag in [false, true] {
            let (_, legacy, identities) = fetch_skip_sets(&conn, flag).unwrap();
            assert!(
                identities.contains(&(500, 42)),
                "a fingerprinted row is always in the identity set (flag={flag})"
            );
            assert!(
                !legacy.contains("/m/a.mp4"),
                "a fingerprinted non-in-place row must NOT be path-matched (recycling guard, flag={flag})"
            );
        }
    }

    #[test]
    fn fetch_skip_sets_null_mtime_row_is_legacy_only_when_flag_on() {
        let conn = test_conn();
        // insert_history writes no fingerprint -> a pre-migration row.
        insert_history(
            &conn,
            "h1",
            "/m/b.mkv",
            "done",
            0,
            1000,
            "2020-01-01T00:00:00Z",
        );

        let (_, legacy_off, _) = fetch_skip_sets(&conn, false).unwrap();
        assert!(
            !legacy_off.contains("/m/b.mkv"),
            "flag off: no legacy path skip"
        );

        let (_, legacy_on, _) = fetch_skip_sets(&conn, true).unwrap();
        assert!(
            legacy_on.contains("/m/b.mkv"),
            "flag on: legacy path skip preserved"
        );
    }

    #[test]
    fn fetch_skip_sets_null_mtime_in_place_row_is_always_legacy() {
        let conn = test_conn();
        // A pre-migration in-place row (source == output by Path equality, note the `//`) must be
        // path-skipped regardless of the flag, or re-scanning it triggers a re-encode cascade.
        conn.execute(
            "INSERT INTO jobs (id, source_path, output_path, preset, status, queue_order, created_at)
             VALUES ('h1', '/m//c.mp4', '/m/c.mp4', 'preset', 'done', 0, '2020-01-01T00:00:00Z')",
            [],
        )
        .unwrap();

        for flag in [false, true] {
            let (_, legacy, _) = fetch_skip_sets(&conn, flag).unwrap();
            assert!(
                legacy.contains("/m//c.mp4"),
                "a legacy in-place row is path-skipped regardless of flag (flag={flag})"
            );
        }
    }

    // ---- end-to-end recycling scenarios against real files ----

    #[test]
    fn add_files_skips_a_recycled_path_whose_identity_matches() {
        let conn = test_conn();
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("movie.mkv");
        std::fs::write(&src, b"same-content").unwrap();
        let src_str = src.to_string_lossy().to_string();
        let id = crate::probe_cache::file_identity(&src_str).unwrap();
        insert_done_with_identity(
            &conn,
            "h1",
            &src_str,
            "/m/movie.h265.mp4",
            id.size,
            id.mtime,
        );

        let result = add_files_to_db(&conn, &[src_str], "preset", ".h265", false).unwrap();

        assert!(
            result.added.is_empty(),
            "an unchanged, already-converted file is skipped"
        );
        assert_eq!(
            result.skipped,
            vec![SkipCount {
                reason: SkipReason::AlreadyConverted,
                count: 1
            }]
        );
    }

    #[test]
    fn add_files_converts_and_renumbers_when_a_different_file_recycles_the_path() {
        // The reported bug: source A is converted, its path is later reused by a DIFFERENT video B
        // while A's output still sits on disk. B must be queued (not skipped) to a renumbered
        // output, leaving A's output untouched.
        let conn = test_conn();
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("movie.mkv");
        std::fs::write(&src, b"A").unwrap();
        let src_str = src.to_string_lossy().to_string();
        let id_a = crate::probe_cache::file_identity(&src_str).unwrap();

        let old_output = dir.path().join("movie.h265.mp4");
        std::fs::write(&old_output, b"A-converted").unwrap();
        insert_done_with_identity(
            &conn,
            "h1",
            &src_str,
            &old_output.to_string_lossy(),
            id_a.size,
            id_a.mtime,
        );

        // A different video B recycles the same path.
        std::fs::write(&src, b"B is a completely different, longer video").unwrap();

        let result = add_files_to_db(&conn, &[src_str], "preset", ".h265", false).unwrap();

        assert_eq!(
            result.added.len(),
            1,
            "the different file must be queued, not skipped"
        );
        assert!(
            result.added[0].output_path.ends_with("movie (1).h265.mp4"),
            "queued to a renumbered output, got {}",
            result.added[0].output_path
        );
        assert!(result.skipped.is_empty());
        assert_eq!(
            std::fs::read(&old_output).unwrap(),
            b"A-converted",
            "the earlier conversion's output must never be clobbered"
        );
    }

    // ---- probe_candidates (source-media probe is only worth running on these) ----

    #[test]
    fn probe_candidates_excludes_files_the_cheap_checks_would_skip() {
        // The whole point of probing only candidates: a re-scan of an already-handled folder must
        // not pay for a HandBrake probe on files the cheap checks already reject.
        let conn = test_conn();
        insert_queued(&conn, "j1", "/movies/queued.mp4", "queued", 1);
        insert_history(
            &conn,
            "h1",
            "/movies/done.mp4",
            "done",
            0,
            0,
            "2020-01-01T00:00:00Z",
        );

        let paths = vec![
            "/movies/queued.mp4".to_string(), // already queued -> never probed
            "/movies/done.mp4".to_string(),   // in history -> probed only when the flag is off
            "/movies/fresh.mp4".to_string(),  // new -> always a probe candidate
            "/movies/notes.txt".to_string(),  // not a video -> never probed
        ];

        // skip_already_converted ON: the history file is excluded alongside the queued one.
        let with_flag = probe_candidates(&conn, &paths, "", true).unwrap();
        assert_eq!(with_flag, vec!["/movies/fresh.mp4".to_string()]);

        // Flag OFF: the history file is no longer a cheap skip, so it becomes a probe candidate.
        let without_flag = probe_candidates(&conn, &paths, "", false).unwrap();
        assert_eq!(
            without_flag,
            vec![
                "/movies/done.mp4".to_string(),
                "/movies/fresh.mp4".to_string()
            ]
        );
    }

    // ---- get_next_queue_order ----

    #[test]
    fn next_queue_order_starts_at_one_then_follows_max_active() {
        let conn = test_conn();
        assert_eq!(
            get_next_queue_order(&conn).unwrap(),
            1,
            "empty queue starts at 1"
        );

        insert_queued(&conn, "a", "/m/a.mp4", "queued", 3);
        assert_eq!(
            get_next_queue_order(&conn).unwrap(),
            4,
            "next is max active order + 1"
        );

        // A finished job with a high order must not influence the next active order.
        insert_history(&conn, "h", "/m/h.mp4", "done", 0, 0, "2020-01-01T00:00:00Z");
        conn.execute("UPDATE jobs SET queue_order = 100 WHERE id = 'h'", [])
            .unwrap();
        assert_eq!(
            get_next_queue_order(&conn).unwrap(),
            4,
            "done jobs are excluded from the active max"
        );
    }

    // ---- reorder_queue_inner ----

    #[test]
    fn reorder_queue_reassigns_orders_in_listed_sequence() {
        let conn = test_conn();
        insert_queued(&conn, "A", "/m/a.mp4", "queued", 0);
        insert_queued(&conn, "B", "/m/b.mp4", "queued", 1);
        insert_queued(&conn, "C", "/m/c.mp4", "queued", 2);

        reorder_queue_inner(&conn, &["C".to_string(), "A".to_string(), "B".to_string()]).unwrap();

        let order = |id: &str| -> i32 {
            conn.query_row(
                "SELECT queue_order FROM jobs WHERE id = ?1",
                params![id],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert_eq!(order("C"), 0);
        assert_eq!(order("A"), 1);
        assert_eq!(order("B"), 2);
    }

    // ---- remove_history_entry_inner ----

    #[test]
    fn remove_history_entry_deletes_terminal_rows_only() {
        let conn = test_conn();
        insert_history(
            &conn,
            "done1",
            "/m/a.mp4",
            "done",
            100,
            1000,
            "2020-01-02T00:00:00Z",
        );
        insert_history(
            &conn,
            "err1",
            "/m/b.mp4",
            "error",
            0,
            1000,
            "2020-01-02T00:00:00Z",
        );
        insert_queued(&conn, "q1", "/m/c.mp4", "queued", 0);

        remove_history_entry_inner(&conn, "done1").unwrap();
        remove_history_entry_inner(&conn, "err1").unwrap();
        // A queued job must survive a history delete for its id.
        remove_history_entry_inner(&conn, "q1").unwrap();

        let status_count = |status: &str| -> i64 {
            conn.query_row(
                "SELECT COUNT(*) FROM jobs WHERE status = ?1",
                params![status],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert_eq!(status_count("done"), 0);
        assert_eq!(status_count("error"), 0);
        assert_eq!(status_count("queued"), 1, "queued row must not be deleted");
    }

    // ---- get_history search / sort / pagination ----

    #[test]
    fn history_search_filters_by_path() {
        let conn = test_conn();
        insert_history(
            &conn,
            "1",
            "/m/alpha.mp4",
            "done",
            10,
            100,
            "2020-01-01T00:00:00Z",
        );
        insert_history(
            &conn,
            "2",
            "/m/beta.mp4",
            "done",
            20,
            200,
            "2020-01-02T00:00:00Z",
        );

        let page = get_history_inner(&conn, 10, 0, Some("alpha".to_string()), None).unwrap();
        assert_eq!(page.total, 1);
        assert_eq!(page.jobs.len(), 1);
        assert_eq!(page.jobs[0].source_path, "/m/alpha.mp4");
    }

    #[test]
    fn history_sorts_by_space_saved_and_source_path() {
        let conn = test_conn();
        insert_history(
            &conn,
            "1",
            "/m/c.mp4",
            "done",
            10,
            100,
            "2020-01-01T00:00:00Z",
        );
        insert_history(
            &conn,
            "2",
            "/m/a.mp4",
            "done",
            100,
            200,
            "2020-01-02T00:00:00Z",
        );
        insert_history(
            &conn,
            "3",
            "/m/b.mp4",
            "done",
            50,
            300,
            "2020-01-03T00:00:00Z",
        );

        let by_saved =
            get_history_inner(&conn, 10, 0, None, Some("space_saved".to_string())).unwrap();
        assert_eq!(
            by_saved.jobs[0].space_saved,
            Some(100),
            "space_saved sorts descending"
        );

        let by_path =
            get_history_inner(&conn, 10, 0, None, Some("source_path".to_string())).unwrap();
        assert_eq!(
            by_path.jobs[0].source_path, "/m/a.mp4",
            "source_path sorts ascending"
        );
    }

    #[test]
    fn history_default_sort_is_newest_completed_first() {
        let conn = test_conn();
        insert_history(
            &conn,
            "old",
            "/m/old.mp4",
            "done",
            1,
            1,
            "2020-01-01T00:00:00Z",
        );
        insert_history(
            &conn,
            "new",
            "/m/new.mp4",
            "done",
            1,
            1,
            "2024-12-31T00:00:00Z",
        );

        let page = get_history_inner(&conn, 10, 0, None, None).unwrap();
        assert_eq!(page.jobs[0].id, "new", "default order is completed_at DESC");
    }

    #[test]
    fn history_paginates_with_limit_and_offset() {
        let conn = test_conn();
        for i in 0..3 {
            insert_history(
                &conn,
                &format!("j{i}"),
                &format!("/m/{i}.mp4"),
                "done",
                1,
                1,
                "2020-01-01T00:00:00Z",
            );
        }

        let first = get_history_inner(&conn, 2, 0, None, None).unwrap();
        assert_eq!(first.jobs.len(), 2);
        assert_eq!(first.total, 3, "total reflects all rows, not the page size");

        let second = get_history_inner(&conn, 2, 2, None, None).unwrap();
        assert_eq!(second.jobs.len(), 1, "offset returns the remaining page");
    }

    // ---- get_history_summary ----

    #[test]
    fn history_summary_sums_only_done_jobs() {
        let conn = test_conn();
        insert_history(
            &conn,
            "1",
            "/m/a.mp4",
            "done",
            100,
            1000,
            "2020-01-01T00:00:00Z",
        );
        insert_history(
            &conn,
            "2",
            "/m/b.mp4",
            "done",
            200,
            2000,
            "2020-01-02T00:00:00Z",
        );
        // Errors live in history but never count as saved space.
        insert_history(
            &conn,
            "3",
            "/m/c.mp4",
            "error",
            999,
            3000,
            "2020-01-03T00:00:00Z",
        );

        let summary = get_history_summary_inner(&conn, None).unwrap();
        assert_eq!(summary.total_saved_bytes, 300);
        assert_eq!(summary.total_files, 2);
    }

    #[test]
    fn history_summary_respects_search() {
        let conn = test_conn();
        insert_history(
            &conn,
            "1",
            "/m/keep.mp4",
            "done",
            100,
            1000,
            "2020-01-01T00:00:00Z",
        );
        insert_history(
            &conn,
            "2",
            "/m/other.mp4",
            "done",
            200,
            2000,
            "2020-01-02T00:00:00Z",
        );

        let summary = get_history_summary_inner(&conn, Some("keep".to_string())).unwrap();
        assert_eq!(summary.total_saved_bytes, 100);
        assert_eq!(summary.total_files, 1);
    }

    // End-to-end (local only): needs ffmpeg to synthesize clips and HandBrakeCLI to probe them.
    // Drives the whole skip-by-source-media path through add_files_inner: an at-target source is
    // dropped and reported as AlreadyAtTarget, a codec-upgrade source is queued. Run with:
    //   cargo test -- --ignored add_files_inner_skips_at_target_source_end_to_end
    #[test]
    #[ignore]
    fn add_files_inner_skips_at_target_source_end_to_end() {
        let conn = test_conn();
        let preset: String = conn
            .query_row("SELECT value FROM settings WHERE key = 'preset'", [], |r| {
                r.get(0)
            })
            .unwrap();
        conn.execute(
            "UPDATE settings SET value = 'true' WHERE key = 'skip_by_source_media'",
            [],
        )
        .unwrap();
        // PathLocator, not the fixture default: this e2e genuinely wants the host's real
        // HandBrakeCLI to probe the synthesized clips — the one place reading the machine is
        // the point rather than an accident.
        let (ctx, _sink, _disposer) =
            test_ctx_with_locator(conn, Arc::new(crate::handbrake::PathLocator));
        // Pin the target to h265/1080p without shelling out to HandBrake for preset metadata.
        ctx.preset_cache.lock().unwrap().insert(
            preset,
            crate::handbrake::PresetMetadata {
                codec: "h265".into(),
                resolution: "1080p".into(),
                quality: "hq".into(),
                preset: "p".into(),
                device: String::new(),
            },
        );

        let dir = tempfile::tempdir().unwrap();
        let make = |name: &str, vcodec: &str| {
            let path = dir.path().join(name);
            let ok = std::process::Command::new("ffmpeg")
                .args([
                    "-y",
                    "-f",
                    "lavfi",
                    "-i",
                    "testsrc=duration=0.3:size=1920x1080:rate=12",
                    "-pix_fmt",
                    "yuv420p",
                    "-c:v",
                    vcodec,
                ])
                .arg(&path)
                .status()
                .expect("run ffmpeg")
                .success();
            assert!(ok, "ffmpeg failed for {name}");
            path.to_str().unwrap().to_string()
        };
        let at_target = make("a.mp4", "libx265"); // h265 1080p -> skip
        let upgrade = make("b.mp4", "libx264"); // h264 1080p -> queue

        let inputs = vec![at_target, upgrade];
        let result = add_files_inner(&ctx, &inputs, None).unwrap();

        assert_eq!(result.added.len(), 1, "only the h264 source is queued");
        assert!(result.added[0].source_path.ends_with("b.mp4"));
        let reported = result
            .skipped
            .iter()
            .find(|c| c.reason == SkipReason::AlreadyAtTarget);
        assert_eq!(
            reported.map(|c| c.count),
            Some(1),
            "the h265 source must be reported as already-at-target, not silently dropped"
        );

        // Second pass over the same inputs: the at-target source's identity is unchanged, so
        // its media is served from probe_cache (zero re-probe), and the codec-upgrade source
        // is now already queued. The at-target source must STILL be reported skipped —
        // proving the cached media drives the same decision as a live probe.
        let again = add_files_inner(&ctx, &inputs, None).unwrap();
        assert!(
            again.added.is_empty(),
            "nothing new to queue on a repeat add"
        );
        let at_target_again = again
            .skipped
            .iter()
            .find(|c| c.reason == SkipReason::AlreadyAtTarget);
        assert_eq!(
            at_target_again.map(|c| c.count),
            Some(1),
            "the cached at-target source is still recognized on re-scan"
        );
    }

    // Clearing the queue must also drop any low-disk pause reason: otherwise a Some reason
    // outlives the jobs that justified it, and a QueuePage remount re-seeds the "low disk"
    // banner over an empty queue (the async seed races past the clear-on-empty effect).
    #[test]
    fn clear_queue_drops_the_low_disk_pause_reason() {
        use crate::converter::LowDiskPause;

        let conn = test_conn();
        insert_queued(&conn, "j1", "/m/a.mp4", "queued", 0);
        let (ctx, _sink, _disposer) = test_ctx(conn);

        *ctx.converter.low_disk_pause.lock().unwrap() = Some(LowDiskPause {
            path: "/m/a.mp4.out".into(),
            available_bytes: 3,
            required_bytes: 5,
        });

        clear_queue(&ctx).unwrap();

        let remaining: i64 = ctx
            .db
            .lock()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM jobs WHERE status = 'queued'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(remaining, 0, "clearing the queue deletes the queued job");
        assert!(
            ctx.converter.low_disk_pause().is_none(),
            "clearing the queue also drops the low-disk pause reason so it can't re-seed the banner"
        );
    }

    #[test]
    fn clear_queue_clears_the_persisted_pause() {
        let conn = test_conn();
        // A remembered pause + a queued job.
        conn.execute(
            "INSERT INTO settings (key, value) VALUES ('queue_paused', 'true')
             ON CONFLICT(key) DO UPDATE SET value = 'true'",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO jobs (id, source_path, output_path, preset, status, queue_order, created_at)
             VALUES ('j', '/s.mp4', '/o.mp4', 'p', 'queued', 0, '2020-01-01T00:00:00Z')",
            [],
        )
        .unwrap();
        let (ctx, _sink, _disposer) = test_ctx(conn);

        clear_queue(&ctx).unwrap();

        let db = ctx.db.lock().unwrap();
        let paused: String = db
            .query_row(
                "SELECT value FROM settings WHERE key='queue_paused'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            paused, "false",
            "clearing the queue drops the remembered pause"
        );
        let n: i64 = db
            .query_row("SELECT COUNT(*) FROM jobs WHERE status='queued'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(n, 0);
    }

    // ---- bad-source review list + purge ----

    fn insert_error_row(conn: &Connection, id: &str, path: &str, class: &str) {
        // queue_order and created_at are NOT NULL with no default; the review-list/purge
        // logic under test doesn't touch either, so any fixed value satisfies the schema.
        conn.execute(
            "INSERT INTO jobs (id, source_path, output_path, preset, status, failure_class,
                               queue_order, created_at, completed_at)
             VALUES (?1, ?2, '/o.mp4', 'p', 'error', ?3, 0, '2026-07-25T10:00:00Z',
                     '2026-07-25T10:00:00Z')",
            params![id, path, class],
        )
        .unwrap();
    }

    /// Record the CURRENT on-disk fingerprint so the purge identity check passes. Without
    /// this the row looks like a pre-feature NULL-fingerprint row and purge refuses.
    fn stamp_identity(conn: &Connection, id: &str, path: &str) {
        let ident = crate::probe_cache::file_identity(path).expect("file exists");
        conn.execute(
            "UPDATE jobs SET source_size = ?2, source_mtime = ?3 WHERE id = ?1",
            params![id, ident.size, ident.mtime],
        )
        .unwrap();
    }

    /// R3: `purge_one_locked` now takes `handbrake_path` as an already-resolved value instead of
    /// detecting it itself, since production resolves it ONCE per batch outside the lock (see
    /// `purge_bad_sources`). This mirrors that same resolution for tests whose row's class
    /// actually reaches the rescan rung and needs a real answer (from the `handbrake_path`
    /// setting the test configured).
    fn resolve_handbrake_for_test(ctx: &Arc<Ctx>) -> Result<String, String> {
        let conn = ctx.db.lock().unwrap();
        get_handbrake_path(&conn, &*ctx.handbrake)
    }

    /// Placeholder for `purge_one_locked` tests whose row never reaches the rescan rung (blocked
    /// earlier by InUse, AlreadyGone, Changed, or eligibility) — the value is never consumed, so
    /// it's deliberately not a real detection (no `which`/`where` subprocess spawn).
    fn handbrake_path_not_needed() -> Result<String, String> {
        Err("not needed for this test".to_string())
    }

    #[test]
    fn get_bad_sources_lists_both_bad_classes_and_excludes_everything_else() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::init_db(&conn).unwrap();
        // completed_at is set explicitly: the query orders by it, and NULLs would make the
        // assertion order-dependent on SQLite's row layout.
        for (id, status, class, done_at) in [
            ("a", "error", Some("bad_source"), "2026-07-25T10:00:00Z"),
            (
                "b",
                "error",
                Some("bad_source_truncated"),
                "2026-07-25T09:00:00Z",
            ),
            ("c", "error", Some("environment"), "2026-07-25T08:00:00Z"),
            ("d", "error", Some("unknown"), "2026-07-25T07:00:00Z"),
            (
                "e",
                "error",
                Some("bad_source_purged"),
                "2026-07-25T06:00:00Z",
            ),
            ("f", "error", None, "2026-07-25T05:00:00Z"),
            ("g", "done", Some("bad_source"), "2026-07-25T04:00:00Z"),
        ] {
            conn.execute(
                "INSERT INTO jobs (id, source_path, output_path, preset, status, failure_class,
                                   queue_order, created_at, completed_at)
                 VALUES (?1, '/s.mkv', '/o.mp4', 'p', ?2, ?3, 0, ?4, ?4)",
                params![id, status, class, done_at],
            )
            .unwrap();
        }
        // Goes through get_bad_sources_inner (the exact query the command runs), not a
        // hand-duplicated copy — so a drift in its WHERE clause or SELECT column list would
        // fail here instead of staying invisible behind a shadow test.
        let ids: Vec<String> = get_bad_sources_inner(&conn)
            .unwrap()
            .into_iter()
            .map(|j| j.id)
            .collect();
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
        // F7: all three LIVE statuses must block a destroy, not just 'queued' — mutation testing
        // showed narrowing path_is_in_use's `IN ('queued', 'encoding', 'paused')` down to just
        // `IN ('queued')` survived the whole suite, because only a 'queued' sibling was ever
        // tested. 'encoding' is the sharpest case: it is the running job's OWN source file.
        for live_status in ["queued", "encoding", "paused"] {
            let conn = Connection::open_in_memory().unwrap();
            crate::db::init_db(&conn).unwrap();
            let dir = tempfile::tempdir().unwrap();
            let f = dir.path().join("movie.mkv");
            std::fs::write(&f, b"content").unwrap();
            let p = f.to_str().unwrap();

            // The bad-source row, plus a re-added copy of the same file now live.
            insert_error_row(&conn, "old", p, "bad_source");
            conn.execute(
                "INSERT INTO jobs (id, source_path, output_path, preset, status, queue_order,
                                   created_at)
                 VALUES ('new', ?1, '/o.mp4', 'p', ?2, 0, '2026-07-25T10:00:00Z')",
                params![p, live_status],
            )
            .unwrap();
            let db = Arc::new(Mutex::new(conn));

            let outcome = purge_one_locked(
                &db,
                "old",
                PurgeAction::Delete,
                &handbrake_path_not_needed(),
                &DeleteDisposer,
            );
            assert_eq!(
                outcome,
                PurgeOutcome::InUse,
                "a '{live_status}' job depending on this path must block the purge"
            );
            assert!(
                f.exists(),
                "a file a '{live_status}' job depends on must never be destroyed"
            );
        }
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
        let db = Arc::new(Mutex::new(conn));

        let outcome = purge_one_locked(
            &db,
            "old",
            PurgeAction::Delete,
            &handbrake_path_not_needed(),
            &DeleteDisposer,
        );
        assert_eq!(outcome, PurgeOutcome::Changed);
        assert!(
            f.exists(),
            "a stale verdict must not condemn a re-downloaded file"
        );
    }

    #[test]
    fn purge_reports_already_gone_without_failing() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::init_db(&conn).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("never-existed.mkv");
        insert_error_row(&conn, "old", missing.to_str().unwrap(), "bad_source");
        let db = Arc::new(Mutex::new(conn));

        let outcome = purge_one_locked(
            &db,
            "old",
            PurgeAction::Delete,
            &handbrake_path_not_needed(),
            &DeleteDisposer,
        );
        assert_eq!(outcome, PurgeOutcome::AlreadyGone);
        // A file the user already deleted by hand must leave the review list too — otherwise
        // it reappears and reports AlreadyGone again on every future press, and the list can
        // never be emptied.
        assert!(
            get_bad_sources_inner(&db.lock().unwrap())
                .unwrap()
                .is_empty(),
            "an already-gone row must drop out of the list"
        );
        let still_there: i64 = db
            .lock()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM jobs WHERE id = 'old'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(still_there, 1, "the history entry itself survives");
    }

    // I1 regression test: an offline external/network volume makes EVERY path under it read as
    // missing, identically to a genuinely deleted file. Before this fix, purge stamped every
    // such row purged on the first press — permanently erasing it from the review list the
    // instant the volume was unplugged, with no way back once it returned. The parent directory
    // being unreachable too (not just the leaf file) is what must block that stamp.
    #[test]
    fn purge_reports_unverifiable_when_the_parent_directory_is_also_missing() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::init_db(&conn).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let volume = dir.path().join("volume");
        std::fs::create_dir(&volume).unwrap();
        let f = volume.join("movie.mkv");
        std::fs::write(&f, b"content").unwrap();
        let p = f.to_str().unwrap().to_string();
        insert_error_row(&conn, "old", &p, "bad_source");
        stamp_identity(&conn, "old", &p);

        // Simulate the volume going offline: the whole directory tree disappears, not just the
        // leaf file — unlike purge_reports_already_gone_without_failing, whose parent tempdir
        // stays reachable.
        std::fs::remove_dir_all(&volume).unwrap();
        let db = Arc::new(Mutex::new(conn));

        let outcome = purge_one_locked(
            &db,
            "old",
            PurgeAction::Delete,
            &handbrake_path_not_needed(),
            &DeleteDisposer,
        );
        assert_eq!(
            outcome,
            PurgeOutcome::Unverifiable,
            "an unreachable parent must not be treated as proof the file was deleted"
        );
        let ids: Vec<String> = get_bad_sources_inner(&db.lock().unwrap())
            .unwrap()
            .into_iter()
            .map(|j| j.id)
            .collect();
        assert_eq!(
            ids,
            vec!["old".to_string()],
            "the row must stay in the review list so it can be retried once the volume returns"
        );
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
        let db = Arc::new(Mutex::new(conn));

        let outcome = purge_one_locked(
            &db,
            "old",
            PurgeAction::Delete,
            &handbrake_path_not_needed(),
            &DeleteDisposer,
        );
        assert_eq!(outcome, PurgeOutcome::Purged);
        assert!(!f.exists(), "delete mode removes the file");
        assert!(
            get_bad_sources_inner(&db.lock().unwrap())
                .unwrap()
                .is_empty(),
            "a purged row must drop out of the list or a second press just errors"
        );
        let still_there: i64 = db
            .lock()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM jobs WHERE id = 'old'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(still_there, 1, "the history entry itself survives");
    }

    // Pins the DI wiring at purge_bad_sources' own call site (`&*ctx.disposer` in the
    // purge_one_locked call), not just the lower-level purge_one_locked/destroy_and_record
    // functions the tests above and below exercise directly with their own injected disposer.
    // Every other purge test in this file enters one level down, so none of them can catch a
    // mutation that swaps `&*ctx.disposer` for a hardcoded delete primitive at that call site —
    // this is the one test that goes in through the actual public entry point and checks the
    // disposer that's WIRED INTO ctx (via test_ctx) is the one that performed the destroy.
    #[test]
    fn purge_bad_sources_destroys_through_the_ctx_disposer() {
        let conn = test_conn();
        conn.execute(
            "UPDATE settings SET value = 'trash' WHERE key = 'bad_source_action'",
            [],
        )
        .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("movie.mkv");
        std::fs::write(&f, b"garbage").unwrap();
        let p = f.to_str().unwrap().to_string();
        insert_error_row(&conn, "old", &p, "bad_source_truncated");
        stamp_identity(&conn, "old", &p);
        // The assertion is about the disposer, not about HandBrake: this row's class is
        // bad_source_truncated, which never reaches the rescan rung, so the resolved path is
        // never consumed. AbsentLocator says so instead of letting the host answer.
        let (ctx, _sink, disposer) =
            test_ctx_with_locator(conn, Arc::new(crate::handbrake::AbsentLocator));

        let results = purge_bad_sources(&ctx, vec!["old".to_string()]).unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].outcome, PurgeOutcome::Purged);
        assert_eq!(
            disposer.0.lock().unwrap().as_slice(),
            [p],
            "purge_bad_sources must destroy through ctx.disposer, not some other hardcoded \
             primitive — a mutation swapping in DeleteDisposer (or a raw remove_file) still \
             deletes the file, but leaves the ctx-wired RecordingDisposer's record empty"
        );
    }

    // ---- destructive-path review findings (C1, I2, I3, I5, I6) ----

    #[test]
    fn purge_refuses_an_id_that_is_not_an_eligible_bad_source_error() {
        // The row lookup itself enforces eligibility (status='error' AND failure_class IN
        // (bad_source, bad_source_truncated)). Guards the exact scenario a review flagged: a UI
        // wiring mistake (e.g. sending the History list's ids instead of the review list's)
        // selecting a `done` row — for an in-place conversion, that "source" is the user's only
        // remaining copy of the file, and every one of the five safety rungs would otherwise
        // verify it right up to destruction (not queued -> not InUse; exists; identity
        // untouched since completion -> matches; class isn't bad_source -> no rescan gate).
        let conn = Connection::open_in_memory().unwrap();
        crate::db::init_db(&conn).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("movie.mkv");
        std::fs::write(&f, b"finished in-place conversion").unwrap();
        let p = f.to_str().unwrap();
        conn.execute(
            "INSERT INTO jobs (id, source_path, output_path, preset, status, queue_order,
                               created_at, completed_at)
             VALUES ('done1', ?1, ?1, 'p', 'done', 0, '2026-07-25T10:00:00Z',
                     '2026-07-25T10:00:00Z')",
            params![p],
        )
        .unwrap();
        let db = Arc::new(Mutex::new(conn));

        let outcome = purge_one_locked(
            &db,
            "done1",
            PurgeAction::Delete,
            &handbrake_path_not_needed(),
            &DeleteDisposer,
        );
        assert_eq!(outcome, PurgeOutcome::Failed);
        assert!(
            f.exists(),
            "a completed job's file must never be destroyed, even if the wrong id reaches purge"
        );
    }

    // Pins the exact mapping I2's fix depends on. If someone later collapses NoTitle and
    // CouldNotRun back into one arm — the mistake round 1 originally made — this fails
    // regardless of whether HandBrakeCLI is installed on the machine running the test.
    #[test]
    fn rescan_verdict_maps_each_scan_outcome_independently() {
        assert_eq!(
            rescan_verdict(crate::probe::ScanOutcome::Titled(
                crate::media_skip::SourceMedia {
                    codec: "h264".to_string(),
                    height: 1080
                }
            )),
            RescanVerdict::Recovered,
            "a title read on rescan means the original verdict was a transient fault"
        );
        assert_eq!(
            rescan_verdict(crate::probe::ScanOutcome::NoTitle),
            RescanVerdict::Destroy,
            "a scan that ran to completion and still found nothing is a real, \
             re-confirmed verdict — this is the primary case the feature exists for"
        );
        assert_eq!(
            rescan_verdict(crate::probe::ScanOutcome::CouldNotRun),
            RescanVerdict::Unverifiable,
            "a scan that never ran says nothing about the file and must never be treated as \
             confirmation"
        );
    }

    #[test]
    fn purge_marks_unverifiable_when_the_rescan_cannot_run() {
        // A configured handbrake_path that EXISTS (so get_handbrake_path returns Ok without
        // falling back to a real PATH search — keeping this test deterministic regardless of
        // whether the host running it has a real HandBrakeCLI installed) but is a directory,
        // not an executable, makes the scan's spawn fail (ScanOutcome::CouldNotRun). An
        // end-to-end companion to rescan_verdict_maps_each_scan_outcome_independently, through
        // the real purge_one_locked path rather than the pure mapping alone.
        let conn = Connection::open_in_memory().unwrap();
        crate::db::init_db(&conn).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("movie.mkv");
        std::fs::write(&f, b"content").unwrap();
        let p = f.to_str().unwrap();
        insert_error_row(&conn, "old", p, "bad_source");
        stamp_identity(&conn, "old", p);
        conn.execute(
            "UPDATE settings SET value = ?1 WHERE key = 'handbrake_path'",
            params![dir.path().to_str().unwrap()],
        )
        .unwrap();
        let (ctx, _sink, _disposer) = test_ctx(conn);
        let handbrake_path = resolve_handbrake_for_test(&ctx);

        let outcome = purge_one_locked(
            &ctx.db,
            "old",
            PurgeAction::Delete,
            &handbrake_path,
            &DeleteDisposer,
        );
        assert_eq!(outcome, PurgeOutcome::Unverifiable);
        assert!(
            f.exists(),
            "a rescan that could not even run must never be treated as confirming the file bad"
        );
    }

    // F9: stand-in HandBrake `--scan` scripts for the three purge tests below that drive a real
    // rescan through `purge_one_locked`. Both platform variants live here so all three tests run
    // on Windows CI too — before this, they were `#[cfg(unix)]` with inline `/bin/sh` scripts,
    // so `destroy_and_record` never executed on Windows at all.

    /// A stand-in that runs to completion but emits no parseable title set
    /// (`ScanOutcome::NoTitle`) — the shape of a real scan that opened the file fine and found
    /// nothing.
    fn no_title_scan_script(dir: &std::path::Path) -> std::path::PathBuf {
        #[cfg(windows)]
        {
            let p = dir.join("fake-handbrake.cmd");
            std::fs::write(&p, "@echo off\r\necho no title here\r\n@exit /b 0\r\n").unwrap();
            p
        }
        #[cfg(not(windows))]
        {
            use std::os::unix::fs::PermissionsExt;
            let p = dir.join("fake-handbrake.sh");
            std::fs::write(&p, "#!/bin/sh\necho 'no title here'\nexit 0\n").unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
            p
        }
    }

    /// A stand-in that runs to completion and DOES emit a parseable title set
    /// (`ScanOutcome::Titled`) — the shape of a real scan succeeding on a rescan after the
    /// original scan failed to run.
    fn titled_scan_script(dir: &std::path::Path) -> std::path::PathBuf {
        #[cfg(windows)]
        {
            let p = dir.join("fake-handbrake-titled.cmd");
            std::fs::write(
                &p,
                "@echo off\r\necho JSON Title Set: {\"TitleList\":[{\"Geometry\":{\"Height\":1080},\"VideoCodec\":\"h264\"}]}\r\n@exit /b 0\r\n",
            )
            .unwrap();
            p
        }
        #[cfg(not(windows))]
        {
            use std::os::unix::fs::PermissionsExt;
            let p = dir.join("fake-handbrake-titled.sh");
            std::fs::write(
                &p,
                "#!/bin/sh\nprintf 'JSON Title Set: {\"TitleList\":[{\"Geometry\":{\"Height\":1080},\"VideoCodec\":\"h264\"}]}'\nexit 0\n",
            )
            .unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
            p
        }
    }

    /// A stand-in that overwrites `target` (standing in for an in-place re-conversion completing
    /// mid-scan) before reporting no title and exiting cleanly.
    fn scan_script_that_overwrites_then_reports_no_title(
        dir: &std::path::Path,
        target: &str,
    ) -> std::path::PathBuf {
        #[cfg(windows)]
        {
            let p = dir.join("fake-handbrake-overwrite.cmd");
            // Redirect BEFORE the write, same reason as bad_source_fake_handbrake_script
            // (converter.rs): cmd.exe keeps a space in front of a trailing redirect token.
            std::fs::write(
                &p,
                format!(
                    "@echo off\r\n>\"{target}\" echo freshly converted, different bytes\r\n\
                     echo no title here\r\n@exit /b 0\r\n"
                ),
            )
            .unwrap();
            p
        }
        #[cfg(not(windows))]
        {
            use std::os::unix::fs::PermissionsExt;
            let p = dir.join("fake-handbrake-overwrite.sh");
            std::fs::write(
                &p,
                format!(
                    "#!/bin/sh\nprintf 'freshly converted, different bytes' > '{target}'\n\
                     echo 'no title here'\nexit 0\n"
                ),
            )
            .unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
            p
        }
    }

    // The primary scenario the whole feature exists for: a genuinely corrupt file must still be
    // purgeable, not just recoverable/unverifiable. Round 1's fix for I2 over-corrected and made
    // every bad_source row permanently un-purgeable; this pins that round 2 restores the case.
    #[test]
    fn purge_destroys_a_bad_source_row_when_the_rescan_confirms_it_is_still_bad() {
        // A configured handbrake_path pointing at a real, executable stand-in that runs to
        // completion but emits no parseable title set (ScanOutcome::NoTitle) — the shape of a
        // real HandBrakeCLI scan that opened the file fine and found nothing.
        let conn = Connection::open_in_memory().unwrap();
        crate::db::init_db(&conn).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("movie.mkv");
        std::fs::write(&f, b"garbage").unwrap();
        let p = f.to_str().unwrap();
        insert_error_row(&conn, "old", p, "bad_source");
        stamp_identity(&conn, "old", p);

        let script = no_title_scan_script(dir.path());
        conn.execute(
            "UPDATE settings SET value = ?1 WHERE key = 'handbrake_path'",
            params![script.to_str().unwrap()],
        )
        .unwrap();
        let (ctx, _sink, _disposer) = test_ctx(conn);
        let handbrake_path = resolve_handbrake_for_test(&ctx);

        let outcome = purge_one_locked(
            &ctx.db,
            "old",
            PurgeAction::Delete,
            &handbrake_path,
            &DeleteDisposer,
        );
        assert_eq!(
            outcome,
            PurgeOutcome::Purged,
            "a bad_source row must still be purgeable once the rescan re-confirms it — \
             I2's fix must not make bad_source rows permanently un-purgeable"
        );
        assert!(!f.exists(), "delete mode removes the confirmed-bad file");
    }

    // M2: the sibling of the test above, and the sharpest failure mode the whole rescan-before-
    // destroy design exists to prevent — a healthy file must be SPARED, not destroyed, when the
    // original bad_source verdict turns out to have been a transient environment fault (e.g. a
    // hiccuping mount) rather than a genuinely corrupt file. Only the pure `rescan_verdict`
    // mapping was pinned before this; this drives the real `PurgeOutcome::Recovered` outcome
    // end-to-end through the production `purge_one_locked` path.
    #[test]
    fn purge_recovers_a_bad_source_row_when_the_rescan_finds_a_title() {
        // A configured handbrake_path pointing at a real, executable stand-in that runs to
        // completion and DOES emit a parseable title set (ScanOutcome::Titled) — the shape of a
        // real HandBrakeCLI scan succeeding on a rescan after the original scan failed to run.
        let conn = Connection::open_in_memory().unwrap();
        crate::db::init_db(&conn).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("movie.mkv");
        std::fs::write(&f, b"actually fine").unwrap();
        let p = f.to_str().unwrap();
        insert_error_row(&conn, "old", p, "bad_source");
        stamp_identity(&conn, "old", p);

        let script = titled_scan_script(dir.path());
        conn.execute(
            "UPDATE settings SET value = ?1 WHERE key = 'handbrake_path'",
            params![script.to_str().unwrap()],
        )
        .unwrap();
        let (ctx, _sink, _disposer) = test_ctx(conn);
        let handbrake_path = resolve_handbrake_for_test(&ctx);

        let outcome = purge_one_locked(
            &ctx.db,
            "old",
            PurgeAction::Delete,
            &handbrake_path,
            &DeleteDisposer,
        );
        assert_eq!(
            outcome,
            PurgeOutcome::Recovered,
            "a rescan that reads a real title must spare the file, not destroy it"
        );
        assert!(
            f.exists(),
            "THE POINT OF THIS RUNG: a file recovered on rescan must survive"
        );
        // F12: a file just proved healthy must not sit in "Bad sources" forever.
        assert!(
            get_bad_sources_inner(&ctx.db.lock().unwrap())
                .unwrap()
                .is_empty(),
            "a recovered row must drop out of the review list, or it costs a fresh ~30s \
             rescan on every future purge press"
        );
        let still_there: i64 = ctx
            .db
            .lock()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM jobs WHERE id = 'old'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(still_there, 1, "the history entry itself survives");
    }

    #[test]
    fn purge_refuses_a_row_with_no_stored_fingerprint() {
        // Pre-feature/pre-migration rows (or a source that was itself unstattable at add time)
        // carry NULL source_size/source_mtime. There is nothing to verify current identity
        // against, so purge must refuse rather than treat "no proof of tampering" as "safe".
        // (insert_error_row alone, without stamp_identity, leaves size/mtime NULL.)
        let conn = Connection::open_in_memory().unwrap();
        crate::db::init_db(&conn).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("movie.mkv");
        std::fs::write(&f, b"content").unwrap();
        let p = f.to_str().unwrap();
        insert_error_row(&conn, "old", p, "bad_source");
        let db = Arc::new(Mutex::new(conn));

        let outcome = purge_one_locked(
            &db,
            "old",
            PurgeAction::Delete,
            &handbrake_path_not_needed(),
            &DeleteDisposer,
        );
        assert_eq!(outcome, PurgeOutcome::Changed);
        assert!(
            f.exists(),
            "a row with no fingerprint to verify against must never be destroyed"
        );
    }

    #[test]
    fn purge_refuses_a_file_whose_current_mtime_cannot_be_represented() {
        // file_identity converts mtime to milliseconds since the Unix epoch and returns None
        // if the file's mtime is before the epoch (duration_since errors) — a real, portable
        // way to make "exists, but cannot be stat'd into an identity right now" happen
        // deterministically. (The other real cause, a delete racing between the AlreadyGone
        // check and this one, can't be forced in a test without instrumenting production code.)
        let conn = Connection::open_in_memory().unwrap();
        crate::db::init_db(&conn).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("ancient.mkv");
        std::fs::write(&f, b"content").unwrap();
        let p = f.to_str().unwrap();
        insert_error_row(&conn, "old", p, "bad_source");
        stamp_identity(&conn, "old", p);

        // A write handle, not a bare File::open: on Windows, std's set_times calls
        // SetFileTime on the handle it's given without reopening, and that needs
        // FILE_WRITE_ATTRIBUTES — a read-only (GENERIC_READ) handle gets ERROR_ACCESS_DENIED.
        let file = std::fs::File::options().write(true).open(&f).unwrap();
        let pre_epoch = std::time::SystemTime::UNIX_EPOCH
            .checked_sub(std::time::Duration::from_secs(86_400))
            .unwrap();
        file.set_modified(pre_epoch).unwrap();
        let db = Arc::new(Mutex::new(conn));

        let outcome = purge_one_locked(
            &db,
            "old",
            PurgeAction::Delete,
            &handbrake_path_not_needed(),
            &DeleteDisposer,
        );
        assert_eq!(outcome, PurgeOutcome::Changed);
        assert!(
            f.exists(),
            "a file that cannot be stat'd into an identity right now must never be destroyed"
        );
    }

    #[test]
    fn path_is_in_use_fails_safe_when_the_query_errors() {
        // No init_db: the `jobs` table doesn't exist, so the query errors. `true` ("assume in
        // use") is the correct fail-safe value here — the counterintuitive one for a predicate
        // with this name, and exactly what a "simplify this" edit might flip to `false`,
        // silently turning any DB error during the check into permission to destroy a file a
        // running encode still depends on. Both variants (phase 1's raw check and phase 3's
        // canonicalized one) must fail the same safe way.
        let conn = Connection::open_in_memory().unwrap();
        assert!(path_is_in_use_raw(&conn, "/whatever.mkv"));
        assert!(path_is_in_use_canonical(&conn, "/whatever.mkv"));
    }

    // F13: an exact-string comparison misses a live job recorded under a different spelling of
    // the SAME file — a symlinked watched directory being the clearest, most portable case to
    // force deterministically (macOS/Windows case-insensitivity and /tmp vs /private/tmp are
    // platform quirks, not something a test can rely on everywhere). R2: only the canonicalized
    // check (phase 3, the rung that actually authorizes destruction) needs to resolve this — see
    // `path_is_in_use_raw`'s doc comment for why phase 1 deliberately doesn't.
    #[cfg(unix)]
    #[test]
    fn path_is_in_use_canonical_matches_through_a_symlinked_directory() {
        let real_dir = tempfile::tempdir().unwrap();
        let outer = tempfile::tempdir().unwrap();
        let link = outer.path().join("watched-link");
        std::os::unix::fs::symlink(real_dir.path(), &link).unwrap();

        let real_path = real_dir.path().join("movie.mkv");
        std::fs::write(&real_path, b"content").unwrap();
        let via_symlink = link.join("movie.mkv");

        let conn = Connection::open_in_memory().unwrap();
        crate::db::init_db(&conn).unwrap();
        // A live job recorded via the SYMLINKED path...
        conn.execute(
            "INSERT INTO jobs (id, source_path, output_path, preset, status, queue_order,
                               created_at)
             VALUES ('new', ?1, '/o.mp4', 'p', 'queued', 0, '2026-07-25T10:00:00Z')",
            params![via_symlink.to_str().unwrap()],
        )
        .unwrap();

        // ...must still be found in-use when purge checks the REAL (canonical) path.
        assert!(
            path_is_in_use_canonical(&conn, real_path.to_str().unwrap()),
            "a live job recorded under a symlinked path must still block a purge check against \
             the same file's real path"
        );
    }

    // N2 regression test: the DB lock is released around the rung-4 scan (up to PROBE_TIMEOUT,
    // ~30s), and in that window an in-place re-conversion of the SAME path could complete —
    // leaving a brand-new file at `path` with different bytes, while InUse alone reads as free
    // again (the job finished). Re-checking only InUse after re-acquiring the lock (round 1's
    // fix) would miss this; the file's identity must be re-verified too. Simulated
    // deterministically, without real thread concurrency: the stand-in HandBrake script itself
    // rewrites the target file before reporting no title, standing in for a completed in-place
    // conversion landing during the scan. If the post-scan re-check is ever narrowed back down
    // to just path_is_in_use, this test destroys the file and fails.
    #[test]
    fn purge_one_locked_refuses_when_the_file_changes_during_the_scan_window() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::init_db(&conn).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("movie.mkv");
        std::fs::write(&f, b"original bad bytes").unwrap();
        let p = f.to_str().unwrap();
        insert_error_row(&conn, "old", p, "bad_source");
        stamp_identity(&conn, "old", p); // fingerprint pinned to the ORIGINAL bytes

        // Stand-in HandBrake: overwrite the target file (an in-place re-conversion completing
        // mid-scan), then report no title and exit cleanly.
        let script = scan_script_that_overwrites_then_reports_no_title(dir.path(), p);
        conn.execute(
            "UPDATE settings SET value = ?1 WHERE key = 'handbrake_path'",
            params![script.to_str().unwrap()],
        )
        .unwrap();
        let (ctx, _sink, _disposer) = test_ctx(conn);
        let handbrake_path = resolve_handbrake_for_test(&ctx);

        let outcome = purge_one_locked(
            &ctx.db,
            "old",
            PurgeAction::Delete,
            &handbrake_path,
            &DeleteDisposer,
        );
        assert_eq!(
            outcome,
            PurgeOutcome::Changed,
            "the file's identity changed while the lock was released for the scan — the \
             post-scan re-check must catch this, not just re-check InUse"
        );
        assert!(
            f.exists(),
            "a file that changed during the scan window must never be destroyed, even though \
             the scan confirmed the ORIGINAL content had no title"
        );
    }

    // F1: HandBrake's rescan diagnostics are byte-identical for a genuinely corrupt file and a
    // healthy file WE merely can't open right now (a hiccuping mount, a permission fluke) — the
    // exact blind spot the whole rescan-before-destroy design exists to close. The rescan alone
    // proving "HandBrake still can't parse it" is not enough; purge must also prove it can read
    // the file itself before honoring RescanVerdict::Destroy. Root bypasses mode bits entirely,
    // so this assertion is meaningless as uid 0 (rootful docker / `act`); GitHub's ubuntu runner
    // is non-root, so PR CI runs it — mirrors converter.rs's `source_is_readable` root guard.
    #[cfg(unix)]
    #[test]
    fn purge_refuses_to_destroy_a_confirmed_bad_source_it_cannot_read_itself() {
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        let conn = Connection::open_in_memory().unwrap();
        crate::db::init_db(&conn).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("movie.mkv");
        std::fs::write(&f, b"garbage").unwrap();
        let p = f.to_str().unwrap();
        insert_error_row(&conn, "old", p, "bad_source");
        stamp_identity(&conn, "old", p);

        // Lock the file down AFTER stamping identity: chmod does not change mtime, so the
        // identity check still passes and the rescan gate is reached.
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o000)).unwrap();

        // A rescan stand-in that runs to completion and confirms no title — the primary
        // Destroy-producing case, just like `purge_destroys_a_bad_source_row_...` above.
        let script = dir.path().join("fake-handbrake.sh");
        std::fs::write(&script, "#!/bin/sh\necho 'no title here'\nexit 0\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        conn.execute(
            "UPDATE settings SET value = ?1 WHERE key = 'handbrake_path'",
            params![script.to_str().unwrap()],
        )
        .unwrap();
        let (ctx, _sink, _disposer) = test_ctx(conn);
        let handbrake_path = resolve_handbrake_for_test(&ctx);

        let outcome = purge_one_locked(
            &ctx.db,
            "old",
            PurgeAction::Delete,
            &handbrake_path,
            &DeleteDisposer,
        );
        // Restore permissions unconditionally so the tempdir's Drop cleanup can remove it.
        let _ = std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o644));

        assert_eq!(
            outcome,
            PurgeOutcome::Unverifiable,
            "HandBrake's word alone must never be enough — we could not confirm this ourselves"
        );
        assert!(
            f.exists(),
            "THE POINT OF THIS RUNG: a file we cannot verify ourselves must survive"
        );
    }

    // F4: `Path::exists()` collapses every stat error (a Windows ACL denial, an EIO on a
    // half-broken mount) into a bare `false`, which the old code read as "the file was deleted"
    // and permanently stamped the row purged. A NUL byte in the path is a portable, deterministic
    // way to force the underlying stat syscall itself to fail (`InvalidInput`) rather than merely
    // report "not found" — without needing a real permission fault or a dead network mount.
    #[test]
    fn purge_treats_a_stat_error_as_unverifiable_never_as_already_gone() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::init_db(&conn).unwrap();
        let bogus_path = "/tmp/convertbar-test-\0-nul-byte.mkv";
        insert_error_row(&conn, "old", bogus_path, "bad_source");
        let db = Arc::new(Mutex::new(conn));

        let outcome = purge_one_locked(
            &db,
            "old",
            PurgeAction::Delete,
            &handbrake_path_not_needed(),
            &DeleteDisposer,
        );
        assert_eq!(
            outcome,
            PurgeOutcome::Unverifiable,
            "a stat error must never be read as \"confirmed gone\" — Path::exists() would have \
             swallowed this into false and AlreadyGone"
        );
        assert_eq!(
            get_bad_sources_inner(&db.lock().unwrap()).unwrap().len(),
            1,
            "an unverifiable row must not be stamped purged"
        );
    }

    // ---- F5: trash-vs-delete dispatch, previously completely unverified ----

    #[test]
    fn purge_action_from_setting_maps_trash_delete_and_garbage() {
        assert_eq!(PurgeAction::from_setting("trash"), PurgeAction::Trash);
        assert_eq!(PurgeAction::from_setting("delete"), PurgeAction::Delete);
        assert_eq!(
            PurgeAction::from_setting(""),
            PurgeAction::Trash,
            "a missing/empty setting must default to the recoverable option"
        );
        assert_eq!(
            PurgeAction::from_setting("garbage"),
            PurgeAction::Trash,
            "a corrupted/future setting value must never silently escalate to permanent delete"
        );
    }

    #[test]
    fn destroy_and_record_routes_delete_action_to_remove_file_never_trash() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::init_db(&conn).unwrap();
        insert_error_row(&conn, "old", "/whatever.mkv", "bad_source");
        let mut remove_called = false;
        let mut trash_called = false;

        let outcome = destroy_and_record_with(
            &conn,
            "old",
            PurgeAction::Delete,
            "/whatever.mkv",
            |_| {
                remove_called = true;
                true
            },
            |_| {
                trash_called = true;
                true
            },
        );

        assert_eq!(outcome, PurgeOutcome::Purged);
        assert!(
            remove_called && !trash_called,
            "setting=delete must dispatch to remove_file, never trash::delete"
        );
    }

    #[test]
    fn destroy_and_record_routes_trash_action_to_trash_delete_never_remove_file() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::init_db(&conn).unwrap();
        insert_error_row(&conn, "old", "/whatever.mkv", "bad_source");
        let mut remove_called = false;
        let mut trash_called = false;

        let outcome = destroy_and_record_with(
            &conn,
            "old",
            PurgeAction::Trash,
            "/whatever.mkv",
            |_| {
                remove_called = true;
                true
            },
            |_| {
                trash_called = true;
                true
            },
        );

        assert_eq!(outcome, PurgeOutcome::Purged);
        assert!(
            trash_called && !remove_called,
            "setting=trash (the DEFAULT, sold to users as recoverable) must dispatch to \
             trash::delete, never a permanent remove_file"
        );
    }

    // R4: the two tests above pin destroy_and_record_with's dispatch, but destroy_and_record
    // itself hardcodes its two arguments with no DI seam of its own — a transposition of
    // remove_file_primitive <-> trash_delete_primitive at that call site would still pass both
    // tests above untouched, since neither ever calls destroy_and_record. These two call
    // destroy_and_record directly: the Delete case is pinned via the #[cfg(test)] call-count
    // instrumentation on remove_file_primitive; the Trash case is pinned via a RecordingDisposer,
    // asserting which path it recorded — without ever invoking the real OS Trash.
    #[test]
    fn destroy_and_record_binds_delete_action_to_remove_file_primitive() {
        REMOVE_FILE_PRIMITIVE_CALLS.with(|c| c.set(0));
        let conn = Connection::open_in_memory().unwrap();
        crate::db::init_db(&conn).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("movie.mkv");
        std::fs::write(&f, b"content").unwrap();
        let p = f.to_str().unwrap();
        insert_error_row(&conn, "old", p, "bad_source");
        let disposer = RecordingDisposer::default();

        let outcome = destroy_and_record(&conn, "old", PurgeAction::Delete, p, &disposer);

        assert_eq!(outcome, PurgeOutcome::Purged);
        assert_eq!(
            REMOVE_FILE_PRIMITIVE_CALLS.with(|c| c.get()),
            1,
            "action=Delete must invoke the permanent-delete primitive"
        );
        assert!(
            disposer.0.lock().unwrap().is_empty(),
            "action=Delete must never invoke the trash/dispose primitive"
        );
    }

    #[test]
    fn destroy_and_record_binds_trash_action_to_trash_delete_primitive() {
        REMOVE_FILE_PRIMITIVE_CALLS.with(|c| c.set(0));
        let conn = Connection::open_in_memory().unwrap();
        crate::db::init_db(&conn).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("movie.mkv");
        std::fs::write(&f, b"content").unwrap();
        let p = f.to_str().unwrap();
        insert_error_row(&conn, "old", p, "bad_source");
        let disposer = RecordingDisposer::default();

        let outcome = destroy_and_record(&conn, "old", PurgeAction::Trash, p, &disposer);

        assert_eq!(outcome, PurgeOutcome::Purged);
        assert_eq!(
            disposer.0.lock().unwrap().as_slice(),
            [p.to_string()],
            "action=Trash must invoke the disposer with the file's path"
        );
        assert_eq!(
            REMOVE_FILE_PRIMITIVE_CALLS.with(|c| c.get()),
            0,
            "action=Trash must never invoke the permanent-delete primitive — that would \
             silently make every Trash-configured (default) purge unrecoverable"
        );
        assert!(
            !f.exists(),
            "the disposer's dispose call actually removed the file"
        );
    }
}
