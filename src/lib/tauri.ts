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

export interface PresetMetadata {
  codec: string;
  resolution: string;
  quality: string;
  preset: string;
  device: string;
}

export const commands = {
  addFiles: (paths: string[]) => invoke<JobInfo[]>("add_files", { paths }),
  scanFolder: (path: string) =>
    invoke<FolderScanResult>("scan_folder", { path }),
  confirmFolderAdd: (path: string) =>
    invoke<JobInfo[]>("confirm_folder_add", { path }),
  getQueue: () => invoke<JobInfo[]>("get_queue"),
  removeJob: (id: string) => invoke<void>("remove_job", { id }),
  reorderQueue: (jobIds: string[]) =>
    invoke<void>("reorder_queue", { jobIds }),
  clearCompleted: (mode: string) => invoke<void>("clear_completed", { mode }),
  startQueue: () => invoke<void>("start_queue"),
  pauseConversion: () => invoke<void>("pause_conversion"),
  resumeConversion: () => invoke<void>("resume_conversion"),
  cancelConversion: () => invoke<void>("cancel_conversion"),
  getHistory: (limit: number, offset: number) =>
    invoke<HistoryPage>("get_history", { limit, offset }),
  getHistorySummary: () => invoke<HistorySummary>("get_history_summary"),
  getSettings: () => invoke<AppSettings>("get_settings"),
  updateSetting: (key: string, value: string) =>
    invoke<void>("update_setting", { key, value }),
  getPresetSuffix: (preset: string) =>
    invoke<string | null>("get_preset_suffix", { preset }),
  setPresetSuffix: (preset: string, suffix: string) =>
    invoke<void>("set_preset_suffix", { preset, suffix }),
  listHandbrakePresets: () => invoke<string[]>("list_handbrake_presets"),
  detectHandbrake: () => invoke<string | null>("detect_handbrake"),
  classifyPaths: (paths: string[]) =>
    invoke<ClassifiedPaths>("classify_paths", { paths }),
  clearQueue: () => invoke<void>("clear_queue"),
  generatePresetSuffix: (preset: string) =>
    invoke<PresetMetadata>("generate_preset_suffix", { preset }),
  pauseAfterCurrent: () => invoke<void>("pause_after_current"),
  cancelPauseAfterCurrent: () => invoke<void>("cancel_pause_after_current"),
  validateHandbrake: () => invoke<HandbrakeStatus>("validate_handbrake"),
  quitApp: () => invoke<void>("quit_app"),
  hideWindow: () => getCurrentWebviewWindow().hide(),
};
