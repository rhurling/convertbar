# ConvertBar — macOS Menu Bar Video Converter

## Overview

**Problem:** Converting video files with HandBrakeCLI via a bash function works but lacks queueing, progress visibility, history tracking, and a user-friendly interface.

**Solution:** A lightweight macOS menu bar app that wraps HandBrakeCLI with a queue system, real-time progress in the menu bar, conversion history with space savings tracking, and drag-and-drop file management.

**Goals:**
- One-click/drag-drop video conversion with HandBrakeCLI
- Real-time progress and ETA visible in the menu bar
- Queue management with pause/resume
- History showing space saved and which file was kept
- Minimal footprint — no dock icon, lives entirely in the menu bar

**Success criteria:**
- Converts files identically to the existing bash function
- Progress and ETA visible without opening the popover
- Queue handles 100+ files without issues
- App uses <50MB RAM when idle

## Tech Stack

- **Framework:** Tauri v2
- **Frontend:** React + TypeScript
- **Backend:** Rust (thin layer for process management, file I/O, SQLite)
- **Database:** SQLite (via `rusqlite`) for queue and history persistence
- **App data location:** `~/Library/Application Support/com.convertbar.app/`

## User Stories

### US1: Drop files to convert
**As a** user, **I want to** drag video files or folders onto the app **so that** they get queued for conversion.

**Acceptance criteria:**
- Drop zone accepts files with extensions: `.mp4`, `.mkv`, `.avi`, `.mov`, `.wmv`, `.flv`, `.webm`, `.m4v`, `.ts`
- Dropping a folder recursively scans for video files in all subfolders
- When a folder drop finds files, a confirmation dialog shows: "Found N video files in [folder name]. Add all to queue?"
- Files already ending in the preset's suffix (e.g., `.1080p-h265.mp4`) are silently skipped
- Files where the output file already exists in the same directory are silently skipped
- Skipped files show a brief notification but don't block the queue

### US2: Monitor progress in menu bar
**As a** user, **I want to** see conversion progress and ETA in the menu bar **so that** I don't need to open the app.

**Acceptance criteria:**
- Idle state: static icon `◇`
- Encoding state: icon with percentage `◇ 62%`
- Paused state: icon with pause indicator `◇ ⏸`
- Error state: icon with attention indicator `◇ !`
- Tooltip on hover shows: current file name, percentage, ETA, queue count

### US3: Manage the queue
**As a** user, **I want to** view, pause, and manage queued conversions **so that** I have control over what's being processed.

**Acceptance criteria:**
- Queue tab shows: active conversion with progress bar, pending items, recently completed items
- Pause button sends SIGSTOP to HandBrakeCLI process, freezing it in place
- Resume sends SIGCONT, continuing from where it left off
- Cancel kills the process, cleans up partial output file
- Items can be reordered in the queue (drag to reorder)
- Items can be removed from the queue before processing

### US4: View conversion history
**As a** user, **I want to** see a history of past conversions **so that** I know how much space was saved and which file was kept.

**Acceptance criteria:**
- History tab shows all completed conversions
- Each entry shows: original filename, original size → converted size, percentage saved, which file was kept (original or converted)
- Summary at top: "Total saved: X GB (N files)"
- Entries where the original was kept (converted was larger) are visually distinct
- History persists across app restarts

### US5: Configure settings
**As a** user, **I want to** configure the conversion preset and cleanup behavior **so that** the app works how I prefer.

**Acceptance criteria:**
- **Preset selector:** dropdown of HandBrakeCLI presets, default "H.265 Apple VideoToolbox 1080p"
- **Filename suffix:** tied to the selected preset (e.g., preset "H.265 Apple VideoToolbox 1080p" → suffix `.1080p-h265.mp4`). Configurable per preset.
- **Cleanup mode:** toggle between "Move to Trash" (default) and "Delete permanently"
- **Launch at login:** toggle, off by default
- **HandBrakeCLI path:** auto-detected, manually overridable
- Settings persist across app restarts

### US6: Auto-resume on launch
**As a** user, **I want** unfinished jobs to automatically resume when the app starts **so that** I don't lose progress after a restart.

**Acceptance criteria:**
- On launch, partial output files from interrupted conversions are deleted
- Jobs that were `queued` or `encoding` when the app quit are reset to `queued`
- Queue processing starts automatically

## Technical Design

### Architecture

```
┌─────────────────────────────────────────────┐
│                 Tauri Shell                   │
│  ┌──────────────┐    ┌────────────────────┐  │
│  │  React UI    │◄──►│   Rust Backend     │  │
│  │  (Popover)   │    │                    │  │
│  │              │    │  ┌──────────────┐  │  │
│  │  - Queue tab │    │  │ Job Manager  │  │  │
│  │  - History   │    │  │ (spawn HB,   │  │  │
│  │  - Settings  │    │  │  signals,    │  │  │
│  │  - Drop zone │    │  │  progress)   │  │  │
│  │              │    │  └──────┬───────┘  │  │
│  └──────────────┘    │         │          │  │
│                      │  ┌──────▼───────┐  │  │
│                      │  │   SQLite DB  │  │  │
│                      │  └──────────────┘  │  │
│                      └────────────────────┘  │
└─────────────────────────────────────────────┘
         │
         ▼
   HandBrakeCLI (child process)
```

### Data Model

**SQLite database** at `~/Library/Application Support/com.convertbar.app/convertbar.db`

```sql
CREATE TABLE jobs (
    id              TEXT PRIMARY KEY,  -- UUID
    source_path     TEXT NOT NULL,
    output_path     TEXT NOT NULL,
    preset          TEXT NOT NULL,
    status          TEXT NOT NULL DEFAULT 'queued',
        -- queued | encoding | paused | done | error | skipped
    original_size   INTEGER,          -- bytes
    converted_size  INTEGER,          -- bytes, NULL until done
    kept_file       TEXT,             -- 'original' | 'converted', NULL until done
    space_saved     INTEGER,          -- bytes, NULL until done (negative if converted was larger)
    error_message   TEXT,
    queue_order     INTEGER NOT NULL, -- for ordering
    created_at      TEXT NOT NULL,    -- ISO 8601
    completed_at    TEXT              -- ISO 8601, NULL until done
);

CREATE TABLE settings (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE TABLE preset_suffixes (
    preset_name TEXT PRIMARY KEY,
    suffix      TEXT NOT NULL       -- e.g., ".1080p-h265" for "H.265 Apple VideoToolbox 1080p"
);
```

**Default settings:**
| Key | Default Value |
|-----|---------------|
| `preset` | `H.265 Apple VideoToolbox 1080p` |
| `cleanup_mode` | `trash` |
| `launch_at_login` | `false` |
| `handbrake_path` | auto-detected |

**Default preset suffixes:**
| Preset | Suffix |
|--------|--------|
| `H.265 Apple VideoToolbox 1080p` | `.1080p-h265` |

When the user selects a new preset that has no suffix mapping yet, prompt them to define one.

### Rust Backend — Tauri Commands

```
// Queue management
add_files(paths: Vec<String>) -> Vec<JobInfo>
add_folder(path: String) -> FolderScanResult { file_count, folder_name }
confirm_folder_add(path: String) -> Vec<JobInfo>
remove_job(id: String)
reorder_queue(job_ids: Vec<String>)
clear_completed()

// Conversion control
start_queue()
pause()
resume()
cancel_current()

// Data
get_queue() -> Vec<JobInfo>
get_history(limit: u32, offset: u32) -> HistoryPage
get_history_summary() -> HistorySummary { total_saved_bytes, total_files }

// Settings
get_settings() -> Settings
update_setting(key: String, value: String)
get_preset_suffix(preset: String) -> Option<String>
set_preset_suffix(preset: String, suffix: String)
list_handbrake_presets() -> Vec<String>  // runs HandBrakeCLI --preset-list
detect_handbrake_path() -> Option<String>
```

### Tauri Events (backend → frontend)

```
// Emitted ~every second during encoding
conversion-progress { job_id, percent, eta_seconds, fps, avg_fps }

// Emitted on status changes
job-status-changed { job_id, old_status, new_status }

// Emitted when a conversion completes
job-completed { job_id, original_size, converted_size, kept_file, space_saved }

// Emitted on errors
job-error { job_id, message }

// Menu bar title updates
menu-bar-update { text, tooltip }
```

### HandBrakeCLI Integration

**Spawning:**
```
HandBrakeCLI -Z "{preset}" -O -i "{input}" -o "{output}"
```

The `-O` flag enables hardware optimization. The preset is user-configurable.

**Progress parsing:**
HandBrakeCLI outputs to stderr lines matching:
```
Encoding: task 1 of 1, 45.23 % (28.4 fps, avg 31.2 fps, ETA 00h03m12s)
```

Regex: `(\d+\.\d+)\s*%.*?(\d+\.\d+)\s*fps.*?avg\s*(\d+\.\d+)\s*fps.*?ETA\s*(\d+h\d+m\d+s)`

**Process signals:**
- Pause: `kill(pid, SIGSTOP)`
- Resume: `kill(pid, SIGCONT)`
- Cancel: `kill(pid, SIGTERM)`, then clean up partial output file

**Post-conversion logic:**
1. Check HandBrakeCLI exit code (0 = success)
2. Compare `original_size` vs `converted_size`
3. If converted is smaller or equal: remove original (trash or delete per setting)
4. If converted is larger: remove converted file (trash or delete per setting)
5. Record which file was kept and space saved in the database

### File Naming

Output file: `{original_name_without_ext}{preset_suffix}.mp4`

Example: `vacation.mkv` → `vacation.1080p-h265.mp4` (using preset "H.265 Apple VideoToolbox 1080p")

Skip logic:
- If filename already ends with `{preset_suffix}.mp4` → skip
- If output file already exists → skip

## UI/UX Specification

### Popover Layout (~400px wide, ~500px tall)

**Tab bar:** Queue | History | Settings

**Queue tab:**
- Drop zone at top (dashed border, accepts files and folders)
- Active conversion: filename, progress bar with percentage, ETA, [Pause] button
- Pending items: list with drag handles for reordering, [×] to remove
- Recently completed (last 3): filename, size saved, status icon

**History tab:**
- Summary header: "Total saved: 12.4 GB (47 files)"
- Scrollable list of completed jobs
- Each entry: original name → output name, size comparison, badge (kept smaller / kept original / error)
- Entries where original was kept have a distinct style (e.g., amber badge)

**Settings tab:**
- Preset dropdown (populated from `HandBrakeCLI --preset-list`)
- Filename suffix field (auto-populated from preset_suffixes table, editable)
- Cleanup mode: radio buttons (Trash / Delete permanently)
- Launch at login: checkbox
- HandBrakeCLI path: text field with [Browse] and [Auto-detect] buttons

### Menu Bar States

| State | Icon Text | Tooltip |
|-------|-----------|---------|
| Idle, empty queue | `◇` | "ConvertBar — No active conversions" |
| Idle, items queued | `◇` | "ConvertBar — N items queued" |
| Encoding | `◇ 62%` | "Converting movie.mkv — 62% — ETA 4:32 — 3 queued" |
| Paused | `◇ ⏸` | "ConvertBar — Paused (movie.mkv at 62%)" |
| Error | `◇ !` | "ConvertBar — Error converting movie.mkv" |

### Drag-and-Drop Behavior

1. User drags files/folders onto the drop zone (or the menu bar icon itself if feasible)
2. Frontend filters by accepted extensions
3. If any folders: Rust scans recursively for video files
4. If folder scan finds files: confirmation dialog "Found N video files in [folder]. Add all to queue?"
5. Files added to queue, processing starts if not already running

## Non-Functional Requirements

- **Memory:** <50MB RAM when idle, <100MB during conversion (HandBrakeCLI is a separate process)
- **Binary size:** <10MB (Tauri target)
- **Startup time:** <1s to menu bar icon visible
- **Progress updates:** ~1/second to frontend, menu bar text updated every 2-3 seconds to avoid flicker
- **History retention:** unlimited, paginated queries
- **Platform:** macOS only (Apple Silicon + Intel)
- **HandBrakeCLI dependency:** must be pre-installed, app shows helpful error if not found

## Open Questions

1. **Notifications:** Should the app send a macOS notification when a conversion completes or the queue finishes? (Likely yes, but not discussed)
2. **Multiple simultaneous encodes:** Currently sequential. Some users may want 2 parallel encodes on powerful machines. Defer to v2?
3. **Keyboard shortcut:** Global hotkey to open the popover? (Nice to have, not essential)
4. **App icon/name:** "ConvertBar" is a working title — finalize before release

## Out of Scope

- Built-in HandBrakeCLI installation/updates
- Custom HandBrake encoding parameters beyond preset selection
- Video preview or playback
- Cloud sync of history
- Windows/Linux support
- Batch renaming beyond the suffix pattern
- Scheduled/timed conversions

## Implementation Notes

### Key Decisions
- **Tauri v2 over Electron:** ~5MB vs ~150MB binary, lower memory footprint, native macOS feel
- **SQLite over JSON files:** proper querying for history, atomic writes, handles concurrent access
- **SIGSTOP/SIGCONT for pause:** preserves encoding progress, instant pause/resume, no re-encoding penalty
- **Sequential encoding:** HandBrake uses hardware VideoToolbox acceleration — parallel encodes would compete for the GPU and likely be slower overall
- **Preset-linked suffixes:** suffix is stored per-preset in the database, so switching presets automatically updates the output filename pattern

### HandBrakeCLI Path Detection
Search order:
1. User-configured path in settings
2. `which HandBrakeCLI`
3. `/usr/local/bin/HandBrakeCLI`
4. `/opt/homebrew/bin/HandBrakeCLI`

### Trash Implementation
Use macOS `NSFileManager.trashItem` via Rust's `objc` crate or `trash` crate for proper Finder trash behavior (items appear in Trash with "Put Back" support).
