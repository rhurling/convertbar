# ConvertBar — Improvement Recommendations (written at v0.6.0; statuses refreshed 2026-07-28 at v1.0.0)

This is the live backlog. Everything below the "Open" headings was verified against
shipped v1.0.0 code on 2026-07-28; items found already implemented were moved into
"Implemented". Historical design docs live in `docs/archive/`.

## Current State Summary

Core functionality: drag-and-drop queuing, HandBrakeCLI encoding with progress parsing (stdout, `\r`-delimited), SIGSTOP/SIGCONT pause/resume (macOS) with queue-level pause elsewhere, template-based suffix generation from preset metadata, configurable menu bar display, history with search/sort and space savings tracking, draggable popup with position memory, screen confinement, native notifications, tray context menu, and queue drag reordering. Cross-platform: macOS, Windows, Linux.

### Known Working
- Queue management (add files/folders, remove, clear queue, pause after current, drag reorder)
- Progress display in UI and menu bar (percent, ETA, fps, queue count, filename — all configurable)
- History with search, sort, "Clear All" / "Clear Errors Only" dropdown, context menu
- Native notifications (per-file, errors-only, queue complete — all configurable)
- HandBrakeCLI startup validation with warning banner
- Settings: preset, suffix template with variables, cleanup mode, launch at login, HandBrakeCLI path, menu bar display, notifications
- Template tray icon (auto dark/light mode) with right-click context menu
- Close/Quit buttons, draggable window with position memory
- Watched folders with serialized intake and an add-progress indicator (v0.14–v0.16)
- True in-place re-encode (temp file + atomic rename), skip-by-source-media, probe-once cache
- Low-disk auto-pause with a Resume affordance (v0.17)
- Queue-pause persistence across restarts (v0.18)
- Bad-source detection, review list, and truncation guard (v0.19)
- Auto-update check and install with notification

### Known Limitations
- HandBrakeCLI already prevents macOS sleep during encoding (verified via `pmset -g assertions`) — no wrapper needed
- Progress output goes to stdout (not stderr) when piped — fixed in v0.3.0
- `window.confirm()` doesn't work in Tauri popup — replaced with in-app confirmation UI
- Folders with 1-5 files auto-add, >5 files prompt for confirmation
- One encode at a time by design (parallel encodes contend for the same GPU encoder)

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

### 10. Better Empty States — *verified shipped at 1.0.0*
- Queue: "Drag video files or folders here to get started" (`src/pages/QueuePage.tsx`)
- History: "Completed conversions will appear here" (`src/pages/HistoryPage.tsx`)
- Partial: `brew install handbrake` appears in the Queue warning banner, not in Settings,
  and is not click-to-copy. Remaining polish tracked under "Open — Polish" below.

### 11. Button Press Feedback — *verified shipped at 1.0.0*
- `.btn:active { transform: scale(0.96) }` and `.btn:disabled { opacity: .5; cursor: not-allowed }`
  in `src/App.css`, plus `:active` states on `.btn-icon` / `.btn-quit`
- `cursor: pointer` applied consistently across interactive classes

---

## Open — High Impact

### 15. Server Head — Login Throttling and a Token-Entropy Floor
**Why:** the server head (`crates/convertbar-server`, shipped in PR #130) authenticates with a
single static token, and neither end of that is currently defended. `POST /api/login` accepts
unlimited attempts at HTTP speed, and `ServerConfig::from_vars` accepts any non-empty
`CONVERTBAR_AUTH_TOKEN` — `"1"` starts the server just as happily as a 32-byte random string.
An authenticated session can browse the mounted filesystem, permanently delete files
(`purge_bad_sources` under the server's forced-delete disposer), and point `handbrake_path` at
an arbitrary binary that the next encode executes. The whole-branch review called this the
weakest link in the auth posture; it is acceptable for a trusted LAN and not acceptable for
anything wider.

**What:**
- Reject (or loudly warn at startup about) tokens below a minimum length/entropy — a hard floor
  is friendlier than a warning nobody reads, but a warning avoids breaking existing deployments.
- Rate-limit failed logins: a small fixed delay after a failure plus a per-IP failure counter is
  enough; the threat is online guessing, not a distributed attack.
- Consider a constant-time-safe generic failure response so throttling can't be used to
  distinguish "wrong token" from "throttled".

**How:** both live in the auth layer — `crates/convertbar-server/src/auth.rs` (the `login`
handler and `token_matches`) and `crates/convertbar-server/src/config.rs` (the
`AuthMode::Token` construction in `from_vars`). The middleware order and cookie handling do not
need to change.

**Files:** `crates/convertbar-server/src/auth.rs`, `crates/convertbar-server/src/config.rs`,
`crates/convertbar-server/src/routes/login.rs`, README's Auth section.

---

### 3. Keyboard Shortcuts
**Why:** Power users want to control the app without clicking.

**What:**
- ~~`Escape` — close/hide popover~~ — **shipped**, `src/App.tsx` keydown handler → `commands.hideWindow()`
- `Space` — pause/resume active conversion — still open
- `Cmd+Q` — quit app — still open (no `metaKey`/`ctrlKey` handling anywhere in `src/`)
- ~~`Cmd+Shift+C` (global) — toggle popover visibility from anywhere~~ — **dropped at 1.0**, see the
  spec-compliance table below

**How:** add two cases to the existing keydown handler in `src/App.tsx`.

**Files:** `src/App.tsx` (keydown handler)

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

**How:** spawn `afplay /System/Library/Sounds/Glass.aiff` as a subprocess. Note the app is
cross-platform as of 1.0 — `afplay` is macOS-only, so this needs a per-platform arm
(or a no-op) for Windows and Linux.

**Files:** `src-tauri/src/converter.rs` (play sound after job completion), `src-tauri/src/db.rs` (add setting), `src/pages/SettingsPage.tsx` (toggle)

---

### 9. File Picker for HandBrakeCLI Path
**Why:** Typing a file path manually is error-prone. A native file browser is more user-friendly.

**What:**
- "Browse" button next to the HandBrakeCLI path field (Settings currently has **Detect** only)
- Opens the native file picker filtered to executables

**How:** *(corrected 2026-07-28 — the original "use `tauri-apps/plugin-dialog` from the
frontend" advice no longer applies: that npm package was removed; only the Rust half of the
plugin is still registered.)* Add a `pick_file` command mirroring `pick_folder` in
`src-tauri/src/commands/watch.rs` and invoke it from the button. Two constraints carried by
that existing command: it **must** be `async` (a sync command runs on the main thread, and
`blocking_pick_*` dispatches the panel to the main thread and then blocks — deadlocking the
event loop), and being Rust-invoked it needs **no** frontend `dialog` ACL grant.

**Files:** `src-tauri/src/commands/watch.rs` (new `pick_file`), `src-tauri/src/lib.rs`
(register), `src/pages/SettingsPage.tsx` (browse button)

---

## Open — Polish

### 10b. Empty-State Remainder
- Settings preset error: "Install HandBrakeCLI: `brew install handbrake`" with copyable command.
  The command currently appears only in the Queue warning banner and is not copyable.
  (The Queue and History empty states themselves shipped — see "Implemented".)

### 12. Accessibility
- Add `role` and `aria-label` attributes to buttons (Pause, Resume, Cancel) — `src/components/ActiveJob.tsx`
  uses `title=` only, and `aria-label` appears exactly once in all of `src/` (`TabBar.tsx`)
- Visible focus indicators (`:focus-visible` outline) — zero occurrences in `src/App.css`
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
| US1: Skipped files notification | **Done** | Skips reported via `SkipReason` / `summarizeAdds` feedback |
| US2: Error state icon in menu bar | **Done** | Tray error state flag shared with menu-bar status |
| Notifications on completion | **Done** (v0.5.0) | — |
| Global hotkey for popover | **Dropped** (1.0) | Deliberately not shipping. The tray click already opens the popover from anywhere; a configurable global shortcut needs a capture UI, persistence, and conflict handling when another app owns the combo — more surface than the convenience earns. Revisit if users ask. |
| Launch at login | **Done** | `tauri-plugin-autostart` registered and wired to the setting (`src-tauri/src/lib.rs`) |

---

## Technical Debt

- Tray context menu is static (Show + Quit) — dynamic Pause/Resume items deferred due to complexity

*(Removed 2026-07-28: the `debug_progress.log` entry — zero references remain anywhere in
`src-tauri/src/`. The "hardcoded CSS colors" entry — `src/App.css` is now ~140 `var(--…)`
uses against 17 raw hex values, which is convergence rather than debt.)*
