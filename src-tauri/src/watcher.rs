use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use tauri::{AppHandle, Emitter, Manager};

use crate::commands::queue;
use crate::converter::{self, ConverterState};
use crate::AppState;

/// Download tools write to a temporary name and rename to the final name only when complete.
/// Files carrying one of these extensions are known-incomplete and must never be enqueued —
/// we wait for the rename to the real (video) extension instead. This is an explicit guard
/// rather than relying on the fact that these happen not to be in `VIDEO_EXTENSIONS`, so the
/// intent survives any future change to the video-extension list.
const TEMP_EXTENSIONS: &[&str] = &["part", "crdownload", "download", "tmp", "partial", "!ut"];

/// True when `path`'s extension marks an in-progress download (see `TEMP_EXTENSIONS`).
pub(crate) fn is_temp_file(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| TEMP_EXTENSIONS.contains(&ext.to_lowercase().as_str()))
        .unwrap_or(false)
}

/// A file the watcher has seen change and is waiting to settle before enqueuing.
#[derive(Debug, Clone)]
pub(crate) struct PendingEntry {
    size: u64,
    mtime: SystemTime,
    /// When `size`/`mtime` were last observed to change. The stability timer counts from here.
    last_change: Instant,
    /// How long the file must stay unchanged before it is considered finished writing.
    delay: Duration,
}

impl PendingEntry {
    pub(crate) fn new(size: u64, mtime: SystemTime, now: Instant, delay: Duration) -> Self {
        Self {
            size,
            mtime,
            last_change: now,
            delay,
        }
    }

    /// Fold in a fresh stat reading taken at `now`. Returns `true` when the file has been
    /// unchanged (same size and mtime) for at least `delay` — i.e. it is finished writing.
    /// Any change resets the stability timer.
    pub(crate) fn observe(&mut self, size: u64, mtime: SystemTime, now: Instant) -> bool {
        if size != self.size || mtime != self.mtime {
            self.size = size;
            self.mtime = mtime;
            self.last_change = now;
            return false;
        }
        now.duration_since(self.last_change) >= self.delay
    }
}

/// Computes the watch changes needed to move from the `current` set to the `desired` set.
/// Each entry is `(path, recursive)`. Returns `(to_unwatch, to_watch)`. A path whose recursive
/// mode changed appears in both — unwatch the old, re-watch with the new mode.
pub(crate) fn diff_watches(
    current: &[(PathBuf, bool)],
    desired: &[(PathBuf, bool)],
) -> (Vec<PathBuf>, Vec<(PathBuf, bool)>) {
    let to_unwatch = current
        .iter()
        .filter(|entry| !desired.contains(entry))
        .map(|(path, _)| path.clone())
        .collect();
    let to_watch = desired
        .iter()
        .filter(|entry| !current.contains(entry))
        .cloned()
        .collect();
    (to_unwatch, to_watch)
}

/// Drop pending (mid-stabilization) files that no desired config still covers. A file still
/// settling when the user disables/removes its watch must not be enqueued by the reaper once it
/// stabilizes. Testing "still covered by a desired config" (rather than "under a removed root")
/// avoids over-purging: removing an enclosing watch (`/w`) must NOT drop a file under a still-active
/// nested watch (`/w/sub`) — no further FS event would re-add a file that already stopped changing.
/// It also subsumes the recursive-mode-flip case: a subfolder file survives only if the new
/// (possibly non-recursive) config still covers it.
pub(crate) fn purge_pending_uncovered(
    pending: &mut HashMap<PathBuf, PendingEntry>,
    desired: &[WatchedDirConfig],
) {
    pending.retain(|path, _| delay_for_path(desired, path).is_some());
}

/// A watched directory's runtime config, used by the filesystem-event handler to decide whether
/// (and with what delay) an incoming path should be tracked. Kept separate from the serde
/// `WatchedDirectory` DB row so the hot path doesn't carry id/created_at/enabled.
#[derive(Debug, Clone)]
pub(crate) struct WatchedDirConfig {
    pub path: PathBuf,
    pub recursive: bool,
    pub delay: Duration,
}

/// Returns the stability delay to apply to `path` if it is a video file that belongs to one of
/// the watched directories (respecting each directory's recursive flag), or `None` if the path
/// should be ignored. Temp/partial download files are always ignored.
pub(crate) fn delay_for_path(configs: &[WatchedDirConfig], path: &Path) -> Option<Duration> {
    if !queue::is_video_file(path) || is_temp_file(path) {
        return None;
    }
    let parent = path.parent()?;
    configs.iter().find_map(|config| {
        let inside = if config.recursive {
            path.starts_with(&config.path)
        } else {
            parent == config.path
        };
        inside.then_some(config.delay)
    })
}

/// True when a skip-marker file named `marker` exists in `path`'s own directory or any ancestor
/// up to (and including) the watched root that contains `path`. The walk is bounded at the watched
/// root so a stray file with the marker name above the watched tree is never honored, and returns
/// `false` when `path` sits inside no watched directory. `marker_exists` is injected so the walk
/// is unit-testable without touching the filesystem.
pub(crate) fn has_active_marker(
    configs: &[WatchedDirConfig],
    path: &Path,
    marker: &str,
    marker_exists: impl Fn(&Path) -> bool,
) -> bool {
    let Some(root) = configs
        .iter()
        .map(|config| config.path.as_path())
        .find(|root| path.starts_with(root))
    else {
        return false;
    };
    let mut dir = path.parent();
    while let Some(current) = dir {
        if marker_exists(&current.join(marker)) {
            return true;
        }
        if current == root {
            break;
        }
        dir = current.parent();
    }
    false
}

/// If `path` is a skip-marker file that was just *removed* from a watched directory, returns the
/// `(directory, recursive)` subtree to re-scan so files ignored while the marker existed get picked
/// up. Returns `None` when `path` isn't the marker, isn't inside a watched directory, or still
/// exists (a create/modify, not a delete). `exists` is injected for testability.
pub(crate) fn marker_removed_dir(
    configs: &[WatchedDirConfig],
    path: &Path,
    marker: &str,
    exists: impl Fn(&Path) -> bool,
) -> Option<(PathBuf, bool)> {
    if path.file_name().and_then(|name| name.to_str()) != Some(marker) {
        return None;
    }
    let parent = path.parent()?;
    let config = configs.iter().find(|config| {
        if config.recursive {
            path.starts_with(&config.path)
        } else {
            parent == config.path
        }
    })?;
    if exists(path) {
        return None;
    }
    Some((parent.to_path_buf(), config.recursive))
}

/// Reads a file's size and modification time in one stat. Returns `None` for missing paths or
/// non-files (e.g. directories), which the caller treats as "stop tracking".
fn stat_size_mtime(path: &Path) -> Option<(u64, SystemTime)> {
    let metadata = std::fs::metadata(path).ok()?;
    if !metadata.is_file() {
        return None;
    }
    Some((metadata.len(), metadata.modified().ok()?))
}

/// Owns the OS filesystem watcher and the bookkeeping shared between the event handler and the
/// reaper thread. Managed by Tauri for the lifetime of the app.
pub struct WatcherState {
    watcher: Mutex<Option<RecommendedWatcher>>,
    /// The `(path, recursive)` set currently armed on `watcher`, so `reconcile` can diff against it.
    watched: Mutex<Vec<(PathBuf, bool)>>,
    /// Files seen changing, awaiting stability. Shared with the event handler and reaper.
    pending: Arc<Mutex<HashMap<PathBuf, PendingEntry>>>,
    /// Current per-directory configs, read by the event handler on each event.
    configs: Arc<Mutex<Vec<WatchedDirConfig>>>,
    /// The active skip-marker filename (`None`/empty = feature off). Refreshed on reconcile and
    /// whenever the setting changes; read by the event handler and the enqueue filter.
    skip_marker: Arc<Mutex<Option<String>>>,
}

impl WatcherState {
    pub fn new() -> Self {
        Self {
            watcher: Mutex::new(None),
            watched: Mutex::new(Vec::new()),
            pending: Arc::new(Mutex::new(HashMap::new())),
            configs: Arc::new(Mutex::new(Vec::new())),
            skip_marker: Arc::new(Mutex::new(None)),
        }
    }
}

/// Builds the OS watcher. Its event handler is deliberately event-kind-agnostic: it never trusts
/// the event type (which differs across FSEvents/inotify/ReadDirectoryChangesW), only that
/// *something* touched a path, then (re)records it for the reaper to verify via stat.
fn build_watcher(
    pending: Arc<Mutex<HashMap<PathBuf, PendingEntry>>>,
    configs: Arc<Mutex<Vec<WatchedDirConfig>>>,
    skip_marker: Arc<Mutex<Option<String>>>,
    app: AppHandle,
) -> RecommendedWatcher {
    notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        let event = match res {
            Ok(event) => event,
            Err(_) => return,
        };
        let configs = match configs.lock() {
            Ok(configs) => configs,
            Err(_) => return,
        };
        let marker = skip_marker.lock().ok().and_then(|guard| guard.clone());
        let now = Instant::now();
        for path in &event.paths {
            // A removed skip marker means "this folder finished downloading" — re-scan its subtree
            // so files we ignored while the marker existed get enqueued. This is the only reason we
            // look at non-video paths at all.
            if let Some(marker) = marker.as_deref() {
                if let Some((dir, recursive)) =
                    marker_removed_dir(&configs, path, marker, |p| p.exists())
                {
                    scan_existing_background(&app, dir, recursive);
                    continue;
                }
            }
            let Some(delay) = delay_for_path(&configs, path) else {
                continue;
            };
            let Some((size, mtime)) = stat_size_mtime(path) else {
                continue;
            };
            if let Ok(mut pending) = pending.lock() {
                // Overwrite on every event: a fresh event means the file changed, so the
                // stability timer must restart from now.
                pending.insert(path.clone(), PendingEntry::new(size, mtime, now, delay));
            }
        }
    })
    .expect("failed to create filesystem watcher")
}

/// One reaper pass over the pending set: re-stat each tracked file via `stat`, drop files that
/// vanished, and collect + remove those that have settled (unchanged for their delay). Returns the
/// stabilized paths to enqueue. Generic over the stat function so a full tick is unit-testable
/// without the filesystem or the reaper's 1s sleep loop.
fn reap_pending_once(
    pending: &mut HashMap<PathBuf, PendingEntry>,
    now: Instant,
    stat: impl Fn(&Path) -> Option<(u64, SystemTime)>,
) -> Vec<String> {
    let mut stable: Vec<String> = Vec::new();
    pending.retain(|path, entry| match stat(path) {
        None => false, // gone → stop tracking
        Some((size, mtime)) => {
            if entry.observe(size, mtime, now) {
                if let Some(path) = path.to_str() {
                    stable.push(path.to_string());
                }
                false // settled → remove from pending
            } else {
                true // keep waiting
            }
        }
    });
    stable
}

/// Spawns the reaper: once a second it re-stats every pending file. Files that have settled are
/// enqueued; files that vanished are dropped. The stat re-check is the safety net for events the
/// OS coalesced or dropped.
fn spawn_reaper(app: AppHandle, pending: Arc<Mutex<HashMap<PathBuf, PendingEntry>>>) {
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_secs(1));
        let now = Instant::now();
        let stable = match pending.lock() {
            Ok(mut pending) => reap_pending_once(&mut pending, now, stat_size_mtime),
            Err(_) => Vec::new(),
        };
        if !stable.is_empty() {
            enqueue_and_start(&app, stable);
        }
    });
}

/// Drops paths that currently sit under an active skip marker (see `has_active_marker`). A no-op
/// when the feature is disabled. Applied at the single enqueue chokepoint so live events, startup
/// and enable scans, and marker-removal rescans all honor markers uniformly.
fn filter_marked(app: &AppHandle, paths: Vec<String>) -> Vec<String> {
    let state = app.state::<WatcherState>();
    let marker = match state.skip_marker.lock() {
        Ok(guard) => guard.clone(),
        Err(_) => return paths,
    };
    let Some(marker) = marker else {
        return paths;
    };
    let configs = match state.configs.lock() {
        Ok(configs) => configs.clone(),
        Err(_) => return paths,
    };
    paths
        .into_iter()
        .filter(|path| !has_active_marker(&configs, Path::new(path), &marker, |p| p.exists()))
        .collect()
}

/// Keep only paths still covered by a current watch config. Pure core of `filter_watched`,
/// split out so the coverage rule is unit-testable without a live `WatcherState`.
fn covered_paths(configs: &[WatchedDirConfig], paths: Vec<String>) -> Vec<String> {
    paths
        .into_iter()
        .filter(|p| delay_for_path(configs, Path::new(p)).is_some())
        .collect()
}

/// Drop paths no longer under any current watch. Closes the window where a background scan
/// (`scan_existing_background`) that started before the user removed/disabled the watch still
/// enqueues the folder's files — the detached scan thread bypasses `pending`, so the reconcile
/// purge can't reach it. Also hardens the reaper against the same-tick remove/stabilize race.
fn filter_watched(app: &AppHandle, paths: Vec<String>) -> Vec<String> {
    let state = app.state::<WatcherState>();
    let configs = match state.configs.lock() {
        Ok(configs) => configs.clone(),
        Err(_) => return paths,
    };
    covered_paths(&configs, paths)
}

/// Paths with an unpurged `bad_source`/`bad_source_truncated` row — the same set the "Bad
/// sources" review list shows (identical WHERE clause to `get_bad_sources_inner`). Fails open
/// (returns empty) on a DB error: a lock hiccup must not silently block a legitimate add.
fn unpurged_bad_source_paths(conn: &rusqlite::Connection) -> std::collections::HashSet<String> {
    let mut stmt = match conn.prepare(
        "SELECT source_path FROM jobs WHERE status = 'error' AND failure_class IN (?1, ?2)",
    ) {
        Ok(s) => s,
        Err(_) => return std::collections::HashSet::new(),
    };
    stmt.query_map(
        rusqlite::params![
            crate::failure_class::CLASS_BAD_SOURCE,
            crate::failure_class::CLASS_BAD_SOURCE_TRUNCATED,
        ],
        |row| row.get::<_, String>(0),
    )
    .map(|rows| rows.flatten().collect())
    .unwrap_or_default()
}

/// Pure core of `filter_known_bad_sources`: drop any path present in `bad`. Split out so the
/// rule is unit-testable without a live DB.
fn drop_known_bad_sources(
    bad: &std::collections::HashSet<String>,
    paths: Vec<String>,
) -> Vec<String> {
    paths.into_iter().filter(|p| !bad.contains(p)).collect()
}

/// Drop paths that already have an unpurged bad-source verdict. Without this, one corrupt file
/// sitting in a watched folder is re-ingested by EVERY launch's startup rescan (`reconcile` scans
/// every enabled folder unconditionally): a new `bad_source` row, a failed encode, and a "failed"
/// notification, forever — the review list grows unbounded with duplicates of the same file.
///
/// Applied ONLY at the watcher's enqueue chokepoint (`enqueue_and_start`), never from
/// `add_files_inner` itself: manual drag-and-drop must stay permissive, since re-adding a file by
/// hand is the user's deliberate retry mechanism (e.g. after fixing a permission problem or
/// confirming a "corrupt" file is actually fine).
fn filter_known_bad_sources(app: &AppHandle, paths: Vec<String>) -> Vec<String> {
    let app_state = app.state::<AppState>();
    let bad = match app_state.db.lock() {
        Ok(conn) => unpurged_bad_source_paths(&conn),
        Err(_) => return paths,
    };
    drop_known_bad_sources(&bad, paths)
}

/// The basename of the single directory a batch of paths shares, or empty when the batch spans
/// multiple directories (e.g. a recursive reaper batch). Used only to name the intake scanner in
/// the UI, so an empty fallback is harmless.
fn batch_label(paths: &[String]) -> String {
    let mut parents = paths.iter().map(|p| Path::new(p).parent());
    let first = match parents.next() {
        Some(Some(p)) => p,
        _ => return String::new(),
    };
    if parents.all(|p| p == Some(first)) {
        first
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_string()
    } else {
        String::new()
    }
}

/// Feeds stabilized paths through the same pipeline as drag-dropped files, then (auto-start)
/// kicks the queue and notifies the UI. `add_files_inner` applies all existing skip rules, so
/// already-converted or already-queued files are dropped here.
fn enqueue_and_start(app: &AppHandle, paths: Vec<String>) {
    let paths = filter_watched(app, paths);
    if paths.is_empty() {
        return;
    }
    let paths = filter_marked(app, paths);
    if paths.is_empty() {
        return;
    }
    let paths = filter_known_bad_sources(app, paths);
    if paths.is_empty() {
        return;
    }
    let app_state = app.state::<AppState>();
    let result = {
        let op = crate::add_progress::AddOp::new(app, batch_label(&paths));
        let reporter = |done: u32, total: u32| op.report(done, total);
        match queue::add_files_inner(&app_state, &paths, Some(&reporter as &dyn Fn(u32, u32))) {
            Ok(result) => result,
            Err(err) => {
                eprintln!("watcher: failed to enqueue {paths:?}: {err}");
                return;
            }
        }
        // `op` drops here → add-finished (also on the early return above).
    };
    if result.added.is_empty() {
        return;
    }
    let db = app_state.db.clone();
    let converter = (*app.state::<Arc<ConverterState>>()).clone();
    // An update install holds the queue interlock, so `run_queue` below would refuse — after the
    // paused flag had already been cleared, leaving the queue neither running nor paused. Bail
    // before touching it, exactly as `start_queue` does; the install re-triggers the queue when
    // it finishes (`resume_queue_after_install`). The files themselves are already enqueued, so
    // the UI still needs telling.
    if converter
        .installing
        .load(std::sync::atomic::Ordering::SeqCst)
    {
        let _ = app.emit("queue-updated", ());
        return;
    }
    // A watched-folder file arriving is an add; per the design, adding files starts the queue,
    // so clear any remembered pause before running.
    if let Ok(conn) = app_state.db.lock() {
        crate::converter::set_queue_paused(&conn, false);
    }
    converter::run_queue(app.clone(), db, converter);
    let _ = app.emit("queue-updated", ());
}

/// Reads the enabled watched directories from the DB into runtime configs. The stability delay
/// is floored at one second so a misconfigured zero can't enqueue files mid-write.
fn read_enabled_configs(app: &AppHandle) -> Result<Vec<WatchedDirConfig>, String> {
    let app_state = app.state::<AppState>();
    let conn = app_state.db.lock().map_err(|e| e.to_string())?;
    let mut stmt = conn
        .prepare(
            "SELECT path, recursive, stability_delay_secs FROM watched_directories WHERE enabled = 1",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |row| {
            let path: String = row.get(0)?;
            let recursive: bool = row.get(1)?;
            let delay_secs: i64 = row.get(2)?;
            Ok(WatchedDirConfig {
                path: PathBuf::from(path),
                recursive,
                delay: Duration::from_secs(delay_secs.max(1) as u64),
            })
        })
        .map_err(|e| e.to_string())?;
    Ok(rows.filter_map(|r| r.ok()).collect())
}

/// Validates a configured skip-marker value: it must be a single plain filename. Anything empty,
/// or containing a path separator / `.` / `..` (which would make `dir.join(marker)` resolve to a
/// different location and silently pass every file through), is rejected as "feature off".
/// `Path::file_name` makes this platform-correct (`\` is a separator on Windows, a filename char
/// on Unix).
fn valid_marker(value: &str) -> Option<String> {
    (Path::new(value).file_name() == Some(std::ffi::OsStr::new(value))).then(|| value.to_string())
}

/// Reads the `watch_skip_marker` setting, returning the marker only when it is a valid plain
/// filename. An empty, missing, or malformed value disables the skip-marker feature.
fn read_skip_marker(app: &AppHandle) -> Option<String> {
    let app_state = app.state::<AppState>();
    let conn = app_state.db.lock().ok()?;
    let value: String = conn
        .query_row(
            "SELECT value FROM settings WHERE key = 'watch_skip_marker'",
            [],
            |row| row.get(0),
        )
        .ok()?;
    valid_marker(&value)
}

/// Refreshes the watcher's cached skip-marker name from the DB. Called on reconcile and whenever
/// the setting changes, so the event handler and enqueue filter see the current value.
pub fn refresh_skip_marker(app: &AppHandle) {
    let marker = read_skip_marker(app);
    if let Ok(mut guard) = app.state::<WatcherState>().skip_marker.lock() {
        *guard = marker;
    }
}

/// Collects existing video files in `dir` (recursively when `recursive`), skipping temp/partial
/// files. Reuses the queue module's scanner so the recursive walk stays in one place.
fn collect_video_paths(dir: &Path, recursive: bool) -> Vec<String> {
    let paths: Vec<PathBuf> = if recursive {
        queue::scan_video_files(dir)
    } else {
        std::fs::read_dir(dir)
            .map(|entries| {
                entries
                    .flatten()
                    .map(|e| e.path())
                    .filter(|p| p.is_file() && queue::is_video_file(p))
                    .collect()
            })
            .unwrap_or_default()
    };
    paths
        .into_iter()
        .filter(|p| !is_temp_file(p))
        .filter_map(|p| p.to_str().map(|s| s.to_string()))
        .collect()
}

/// Enqueues files already present in `dir` when a watch is first enabled or on app start, so
/// downloads that landed while the app was closed aren't missed.
pub fn scan_existing(app: &AppHandle, dir: &Path, recursive: bool) {
    let paths = collect_video_paths(dir, recursive);
    if !paths.is_empty() {
        enqueue_and_start(app, paths);
    }
}

/// Background variant of `scan_existing` for Tauri command handlers, which run on the main thread.
/// The scan probes every existing file with a blocking `HandBrakeCLI --scan`, so scanning inline
/// freezes the UI when a folder holds many files (identical hazard to the initial scan in `start`).
/// Spawns the scan off-thread — the same proven-safe path the startup scan and reaper already use.
pub fn scan_existing_background(app: &AppHandle, dir: PathBuf, recursive: bool) {
    let app = app.clone();
    std::thread::spawn(move || scan_existing(&app, &dir, recursive));
}

/// Scans the existing contents of every enabled watched directory.
fn scan_all_enabled(app: &AppHandle) {
    if let Ok(configs) = read_enabled_configs(app) {
        for config in configs {
            scan_existing(app, &config.path, config.recursive);
        }
    }
}

/// Arms the OS watcher to exactly the set of enabled directories in the DB, adding/removing
/// watches as needed. Called on startup and after any change to the watched-directory config.
pub fn reconcile(app: &AppHandle) {
    let desired = match read_enabled_configs(app) {
        Ok(configs) => configs,
        Err(err) => {
            eprintln!("watcher: failed to read watched directories: {err}");
            return;
        }
    };
    let desired_tuples: Vec<(PathBuf, bool)> = desired
        .iter()
        .map(|config| (config.path.clone(), config.recursive))
        .collect();

    let state = app.state::<WatcherState>();
    if let Ok(mut configs) = state.configs.lock() {
        *configs = desired.clone();
    }
    refresh_skip_marker(app);

    let mut watched = match state.watched.lock() {
        Ok(watched) => watched,
        Err(_) => return,
    };
    let (to_unwatch, to_watch) = diff_watches(&watched, &desired_tuples);

    if let Ok(mut pending) = state.pending.lock() {
        purge_pending_uncovered(&mut pending, &desired);
    }

    if let Ok(mut guard) = state.watcher.lock() {
        if let Some(watcher) = guard.as_mut() {
            for path in &to_unwatch {
                let _ = watcher.unwatch(path);
            }
            for (path, recursive) in &to_watch {
                let mode = if *recursive {
                    RecursiveMode::Recursive
                } else {
                    RecursiveMode::NonRecursive
                };
                if let Err(err) = watcher.watch(path, mode) {
                    eprintln!("watcher: failed to watch {path:?}: {err}");
                }
            }
        }
    }
    *watched = desired_tuples;
}

/// Starts the watcher subsystem: builds the OS watcher, spawns the reaper, arms watches from the
/// DB, and scans existing contents. Call once during app setup.
pub fn start(app: AppHandle) {
    let (pending, configs, skip_marker) = {
        let state = app.state::<WatcherState>();
        (
            state.pending.clone(),
            state.configs.clone(),
            state.skip_marker.clone(),
        )
    };

    let watcher = build_watcher(pending.clone(), configs, skip_marker, app.clone());
    *app.state::<WatcherState>().watcher.lock().unwrap() = Some(watcher);

    spawn_reaper(app.clone(), pending);
    reconcile(&app);
    // The initial scan probes every existing file with a blocking `HandBrakeCLI --scan` (seconds
    // per file when skip-by-source-media is on). `start` runs inside Tauri's `setup` on the main
    // thread, so scanning inline freezes the UI at launch — the event loop never starts pumping.
    // Run it off-thread; the reaper already enqueues from a background thread, so this is the same
    // proven-safe path.
    std::thread::spawn(move || scan_all_enabled(&app));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mtime(secs: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(secs)
    }

    #[test]
    fn is_temp_file_matches_download_extensions_case_insensitively() {
        assert!(is_temp_file(Path::new("/d/movie.mp4.part")));
        assert!(is_temp_file(Path::new("/d/movie.mp4.crdownload")));
        assert!(is_temp_file(Path::new("/d/movie.mp4.CRDOWNLOAD")));
        assert!(is_temp_file(Path::new("/d/movie.mp4.download")));
        assert!(is_temp_file(Path::new("/d/movie.mkv.partial")));
        assert!(is_temp_file(Path::new("/d/movie.mp4.!ut")));

        // Real video files and extensionless paths are not temp files.
        assert!(!is_temp_file(Path::new("/d/movie.mp4")));
        assert!(!is_temp_file(Path::new("/d/movie.mkv")));
        assert!(!is_temp_file(Path::new("/d/README")));
    }

    #[test]
    fn observe_reports_stable_only_after_delay_with_no_change() {
        let t0 = Instant::now();
        let delay = Duration::from_secs(5);
        let mut entry = PendingEntry::new(100, mtime(10), t0, delay);

        // Unchanged but the delay has not elapsed yet → not stable.
        assert!(!entry.observe(100, mtime(10), t0 + Duration::from_secs(3)));
        // Unchanged and the delay has elapsed (since t0) → stable.
        assert!(entry.observe(100, mtime(10), t0 + Duration::from_secs(6)));
    }

    #[test]
    fn observe_resets_timer_when_size_changes() {
        let t0 = Instant::now();
        let delay = Duration::from_secs(5);
        let mut entry = PendingEntry::new(100, mtime(10), t0, delay);

        // File grew at +3s → not stable, and the timer restarts from +3s.
        assert!(!entry.observe(200, mtime(11), t0 + Duration::from_secs(3)));
        // +7s: only 4s since the last change → still not stable.
        assert!(!entry.observe(200, mtime(11), t0 + Duration::from_secs(7)));
        // +9s: 6s since the last change (at +3s) → stable.
        assert!(entry.observe(200, mtime(11), t0 + Duration::from_secs(9)));
    }

    #[test]
    fn observe_resets_timer_when_mtime_changes_even_if_size_is_equal() {
        let t0 = Instant::now();
        let delay = Duration::from_secs(5);
        let mut entry = PendingEntry::new(100, mtime(10), t0, delay);

        // Same size but newer mtime (e.g. a rewrite) at +4s → change, timer resets.
        assert!(!entry.observe(100, mtime(20), t0 + Duration::from_secs(4)));
        // +8s: only 4s since the change → not stable.
        assert!(!entry.observe(100, mtime(20), t0 + Duration::from_secs(8)));
        // +10s: 6s since the change → stable.
        assert!(entry.observe(100, mtime(20), t0 + Duration::from_secs(10)));
    }

    #[test]
    fn reap_pending_once_enqueues_a_settled_file_exactly_once() {
        // A file that has stopped changing for its full delay is enqueued once and dropped from
        // pending, so a later tick can't enqueue it again and re-convert a finished file.
        let t0 = Instant::now();
        let key = PathBuf::from("/watch/done.mp4");
        let mut pending = HashMap::new();
        pending.insert(
            key.clone(),
            PendingEntry::new(100, mtime(10), t0, Duration::from_secs(5)),
        );

        let stable = reap_pending_once(&mut pending, t0 + Duration::from_secs(6), |_| {
            Some((100, mtime(10)))
        });
        assert_eq!(stable, vec![key.to_string_lossy().to_string()]);
        assert!(pending.is_empty(), "a settled file is removed from pending");

        // A second tick has nothing left to enqueue — the file settled exactly once.
        let again = reap_pending_once(&mut pending, t0 + Duration::from_secs(12), |_| {
            Some((100, mtime(10)))
        });
        assert!(again.is_empty());
    }

    #[test]
    fn reap_pending_once_keeps_a_growing_file_pending() {
        // A file still being written (size keeps changing) must never be enqueued mid-write; the
        // change resets its stability timer and it stays pending even past the original delay.
        let t0 = Instant::now();
        let key = PathBuf::from("/watch/growing.mp4");
        let mut pending = HashMap::new();
        pending.insert(
            key.clone(),
            PendingEntry::new(100, mtime(10), t0, Duration::from_secs(5)),
        );

        let stable = reap_pending_once(&mut pending, t0 + Duration::from_secs(6), |_| {
            Some((200, mtime(10)))
        });
        assert!(
            stable.is_empty(),
            "a file that changed this tick is not enqueued"
        );
        assert!(
            pending.contains_key(&key),
            "it keeps waiting for stability instead of being dropped"
        );
    }

    #[test]
    fn reap_pending_once_drops_a_vanished_file() {
        // A file that disappeared before settling (deleted or renamed away) is dropped, not enqueued.
        let t0 = Instant::now();
        let key = PathBuf::from("/watch/gone.mp4");
        let mut pending = HashMap::new();
        pending.insert(
            key.clone(),
            PendingEntry::new(100, mtime(10), t0, Duration::from_secs(5)),
        );

        let stable = reap_pending_once(&mut pending, t0 + Duration::from_secs(6), |_| None);
        assert!(stable.is_empty());
        assert!(pending.is_empty(), "a vanished file stops being tracked");
    }

    #[test]
    fn diff_watches_adds_new_and_removes_gone() {
        let a = PathBuf::from("/a");
        let b = PathBuf::from("/b");
        let c = PathBuf::from("/c");

        let current = vec![(a.clone(), false), (b.clone(), false)];
        let desired = vec![(b.clone(), false), (c.clone(), false)];

        let (to_unwatch, to_watch) = diff_watches(&current, &desired);
        assert_eq!(to_unwatch, vec![a]); // /a removed
        assert_eq!(to_watch, vec![(c, false)]); // /c added; /b unchanged → neither
    }

    #[test]
    fn diff_watches_rewatches_when_recursive_mode_flips() {
        let a = PathBuf::from("/a");
        let current = vec![(a.clone(), false)];
        let desired = vec![(a.clone(), true)];

        let (to_unwatch, to_watch) = diff_watches(&current, &desired);
        // Same path, different mode → unwatch the old and re-watch recursively.
        assert_eq!(to_unwatch, vec![a.clone()]);
        assert_eq!(to_watch, vec![(a, true)]);
    }

    #[test]
    fn diff_watches_noop_when_identical() {
        let current = vec![(PathBuf::from("/a"), true)];
        let desired = vec![(PathBuf::from("/a"), true)];
        let (to_unwatch, to_watch) = diff_watches(&current, &desired);
        assert!(to_unwatch.is_empty());
        assert!(to_watch.is_empty());
    }

    fn pending_entry() -> PendingEntry {
        PendingEntry::new(
            1,
            SystemTime::UNIX_EPOCH,
            Instant::now(),
            Duration::from_secs(1),
        )
    }

    #[test]
    fn purge_pending_uncovered_drops_files_no_desired_config_covers() {
        // A file mid-stabilization when its watch is removed must NOT be enqueued once it
        // settles — otherwise removing a watch doesn't fully take effect.
        let mut pending: HashMap<PathBuf, PendingEntry> = HashMap::new();
        let doomed = PathBuf::from("/removed/a.mp4");
        let survivor = PathBuf::from("/kept/b.mp4");
        pending.insert(doomed.clone(), pending_entry());
        pending.insert(survivor.clone(), pending_entry());

        // Only /kept remains a desired watch.
        purge_pending_uncovered(&mut pending, &[config("/kept", false, 1)]);

        assert!(
            !pending.contains_key(&doomed),
            "a file under no desired watch is dropped"
        );
        assert!(
            pending.contains_key(&survivor),
            "a file under a still-desired watch keeps stabilizing"
        );
    }

    #[test]
    fn purge_pending_uncovered_retains_files_under_a_still_active_nested_watch() {
        // The overreach fix: removing the enclosing /w watch must NOT drop a file under a
        // still-active nested /w/sub watch. If it already finished writing, no further FS event
        // would re-add it, so it would be silently lost until a rescan. The old "under a removed
        // root" purge dropped it because /w/sub/c.mp4 starts_with /w.
        let mut pending: HashMap<PathBuf, PendingEntry> = HashMap::new();
        let nested = PathBuf::from("/w/sub/c.mp4");
        pending.insert(nested.clone(), pending_entry());

        // /w was removed; only the nested /w/sub watch remains.
        purge_pending_uncovered(&mut pending, &[config("/w/sub", true, 1)]);

        assert!(
            pending.contains_key(&nested),
            "a file under the still-active nested watch survives removal of the enclosing watch"
        );
    }

    #[test]
    fn covered_paths_keeps_only_files_under_a_current_watch() {
        // enqueue_and_start uses this so a background scan whose watch was removed mid-scan
        // enqueues nothing from that folder (the detached scan thread bypasses `pending`).
        let configs = vec![config("/watch", true, 5)];
        let kept = covered_paths(
            &configs,
            vec!["/watch/keep.mp4".to_string(), "/gone/drop.mp4".to_string()],
        );
        assert_eq!(kept, vec!["/watch/keep.mp4".to_string()]);
    }

    fn config(path: &str, recursive: bool, delay_secs: u64) -> WatchedDirConfig {
        WatchedDirConfig {
            path: PathBuf::from(path),
            recursive,
            delay: Duration::from_secs(delay_secs),
        }
    }

    #[test]
    fn delay_for_path_matches_top_level_video_and_returns_its_delay() {
        let configs = vec![config("/watch", false, 7)];
        assert_eq!(
            delay_for_path(&configs, Path::new("/watch/movie.mp4")),
            Some(Duration::from_secs(7))
        );
    }

    #[test]
    fn delay_for_path_ignores_subfolders_unless_recursive() {
        let nonrec = vec![config("/watch", false, 5)];
        // A file in a subfolder is ignored when the directory is not recursive.
        assert_eq!(
            delay_for_path(&nonrec, Path::new("/watch/sub/movie.mp4")),
            None
        );

        let rec = vec![config("/watch", true, 5)];
        // The same file is picked up when the directory is recursive.
        assert_eq!(
            delay_for_path(&rec, Path::new("/watch/sub/movie.mp4")),
            Some(Duration::from_secs(5))
        );
    }

    #[test]
    fn delay_for_path_ignores_temp_and_non_video_files() {
        let configs = vec![config("/watch", true, 5)];
        assert_eq!(
            delay_for_path(&configs, Path::new("/watch/movie.mp4.part")),
            None
        );
        assert_eq!(
            delay_for_path(&configs, Path::new("/watch/notes.txt")),
            None
        );
    }

    #[test]
    fn delay_for_path_ignores_paths_outside_every_watched_dir() {
        let configs = vec![config("/watch", true, 5)];
        assert_eq!(
            delay_for_path(&configs, Path::new("/elsewhere/movie.mp4")),
            None
        );
    }

    /// Builds an `exists` closure that reports the given absolute paths as present. Comparison is
    /// separator-normalized: `has_active_marker` builds candidates with `Path::join`, which emits
    /// `\` on Windows, but the fixtures below are written with `/`.
    fn present(paths: &'static [&'static str]) -> impl Fn(&Path) -> bool {
        move |p: &Path| {
            p.to_str()
                .map(|s| paths.contains(&s.replace('\\', "/").as_str()))
                .unwrap_or(false)
        }
    }

    #[test]
    fn has_active_marker_true_when_marker_in_files_own_dir() {
        let configs = vec![config("/watch", true, 5)];
        assert!(has_active_marker(
            &configs,
            Path::new("/watch/sub/movie.mp4"),
            ".downloading",
            present(&["/watch/sub/.downloading"]),
        ));
    }

    #[test]
    fn has_active_marker_true_when_marker_in_parent_subfolder() {
        // Recursive watch: the marker sits above the video but still inside the watched tree, so
        // the whole subtree under it is ignored.
        let configs = vec![config("/watch", true, 5)];
        assert!(has_active_marker(
            &configs,
            Path::new("/watch/sub/deeper/movie.mp4"),
            ".downloading",
            present(&["/watch/sub/.downloading"]),
        ));
    }

    #[test]
    fn has_active_marker_true_when_marker_at_watched_root() {
        let configs = vec![config("/watch", true, 5)];
        assert!(has_active_marker(
            &configs,
            Path::new("/watch/sub/movie.mp4"),
            ".downloading",
            present(&["/watch/.downloading"]),
        ));
    }

    #[test]
    fn has_active_marker_false_without_any_marker() {
        let configs = vec![config("/watch", true, 5)];
        assert!(!has_active_marker(
            &configs,
            Path::new("/watch/sub/movie.mp4"),
            ".downloading",
            |_| false,
        ));
    }

    #[test]
    fn has_active_marker_stops_at_watched_root_and_ignores_markers_above_it() {
        // A file named like the marker above the watched root must not gate files inside it.
        let configs = vec![config("/watch", true, 5)];
        assert!(!has_active_marker(
            &configs,
            Path::new("/watch/movie.mp4"),
            ".downloading",
            present(&["/.downloading"]),
        ));
    }

    #[test]
    fn has_active_marker_false_for_path_outside_watched_dirs() {
        let configs = vec![config("/watch", true, 5)];
        // Even if markers "existed" everywhere, a path in no watched dir is never gated.
        assert!(!has_active_marker(
            &configs,
            Path::new("/elsewhere/movie.mp4"),
            ".downloading",
            |_| true,
        ));
    }

    #[test]
    fn marker_removed_dir_none_when_filename_is_not_the_marker() {
        let configs = vec![config("/watch", true, 5)];
        assert_eq!(
            marker_removed_dir(
                &configs,
                Path::new("/watch/movie.mp4"),
                ".downloading",
                |_| false
            ),
            None
        );
    }

    #[test]
    fn marker_removed_dir_returns_subtree_when_deleted_in_recursive_subfolder() {
        let configs = vec![config("/watch", true, 5)];
        // Marker gone inside a recursive watch's subfolder → re-scan that subfolder recursively.
        assert_eq!(
            marker_removed_dir(
                &configs,
                Path::new("/watch/sub/.downloading"),
                ".downloading",
                |_| false
            ),
            Some((PathBuf::from("/watch/sub"), true))
        );
    }

    #[test]
    fn marker_removed_dir_none_when_marker_still_present() {
        let configs = vec![config("/watch", true, 5)];
        // exists = true → a create/modify, not a delete; nothing to reprocess yet.
        assert_eq!(
            marker_removed_dir(
                &configs,
                Path::new("/watch/sub/.downloading"),
                ".downloading",
                |_| true
            ),
            None
        );
    }

    #[test]
    fn marker_removed_dir_handles_non_recursive_root_marker() {
        let configs = vec![config("/watch", false, 5)];
        assert_eq!(
            marker_removed_dir(
                &configs,
                Path::new("/watch/.downloading"),
                ".downloading",
                |_| false
            ),
            Some((PathBuf::from("/watch"), false))
        );
    }

    #[test]
    fn marker_removed_dir_none_for_non_recursive_subfolder_marker() {
        // A non-recursive watch never observes its subfolders, so a marker there isn't ours.
        let configs = vec![config("/watch", false, 5)];
        assert_eq!(
            marker_removed_dir(
                &configs,
                Path::new("/watch/sub/.downloading"),
                ".downloading",
                |_| false
            ),
            None
        );
    }

    #[test]
    fn marker_removed_dir_none_outside_watched_dirs() {
        let configs = vec![config("/watch", true, 5)];
        assert_eq!(
            marker_removed_dir(
                &configs,
                Path::new("/elsewhere/.downloading"),
                ".downloading",
                |_| false
            ),
            None
        );
    }

    #[test]
    fn valid_marker_accepts_plain_filenames() {
        assert_eq!(
            valid_marker(".downloading"),
            Some(".downloading".to_string())
        );
        assert_eq!(valid_marker("in-progress"), Some("in-progress".to_string()));
    }

    #[test]
    fn valid_marker_rejects_empty_separators_and_dot_paths() {
        // Empty disables the feature; a value with a separator would make `dir.join(marker)`
        // resolve elsewhere and silently pass every file, so it must be rejected too.
        assert_eq!(valid_marker(""), None);
        assert_eq!(valid_marker("sub/.downloading"), None);
        assert_eq!(valid_marker(".downloading/"), None);
        assert_eq!(valid_marker(".."), None);
        assert_eq!(valid_marker("."), None);
    }

    #[test]
    fn batch_label_names_a_single_dir_batch_and_empties_a_mixed_one() {
        assert_eq!(
            super::batch_label(&["/movies/SEOA/a.mp4".into(), "/movies/SEOA/b.mp4".into()]),
            "SEOA",
            "all-same-parent batch takes the parent's basename"
        );
        assert_eq!(
            super::batch_label(&["/movies/SEOA/a.mp4".into(), "/movies/Other/b.mp4".into()]),
            "",
            "a multi-directory batch has no single name"
        );
        assert_eq!(super::batch_label(&[]), "", "empty batch → empty label");
    }

    // ---- F8: the watcher must not re-ingest a classified bad source forever ----

    #[test]
    fn drop_known_bad_sources_removes_only_paths_in_the_bad_set() {
        let bad: std::collections::HashSet<String> =
            ["/w/corrupt.mkv".to_string()].into_iter().collect();
        let paths = vec!["/w/corrupt.mkv".to_string(), "/w/healthy.mkv".to_string()];
        assert_eq!(
            drop_known_bad_sources(&bad, paths),
            vec!["/w/healthy.mkv".to_string()],
            "only the path with an unpurged bad-source verdict is dropped"
        );
    }

    #[test]
    fn unpurged_bad_source_paths_matches_the_bad_sources_review_lists_query() {
        // Deliberately the same fixture shape as
        // get_bad_sources_lists_both_bad_classes_and_excludes_everything_else in
        // commands/queue.rs, so this stays in lockstep with what the review list itself shows:
        // a purged row, a live 'done' row, and an environment/unknown failure must all still be
        // re-ingestible, while an unpurged bad_source or bad_source_truncated row must not.
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::db::init_db(&conn).unwrap();
        for (id, source_path, status, class) in [
            ("a", "/w/bad.mkv", "error", Some("bad_source")),
            ("b", "/w/trunc.mkv", "error", Some("bad_source_truncated")),
            ("c", "/w/env.mkv", "error", Some("environment")),
            ("d", "/w/purged.mkv", "error", Some("bad_source_purged")),
            ("e", "/w/done.mkv", "done", Some("bad_source")),
        ] {
            conn.execute(
                "INSERT INTO jobs (id, source_path, output_path, preset, status, failure_class,
                                   queue_order, created_at, completed_at)
                 VALUES (?1, ?2, '/o.mp4', 'p', ?3, ?4, 0, '2026-07-25T10:00:00Z',
                         '2026-07-25T10:00:00Z')",
                rusqlite::params![id, source_path, status, class],
            )
            .unwrap();
        }
        let bad = unpurged_bad_source_paths(&conn);
        assert_eq!(
            bad,
            ["/w/bad.mkv".to_string(), "/w/trunc.mkv".to_string()]
                .into_iter()
                .collect(),
            "only unpurged bad_source/bad_source_truncated rows must be treated as known-bad"
        );
    }
}
