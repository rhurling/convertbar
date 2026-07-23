use regex::Regex;
use rusqlite::{params, Connection};
use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter};

use crate::types::JobInfo;
use tauri_plugin_notification::NotificationExt;

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

/// Refresh a completed job's source-identity fingerprint to the file currently at `path`.
/// Called after an in-place encode replaces the source: the recorded `(size, mtime)` must match
/// what a folder re-scan will stat, so the encoded result is recognized as already done (no
/// re-encode cascade) while a genuinely different file that later recycles the path still fails
/// the identity check. Reuses `queue::file_identity` so the encoding stays identical to insert.
fn record_source_identity(db: &Connection, job_id: &str, path: &str) {
    if let Some(id) = crate::commands::queue::file_identity(path) {
        let _ = db.execute(
            "UPDATE jobs SET source_size = ?2, source_mtime = ?3 WHERE id = ?1",
            params![job_id, id.size, id.mtime],
        );
    }
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

/// Whether a failed in-place cleanup must demote a "successful" encode to an error. Only a failed
/// *rename* matters: for `KeptFile::Converted` the re-encode was meant to replace the source, so a
/// rename failure means it did not (and in trash mode the original may already be in Trash). A failed
/// temp removal (`Original`/`Neither`) is benign — the source is correctly kept, only an orphan temp
/// lingers (it is marker-excluded from scans and pre-cleared on the next in-place encode).
fn in_place_apply_is_fatal(kept: KeptFile, apply_failed: bool) -> bool {
    apply_failed && matches!(kept, KeptFile::Converted)
}

pub struct ConverterState {
    pub current_pid: Mutex<Option<u32>>,
    pub current_child: Mutex<Option<Child>>,
    pub current_job_id: Mutex<Option<String>>,
    pub is_paused: Mutex<bool>,
    pub is_running: Mutex<bool>,
    pub pause_after_current: Mutex<bool>,
    /// One-way app-teardown latch: armed by `kill_active_child`, checked by
    /// `process_queue` so the queue thread never spawns another encoder mid-quit.
    pub shutdown: std::sync::atomic::AtomicBool,
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
        }
    }

    pub fn is_shutting_down(&self) -> bool {
        self.shutdown.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Returns true if the current platform supports real process pause/resume (SIGSTOP/SIGCONT).
    pub fn can_pause_process() -> bool {
        cfg!(target_os = "macos")
    }

    /// Whether the queue is armed to pause after the current job. The source of truth for
    /// the "Pause after this" button, which reads it on mount rather than mirroring locally.
    pub fn is_pause_after_current(&self) -> bool {
        self.pause_after_current.lock().map(|g| *g).unwrap_or(false)
    }
}

/// Kill the active HandBrake child (resuming it first if SIGSTOP-paused, since a
/// stopped process can't act on SIGTERM-class signals) and reap it, so quitting the
/// app can't orphan an encoder that would keep burning CPU for hours. The partial
/// output is left alone: the next launch's auto-resume deletes it once no process
/// holds it.
pub(crate) fn kill_active_child(converter: &ConverterState) {
    // Arm shutdown BEFORE touching the child: without it the queue thread's next
    // iteration spawns a fresh encoder during teardown, and a spawn that raced this
    // call (child not yet stored in current_child) would be missed entirely —
    // process_queue re-checks the flag right after storing the handle.
    converter
        .shutdown
        .store(true, std::sync::atomic::Ordering::SeqCst);
    #[cfg(target_os = "macos")]
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
                kept_file, space_saved, error_message, queue_order, created_at, completed_at
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
            queue_order: row.get(10)?,
            created_at: row.get(11)?,
            completed_at: row.get(12)?,
        })
    })
    .ok()
}

fn get_handbrake_path(db: &Connection) -> Option<String> {
    let configured: Option<String> = db
        .query_row(
            "SELECT value FROM settings WHERE key = 'handbrake_path'",
            [],
            |row| row.get(0),
        )
        .ok();

    if let Some(ref path) = configured {
        if !path.is_empty() && std::path::Path::new(path).exists() {
            return Some(path.clone());
        }
    }

    crate::handbrake::detect_handbrake_path()
}

fn get_cleanup_mode(db: &Connection) -> String {
    db.query_row(
        "SELECT value FROM settings WHERE key = 'cleanup_mode'",
        [],
        |row| row.get::<_, String>(0),
    )
    .unwrap_or_else(|_| "trash".to_string())
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

const STDERR_TAIL_BYTES: usize = 4096;

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

/// Substrings that mark a line as the actual failure reason. HandBrake opens its
/// stderr with a build banner and host-info preamble (none of which match these), so
/// the first hit is the diagnostic rather than the noise above it.
const DIAGNOSTIC_MARKERS: [&str; 18] = [
    "error",
    "failed",
    "fatal",
    "aborted",
    "not found",
    "no such file",
    "no title",
    "unrecognized",
    "unsupported",
    "invalid",
    "corrupt",
    "no space",
    "read-only",
    "permission denied",
    "not permitted",
    "cannot",
    "could not",
    "unable",
];

/// The first line that reads like a failure reason, or None if nothing stands out.
fn diagnostic_headline<'a>(lines: &[&'a str]) -> Option<&'a str> {
    lines.iter().copied().find(|line| {
        let lower = line.to_lowercase();
        DIAGNOSTIC_MARKERS.iter().any(|m| lower.contains(m))
    })
}

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
    match diagnostic_headline(&lines) {
        Some(headline) => format!("{prefix}: {headline}\n{tail_block}"),
        None => format!("{prefix}:\n{tail_block}"),
    }
}

/// The bare failure prefixes written before the diagnostic headline was promoted. A
/// stored message whose first line is exactly one of these predates the change and
/// still leads with HandBrake's banner. Kept in sync with the `message_with_tail`
/// callers below.
const LEGACY_ERROR_PREFIXES: [&str; 2] = [
    "Conversion failed:",
    "Conversion produced an empty output file:",
];

/// Rewrite a previously-stored error message so its first line is the failure reason
/// instead of HandBrake's build banner. Returns None when the message is already
/// headlined, isn't one of our messages, or has no recognizable diagnostic — which
/// makes the backfill that calls this idempotent (a rewritten first line no longer
/// matches a legacy prefix).
pub(crate) fn promote_stored_diagnostic(message: &str) -> Option<String> {
    let (first_line, body) = message.split_once('\n')?;
    if !LEGACY_ERROR_PREFIXES.contains(&first_line) {
        return None;
    }
    let headline = diagnostic_headline(&body.lines().collect::<Vec<_>>())?;
    Some(format!("{first_line} {headline}\n{body}"))
}

fn error_message_from_tail(tail: &str) -> String {
    message_with_tail("Conversion failed", tail)
}

/// Zero-byte outputs fail with the same stderr diagnostics as a nonzero exit —
/// HandBrake usually said why nothing was written (e.g. "No title found").
fn empty_output_error_message(tail: &str) -> String {
    message_with_tail("Conversion produced an empty output file", tail)
}

/// Record a failed job: status + error_message in the DB, the two frontend events,
/// and the per-file notification. Shared by every failure path in process_queue.
fn record_job_error<R: tauri::Runtime>(
    app: &AppHandle<R>,
    db: &Arc<Mutex<Connection>>,
    job_id: &str,
    file_name: &str,
    err_msg: &str,
) {
    let now = chrono::Utc::now().to_rfc3339();
    {
        let db = db.lock().unwrap();
        let _ = db.execute(
            "UPDATE jobs SET status = 'error', error_message = ?2, completed_at = ?3 WHERE id = ?1",
            params![job_id, err_msg, now],
        );
    }
    let _ = app.emit(
        "job-error",
        serde_json::json!({ "job_id": job_id, "error": err_msg }),
    );
    let _ = app.emit(
        "job-status-changed",
        serde_json::json!({ "job_id": job_id, "status": "error" }),
    );

    let notify_per_file = {
        let db = db.lock().unwrap();
        db.query_row(
            "SELECT value FROM settings WHERE key='notifications_per_file'",
            params![],
            |r| r.get::<_, String>(0),
        )
        .map(|v| v == "true")
        .unwrap_or(true)
    };
    if notify_per_file {
        let _ = app
            .notification()
            .builder()
            .title("ConvertBar")
            .body(&format!("{} failed", file_name))
            .show();
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
fn process_queue<R: tauri::Runtime>(
    app: &AppHandle<R>,
    db: &Arc<Mutex<Connection>>,
    converter: &ConverterState,
) {
    // Clears is_running on every exit path (normal, early return, or panic).
    let _running = RunningGuard(converter);
    let mut had_errors = false;
    loop {
        // Quit path: kill_active_child armed shutdown. Bail before picking up another
        // job — teardown would otherwise race a fresh HandBrakeCLI spawn and orphan it.
        // Return (not break) so no "Queue complete" notification fires mid-quit.
        if converter.is_shutting_down() {
            return;
        }
        let job;
        let handbrake_path_opt;
        let cleanup_mode;
        let low_disk_min_gb;
        {
            let db = db.lock().unwrap();
            job = match get_next_job(&db) {
                Some(j) => j,
                None => break,
            };
            handbrake_path_opt = get_handbrake_path(&db);
            cleanup_mode = get_cleanup_mode(&db);
            low_disk_min_gb = get_low_disk_min_gb(&db);
            // The job is flipped to 'encoding' below, AFTER the low-disk gate — a gated job
            // must stay 'queued' so the Resume button can retry it.
        }

        let file_name = std::path::Path::new(&job.source_path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown")
            .to_string();

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
                    let _ = app.emit(
                        "queue-paused-low-disk",
                        serde_json::json!({
                            "path": job.output_path,
                            "available_bytes": available,
                            "required_bytes": required,
                        }),
                    );
                    let _ = app.emit(
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
                record_job_error(app, db, &job.id, &file_name, "HandBrakeCLI not found");
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
        let claimed = {
            let db = db.lock().unwrap();
            db.execute(
                "UPDATE jobs SET status = 'encoding' WHERE id = ?1 AND status = 'queued'",
                params![job.id],
            )
            .unwrap_or(0)
        };
        if claimed == 0 {
            continue;
        }

        *converter.current_job_id.lock().unwrap() = Some(job.id.clone());
        *converter.is_paused.lock().unwrap() = false;

        let _ = app.emit(
            "job-status-changed",
            serde_json::json!({
                "job_id": job.id,
                "status": "encoding"
            }),
        );

        // Count remaining queued jobs for tray info
        let queue_count: usize = {
            let db = db.lock().unwrap();
            db.query_row(
                "SELECT COUNT(*) FROM jobs WHERE status = 'queued'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap_or(0) as usize
        };

        let _ = app.emit(
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
                    app,
                    db,
                    &job.id,
                    &file_name,
                    &format!("Failed to start HandBrakeCLI: {}", e),
                );
                *converter.current_job_id.lock().unwrap() = None;
                continue;
            }
        };

        let pid = child.id();
        *converter.current_pid.lock().unwrap() = Some(pid);

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
        *converter.current_child.lock().unwrap() = Some(child);

        // Close the spawn→store window: if quit armed shutdown after the spawn but
        // before the handle was stored, kill_active_child found None — reap it here.
        // wait_for_active_child then sees the killed status and takes the error path.
        if converter.is_shutting_down() {
            kill_active_child(converter);
        }

        let job_id = job.id.clone();
        let app_clone = app.clone();
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
                                        let _ = app_clone.emit(
                                            "conversion-progress",
                                            ConversionProgress {
                                                job_id: job_id.clone(),
                                                percent,
                                                fps,
                                                avg_fps,
                                                eta_seconds: eta,
                                            },
                                        );
                                        let _ = app_clone.emit(
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

        let exit_status = wait_for_active_child(converter);

        if let Some(handle) = progress_thread {
            let _ = handle.join();
        }

        *converter.current_pid.lock().unwrap() = None;
        *converter.current_child.lock().unwrap() = None;
        *converter.current_job_id.lock().unwrap() = None;

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
                    // The child exited, so the drain thread is at EOF and joins
                    // promptly; its tail says why nothing was written.
                    let tail = stderr_tail_thread
                        .and_then(|t| t.join().ok())
                        .unwrap_or_default();
                    record_job_error(
                        app,
                        db,
                        &job.id,
                        &file_name,
                        &empty_output_error_message(&tail),
                    );
                    continue;
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

                // Act on the decision. In-place replaces/keeps the source via the temp; the
                // distinct-file path keeps both names and trashes/deletes the loser as before.
                let in_place_apply_failed = if in_place {
                    let action = in_place_action(kept, &cleanup_mode);
                    apply_in_place_action(
                        action,
                        &encode_target,
                        std::path::Path::new(&job.source_path),
                    )
                    .is_err()
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
                    false
                };

                // A failed in-place *rename* means the re-encode never replaced the source (and in
                // trash mode the original may now be in Trash, with the temp left behind). Record an
                // error instead of a false "done" so history never claims a success that left the
                // file out of place. A failed temp *removal* is benign and handled as success.
                if in_place_apply_is_fatal(kept, in_place_apply_failed) {
                    had_errors = true;
                    // In trash mode the original was moved to Trash before the rename failed; in
                    // delete mode the rename-over-source failed and the original is untouched.
                    let err_msg = if cleanup_mode == "delete" {
                        "In-place replacement failed; original left unchanged"
                    } else {
                        "In-place replacement failed; original may be in Trash"
                    };
                    record_job_error(app, db, &job.id, &file_name, err_msg);
                    // Intentionally leave the temp (`.{stem}.convertbar-tmp.mp4`): it holds the
                    // re-encoded content, and in trash mode it is the only in-place copy (the
                    // original is in Trash), so removing it would force trash recovery. The marker
                    // keeps it out of scans, and the next in-place encode pre-clears it.
                    continue;
                }

                let kept_file = match kept {
                    KeptFile::Converted => "converted",
                    KeptFile::Original | KeptFile::Neither => "original",
                };

                let now = chrono::Utc::now().to_rfc3339();

                {
                    let db = db.lock().unwrap();
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

                let _ = app.emit(
                    "job-completed",
                    serde_json::json!({
                        "job_id": job.id,
                        "status": status_str,
                        "kept_file": kept_file,
                        "space_saved": space_saved,
                    }),
                );

                let _ = app.emit(
                    "job-status-changed",
                    serde_json::json!({
                        "job_id": job.id,
                        "status": status_str,
                    }),
                );

                // Notification logic for successful/skipped jobs
                {
                    let (notify_per_file, errors_only) = {
                        let db = db.lock().unwrap();
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
                            let _ = app
                                .notification()
                                .builder()
                                .title("ConvertBar")
                                .body(&body)
                                .show();
                        }
                    }
                }

                // Pause after this job if the one-shot flag is armed (consumed here so the next
                // queued job does not also pause).
                if take_pause_after_current(converter) {
                    let _ = app.emit(
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
            Ok(_) | Err(_) => {
                // Quit path: the exit handler killed this child, the encode didn't
                // fail. Skip ALL bookkeeping — deleting the partial, writing
                // status='error' (auto-resume only picks up 'encoding'/'paused'),
                // or notifying "failed" would let a scheduler race decide the
                // job's fate. The loop head turns this continue into the return.
                if converter.is_shutting_down() {
                    continue;
                }
                had_errors = true;
                // Remove the partial encode output (the temp for in-place jobs), never the source.
                let _ = std::fs::remove_file(&encode_target);

                let current_status: Option<String> = db
                    .lock()
                    .unwrap()
                    .query_row(
                        "SELECT status FROM jobs WHERE id = ?1",
                        params![job.id],
                        |row| row.get(0),
                    )
                    .ok();

                if current_status.as_deref() != Some("error") {
                    // The drain thread hit EOF when the child died, so this join is
                    // prompt. Its tail is the only diagnostic HandBrake leaves behind.
                    let tail = stderr_tail_thread
                        .and_then(|t| t.join().ok())
                        .unwrap_or_default();
                    record_job_error(
                        app,
                        db,
                        &job.id,
                        &file_name,
                        &error_message_from_tail(&tail),
                    );
                }
            }
        }
    }

    // No more jobs — queue done notification
    {
        let notify_queue_done = {
            let db = db.lock().unwrap();
            db.query_row(
                "SELECT value FROM settings WHERE key='notifications_queue_done'",
                params![],
                |r| r.get::<_, String>(0),
            )
            .map(|v| v == "true")
            .unwrap_or(true)
        };
        if notify_queue_done {
            let _ = app
                .notification()
                .builder()
                .title("ConvertBar")
                .body("Queue complete")
                .show();
        }
    }

    let final_status = final_run_status(had_errors);
    let _ = app.emit(
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

/// Starts queue processing in a new background thread.
/// Sets `is_running` to true atomically before spawning.
pub fn run_queue<R: tauri::Runtime>(
    app: AppHandle<R>,
    db: Arc<Mutex<Connection>>,
    converter: Arc<ConverterState>,
) {
    {
        // Poison-tolerant: if a prior queue thread panicked while briefly holding this lock,
        // recover the flag rather than propagating the poison and permanently wedging starts.
        let mut running = converter
            .is_running
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if *running {
            return;
        }
        *running = true;
    }

    std::thread::spawn(move || {
        process_queue(&app, &db, &converter);
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
    use tauri::Listener;

    // --- process_queue integration harness (mock runtime, in-memory DB) ---

    fn mock_app() -> tauri::App<tauri::test::MockRuntime> {
        tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap()
    }

    fn test_db() -> Arc<Mutex<Connection>> {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::init_db(&conn).unwrap();
        // Notifications route through the notification plugin, which isn't
        // registered on the mock app — disable them so process_queue skips it.
        conn.execute(
            "UPDATE settings SET value = 'false'
             WHERE key IN ('notifications_per_file', 'notifications_queue_done')",
            [],
        )
        .unwrap();
        Arc::new(Mutex::new(conn))
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

    fn record_events(
        app: &tauri::App<tauri::test::MockRuntime>,
        name: &str,
    ) -> Arc<Mutex<Vec<String>>> {
        let store = Arc::new(Mutex::new(Vec::new()));
        let sink = store.clone();
        app.listen_any(name.to_string(), move |event| {
            sink.lock().unwrap().push(event.payload().to_string());
        });
        store
    }

    #[test]
    fn spawn_failure_surfaces_like_every_other_error() {
        // A configured handbrake_path that exists but is not executable makes
        // Command::spawn fail. That branch must behave like all other failures:
        // job-status-changed fires (so the UI row updates) and the run ends with
        // the tray in "error", not a clean "idle" that hides every job failing.
        let app = mock_app();
        let db = test_db();
        let converter = ConverterState::new();

        let dir = tempfile::tempdir().unwrap();
        let fake = dir.path().join("not-a-binary.txt");
        std::fs::write(&fake, "plain text").unwrap();
        set_setting(&db, "handbrake_path", fake.to_str().unwrap());
        queue_job(&db, "j1", "/nowhere/in.mp4", "/nowhere/out.mp4", 1000);

        let status_events = record_events(&app, "job-status-changed");
        let menubar_events = record_events(&app, "menu-bar-update");

        process_queue(app.handle(), &db, &converter);

        let (status, msg) = job_row(&db, "j1");
        assert_eq!(status, "error");
        assert!(msg.unwrap().contains("Failed to start HandBrakeCLI"));
        assert!(
            status_events
                .lock()
                .unwrap()
                .iter()
                .any(|p| p.contains("\"error\"")),
            "spawn failure must emit job-status-changed like other failure paths"
        );
        let final_update = menubar_events.lock().unwrap().last().cloned().unwrap();
        assert!(
            final_update.contains("\"error\""),
            "a run where the only job failed must end 'error', got: {final_update}"
        );
    }

    #[test]
    fn low_disk_threshold_pauses_before_spawning_and_leaves_the_job_queued() {
        // An absurd threshold makes required-free exceed any real disk, so the gate always trips —
        // deterministic regardless of the test machine's free space.
        let app = mock_app();
        let db = test_db();
        let converter = ConverterState::new();

        let dir = tempfile::tempdir().unwrap();
        // A real fake HandBrake IS configured: if the gate failed to stop the run, the job would be
        // spawned and end 'error' (empty output). Staying 'queued' proves the gate blocked the spawn.
        let script = fake_handbrake_script(dir.path());
        set_setting(&db, "handbrake_path", script.to_str().unwrap());
        set_setting(&db, "low_disk_min_gb", "1000000000"); // 1e9 GB
        let out = dir.path().join("out.mp4");
        queue_job(&db, "j1", "/nowhere/a.mp4", out.to_str().unwrap(), 1000);

        let paused_events = record_events(&app, "queue-paused-low-disk");
        let status_events = record_events(&app, "job-status-changed");

        *converter.is_running.lock().unwrap() = true;
        process_queue(app.handle(), &db, &converter);

        let (status, _msg) = job_row(&db, "j1");
        assert_eq!(
            status, "queued",
            "a low-disk pause leaves the job queued, never encoding/error"
        );
        assert!(
            !out.exists(),
            "the encode must never start, so no output is written"
        );
        assert_eq!(
            paused_events.lock().unwrap().len(),
            1,
            "exactly one low-disk pause event fires"
        );
        assert!(
            paused_events.lock().unwrap()[0].contains("required_bytes"),
            "the event carries the required-free figure for the UI"
        );
        assert!(
            status_events
                .lock()
                .unwrap()
                .iter()
                .all(|p| !p.contains("\"encoding\"")),
            "the job must never transition to encoding"
        );
        assert!(
            !*converter.is_running.lock().unwrap(),
            "the queue thread must have stopped"
        );
    }

    #[test]
    fn low_disk_check_is_skipped_when_threshold_is_zero() {
        // Threshold 0 = disabled: the gate is a no-op and the job runs through to the encode stage.
        let app = mock_app();
        let db = test_db();
        let converter = ConverterState::new();

        let dir = tempfile::tempdir().unwrap();
        let script = fake_handbrake_script(dir.path()); // exits 0, writes nothing -> empty-output error
        set_setting(&db, "handbrake_path", script.to_str().unwrap());
        set_setting(&db, "low_disk_min_gb", "0");
        let out = dir.path().join("out.mp4");
        queue_job(&db, "j1", "/nowhere/a.mp4", out.to_str().unwrap(), 1000);

        process_queue(app.handle(), &db, &converter);

        let (status, msg) = job_row(&db, "j1");
        assert_eq!(
            status, "error",
            "with the check disabled, the job is processed, not held 'queued'"
        );
        assert!(
            msg.unwrap().contains("empty output file"),
            "the job reached the encode stage (fake HandBrake produced no output)"
        );
    }

    #[test]
    fn low_disk_check_fails_open_when_destination_is_unstattable() {
        // A configured threshold must NOT block a job whose destination free space can't be
        // read (nonexistent parent dir -> fs4::available_space errs -> None). Fail open: the
        // encode proceeds rather than the queue wedging on an unreadable volume.
        let app = mock_app();
        let db = test_db();
        let converter = ConverterState::new();

        let dir = tempfile::tempdir().unwrap();
        let script = fake_handbrake_script(dir.path()); // exits 0, writes nothing -> empty-output error
        set_setting(&db, "handbrake_path", script.to_str().unwrap());
        set_setting(&db, "low_disk_min_gb", "5"); // gate enabled, but destination can't be stat'd
                                                  // Output under a parent directory that does not exist -> available-space query fails -> None.
        queue_job(
            &db,
            "j1",
            "/nowhere/a.mp4",
            "/no-such-dir-xyz/out.mp4",
            1000,
        );

        let paused_events = record_events(&app, "queue-paused-low-disk");

        process_queue(app.handle(), &db, &converter);

        let (status, msg) = job_row(&db, "j1");
        assert_eq!(
            status, "error",
            "fail-open: the job runs to the encode stage, not held 'queued'"
        );
        assert!(
            msg.unwrap().contains("empty output file"),
            "the disabled/unreadable-disk gate let the encode proceed"
        );
        assert!(
            paused_events.lock().unwrap().is_empty(),
            "no low-disk pause event fires when free space is unknown"
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
        let app = mock_app();
        let db = test_db();
        let converter = Arc::new(ConverterState::new());

        let dir = tempfile::tempdir().unwrap();
        let script = slow_fake_handbrake_script(dir.path());
        set_setting(&db, "handbrake_path", script.to_str().unwrap());
        let output = dir.path().join("out.mp4");
        queue_job(&db, "j1", "/nowhere/a.mp4", output.to_str().unwrap(), 1000);

        let error_events = record_events(&app, "job-error");

        run_queue(app.handle().clone(), db.clone(), converter.clone());
        // The partial must exist BEFORE the kill, or the exists() assertion below
        // would pass vacuously against a file the script never got to write.
        wait_until(
            "the fake encode to be running with a partial on disk",
            || {
                job_row(&db, "j1").0 == "encoding"
                    && converter.current_child.lock().unwrap().is_some()
                    && output.exists()
            },
        );

        kill_active_child(&converter);
        wait_until("the queue thread to exit", || {
            !*converter.is_running.lock().unwrap()
        });

        let (status, msg) = job_row(&db, "j1");
        assert_eq!(
            status, "encoding",
            "quit must leave the job for auto-resume, not record it as failed (msg: {msg:?})"
        );
        assert!(
            output.exists(),
            "the partial output belongs to next-launch auto-resume cleanup, not the quit path"
        );
        assert!(
            error_events.lock().unwrap().is_empty(),
            "no failure events/notifications may fire because the user quit: {:?}",
            error_events.lock().unwrap()
        );
    }

    #[test]
    fn zero_byte_output_fails_with_diagnostics_and_the_queue_continues() {
        // Exit 0 with an empty/missing output must record an error carrying the
        // stderr tail (not a bare fixed string), and must not abort the run — the
        // next queued job still gets processed.
        let app = mock_app();
        let db = test_db();
        let converter = ConverterState::new();

        let dir = tempfile::tempdir().unwrap();
        let script = fake_handbrake_script(dir.path());
        set_setting(&db, "handbrake_path", script.to_str().unwrap());
        let out1 = dir.path().join("out1.mp4");
        let out2 = dir.path().join("out2.mp4");
        queue_job(&db, "j1", "/nowhere/a.mp4", out1.to_str().unwrap(), 1000);
        queue_job(&db, "j2", "/nowhere/b.mp4", out2.to_str().unwrap(), 1000);

        let menubar_events = record_events(&app, "menu-bar-update");

        process_queue(app.handle(), &db, &converter);

        for id in ["j1", "j2"] {
            let (status, msg) = job_row(&db, id);
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
        }
        assert!(
            !out1.exists() && !out2.exists(),
            "empty outputs are removed"
        );
        let final_update = menubar_events.lock().unwrap().last().cloned().unwrap();
        assert!(final_update.contains("\"error\""));
    }

    // End-to-end (local/e2e-ignored CI only): needs ffmpeg to synthesize a clip and a
    // real HandBrakeCLI on PATH. The only test driving process_queue's full DB state
    // machine queued → encoding → done with a real encode. Run with:
    //   cargo test -- --ignored process_queue_drives_a_real_encode
    #[test]
    #[ignore]
    fn process_queue_drives_a_real_encode_from_queued_to_done() {
        let app = mock_app();
        let db = test_db();
        let converter = ConverterState::new();
        // 'delete' keeps the cleanup assertion filesystem-local (no Trash involved).
        set_setting(&db, "cleanup_mode", "delete");

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
            &db,
            "j1",
            source.to_str().unwrap(),
            output.to_str().unwrap(),
            original_size,
        );

        let status_events = record_events(&app, "job-status-changed");

        process_queue(app.handle(), &db, &converter);

        // The UI saw the full transition, not just the terminal state.
        let events = status_events.lock().unwrap().clone();
        assert!(
            events.iter().any(|p| p.contains("\"encoding\"")),
            "missing encoding transition: {events:?}"
        );
        assert!(
            events.last().unwrap().contains("\"done\""),
            "missing done transition: {events:?}"
        );

        let (status, completed_at, converted_size, kept_file, space_saved) = db
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
    fn promote_stored_diagnostic_rewrites_old_banner_first_messages() {
        let old = "Conversion failed:\n\
                   [00:00:00] Compile-time hardening features are enabled\n\
                   [mov] moov atom not found\n\
                   No title found.";
        let promoted =
            promote_stored_diagnostic(old).expect("a banner-first legacy row should be rewritten");
        assert_eq!(
            promoted.lines().next().unwrap(),
            "Conversion failed: [mov] moov atom not found"
        );
        assert!(promoted.contains("No title found."), "detail is preserved");
        // Idempotent: a second pass over the rewritten message is a no-op.
        assert_eq!(promote_stored_diagnostic(&promoted), None);
    }

    #[test]
    fn promote_stored_diagnostic_leaves_foreign_messages_untouched() {
        // Already headlined (space after the prefix, not a bare "prefix:").
        assert_eq!(
            promote_stored_diagnostic(
                "Conversion failed: moov atom not found\nmoov atom not found"
            ),
            None
        );
        // Single-line generic fallback with no tail to promote from.
        assert_eq!(promote_stored_diagnostic("Conversion failed"), None);
        // Legacy shape but nothing diagnostic in the body — leave it rather than promote noise.
        assert_eq!(
            promote_stored_diagnostic("Conversion failed:\nScanning title 1\nOpening file"),
            None
        );
        // The empty-output prefix is handled too.
        assert_eq!(
            promote_stored_diagnostic(
                "Conversion produced an empty output file:\nbanner\nNo space left on device"
            )
            .unwrap()
            .lines()
            .next()
            .unwrap(),
            "Conversion produced an empty output file: No space left on device"
        );
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
        let expected = crate::commands::queue::file_identity(&path_str).unwrap();
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
    fn apply_rename_replaces_source_with_temp() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("clip.mp4");
        let temp = dir.path().join(".clip.convertbar-tmp.mp4");
        std::fs::write(&source, b"original").unwrap();
        std::fs::write(&temp, b"reencoded").unwrap();

        apply_in_place_action(InPlaceAction::RenameTempOverSource, &temp, &source).unwrap();

        assert_eq!(
            std::fs::read(&source).unwrap(),
            b"reencoded",
            "source now holds the re-encode"
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

        apply_in_place_action(InPlaceAction::RemoveTemp, &temp, &source).unwrap();

        assert!(!temp.exists(), "temp was removed");
        assert_eq!(
            std::fs::read(&source).unwrap(),
            b"original",
            "source is left exactly as it was"
        );
    }

    #[test]
    fn in_place_apply_is_fatal_only_for_failed_rename() {
        // A failed rename (Converted) is fatal: the source was meant to be replaced and wasn't.
        assert!(in_place_apply_is_fatal(KeptFile::Converted, true));
        // A successful apply is never fatal.
        assert!(!in_place_apply_is_fatal(KeptFile::Converted, false));
        // A failed temp removal (Original/Neither) is benign: the source is correctly kept.
        assert!(!in_place_apply_is_fatal(KeptFile::Original, true));
        assert!(!in_place_apply_is_fatal(KeptFile::Neither, true));
    }

    #[test]
    fn apply_rename_surfaces_failure_when_temp_missing() {
        // The hardening relies on apply_in_place_action returning Err so the job can be failed
        // rather than recorded as a false success. A missing temp makes the rename fail.
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("clip.mp4");
        let temp = dir.path().join(".clip.convertbar-tmp.mp4");
        std::fs::write(&source, b"original").unwrap();

        let result = apply_in_place_action(InPlaceAction::RenameTempOverSource, &temp, &source);

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
}
