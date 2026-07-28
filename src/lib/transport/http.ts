// The server-head Transport implementation: every command is a `fetch` call against the
// axum routes in `crates/convertbar-server/routes.json` (the source of truth for method+path).
import type {
  AddResult,
  AppInfo,
  AppSettings,
  ClassifiedPaths,
  FolderScanResult,
  FsListResult,
  HandbrakeStatus,
  HistoryPage,
  HistorySummary,
  JobInfo,
  LowDiskPause,
  PresetMetadata,
  PurgeResult,
  Transport,
  WatchedDirectory,
} from "./types";

const api = async <T>(method: string, path: string, body?: unknown): Promise<T> => {
  const res = await fetch(path, {
    method,
    // ALWAYS send the JSON content-type on mutating methods, even with no body:
    // the server's CSRF guard requires it (bodyless POSTs like /api/converter/start
    // would 415 otherwise), and it is harmless on an empty body.
    headers: method === "GET" ? {} : { "Content-Type": "application/json" },
    body: body !== undefined ? JSON.stringify(body) : undefined,
    credentials: "same-origin",
  });
  if (res.status === 401) { window.dispatchEvent(new Event("convertbar:unauthorized")); throw new Error("unauthorized"); }
  if (!res.ok) throw new Error((await res.json().catch(() => ({}))).error ?? `HTTP ${res.status}`);
  return res.status === 204 ? (undefined as T) : res.json();
};

// Desktop-only members: the server UI never calls them (Task 12 gates their callers). The
// throw is the tripwire if that gating ever regresses.
const notAvailable = (): never => {
  throw new Error("not available on server");
};

export const httpCommands = {
  addFiles: (paths: string[]): Promise<AddResult> =>
    api("POST", "/api/queue/files", { paths }),
  scanFolder: (path: string): Promise<FolderScanResult> =>
    api("POST", "/api/folders/scan", { path }),
  confirmFolderAdd: (path: string): Promise<AddResult> =>
    api("POST", "/api/queue/folder", { path }),
  getQueue: (): Promise<JobInfo[]> => api("GET", "/api/queue"),
  removeJob: (id: string): Promise<void> =>
    api("DELETE", `/api/queue/jobs/${encodeURIComponent(id)}`),
  reorderQueue: (jobIds: string[]): Promise<void> =>
    api("PUT", "/api/queue/order", { jobIds }),
  clearCompleted: (mode: string): Promise<void> =>
    api("POST", "/api/history/clear", { mode }),
  startQueue: (): Promise<void> => api("POST", "/api/converter/start"),
  pauseConversion: (): Promise<void> => api("POST", "/api/converter/pause"),
  resumeConversion: (): Promise<void> => api("POST", "/api/converter/resume"),
  cancelConversion: (): Promise<void> => api("POST", "/api/converter/cancel"),
  getHistory: (limit: number, offset: number, search?: string, sortBy?: string): Promise<HistoryPage> =>
    api(
      "GET",
      `/api/history?limit=${limit}&offset=${offset}` +
        (search ? `&search=${encodeURIComponent(search)}` : "") +
        (sortBy ? `&sortBy=${encodeURIComponent(sortBy)}` : ""),
    ),
  getHistorySummary: (search?: string): Promise<HistorySummary> =>
    api("GET", `/api/history/summary${search ? `?search=${encodeURIComponent(search)}` : ""}`),
  removeHistoryEntry: (id: string): Promise<void> =>
    api("DELETE", `/api/history/${encodeURIComponent(id)}`),
  checkPathsExist: notAvailable,
  openPath: notAvailable,
  revealInDir: notAvailable,
  getSettings: (): Promise<AppSettings> => api("GET", "/api/settings"),
  updateSetting: (key: string, value: string): Promise<void> =>
    api("PUT", `/api/settings/${encodeURIComponent(key)}`, { value }),
  getPresetSuffix: (preset: string): Promise<string> =>
    api("GET", `/api/presets/${encodeURIComponent(preset)}/suffix`),
  setPresetSuffix: (preset: string, suffix: string): Promise<void> =>
    api("PUT", `/api/presets/${encodeURIComponent(preset)}/suffix`, { suffix }),
  resolveSuffixTemplate: (template: string, metadata: PresetMetadata): Promise<string> =>
    api("POST", "/api/suffix/resolve", { template, metadata }),
  listHandbrakePresets: (): Promise<string[]> => api("GET", "/api/handbrake/presets"),
  detectHandbrake: (): Promise<string | null> => api("GET", "/api/handbrake/detect"),
  classifyPaths: (paths: string[]): Promise<ClassifiedPaths> =>
    api("POST", "/api/paths/classify", { paths }),
  clearQueue: (): Promise<void> => api("DELETE", "/api/queue"),
  generatePresetSuffix: (preset: string): Promise<PresetMetadata> =>
    api("POST", `/api/presets/${encodeURIComponent(preset)}/suffix/generate`),
  pauseAfterCurrent: (): Promise<void> => api("POST", "/api/converter/pause-after-current"),
  cancelPauseAfterCurrent: (): Promise<void> =>
    api("DELETE", "/api/converter/pause-after-current"),
  getPauseAfterCurrent: (): Promise<boolean> => api("GET", "/api/converter/pause-after-current"),
  getLowDiskPause: (): Promise<LowDiskPause | null> => api("GET", "/api/converter/low-disk-pause"),
  getPlatformCapabilities: notAvailable,
  validateHandbrake: (): Promise<HandbrakeStatus> => api("GET", "/api/handbrake/validate"),
  quitApp: notAvailable,
  hideWindow: notAvailable,
  getWatchedDirectories: (): Promise<WatchedDirectory[]> => api("GET", "/api/watched"),
  addWatchedDirectory: (
    path: string,
    recursive: boolean,
    stabilityDelaySecs: number,
  ): Promise<WatchedDirectory> =>
    api("POST", "/api/watched", { path, recursive, stabilityDelaySecs }),
  updateWatchedDirectory: (
    id: string,
    recursive: boolean,
    stabilityDelaySecs: number,
  ): Promise<void> =>
    api("PUT", `/api/watched/${encodeURIComponent(id)}`, { recursive, stabilityDelaySecs }),
  setWatchedDirectoryEnabled: (id: string, enabled: boolean): Promise<void> =>
    api("PUT", `/api/watched/${encodeURIComponent(id)}/enabled`, { enabled }),
  removeWatchedDirectory: (id: string): Promise<void> =>
    api("DELETE", `/api/watched/${encodeURIComponent(id)}`),
  pickFolder: notAvailable,
  getBadSources: (): Promise<JobInfo[]> => api("GET", "/api/bad-sources"),
  purgeBadSources: (ids: string[]): Promise<PurgeResult[]> =>
    api("POST", "/api/bad-sources/purge", { ids }),
  getAppInfo: (): Promise<AppInfo> => api("GET", "/api/info"),

  // Extras beyond Transport (server-only):
  login: (token: string): Promise<void> => api("POST", "/api/login", { token }),
  fsList: (path: string): Promise<FsListResult> =>
    api("GET", `/api/fs/list?path=${encodeURIComponent(path)}`),
} satisfies Transport & {
  login(token: string): Promise<void>;
  fsList(path: string): Promise<FsListResult>;
};
