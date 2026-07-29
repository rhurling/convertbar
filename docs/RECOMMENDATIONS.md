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

### 15. Server Head — Close the Login-Throttle Throughput Gap — *shipped, branch `feature/server-auth-throttling`*
- The gap this closes: the original throttle evaluated the credential before
  sleeping (`token_matches`, then `sleep`, then 401), so concurrent and
  abandon-early guesses paid nothing — a tight loop reached ~18,000
  guesses/sec against it. Fixed by gating the *comparison*, not the response:
  `LoginThrottle::check` (`crates/convertbar-server/src/throttle.rs`) reserves
  a source's evaluation slot — inside the one lock that reads it — before the
  credential is compared. A denied source gets 401 with no comparison at all,
  and nothing sleeps; every response is immediate.
- Also shipped: the 16-character/8-distinct-character token strength floor
  (`config.rs`) and `CONVERTBAR_TRUSTED_PROXIES` support for identifying real
  clients behind a reverse proxy.
- Policy (`ThrottlePolicy::default()`): 8 free evaluations per source, then
  one evaluation per interval doubling 500ms → 30s cap, 15-minute forget
  window, cleared immediately on a successful sign-in.
- **Accepted trade-off:** while a source is gated, even the correct token is
  refused — there is no exception for the legitimate owner. This is
  deliberate (a mechanism that always honoured a correct token would still
  answer every guess); the operator-facing guidance is to wait, not retry.

### 16. Server — Panics No Longer Masquerade as Deliberate Errors — *shipped, branch `fix/server-error-taxonomy`*
- The gap this closes: all ten `spawn_blocking` join-error sites returned a 500 with an
  `{"error": ...}` body identical in shape to an ordinary core failure, so a client could not
  tell a server bug from an expected condition such as a missing HandBrakeCLI — and tests could
  separate them only by matching on the message text.
- Fixed by `routes::join_err`: one definition, still 500, carrying a `"kind": "panic"`
  discriminator that appears on that shape alone. Deliberate failures keep `core_err`'s bare
  `{"error": ...}` body. The panic detail stays on the wire exactly as before — the API is
  auth-gated by default and the threat model is single-user LAN, so debuggability wins.
- **Why both stay 500:** each really is "the server could not answer". Moving deliberate
  failures onto 4xx is the semantically cleaner design, but it is a far larger contract change
  (all 39 routes, the frontend transport, every route test) and this item asked for a
  distinction, not a re-taxonomy.
- Nine sites went through `core_err`; the tenth (`fs.rs`) built the same body through that
  module's local `json_err`, which is why grepping `core_err` found nine.
- **Applied is not enforced.** The first cut left each handler writing its own
  `spawn_blocking` match and guarded them with a tripwire matching the literal text
  `task panicked` — so a handler mapping its join arm through `core_err` with *fresh wording*
  reintroduced the gap with the whole suite green. That was confirmed by mutation, not assumed.
  Two helpers now own the mapping — `blocking_json` for work returning a `Result`, and
  `blocking_response` for `fs::fs_list`, which picks its own 200/403/404/500 — so all ten
  handlers stopped spelling their failure arms, and the tripwire checks the class instead of
  the phrase: no route module may call `spawn_blocking(`, with no exemption. Exempting
  `fs.rs` (the tenth site, and the one that diverged) would have left the tripwire blind to a
  second endpoint in that same module. It covers a route module added later, since it walks
  `src/routes` rather than listing modules; it reads source text, so it is a backstop rather
  than a proof.
- **Out of scope, recorded:** the desktop head has the same indistinguishability.
  `src-tauri/src/commands/*` map join failures with `.map_err(|e| e.to_string())?`, so the
  frontend receives a plain string with no channel to carry a discriminator. Fixing it there
  means changing the commands' return type — its own change, with its own argument.

---

## Open — High Impact

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
