import { invoke } from "@tauri-apps/api/core";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import { getVersion } from "@tauri-apps/api/app";
import type {
  AddResult,
  AppInfo,
  AppSettings,
  ClassifiedPaths,
  FolderScanResult,
  HandbrakeStatus,
  HistoryPage,
  HistorySummary,
  JobInfo,
  LowDiskPause,
  PathsExist,
  PlatformCapabilities,
  PresetMetadata,
  PurgeResult,
  WatchedDirectory,
} from "./types";

export const tauriCommands = {
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
  getBadSources: () => invoke<JobInfo[]>("get_bad_sources"),
  purgeBadSources: (ids: string[]) =>
    invoke<PurgeResult[]>("purge_bad_sources", { ids }),
  // Synthesized desktop-side: the server transport gets this straight from its own
  // /api/app-info endpoint, but desktop has no single command for it, so we compose it
  // from the app's own version plus the existing platform-capabilities probe.
  getAppInfo: async (): Promise<AppInfo> => {
    const [version, caps] = await Promise.all([
      getVersion(),
      invoke<PlatformCapabilities>("get_platform_capabilities"),
    ]);
    return {
      version,
      head: "desktop",
      can_pause_process: caps.can_pause_process,
      auth_required: false,
    };
  },
};
