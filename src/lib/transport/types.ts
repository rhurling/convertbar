import type { tauriCommands } from "./tauri";

export interface JobInfo {
  id: string;
  source_path: string;
  output_path: string;
  preset: string;
  status: "queued" | "encoding" | "paused" | "done" | "error" | "skipped";
  original_size: number | null;
  converted_size: number | null;
  kept_file: "original" | "converted" | null;
  space_saved: number | null;
  error_message: string | null;
  failure_class: string | null;
  queue_order: number;
  created_at: string;
  completed_at: string | null;
}

export type SkipReason =
  | "not_video"
  | "already_queued"
  | "already_converted"
  | "output_exists"
  | "already_at_target"
  | "in_place_keep_blocked";

export interface SkipCount {
  reason: SkipReason;
  count: number;
}

export interface AddResult {
  added: JobInfo[];
  skipped: SkipCount[];
}

export interface FolderScanResult {
  file_count: number;
  folder_name: string;
  folder_path: string;
}

export interface ConversionProgress {
  job_id: string;
  percent: number;
  eta_seconds: number;
  fps: number;
  avg_fps: number;
}

export interface AddStarted {
  op_id: string;
  label: string;
}

export interface AddProgress {
  op_id: string;
  label: string;
  done: number;
  total: number;
}

export interface AddFinished {
  op_id: string;
}

// Frontend view of the current add operation. `done`/`total` are null during the
// indeterminate scan phase (before the first per-file probe tick). `label` is the folder
// name (empty for loose-file adds).
export interface AddActivity {
  opId: string;
  label: string;
  done: number | null;
  total: number | null;
}

export interface AppSettings {
  preset: string;
  cleanup_mode: string;
  launch_at_login: boolean;
  handbrake_path: string;
  menubar_show_percent: boolean;
  menubar_show_eta: boolean;
  menubar_show_queue: boolean;
  menubar_show_filename: boolean;
  menubar_show_fps: boolean;
  notifications_per_file: boolean;
  notifications_errors_only: boolean;
  notifications_queue_done: boolean;
  skip_already_converted: boolean;
  skip_by_source_media: boolean;
  watch_skip_marker: string;
  low_disk_min_gb: number;
  bad_source_action: "trash" | "delete";
  // Narrowed like bad_source_action: get_settings runs the raw stored string through
  // `normalize_update_mode` before returning it, so only these three ever reach the frontend.
  update_mode: UpdateMode;
}

export interface PathsExist {
  source_exists: boolean;
  output_exists: boolean;
}

export interface HistorySummary {
  total_saved_bytes: number;
  total_files: number;
}

export interface HistoryPage {
  jobs: JobInfo[];
  total: number;
}

// Mirrors src-tauri/src/types.rs PurgeOutcome exactly — every variant but "purged" means the
// file was left alone, and the UI must report that honestly rather than implying destruction.
export type PurgeOutcome =
  | "purged"
  | "in_use"
  | "already_gone"
  | "changed"
  | "recovered"
  | "unverifiable"
  | "failed";

export interface PurgeResult {
  id: string;
  outcome: PurgeOutcome;
}

export interface ClassifiedPaths {
  files: string[];
  folders: FolderScanResult[];
}

export interface HandbrakeStatus {
  found: boolean;
  path: string;
  version: string;
}

export interface PlatformCapabilities {
  can_pause_process: boolean;
}

export interface LowDiskPause {
  path: string;
  available_bytes: number;
  required_bytes: number;
}

export interface PresetMetadata {
  codec: string;
  resolution: string;
  quality: string;
  preset: string;
  device: string;
}

export interface WatchedDirectory {
  id: string;
  path: string;
  recursive: boolean;
  stability_delay_secs: number;
  enabled: boolean;
  created_at: string;
}

export type UpdateMode = "automatic" | "notify" | "off";

export type UpdateStatus =
  | "idle"
  | "checking"
  | "available"
  | "downloading"
  | "waitingForIdle"
  | "readyToRestart"
  | "error";

export interface AvailableUpdate {
  version: string;
  date: string | null;
  notes: string | null;
}

export interface InstalledUpdate {
  version: string;
  notes: string | null;
}

// Mirrors src-tauri/src/updater.rs UpdateState exactly.
export interface UpdateState {
  mode: UpdateMode;
  status: UpdateStatus;
  current_version: string;
  available: AvailableUpdate | null;
  just_installed: InstalledUpdate | null;
  last_checked: number | null;
  last_error: string | null;
}

// The command surface both transports implement. Derived from the existing `commands`
// object so the desktop shape is definitionally authoritative:
export type Transport = typeof tauriCommands;

// New in this plan (both transports implement):
export interface AppInfo {
  version: string;
  head: "desktop" | "server";
  can_pause_process: boolean;
  auth_required: boolean;
  // Confines the file-browser modal's starting path and up-navigation. Always empty on
  // desktop (no browse roots there); on server it mirrors ServerConfig::browse_roots.
  browse_roots: string[];
}

// Server-only (file browser; desktop never calls these):
export interface FsEntry {
  name: string;
  path: string;
  is_dir: boolean;
  size: number | null;
}

export interface FsListResult {
  entries: FsEntry[];
}
