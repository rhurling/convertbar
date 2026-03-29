# ConvertBar — Improvement Recommendations (v0.4.0)

## Current State Summary

The app covers ~85% of the original spec. Core functionality works: drag-and-drop queuing, HandBrakeCLI encoding with progress parsing (stdout, `\r`-delimited), SIGSTOP/SIGCONT pause/resume, template-based suffix generation from preset metadata, configurable menu bar display, history with space savings tracking, draggable popup with position memory, and screen confinement.

### Known Working
- Queue management (add files/folders, remove, clear queue, pause after current)
- Progress display in UI and menu bar (percent, ETA, fps, queue count, filename — all configurable)
- History with "Clear All" / "Clear Errors Only" dropdown
- Settings: preset, suffix template with {codec}/{resolution}/{quality}/{preset}/{device} variables, cleanup mode, launch at login, HandBrakeCLI path
- Template tray icon (auto dark/light mode)
- Close/Quit buttons, draggable window

### Known Limitations
- HandBrakeCLI already prevents macOS sleep during encoding (verified via `pmset -g assertions`) — no wrapper needed
- Progress output goes to stdout (not stderr) when piped — fixed in v0.3.0
- `window.confirm()` doesn't work in Tauri popup — replaced with in-app confirmation UI
- Folders with 1-5 files auto-add, >5 files prompt for confirmation

---

## High Impact Recommendations

### 1. macOS Native Notifications
**Why:** No feedback when conversion finishes or fails unless the popover is open. Users walk away during long encodes and have no way to know when it's done.

**What:**
- Notify on job completion: "movie.mkv converted — saved 340MB"
- Notify on job error: "movie.mkv failed: HandBrakeCLI error"
- Notify when entire queue finishes: "Queue complete — 5 files converted, 2.1GB saved"
- Settings toggle to enable/disable notifications

**How:** Use Tauri's `tauri-plugin-notification` or the `notify-rust` crate. Emit from `converter.rs` after job completion/error and after queue exhaustion.

**Files:** `src-tauri/Cargo.toml` (add plugin), `src-tauri/src/converter.rs` (emit notifications), `src-tauri/src/db.rs` (add `notifications_enabled` setting), `src/pages/SettingsPage.tsx` (add toggle)

---

### 2. Startup HandBrakeCLI Validation
**Why:** Currently, missing HandBrakeCLI is only discovered when the first encode fails. The user queues files, waits, and gets a silent error.

**What:**
- On app launch, check if HandBrakeCLI is available
- If not found: show a persistent banner in the Queue tab: "HandBrakeCLI not found. Install it via `brew install handbrake` or set the path in Settings."
- Disable the drop zone / show it dimmed until HandBrakeCLI is configured

**How:** Call `detect_handbrake` at startup in the frontend (or emit a Tauri event from `lib.rs` setup). Store result in React context/state.

**Files:** `src/App.tsx` or `src/pages/QueuePage.tsx` (check on mount), `src-tauri/src/lib.rs` (optional startup check)

---

### 3. Keyboard Shortcuts
**Why:** Power users want to control the app without clicking.

**What:**
- `Space` — pause/resume active conversion
- `Escape` — close/hide popover
- `Cmd+Q` — quit app
- `Cmd+Shift+C` (global) — toggle popover visibility from anywhere

**How:**
- In-app shortcuts: add `onKeyDown` handler to the App component
- Global hotkey: use Tauri's `tauri-plugin-global-shortcut`

**Files:** `src/App.tsx` (keydown handler), `src-tauri/Cargo.toml` (global shortcut plugin), `src-tauri/src/lib.rs` (register global shortcut)

---

### 4. Tray Right-Click Context Menu
**Why:** Quick access to common actions without opening the popover.

**What:**
- Right-click tray icon shows menu:
  - "Pause" / "Resume" (depending on state)
  - "Show ConvertBar"
  - Separator
  - "Quit"

**How:** Use Tauri v2's `Menu` + `MenuItem` API in the tray setup. Update menu items dynamically based on converter state.

**Files:** `src-tauri/src/lib.rs` (tray menu setup, dynamic updates in menu-bar-update listener)

---

## Medium Impact Recommendations

### 5. History Search & Filter
**Why:** After dozens of conversions, finding a specific file or understanding patterns (which files grew larger?) becomes hard.

**What:**
- Text input at top of History tab to filter by filename
- Sort buttons: by date (default), by space saved, by original size
- Optional: date range filter

**How:**
- Frontend: add search input and sort state to `useHistory` hook
- Backend: modify `get_history` command to accept `search` and `sort_by` parameters
- SQL: `WHERE source_path LIKE '%search%' ORDER BY {sort_by} DESC`

**Files:** `src-tauri/src/commands/queue.rs` (modify `get_history`), `src/hooks/useHistory.ts`, `src/pages/HistoryPage.tsx`

---

### 6. History Export (CSV)
**Why:** Users may want a record of space savings for reporting or personal tracking.

**What:**
- "Export" button in History tab header
- Exports CSV: filename, original size, converted size, space saved, percentage saved, kept file, preset, date

**How:**
- Rust command `export_history()` that queries all done/error jobs and formats as CSV string
- Frontend triggers download via Tauri's `save` dialog or writes to user-chosen path

**Files:** `src-tauri/src/commands/queue.rs` (new command), `src/pages/HistoryPage.tsx` (export button)

---

### 7. Completion Sound
**Why:** Audio cue when encoding finishes, especially useful when the app is in the background.

**What:**
- Play macOS system sound (e.g., "Glass" or "Ping") when a job completes
- Play a different sound on error
- Settings toggle + sound selector

**How:** Use `NSSound` via Rust FFI, or simpler: spawn `afplay /System/Library/Sounds/Glass.aiff` as a subprocess.

**Files:** `src-tauri/src/converter.rs` (play sound after job completion), `src-tauri/src/db.rs` (add setting), `src/pages/SettingsPage.tsx` (toggle)

---

### 8. Visual Queue Reordering (Drag Handles)
**Why:** The `reorder_queue` backend command exists but there's no way to drag-reorder in the UI. Users with 10+ queued items want to prioritize.

**What:**
- Add drag handle icon (≡) on each QueueItem
- HTML5 drag-and-drop or a lightweight library (e.g., `@dnd-kit/core`)
- On drop: call `commands.reorderQueue(newOrderedIds)`

**How:** Add `draggable` attribute and drag event handlers to QueueItem. Track drag source/target indices. On drop, compute new order and call backend.

**Files:** `src/components/QueueItem.tsx` (drag handle + events), `src/pages/QueuePage.tsx` (drag state management), `src/App.css` (drag styles)

---

### 9. File Picker for HandBrakeCLI Path
**Why:** Typing a file path manually is error-prone. A native file browser is more user-friendly.

**What:**
- "Browse" button next to the HandBrakeCLI path field
- Opens native macOS file picker filtered to executables

**How:** Use `tauri-plugin-dialog` for native file selection.

**Files:** `src-tauri/Cargo.toml` (add plugin), `src/pages/SettingsPage.tsx` (browse button)

---

## Polish Recommendations

### 10. Better Empty States
- Queue: "Drag video files or folders here to get started" with a subtle icon
- History: "Completed conversions will appear here"
- Settings preset error: "Install HandBrakeCLI: `brew install handbrake`" with copyable command

### 11. Button Press Feedback
- Add `:active` pseudo-state to all `.btn` classes (slight scale or darken)
- Add `cursor: pointer` consistently on all interactive elements
- Add disabled state styling (opacity 0.5, cursor: not-allowed)

### 12. Accessibility
- Add `role` and `aria-label` attributes to buttons (Pause, Resume, Cancel)
- Visible focus indicators (`:focus-visible` outline)
- Ensure tab order is logical through all pages
- Test with VoiceOver

### 13. Auto-Cleanup Old History
- Setting: "Auto-delete history older than X months" (default: never)
- Run cleanup on app startup
- Prevents database from growing indefinitely

### 14. Drop on Tray Icon
- Allow dragging files directly onto the menu bar icon to queue them
- Requires modifying the Tauri tray event handler to accept drag-drop events
- Note: Tauri v2 may not support this natively — would need a native macOS plugin

---

## Spec Compliance Gaps

| Spec Requirement | Status | Gap |
|---|---|---|
| US3: Queue drag reordering | Backend only | No drag handles in UI |
| US1: Skipped files notification | Missing | Silent skip, no user feedback |
| US2: Error state icon in menu bar | Missing | No `!` indicator on error |
| Notifications on completion | Missing | Not implemented |
| Global hotkey for popover | Missing | Not implemented |
| Launch at login | Setting only | No actual macOS login item registration (SMAppService / tauri-plugin-autostart) |

---

## Technical Debt

- `window.confirm()` replaced with in-app UI but old import may still exist
- Debug log file (`debug_progress.log`) may still be created in app data dir — cleanup code removed but file persists from testing
- `clear_completed` accepts `mode` param but the Rust command signature change may not match all frontend callers
- CSS has some hardcoded colors instead of using CSS variables consistently
- No tests (unit or integration) exist for either Rust or TypeScript code
