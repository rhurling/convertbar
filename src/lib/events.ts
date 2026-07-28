// Single seam for backend events. Desktop: Tauri's event system. The server build
// (Plan 2) replaces the internals with one shared EventSource; consumers never change.
export { listen } from "@tauri-apps/api/event";
export type { UnlistenFn, Event } from "@tauri-apps/api/event";
