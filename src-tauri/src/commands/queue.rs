use rusqlite::params;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter, Manager, State};

use crate::converter::IN_PLACE_TEMP_MARKER;
use crate::failure_class::{CLASS_BAD_SOURCE, CLASS_BAD_SOURCE_PURGED, CLASS_BAD_SOURCE_TRUNCATED};
use crate::handbrake;
use crate::types::{
    AddResult, ClassifiedPaths, FolderScanResult, HistoryPage, HistorySummary, JobInfo,
    PurgeOutcome, PurgeResult, SkipCount, SkipReason,
};
use crate::AppState;

const VIDEO_EXTENSIONS: &[&str] = &[
    "mp4", "mkv", "avi", "mov", "wmv", "flv", "webm", "m4v", "ts",
];

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

pub(crate) fn scan_video_files(dir: &Path) -> Vec<PathBuf> {
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

/// The cache key for a probe: the file's byte size + last-modified time (epoch millis).
/// `None` when the file can't be stat'd or has no readable mtime — such a file has no
/// stable identity and is probed every scan (handled by `resolve_media`'s forced-miss path).
pub(crate) fn file_identity(path: &str) -> Option<crate::probe_cache::FileIdentity> {
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

fn get_handbrake_path(conn: &rusqlite::Connection) -> Result<String, String> {
    let configured: Option<String> = conn
        .query_row(
            "SELECT value FROM settings WHERE key = 'handbrake_path'",
            params![],
            |row| row.get(0),
        )
        .ok();

    if let Some(ref path) = configured {
        if !path.is_empty() && std::path::Path::new(path).exists() {
            return Ok(path.clone());
        }
    }

    handbrake::detect_handbrake_path().ok_or_else(|| "HandBrakeCLI not found".to_string())
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
fn path_is_in_use(conn: &rusqlite::Connection, path: &str) -> bool {
    conn.query_row(
        "SELECT COUNT(*) FROM jobs
         WHERE source_path = ?1 AND status IN ('queued', 'encoding', 'paused')",
        params![path],
        |r| r.get::<_, i64>(0),
    )
    .map(|n| n > 0)
    .unwrap_or(true) // a failed check means "assume in use" — never destroy on uncertainty
}

/// Outcome of the DB-only part of the ladder: eligibility (folded into the row lookup),
/// InUse, AlreadyGone, Changed, and — for scan-failure rows — the rung-4 rescan decision.
/// Touches the filesystem only via `Path::exists`/`file_identity`; never destroys anything.
enum PreDestroy {
    /// A final, non-destructive verdict. Nothing more to do.
    Stop(PurgeOutcome),
    /// Everything before the scan passed and a rescan is required: probe `handbrake_path`
    /// against `path`. The probe can block for `PROBE_TIMEOUT` (~30s), so callers that share
    /// `conn` with other threads must not hold that lock while probing. A scan that runs to
    /// completion and finds no title (`ScanOutcome::NoTitle`) is a real, re-confirmed verdict
    /// and falls through to destroy `path`, same as `ReadyToDestroy`.
    NeedsScan {
        path: String,
        handbrake_path: String,
    },
    /// Everything passed and no rescan is required (a `bad_source_truncated` row, whose
    /// verdict does not depend on a scan) — safe to destroy `path`.
    ReadyToDestroy { path: String },
}

/// Rungs 1-3 of the ladder plus the rung-4 rescan decision, against a single already-acquired
/// connection. Callers own how long any lock around `conn` is held.
fn pre_destroy_check(conn: &rusqlite::Connection, id: &str) -> PreDestroy {
    // Eligibility is folded into the row lookup itself: an id that is not a live, unpurged
    // bad-source error row (wrong status, wrong/absent failure_class, or simply nonexistent)
    // matches no row, and the existing "not found" arm below reports Failed for it. The UI is
    // expected to only ever pass ids from the review list, but a wiring mistake (e.g. passing
    // a History row's id) must never be able to reach a live `done`/`queued` row.
    let row: Result<(String, Option<String>, Option<i64>, Option<i64>), _> = conn.query_row(
        "SELECT source_path, failure_class, source_size, source_mtime FROM jobs
         WHERE id = ?1 AND status = 'error' AND failure_class IN (?2, ?3)",
        params![id, CLASS_BAD_SOURCE, CLASS_BAD_SOURCE_TRUNCATED],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    );
    let (path, class, size, mtime) = match row {
        Ok(v) => v,
        Err(_) => return PreDestroy::Stop(PurgeOutcome::Failed),
    };

    if path_is_in_use(conn, &path) {
        return PreDestroy::Stop(PurgeOutcome::InUse);
    }
    if !std::path::Path::new(&path).exists() {
        return PreDestroy::Stop(PurgeOutcome::AlreadyGone);
    }

    // Identity: the (size, mtime) fingerprint the codebase already keeps. A replacement file
    // of coincidentally identical size still fails on mtime. Anything short of a PROVEN match
    // — a current stat failure, or a stored NULL fingerprint (pre-feature row, or a source
    // that was itself unstattable at add time) — is treated as a mismatch: there is nothing to
    // verify against, so purge refuses rather than guess.
    let identity_matches = matches!(
        (file_identity(&path), size, mtime),
        (Some(current), Some(s), Some(m)) if current.size == s && current.mtime == m
    );
    if !identity_matches {
        return PreDestroy::Stop(PurgeOutcome::Changed);
    }

    if should_rescan_before_purge(class.as_deref()) {
        return match get_handbrake_path(conn) {
            // Cannot even attempt the rescan (e.g. HandBrakeCLI moved since classification) —
            // indistinguishable from "the scan ran and failed", so this must not fall through
            // to destruction.
            Err(_) => PreDestroy::Stop(PurgeOutcome::Unverifiable),
            Ok(handbrake_path) => PreDestroy::NeedsScan {
                path,
                handbrake_path,
            },
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

/// Destroy `path` per `action` and stamp the row purged. Called only once every earlier rung
/// has passed.
fn destroy_and_record(
    conn: &rusqlite::Connection,
    id: &str,
    action: &str,
    path: &str,
) -> PurgeOutcome {
    let destroyed = if action == "delete" {
        std::fs::remove_file(path).is_ok()
    } else {
        trash::delete(path).is_ok()
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

/// Decide and act for one id against a bare connection: rung 4's scan (if any) runs without
/// releasing any lock, which is fine for the single-threaded callers of this function (tests,
/// and any future non-shared-DB use). The production command instead uses `purge_one_locked`,
/// which releases the shared DB mutex around that same scan.
fn purge_one(conn: &rusqlite::Connection, id: &str, action: &str) -> PurgeOutcome {
    match pre_destroy_check(conn, id) {
        PreDestroy::Stop(outcome) => outcome,
        PreDestroy::ReadyToDestroy { path } => destroy_and_record(conn, id, action, &path),
        PreDestroy::NeedsScan {
            path,
            handbrake_path,
        } => match rescan_verdict(crate::probe::scan_outcome(&handbrake_path, &path)) {
            RescanVerdict::Recovered => PurgeOutcome::Recovered,
            RescanVerdict::Unverifiable => PurgeOutcome::Unverifiable,
            // The primary case the whole feature exists for: a re-confirmed bad file.
            RescanVerdict::Destroy => destroy_and_record(conn, id, action, &path),
        },
    }
}

fn purge_ids(
    conn: &rusqlite::Connection,
    ids: &[String],
    action: &str,
) -> Result<Vec<PurgeResult>, String> {
    Ok(ids
        .iter()
        .map(|id| PurgeResult {
            id: id.clone(),
            outcome: purge_one(conn, id, action),
        })
        .collect())
}

/// Same ladder as `purge_one`, for the production async command: the shared DB mutex is
/// released around the rung-4 scan (which can block for `PROBE_TIMEOUT`, ~30s) so it cannot
/// stall the converter thread's progress writes — or any other command — for the duration.
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
/// `RescanVerdict::Destroy` obtained outside the lock already stands as the scan's answer.
fn purge_one_locked(db: &Arc<Mutex<rusqlite::Connection>>, id: &str, action: &str) -> PurgeOutcome {
    let pre = {
        let conn = db.lock().unwrap();
        pre_destroy_check(&conn, id)
    };
    match pre {
        PreDestroy::Stop(outcome) => return outcome,
        PreDestroy::ReadyToDestroy { .. } => {}
        PreDestroy::NeedsScan {
            path,
            handbrake_path,
        } => {
            // Deliberately outside the lock acquired above (already dropped) — see the
            // doc comment above.
            match rescan_verdict(crate::probe::scan_outcome(&handbrake_path, &path)) {
                RescanVerdict::Recovered => return PurgeOutcome::Recovered,
                RescanVerdict::Unverifiable => return PurgeOutcome::Unverifiable,
                RescanVerdict::Destroy => {}
            }
        }
    }

    // Re-verify everything — not just InUse — under a freshly acquired lock before destroying.
    let conn = db.lock().unwrap();
    match pre_destroy_check(&conn, id) {
        PreDestroy::Stop(outcome) => outcome,
        // Either never needed a rescan (bad_source_truncated), or needed one and every other
        // rung still passes — in the latter case the scan already ran above and confirmed
        // Destroy, so this is not a second rescan, just re-confirmation of the DB-side facts.
        PreDestroy::ReadyToDestroy { path } | PreDestroy::NeedsScan { path, .. } => {
            destroy_and_record(&conn, id, action, &path)
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
            let identity = file_identity(p).map(|i| (i.size, i.mtime));
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

pub(crate) fn add_files_inner(
    state: &AppState,
    paths: &[String],
    progress: Option<&dyn Fn(u32, u32)>,
) -> Result<AddResult, String> {
    // First, read preset and suffix template from DB
    let (preset, suffix_template, hb_path, skip_already_converted, skip_by_source_media) = {
        let conn = state.db.lock().map_err(|e| e.to_string())?;

        let preset: String = conn
            .query_row(
                "SELECT value FROM settings WHERE key = 'preset'",
                [],
                |row| row.get(0),
            )
            .map_err(|e| e.to_string())?;

        let suffix_template = crate::commands::settings::read_suffix_template(&conn, &preset);

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
            get_handbrake_path(&conn).ok()
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
        let metadata = super::handbrake::cached_preset_metadata(state, &hb, &preset)?;
        handbrake::resolve_suffix_template(&suffix_template, &metadata)
    } else {
        suffix_template
    };

    // Source-media skip: probe candidate files and drop those already at/below the target. Only
    // files that survive the probe-free skip checks are probed, so a re-scan of an already-handled
    // folder shells out to HandBrake zero times. Probing runs outside the DB lock; on any
    // uncertainty (no HandBrake, probe failure/timeout, unknown codec) the file is kept.
    let candidates_to_probe: Vec<String> = if skip_by_source_media {
        let conn = state.db.lock().map_err(|e| e.to_string())?;
        probe_candidates(&conn, paths, &suffix, skip_already_converted)?
    } else {
        Vec::new()
    };

    let media_skipped: HashSet<String> = if !candidates_to_probe.is_empty() {
        if let Some(hb) = hb_path.as_deref() {
            let metadata = super::handbrake::cached_preset_metadata(state, hb, &preset)?;
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
                    .map(|p| (p.clone(), file_identity(p)))
                    .collect();
            let total = candidates_to_probe.len() as u32;
            let probe_count = std::cell::Cell::new(0u32);
            let probed = crate::probe_cache::resolve_media(
                &with_identity,
                |ids| {
                    let conn = state.db.lock().expect("db mutex poisoned");
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
                    let conn = state.db.lock().expect("db mutex poisoned");
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
    let conn = state.db.lock().map_err(|e| e.to_string())?;
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
        let identity = file_identity(path_str);
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

#[tauri::command]
pub async fn add_files(app: AppHandle, paths: Vec<String>) -> Result<AddResult, String> {
    // add_files_inner runs a blocking HandBrakeCLI probe per file (source-media skip), so a large
    // drop would freeze the main-thread event loop. Offload to a blocking thread; the AddResult
    // still returns to the awaiting frontend. Same hazard the watcher avoids via scan_existing_background.
    let app_for_emit = app.clone();
    let result = tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        let op = crate::add_progress::AddOp::new(&app, String::new());
        let reporter = |done: u32, total: u32| op.report(done, total);
        add_files_inner(&state, &paths, Some(&reporter as &dyn Fn(u32, u32)))
    })
    .await
    .map_err(|e| e.to_string())?;
    if result.is_ok() {
        // Mirror enqueue_and_start (watcher.rs) so useQueue refreshes without a frontend callback.
        let _ = app_for_emit.emit("queue-updated", ());
    }
    result
}

#[tauri::command]
pub async fn scan_folder(path: String) -> Result<FolderScanResult, String> {
    // scan_video_files stats every entry of an unbounded recursive walk; a deep tree
    // or network volume would freeze the UI if this ran on the main thread.
    tauri::async_runtime::spawn_blocking(move || scan_folder_inner(path))
        .await
        .map_err(|e| e.to_string())?
}

fn scan_folder_inner(path: String) -> Result<FolderScanResult, String> {
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

#[tauri::command]
pub async fn confirm_folder_add(app: AppHandle, path: String) -> Result<AddResult, String> {
    if !Path::new(&path).is_dir() {
        return Err("Path is not a directory".to_string());
    }

    // Both the recursive scan and the per-file probe block; run them off the main thread so
    // confirming a large folder doesn't freeze the UI (same hazard as add_files).
    let app_for_emit = app.clone();
    let result = tauri::async_runtime::spawn_blocking(move || {
        let label = Path::new(&path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_string();
        let op = crate::add_progress::AddOp::new(&app, label);
        let files = scan_video_files(Path::new(&path));
        let paths: Vec<String> = files
            .into_iter()
            .filter_map(|p| p.to_str().map(|s| s.to_string()))
            .collect();
        let state = app.state::<AppState>();
        let reporter = |done: u32, total: u32| op.report(done, total);
        add_files_inner(&state, &paths, Some(&reporter as &dyn Fn(u32, u32)))
    })
    .await
    .map_err(|e| e.to_string())?;
    if result.is_ok() {
        let _ = app_for_emit.emit("queue-updated", ());
    }
    result
}

#[tauri::command]
pub fn get_queue(state: State<'_, AppState>) -> Result<Vec<JobInfo>, String> {
    let conn = state.db.lock().map_err(|e| e.to_string())?;
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

#[tauri::command]
pub fn remove_job(state: State<'_, AppState>, id: String) -> Result<(), String> {
    let conn = state.db.lock().map_err(|e| e.to_string())?;
    conn.execute(
        "DELETE FROM jobs WHERE id = ?1 AND status = 'queued'",
        params![id],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub fn remove_history_entry(state: State<'_, AppState>, id: String) -> Result<(), String> {
    let conn = state.db.lock().map_err(|e| e.to_string())?;
    remove_history_entry_inner(&conn, &id)
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

#[tauri::command]
pub fn reorder_queue(state: State<'_, AppState>, job_ids: Vec<String>) -> Result<(), String> {
    let conn = state.db.lock().map_err(|e| e.to_string())?;
    reorder_queue_inner(&conn, &job_ids)
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

#[tauri::command]
pub fn clear_completed(state: State<'_, AppState>, mode: String) -> Result<(), String> {
    let conn = state.db.lock().map_err(|e| e.to_string())?;
    match mode.as_str() {
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

#[tauri::command]
pub fn clear_queue(
    state: State<'_, AppState>,
    converter_state: State<'_, std::sync::Arc<crate::converter::ConverterState>>,
) -> Result<(), String> {
    let conn = state.db.lock().map_err(|e| e.to_string())?;
    conn.execute("DELETE FROM jobs WHERE status = 'queued'", [])
        .map_err(|e| e.to_string())?;
    // A cleared queue has no job to justify a low-disk pause reason; drop it so the banner
    // can't be re-seeded over an empty queue after a remount.
    *converter_state
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

#[tauri::command]
pub fn get_bad_sources(state: State<'_, AppState>) -> Result<Vec<JobInfo>, String> {
    let conn = state.db.lock().map_err(|e| e.to_string())?;
    get_bad_sources_inner(&conn)
}

#[tauri::command]
pub async fn purge_bad_sources(
    app: AppHandle,
    ids: Vec<String>,
) -> Result<Vec<PurgeResult>, String> {
    // Rung 4 can block per id for up to PROBE_TIMEOUT (~30s) scanning a stalled/offline source.
    // Running that synchronously on the main thread would freeze the UI for the whole batch —
    // offload like this file's other probe-touching commands (add_files, scan_folder,
    // confirm_folder_add, classify_paths). purge_one_locked additionally releases the DB mutex
    // around each scan so a slow purge can't stall the converter thread too.
    tauri::async_runtime::spawn_blocking(move || -> Result<Vec<PurgeResult>, String> {
        let state = app.state::<AppState>();
        let action: String = {
            let conn = state.db.lock().map_err(|e| e.to_string())?;
            conn.query_row(
                "SELECT value FROM settings WHERE key = 'bad_source_action'",
                params![],
                |r| r.get(0),
            )
            .unwrap_or_else(|_| "trash".to_string())
        };
        let action = crate::commands::settings::normalize_bad_source_action(&action);
        Ok(ids
            .iter()
            .map(|id| PurgeResult {
                id: id.clone(),
                outcome: purge_one_locked(&state.db, id, action),
            })
            .collect())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub fn get_history(
    state: State<'_, AppState>,
    limit: u32,
    offset: u32,
    search: Option<String>,
    sort_by: Option<String>,
) -> Result<HistoryPage, String> {
    let conn = state.db.lock().map_err(|e| e.to_string())?;
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

#[tauri::command]
pub fn get_history_summary(
    state: State<'_, AppState>,
    search: Option<String>,
) -> Result<HistorySummary, String> {
    let conn = state.db.lock().map_err(|e| e.to_string())?;
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

#[tauri::command]
pub async fn classify_paths(paths: Vec<String>) -> Result<ClassifiedPaths, String> {
    // Dropped folders get the same recursive walk as scan_folder — off the main thread.
    tauri::async_runtime::spawn_blocking(move || classify_paths_inner(paths))
        .await
        .map_err(|e| e.to_string())?
}

fn classify_paths_inner(paths: Vec<String>) -> Result<ClassifiedPaths, String> {
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
    use rusqlite::Connection;
    use std::path::Path;

    fn test_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::init_db(&conn).unwrap();
        conn
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
        let id = file_identity(&src_str).unwrap();
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
        let id_a = file_identity(&src_str).unwrap();

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
        let state = crate::AppState {
            db: std::sync::Arc::new(std::sync::Mutex::new(conn)),
            preset_cache: std::sync::Mutex::new(std::collections::HashMap::new()),
        };
        // Pin the target to h265/1080p without shelling out to HandBrake for preset metadata.
        state.preset_cache.lock().unwrap().insert(
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
        let result = add_files_inner(&state, &inputs, None).unwrap();

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
        let again = add_files_inner(&state, &inputs, None).unwrap();
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
        use crate::converter::{ConverterState, LowDiskPause};

        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();

        let conn = test_conn();
        insert_queued(&conn, "j1", "/m/a.mp4", "queued", 0);
        app.manage(crate::AppState {
            db: std::sync::Arc::new(std::sync::Mutex::new(conn)),
            preset_cache: std::sync::Mutex::new(std::collections::HashMap::new()),
        });

        let converter = std::sync::Arc::new(ConverterState::new());
        *converter.low_disk_pause.lock().unwrap() = Some(LowDiskPause {
            path: "/m/a.mp4.out".into(),
            available_bytes: 3,
            required_bytes: 5,
        });
        app.manage(converter.clone());

        clear_queue(app.state(), app.state()).unwrap();

        let state: State<'_, AppState> = app.state();
        let remaining: i64 = state
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
            converter.low_disk_pause().is_none(),
            "clearing the queue also drops the low-disk pause reason so it can't re-seed the banner"
        );
    }

    #[test]
    fn clear_queue_clears_the_persisted_pause() {
        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::db::init_db(&conn).unwrap();
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
        app.manage(crate::AppState {
            db: std::sync::Arc::new(std::sync::Mutex::new(conn)),
            preset_cache: std::sync::Mutex::new(Default::default()),
        });
        app.manage(std::sync::Arc::new(crate::converter::ConverterState::new()));

        clear_queue(app.state(), app.state()).unwrap();

        let state: State<'_, AppState> = app.state();
        let db = state.db.lock().unwrap();
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
        let ident = file_identity(path).expect("file exists");
        conn.execute(
            "UPDATE jobs SET source_size = ?2, source_mtime = ?3 WHERE id = ?1",
            params![id, ident.size, ident.mtime],
        )
        .unwrap();
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
        let conn = Connection::open_in_memory().unwrap();
        crate::db::init_db(&conn).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("movie.mkv");
        std::fs::write(&f, b"content").unwrap();
        let p = f.to_str().unwrap();

        // The bad-source row, plus a re-added copy of the same file now queued.
        insert_error_row(&conn, "old", p, "bad_source");
        conn.execute(
            "INSERT INTO jobs (id, source_path, output_path, preset, status, queue_order,
                               created_at)
             VALUES ('new', ?1, '/o.mp4', 'p', 'queued', 0, '2026-07-25T10:00:00Z')",
            params![p],
        )
        .unwrap();

        let outcomes = purge_ids(&conn, &["old".to_string()], "delete").unwrap();
        assert_eq!(outcomes[0].outcome, PurgeOutcome::InUse);
        assert!(
            f.exists(),
            "a file a queued job depends on must never be destroyed"
        );
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
            get_bad_sources_inner(&conn).unwrap().is_empty(),
            "a purged row must drop out of the list or a second press just errors"
        );
        let still_there: i64 = conn
            .query_row("SELECT COUNT(*) FROM jobs WHERE id = 'old'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(still_there, 1, "the history entry itself survives");
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

        let outcomes = purge_ids(&conn, &["done1".to_string()], "delete").unwrap();
        assert_eq!(outcomes[0].outcome, PurgeOutcome::Failed);
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
        // the real purge_ids path rather than the pure mapping alone.
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

        let outcomes = purge_ids(&conn, &["old".to_string()], "delete").unwrap();
        assert_eq!(outcomes[0].outcome, PurgeOutcome::Unverifiable);
        assert!(
            f.exists(),
            "a rescan that could not even run must never be treated as confirming the file bad"
        );
    }

    // The primary scenario the whole feature exists for: a genuinely corrupt file must still be
    // purgeable, not just recoverable/unverifiable. Round 1's fix for I2 over-corrected and made
    // every bad_source row permanently un-purgeable; this pins that round 2 restores the case.
    #[cfg(unix)]
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

        let script = dir.path().join("fake-handbrake.sh");
        std::fs::write(&script, "#!/bin/sh\necho 'no title here'\nexit 0\n").unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        conn.execute(
            "UPDATE settings SET value = ?1 WHERE key = 'handbrake_path'",
            params![script.to_str().unwrap()],
        )
        .unwrap();

        let outcomes = purge_ids(&conn, &["old".to_string()], "delete").unwrap();
        assert_eq!(
            outcomes[0].outcome,
            PurgeOutcome::Purged,
            "a bad_source row must still be purgeable once the rescan re-confirms it — \
             I2's fix must not make bad_source rows permanently un-purgeable"
        );
        assert!(!f.exists(), "delete mode removes the confirmed-bad file");
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

        let outcomes = purge_ids(&conn, &["old".to_string()], "delete").unwrap();
        assert_eq!(outcomes[0].outcome, PurgeOutcome::Changed);
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

        let outcomes = purge_ids(&conn, &["old".to_string()], "delete").unwrap();
        assert_eq!(outcomes[0].outcome, PurgeOutcome::Changed);
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
        // running encode still depends on.
        let conn = Connection::open_in_memory().unwrap();
        assert!(path_is_in_use(&conn, "/whatever.mkv"));
    }

    #[test]
    fn purge_one_locked_destroys_a_truncated_row_like_the_pure_ladder() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::init_db(&conn).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("movie.mkv");
        std::fs::write(&f, b"garbage").unwrap();
        let p = f.to_str().unwrap();
        insert_error_row(&conn, "old", p, "bad_source_truncated");
        stamp_identity(&conn, "old", p);
        let db = Arc::new(Mutex::new(conn));

        let outcome = purge_one_locked(&db, "old", "delete");
        assert_eq!(outcome, PurgeOutcome::Purged);
        assert!(!f.exists(), "delete mode removes the file");
    }

    #[test]
    fn purge_one_locked_refuses_a_path_a_live_job_still_needs() {
        // Same scenario as purge_skips_a_path_a_live_job_still_needs, through the lock-
        // releasing production path instead of the pure single-connection one — the InUse
        // guarantee must hold on both.
        let conn = Connection::open_in_memory().unwrap();
        crate::db::init_db(&conn).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("movie.mkv");
        std::fs::write(&f, b"content").unwrap();
        let p = f.to_str().unwrap();
        insert_error_row(&conn, "old", p, "bad_source");
        conn.execute(
            "INSERT INTO jobs (id, source_path, output_path, preset, status, queue_order,
                               created_at)
             VALUES ('new', ?1, '/o.mp4', 'p', 'queued', 0, '2026-07-25T10:00:00Z')",
            params![p],
        )
        .unwrap();
        let db = Arc::new(Mutex::new(conn));

        let outcome = purge_one_locked(&db, "old", "delete");
        assert_eq!(outcome, PurgeOutcome::InUse);
        assert!(
            f.exists(),
            "a file a queued job depends on must never be destroyed via the locked path either"
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
    #[cfg(unix)]
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
        let script = dir.path().join("fake-handbrake.sh");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\nprintf 'freshly converted, different bytes' > '{p}'\n\
                 echo 'no title here'\nexit 0\n"
            ),
        )
        .unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        conn.execute(
            "UPDATE settings SET value = ?1 WHERE key = 'handbrake_path'",
            params![script.to_str().unwrap()],
        )
        .unwrap();
        let db = Arc::new(Mutex::new(conn));

        let outcome = purge_one_locked(&db, "old", "delete");
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
}
