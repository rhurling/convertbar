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
}

impl WatcherState {
    pub fn new() -> Self {
        Self {
            watcher: Mutex::new(None),
            watched: Mutex::new(Vec::new()),
            pending: Arc::new(Mutex::new(HashMap::new())),
            configs: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

/// Builds the OS watcher. Its event handler is deliberately event-kind-agnostic: it never trusts
/// the event type (which differs across FSEvents/inotify/ReadDirectoryChangesW), only that
/// *something* touched a path, then (re)records it for the reaper to verify via stat.
fn build_watcher(
    pending: Arc<Mutex<HashMap<PathBuf, PendingEntry>>>,
    configs: Arc<Mutex<Vec<WatchedDirConfig>>>,
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
        let now = Instant::now();
        for path in &event.paths {
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

/// Spawns the reaper: once a second it re-stats every pending file. Files that have settled are
/// enqueued; files that vanished are dropped. The stat re-check is the safety net for events the
/// OS coalesced or dropped.
fn spawn_reaper(app: AppHandle, pending: Arc<Mutex<HashMap<PathBuf, PendingEntry>>>) {
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_secs(1));
        let now = Instant::now();
        let mut stable: Vec<String> = Vec::new();
        if let Ok(mut pending) = pending.lock() {
            pending.retain(|path, entry| match stat_size_mtime(path) {
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
        }
        if !stable.is_empty() {
            enqueue_and_start(&app, stable);
        }
    });
}

/// Feeds stabilized paths through the same pipeline as drag-dropped files, then (auto-start)
/// kicks the queue and notifies the UI. `add_files_inner` applies all existing skip rules, so
/// already-converted or already-queued files are dropped here.
fn enqueue_and_start(app: &AppHandle, paths: Vec<String>) {
    let app_state = app.state::<AppState>();
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
    let db = app_state.db.clone();
    let converter = (*app.state::<Arc<ConverterState>>()).clone();
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

    let mut watched = match state.watched.lock() {
        Ok(watched) => watched,
        Err(_) => return,
    };
    let (to_unwatch, to_watch) = diff_watches(&watched, &desired_tuples);

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
    let (pending, configs) = {
        let state = app.state::<WatcherState>();
        (state.pending.clone(), state.configs.clone())
    };

    let watcher = build_watcher(pending.clone(), configs);
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
}
