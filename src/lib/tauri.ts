import { invoke } from "@tauri-apps/api/core";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";

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
  queue_order: number;
  created_at: string;
  completed_at: string | null;
}

export type SkipReason =
  | "not_video"
  | "already_queued"
  | "already_converted"
  | "output_exists"
  | "already_at_target";

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

export const commands = {
  addFiles: (paths: string[]) => invoke<AddResult>("add_files", { paths }),
  scanFolder: (path: string) =>
    invoke<FolderScanResult>("scan_folder", { path }),
  confirmFolderAdd: (path: string) =>
    invoke<AddResult>("confirm_folder_add", { path }),
  getQueue: () => invoke<JobInfo[]>("get_queue"),
  removeJob: (id: string) => invoke<void>("remove_job", { id }),
  reorderQueue: (jobIds: string[]) =>
    invoke<void>("reorder_queue", { jobIds }),
  clearCompleted: (mode: string) => invoke<void>("clear_completed", { mode }),
  startQueue: () => invoke<void>("start_queue"),
  pauseConversion: () => invoke<void>("pause_conversion"),
  resumeConversion: () => invoke<void>("resume_conversion"),
  cancelConversion: () => invoke<void>("cancel_conversion"),
  getHistory: (limit: number, offset: number, search?: string, sortBy?: string) =>
    invoke<HistoryPage>("get_history", { limit, offset, search: search || null, sortBy: sortBy || null }),
  getHistorySummary: (search?: string) =>
    invoke<HistorySummary>("get_history_summary", { search: search || null }),
  removeHistoryEntry: (id: string) =>
    invoke<void>("remove_history_entry", { id }),
  checkPathsExist: (sourcePath: string, outputPath: string) =>
    invoke<PathsExist>("check_paths_exist", { sourcePath, outputPath }),
  openPath: (path: string) => invoke<void>("open_path", { path }),
  revealInDir: (path: string) => invoke<void>("reveal_in_dir", { path }),
  getSettings: () => invoke<AppSettings>("get_settings"),
  updateSetting: (key: string, value: string) =>
    invoke<void>("update_setting", { key, value }),
  getPresetSuffix: (preset: string) =>
    invoke<string>("get_preset_suffix", { preset }),
  setPresetSuffix: (preset: string, suffix: string) =>
    invoke<void>("set_preset_suffix", { preset, suffix }),
  resolveSuffixTemplate: (template: string, metadata: PresetMetadata) =>
    invoke<string>("resolve_suffix_template", { template, metadata }),
  listHandbrakePresets: () => invoke<string[]>("list_handbrake_presets"),
  detectHandbrake: () => invoke<string | null>("detect_handbrake"),
  classifyPaths: (paths: string[]) =>
    invoke<ClassifiedPaths>("classify_paths", { paths }),
  clearQueue: () => invoke<void>("clear_queue"),
  generatePresetSuffix: (preset: string) =>
    invoke<PresetMetadata>("generate_preset_suffix", { preset }),
  pauseAfterCurrent: () => invoke<void>("pause_after_current"),
  cancelPauseAfterCurrent: () => invoke<void>("cancel_pause_after_current"),
  getPauseAfterCurrent: () => invoke<boolean>("get_pause_after_current"),
  getLowDiskPause: () => invoke<LowDiskPause | null>("get_low_disk_pause"),
  getPlatformCapabilities: () =>
    invoke<PlatformCapabilities>("get_platform_capabilities"),
  validateHandbrake: () => invoke<HandbrakeStatus>("validate_handbrake"),
  quitApp: () => invoke<void>("quit_app"),
  hideWindow: () => getCurrentWebviewWindow().hide(),
  getWatchedDirectories: () =>
    invoke<WatchedDirectory[]>("get_watched_directories"),
  addWatchedDirectory: (
    path: string,
    recursive: boolean,
    stabilityDelaySecs: number,
  ) =>
    invoke<WatchedDirectory>("add_watched_directory", {
      path,
      recursive,
      stabilityDelaySecs,
    }),
  updateWatchedDirectory: (
    id: string,
    recursive: boolean,
    stabilityDelaySecs: number,
  ) =>
    invoke<void>("update_watched_directory", {
      id,
      recursive,
      stabilityDelaySecs,
    }),
  setWatchedDirectoryEnabled: (id: string, enabled: boolean) =>
    invoke<void>("set_watched_directory_enabled", { id, enabled }),
  removeWatchedDirectory: (id: string) =>
    invoke<void>("remove_watched_directory", { id }),
  pickFolder: () => invoke<string | null>("pick_folder"),
};
