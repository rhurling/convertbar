# ConvertBar — Improvement Recommendations (v0.6.0)

## Current State Summary

The app covers ~95% of the original spec. Core functionality works: drag-and-drop queuing, HandBrakeCLI encoding with progress parsing (stdout, `\r`-delimited), SIGSTOP/SIGCONT pause/resume, template-based suffix generation from preset metadata, configurable menu bar display, history with search/sort and space savings tracking, draggable popup with position memory, screen confinement, macOS notifications, tray context menu, and queue drag reordering.

### Known Working
- Queue management (add files/folders, remove, clear queue, pause after current, drag reorder)
- Progress display in UI and menu bar (percent, ETA, fps, queue count, filename — all configurable)
- History with search, sort, "Clear All" / "Clear Errors Only" dropdown
- macOS native notifications (per-file, errors-only, queue complete — all configurable)
- HandBrakeCLI startup validation with warning banner
- Settings: preset, suffix template with variables, cleanup mode, launch at login, HandBrakeCLI path, menu bar display, notifications
- Template tray icon (auto dark/light mode) with right-click context menu
- Close/Quit buttons, draggable window with position memory

### Known Limitations
- HandBrakeCLI already prevents macOS sleep during encoding (verified via `pmset -g assertions`) — no wrapper needed
- Progress output goes to stdout (not stderr) when piped — fixed in v0.3.0
- `window.confirm()` doesn't work in Tauri popup — replaced with in-app confirmation UI
- Folders with 1-5 files auto-add, >5 files prompt for confirmation

---

## Implemented (completed)

### 1. macOS Native Notifications — *v0.5.0*
- Per-file notifications (success replaces previous, errors stack individually)
- "Errors only" sub-option for per-file notifications
- Queue completion notification (independent toggle)
- 3 settings toggles in Settings page

### 2. Startup HandBrakeCLI Validation — *v0.5.0*
- Validates on app startup, after path change, after Detect click
- Warning banner in Queue tab when not found
- Shows install instructions (`brew install handbrake`)

### 4. Tray Right-Click Context Menu — *v0.6.0*
- Right-click tray icon shows: Show ConvertBar, separator, Quit
- Left-click still toggles the popover window

### 5. History Search & Filter — *v0.6.0*
- Text search by filename (debounced 300ms)
- Sort buttons: Date, Saved, Size, Name
- Summary updates to reflect filtered results

### 8. Visual Queue Reordering (Drag Handles) — *v0.6.0*
- Drag handle (≡) on each queue item
- HTML5 drag-and-drop with visual drop target indicator
- Calls `reorderQueue` on drop to persist new order

---

## Open — High Impact

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

## Open — Medium Impact

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

### 9. File Picker for HandBrakeCLI Path
**Why:** Typing a file path manually is error-prone. A native file browser is more user-friendly.

**What:**
- "Browse" button next to the HandBrakeCLI path field
- Opens native macOS file picker filtered to executables

**How:** Use `tauri-plugin-dialog` for native file selection.

**Files:** `src-tauri/Cargo.toml` (add plugin), `src/pages/SettingsPage.tsx` (browse button)

---

## Open — Polish

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
| US3: Queue drag reordering | **Done** (v0.6.0) | — |
| US1: Skipped files notification | Missing | Silent skip, no user feedback |
| US2: Error state icon in menu bar | Missing | No `!` indicator on error |
| Notifications on completion | **Done** (v0.5.0) | — |
| Global hotkey for popover | Missing | Not implemented |
| Launch at login | Setting only | No actual macOS login item registration (SMAppService / tauri-plugin-autostart) |

---

## Technical Debt

- Debug log file (`debug_progress.log`) may still exist in app data dir from testing — can be manually deleted
- CSS has some hardcoded colors instead of using CSS variables consistently
- No tests (unit or integration) exist for either Rust or TypeScript code
- Tray context menu is static (Show + Quit) — dynamic Pause/Resume items deferred due to complexity
