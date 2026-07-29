use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobInfo {
    pub id: String,
    pub source_path: String,
    pub output_path: String,
    pub preset: String,
    pub status: String,
    pub original_size: Option<i64>,
    pub converted_size: Option<i64>,
    pub kept_file: Option<String>,
    pub space_saved: Option<i64>,
    pub error_message: Option<String>,
    pub failure_class: Option<String>,
    pub queue_order: i32,
    pub created_at: String,
    pub completed_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    pub preset: String,
    pub cleanup_mode: String,
    pub launch_at_login: bool,
    pub handbrake_path: String,
    pub menubar_show_percent: bool,
    pub menubar_show_eta: bool,
    pub menubar_show_queue: bool,
    pub menubar_show_filename: bool,
    pub menubar_show_fps: bool,
    pub notifications_per_file: bool,
    pub notifications_errors_only: bool,
    pub notifications_queue_done: bool,
    pub skip_already_converted: bool,
    pub skip_by_source_media: bool,
    pub watch_skip_marker: String,
    pub low_disk_min_gb: f64,
    pub bad_source_action: String,
    pub update_mode: String,
}

/// Existence of a history entry's two paths, checked when its context menu opens.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PathsExist {
    pub source_exists: bool,
    pub output_exists: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistorySummary {
    pub total_saved_bytes: i64,
    pub total_files: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryPage {
    pub jobs: Vec<JobInfo>,
    pub total: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FolderScanResult {
    pub file_count: usize,
    pub folder_name: String,
    pub folder_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClassifiedPaths {
    pub files: Vec<String>,
    pub folders: Vec<FolderScanResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandbrakeStatus {
    pub found: bool,
    pub path: String,
    pub version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WatchedDirectory {
    pub id: String,
    pub path: String,
    pub recursive: bool,
    pub stability_delay_secs: i64,
    pub enabled: bool,
    pub created_at: String,
}

/// Why a dropped/scanned path was not queued. Surfaced at add time and counted per reason;
/// never written to history.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SkipCount {
    pub reason: SkipReason,
    pub count: u32,
}

/// Result of an add operation: the jobs actually queued, plus per-reason counts of paths skipped.
/// `Default` is the empty result — `add_files_inner` returns it for an intake with no paths.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AddResult {
    pub added: Vec<JobInfo>,
    pub skipped: Vec<SkipCount>,
}

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
    /// A `bad_source` verdict requires a rescan to confirm before destroying, and the rescan
    /// could not be *performed* — no HandBrakeCLI on the configured path, a spawn failure, or a
    /// scan timeout. Distinct from a scan that ran and found no title, which is a real verdict
    /// and does destroy: see `probe::ScanOutcome`. Left alone pending a retry.
    Unverifiable,
    /// The delete/trash call itself failed.
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PurgeResult {
    pub id: String,
    pub outcome: PurgeOutcome,
}
