use regex::Regex;
use rusqlite::{params, Connection};
use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};

use crate::ctx::Ctx;
use crate::events::EventSinkExt;
use crate::types::JobInfo;

/// Filename marker for an in-flight in-place encode. A recognizable, non-suffix token so
/// `is_video_file` can exclude it — a folder scan or watched folder must never enqueue a temp.
pub(crate) const IN_PLACE_TEMP_MARKER: &str = ".convertbar-tmp.";

/// A job re-encodes a file onto itself exactly when its stored output path equals its source.
/// Compared as `Path` (not raw strings) so this predicate matches the add-time detection in
/// `add_files_to_db` (`output_path.as_path() == path`), which normalizes `//` and `/.` segments.
/// A mismatch here would route an in-place job through the distinct-file path and overwrite/delete
/// the user's source — so the two predicates MUST stay identical.
pub(crate) fn is_in_place(source_path: &str, output_path: &str) -> bool {
    std::path::Path::new(source_path) == std::path::Path::new(output_path)
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

/// Reset jobs interrupted by a quit/crash (`encoding`/`paused`) back to `queued` for the next
/// run, deleting only the partial output — NEVER the source. For an in-place job `output_path`
/// equals `source_path`, so the partial to remove is the hidden temp sibling, not the original
/// (mirrors `cancel_conversion`'s guard; deleting `output_path` here would destroy the user's file).
pub fn recover_interrupted_jobs(db: &Connection) {
    let mut stmt = match db.prepare(
        "SELECT id, source_path, output_path FROM jobs WHERE status IN ('encoding', 'paused')",
    ) {
        Ok(s) => s,
        Err(_) => return,
    };
    let interrupted: Vec<(String, String, String)> = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .map(|rows| rows.flatten().collect())
        .unwrap_or_default();

    for (id, source_path, output_path) in &interrupted {
        let target = if is_in_place(source_path, output_path) {
            in_place_temp_path(source_path)
        } else {
            std::path::PathBuf::from(output_path)
        };
        let _ = std::fs::remove_file(&target);
        let _ = db.execute(
            // started_at is cleared with the status: the abandoned attempt's start time must
            // not survive into the next one. Three error paths in process_queue (vanished
            // source, HandBrake-not-found, ClaimOutcome::Failed) write completed_at WITHOUT
            // re-claiming, so a surviving stamp would report the whole downtime as encode time.
            "UPDATE jobs SET status = 'queued', started_at = NULL WHERE id = ?1",
            params![id],
        );
    }
}

/// Refresh a completed job's source-identity fingerprint to the file currently at `path`.
/// Called after an in-place encode replaces the source: the recorded `(size, mtime)` must match
/// what a folder re-scan will stat, so the encoded result is recognized as already done (no
/// re-encode cascade) while a genuinely different file that later recycles the path still fails
/// the identity check. Reuses `queue::file_identity` so the encoding stays identical to insert.
fn record_source_identity(db: &Connection, job_id: &str, path: &str) {
    if let Some(id) = crate::probe_cache::file_identity(path) {
        let _ = db.execute(
            "UPDATE jobs SET source_size = ?2, source_mtime = ?3 WHERE id = ?1",
            params![job_id, id.size, id.mtime],
        );
    }
}

/// Re-stamp a condemned row's identity fingerprint to the source's CURRENT stat, taken at the
/// moment we classify it bad_source[_truncated] — NOT the stale fingerprint recorded when the
/// file was queued.
///
/// Without this: a healthy file is queued (fingerprint S,M); it is truncated in place by a sync
/// tool before its turn; the encode condemns it while the DB fingerprint still describes the
/// HEALTHY original; the sync tool then repairs it, and — because rsync -t / Syncthing / wget -N
/// all preserve mtime — it lands back at exactly (S,M). Purge would see a "match" against the
/// stale fingerprint and destroy the REPAIRED file. Stamping at condemnation time closes that
/// window. Unlike `record_source_identity` (called only on a SUCCESSFUL in-place encode, where
/// leaving a slightly-stale fingerprint on a rare failed stat is harmless), a failed stat here
/// writes NULL: purge already refuses to destroy a row with a NULL fingerprint, and leaving the
/// old, now-provably-wrong "healthy" fingerprint in place would be actively unsafe.
fn record_condemned_identity(db: &Arc<Mutex<Connection>>, job_id: &str, path: &str) {
    let id = crate::probe_cache::file_identity(path);
    let db = db.lock().unwrap();
    let _ = db.execute(
        "UPDATE jobs SET source_size = ?2, source_mtime = ?3 WHERE id = ?1",
        params![
            job_id,
            id.as_ref().map(|i| i.size),
            id.as_ref().map(|i| i.mtime)
        ],
    );
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

fn apply_in_place_action(
    action: InPlaceAction,
    temp: &std::path::Path,
    source: &std::path::Path,
    disposer: &dyn crate::dispose::FileDisposer,
) -> std::io::Result<()> {
    match action {
        InPlaceAction::RenameTempOverSource => std::fs::rename(temp, source),
        InPlaceAction::TrashSourceThenRename => {
            if let Some(s) = source.to_str() {
                let _ = disposer.dispose(s);
            } else {
                // Source paths are read from the DB as UTF-8 `String`s (rusqlite TEXT
                // columns), so this is unreachable in practice — pinned rather than
                // silently skipping the dispose call and falling through to the rename.
                debug_assert!(false, "source paths come from the DB as UTF-8 Strings");
            }
            // The rename is only safe once the source has actually left: renaming over a
            // source that is still there overwrites the user's only copy with the re-encode,
            // and in trash mode there is then nothing in the Trash to recover. Refuse instead
            // — the original stays put and the temp keeps the re-encoded content. Checked on
            // the filesystem rather than on the dispose bool, for the same reason the
            // distinct-file cleanup is: it is the end state that matters.
            if source.exists() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "source could not be moved to Trash; refusing to overwrite it",
                ));
            }
            std::fs::rename(temp, source)
        }
        InPlaceAction::RemoveTemp => std::fs::remove_file(temp),
    }
}

/// Whether a failed in-place cleanup must demote a "successful" encode to an error. Only a failed
/// *rename* (`RenameTempOverSource`/`TrashSourceThenRename`) matters: those are the two actions
/// that were meant to replace the source, so a failure there means it did not (and in trash mode
/// the original may already be in Trash). A failed `RemoveTemp` is benign — the source is
/// correctly kept, only an orphan temp lingers (it is marker-excluded from scans and pre-cleared
/// on the next in-place encode).
///
/// Keyed on the ACTION, not on `kept`: `RemoveTemp` with a `kept == KeptFile::Converted` decision
/// is reachable whenever cleanup_mode is `"keep"` (`in_place_action` discards a winning re-encode
/// there instead of keeping "both" — there is no second file for an in-place job). Keying fatality
/// on `kept` alone mistook that benign, by-design temp-removal failure for a botched replacement
/// and recorded a false "In-place replacement failed" error for a job that behaved exactly as
/// keep mode intends.
fn in_place_apply_is_fatal(action: Option<InPlaceAction>, apply_failed: bool) -> bool {
    apply_failed
        && matches!(
            action,
            Some(InPlaceAction::RenameTempOverSource) | Some(InPlaceAction::TrashSourceThenRename)
        )
}

pub struct ConverterState {
    // `current_pid`/`current_child`/`is_running`/`pause_after_current` stay `pub` (not
    // `pub(crate)`): the desktop-only updater (src-tauri/src/updater.rs, commands/updater.rs)
    // reaches across the crate boundary to lock/read them directly — `try_install_now` claims
    // the same `is_running` mutex `run_queue`/`claim_queue_slot` use so the two atomically
    // exclude each other, and `restart_after_killing_encoder`'s test seeds a live child via
    // `current_pid`/`current_child`. Fields not touched cross-crate stay narrowed below.
    pub current_pid: Mutex<Option<u32>>,
    pub current_child: Mutex<Option<Child>>,
    pub(crate) current_job_id: Mutex<Option<String>>,
    pub(crate) is_paused: Mutex<bool>,
    pub is_running: Mutex<bool>,
    pub pause_after_current: Mutex<bool>,
    /// One-way app-teardown latch: armed by `kill_active_child`, checked by
    /// `process_queue` so the queue thread never spawns another encoder mid-quit.
    pub(crate) shutdown: std::sync::atomic::AtomicBool,
    /// Latched while an update is installing, so the queue cannot start a job underneath it.
    /// Claimed and released under the `is_running` lock, making the gate atomic against
    /// `run_queue` rather than a check-then-act race.
    pub installing: std::sync::atomic::AtomicBool,
    /// Reason the queue is currently paused for low disk space, if any. Set when the low-disk
    /// gate trips, cleared at the start of every `process_queue` run so a resume (or a run that
    /// never hits the gate) doesn't leave a stale reason around. Lets the UI seed the banner from
    /// backend state on mount, not just the live `queue-paused-low-disk` event.
    pub(crate) low_disk_pause: Mutex<Option<LowDiskPause>>,
}

impl ConverterState {
    pub fn new() -> Self {
        Self {
            current_pid: Mutex::new(None),
            current_child: Mutex::new(None),
            current_job_id: Mutex::new(None),
            is_paused: Mutex::new(false),
            is_running: Mutex::new(false),
            pause_after_current: Mutex::new(false),
            shutdown: std::sync::atomic::AtomicBool::new(false),
            installing: std::sync::atomic::AtomicBool::new(false),
            low_disk_pause: Mutex::new(None),
        }
    }

    pub fn is_shutting_down(&self) -> bool {
        self.shutdown.load(std::sync::atomic::Ordering::SeqCst)
    }

    pub fn low_disk_pause(&self) -> Option<LowDiskPause> {
        self.low_disk_pause
            .lock()
            .map(|g| g.clone())
            .unwrap_or(None)
    }

    /// Returns true if the current platform supports real process pause/resume (SIGSTOP/SIGCONT).
    pub fn can_pause_process() -> bool {
        cfg!(unix)
    }

    /// Whether the queue is armed to pause after the current job. The source of truth for
    /// the "Pause after this" button, which reads it on mount rather than mirroring locally.
    pub fn is_pause_after_current(&self) -> bool {
        self.pause_after_current.lock().map(|g| *g).unwrap_or(false)
    }

    /// Whether the queue is currently processing jobs. The convenience read of `is_running`
    /// for callers that only need the bool (e.g. the desktop tray listener) — the updater
    /// (src-tauri/src/updater.rs) still locks the raw field directly where it needs the mutex
    /// itself, to claim it atomically alongside `installing`.
    pub fn is_running(&self) -> bool {
        self.is_running.lock().map(|g| *g).unwrap_or(false)
    }
}

/// Kill the active HandBrake child (resuming it first if SIGSTOP-paused, since a
/// stopped process can't act on SIGTERM-class signals) and reap it, so quitting the
/// app can't orphan an encoder that would keep burning CPU for hours. The partial
/// output is left alone: the next launch's auto-resume deletes it once no process
/// holds it.
pub fn kill_active_child(converter: &ConverterState) {
    // Arm shutdown BEFORE touching the child: without it the queue thread's next
    // iteration spawns a fresh encoder during teardown, and a spawn that raced this
    // call (child not yet stored in current_child) would be missed entirely —
    // process_queue re-checks the flag right after storing the handle.
    converter
        .shutdown
        .store(true, std::sync::atomic::Ordering::SeqCst);
    #[cfg(unix)]
    {
        if let Ok(pid) = converter.current_pid.lock() {
            if let Some(pid) = *pid {
                unsafe {
                    libc::kill(pid as i32, libc::SIGCONT);
                }
            }
        }
    }
    if let Ok(mut guard) = converter.current_child.lock() {
        if let Some(ref mut child) = *guard {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Which file `process_queue` keeps after a successful encode. The variant *is* the cleanup
/// decision — it tells the call site which file (if any) to delete with an irreversible
/// `trash`/`remove_file`. `Original` and `Neither` are distinct so the call site can keep
/// the original while choosing whether to also delete the converted output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KeptFile {
    /// Converted output is smaller — keep it, delete the original source.
    Converted,
    /// Original is smaller or equal — keep it, delete the converted output.
    Original,
    /// No usable converted output (size 0) — keep the original, delete nothing.
    Neither,
}

/// Pure cleanup decision for a finished encode: from the original and converted byte sizes,
/// decide which file to keep, the space saved (signed — negative when the converted file is
/// larger), and the resulting job status. Behavior-preserving extraction of the logic that
/// used to be inlined in `process_queue`; the actual deletion stays at the call site.
fn decide_cleanup(original_size: i64, converted_size: i64) -> (KeptFile, i64, &'static str) {
    let (kept_file, space_saved) = if converted_size > 0 && converted_size < original_size {
        (KeptFile::Converted, original_size - converted_size)
    } else if converted_size > 0 {
        (KeptFile::Original, original_size - converted_size)
    } else {
        (KeptFile::Neither, 0)
    };

    // Original and Neither both record kept_file = "original" in the DB, so the
    // skipped/done check treats them identically — exactly as the pre-refactor code did.
    let status = if matches!(kept_file, KeptFile::Original | KeptFile::Neither)
        && converted_size >= original_size
    {
        "skipped"
    } else {
        "done"
    };

    (kept_file, space_saved, status)
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct ConversionProgress {
    pub job_id: String,
    pub percent: f64,
    pub fps: f64,
    pub avg_fps: f64,
    pub eta_seconds: u64,
}

#[derive(Clone, serde::Serialize)]
pub struct LowDiskPause {
    pub path: String,
    pub available_bytes: u64,
    pub required_bytes: u64,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct MenuBarUpdate {
    pub status: String,
    pub percent: Option<f64>,
    pub file_name: Option<String>,
    pub eta_seconds: Option<u64>,
    pub queue_count: Option<usize>,
    pub fps: Option<f64>,
}

fn parse_progress(line: &str) -> Option<(f64, f64, f64, u64)> {
    use std::sync::OnceLock;

    static FULL_RE: OnceLock<Regex> = OnceLock::new();
    static SIMPLE_RE: OnceLock<Regex> = OnceLock::new();

    // Only match lines containing "Encoding:" to avoid false positives from log lines
    if !line.contains("Encoding:") {
        return None;
    }

    // Try full format: percent + fps + ETA
    let full_re = FULL_RE.get_or_init(|| {
        Regex::new(
            r"Encoding:.*?(\d+\.?\d*)\s*%\s*\((\d+\.?\d*)\s*fps,\s*avg\s*(\d+\.?\d*)\s*fps,\s*ETA\s*(\d+)h(\d+)m(\d+)s\)"
        ).unwrap()
    });

    if let Some(caps) = full_re.captures(line) {
        let percent: f64 = caps.get(1)?.as_str().parse().ok()?;
        let fps: f64 = caps.get(2)?.as_str().parse().ok()?;
        let avg_fps: f64 = caps.get(3)?.as_str().parse().ok()?;
        let hours: u64 = caps.get(4)?.as_str().parse().ok()?;
        let minutes: u64 = caps.get(5)?.as_str().parse().ok()?;
        let seconds: u64 = caps.get(6)?.as_str().parse().ok()?;
        let eta = hours * 3600 + minutes * 60 + seconds;
        return Some((percent, fps, avg_fps, eta));
    }

    // Fallback: percent only (early progress lines without fps/ETA)
    let simple_re = SIMPLE_RE.get_or_init(|| Regex::new(r"Encoding:.*?(\d+\.?\d*)\s*%").unwrap());

    if let Some(caps) = simple_re.captures(line) {
        let percent: f64 = caps.get(1)?.as_str().parse().ok()?;
        return Some((percent, 0.0, 0.0, 0));
    }

    None
}

/// Peak disk headroom multiplier applied to the next file's source size. An in-place re-encode
/// writes its temp output alongside the still-present source on the same filesystem, so usage
/// peaks at ~2× the source before cleanup removes one.
const LOW_DISK_HEADROOM_FACTOR: u64 = 2;

/// Bytes that must remain free on a job's destination filesystem before its encode may start:
/// the user's configured floor plus headroom for the encode itself. Saturating so an enormous
/// configured floor (or source size) can't wrap.
pub(crate) fn required_free_bytes(reserve_floor: u64, source_size: u64) -> u64 {
    reserve_floor.saturating_add(source_size.saturating_mul(LOW_DISK_HEADROOM_FACTOR))
}

/// Whether `available` free bytes clear the reserve floor plus the encode headroom.
pub(crate) fn has_enough_disk(available: u64, reserve_floor: u64, source_size: u64) -> bool {
    available >= required_free_bytes(reserve_floor, source_size)
}

/// GiB (1024³ bytes), as configured in settings, to bytes. `f64 as u64` saturates at `u64::MAX`
/// and clamps negatives/zero to 0, so a garbage or huge stored value can't panic or wrap.
pub(crate) fn gb_to_bytes(gb: f64) -> u64 {
    if gb <= 0.0 {
        return 0;
    }
    (gb * 1024.0 * 1024.0 * 1024.0) as u64
}

/// Free bytes available to this process on the filesystem holding `output_path`'s parent
/// directory (the output file itself does not exist yet). `None` when the parent can't be
/// resolved or the platform query fails — the caller treats that as "don't block the queue".
fn destination_available_bytes(output_path: &str) -> Option<u64> {
    let parent = std::path::Path::new(output_path).parent()?;
    fs4::available_space(parent).ok()
}

fn get_next_job(db: &Connection) -> Option<JobInfo> {
    let mut stmt = db
        .prepare(
            "SELECT id, source_path, output_path, preset, status, original_size, converted_size,
                kept_file, space_saved, error_message, failure_class, queue_order, created_at,
                completed_at, started_at
         FROM jobs WHERE status = 'queued'
         ORDER BY queue_order ASC LIMIT 1",
        )
        .ok()?;

    stmt.query_row([], |row| {
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
            started_at: row.get(14)?,
        })
    })
    .ok()
}

/// Takes the locator as a parameter rather than a `&Ctx`: this runs *under* the DB guard at its
/// `process_queue` call site, and a `&Ctx`-taking resolver would invite re-locking the
/// non-reentrant `ctx.db` mutex there.
fn get_handbrake_path(
    db: &Connection,
    locator: &dyn crate::handbrake::HandbrakeLocator,
) -> Option<String> {
    let configured: Option<String> = db
        .query_row(
            "SELECT value FROM settings WHERE key = 'handbrake_path'",
            [],
            |row| row.get(0),
        )
        .ok();

    crate::handbrake::resolve_with_locator(configured.as_deref(), locator)
}

fn get_cleanup_mode(db: &Connection) -> String {
    crate::settings_ops::read_cleanup_mode(db)
}

fn get_low_disk_min_gb(db: &Connection) -> f64 {
    db.query_row(
        "SELECT value FROM settings WHERE key = 'low_disk_min_gb'",
        [],
        |row| row.get::<_, String>(0),
    )
    .ok()
    .and_then(|v| v.parse::<f64>().ok())
    .unwrap_or(0.0)
}

/// Persisted "the user deliberately stopped the queue" flag, stored in the settings table.
/// Read-with-default (no seed) so existing databases need no migration and the settings-count
/// guard test is untouched. It is backend runtime state — NOT in ALLOWED_KEYS, NOT in the UI.
// `pub` (not `pub(crate)`): the desktop-only updater's test suite (src-tauri/src/updater.rs)
// calls this directly across the crate boundary to set up queue-pause scenarios.
pub fn set_queue_paused(db: &Connection, paused: bool) {
    // Lifting a stop that is actually in force means somebody — Start, Resume, Cancel, a cleared
    // queue, a watched file, or the launch-time lift itself — has taken ownership of the pause
    // state. The updater's `update_drain_pause` breadcrumb records only that it caused *a* pause,
    // never which one, so it must not outlive that: otherwise a later, unrelated pause the user
    // set deliberately would be lifted at the next launch. Guarded on the queue really being
    // paused, so a no-op clear (the watcher's, while a drain is armed but has not landed yet)
    // does not spend a breadcrumb that refers to a stop still to come.
    if !paused && is_queue_paused(db) {
        forget_drain_pause(db);
    }
    let _ = db.execute(
        "INSERT INTO settings (key, value) VALUES ('queue_paused', ?1)
         ON CONFLICT(key) DO UPDATE SET value = ?1",
        params![if paused { "true" } else { "false" }],
    );
}

/// Persists a stop the *user* asked for: the paused flag, plus the release of any updater claim
/// on it.
///
/// One function so the two cannot drift apart. A user pause that left the updater's breadcrumb
/// standing would be lifted by a failed install or by the next launch — and on unix the two are
/// otherwise indistinguishable, because SIGSTOP leaves the queue thread alive so `is_running`
/// never clears.
pub fn set_user_queue_pause(db: &Connection) {
    set_queue_paused(db, true);
    forget_drain_pause(db);
}

/// Backend-only settings row recording that a persisted queue pause was the *updater's* doing —
/// it drained a busy queue for a user-requested "Install and restart". Read-with-default (no
/// seed) so existing databases need no migration and the settings-count guard is untouched. NOT
/// in ALLOWED_KEYS, NOT in the UI — same discipline as `queue_paused`.
///
/// Lives here rather than in the desktop shell's `updater` module because `set_queue_paused`
/// below must release the claim, and core cannot reach into the shell.
pub fn read_drain_pause(db: &Connection) -> bool {
    db.query_row(
        "SELECT value FROM settings WHERE key = 'update_drain_pause'",
        [],
        |r| r.get::<_, String>(0),
    )
    .map(|v| v == "true")
    .unwrap_or(false)
}

pub fn set_drain_pause(db: &Connection, armed: bool) {
    let _ = db.execute(
        "INSERT INTO settings (key, value) VALUES ('update_drain_pause', ?1)
         ON CONFLICT(key) DO UPDATE SET value = ?1",
        params![if armed { "true" } else { "" }],
    );
}

/// Drops the updater's claim on the persisted queue pause.
///
/// The breadcrumb is a bare boolean — it records that the updater caused *a* pause, not *which*
/// one — so it is only sound for as long as nobody else has touched the pause state. Called by
/// `set_queue_paused` whenever a real stop is being lifted, and by `pause_conversion` when the
/// user stops the queue themselves: after either, the pause in force is somebody else's and
/// lifting it at the next launch would override a deliberate decision.
pub fn forget_drain_pause(db: &Connection) {
    set_drain_pause(db, false);
}

/// The launch-time decision: whether to start the queue, after lifting any pause the updater
/// itself caused. Composed here rather than inline in `setup()` so the "consult the breadcrumb
/// before honouring the pause" step is pinned by a test — `setup()` itself is not reachable from
/// one.
pub fn should_resume_queue_at_launch(db: &Connection, has_queued: bool) -> bool {
    let queue_paused = take_drain_pause(db, is_queue_paused(db));
    should_auto_resume(has_queued, queue_paused)
}

/// Lifts a queue pause the updater itself caused, exactly once, and reports the queue's real
/// paused state.
///
/// A user-initiated "Install and restart" against a busy queue drains it by arming
/// `pause_after_current`, which `process_queue` consumes into a *persisted* `queue_paused = true`
/// plus a `break` with jobs still queued. The user never pressed Pause — the updater did — so
/// leaving that pause in force after the update restart would strand the rest of their batch
/// indefinitely. The breadcrumb marks the pause as the updater's doing.
///
/// Consumed whether or not it is used, so a breadcrumb left behind by an install that never got
/// as far as pausing anything cannot resurface against an unrelated pause later.
pub fn take_drain_pause(db: &Connection, queue_paused: bool) -> bool {
    // Read before any write: `set_queue_paused(false)` below re-enters `forget_drain_pause`.
    let was_update_drain = read_drain_pause(db);
    set_drain_pause(db, false);
    if was_update_drain && queue_paused {
        set_queue_paused(db, false);
        return false;
    }
    queue_paused
}

pub fn is_queue_paused(db: &Connection) -> bool {
    db.query_row(
        "SELECT value FROM settings WHERE key = 'queue_paused'",
        [],
        |r| r.get::<_, String>(0),
    )
    .map(|v| v == "true")
    .unwrap_or(false)
}

/// Whether launch should auto-start the queue: only when jobs are queued AND the user did not
/// leave the queue deliberately paused. Pure so the launch decision is unit-testable.
pub fn should_auto_resume(has_queued: bool, queue_paused: bool) -> bool {
    has_queued && !queue_paused
}

/// Whether a source path is *confirmed* gone, from the result of a `try_exists` probe.
/// Only a clean `Ok(false)` counts. An `Err` — an unreadable parent directory, a stalled
/// mount — means the answer is unknown, so fail open (same policy as the low-disk gate) and
/// let HandBrake try: a stat quirk must never report a file that exists as deleted.
fn source_is_confirmed_missing(probe: std::io::Result<bool>) -> bool {
    matches!(probe, Ok(false))
}

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
pub(crate) fn source_is_readable(path: &str) -> bool {
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

/// Outcome of trying to claim the next job by flipping it 'queued' -> 'encoding'. The claim is
/// conditional (`AND status = 'queued'`) so a job that `clear_queue`/`remove_job` deleted during
/// the pre-spawn window is not resurrected — spawning on a deleted row could trash the source.
#[derive(Debug, PartialEq, Eq)]
enum ClaimOutcome {
    Claimed,
    Gone,
    Failed(String),
}

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

const STDERR_TAIL_BYTES: usize = 8192;

/// Drain a reader to EOF keeping only the last STDERR_TAIL_BYTES bytes, so the pipe
/// never fills while memory stays bounded no matter how much HandBrake logs.
fn read_bounded_tail(mut reader: impl Read) -> String {
    let mut tail: Vec<u8> = Vec::with_capacity(STDERR_TAIL_BYTES * 2);
    let mut buf = [0u8; 4096];
    while let Ok(n) = reader.read(&mut buf) {
        if n == 0 {
            break;
        }
        tail.extend_from_slice(&buf[..n]);
        if tail.len() > STDERR_TAIL_BYTES {
            tail.drain(..tail.len() - STDERR_TAIL_BYTES);
        }
    }
    String::from_utf8_lossy(&tail).into_owned()
}

const ERROR_TAIL_LINES: usize = 20;

/// A failure prefix plus the informative end of HandBrake's stderr, so the history
/// entry says WHY the encode failed instead of just that it did. The diagnostic line
/// is promoted to the headline because the UI truncates the entry to a single line —
/// leading with HandBrake's build banner would bury the reason (see the "Compile-time
/// hardening features are enabled" false alarm).
fn message_with_tail(prefix: &str, tail: &str) -> String {
    let lines: Vec<&str> = tail.lines().filter(|l| !l.trim().is_empty()).collect();
    if lines.is_empty() {
        return prefix.to_string();
    }
    let start = lines.len().saturating_sub(ERROR_TAIL_LINES);
    let tail_block = lines[start..].join("\n");
    match crate::failure_class::diagnostic_headline(&lines) {
        Some(headline) => format!("{prefix}: {headline}\n{tail_block}"),
        None => format!("{prefix}:\n{tail_block}"),
    }
}

fn error_message_from_tail(tail: &str) -> String {
    message_with_tail("Conversion failed", tail)
}

/// Zero-byte outputs fail with the same stderr diagnostics as a nonzero exit —
/// HandBrake usually said why nothing was written (e.g. "No title found").
fn empty_output_error_message(tail: &str) -> String {
    message_with_tail("Conversion produced an empty output file", tail)
}

/// Record a failed job WITHOUT notifying: status + error_message in the DB and the two
/// frontend events. For failures the user already knows about because they caused them —
/// a source file they moved or deleted — where a desktop notification is just noise.
fn record_job_error_quiet(
    ctx: &Ctx,
    job_id: &str,
    err_msg: &str,
    class: crate::failure_class::FailureClass,
) {
    let now = chrono::Utc::now().to_rfc3339();
    {
        let db = ctx.db.lock().unwrap();
        let _ = db.execute(
            "UPDATE jobs SET status = 'error', error_message = ?2, completed_at = ?3, \
             failure_class = ?4 WHERE id = ?1",
            params![job_id, err_msg, now, class.as_str()],
        );
    }
    ctx.events.emit_t(
        "job-error",
        serde_json::json!({ "job_id": job_id, "error": err_msg }),
    );
    ctx.events.emit_t(
        "job-status-changed",
        serde_json::json!({ "job_id": job_id, "status": "error" }),
    );
}

/// Record a failed job: everything `record_job_error_quiet` does, plus the per-file
/// notification. The default for every failure path in process_queue.
fn record_job_error(
    ctx: &Ctx,
    job_id: &str,
    file_name: &str,
    err_msg: &str,
    class: crate::failure_class::FailureClass,
) {
    record_job_error_quiet(ctx, job_id, err_msg, class);

    let notify_per_file = {
        let db = ctx.db.lock().unwrap();
        db.query_row(
            "SELECT value FROM settings WHERE key='notifications_per_file'",
            params![],
            |r| r.get::<_, String>(0),
        )
        .map(|v| v == "true")
        .unwrap_or(true)
    };
    if notify_per_file {
        ctx.events
            .notify("ConvertBar", &format!("{} failed", file_name));
    }
}

/// Consume the "pause after current job" flag: returns true when the queue should stop after the
/// job that just finished, clearing the flag so the pause fires exactly once. A single atomic take
/// replaces the previous read-then-clear (two separate lock acquisitions) at the call site.
fn take_pause_after_current(converter: &ConverterState) -> bool {
    let mut guard = converter.pause_after_current.lock().unwrap();
    std::mem::replace(&mut *guard, false)
}

/// The menu-bar status shown once the queue drains: `error` if any job failed during the run,
/// otherwise `idle`. Kept pure so the end-of-run transition is unit-testable.
fn final_run_status(had_errors: bool) -> &'static str {
    if had_errors {
        "error"
    } else {
        "idle"
    }
}

/// Resets `is_running` to false when the queue thread exits — including on an unwinding panic,
/// so a crash in `process_queue` can't wedge the queue by leaving the flag stuck true (which
/// makes every future `run_queue` early-return, permanently). Poison-tolerant so a poisoned
/// `is_running` still gets cleared.
struct RunningGuard<'a>(&'a ConverterState);

impl Drop for RunningGuard<'_> {
    fn drop(&mut self) {
        *self.0.is_running.lock().unwrap_or_else(|e| e.into_inner()) = false;
    }
}

/// Core queue processing logic. Call from a background thread.
/// The `is_running` flag must be set to true before calling this.
fn process_queue(ctx: &Ctx) {
    // Clears is_running on every exit path (normal, early return, or panic).
    let _running = RunningGuard(&ctx.converter);
    // Every run/resume starts fresh: a stale reason from a prior pause must not linger once
    // the queue is running again (and be gone entirely if this run never hits the gate).
    *ctx.converter.low_disk_pause.lock().unwrap() = None;
    let mut had_errors = false;
    loop {
        // Quit path: kill_active_child armed shutdown. Bail before picking up another
        // job — teardown would otherwise race a fresh HandBrakeCLI spawn and orphan it.
        // Return (not break) so no "Queue complete" notification fires mid-quit.
        if ctx.converter.is_shutting_down() {
            return;
        }
        let job;
        let handbrake_path_opt;
        let low_disk_min_gb;
        {
            let db = ctx.db.lock().unwrap();
            job = match get_next_job(&db) {
                Some(j) => j,
                None => break,
            };
            handbrake_path_opt = get_handbrake_path(&db, &*ctx.handbrake);
            low_disk_min_gb = get_low_disk_min_gb(&db);
            // The job is flipped to 'encoding' below, AFTER the low-disk gate — a gated job
            // must stay 'queued' so the Resume button can retry it.
            //
            // cleanup_mode is deliberately NOT captured here: an encode can run for minutes to
            // hours, and the user may switch to "keep" while this job is mid-flight (see the
            // fresh read right before the cleanup decision, below).
        }

        let file_name = std::path::Path::new(&job.source_path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown")
            .to_string();

        // Vanished-source gate: a queued file can be moved, trashed, or consumed by another tool
        // before its turn. Handing the dead path to HandBrakeCLI makes it fail with the reason
        // buried in a stderr dump, so stat first and record what actually happened. Quietly: the
        // user removed the file themselves, so the history entry is enough — no notification.
        // Ahead of the low-disk gate on purpose: a job that can never run must not hold up the
        // queue behind a disk pause, and the gate would size it from a source that isn't there.
        if source_is_confirmed_missing(std::path::Path::new(&job.source_path).try_exists()) {
            had_errors = true;
            record_job_error_quiet(
                ctx,
                &job.id,
                "Source file no longer exists",
                crate::failure_class::FailureClass::Environment,
            );
            continue;
        }

        // Low-disk gate: before committing this job to 'encoding', ensure the destination
        // filesystem has room for the floor plus the encode (2× source). On a shortfall, stop
        // the run like "Pause after this" — leave the job 'queued', tell the UI why, and return
        // (nothing completed, so no "Queue complete" notification). Fail open: a 0 threshold, an
        // unresolvable parent, or a failed free-space query all let the encode proceed.
        if low_disk_min_gb > 0.0 {
            if let Some(available) = destination_available_bytes(&job.output_path) {
                let floor = gb_to_bytes(low_disk_min_gb);
                let source_size = job.original_size.unwrap_or(0).max(0) as u64;
                if !has_enough_disk(available, floor, source_size) {
                    let required = required_free_bytes(floor, source_size);
                    let pause = LowDiskPause {
                        path: job.output_path.clone(),
                        available_bytes: available,
                        required_bytes: required,
                    };
                    *ctx.converter.low_disk_pause.lock().unwrap() = Some(pause.clone());
                    ctx.events.emit_t("queue-paused-low-disk", &pause);
                    ctx.events.emit_t(
                        "menu-bar-update",
                        MenuBarUpdate {
                            // Reuse the end-of-run status so a prior job's failure in this run
                            // still surfaces as "error" in the tray; otherwise a paused queue reads
                            // "idle". (had_errors is in scope at the top of process_queue.)
                            status: final_run_status(had_errors).to_string(),
                            percent: None,
                            file_name: None,
                            eta_seconds: None,
                            queue_count: None,
                            fps: None,
                        },
                    );
                    return;
                }
            }
        }

        // Outside the db-lock scope: record_job_error takes the lock itself, and this
        // failure must count toward had_errors and emit the same events as any other.
        let handbrake_path = match handbrake_path_opt {
            Some(p) => p,
            None => {
                had_errors = true;
                record_job_error(
                    ctx,
                    &job.id,
                    &file_name,
                    crate::handbrake::HANDBRAKE_NOT_FOUND,
                    crate::failure_class::FailureClass::Environment,
                );
                continue;
            }
        };

        // Claim the job by flipping it to 'encoding' ONLY if it is still 'queued'. The original
        // code did this select+flip under a single db-lock; relocating the flip past the disk
        // gate (whose statvfs can stall on a slow/asleep/network volume) reopened a window where
        // `clear_queue`/`remove_job` — which delete 'queued' rows — could remove this job before
        // the flip. Without the `AND status = 'queued'` guard the loop would then spawn HandBrake
        // on a deleted row and, on success, trash/delete the user's SOURCE file. The conditional
        // claim + row-count check closes that window: 0 rows affected means the job is gone, so
        // skip it.
        let claim = {
            let conn = ctx.db.lock().unwrap();
            claim_job(&conn, &job.id)
        };
        match claim {
            ClaimOutcome::Claimed => {}
            // Removed during the pre-spawn window (e.g. Clear/Remove while the disk stat stalled).
            ClaimOutcome::Gone => continue,
            // A failing UPDATE must NOT re-loop on the same job forever; record and move on.
            ClaimOutcome::Failed(e) => {
                had_errors = true;
                record_job_error(
                    ctx,
                    &job.id,
                    &file_name,
                    &format!("Failed to claim job: {e}"),
                    crate::failure_class::FailureClass::Environment,
                );
                continue;
            }
        }

        *ctx.converter.current_job_id.lock().unwrap() = Some(job.id.clone());
        *ctx.converter.is_paused.lock().unwrap() = false;

        ctx.events.emit_t(
            "job-status-changed",
            serde_json::json!({
                "job_id": job.id,
                "status": "encoding"
            }),
        );

        // Count remaining queued jobs for tray info
        let queue_count: usize = {
            let db = ctx.db.lock().unwrap();
            db.query_row(
                "SELECT COUNT(*) FROM jobs WHERE status = 'queued'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap_or(0) as usize
        };

        ctx.events.emit_t(
            "menu-bar-update",
            MenuBarUpdate {
                status: "encoding".to_string(),
                percent: Some(0.0),
                file_name: Some(file_name.clone()),
                eta_seconds: None,
                queue_count: Some(queue_count),
                fps: None,
            },
        );

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

        // Spawn HandBrakeCLI
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

        let mut child = match child {
            Ok(c) => c,
            Err(e) => {
                had_errors = true;
                record_job_error(
                    ctx,
                    &job.id,
                    &file_name,
                    &format!("Failed to start HandBrakeCLI: {}", e),
                    crate::failure_class::FailureClass::Environment,
                );
                *ctx.converter.current_job_id.lock().unwrap() = None;
                continue;
            }
        };

        let pid = child.id();
        *ctx.converter.current_pid.lock().unwrap() = Some(pid);

        // Read stdout for progress (HandBrakeCLI sends progress to stdout when piped)
        let progress_stream = child.stdout.take();

        // Drain stderr so the process doesn't block on a full pipe buffer, keeping a
        // bounded tail — HandBrake writes its failure reason there, and a bare
        // "Conversion failed" has proven undiagnosable in bug reports.
        let stderr_tail_thread = child
            .stderr
            .take()
            .map(|stderr| std::thread::spawn(move || read_bounded_tail(stderr)));

        // Store child handle for cross-platform cancel support
        *ctx.converter.current_child.lock().unwrap() = Some(child);

        // Close the spawn→store window: if quit armed shutdown after the spawn but
        // before the handle was stored, kill_active_child found None — reap it here.
        // wait_for_active_child then sees the killed status and takes the error path.
        if ctx.converter.is_shutting_down() {
            kill_active_child(&ctx.converter);
        }

        let job_id = job.id.clone();
        let events_clone = ctx.events.clone();
        let file_name_clone = file_name.clone();

        let progress_thread = if let Some(stdout) = progress_stream {
            let handle = std::thread::spawn(move || {
                let mut reader = stdout;
                let mut buf = [0u8; 1024];
                let mut partial = String::new();
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) => break,
                        Ok(n) => {
                            partial.push_str(&String::from_utf8_lossy(&buf[..n]));
                            while let Some(pos) = partial.find(|c: char| c == '\r' || c == '\n') {
                                let line = partial[..pos].to_string();
                                partial = partial[pos + 1..].to_string();
                                if !line.is_empty() {
                                    if let Some((percent, fps, avg_fps, eta)) =
                                        parse_progress(&line)
                                    {
                                        events_clone.emit_t(
                                            "conversion-progress",
                                            ConversionProgress {
                                                job_id: job_id.clone(),
                                                percent,
                                                fps,
                                                avg_fps,
                                                eta_seconds: eta,
                                            },
                                        );
                                        events_clone.emit_t(
                                            "menu-bar-update",
                                            MenuBarUpdate {
                                                status: "encoding".to_string(),
                                                percent: Some(percent),
                                                file_name: Some(file_name_clone.clone()),
                                                eta_seconds: Some(eta),
                                                queue_count: None,
                                                fps: Some(fps),
                                            },
                                        );
                                    }
                                }
                            }
                        }
                        Err(_) => break,
                    }
                }
            });
            Some(handle)
        } else {
            None
        };

        let exit_status = wait_for_active_child(&ctx.converter);

        if let Some(handle) = progress_thread {
            let _ = handle.join();
        }

        *ctx.converter.current_pid.lock().unwrap() = None;
        *ctx.converter.current_child.lock().unwrap() = None;
        *ctx.converter.current_job_id.lock().unwrap() = None;

        // Joined here rather than inside individual arms: the child has already exited, so
        // the drain thread is at EOF and this returns promptly on every path. The success
        // arm needs the tail too (truncation detection), and hoisting also removes the
        // duplicate join the two failure arms previously had.
        let tail = stderr_tail_thread
            .and_then(|t| t.join().ok())
            .unwrap_or_default();

        match exit_status {
            Ok(status) if status.success() => {
                let converted_size = std::fs::metadata(&encode_target)
                    .map(|m| m.len() as i64)
                    .ok();

                // HandBrake can exit 0 yet write nothing usable. A missing or empty
                // output is never a success — without this guard, cleanup could trash
                // the source in favor of a 0-byte file and history would record
                // "done — saved 0B".
                if converted_size.unwrap_or(0) == 0 {
                    had_errors = true;
                    let _ = std::fs::remove_file(&encode_target);
                    let class =
                        crate::failure_class::classify(&crate::failure_class::FailureFacts {
                            exit_code: status.code(),
                            source_readable: source_is_readable(&job.source_path),
                            stderr_tail: &tail,
                        });
                    // Stamp the identity fingerprint to the source's CURRENT stat at the
                    // moment of condemnation, not the stale add-time value — see
                    // `record_condemned_identity`. Never for Environment/Unknown: those aren't
                    // a verdict on the file at all.
                    if class == crate::failure_class::FailureClass::BadSource {
                        record_condemned_identity(&ctx.db, &job.id, &job.source_path);
                    }
                    record_job_error(
                        ctx,
                        &job.id,
                        &file_name,
                        &empty_output_error_message(&tail),
                        class,
                    );
                    continue;
                }

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
                        // Same re-stamp as the classify()-based site below: the truncation is
                        // discovered right now, so the fingerprint recorded at queue time is
                        // stale the instant it's condemned. See `record_condemned_identity`.
                        record_condemned_identity(&ctx.db, &job.id, &job.source_path);
                        record_job_error(
                            ctx,
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

                // Re-read fresh, right here, rather than trusting the value captured at job
                // pickup: this encode can have run for minutes or hours, and update_setting only
                // drops in-place jobs that are still 'queued' — it cannot reach a row that was
                // already 'encoding'. Without this re-read, a job already in flight when the user
                // clicks "Keep both files" would still apply the STALE pre-encode mode below,
                // permanently destroying the original the user just asked to keep. Narrowly
                // scoped and dropped before any emit below (never hold ctx.db across
                // ctx.events.emit_t — the tray listener re-locks it synchronously and
                // std::sync::Mutex is not reentrant).
                let cleanup_mode = {
                    let db = ctx.db.lock().unwrap();
                    get_cleanup_mode(&db)
                };

                // Set when the distinct-file cleanup left the loser on disk; carries which file
                // that was so the error can name it.
                let mut cleanup_failed: Option<KeptFile> = None;

                // Which action was actually taken for an in-place job, so the fatality check
                // below can key off the action rather than off `kept` — see
                // `in_place_apply_is_fatal`'s doc.
                let mut in_place_action_taken: Option<InPlaceAction> = None;

                // Act on the decision. In-place replaces/keeps the source via the temp; the
                // distinct-file path keeps both names and trashes/deletes the loser as before.
                let in_place_apply_failed = if in_place {
                    let action = in_place_action(kept, &cleanup_mode);
                    in_place_action_taken = Some(action);
                    apply_in_place_action(
                        action,
                        &encode_target,
                        std::path::Path::new(&job.source_path),
                        ctx.disposer.as_ref(),
                    )
                    .is_err()
                } else if cleanup_mode == "keep" {
                    // Keep both files. decide_cleanup still ran, so status and space_saved are
                    // unchanged — only the disposal is skipped. cleanup_failed deliberately stays
                    // None: under keep the loser is on disk BY DESIGN, so the exists() verdict
                    // below must not run or every keep job would report a false cleanup failure.
                    false
                } else {
                    // The loser of the size comparison — the file this job promises to remove.
                    // Neither means there is nothing to remove (no usable output).
                    let loser = match kept {
                        KeptFile::Converted => Some(job.source_path.as_str()),
                        KeptFile::Original => Some(job.output_path.as_str()),
                        KeptFile::Neither => None,
                    };
                    if let Some(loser) = loser {
                        if cleanup_mode == "delete" {
                            let _ = std::fs::remove_file(loser);
                        } else {
                            let _ = ctx.disposer.dispose(loser);
                        }
                        // Read the verdict off the filesystem, not off the primitive's bool: a
                        // loser that is gone satisfies the contract however the call reported
                        // (a source that vanished mid-encode makes `dispose` report failure),
                        // and a loser still sitting there is a failure however it reported.
                        if std::path::Path::new(loser).exists() {
                            cleanup_failed = Some(kept);
                        }
                    }
                    false
                };

                // A failed in-place *rename* means the re-encode never replaced the source (and in
                // trash mode the original may now be in Trash, with the temp left behind). Record an
                // error instead of a false "done" so history never claims a success that left the
                // file out of place. A failed temp *removal* is benign and handled as success.
                if in_place_apply_is_fatal(in_place_action_taken, in_place_apply_failed) {
                    had_errors = true;
                    // Which of the two shapes this is, read off the filesystem rather than
                    // inferred from the mode: the original is still in place both when the
                    // rename failed (delete mode) and when the Trash step was refused, and is
                    // in the Trash only when it genuinely left. Keying this on cleanup_mode
                    // sent trash-mode users hunting in the Trash for a file still on disk.
                    let err_msg = if std::path::Path::new(&job.source_path).exists() {
                        "In-place replacement failed; original left unchanged"
                    } else {
                        "In-place replacement failed; original may be in Trash"
                    };
                    record_job_error(
                        ctx,
                        &job.id,
                        &file_name,
                        err_msg,
                        crate::failure_class::FailureClass::Environment,
                    );
                    // Intentionally leave the temp (`.{stem}.convertbar-tmp.mp4`): it holds the
                    // re-encoded content, and when the original did reach the Trash it is the
                    // only in-place copy, so removing it would force trash recovery. The marker
                    // keeps it out of scans, and the next in-place encode pre-clears it.
                    continue;
                }

                // The distinct-file counterpart: the encode itself succeeded, but the file this
                // job promised to remove is still on disk, so both copies remain. Recording
                // 'done'/'skipped' here would claim a cleanup that did not happen and book
                // space_saved that was never freed — the shape of the v2.0.0 field regression,
                // where macOS refused the Trash Apple Event and a whole queue silently kept
                // every original while reporting gigabytes saved.
                if let Some(stuck) = cleanup_failed {
                    had_errors = true;
                    let which = match stuck {
                        KeptFile::Converted => "original",
                        // Only reachable for Original: Neither never sets cleanup_failed.
                        _ => "larger re-encode",
                    };
                    let verb = if cleanup_mode == "delete" {
                        "deleted"
                    } else {
                        "moved to Trash"
                    };
                    record_job_error(
                        ctx,
                        &job.id,
                        &file_name,
                        &format!("Encode finished, but the {which} could not be {verb}; both files remain"),
                        crate::failure_class::FailureClass::Environment,
                    );
                    continue;
                }

                // In-place + keep is the backstop for the race where a job reaches
                // 'encoding'/'paused' between the user choosing keep and this job's turn:
                // drop_queued_in_place_jobs (queue_ops.rs) only reaches rows still waiting
                // ('queued'/'paused') and can't touch one already running unpaused. RemoveTemp,
                // above, already discarded the wasted re-encode and left the untouched source in
                // place. Recording a 'done' row here — as this code used to — would be a lie on
                // three counts: kept_file/converted_size/space_saved would describe a conversion
                // that never happened; the notification below would claim bytes were saved that
                // weren't; and record_source_identity would fingerprint the UNTOUCHED source,
                // which cheap_skip_reason's identity check (queue_ops.rs) is unconditional on —
                // permanently skipping the file as AlreadyConverted on every future scan/add,
                // even after switching back to Delete, with no escape short of clearing History
                // or touching the file. Delete the row instead, mirroring what
                // drop_queued_in_place_jobs does for a job that hadn't started yet: no row, no
                // fingerprint, no false notification, no phantom savings — and the file is fully
                // eligible to be queued and converted again.
                if in_place && cleanup_mode == "keep" {
                    {
                        let db = ctx.db.lock().unwrap();
                        let _ = db.execute("DELETE FROM jobs WHERE id = ?1", params![job.id]);
                    } // guard dropped before the emit below — the tray listener re-locks ctx.db
                      // synchronously on this same thread, and std::sync::Mutex is not reentrant.
                    ctx.events.emit_t("queue-updated", ());
                    continue;
                }

                let kept_file = match kept {
                    KeptFile::Converted => "converted",
                    KeptFile::Original | KeptFile::Neither => "original",
                };

                let now = chrono::Utc::now().to_rfc3339();

                {
                    let db = ctx.db.lock().unwrap();
                    let _ = db.execute(
                        "UPDATE jobs SET status = ?2, converted_size = ?3, kept_file = ?4, space_saved = ?5, completed_at = ?6 WHERE id = ?1",
                        params![job.id, status_str, converted_size, kept_file, space_saved, now],
                    );
                    // In-place encoding replaced the file at source_path, so its insert-time
                    // fingerprint is stale. Refresh it to the final file's identity.
                    if in_place {
                        record_source_identity(&db, &job.id, &job.source_path);
                    }
                }

                ctx.events.emit_t(
                    "job-completed",
                    serde_json::json!({
                        "job_id": job.id,
                        "status": status_str,
                        "kept_file": kept_file,
                        "space_saved": space_saved,
                    }),
                );

                ctx.events.emit_t(
                    "job-status-changed",
                    serde_json::json!({
                        "job_id": job.id,
                        "status": status_str,
                    }),
                );

                // Notification logic for successful/skipped jobs
                {
                    let (notify_per_file, errors_only) = {
                        let db = ctx.db.lock().unwrap();
                        let get = |k: &str, default: bool| -> bool {
                            db.query_row(
                                "SELECT value FROM settings WHERE key=?1",
                                params![k],
                                |r| r.get::<_, String>(0),
                            )
                            .map(|v| v == "true")
                            .unwrap_or(default)
                        };
                        (
                            get("notifications_per_file", true),
                            get("notifications_errors_only", false),
                        )
                    };

                    if notify_per_file {
                        let is_error = status_str == "error";
                        let should_notify = if errors_only { is_error } else { true };

                        if should_notify {
                            let body = match status_str {
                                "done" => format!(
                                    "{} converted — saved {}",
                                    file_name,
                                    format_bytes_short(space_saved)
                                ),
                                "skipped" => {
                                    format!("{} — kept original (converted was larger)", file_name)
                                }
                                _ => format!("{} failed", file_name),
                            };
                            ctx.events.notify("ConvertBar", &body);
                        }
                    }
                }

                // Pause after this job if the one-shot flag is armed (consumed here so the next
                // queued job does not also pause).
                if take_pause_after_current(&ctx.converter) {
                    set_queue_paused(&ctx.db.lock().unwrap(), true);
                    ctx.events.emit_t(
                        "menu-bar-update",
                        MenuBarUpdate {
                            status: "idle".to_string(),
                            percent: None,
                            file_name: None,
                            eta_seconds: None,
                            queue_count: None,
                            fps: None,
                        },
                    );
                    break;
                }
            }
            other => {
                let exit_code = match &other {
                    Ok(s) => s.code(),
                    Err(_) => None,
                };
                // Quit path: the exit handler killed this child, the encode didn't
                // fail. Skip ALL bookkeeping — deleting the partial, writing
                // status='error' (auto-resume only picks up 'encoding'/'paused'),
                // or notifying "failed" would let a scheduler race decide the
                // job's fate. The loop head turns this continue into the return.
                if ctx.converter.is_shutting_down() {
                    continue;
                }
                had_errors = true;
                // Remove the partial encode output (the temp for in-place jobs), never the source.
                let _ = std::fs::remove_file(&encode_target);

                let current_status: Option<String> = ctx
                    .db
                    .lock()
                    .unwrap()
                    .query_row(
                        "SELECT status FROM jobs WHERE id = ?1",
                        params![job.id],
                        |row| row.get(0),
                    )
                    .ok();

                if current_status.as_deref() != Some("error") {
                    let class =
                        crate::failure_class::classify(&crate::failure_class::FailureFacts {
                            exit_code,
                            source_readable: source_is_readable(&job.source_path),
                            stderr_tail: &tail,
                        });
                    // See `record_condemned_identity`: never for Environment/Unknown.
                    if class == crate::failure_class::FailureClass::BadSource {
                        record_condemned_identity(&ctx.db, &job.id, &job.source_path);
                    }
                    record_job_error(
                        ctx,
                        &job.id,
                        &file_name,
                        &error_message_from_tail(&tail),
                        class,
                    );
                }
            }
        }
    }

    // No more jobs — queue done notification
    {
        let notify_queue_done = {
            let db = ctx.db.lock().unwrap();
            db.query_row(
                "SELECT value FROM settings WHERE key='notifications_queue_done'",
                params![],
                |r| r.get::<_, String>(0),
            )
            .map(|v| v == "true")
            .unwrap_or(true)
        };
        if notify_queue_done {
            ctx.events.notify("ConvertBar", "Queue complete");
        }
    }

    let final_status = final_run_status(had_errors);
    ctx.events.emit_t(
        "menu-bar-update",
        MenuBarUpdate {
            status: final_status.to_string(),
            percent: None,
            file_name: None,
            eta_seconds: None,
            queue_count: None,
            fps: None,
        },
    );

    // is_running is reset by RunningGuard on return (and on an unwinding panic).
}

/// Atomically claims the right to run the queue. Returns false when the queue is already
/// running or an update install holds the interlock. Poison-tolerant: if a prior queue thread
/// panicked while briefly holding this lock, recover the flag rather than propagating the
/// poison and permanently wedging starts.
pub fn claim_queue_slot(converter: &ConverterState) -> bool {
    let mut running = converter
        .is_running
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if *running
        || converter
            .installing
            .load(std::sync::atomic::Ordering::SeqCst)
    {
        return false;
    }
    *running = true;
    true
}

/// Starts queue processing in a new background thread.
/// Sets `is_running` to true atomically before spawning.
pub fn run_queue(ctx: Arc<Ctx>) {
    if !claim_queue_slot(&ctx.converter) {
        return;
    }

    std::thread::spawn(move || {
        process_queue(&ctx);
    });
}

/// Waits for the active child process to exit, polling with `try_wait` so the
/// `current_child` lock is released between checks. This lets `cancel_conversion`
/// acquire the lock and kill the process instead of deadlocking against a lock held
/// for the entire blocking `wait()`. Progress is reported by the separate stdout
/// thread, so the poll interval is invisible to the user.
fn wait_for_active_child(converter: &ConverterState) -> std::io::Result<std::process::ExitStatus> {
    loop {
        {
            let mut child_guard = converter.current_child.lock().unwrap();
            match child_guard.as_mut() {
                Some(child) => {
                    if let Some(status) = child.try_wait()? {
                        return Ok(status);
                    }
                    // None: still running — release the lock and poll again
                }
                None => {
                    // Child was already taken (e.g. by cancel), treat as failure
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::Other,
                        "Process handle missing",
                    ));
                }
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

fn format_bytes_short(bytes: i64) -> String {
    let abs = bytes.unsigned_abs();
    if abs >= 1_073_741_824 {
        format!("{:.1}GB", abs as f64 / 1_073_741_824.0)
    } else if abs >= 1_048_576 {
        format!("{:.0}MB", abs as f64 / 1_048_576.0)
    } else if abs >= 1024 {
        format!("{:.0}KB", abs as f64 / 1024.0)
    } else {
        format!("{}B", abs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispose::RecordingDisposer;
    use crate::events::TestSink;

    // --- process_queue integration harness (Ctx + recording sink/disposer, in-memory DB) ---

    fn test_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::init_db(&conn).unwrap();
        // Notifications are recorded by TestSink regardless, but most tests don't care about
        // them — disable so assertions on job-status-changed/menu-bar-update aren't a distraction.
        conn.execute(
            "UPDATE settings SET value = 'false'
             WHERE key IN ('notifications_per_file', 'notifications_queue_done')",
            [],
        )
        .unwrap();
        conn
    }

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

    /// `test_ctx` with a caller-supplied disposer, for tests about what process_queue does
    /// when the Trash primitive itself fails rather than succeeding silently.
    fn test_ctx_with_disposer(
        conn: Connection,
        disposer: Arc<dyn crate::dispose::FileDisposer>,
    ) -> (Arc<Ctx>, Arc<TestSink>) {
        let sink = Arc::new(TestSink::default());
        let ctx = Ctx::new(
            conn,
            sink.clone(),
            disposer,
            Arc::new(crate::handbrake::PanickingLocator),
        );
        (ctx, sink)
    }

    fn saved_of(db: &Arc<Mutex<Connection>>, id: &str) -> Option<i64> {
        db.lock()
            .unwrap()
            .query_row(
                "SELECT space_saved FROM jobs WHERE id = ?1",
                params![id],
                |r| r.get(0),
            )
            .unwrap()
    }

    fn set_setting(db: &Arc<Mutex<Connection>>, key: &str, value: &str) {
        db.lock()
            .unwrap()
            .execute(
                "INSERT OR REPLACE INTO settings (key, value) VALUES (?1, ?2)",
                params![key, value],
            )
            .unwrap();
    }

    fn queue_job(db: &Arc<Mutex<Connection>>, id: &str, source: &str, output: &str, size: i64) {
        db.lock()
            .unwrap()
            .execute(
                "INSERT INTO jobs (id, source_path, output_path, preset, status,
                                   original_size, queue_order, created_at)
                 VALUES (?1, ?2, ?3,
                         (SELECT value FROM settings WHERE key = 'preset'),
                         'queued', ?4, 0, '2020-01-01T00:00:00Z')",
                params![id, source, output, size],
            )
            .unwrap();
    }

    /// A real (tiny) file on disk to point a job's source at. process_queue stats the source
    /// before spawning, so a fixture naming a path that was never created stops at the
    /// vanished-source gate instead of exercising the behavior under test.
    fn real_source(dir: &std::path::Path, name: &str) -> std::path::PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, b"0123456789").unwrap();
        p
    }

    fn job_row(db: &Arc<Mutex<Connection>>, id: &str) -> (String, Option<String>) {
        db.lock()
            .unwrap()
            .query_row(
                "SELECT status, error_message FROM jobs WHERE id = ?1",
                params![id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap()
    }

    fn job_exists(db: &Arc<Mutex<Connection>>, id: &str) -> bool {
        db.lock()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM jobs WHERE id = ?1",
                params![id],
                |r| r.get::<_, i64>(0),
            )
            .unwrap()
            > 0
    }

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
        assert!(
            !readable,
            "an unreadable healthy file must never be credited as readable"
        );
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
        let (ctx, _sink, _disposer) = test_ctx(test_conn());
        queue_job(&ctx.db, "j1", "/src.mkv", "/out.mp4", 1000);
        record_job_error(
            &ctx,
            "j1",
            "src.mkv",
            "Conversion failed",
            crate::failure_class::FailureClass::BadSource,
        );
        assert_eq!(class_of(&ctx.db, "j1").as_deref(), Some("bad_source"));
    }

    #[test]
    fn record_job_error_quiet_persists_environment_for_a_vanished_source() {
        let (ctx, _sink, _disposer) = test_ctx(test_conn());
        queue_job(&ctx.db, "j2", "/gone.mkv", "/out.mp4", 1000);
        record_job_error_quiet(
            &ctx,
            "j2",
            "Source file no longer exists",
            crate::failure_class::FailureClass::Environment,
        );
        assert_eq!(
            class_of(&ctx.db, "j2").as_deref(),
            Some("environment"),
            "a file that vanished is never the user's corrupt-download problem"
        );
    }

    #[test]
    fn a_queued_job_fails_as_environment_when_handbrake_is_missing() {
        // process_queue resolves HandBrake per job. Absent, the job must be recorded as an
        // Environment failure and the queue must move on — not hang, not retry forever, and not be
        // mistaken for a bad source file. Before the locator seam this arm was unreachable in tests,
        // because "HandBrake is not installed" could not be expressed.
        let (ctx, _sink, _disposer) =
            test_ctx_with_locator(test_conn(), Arc::new(crate::handbrake::AbsentLocator));

        let dir = tempfile::tempdir().unwrap();
        // A real file: the vanished-source gate runs first and would otherwise claim this job.
        let src = real_source(dir.path(), "in.mp4");
        queue_job(
            &ctx.db,
            "j1",
            src.to_str().unwrap(),
            "/nowhere/out.mp4",
            1000,
        );

        process_queue(&ctx);

        let (status, msg) = job_row(&ctx.db, "j1");
        assert_eq!(status, "error");
        assert!(
            msg.clone()
                .unwrap_or_default()
                .contains(crate::handbrake::HANDBRAKE_NOT_FOUND),
            "the failure must name the missing binary, not blame the source file — got {msg:?}"
        );
        assert_eq!(class_of(&ctx.db, "j1").as_deref(), Some("environment"));
    }

    #[test]
    fn spawn_failure_surfaces_like_every_other_error() {
        // A configured handbrake_path that exists but is not executable makes
        // Command::spawn fail. That branch must behave like all other failures:
        // job-status-changed fires (so the UI row updates) and the run ends with
        // the tray in "error", not a clean "idle" that hides every job failing.
        let (ctx, sink, _disposer) = test_ctx(test_conn());

        let dir = tempfile::tempdir().unwrap();
        let fake = dir.path().join("not-a-binary.txt");
        std::fs::write(&fake, "plain text").unwrap();
        set_setting(&ctx.db, "handbrake_path", fake.to_str().unwrap());
        let src = real_source(dir.path(), "in.mp4");
        queue_job(
            &ctx.db,
            "j1",
            src.to_str().unwrap(),
            "/nowhere/out.mp4",
            1000,
        );

        process_queue(&ctx);

        let (status, msg) = job_row(&ctx.db, "j1");
        assert_eq!(status, "error");
        assert!(msg.unwrap().contains("Failed to start HandBrakeCLI"));
        assert_eq!(
            class_of(&ctx.db, "j1").as_deref(),
            Some("environment"),
            "a spawn failure is an environment problem, never a bad-source verdict — \
             purge must not treat every queued file as clearable just because HandBrakeCLI \
             couldn't launch"
        );
        assert!(
            sink.payloads("job-status-changed")
                .iter()
                .any(|p| p["status"] == "error"),
            "spawn failure must emit job-status-changed like other failure paths"
        );
        let final_update = sink.payloads("menu-bar-update").last().cloned().unwrap();
        assert!(
            final_update["status"] == "error",
            "a run where the only job failed must end 'error', got: {final_update}"
        );
    }

    #[test]
    fn claim_failure_records_environment_not_bad_source() {
        // A conditional trigger makes claim_job's UPDATE ... SET status='encoding' fail while
        // leaving record_job_error's UPDATE ... SET status='error' untouched — the only way to
        // exercise ClaimOutcome::Failed without a real concurrent DB error. Without this test,
        // someone changing that call site's failure class to BadSource would pass the whole
        // suite, and a transient DB error would route a healthy file into the purge list.
        let (ctx, _sink, _disposer) = test_ctx(test_conn());

        ctx.db
            .lock()
            .unwrap()
            .execute(
                "CREATE TRIGGER block_encoding_claim BEFORE UPDATE ON jobs \
                 WHEN NEW.status = 'encoding' \
                 BEGIN SELECT RAISE(ABORT, 'boom'); END",
                [],
            )
            .unwrap();

        let dir = tempfile::tempdir().unwrap();
        let script = fake_handbrake_script(dir.path());
        set_setting(&ctx.db, "handbrake_path", script.to_str().unwrap());
        let src = real_source(dir.path(), "in.mp4");
        queue_job(
            &ctx.db,
            "j1",
            src.to_str().unwrap(),
            "/nowhere/out.mp4",
            1000,
        );

        process_queue(&ctx);

        let (status, msg) = job_row(&ctx.db, "j1");
        assert_eq!(status, "error");
        assert!(msg.unwrap().contains("Failed to claim job"));
        assert_eq!(
            class_of(&ctx.db, "j1").as_deref(),
            Some("environment"),
            "a DB error while claiming the job is an environment problem, not a verdict on \
             the source file"
        );
    }

    #[test]
    fn low_disk_threshold_pauses_before_spawning_and_leaves_the_job_queued() {
        // An absurd threshold makes required-free exceed any real disk, so the gate always trips —
        // deterministic regardless of the test machine's free space.
        let (ctx, sink, _disposer) = test_ctx(test_conn());

        let dir = tempfile::tempdir().unwrap();
        // A real fake HandBrake IS configured: if the gate failed to stop the run, the job would be
        // spawned and end 'error' (empty output). Staying 'queued' proves the gate blocked the spawn.
        let script = fake_handbrake_script(dir.path());
        set_setting(&ctx.db, "handbrake_path", script.to_str().unwrap());
        set_setting(&ctx.db, "low_disk_min_gb", "1000000000"); // 1e9 GB
        let out = dir.path().join("out.mp4");
        let src = real_source(dir.path(), "a.mp4");
        queue_job(
            &ctx.db,
            "j1",
            src.to_str().unwrap(),
            out.to_str().unwrap(),
            1000,
        );

        *ctx.converter.is_running.lock().unwrap() = true;
        process_queue(&ctx);

        let (status, _msg) = job_row(&ctx.db, "j1");
        assert_eq!(
            status, "queued",
            "a low-disk pause leaves the job queued, never encoding/error"
        );
        assert!(
            !out.exists(),
            "the encode must never start, so no output is written"
        );
        let paused_events = sink.payloads("queue-paused-low-disk");
        assert_eq!(
            paused_events.len(),
            1,
            "exactly one low-disk pause event fires"
        );
        // Assert the actual payload VALUES, not just the presence of the key: an available/required
        // swap or the wrong path would still contain "required_bytes" but mislead the UI.
        let v = &paused_events[0];
        let available = v["available_bytes"]
            .as_u64()
            .expect("available_bytes is a number");
        let required = v["required_bytes"]
            .as_u64()
            .expect("required_bytes is a number");
        assert!(
            available < required,
            "a pause only fires on a shortfall: available ({available}) must be < required ({required})"
        );
        assert_eq!(
            v["path"].as_str(),
            out.to_str(),
            "the pause event names the job's output path so the UI can point the user at it"
        );
        assert!(
            sink.payloads("job-status-changed")
                .iter()
                .all(|p| p["status"] != "encoding"),
            "the job must never transition to encoding"
        );
        assert!(
            !*ctx.converter.is_running.lock().unwrap(),
            "the queue thread must have stopped"
        );
    }

    #[test]
    fn low_disk_check_is_skipped_when_threshold_is_zero() {
        // Threshold 0 = disabled: the gate is a no-op and the job runs through to the encode stage.
        let (ctx, _sink, _disposer) = test_ctx(test_conn());

        let dir = tempfile::tempdir().unwrap();
        let script = fake_handbrake_script(dir.path()); // exits 0, writes nothing -> empty-output error
        set_setting(&ctx.db, "handbrake_path", script.to_str().unwrap());
        set_setting(&ctx.db, "low_disk_min_gb", "0");
        let out = dir.path().join("out.mp4");
        // i64::MAX source: a wrongly-running gate would saturate the requirement to ~u64::MAX and
        // trip (job stays 'queued'), so reaching the encode/error outcome proves the gate was
        // genuinely skipped, not that it ran and happened to pass on a tiny source.
        let src = real_source(dir.path(), "a.mp4");
        queue_job(
            &ctx.db,
            "j1",
            src.to_str().unwrap(),
            out.to_str().unwrap(),
            i64::MAX,
        );

        process_queue(&ctx);

        let (status, msg) = job_row(&ctx.db, "j1");
        assert_eq!(
            status, "error",
            "with the check disabled, the job is processed, not held 'queued'"
        );
        assert!(
            msg.unwrap().contains("empty output file"),
            "the job reached the encode stage (fake HandBrake produced no output)"
        );
    }

    // Unix-only: this exercises fail-open when statvfs ERRORS for a missing path. On Windows,
    // GetDiskFreeSpaceEx resolves a missing subdir to the volume root and returns Some, so the
    // job would (correctly) be gated by the absurd threshold instead. The universal fail-open
    // path (a None parent) is covered by `destination_available_bytes_resolves_or_fails_open`.
    #[cfg(unix)]
    #[test]
    fn low_disk_check_fails_open_when_destination_is_unstattable() {
        // A configured threshold must NOT block a job whose destination free space can't be
        // read (nonexistent parent dir -> fs4::available_space errs -> None). Fail open: the
        // encode proceeds rather than the queue wedging on an unreadable volume.
        let (ctx, sink, _disposer) = test_ctx(test_conn());

        let dir = tempfile::tempdir().unwrap();
        let script = fake_handbrake_script(dir.path()); // exits 0, writes nothing -> empty-output error
        set_setting(&ctx.db, "handbrake_path", script.to_str().unwrap());
        // Absurd threshold: ANY real stat result would trip the gate (job stays 'queued'), so
        // reaching the encode/error outcome below proves a genuine None (fail open), not that
        // some other disk's ample free space slipped through.
        set_setting(&ctx.db, "low_disk_min_gb", "1000000000"); // 1e9 GB
                                                               // Output under a parent directory that does not exist -> available-space query fails -> None.
        let src = real_source(dir.path(), "a.mp4");
        queue_job(
            &ctx.db,
            "j1",
            src.to_str().unwrap(),
            "/no-such-dir-xyz/out.mp4",
            1000,
        );

        process_queue(&ctx);

        let (status, msg) = job_row(&ctx.db, "j1");
        assert_eq!(
            status, "error",
            "fail-open: the job runs to the encode stage, not held 'queued'"
        );
        assert!(
            msg.unwrap().contains("empty output file"),
            "the disabled/unreadable-disk gate let the encode proceed"
        );
        assert!(
            sink.payloads("queue-paused-low-disk").is_empty(),
            "no low-disk pause event fires when free space is unknown"
        );
    }

    #[test]
    fn low_disk_paused_job_is_resumable_after_the_threshold_is_lowered() {
        // Paused-for-disk jobs stay 'queued'; lowering the threshold (freeing space) and re-running
        // the queue must let them proceed — the "free up space, then Resume" contract.
        let (ctx, _sink, _disposer) = test_ctx(test_conn());
        let dir = tempfile::tempdir().unwrap();
        let script = fake_handbrake_script(dir.path());
        set_setting(&ctx.db, "handbrake_path", script.to_str().unwrap());
        set_setting(&ctx.db, "low_disk_min_gb", "1000000000");
        let out = dir.path().join("out.mp4");
        let src = real_source(dir.path(), "a.mp4");
        queue_job(
            &ctx.db,
            "j1",
            src.to_str().unwrap(),
            out.to_str().unwrap(),
            1000,
        );

        process_queue(&ctx);
        assert_eq!(job_row(&ctx.db, "j1").0, "queued", "gate pauses the job");
        let pause = ctx
            .converter
            .low_disk_pause()
            .expect("the pause reason is persisted in backend state, not just emitted");
        assert_eq!(
            pause.path,
            out.to_str().unwrap(),
            "the persisted reason names the job's output path"
        );
        assert!(
            pause.available_bytes < pause.required_bytes,
            "the persisted reason reflects the shortfall"
        );

        // Free space (disable the gate) and run again: the job must now reach the encode stage.
        set_setting(&ctx.db, "low_disk_min_gb", "0");
        process_queue(&ctx);
        assert_eq!(
            job_row(&ctx.db, "j1").0,
            "error",
            "after resume the job runs (fake HB -> empty output)"
        );
        assert!(
            ctx.converter.low_disk_pause().is_none(),
            "a resumed run that gets past the gate must clear the stale pause reason"
        );
    }

    // A stand-in for HandBrakeCLI that logs to stderr, writes no output, and exits 0 —
    // the "successful" run that produces nothing, which D3 defines as a failure.
    fn fake_handbrake_script(dir: &std::path::Path) -> std::path::PathBuf {
        #[cfg(windows)]
        {
            let p = dir.join("hb.cmd");
            std::fs::write(&p, "@echo boom 1>&2\r\n@exit /b 0\r\n").unwrap();
            p
        }
        #[cfg(not(windows))]
        {
            use std::os::unix::fs::PermissionsExt;
            let p = dir.join("hb.sh");
            std::fs::write(&p, "#!/bin/sh\necho boom >&2\nexit 0\n").unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
            p
        }
    }

    // A stand-in for HandBrakeCLI failing to scan the input: exits 2 (HandBrake's real
    // "bad input" code) with the exact diagnostic it emits when it can't parse a file —
    // "No title found." — a SOURCE_MARKERS hit in failure_class.rs. Writes no output, so
    // process_queue takes the generic failure arm (not the empty-output success guard).
    fn bad_source_fake_handbrake_script(dir: &std::path::Path) -> std::path::PathBuf {
        #[cfg(windows)]
        {
            let p = dir.join("hb-badsource.cmd");
            // Redirect BEFORE `echo` (see truncating_fake_handbrake_script's comment below):
            // cmd.exe strips the `1>&2` token but keeps the space in front of it, so
            // `echo No title found. 1>&2` would emit a trailing space after "found.".
            std::fs::write(
                &p,
                "@echo off\r\n>&2 echo No title found.\r\n@exit /b 2\r\n",
            )
            .unwrap();
            p
        }
        #[cfg(not(windows))]
        {
            use std::os::unix::fs::PermissionsExt;
            let p = dir.join("hb-badsource.sh");
            std::fs::write(&p, "#!/bin/sh\necho \"No title found.\" >&2\nexit 2\n").unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
            p
        }
    }

    // A stand-in for a long-running encode: writes a partial output file (last CLI
    // arg, like HandBrakeCLI's -o), then blocks long past the test's timeouts.
    fn slow_fake_handbrake_script(dir: &std::path::Path) -> std::path::PathBuf {
        #[cfg(windows)]
        {
            let p = dir.join("hb-slow.cmd");
            // Block with a cmd-INTERNAL busy loop, not `ping`/`timeout`: a grandchild
            // would inherit our stdout/stderr pipe handles and keep them open after
            // the kill, blocking the queue thread's progress-drain join for ~30s
            // (real HandBrakeCLI spawns no grandchildren, so only this fake cares).
            std::fs::write(
                &p,
                "@echo off\r\n:loop\r\nif not \"%~2\"==\"\" (\r\nshift\r\ngoto loop\r\n)\r\necho partial> \"%~1\"\r\nfor /l %%i in (1,1,2000000000) do rem\r\n",
            )
            .unwrap();
            p
        }
        #[cfg(not(windows))]
        {
            use std::os::unix::fs::PermissionsExt;
            let p = dir.join("hb-slow.sh");
            std::fs::write(
                &p,
                "#!/bin/sh\nfor a; do out=\"$a\"; done\necho partial > \"$out\"\nexec sleep 30\n",
            )
            .unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
            p
        }
    }

    fn wait_until(what: &str, mut cond: impl FnMut() -> bool) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !cond() {
            assert!(
                std::time::Instant::now() < deadline,
                "timed out waiting for: {what}"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }

    #[test]
    fn quit_mid_encode_leaves_the_job_for_auto_resume() {
        // Quitting kills the active child (ExitRequested → kill_active_child). The
        // queue thread then observes the killed status — its error arm must NOT run:
        // deleting the partial, writing status='error' (which auto-resume ignores),
        // or firing a "failed" notification would make the job's fate depend on
        // whether the 100ms poll wakes before teardown finishes. The row stays
        // 'encoding' so the next launch resumes it.
        let (ctx, sink, _disposer) = test_ctx(test_conn());

        let dir = tempfile::tempdir().unwrap();
        let script = slow_fake_handbrake_script(dir.path());
        set_setting(&ctx.db, "handbrake_path", script.to_str().unwrap());
        let output = dir.path().join("out.mp4");
        let src = real_source(dir.path(), "a.mp4");
        queue_job(
            &ctx.db,
            "j1",
            src.to_str().unwrap(),
            output.to_str().unwrap(),
            1000,
        );

        run_queue(ctx.clone());
        // The partial must exist BEFORE the kill, or the exists() assertion below
        // would pass vacuously against a file the script never got to write.
        wait_until(
            "the fake encode to be running with a partial on disk",
            || {
                job_row(&ctx.db, "j1").0 == "encoding"
                    && ctx.converter.current_child.lock().unwrap().is_some()
                    && output.exists()
            },
        );

        kill_active_child(&ctx.converter);
        wait_until("the queue thread to exit", || {
            !*ctx.converter.is_running.lock().unwrap()
        });

        let (status, msg) = job_row(&ctx.db, "j1");
        assert_eq!(
            status, "encoding",
            "quit must leave the job for auto-resume, not record it as failed (msg: {msg:?})"
        );
        assert!(
            output.exists(),
            "the partial output belongs to next-launch auto-resume cleanup, not the quit path"
        );
        assert!(
            sink.payloads("job-error").is_empty(),
            "no failure events/notifications may fire because the user quit: {:?}",
            sink.payloads("job-error")
        );
    }

    #[test]
    fn zero_byte_output_fails_with_diagnostics_and_the_queue_continues() {
        // Exit 0 with an empty/missing output must record an error carrying the
        // stderr tail (not a bare fixed string), and must not abort the run — the
        // next queued job still gets processed.
        let (ctx, sink, _disposer) = test_ctx(test_conn());

        let dir = tempfile::tempdir().unwrap();
        let script = fake_handbrake_script(dir.path());
        set_setting(&ctx.db, "handbrake_path", script.to_str().unwrap());
        let out1 = dir.path().join("out1.mp4");
        let out2 = dir.path().join("out2.mp4");
        let src1 = real_source(dir.path(), "a.mp4");
        let src2 = real_source(dir.path(), "b.mp4");
        queue_job(
            &ctx.db,
            "j1",
            src1.to_str().unwrap(),
            out1.to_str().unwrap(),
            1000,
        );
        queue_job(
            &ctx.db,
            "j2",
            src2.to_str().unwrap(),
            out2.to_str().unwrap(),
            1000,
        );

        process_queue(&ctx);

        for id in ["j1", "j2"] {
            let (status, msg) = job_row(&ctx.db, id);
            assert_eq!(status, "error", "{id} must fail, exit 0 notwithstanding");
            let msg = msg.unwrap();
            assert!(
                msg.starts_with("Conversion produced an empty output file"),
                "{id}: {msg}"
            );
            assert!(
                msg.contains("boom"),
                "{id} must keep the stderr diagnostic, got: {msg}"
            );
            // Exit 0 with an undiagnostic stderr ("boom", no source/environment marker)
            // must classify as Unknown — never BadSource. Pins the empty-output call site
            // against a regression that hardcodes/misreasons a class here instead of
            // consulting classify().
            assert_eq!(
                class_of(&ctx.db, id).as_deref(),
                Some("unknown"),
                "{id}: an undiagnosable empty output must never be classified bad_source"
            );
        }
        assert!(
            !out1.exists() && !out2.exists(),
            "empty outputs are removed"
        );
        let final_update = sink.payloads("menu-bar-update").last().cloned().unwrap();
        assert!(final_update["status"] == "error");
    }

    /// A stand-in for HandBrakeCLI that writes a small non-empty output, emits a stderr tail
    /// claiming a large frame shortfall, and exits 0 — exactly what a truncated source
    /// produces in reality.
    fn truncating_fake_handbrake_script(dir: &std::path::Path) -> std::path::PathBuf {
        #[cfg(windows)]
        {
            let p = dir.join("hb-trunc.cmd");
            std::fs::write(
                &p,
                // The redirect must come BEFORE `echo`: cmd.exe strips the `1>&2` token but
                // keeps the space in front of it, so `echo ... expected 1>&2` would emit a
                // trailing space after "expected" and break the `strip_suffix(" expected")`
                // parse in parse_sync_line.
                "@echo off\r\n\
                 >&2 echo [00:00:01] sync: got 131 frames, 480 expected\r\n\
                 :loop\r\n\
                 if not \"%~2\"==\"\" (\r\n\
                 shift\r\n\
                 goto loop\r\n\
                 )\r\n\
                 echo data> \"%~1\"\r\n\
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
                 for a; do out=\"$a\"; done\n\
                 printf data > \"$out\"\n\
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
        let (ctx, sink, _disposer) = test_ctx(test_conn());

        let dir = tempfile::tempdir().unwrap();
        let script = truncating_fake_handbrake_script(dir.path());
        set_setting(&ctx.db, "handbrake_path", script.to_str().unwrap());
        set_setting(&ctx.db, "cleanup_mode", "trash");
        let source = real_source(dir.path(), "movie.mkv");
        let out = dir.path().join("movie.mp4");
        queue_job(
            &ctx.db,
            "j1",
            source.to_str().unwrap(),
            out.to_str().unwrap(),
            1000,
        );

        process_queue(&ctx);

        let (status, msg) = job_row(&ctx.db, "j1");
        assert_eq!(
            status, "error",
            "a truncated source is a failure, not a success"
        );
        assert!(
            msg.unwrap_or_default().contains("Source appears truncated"),
            "history must say WHY, not just that it failed"
        );
        assert_eq!(
            class_of(&ctx.db, "j1").as_deref(),
            Some("bad_source_truncated")
        );
        assert!(
            source.exists(),
            "THE POINT OF THIS FEATURE: the user's original must still be on disk"
        );
        assert!(!out.exists(), "the short partial output must be removed");
        let final_update = sink.payloads("menu-bar-update").last().cloned().unwrap();
        assert!(
            final_update["status"] == "error",
            "a run whose only job was truncated must end 'error', not a clean 'idle' that \
             hides the rejection — got: {final_update}"
        );
    }

    // For an in-place job output_path IS the source. Removing output_path here would delete
    // the original outright — the same defect class as the fixed auto-resume bug.
    #[test]
    fn truncated_in_place_encode_leaves_the_source_byte_identical() {
        let (ctx, _sink, _disposer) = test_ctx(test_conn());

        let dir = tempfile::tempdir().unwrap();
        let script = truncating_fake_handbrake_script(dir.path());
        set_setting(&ctx.db, "handbrake_path", script.to_str().unwrap());
        set_setting(&ctx.db, "cleanup_mode", "trash");
        let source = real_source(dir.path(), "movie.mkv");
        let original_bytes = std::fs::read(&source).unwrap();
        // in-place: output_path == source_path
        let p = source.to_str().unwrap();
        queue_job(&ctx.db, "j1", p, p, 1000);

        process_queue(&ctx);

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
        // extra audio track appends a mux: line AFTER the marker, eating into that headroom.
        // Simulate a worst-case multi-track trailer behind a flood of preceding noise (so the
        // window's front edge actually gets exercised) and confirm the marker still survives —
        // shrinking STDERR_TAIL_BYTES could silently disable truncation detection on
        // multi-track files without this catching it.
        let body = format!(
            "{}[00:00:01] sync: got 131 frames, 480 expected\n{}",
            "noise\n".repeat(2000),
            "mux: track 2, 100 frames\n".repeat(20),
        );
        let tail = read_bounded_tail(std::io::Cursor::new(body.into_bytes()));
        assert_eq!(
            crate::failure_class::decode_shortfall(&tail),
            Some((131, 480)),
            "a realistic multi-track trailer after the marker must not push it out of the \
             STDERR_TAIL_BYTES window"
        );
    }

    #[test]
    fn a_scan_failure_on_a_readable_source_is_classified_bad_source() {
        // The only path that can produce failure_class = 'bad_source' end-to-end: exit 2
        // (HandBrake's bad-input code) plus a source-marker diagnostic, against a source
        // we ourselves can open. This pins the failure arm's `let exit_code = match &other
        // {...}` wiring (converter.rs) — if that were ever collapsed to `None`, classify's
        // rule 4 (`exit_code == Some(2)`) could never fire and every such job would
        // silently fall through to Unknown with a still-green suite.
        let (ctx, _sink, _disposer) = test_ctx(test_conn());

        let dir = tempfile::tempdir().unwrap();
        let script = bad_source_fake_handbrake_script(dir.path());
        set_setting(&ctx.db, "handbrake_path", script.to_str().unwrap());
        let out = dir.path().join("out.mp4");
        let src = real_source(dir.path(), "a.mp4"); // genuinely exists and is readable
        queue_job(
            &ctx.db,
            "j1",
            src.to_str().unwrap(),
            out.to_str().unwrap(),
            1000,
        );

        process_queue(&ctx);

        let (status, msg) = job_row(&ctx.db, "j1");
        assert_eq!(status, "error");
        assert!(msg.unwrap().contains("No title found"));
        assert_eq!(
            class_of(&ctx.db, "j1").as_deref(),
            Some("bad_source"),
            "exit 2 + a source-marker diagnostic against a readable source must be bad_source"
        );
    }

    // F6: the sibling of the test above with the SAME script (so the diagnostic and exit code
    // are byte-identical), but a source we ourselves cannot open. Pins the wiring of
    // `source_readable: source_is_readable(&job.source_path)` at process_queue's call sites —
    // mutation testing showed hardcoding `source_readable: true` at BOTH call sites survives the
    // whole suite, because only the pure `classify()` was pinned and the observation feeding it
    // was never exercised end-to-end.
    #[cfg(unix)]
    #[test]
    fn a_scan_failure_on_a_source_we_cannot_read_is_classified_environment_not_bad_source() {
        // Root bypasses mode bits entirely, so this assertion is meaningless as uid 0 (rootful
        // docker / `act`). GitHub's ubuntu runner is non-root, so PR CI runs it.
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        let (ctx, _sink, _disposer) = test_ctx(test_conn());

        let dir = tempfile::tempdir().unwrap();
        let script = bad_source_fake_handbrake_script(dir.path());
        set_setting(&ctx.db, "handbrake_path", script.to_str().unwrap());
        let out = dir.path().join("out.mp4");
        let src = real_source(dir.path(), "a.mp4");
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&src, std::fs::Permissions::from_mode(0o000)).unwrap();
        queue_job(
            &ctx.db,
            "j1",
            src.to_str().unwrap(),
            out.to_str().unwrap(),
            1000,
        );

        process_queue(&ctx);

        // Restore unconditionally so the tempdir's Drop cleanup can remove it.
        let _ = std::fs::set_permissions(&src, std::fs::Permissions::from_mode(0o644));

        let (status, msg) = job_row(&ctx.db, "j1");
        assert_eq!(status, "error");
        assert!(msg.unwrap().contains("No title found"));
        assert_eq!(
            class_of(&ctx.db, "j1").as_deref(),
            Some("environment"),
            "we could not open the source ourselves, so HandBrake's IDENTICAL exit-2 \
             diagnostic must never be credited as a verdict on the file"
        );
    }

    fn source_size_mtime(db: &Arc<Mutex<Connection>>, id: &str) -> (Option<i64>, Option<i64>) {
        db.lock()
            .unwrap()
            .query_row(
                "SELECT source_size, source_mtime FROM jobs WHERE id = ?1",
                params![id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap()
    }

    // F2 regression: the fingerprint recorded at ADD time describes the file as it was WHEN
    // QUEUED, not as it is when condemned. Without re-stamping at condemnation time, a healthy
    // file queued with fingerprint (S,M) that is later truncated in place before its turn is
    // condemned while the DB still says (S,M) — and a sync tool that repairs it back to exactly
    // (S,M) (rsync -t / Syncthing / wget -N all preserve mtime) would make purge see a "match"
    // and destroy the REPAIRED file.
    #[test]
    fn bad_source_condemnation_restamps_identity_to_the_current_file_not_the_stale_add_time_value()
    {
        let (ctx, _sink, _disposer) = test_ctx(test_conn());

        let dir = tempfile::tempdir().unwrap();
        let script = bad_source_fake_handbrake_script(dir.path());
        set_setting(&ctx.db, "handbrake_path", script.to_str().unwrap());
        let out = dir.path().join("out.mp4");
        let src = real_source(dir.path(), "a.mp4");
        queue_job(
            &ctx.db,
            "j1",
            src.to_str().unwrap(),
            out.to_str().unwrap(),
            1000,
        );
        // Simulate a fingerprint captured when a DIFFERENT (larger, healthy) file lived at this
        // path at queue time.
        ctx.db
            .lock()
            .unwrap()
            .execute(
                "UPDATE jobs SET source_size = 999999, source_mtime = 1 WHERE id = 'j1'",
                [],
            )
            .unwrap();

        process_queue(&ctx);

        assert_eq!(class_of(&ctx.db, "j1").as_deref(), Some("bad_source"));
        let current = crate::probe_cache::file_identity(src.to_str().unwrap())
            .expect("source is still on disk");
        assert_eq!(
            source_size_mtime(&ctx.db, "j1"),
            (Some(current.size), Some(current.mtime)),
            "the fingerprint must be re-stamped to the CURRENT file at condemnation time, not \
             left at the stale add-time value — otherwise a later repair landing back at the \
             stale (size, mtime) would look like a match to purge"
        );
    }

    // Sibling of the test above for the truncation guard's direct (non-classify()) condemnation
    // site — the same stale-fingerprint window applies there too.
    #[test]
    fn truncated_condemnation_restamps_identity_to_the_current_file_not_the_stale_add_time_value() {
        let (ctx, _sink, _disposer) = test_ctx(test_conn());

        let dir = tempfile::tempdir().unwrap();
        let script = truncating_fake_handbrake_script(dir.path());
        set_setting(&ctx.db, "handbrake_path", script.to_str().unwrap());
        let source = real_source(dir.path(), "movie.mkv");
        let out = dir.path().join("movie.mp4");
        queue_job(
            &ctx.db,
            "j1",
            source.to_str().unwrap(),
            out.to_str().unwrap(),
            1000,
        );
        ctx.db
            .lock()
            .unwrap()
            .execute(
                "UPDATE jobs SET source_size = 999999, source_mtime = 1 WHERE id = 'j1'",
                [],
            )
            .unwrap();

        process_queue(&ctx);

        assert_eq!(
            class_of(&ctx.db, "j1").as_deref(),
            Some("bad_source_truncated")
        );
        let current = crate::probe_cache::file_identity(source.to_str().unwrap())
            .expect("source is still on disk");
        assert_eq!(
            source_size_mtime(&ctx.db, "j1"),
            (Some(current.size), Some(current.mtime)),
            "the truncation guard must also re-stamp the fingerprint at condemnation time"
        );
    }

    // End-to-end (local/e2e-ignored CI only): needs ffmpeg to synthesize a clip and a
    // real HandBrakeCLI on PATH. The only test driving process_queue's full DB state
    // machine queued → encoding → done with a real encode. Run with:
    //   cargo test -- --ignored process_queue_drives_a_real_encode
    #[test]
    #[ignore]
    fn process_queue_drives_a_real_encode_from_queued_to_done() {
        // PathLocator, not the fixture default: this test never pins `handbrake_path` and
        // genuinely wants the host's real HandBrakeCLI to perform the encode.
        let (ctx, sink, _disposer) =
            test_ctx_with_locator(test_conn(), Arc::new(crate::handbrake::PathLocator));
        // 'delete' keeps the cleanup assertion filesystem-local (no Trash involved).
        set_setting(&ctx.db, "cleanup_mode", "delete");

        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("src.mp4");
        // Lossless H.264 so the source is large enough that the H.265 re-encode wins,
        // making the terminal state deterministically 'done' (not 'skipped').
        let ok = Command::new("ffmpeg")
            .args([
                "-y",
                "-f",
                "lavfi",
                "-i",
                "testsrc=duration=1:size=1280x720:rate=25",
                "-pix_fmt",
                "yuv420p",
                "-c:v",
                "libx264",
                "-qp",
                "0",
            ])
            .arg(&source)
            .status()
            .expect("run ffmpeg")
            .success();
        assert!(ok, "ffmpeg failed to synthesize the source clip");
        let original_size = std::fs::metadata(&source).map(|m| m.len() as i64).unwrap();

        let output = dir.path().join("out.mkv");
        queue_job(
            &ctx.db,
            "j1",
            source.to_str().unwrap(),
            output.to_str().unwrap(),
            original_size,
        );

        process_queue(&ctx);

        // The UI saw the full transition, not just the terminal state.
        let events = sink.payloads("job-status-changed");
        assert!(
            events.iter().any(|p| p["status"] == "encoding"),
            "missing encoding transition: {events:?}"
        );
        assert!(
            events.last().unwrap()["status"] == "done",
            "missing done transition: {events:?}"
        );

        let (status, completed_at, converted_size, kept_file, space_saved) = ctx
            .db
            .lock()
            .unwrap()
            .query_row(
                "SELECT status, completed_at, converted_size, kept_file, space_saved
                 FROM jobs WHERE id = 'j1'",
                [],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, Option<String>>(1)?,
                        r.get::<_, Option<i64>>(2)?,
                        r.get::<_, Option<String>>(3)?,
                        r.get::<_, Option<i64>>(4)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(status, "done");
        assert!(completed_at.is_some(), "done must set completed_at");
        let converted_size = converted_size.unwrap();
        assert!(converted_size > 0 && converted_size < original_size);
        assert_eq!(kept_file.as_deref(), Some("converted"));
        assert_eq!(space_saved, Some(original_size - converted_size));
        // Cleanup ran: converted kept on disk, original deleted (mode 'delete').
        assert!(output.exists());
        assert!(!source.exists());
    }

    // End-to-end (local/e2e-ignored CI only): the tripwire for the ENTIRE truncation guard.
    // Every other truncation test (truncated_encode_errors_and_leaves_the_source_on_disk and
    // friends) drives a FAKE stand-in script that echoes the exact
    // `sync: got N frames, M expected` string this guard parses — a string ConvertBar does not
    // control. If a future HandBrake release ever reformats that line, the guard silently stops
    // firing, the ORIGINAL data-loss bug (a truncated download recorded as 'done' and the
    // source trashed) returns, and every one of those fake-script tests stays green regardless,
    // because none of them ever touch real HandBrake output. This is the only test that can
    // catch that. Needs ffmpeg to synthesize a clip and a real HandBrakeCLI on PATH. Run with:
    //   cargo test -p convertbar-core --lib -- --ignored real_handbrake_flags_a_truncated_source
    #[test]
    #[ignore]
    fn real_handbrake_flags_a_truncated_source_and_spares_the_original() {
        let (ctx, _sink, _disposer) = test_ctx(test_conn());
        // No locator declaration needed here: `handbrake_path` is set below to a real, existing
        // path BEFORE any resolution runs, so `resolve_with_locator` short-circuits on the
        // configured branch and the fixture's `PanickingLocator` is never consulted.
        let handbrake_path =
            crate::handbrake::detect_handbrake_path().expect("HandBrakeCLI must be on PATH");
        set_setting(&ctx.db, "handbrake_path", &handbrake_path);
        // 'delete' keeps the survival assertion filesystem-local (no Trash involved).
        set_setting(&ctx.db, "cleanup_mode", "delete");

        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("src.mp4");
        // +faststart is essential: without it, the moov atom is written at the END of the file
        // by default, so truncating below destroys it outright and HandBrake can't open the
        // file AT ALL (a scan failure / bad_source) — a different failure mode than the one
        // this guard exists to catch.
        // 20s is deliberate, not arbitrary: HandBrake's scan samples preview frames spread
        // across the container's FULL declared duration (faststart keeps that duration
        // metadata intact even after truncation). A too-short clip puts every preview past
        // the truncation point, so the SCAN itself fails to find a title (a different failure
        // mode, bad_source) instead of the scan succeeding and the WORK/decode phase running
        // out of data partway through — which is what this guard exists to catch.
        let ok = Command::new("ffmpeg")
            .args([
                "-y",
                "-f",
                "lavfi",
                "-i",
                "testsrc=duration=20:size=1280x720:rate=25",
                "-pix_fmt",
                "yuv420p",
                "-c:v",
                "libx264",
                "-movflags",
                "+faststart",
            ])
            .arg(&source)
            .status()
            .expect("run ffmpeg")
            .success();
        assert!(ok, "ffmpeg failed to synthesize the source clip");

        let full_bytes = std::fs::read(&source).unwrap();
        let truncated_len = (full_bytes.len() as f64 * 0.35) as u64;
        std::fs::OpenOptions::new()
            .write(true)
            .open(&source)
            .unwrap()
            .set_len(truncated_len)
            .unwrap();
        let truncated_bytes = std::fs::read(&source).unwrap();

        // Independently run the REAL HandBrakeCLI ourselves (same args process_queue will use
        // below) to capture its raw stderr on the truncated source. This is a second, separate
        // encode from the one process_queue performs — deliberately: it decouples "does real
        // HandBrake still emit a parseable marker" from "did our code correctly react to it",
        // so a HandBrake output-format change is diagnosed precisely as that, rather than the
        // vaguer "the guard didn't fire".
        let preset: String = ctx
            .db
            .lock()
            .unwrap()
            .query_row("SELECT value FROM settings WHERE key = 'preset'", [], |r| {
                r.get(0)
            })
            .unwrap();
        let probe_output = dir.path().join("probe-out.mkv");
        let probe = Command::new(&handbrake_path)
            .arg("-Z")
            .arg(&preset)
            .arg("-O")
            .arg("-i")
            .arg(&source)
            .arg("-o")
            .arg(&probe_output)
            .output()
            .expect("run real HandBrakeCLI directly to capture its raw stderr");
        let raw_tail = String::from_utf8_lossy(&probe.stderr).to_string();
        let _ = std::fs::remove_file(&probe_output);
        assert!(
            crate::failure_class::decode_shortfall(&raw_tail).is_some(),
            "real HandBrakeCLI no longer emits a parseable 'sync: got N frames, M expected' \
             line on a truncated source — HandBrake changed its output format, this is NOT \
             \"the guard didn't fire\". Raw tail:\n{raw_tail}"
        );
        // The probe encode must not have disturbed the truncated source under test.
        assert_eq!(std::fs::read(&source).unwrap(), truncated_bytes);

        let output = dir.path().join("out.mkv");
        queue_job(
            &ctx.db,
            "j1",
            source.to_str().unwrap(),
            output.to_str().unwrap(),
            truncated_len as i64,
        );

        process_queue(&ctx);

        let (status, msg) = job_row(&ctx.db, "j1");
        assert_eq!(
            status, "error",
            "a truncated source must fail, not succeed: {msg:?}"
        );
        assert_eq!(
            class_of(&ctx.db, "j1").as_deref(),
            Some("bad_source_truncated"),
            "real HandBrake output must still trip the decode-shortfall guard"
        );
        assert_eq!(
            std::fs::read(&source).unwrap(),
            truncated_bytes,
            "THE POINT OF THIS FEATURE: the truncated source must survive byte-identical, not \
             be replaced or trashed"
        );
        assert!(!output.exists(), "the partial output must be removed");
    }

    #[test]
    fn take_pause_after_current_consumes_the_flag_exactly_once() {
        let state = ConverterState::new();
        // Not armed: no pause, the flag stays clear.
        assert!(!take_pause_after_current(&state));

        // Armed: the queue pauses after the job that just finished, and the flag is cleared so
        // the NEXT job does not also pause — the "Pause after this" button is a one-shot.
        *state.pause_after_current.lock().unwrap() = true;
        assert!(take_pause_after_current(&state));
        assert!(
            !take_pause_after_current(&state),
            "pause-after-current must fire once, then re-arm to off"
        );
    }

    #[test]
    fn final_run_status_is_error_only_when_a_job_failed() {
        // The end-of-queue menu-bar transition: a clean run returns to idle; any failure leaves
        // the tray showing an error state until the next run starts.
        assert_eq!(final_run_status(false), "idle");
        assert_eq!(final_run_status(true), "error");
    }

    #[test]
    fn required_free_bytes_adds_double_the_source_to_the_floor() {
        // Peak disk during an in-place encode is source + temp ≈ 2× source, on top of the floor.
        assert_eq!(required_free_bytes(1000, 500), 1000 + 1000);
        // Unknown/zero source size degrades to the bare floor.
        assert_eq!(required_free_bytes(1000, 0), 1000);
    }

    #[test]
    fn required_free_bytes_saturates_instead_of_wrapping() {
        assert_eq!(required_free_bytes(u64::MAX, 10), u64::MAX);
        assert_eq!(required_free_bytes(10, u64::MAX), u64::MAX);
    }

    #[test]
    fn has_enough_disk_is_true_only_at_or_above_the_requirement() {
        // floor 1000 + 2*500 = 2000 required.
        assert!(has_enough_disk(2000, 1000, 500));
        assert!(has_enough_disk(2001, 1000, 500));
        assert!(!has_enough_disk(1999, 1000, 500));
    }

    #[test]
    fn claim_job_only_claims_a_queued_row() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::init_db(&conn).unwrap();
        conn.execute(
            "INSERT INTO jobs (id, source_path, output_path, preset, status, queue_order, created_at)
         VALUES ('q', '/s.mp4', '/o.mp4', 'p', 'queued', 0, '2020-01-01T00:00:00Z'),
                ('d', '/s2.mp4', '/o2.mp4', 'p', 'done', 1, '2020-01-01T00:00:00Z')",
            [],
        ).unwrap();
        // A queued row is claimed and flipped to encoding.
        assert_eq!(claim_job(&conn, "q"), ClaimOutcome::Claimed);
        let s: String = conn
            .query_row("SELECT status FROM jobs WHERE id='q'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(s, "encoding");
        // Re-claiming the now-encoding row reports Gone and does not change it.
        assert_eq!(claim_job(&conn, "q"), ClaimOutcome::Gone);
        // A non-queued (done) row is never claimed and is left untouched.
        assert_eq!(claim_job(&conn, "d"), ClaimOutcome::Gone);
        let sd: String = conn
            .query_row("SELECT status FROM jobs WHERE id='d'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(sd, "done");
        // A missing id is Gone.
        assert_eq!(claim_job(&conn, "nope"), ClaimOutcome::Gone);
    }

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

    #[test]
    fn the_claim_stamp_is_the_moment_of_the_claim_in_parseable_rfc3339() {
        // `is_some()` alone would pass against a hardcoded constant, and against a garbage
        // string: every Rust test would stay green while the frontend's Date.parse returns
        // NaN and the feature silently renders nothing. Pin both the value and the format.
        let (ctx, _sink, _disposer) = test_ctx(test_conn());
        queue_job(&ctx.db, "j1", "/src/a.mkv", "/out/a.mp4", 1000);

        let before = chrono::Utc::now();
        claim_job(&ctx.db.lock().unwrap(), "j1");
        let after = chrono::Utc::now();

        let stamped = started_at_of(&ctx.db, "j1").expect("the claim stamps");
        let parsed = chrono::DateTime::parse_from_rfc3339(&stamped)
            .expect("the frontend parses this string with Date.parse");
        assert!(
            parsed >= before && parsed <= after,
            "the duration anchor must be the claim moment, not a constant: {stamped}"
        );
    }

    #[test]
    fn gb_to_bytes_converts_a_fractional_threshold() {
        // A non-integer configured floor (e.g. 2.5 GB) must scale exactly, not truncate the GiB
        // before multiplying.
        assert_eq!(gb_to_bytes(2.5), (2.5 * 1024.0 * 1024.0 * 1024.0) as u64);
    }

    #[test]
    fn destination_available_bytes_resolves_or_fails_open() {
        // A real, existing parent directory yields a concrete free-space figure.
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out.mp4");
        assert!(
            destination_available_bytes(out.to_str().unwrap()).is_some(),
            "an output inside a real directory has a stat-able free-space figure"
        );
        // The fail-open-to-None cases below rely on POSIX statvfs erroring for an unreadable
        // location; Windows' GetDiskFreeSpaceEx resolves a missing subdir to the volume root and
        // returns Some, so these are Unix-only.
        #[cfg(unix)]
        {
            // A nonexistent parent can't be stat'd -> None (fail open, don't wedge the queue).
            assert_eq!(
                destination_available_bytes("/no-such-dir-xyz/out.mp4"),
                None
            );
            // A bare filename's parent is "" (not None), and statting "" fails -> None.
            assert_eq!(destination_available_bytes("out.mp4"), None);
        }
    }

    #[test]
    fn gb_to_bytes_converts_and_clamps() {
        assert_eq!(gb_to_bytes(1.0), 1024 * 1024 * 1024);
        // Disabled / nonsense values clamp to 0 (never panic, never wrap).
        assert_eq!(gb_to_bytes(0.0), 0);
        assert_eq!(gb_to_bytes(-5.0), 0);
        // Absurd values saturate rather than panicking (used by the Task 4 integration test).
        assert_eq!(gb_to_bytes(f64::MAX), u64::MAX);
        // A stored "NaN"/"inf" (f64::parse accepts both) must resolve to a safe extreme, not a
        // panic. NaN reaches 0 via the float->int cast (NaN < 0.0 is false, so the `gb <= 0.0`
        // guard does NOT catch it) — pin both so a future refactor of the guard can't silently
        // break NaN handling.
        assert_eq!(gb_to_bytes(f64::NAN), 0);
        assert_eq!(gb_to_bytes(f64::INFINITY), u64::MAX);
    }

    #[test]
    fn is_pause_after_current_reflects_the_backend_flag() {
        // ActiveJob seeds its button state from this on mount instead of a local mirror
        // that desyncs across tab remounts / the updater flow arming the flag elsewhere.
        let state = ConverterState::new();
        assert!(!state.is_pause_after_current());
        *state.pause_after_current.lock().unwrap() = true;
        assert!(state.is_pause_after_current());
    }

    #[test]
    fn read_bounded_tail_keeps_only_the_end_of_a_flood() {
        // HandBrake can log megabytes to stderr; only the end explains a failure, and
        // the buffer must stay bounded so a chatty encode can't balloon memory.
        let flood = format!("{}THE-ACTUAL-ERROR", "noise\n".repeat(10_000));
        let tail = read_bounded_tail(std::io::Cursor::new(flood.into_bytes()));
        assert!(tail.len() <= STDERR_TAIL_BYTES);
        assert!(
            tail.ends_with("THE-ACTUAL-ERROR"),
            "the most recent output must survive the truncation"
        );
    }

    #[test]
    fn error_message_from_tail_falls_back_to_the_generic_message() {
        // No captured stderr (e.g. spawn raced the failure) must not produce an
        // empty-looking history entry.
        assert_eq!(error_message_from_tail(""), "Conversion failed");
        assert_eq!(error_message_from_tail("\n  \n"), "Conversion failed");
    }

    #[test]
    fn error_message_from_tail_keeps_the_last_informative_lines() {
        let tail = (1..=30)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n\n");
        let msg = error_message_from_tail(&tail);
        assert!(
            msg.starts_with("Conversion failed:\n"),
            "the generic prefix keeps existing UI copy meaningful"
        );
        assert!(
            !msg.contains("line 10\n") && msg.contains("line 11") && msg.ends_with("line 30"),
            "only the last {ERROR_TAIL_LINES} non-empty lines belong in the history entry"
        );
    }

    #[test]
    fn error_message_from_tail_leads_with_the_diagnostic_not_the_banner() {
        // Real HandBrake stderr opens with a benign build banner and host-info preamble;
        // the line that says WHY the encode failed sits several lines down. The history
        // entry is shown truncated to a single line in the UI, so its first line must be
        // the diagnostic — otherwise the user only ever sees "Compile-time hardening…".
        let tail = "\
[22:35:20] Compile-time hardening features are enabled
[22:35:20] hb_init: starting libhb thread
HandBrake 1.11.2 (2026060700) - Darwin arm64 - https://handbrake.fr
10 CPUs detected
Opening /movies/clip.mp4...
[mov,mp4,m4a,3gp,3g2,mj2 @ 0x0] moov atom not found
[22:35:21] scan: unrecognized file type
No title found.
HandBrake has exited.";
        let msg = error_message_from_tail(tail);
        let headline = msg.lines().next().unwrap();
        assert!(
            headline.contains("moov atom not found"),
            "the first line must surface the root-cause diagnostic, got: {headline:?}"
        );
        assert!(
            !headline.to_lowercase().contains("hardening"),
            "the benign build banner must never be the headline, got: {headline:?}"
        );
        // The full tail is still retained below the headline for detail.
        assert!(msg.contains("No title found."));
    }

    #[test]
    fn error_message_from_tail_surfaces_common_handbrake_failures() {
        // Each of these is a real HandBrakeCLI / libav / OS failure reason that can sit
        // below the build banner. The headline must be the reason, whatever form it takes.
        let cases = [
            (
                "[libavformat] Unsupported color space",
                "Unsupported color space",
            ),
            (
                "Error opening output: Read-only file system",
                "Read-only file system",
            ),
            (
                "Failed to create /out/x.mp4: Operation not permitted",
                "Operation not permitted",
            ),
            (
                "Opening /gone.mp4: No such file or directory",
                "No such file or directory",
            ),
            ("[22:00:00] sync: track failed, encode aborted", "aborted"),
            ("mp4 muxer: fatal: could not write header", "fatal"),
            ("[matroska] corrupt input near sample 42", "corrupt"),
        ];
        for (error_line, expected) in cases {
            let tail = format!(
                "[00:00:00] Compile-time hardening features are enabled\n\
                 HandBrake 1.11.2 - Darwin arm64\n\
                 10 CPUs detected\n\
                 {error_line}\n\
                 HandBrake has exited.",
            );
            let headline = error_message_from_tail(&tail)
                .lines()
                .next()
                .unwrap()
                .to_string();
            assert!(
                headline.contains(expected),
                "headline should surface {expected:?}, got: {headline:?}"
            );
            assert!(
                !headline.to_lowercase().contains("hardening"),
                "the build banner must never win, got: {headline:?}"
            );
        }
    }

    #[test]
    fn kill_active_child_arms_shutdown_so_the_queue_spawns_no_new_jobs() {
        // Killing the current child is not enough on quit: the queue thread's next
        // iteration would spawn a fresh HandBrakeCLI during teardown and orphan it.
        // The kill must arm the shutdown flag process_queue checks — even when no
        // child is currently running (the spawn→store window).
        let state = ConverterState::new();
        assert!(!state.is_shutting_down());
        kill_active_child(&state);
        assert!(state.is_shutting_down());
    }

    #[test]
    fn running_guard_resets_is_running_even_on_a_panic() {
        // A panic in process_queue must not wedge the queue by leaving is_running stuck true
        // (every future run_queue would early-return). The RAII guard clears it on unwind.
        use std::panic::{catch_unwind, AssertUnwindSafe};
        let converter = ConverterState::new();
        *converter.is_running.lock().unwrap() = true;

        // Suppress the expected panic's default stderr print so test output stays pristine.
        let prev_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let result = catch_unwind(AssertUnwindSafe(|| {
            let _running = RunningGuard(&converter);
            assert!(
                *converter.is_running.lock().unwrap(),
                "is_running stays true while the guard is alive"
            );
            panic!("simulated process_queue crash");
        }));
        std::panic::set_hook(prev_hook);

        assert!(result.is_err(), "the guarded closure did panic");
        assert!(
            !*converter.is_running.lock().unwrap(),
            "RunningGuard reset is_running while unwinding, so run_queue can start again"
        );
    }

    #[test]
    fn empty_output_error_message_carries_the_stderr_tail() {
        // The zero-byte guard fires while HandBrake's stderr says WHY the output was
        // empty (e.g. "No title found"); discarding it would undo B3's diagnostics.
        assert_eq!(
            empty_output_error_message(""),
            "Conversion produced an empty output file"
        );
        let msg = empty_output_error_message("scan: No title found\n");
        assert!(msg.starts_with("Conversion produced an empty output file:"));
        assert!(msg.contains("No title found"));
    }

    #[test]
    fn parses_full_progress_line() {
        let line = "Encoding: task 1 of 1, 42.50 % (123.45 fps, avg 120.00 fps, ETA 00h02m30s)";
        let (percent, fps, avg_fps, eta) = parse_progress(line).unwrap();
        assert_eq!(percent, 42.5);
        assert_eq!(fps, 123.45);
        assert_eq!(avg_fps, 120.0);
        assert_eq!(eta, 150); // 2m30s
    }

    #[test]
    fn falls_back_to_percent_only() {
        let line = "Encoding: task 1 of 1, 5.00 %";
        let (percent, fps, avg_fps, eta) = parse_progress(line).unwrap();
        assert_eq!(percent, 5.0);
        assert_eq!(fps, 0.0);
        assert_eq!(avg_fps, 0.0);
        assert_eq!(eta, 0);
    }

    #[test]
    fn ignores_non_encoding_lines() {
        assert!(parse_progress("Scanning title 1 of 1").is_none());
    }

    #[test]
    fn format_bytes_short_picks_units() {
        assert_eq!(format_bytes_short(0), "0B");
        assert_eq!(format_bytes_short(1024), "1KB");
        assert_eq!(format_bytes_short(1_048_576), "1MB");
        assert_eq!(format_bytes_short(1_073_741_824), "1.0GB");
    }

    #[test]
    fn record_source_identity_refreshes_fingerprint_to_the_current_file() {
        // After an in-place encode replaces the source, the recorded fingerprint must match what a
        // folder re-scan will stat (via queue::file_identity), so the encoded result is recognized.
        let conn = Connection::open_in_memory().unwrap();
        crate::db::init_db(&conn).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("clip.mp4");
        std::fs::write(&path, b"encoded-bytes").unwrap();
        let path_str = path.to_string_lossy().to_string();
        conn.execute(
            "INSERT INTO jobs (id, source_path, output_path, preset, status, queue_order, created_at)
             VALUES ('j1', ?1, ?1, 'preset', 'done', 0, '2020-01-01T00:00:00Z')",
            params![path_str],
        )
        .unwrap();

        record_source_identity(&conn, "j1", &path_str);

        let (size, mtime): (i64, i64) = conn
            .query_row(
                "SELECT source_size, source_mtime FROM jobs WHERE id = 'j1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        let expected = crate::probe_cache::file_identity(&path_str).unwrap();
        assert_eq!((size, mtime), (expected.size, expected.mtime));
    }

    // A long-running child process standing in for an active HandBrakeCLI encode. Kept
    // cross-platform (Windows has no `sleep`) so the cancel-deadlock regression below runs
    // everywhere — cancel via `Child::kill()` is a process-control path on ALL platforms.
    // It must outlive the test's timeouts so a premature exit can't mask the deadlock.
    fn spawn_long_running_child() -> Child {
        #[cfg(windows)]
        {
            // ~31 pings at ~1s spacing ≈ 30s; killable directly (ping.exe on PATH).
            Command::new("ping")
                .args(["-n", "31", "127.0.0.1"])
                .stdout(Stdio::null())
                .spawn()
                .unwrap()
        }
        #[cfg(not(windows))]
        {
            Command::new("sleep").arg("30").spawn().unwrap()
        }
    }

    // Regression test for the cancel-freeze deadlock: the queue thread must not hold
    // the `current_child` lock across the blocking wait, or `cancel_conversion` (which
    // needs that same lock to kill the child) blocks the main thread and freezes the UI.
    #[test]
    fn cancel_can_kill_child_while_wait_in_progress() {
        use std::sync::mpsc;
        use std::time::Duration;

        let converter = Arc::new(ConverterState::new());

        let child = spawn_long_running_child();
        *converter.current_child.lock().unwrap() = Some(child);

        // Thread A: the queue loop's wait.
        let waiter_rx = {
            let converter = converter.clone();
            let (tx, rx) = mpsc::channel();
            std::thread::spawn(move || {
                let _ = wait_for_active_child(&converter);
                let _ = tx.send(());
            });
            rx
        };

        // Let Thread A enter the wait before cancel races for the lock.
        std::thread::sleep(Duration::from_millis(200));

        // Thread B: emulate cancel_conversion acquiring the lock and killing the child.
        let killed_rx = {
            let converter = converter.clone();
            let (tx, rx) = mpsc::channel();
            std::thread::spawn(move || {
                if let Some(child) = converter.current_child.lock().unwrap().as_mut() {
                    let _ = child.kill();
                }
                let _ = tx.send(());
            });
            rx
        };

        // Cancel must acquire `current_child` and kill promptly. If the wait holds the
        // lock across the blocking wait, this lock never frees and recv_timeout fails.
        killed_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("cancel could not acquire current_child lock — wait holds it across the wait");

        // And the waiter must observe the killed process and return.
        waiter_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("wait did not return after the child was killed");
    }

    #[test]
    fn decide_cleanup_matrix() {
        // (original_size, converted_size) -> (kept file, space_saved, status).
        // This decision drives an irreversible delete, so every cell is pinned.
        let cases = [
            // Converted is smaller: keep converted, delete the original, count the win.
            (1000i64, 600i64, KeptFile::Converted, 400i64, "done"),
            // Converted is larger: keep original, delete the converted, flag skipped
            // (space_saved is the negative delta, matching the pre-refactor behavior).
            (1000, 1500, KeptFile::Original, -500, "skipped"),
            // Converted equals original: no win, keep original, skipped.
            (1000, 1000, KeptFile::Original, 0, "skipped"),
            // Zero output is UNREACHABLE at the call site: process_queue's zero-byte
            // guard records an error and never calls decide_cleanup (D3). These rows
            // pin only the function's own dead-arm contract (keep original, delete
            // nothing) — do NOT read "done" here as licence to treat 0 bytes as
            // success upstream.
            (1000, 0, KeptFile::Neither, 0, "done"),
            (0, 0, KeptFile::Neither, 0, "skipped"),
        ];
        for (orig, conv, want_kept, want_saved, want_status) in cases {
            let (kept, saved, status) = decide_cleanup(orig, conv);
            assert_eq!(kept, want_kept, "kept for ({orig}, {conv})");
            assert_eq!(saved, want_saved, "space_saved for ({orig}, {conv})");
            assert_eq!(status, want_status, "status for ({orig}, {conv})");
        }
    }

    #[test]
    fn is_in_place_only_when_paths_match() {
        assert!(is_in_place("/m/clip.mp4", "/m/clip.mp4"));
        assert!(!is_in_place("/m/clip.mkv", "/m/clip.mp4"));
        assert!(!is_in_place("/m/clip.mp4", "/m/clip-conv.mp4"));
    }

    #[test]
    fn is_in_place_matches_add_time_path_normalization() {
        // Regression: add-time uses `Path` equality (normalizes `//` and `/.`), but the stored
        // output_path is the normalized join while source_path is verbatim. is_in_place MUST treat
        // these as equal, or the converter routes an in-place job through the distinct-file delete
        // path and destroys the source.
        assert!(is_in_place("/movies//clip.mp4", "/movies/clip.mp4"));
        assert!(is_in_place("/movies/./clip.mp4", "/movies/clip.mp4"));
        // Genuinely different files must still be distinct.
        assert!(!is_in_place("/movies/clip.mp4", "/movies/other.mp4"));
    }

    #[test]
    fn in_place_temp_path_is_marked_hidden_sibling() {
        let temp = in_place_temp_path("/movies/clip.mp4");
        assert_eq!(
            temp,
            std::path::Path::new("/movies/.clip.convertbar-tmp.mp4")
        );
        assert!(temp.to_string_lossy().contains(IN_PLACE_TEMP_MARKER));
    }

    #[test]
    fn in_place_action_maps_decision_to_filesystem_op() {
        assert_eq!(
            in_place_action(KeptFile::Converted, "delete"),
            InPlaceAction::RenameTempOverSource
        );
        assert_eq!(
            in_place_action(KeptFile::Converted, "trash"),
            InPlaceAction::TrashSourceThenRename
        );
        assert_eq!(
            in_place_action(KeptFile::Original, "delete"),
            InPlaceAction::RemoveTemp
        );
        assert_eq!(
            in_place_action(KeptFile::Neither, "trash"),
            InPlaceAction::RemoveTemp
        );
    }

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
        // Original already falls into the catch-all `KeptFile::Original | KeptFile::Neither
        // => RemoveTemp` arm for every mode string, "keep" included — this call pins that
        // catch-all, not keep-specific behavior, and cannot fail for any cleanup_mode.
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

    #[test]
    fn apply_rename_replaces_source_with_temp() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("clip.mp4");
        let temp = dir.path().join(".clip.convertbar-tmp.mp4");
        std::fs::write(&source, b"original").unwrap();
        std::fs::write(&temp, b"reencoded").unwrap();

        apply_in_place_action(
            InPlaceAction::RenameTempOverSource,
            &temp,
            &source,
            &crate::dispose::DeleteDisposer,
        )
        .unwrap();

        assert_eq!(
            std::fs::read(&source).unwrap(),
            b"reencoded",
            "source now holds the re-encode"
        );
        assert!(!temp.exists(), "temp was consumed by the rename");
    }

    #[test]
    fn in_place_trash_mode_disposes_source_then_renames_temp() {
        // Re-encode won (KeptFile::Converted) in trash mode: the decision fn must route to
        // TrashSourceThenRename, and applying it must dispose of the SOURCE (not delete it
        // directly, and not the temp) before renaming the temp over it.
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("clip.mp4");
        let temp = dir.path().join(".clip.convertbar-tmp.mp4");
        std::fs::write(&source, b"original").unwrap();
        std::fs::write(&temp, b"reencoded").unwrap();
        let disposer = RecordingDisposer::default();

        let action = in_place_action(KeptFile::Converted, "trash");
        assert_eq!(action, InPlaceAction::TrashSourceThenRename);

        apply_in_place_action(action, &temp, &source, &disposer).unwrap();

        assert_eq!(
            disposer.0.lock().unwrap().as_slice(),
            [source.to_str().unwrap().to_string()],
            "the disposer must be called with the SOURCE path, not the temp"
        );
        assert_eq!(
            std::fs::read(&source).unwrap(),
            b"reencoded",
            "the temp was renamed over the (now-disposed) source"
        );
        assert!(!temp.exists(), "temp was consumed by the rename");
    }

    #[test]
    fn apply_remove_temp_keeps_source_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("clip.mp4");
        let temp = dir.path().join(".clip.convertbar-tmp.mp4");
        std::fs::write(&source, b"original").unwrap();
        std::fs::write(&temp, b"bigger-reencode").unwrap();

        apply_in_place_action(
            InPlaceAction::RemoveTemp,
            &temp,
            &source,
            &crate::dispose::DeleteDisposer,
        )
        .unwrap();

        assert!(!temp.exists(), "temp was removed");
        assert_eq!(
            std::fs::read(&source).unwrap(),
            b"original",
            "source is left exactly as it was"
        );
    }

    #[test]
    fn in_place_apply_is_fatal_only_for_failed_rename() {
        // A failed rename (either action that was meant to replace the source) is fatal.
        assert!(in_place_apply_is_fatal(
            Some(InPlaceAction::RenameTempOverSource),
            true
        ));
        assert!(in_place_apply_is_fatal(
            Some(InPlaceAction::TrashSourceThenRename),
            true
        ));
        // A successful apply is never fatal, whatever the action.
        assert!(!in_place_apply_is_fatal(
            Some(InPlaceAction::RenameTempOverSource),
            false
        ));
        // THE POINT OF THIS ASSERTION (Finding 5 of the final-review pass): a failed RemoveTemp
        // is benign — the source is correctly kept. RemoveTemp paired with a decision that would
        // have been KeptFile::Converted is exactly what cleanup_mode == "keep" produces for a
        // winning re-encode (in_place_action discards it rather than keeping "both"). Keying
        // fatality on `kept` alone used to misreport this expected, by-design temp-removal
        // failure as "In-place replacement failed"; keying it on the action fixes that.
        assert!(!in_place_apply_is_fatal(
            Some(InPlaceAction::RemoveTemp),
            true
        ));
        // No action taken (only the in_place branch ever sets Some) must never be fatal.
        assert!(!in_place_apply_is_fatal(None, true));
    }

    #[test]
    fn apply_rename_surfaces_failure_when_temp_missing() {
        // The hardening relies on apply_in_place_action returning Err so the job can be failed
        // rather than recorded as a false success. A missing temp makes the rename fail.
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("clip.mp4");
        let temp = dir.path().join(".clip.convertbar-tmp.mp4");
        std::fs::write(&source, b"original").unwrap();

        let result = apply_in_place_action(
            InPlaceAction::RenameTempOverSource,
            &temp,
            &source,
            &crate::dispose::DeleteDisposer,
        );

        assert!(
            result.is_err(),
            "a missing temp must surface as an error, not be swallowed"
        );
        assert_eq!(
            std::fs::read(&source).unwrap(),
            b"original",
            "the source is left intact when the rename could not happen"
        );
    }

    // TrashSourceThenRename disposes the source, then renames the temp onto its path. Those
    // two steps are only safe in that order if the first one actually happened: when the
    // Trash is refused the source is still sitting there, and the rename then overwrites the
    // user's only copy with the re-encode — destroying it outright, with nothing in the Trash
    // to recover. Data loss, not just a bad status row.
    #[test]
    fn in_place_trash_refused_preserves_the_original_instead_of_renaming_over_it() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("clip.mp4");
        let temp = dir.path().join(".clip.convertbar-tmp.mp4");
        std::fs::write(&source, b"original").unwrap();
        std::fs::write(&temp, b"reencoded").unwrap();

        let result = apply_in_place_action(
            InPlaceAction::TrashSourceThenRename,
            &temp,
            &source,
            &crate::dispose::FailingDisposer,
        );

        assert!(
            result.is_err(),
            "a refused Trash must surface so the job fails, not be swallowed"
        );
        assert_eq!(
            std::fs::read(&source).unwrap(),
            b"original",
            "THE POINT OF THIS GUARD: the original must survive a refused Trash — renaming \
             the temp over it here destroys the user's only copy"
        );
        assert!(
            temp.exists(),
            "the temp still holds the re-encoded content and must be kept for recovery"
        );
    }

    // End-to-end counterpart, and the reason the message is derived from the filesystem: the
    // mode alone would report "original may be in Trash" here and send the user hunting in
    // the Trash for a file that never left its folder.
    #[test]
    fn an_in_place_job_whose_trash_is_refused_keeps_the_original_and_says_where_it_is() {
        let (ctx, _sink) =
            test_ctx_with_disposer(test_conn(), Arc::new(crate::dispose::FailingDisposer));
        set_setting(&ctx.db, "cleanup_mode", "trash");
        let dir = tempfile::tempdir().unwrap();
        let script = successful_fake_handbrake_script(dir.path());
        set_setting(&ctx.db, "handbrake_path", script.to_str().unwrap());
        let src = dir.path().join("in.mp4");
        std::fs::write(&src, b"0123456789").unwrap();
        let original_bytes = std::fs::read(&src).unwrap();
        let p = src.to_str().unwrap();
        queue_job(&ctx.db, "j1", p, p, 10); // in-place: output_path == source_path

        process_queue(&ctx);

        let (status, msg) = job_row(&ctx.db, "j1");
        assert_eq!(
            status, "error",
            "an in-place job that could not replace the source is not a success"
        );
        assert_eq!(
            std::fs::read(&src).unwrap(),
            original_bytes,
            "the original must survive byte-identical, not be replaced by the re-encode"
        );
        let msg = msg.unwrap_or_default();
        assert!(
            msg.contains("left unchanged") && !msg.contains("Trash"),
            "the original never reached the Trash, so the message must not send the user \
             looking for it there — got: {msg}"
        );
    }

    #[test]
    fn recover_interrupted_jobs_preserves_an_in_place_source() {
        // An in-place job (output_path == source_path) interrupted mid-encode: recovery must delete
        // the hidden temp sibling and REQUEUE the job, and must NOT delete the user's original source.
        let conn = Connection::open_in_memory().unwrap();
        crate::db::init_db(&conn).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("clip.mp4");
        std::fs::write(&source, b"original").unwrap();
        let temp = in_place_temp_path(source.to_str().unwrap());
        std::fs::write(&temp, b"partial").unwrap();
        let s = source.to_str().unwrap();
        conn.execute(
            "INSERT INTO jobs (id, source_path, output_path, preset, status, queue_order, created_at)
             VALUES ('j', ?1, ?1, 'p', 'encoding', 0, '2020-01-01T00:00:00Z')",
            params![s],
        ).unwrap();

        recover_interrupted_jobs(&conn);

        assert!(source.exists(), "the in-place source must NOT be deleted");
        assert!(!temp.exists(), "the in-place temp partial must be removed");
        let status: String = conn
            .query_row("SELECT status FROM jobs WHERE id='j'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(status, "queued");
    }

    #[test]
    fn recover_interrupted_jobs_removes_a_distinct_output_and_keeps_the_source() {
        // A normal (distinct-output) interrupted job: the partial output is removed, the source is
        // untouched, and the job is requeued.
        let conn = Connection::open_in_memory().unwrap();
        crate::db::init_db(&conn).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("in.mkv");
        let output = dir.path().join("in.h265.mp4");
        std::fs::write(&source, b"src").unwrap();
        std::fs::write(&output, b"partial").unwrap();
        conn.execute(
            "INSERT INTO jobs (id, source_path, output_path, preset, status, queue_order, created_at)
             VALUES ('j', ?1, ?2, 'p', 'paused', 0, '2020-01-01T00:00:00Z')",
            params![source.to_str().unwrap(), output.to_str().unwrap()],
        ).unwrap();

        recover_interrupted_jobs(&conn);

        assert!(source.exists(), "the source is never touched");
        assert!(!output.exists(), "the partial output is removed");
        let status: String = conn
            .query_row("SELECT status FROM jobs WHERE id='j'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(status, "queued");
    }

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

    #[test]
    fn queue_paused_round_trips_and_defaults_false() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::init_db(&conn).unwrap();
        // Absent row -> false (no seed, existing DBs need no migration).
        assert!(!is_queue_paused(&conn));
        set_queue_paused(&conn, true);
        assert!(is_queue_paused(&conn));
        set_queue_paused(&conn, false);
        assert!(!is_queue_paused(&conn));
    }

    #[test]
    fn should_auto_resume_only_when_queued_and_not_paused() {
        assert!(
            should_auto_resume(true, false),
            "queued + not paused -> auto-start"
        );
        assert!(
            !should_auto_resume(true, true),
            "a remembered pause blocks auto-start"
        );
        assert!(
            !should_auto_resume(false, false),
            "nothing queued -> nothing to start"
        );
        assert!(!should_auto_resume(false, true));
    }

    // A stand-in for HandBrakeCLI that writes a small non-empty output (the last CLI arg, like -o)
    // and exits 0 — a job that completes successfully, so process_queue reaches its success/cleanup
    // and pause-after-current path.
    fn successful_fake_handbrake_script(dir: &std::path::Path) -> std::path::PathBuf {
        #[cfg(windows)]
        {
            let p = dir.join("hb-ok.cmd");
            std::fs::write(
                &p,
                "@echo off\r\n:loop\r\nif not \"%~2\"==\"\" (\r\nshift\r\ngoto loop\r\n)\r\necho done> \"%~1\"\r\nexit /b 0\r\n",
            )
            .unwrap();
            p
        }
        #[cfg(not(windows))]
        {
            use std::os::unix::fs::PermissionsExt;
            let p = dir.join("hb-ok.sh");
            std::fs::write(
                &p,
                "#!/bin/sh\nfor a; do out=\"$a\"; done\necho done > \"$out\"\nexit 0\n",
            )
            .unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
            p
        }
    }

    #[test]
    fn only_a_definitive_stat_condemns_a_source() {
        assert!(
            source_is_confirmed_missing(Ok(false)),
            "a clean stat saying the file is not there is the one case that fails the job"
        );
        assert!(
            !source_is_confirmed_missing(Ok(true)),
            "a file that is there is never condemned"
        );
        assert!(
            !source_is_confirmed_missing(Err(std::io::Error::from(
                std::io::ErrorKind::PermissionDenied
            ))),
            "an unreadable parent directory means we cannot tell — fail open rather than \
             report a file that exists as gone"
        );
    }

    #[test]
    fn a_vanished_source_fails_the_job_without_starting_handbrake() {
        // A queued file can be moved, trashed, or consumed by another tool before its turn.
        // Handing the dead path to HandBrakeCLI produces a failure whose reason is buried in a
        // stderr dump, so the queue must stat the source first and fail the job with a message
        // that says what actually happened.
        let (ctx, _sink, _disposer) = test_ctx(test_conn());

        let dir = tempfile::tempdir().unwrap();
        // A fake that DOES write its output: if the guard let the spawn through, out.mp4 exists
        // and the job ends 'done' — so the assertions below can only pass if nothing was started.
        let script = successful_fake_handbrake_script(dir.path());
        set_setting(&ctx.db, "handbrake_path", script.to_str().unwrap());
        let gone = dir.path().join("gone.mp4"); // deliberately never created
        let out = dir.path().join("out.mp4");
        queue_job(
            &ctx.db,
            "j1",
            gone.to_str().unwrap(),
            out.to_str().unwrap(),
            1000,
        );

        process_queue(&ctx);

        let (status, msg) = job_row(&ctx.db, "j1");
        assert_eq!(status, "error", "a vanished source fails the job");
        assert!(
            msg.unwrap().contains("Source file no longer exists"),
            "the history entry must name the real reason, not HandBrake's stderr"
        );
        assert!(
            !out.exists(),
            "HandBrakeCLI must never be started for a source that is gone"
        );
    }

    #[test]
    fn a_vanished_source_is_recorded_without_a_notification() {
        // The user removed the file themselves, so the history entry is the whole story — a
        // desktop notification would just be noise. record_job_error_quiet (not record_job_error)
        // is the vanished-source path, so TestSink must record zero notifications.
        let (ctx, sink, _disposer) = test_ctx(test_conn());
        // Per-file notifications ON — the setting that makes every other failure path notify.
        set_setting(&ctx.db, "notifications_per_file", "true");

        let dir = tempfile::tempdir().unwrap();
        let script = successful_fake_handbrake_script(dir.path());
        set_setting(&ctx.db, "handbrake_path", script.to_str().unwrap());
        let gone = dir.path().join("gone.mp4"); // deliberately never created
        queue_job(
            &ctx.db,
            "j1",
            gone.to_str().unwrap(),
            dir.path().join("out.mp4").to_str().unwrap(),
            1000,
        );

        process_queue(&ctx);

        assert_eq!(
            job_row(&ctx.db, "j1").0,
            "error",
            "the job still fails — it is only the notification that is suppressed"
        );
        assert_eq!(
            class_of(&ctx.db, "j1").as_deref(),
            Some("environment"),
            "a vanished source is structurally known to be Environment — never BadSource, \
             since we never got the chance to read the file ourselves"
        );
        assert!(
            sink.notifications.lock().unwrap().is_empty(),
            "the vanished-source path must never notify, even with notifications_per_file on"
        );
    }

    #[test]
    fn pause_after_current_firing_persists_queue_paused() {
        // When "Pause after this" is armed, the job completes and the queue stops — and that stop
        // must be REMEMBERED (queue_paused persisted) so the next launch does not auto-resume.
        let (ctx, _sink, _disposer) = test_ctx(test_conn());
        set_setting(&ctx.db, "cleanup_mode", "delete"); // keep cleanup filesystem-local (no Trash)
        let dir = tempfile::tempdir().unwrap();
        let script = successful_fake_handbrake_script(dir.path());
        set_setting(&ctx.db, "handbrake_path", script.to_str().unwrap());
        let src = dir.path().join("in.mp4");
        std::fs::write(&src, b"0123456789").unwrap(); // real source (10 bytes) so cleanup/metadata work
        let out = dir.path().join("out.mp4");
        queue_job(
            &ctx.db,
            "j1",
            src.to_str().unwrap(),
            out.to_str().unwrap(),
            10,
        );
        // Arm "pause after this" before the run.
        *ctx.converter.pause_after_current.lock().unwrap() = true;

        process_queue(&ctx);

        assert_eq!(job_row(&ctx.db, "j1").0, "done", "the job completes");
        assert!(
            is_queue_paused(&ctx.db.lock().unwrap()),
            "pause-after-current firing must persist the paused state"
        );
    }

    #[test]
    fn successful_encode_notifies_with_the_file_name_when_notifications_are_enabled() {
        // `test_conn` disables per-file notifications so unrelated tests aren't distracted by
        // them; this test turns the setting back on to pin the positive case a sibling of
        // `pause_after_current_firing_persists_queue_paused` doesn't cover: a successful encode
        // notifies, and the notification body names the file.
        let (ctx, sink, _disposer) = test_ctx(test_conn());
        set_setting(&ctx.db, "notifications_per_file", "true");
        set_setting(&ctx.db, "cleanup_mode", "delete"); // keep cleanup filesystem-local (no Trash)
        let dir = tempfile::tempdir().unwrap();
        let script = successful_fake_handbrake_script(dir.path());
        set_setting(&ctx.db, "handbrake_path", script.to_str().unwrap());
        let src = dir.path().join("in.mp4");
        std::fs::write(&src, b"0123456789").unwrap(); // real source (10 bytes) so cleanup/metadata work
        let out = dir.path().join("out.mp4");
        queue_job(
            &ctx.db,
            "j1",
            src.to_str().unwrap(),
            out.to_str().unwrap(),
            10,
        );

        process_queue(&ctx);

        assert_eq!(job_row(&ctx.db, "j1").0, "done", "the job completes");
        let notifications = sink.notifications.lock().unwrap();
        assert!(
            notifications
                .iter()
                .any(|(_, body)| body.contains("in.mp4")),
            "a successful encode must notify with the file name, got: {notifications:?}"
        );
    }

    // The v2.0.0 field regression. macOS refused the Trash Apple Event (the app's Automation
    // grant is pinned to a cdhash that every unsigned rebuild changes), `trash::delete`
    // returned Err — and the swallowed `let _ =` still wrote status='done',
    // kept_file='converted' and a positive space_saved while BOTH files sat on disk. Seven
    // jobs recorded ~1.1 GB "saved" that was never freed; the only way to notice was to go
    // look at the folder. A cleanup that did not happen is a failed job, not a silent one.
    #[test]
    fn a_failed_trash_of_the_original_errors_instead_of_claiming_a_false_success() {
        let (ctx, _sink) =
            test_ctx_with_disposer(test_conn(), Arc::new(crate::dispose::FailingDisposer));
        set_setting(&ctx.db, "cleanup_mode", "trash");
        let dir = tempfile::tempdir().unwrap();
        let script = successful_fake_handbrake_script(dir.path());
        set_setting(&ctx.db, "handbrake_path", script.to_str().unwrap());
        let src = dir.path().join("in.mp4");
        std::fs::write(&src, b"0123456789").unwrap(); // 10 bytes; the fake encode writes fewer
        let out = dir.path().join("out.mp4");
        queue_job(
            &ctx.db,
            "j1",
            src.to_str().unwrap(),
            out.to_str().unwrap(),
            10,
        );

        process_queue(&ctx);

        let (status, msg) = job_row(&ctx.db, "j1");
        assert_eq!(
            status, "error",
            "a job whose original is still on disk has not done what 'done' promises"
        );
        let msg = msg.unwrap_or_default();
        assert!(
            msg.contains("original") && msg.contains("both files remain"),
            "history must say WHAT is wrong on disk, not just that something failed — got: {msg}"
        );
        assert_eq!(
            class_of(&ctx.db, "j1").as_deref(),
            Some("environment"),
            "a refused Trash operation is the environment's fault, never the file's"
        );
        assert!(
            src.exists() && out.exists(),
            "fixture check: both files remaining IS the condition under test"
        );
        assert_eq!(
            saved_of(&ctx.db, "j1"),
            None,
            "nothing was freed, so the run must not book space_saved — the false 1.1 GB \
             total is what hid this bug for a whole queue"
        );
    }

    // The mirror: when the re-encode LOSES, the loser is the new file. A failed disposal
    // there leaves a stray output beside the original, so it fails the same contract.
    #[test]
    fn a_failed_trash_of_the_larger_reencode_errors_too() {
        let (ctx, _sink) =
            test_ctx_with_disposer(test_conn(), Arc::new(crate::dispose::FailingDisposer));
        set_setting(&ctx.db, "cleanup_mode", "trash");
        let dir = tempfile::tempdir().unwrap();
        let script = successful_fake_handbrake_script(dir.path());
        set_setting(&ctx.db, "handbrake_path", script.to_str().unwrap());
        let src = dir.path().join("in.mp4");
        std::fs::write(&src, b"0").unwrap(); // 1 byte, so the larger fake encode loses
        let out = dir.path().join("out.mp4");
        queue_job(
            &ctx.db,
            "j1",
            src.to_str().unwrap(),
            out.to_str().unwrap(),
            1,
        );

        process_queue(&ctx);

        let (status, msg) = job_row(&ctx.db, "j1");
        assert_eq!(
            status, "error",
            "'skipped' claims the pointless output was cleaned up — it is still there"
        );
        assert!(
            msg.unwrap_or_default().contains("re-encode"),
            "the message must name the file that is stuck, not the one we kept"
        );
        assert!(src.exists() && out.exists(), "fixture check");
    }

    // The verdict comes from the filesystem, not from the primitive's bool. A source that is
    // gone by the time we look satisfies the contract however the delete call reported — else
    // an already-vanished source would be reported as a cleanup failure it isn't.
    #[test]
    fn a_disposer_that_reports_failure_but_removed_the_file_still_completes() {
        let (ctx, _sink) =
            test_ctx_with_disposer(test_conn(), Arc::new(crate::dispose::LyingDisposer));
        set_setting(&ctx.db, "cleanup_mode", "trash");
        let dir = tempfile::tempdir().unwrap();
        let script = successful_fake_handbrake_script(dir.path());
        set_setting(&ctx.db, "handbrake_path", script.to_str().unwrap());
        let src = dir.path().join("in.mp4");
        std::fs::write(&src, b"0123456789").unwrap();
        let out = dir.path().join("out.mp4");
        queue_job(
            &ctx.db,
            "j1",
            src.to_str().unwrap(),
            out.to_str().unwrap(),
            10,
        );

        process_queue(&ctx);

        assert_eq!(
            job_row(&ctx.db, "j1").0,
            "done",
            "the original is gone, which is all 'done' promises"
        );
        assert!(!src.exists(), "fixture check: the disposer did delete it");
        // Derived from the encode's real size, never a literal: the fake script writes
        // "done\n" on Unix but "done\r\n" on Windows, so a hardcoded delta is a
        // platform-dependent failure rather than a statement about the behavior.
        let encoded = std::fs::metadata(&out).unwrap().len() as i64;
        assert_eq!(
            saved_of(&ctx.db, "j1"),
            Some(10 - encoded),
            "space_saved must be the real delta between the 10-byte source and the encode"
        );
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
        // 1000 is far bigger than the fake encode's few-byte output, so the re-encode wins.
        queue_job(
            &ctx.db,
            "j1",
            source.to_str().unwrap(),
            out.to_str().unwrap(),
            1000,
        );

        process_queue(&ctx);

        assert!(source.exists(), "keep must not remove the source");
        assert!(out.exists(), "keep must not remove the output");
        assert!(
            disposer.0.lock().unwrap().is_empty(),
            "nothing may be routed to the disposer under keep"
        );

        let (status, error) = job_row(&ctx.db, "j1");
        assert_eq!(status, "done");
        assert_eq!(
            error, None,
            "a keep job that left both files as designed is not an error"
        );
        // Derived from the encode's real size, never a literal: the fake script writes
        // "done\n" on Unix but "done\r\n" on Windows.
        let encoded = std::fs::metadata(&out).unwrap().len() as i64;
        assert_eq!(
            saved_of(&ctx.db, "j1"),
            Some(1000 - encoded),
            "keep must record the same delta delete would, even though nothing was disposed"
        );
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
        // 3 bytes is smaller than the fake encode's output (5-6 bytes), so the re-encode loses.
        queue_job(
            &ctx.db,
            "j1",
            source.to_str().unwrap(),
            out.to_str().unwrap(),
            3,
        );

        process_queue(&ctx);

        assert!(source.exists());
        assert!(out.exists(), "even a losing output survives under keep");
        assert!(disposer.0.lock().unwrap().is_empty());

        let (status, error) = job_row(&ctx.db, "j1");
        assert_eq!(status, "skipped");
        assert_eq!(
            error, None,
            "a keep job that left both files as designed is not an error"
        );
        // The negative delta, identical to what delete would record for the same sizes: keep
        // changed the disposal and nothing else.
        let encoded = std::fs::metadata(&out).unwrap().len() as i64;
        assert_eq!(saved_of(&ctx.db, "j1"), Some(3 - encoded));
    }

    // The end-to-end counterpart to `in_place_action_never_disposes_under_keep`: that test only
    // checks the pure mapping, which is why the row mis-record this pins (Finding 1 of a later
    // Fable adversarial-review pass) was invisible to it — the wrong record is produced
    // downstream, in process_queue itself. Output IS source for an in-place job, so "keep both"
    // is impossible; in_place_action maps every KeptFile variant to RemoveTemp under keep,
    // discarding the temp unconditionally. Recording ANY row here — even one that correctly said
    // kept_file = "original" — would still fingerprint the untouched source via
    // record_source_identity and permanently hide it from future scans as AlreadyConverted (see
    // cheap_skip_reason in queue_ops.rs). The fix deletes the row entirely instead.
    #[test]
    fn in_place_keep_discards_the_temp_and_leaves_no_row_behind() {
        let (ctx, _sink, disposer) = test_ctx(test_conn());

        let dir = tempfile::tempdir().unwrap();
        let script = successful_fake_handbrake_script(dir.path());
        set_setting(&ctx.db, "handbrake_path", script.to_str().unwrap());
        set_setting(&ctx.db, "cleanup_mode", "keep");

        let source = real_source(dir.path(), "movie.mkv"); // 10 real bytes on disk
        let original_bytes = std::fs::read(&source).unwrap();
        let p = source.to_str().unwrap();
        // in-place: output_path == source_path. The fake encode's few-byte output is smaller
        // than the 10-byte source, so decide_cleanup says KeptFile::Converted — the exact case
        // Finding 1 was about.
        queue_job(&ctx.db, "j1", p, p, 10);

        process_queue(&ctx);

        assert_eq!(
            std::fs::read(&source).unwrap(),
            original_bytes,
            "the source must survive byte-identical; in-place + keep never touches it"
        );
        let temp = in_place_temp_path(p);
        assert!(
            !temp.exists(),
            "the temp must be discarded, not left as a .convertbar-tmp. leftover"
        );
        assert!(
            disposer.0.lock().unwrap().is_empty(),
            "nothing may be routed to the disposer under keep"
        );

        assert!(
            !job_exists(&ctx.db, "j1"),
            "THE POINT OF THIS TEST: no row may survive an in-place + keep completion, done or \
             otherwise — any recorded row would fingerprint the untouched source and hide it \
             from every future scan as AlreadyConverted"
        );
    }

    // A stand-in for HandBrakeCLI that succeeds like successful_fake_handbrake_script, but takes
    // a beat before finishing — long enough for a test to observe the job sitting 'encoding' and
    // change a setting while it is genuinely mid-flight, short enough not to slow the suite down.
    // Unlike slow_fake_handbrake_script, nothing here ever kills this process early, so the
    // grandchild-pipe concern that rules out `ping`/`timeout` for THAT script (see its comment)
    // does not apply — the process is always allowed to exit on its own.
    fn delayed_success_fake_handbrake_script(dir: &std::path::Path) -> std::path::PathBuf {
        #[cfg(windows)]
        {
            let p = dir.join("hb-delayed-ok.cmd");
            std::fs::write(
                &p,
                "@echo off\r\n:loop\r\nif not \"%~2\"==\"\" (\r\nshift\r\ngoto loop\r\n)\r\nping -n 2 127.0.0.1 >nul\r\necho done> \"%~1\"\r\nexit /b 0\r\n",
            )
            .unwrap();
            p
        }
        #[cfg(not(windows))]
        {
            use std::os::unix::fs::PermissionsExt;
            let p = dir.join("hb-delayed-ok.sh");
            std::fs::write(
                &p,
                "#!/bin/sh\nfor a; do out=\"$a\"; done\nsleep 1\necho done > \"$out\"\nexit 0\n",
            )
            .unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
            p
        }
    }

    // Finding 1 of the final-review pass, part one: converter.rs used to capture cleanup_mode
    // ONCE at job pickup and never look at it again. A job already encoding when the user
    // clicked "Keep both files" would still apply the STALE, pre-encode mode at completion —
    // for an in-place job that meant `in_place_action(Converted, "delete")` ->
    // RenameTempOverSource, permanently overwriting (destroying) the original the user had just
    // asked to keep. `bcc0bae` fixed that by re-reading cleanup_mode fresh right before the
    // cleanup decision, which closes the ENTIRE duration of the encode.
    //
    // That fix correctly discards the temp (RemoveTemp) and leaves the source untouched, but it
    // still recorded a 'done' row with kept_file/converted_size/space_saved describing a
    // conversion that never happened, and fingerprinted the untouched source via
    // record_source_identity — which cheap_skip_reason's identity check (queue_ops.rs) treats
    // as unconditional proof the file is already converted, forever (even after switching back
    // to Delete). THIS test pins the completion side of that fix from a second-round Fable
    // review: the row must be deleted, not recorded, leaving nothing to lie about and nothing to
    // falsely fingerprint. See the sibling test below for the re-add-after-Delete half.
    #[test]
    fn cleanup_mode_switched_to_keep_mid_encode_leaves_no_row_behind() {
        let (ctx, _sink, _disposer) = test_ctx(test_conn());
        // Stale mode AT PICKUP: if the earlier fix regressed, this is what the job would
        // destructively apply at completion despite the switch to "keep" below.
        set_setting(&ctx.db, "cleanup_mode", "delete");

        let dir = tempfile::tempdir().unwrap();
        let script = delayed_success_fake_handbrake_script(dir.path());
        set_setting(&ctx.db, "handbrake_path", script.to_str().unwrap());
        let src = real_source(dir.path(), "clip.mp4"); // 10 real bytes on disk
        let original_bytes = std::fs::read(&src).unwrap();
        let p = src.to_str().unwrap();
        // in-place: output_path == source_path. The fake encode's few-byte output is smaller
        // than the 10-byte source, so decide_cleanup would say KeptFile::Converted — the exact
        // decision that "delete" mode would apply destructively, and that "keep" must discard
        // via RemoveTemp instead of recording.
        queue_job(&ctx.db, "j1", p, p, 10);

        run_queue(ctx.clone());
        wait_until("the fake encode to be running", || {
            job_row(&ctx.db, "j1").0 == "encoding"
        });

        // The switch happens WHILE the job above is mid-encode — after pickup captured the old
        // mode, before the job reaches its cleanup decision.
        crate::settings_ops::update_setting(&ctx, "cleanup_mode", "keep").unwrap();

        wait_until("the queue thread to finish", || {
            !*ctx.converter.is_running.lock().unwrap()
        });

        assert!(
            !job_exists(&ctx.db, "j1"),
            "THE POINT OF THIS TEST: the completion backstop must delete the row entirely, not \
             record a 'done' row that lies about a conversion that never happened and \
             fingerprints the untouched source, permanently hiding it from future scans"
        );
        assert_eq!(
            std::fs::read(&src).unwrap(),
            original_bytes,
            "the original must survive byte-identical: a job that was 'encoding' under 'delete' \
             when the user switched to 'keep' must honor the switch, not the stale mode captured \
             at pickup"
        );
        let temp = in_place_temp_path(p);
        assert!(
            !temp.exists(),
            "keep discards a winning in-place re-encode (RemoveTemp) rather than leaving a temp \
             sibling around — this is a wasted encode, not a kept second file"
        );
    }

    // Finding 1, part two: with the row gone and no fingerprint recorded (previous test), the
    // file must be fully eligible for conversion again once the user switches back to Delete —
    // not silently skipped forever. Before this fix, record_source_identity stamped the
    // untouched source's (size, mtime) onto the wrongly-recorded 'done' row, and
    // cheap_skip_reason's identity check is unconditional, so the file was skipped as
    // AlreadyConverted on every future add/scan, including after switching back to Delete. With
    // the row deleted, add_files must queue it like any other unconverted file, and a second run
    // must actually convert it in place.
    #[test]
    fn file_dropped_by_keep_backstop_converts_after_switching_back_to_delete() {
        let (ctx, _sink, _disposer) = test_ctx(test_conn());
        set_setting(&ctx.db, "cleanup_mode", "delete");

        let dir = tempfile::tempdir().unwrap();
        let script = delayed_success_fake_handbrake_script(dir.path());
        set_setting(&ctx.db, "handbrake_path", script.to_str().unwrap());
        let src = real_source(dir.path(), "clip.mp4");
        let p = src.to_str().unwrap();

        // A literal (non-templated) empty suffix keeps this preset in-place AND keeps
        // `add_files_inner` from ever resolving HandBrake for suffix/metadata — it only needs
        // the configured `handbrake_path` for the actual encode below.
        let preset: String = ctx
            .db
            .lock()
            .unwrap()
            .query_row("SELECT value FROM settings WHERE key = 'preset'", [], |r| {
                r.get(0)
            })
            .unwrap();
        crate::settings_ops::set_preset_suffix(&ctx, &preset, "").unwrap();

        queue_job(&ctx.db, "j1", p, p, 10);

        run_queue(ctx.clone());
        wait_until("the fake encode to be running", || {
            job_row(&ctx.db, "j1").0 == "encoding"
        });
        crate::settings_ops::update_setting(&ctx, "cleanup_mode", "keep").unwrap();
        wait_until("the first run to finish", || {
            !*ctx.converter.is_running.lock().unwrap()
        });
        assert!(
            !job_exists(&ctx.db, "j1"),
            "setup: the keep backstop must have dropped the first job's row"
        );

        // The escape hatch the fix promises: switch back to Delete...
        crate::settings_ops::update_setting(&ctx, "cleanup_mode", "delete").unwrap();

        // ...and re-add the SAME file.
        let add_result = crate::queue_ops::add_files(&ctx, &[p.to_string()]).unwrap();
        assert_eq!(
            add_result.added.len(),
            1,
            "THE POINT OF THIS TEST: no fingerprint survived the backstop, so re-adding the \
             same file must queue it again, not silently skip it as AlreadyConverted \
             (skipped: {:?})",
            add_result.skipped
        );

        run_queue(ctx.clone());
        wait_until("the second run to finish", || {
            !*ctx.converter.is_running.lock().unwrap()
        });

        let new_id = add_result.added[0].id.clone();
        let (status, error) = job_row(&ctx.db, &new_id);
        assert_eq!(
            status, "done",
            "the file must actually convert this time under Delete (error: {error:?})"
        );
    }

    #[test]
    fn claim_queue_slot_refuses_while_an_update_is_installing() {
        // Both sides serialize on the same is_running mutex, so the gate is atomic rather
        // than check-then-act.
        let converter = ConverterState::new();
        converter
            .installing
            .store(true, std::sync::atomic::Ordering::SeqCst);
        assert!(!claim_queue_slot(&converter));
        assert!(
            !*converter.is_running.lock().unwrap(),
            "a refused claim must not leave is_running set"
        );

        converter
            .installing
            .store(false, std::sync::atomic::Ordering::SeqCst);
        assert!(claim_queue_slot(&converter));
        assert!(*converter.is_running.lock().unwrap());
        assert!(
            !claim_queue_slot(&converter),
            "second claim is refused while running"
        );
    }
}
