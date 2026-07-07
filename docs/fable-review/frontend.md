# Fable Review: frontend

Scope: React/TypeScript frontend (non-test files). Rust command/event surface cross-checked against src-tauri.

## main.tsx
Reviewed: /Users/rhurling/Sites/convertbar/src/main.tsx (done)

Clean. StrictMode is enabled (good — it stress-tests the listener cleanup patterns, which hold up; see hooks below).

## lib/tauri.ts
Reviewed: /Users/rhurling/Sites/convertbar/src/lib/tauri.ts (done)

All 33 command names and payload keys verified against `#[tauri::command]` fns in src-tauri — no mismatches. Interfaces mirror the Rust serde shapes (snake_case) correctly. No `any` leakage anywhere in the IPC layer.

- **[Low]** tauri.ts:131 — `updateSetting(key: string, value: string)` is stringly-typed: no compile-time key checking (SettingsPage has to cast `key as keyof AppSettings`), and booleans travel as `"true"`/`"false"` strings. A typo'd key fails only at runtime. Fix: `key: keyof AppSettings` (or a keys union) and keep the string value if the backend contract requires it.
- **[Nit]** tauri.ts:126 — `sortBy?: string` could be a `"completed_at" | "space_saved" | "original_size" | "source_path"` union shared with HistoryPage, which hardcodes the same strings.

## App.tsx
Reviewed: /Users/rhurling/Sites/convertbar/src/App.tsx (done)

- **[Low]** App.tsx:22 — `refreshHbStatus()` result is un-caught; if `validate_handbrake` rejects, it becomes an unhandled promise rejection and `hbStatus` stays `null`, so the "HandBrakeCLI not found" banner silently never appears. Fix: `.catch()` and set a `{found:false,...}` fallback (or log).
- **[Nit]** App.tsx:16 — `refreshHbStatus` isn't memoized, so SettingsPage gets a new `onHbPathChanged` identity every render. Harmless here (nothing depends on it), just noting.

Escape-to-hide listener is correctly added/removed. Tabs mount/unmount pages, which resets per-page state (history search etc.) on tab switch — acceptable design for a menubar popover.

## hooks/useQueue.ts
Reviewed: /Users/rhurling/Sites/convertbar/src/hooks/useQueue.ts (done)

Event names (`conversion-progress`, `job-status-changed`, `job-completed`, `job-error`, `queue-updated`) all exist on the Rust side. Promise-based unlisten cleanup is correct, including under StrictMode double-mount.

- **[Medium]** useQueue.ts:27-38 — a single job transition emits up to 3 of these events, each firing an independent `getQueue()`; responses can resolve out of order, so a stale snapshot can win and be rendered until the next event. Fix: a monotonic request counter in `refresh` (ignore responses older than the latest), or coalesce with a microtask/debounce.
- **[Low]** useQueue.ts:7,25 — `progress` is never cleared on job-completed/job-error; only ActiveJob's `progress.job_id === job.id` guard hides the stale value. Benign today, fragile if a source is re-queued. Fix: `setProgress(null)` in the job-completed/job-error handlers.

## hooks/useHistory.ts
Reviewed: /Users/rhurling/Sites/convertbar/src/hooks/useHistory.ts (done)

Listener registration/cleanup and the refresh-on-search/sort effect are correct; errors are logged, not swallowed silently.

- **[Low]** useHistory.ts:22-26 — the debounce timeout is never cleared on unmount, so a pending `setSearch` fires after the page unmounts (no crash in React 18+, but a leaked timer and wasted work per tab switch). Fix: clear the timeout in an effect cleanup.
- **[Low]** useHistory.ts:45-56 — `loadMore` can race a `refresh` triggered by `job-completed` or a search/sort change: the in-flight page (computed from the old `history.length`/search) is appended onto the refreshed list, producing mixed result sets and potentially duplicate `job.id` React keys. Fix: a generation counter shared by refresh/loadMore; drop responses from an old generation.

## hooks/useSettings.ts
Reviewed: /Users/rhurling/Sites/convertbar/src/hooks/useSettings.ts (done)

- **[High]** useSettings.ts:66-89 — `updateSetting` is non-optimistic: it awaits `update_setting` **and** a full `get_settings` re-fetch before `setSettings` updates state. SettingsPage binds controlled text inputs (`handbrake_path`, `watch_skip_marker`) directly to this per keystroke, so every character round-trips two IPCs before the input reflects it — fast typing drops/reorders characters, and out-of-order `get_settings` responses can revert the field. Fix: optimistically merge `{[key]: value}` into local state and write in the background, or use the draft/commit-on-blur pattern WatchRow already implements.
- **[Medium]** useSettings.ts:42-52 — `refresh()` (a read path) has a write side effect: on a suffix-cache miss it calls `setPresetSuffix(preset, DEFAULT)`. Under StrictMode this runs twice on mount; more importantly the default belongs in the backend (`get_preset_suffix` returning the default when unset), not in a frontend read. Fix: move the default to Rust; also dedupe — the same 10-line fallback block is copy-pasted in `updateSetting`.
- **[Low]** useSettings.ts:66-98 — `updateSetting` and `updatePresetSuffix` have no try/catch, and no caller catches either: any backend failure is a silent unhandled rejection with the UI left stale (a toggle that visually never flips, with no error shown). Fix: catch and surface (error state like useWatchedDirectories).
- **[Nit]** useSettings.ts:69 — full `get_settings` re-fetch after every toggle (2 IPCs per checkbox click). With optimistic update this disappears.

## hooks/useWatchedDirectories.ts
Reviewed: /Users/rhurling/Sites/convertbar/src/hooks/useWatchedDirectories.ts (done)

Clean — the best hook in the codebase: every mutation try/catches into an `error` state, mounted-ref guards the async setState. This is the pattern the other hooks should follow.

- **[Nit]** useWatchedDirectories.ts:34 — rapid double-click on "+ Add folder" can open the native picker twice (no in-flight guard).

## components/DropZone.tsx
Reviewed: /Users/rhurling/Sites/convertbar/src/components/DropZone.tsx (done)

`onDragDropEvent` registration/cleanup is correct (StrictMode-safe promise unlisten). `handlePaths` has proper try/catch with user-visible error status.

- **[Medium]** DropZone.tsx:82-93 — the folder-confirm "Add" onClick is async with no try/catch: if `confirm_folder_add` or `start_queue` rejects, it's an unhandled rejection and the pending row was already... actually the row removal happens after the await, so the row stays but the user gets zero feedback and the click appears dead. Fix: wrap in try/catch and reuse the `setStatus("Error: ...")` path from `handlePaths`.
- **[Low]** DropZone.tsx:37 — dropping new paths while folder confirmations are pending calls `setPendingFolders(toConfirm)`, silently discarding the earlier pending confirmations. Fix: merge (`setPendingFolders(prev => [...prev, ...toConfirm])`, dedupe by path).
- **[Low]** DropZone.tsx:44,48,91 — status-clear `setTimeout`s are untracked: an earlier drop's 4s timer can wipe the status of a later drop early, and timers survive unmount. Fix: keep a timer ref, clear before setting a new one and in effect cleanup.
- **[Nit]** DropZone.tsx:98 — the "Skip" path fire-and-forgets `commands.startQueue()` while the "Add" path awaits it — inconsistent.

## components/ActiveJob.tsx
Reviewed: /Users/rhurling/Sites/convertbar/src/components/ActiveJob.tsx (done)

Progress fields correctly guard on `progress.job_id === job.id`; long filenames get a `title` tooltip.

- **[Medium]** ActiveJob.tsx:12 — `pauseAfter` is frontend-local `useState(false)` mirroring the backend's pause-after-current flag. It desyncs: switch tabs and back while armed and the button reads "Pause after this" although the queue will pause; SettingsPage's updater flow (`pauseAfterCurrent()` at SettingsPage.tsx:352) also arms the flag with this UI none the wiser. This state belongs to the backend. Fix: expose the flag (e.g. in `get_platform_capabilities`-style query or a `queue-updated` payload) and read it on mount / event.
- **[Low]** ActiveJob.tsx:69,76,101 — pause/resume/cancel onClicks are un-caught promises; a rejected invoke (e.g. no active process) vanishes silently.
- **[Nit]** ActiveJob.tsx:13,22-26 — `canPauseProcess` defaults to `true` before the capabilities invoke resolves, so Windows/Linux briefly render the macOS Pause button. Capabilities are static; fetch once at module/App level (or default `false` and accept the inverse flash).

## components/QueueItem.tsx
Reviewed: /Users/rhurling/Sites/convertbar/src/components/QueueItem.tsx (done)

Clean HTML5 drag-and-drop wiring; `title` tooltip covers long paths.

- **[Low]** QueueItem.tsx:17-20 — `handleRemove` has no in-flight guard or catch: double-clicking the × fires `remove_job` twice, the second rejects as an unhandled rejection. Fix: disable while pending, catch errors.
- **[Nit]** QueueItem.tsx:43 — `onDragLeave={() => onDragOver?.("")}` uses empty string as a "no target" sentinel; works, but a nullable callback would be clearer.

## components/HistoryItem.tsx
Reviewed: /Users/rhurling/Sites/convertbar/src/components/HistoryItem.tsx (done)

Clean rendering with null-safe size handling.

- **[Low]** HistoryItem.tsx:14-23 — badge precedence makes "Skipped" nearly unreachable: the Rust converter sets `kept_file = "original"` for converted-larger jobs with status `"skipped"` (converter.rs tests, e.g. line 983), so the `keptOriginal` branch wins and they render "Kept original". Likely intended (it is the more informative label), but the `skipped` branch then only fires for degenerate `kept_file: "neither"/null` cases — worth a comment or a deliberate ordering decision.

## components/TabBar.tsx
Reviewed: /Users/rhurling/Sites/convertbar/src/components/TabBar.tsx (done)

Clean. `data-tauri-drag-region` on bar and spacer, hide via `commands.hideWindow()` — no findings.

## lib/format.ts
Reviewed: /Users/rhurling/Sites/convertbar/src/lib/format.ts (done)

- **[Medium]** format.ts:22-24 — `fileName()` splits on `"/"` only, so on Windows every backslash path renders as the full path in QueueItem, ActiveJob, and HistoryItem. Meanwhile WatchedFoldersPage.tsx:5-8 has its own separator-agnostic `basename()` — duplicated logic, one of them wrong. This matches the project's known Windows path-separator theme (see MEMORY). Fix: `path.split(/[/\\]/).pop() || path` in `fileName`, delete the page-local `basename`.
- **[Nit]** format.ts:1-7 — `formatBytes` abs()es negatives without a sign; only summary/space-saved (guarded > 0) call it, so fine today.

## lib/addSummary.ts
Reviewed: /Users/rhurling/Sites/convertbar/src/lib/addSummary.ts (done)

Clean — deterministic reason ordering, exhaustive `Record<SkipReason, string>` so a new reason is a compile error. No findings.

## pages/QueuePage.tsx
Reviewed: /Users/rhurling/Sites/convertbar/src/pages/QueuePage.tsx (done)

Good empty state and HandBrake-missing banner.

- **[Low]** QueuePage.tsx:18-28 — `handleDrop` computes the reorder from the rendered `pendingJobs` snapshot; if the queue changed mid-drag (watcher added a job, a job started), `reorder_queue` gets a stale id list. Also un-caught: a rejected reorder is silent (refresh happens to self-heal the UI). Fix: catch and rely on refresh; optionally have the backend treat the list as partial ordering (it may already).
- **[Nit]** QueuePage.tsx:60 — `onDragStart={() => {}}` no-op prop; and `dragOverId` isn't cleared on `dragend` when dropping outside a QueueItem, leaving a stale highlight until the next drag. Clear it in a `onDragEnd` on the item.
- **[Nit]** QueuePage.tsx:49-52 — Clear/clearQueue un-caught (same silent-rejection pattern as elsewhere).

## pages/HistoryPage.tsx
Reviewed: /Users/rhurling/Sites/convertbar/src/pages/HistoryPage.tsx (done)

Load-more is correctly disabled while loading; search input keeps its own echo state so typing is responsive (unlike SettingsPage inputs).

- **[Low]** HistoryPage.tsx:32-37 — "Clear All" while a search filter is active clears ALL history, not the visible subset — surprising destructive action with no confirmation. `clearCompleted` is also un-caught. Fix: confirm and/or label "Clear all history".
- **[Low]** HistoryPage.tsx:26-40 — the Clear dropdown has no outside-click or Escape dismissal; it stays open until toggled or an option is clicked.
- **[Nit]** HistoryPage.tsx:14 — the summary/Clear block is hidden when `history.length === 0`, so a search with zero matches also hides the Clear button. Harmless.

## pages/WatchedFoldersPage.tsx
Reviewed: /Users/rhurling/Sites/convertbar/src/pages/WatchedFoldersPage.tsx (done)

The best page: WatchRow's local-draft + commit-on-blur/Enter for the delay field is exactly the controlled-input pattern SettingsPage needs; errors surface via the hook's error state; empty/loading states handled.

- **[Nit]** WatchedFoldersPage.tsx:5-8 — page-local `basename()` duplicates what `format.ts:fileName` should do (see format.ts finding); consolidate.
- **[Nit]** WatchedFoldersPage.tsx:101 — header shows "Watched Folders (0)" during initial load; minor flicker.

## pages/SettingsPage.tsx
Reviewed: /Users/rhurling/Sites/convertbar/src/pages/SettingsPage.tsx (done)

- **[High]** SettingsPage.tsx:308-317, 228-234, 128-135 — three controlled text inputs (`handbrake_path`, `watch_skip_marker`, suffix template) call the non-optimistic `updateSetting`/`updatePresetSuffix` on every keystroke: the input's value doesn't update until 1-2 IPC round-trips complete, so fast typing loses/reorders characters and out-of-order `get_settings` responses can revert the field (see useSettings High finding). It also spams a backend write per keystroke. Fix: adopt WatchRow's draft + commit-on-blur/Enter pattern, or make the hook optimistic with a debounced write.
- **[Medium]** SettingsPage.tsx:312-315 — `onHbPathChanged()` (→ `validate_handbrake`) fires synchronously alongside the un-awaited `updateSetting`, so validation races the settings write and can validate the OLD path — the queue-page warning banner can show the wrong state until the next edit. Fix: `await updateSetting(...)` then notify (once the input is commit-on-blur this is natural).
- **[Medium]** SettingsPage.tsx:20-41 — `resolveTemplate` reimplements Rust's `resolve_suffix_template` (handbrake.rs:212) with a different algorithm (regex cleanup vs separator-aware `replacen`; the `[-_]\.` / `\.[-_]` regexes also lack `/g`, cleaning only the first occurrence), so the "Preview: vacation..." filename can differ from the actual output name — the one job a preview has. Fix: expose a `resolve_suffix_template` command and invoke it for the preview (debounced), deleting the JS copy.
- **[Low]** SettingsPage.tsx:338-369 — updater flow: the `menu-bar-update` listener registered mid-handler is never unlistened if idle/error never arrives, and unmounting Settings loses `updateStatus`, so a returning user can click "Check for updates" again → second download + second relaunch listener. Fix: module-level in-flight guard; unlisten on unmount.
- **[Low]** SettingsPage.tsx:324 — `alert()` for detect-failure blocks the webview and is inconsistent with the app's inline status styling; use a setting-error span like `presetsError`.
- **[Nit]** SettingsPage.tsx:148 — `presetMetadata![key]` non-null assertion (safe due to the filter above, but a filter-to-entries map would avoid `!`).

## index.html / vite.config.ts / tsconfig.json
Reviewed: /Users/rhurling/Sites/convertbar/index.html, vite.config.ts, tsconfig.json (done)

- **[Low]** index.html:5,7 — leftover template scaffolding ships in the product: `<title>Tauri + React + Typescript</title>` and the `/vite.svg` favicon. Fix: title "ConvertBar", drop or replace the icon.
- **[Nit]** vite.config.ts:8 — `defineConfig(async () => ...)` — nothing is awaited; drop the async wrapper.
- tsconfig.json: `strict: true` plus noUnused* — good; no `any` leaks found anywhere in src/. Clean.

## Summary

Overall health: good. This is a small (~1.4k LOC), well-typed frontend with zero `any` in the IPC layer, every invoke() name and event name verified to match the Rust surface, and correct Tauri listener cleanup that survives StrictMode double-mounting. No Critical findings.

Themes:
1. **Non-optimistic settings writes break text inputs** (the one High): controlled inputs bound to await-then-setState IPC round-trips drop keystrokes and can revert on out-of-order responses. The codebase already contains the correct pattern (WatchRow's draft/commit-on-blur) — it just isn't used on SettingsPage.
2. **Silent rejection swallowing**: outside useWatchedDirectories, almost no mutation catch()es — failures become invisible unhandled rejections (pause/cancel/remove/clear/reorder/updateSetting).
3. **Frontend copies of backend state/logic**: `pauseAfter` mirrors a backend flag and desyncs; `resolveTemplate` re-implements Rust suffix resolution divergently. Both should be read from / computed by the backend.
4. **Minor refresh races**: multi-event refresh fan-out in useQueue and loadMore-vs-refresh in useHistory lack request-generation guards; low impact but easy to fix.
5. **Windows display**: `fileName()` is separator-naive — same class of bug already noted in project memory for Rust tests.

## Recommendations

1. (High) Convert SettingsPage's three text inputs to WatchRow's draft + commit-on-blur/Enter pattern (or make useSettings.updateSetting optimistic); await the write before `onHbPathChanged()`.
2. (Medium) Fix `fileName()` to split on `/[/\\]/` and delete WatchedFoldersPage's local `basename`.
3. (Medium) Move pause-after-current state to a backend query so ActiveJob (and the updater flow) can't desync; move the suffix-template default and preview resolution to Rust commands, deleting the divergent JS `resolveTemplate`.
4. (Medium) Add a shared error surface for mutations (the useWatchedDirectories try/catch→error pattern) so rejected invokes stop vanishing; start with DropZone's folder-confirm button, ActiveJob controls, and useSettings.
5. (Low) Add a request-generation counter to useQueue.refresh and useHistory refresh/loadMore; clear `progress` on job-completed/job-error.
6. (Low) Guard the updater flow against re-entry and unlisten on unmount; replace `alert()` with inline status.
7. (Low) Fix index.html title/favicon; add confirmation to "Clear All" history.
