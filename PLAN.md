# ConvertBar Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Build a macOS menu bar app that wraps HandBrakeCLI with queue management, progress tracking, and conversion history.

**Architecture:** Tauri v2 app with React+TypeScript frontend in a popover, Rust backend for process management and SQLite persistence. Menu bar icon shows live progress. HandBrakeCLI runs as a child process with SIGSTOP/SIGCONT for pause/resume.

**Tech Stack:** Tauri v2, React 18, TypeScript, Rust, SQLite (via `rusqlite`), `trash` crate, `uuid` crate

**Reference:** See `SPEC.md` for full specification, UI layouts, and data model.

---

## Task 1: Project Scaffolding

**Goal:** Create the Tauri v2 + React + TypeScript project with all dependencies configured.

**Files:**
- Create: project root (via `create-tauri-app`)
- Modify: `src-tauri/Cargo.toml` (add dependencies)
- Modify: `package.json` (add frontend dependencies)
- Modify: `src-tauri/tauri.conf.json` (configure as menu bar app)

**Step 1: Scaffold Tauri v2 project**

Run:
```bash
cd /Users/rouvenhurling/Downloads/mac-menubar-convert
npm create tauri-app@latest . -- --template react-ts --manager npm
```

If prompted, select: TypeScript, React, npm.

**Step 2: Add Rust dependencies**

Edit `src-tauri/Cargo.toml` — add to `[dependencies]`:
```toml
rusqlite = { version = "0.31", features = ["bundled"] }
uuid = { version = "1", features = ["v4"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
trash = "5"
regex = "1"
dirs = "5"
```

**Step 3: Add frontend dependencies**

Run:
```bash
npm install @tauri-apps/api@^2
```

**Step 4: Configure as menu bar app**

Edit `src-tauri/tauri.conf.json`:
- Set `"identifier"` to `"com.convertbar.app"`
- Set `"productName"` to `"ConvertBar"`
- Under `"app"` > `"windows"`: set `"visible": false` (no main window on launch — we use tray popover)
- Ensure system tray capabilities are enabled

**Step 5: Verify project builds**

Run:
```bash
cd /Users/rouvenhurling/Downloads/mac-menubar-convert
npm install
cd src-tauri && cargo build
cd .. && npm run tauri dev
```

Expected: App launches (may show blank window or tray icon depending on defaults). Close it.

**Step 6: Commit**

```bash
git init
git add -A
git commit -m "chore: scaffold Tauri v2 + React + TypeScript project"
```

---

## Task 2: Database Layer

**Goal:** Set up SQLite database with schema, migrations, and CRUD helpers.

**Files:**
- Create: `src-tauri/src/db.rs`
- Modify: `src-tauri/src/lib.rs` (add mod db)

**Step 1: Create database module**

Create `src-tauri/src/db.rs`:

```rust
use rusqlite::{Connection, Result, params};
use std::path::PathBuf;

pub fn get_db_path() -> PathBuf {
    let app_support = dirs::data_dir()
        .expect("Could not find Application Support directory");
    let db_dir = app_support.join("com.convertbar.app");
    std::fs::create_dir_all(&db_dir).expect("Could not create app data directory");
    db_dir.join("convertbar.db")
}

pub fn init_db(conn: &Connection) -> Result<()> {
    conn.execute_batch("
        CREATE TABLE IF NOT EXISTS jobs (
            id              TEXT PRIMARY KEY,
            source_path     TEXT NOT NULL,
            output_path     TEXT NOT NULL,
            preset          TEXT NOT NULL,
            status          TEXT NOT NULL DEFAULT 'queued',
            original_size   INTEGER,
            converted_size  INTEGER,
            kept_file       TEXT,
            space_saved     INTEGER,
            error_message   TEXT,
            queue_order     INTEGER NOT NULL,
            created_at      TEXT NOT NULL,
            completed_at    TEXT
        );

        CREATE TABLE IF NOT EXISTS settings (
            key   TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS preset_suffixes (
            preset_name TEXT PRIMARY KEY,
            suffix      TEXT NOT NULL
        );

        INSERT OR IGNORE INTO settings (key, value) VALUES ('preset', 'H.265 Apple VideoToolbox 1080p');
        INSERT OR IGNORE INTO settings (key, value) VALUES ('cleanup_mode', 'trash');
        INSERT OR IGNORE INTO settings (key, value) VALUES ('launch_at_login', 'false');
        INSERT OR IGNORE INTO settings (key, value) VALUES ('handbrake_path', '');

        INSERT OR IGNORE INTO preset_suffixes (preset_name, suffix) VALUES ('H.265 Apple VideoToolbox 1080p', '.1080p-h265');
    ")
}
```

**Step 2: Wire up database in lib.rs**

Add `mod db;` to `src-tauri/src/lib.rs`. In the `run()` function, initialize the database connection and store it in Tauri's managed state:

```rust
mod db;

use rusqlite::Connection;
use std::sync::Mutex;

pub struct AppState {
    pub db: Mutex<Connection>,
}
```

Initialize in the Tauri builder:
```rust
let db_path = db::get_db_path();
let conn = Connection::open(&db_path).expect("Failed to open database");
db::init_db(&conn).expect("Failed to initialize database");

tauri::Builder::default()
    .manage(AppState { db: Mutex::new(conn) })
    // ... rest of builder
```

**Step 3: Verify database creates correctly**

Run: `cd src-tauri && cargo build`
Expected: Compiles without errors.

**Step 4: Commit**

```bash
git add src-tauri/src/db.rs src-tauri/src/lib.rs src-tauri/Cargo.toml src-tauri/Cargo.lock
git commit -m "feat: add SQLite database layer with schema and migrations"
```

---

## Task 3: Data Types and Settings Commands

**Goal:** Define shared data types and implement settings Tauri commands.

**Files:**
- Create: `src-tauri/src/types.rs`
- Create: `src-tauri/src/commands/mod.rs`
- Create: `src-tauri/src/commands/settings.rs`
- Modify: `src-tauri/src/lib.rs`

**Step 1: Create shared types**

Create `src-tauri/src/types.rs`:

```rust
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobInfo {
    pub id: String,
    pub source_path: String,
    pub output_path: String,
    pub preset: String,
    pub status: String,
    pub original_size: Option<i64>,
    pub converted_size: Option<i64>,
    pub kept_file: Option<String>,
    pub space_saved: Option<i64>,
    pub error_message: Option<String>,
    pub queue_order: i32,
    pub created_at: String,
    pub completed_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    pub preset: String,
    pub cleanup_mode: String,
    pub launch_at_login: bool,
    pub handbrake_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistorySummary {
    pub total_saved_bytes: i64,
    pub total_files: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryPage {
    pub jobs: Vec<JobInfo>,
    pub total: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FolderScanResult {
    pub file_count: usize,
    pub folder_name: String,
    pub folder_path: String,
}
```

**Step 2: Create settings commands**

Create `src-tauri/src/commands/mod.rs`:
```rust
pub mod settings;
```

Create `src-tauri/src/commands/settings.rs`:

```rust
use crate::AppState;
use crate::types::Settings;
use rusqlite::params;
use tauri::State;

#[tauri::command]
pub fn get_settings(state: State<AppState>) -> Result<Settings, String> {
    let db = state.db.lock().map_err(|e| e.to_string())?;
    let mut stmt = db
        .prepare("SELECT key, value FROM settings")
        .map_err(|e| e.to_string())?;
    let mut settings = Settings {
        preset: String::new(),
        cleanup_mode: String::from("trash"),
        launch_at_login: false,
        handbrake_path: String::new(),
    };
    let rows = stmt
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(|e| e.to_string())?;
    for row in rows {
        let (key, value) = row.map_err(|e| e.to_string())?;
        match key.as_str() {
            "preset" => settings.preset = value,
            "cleanup_mode" => settings.cleanup_mode = value,
            "launch_at_login" => settings.launch_at_login = value == "true",
            "handbrake_path" => settings.handbrake_path = value,
            _ => {}
        }
    }
    Ok(settings)
}

#[tauri::command]
pub fn update_setting(state: State<AppState>, key: String, value: String) -> Result<(), String> {
    let db = state.db.lock().map_err(|e| e.to_string())?;
    db.execute(
        "INSERT OR REPLACE INTO settings (key, value) VALUES (?1, ?2)",
        params![key, value],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub fn get_preset_suffix(state: State<AppState>, preset: String) -> Result<Option<String>, String> {
    let db = state.db.lock().map_err(|e| e.to_string())?;
    let result = db.query_row(
        "SELECT suffix FROM preset_suffixes WHERE preset_name = ?1",
        params![preset],
        |row| row.get::<_, String>(0),
    );
    match result {
        Ok(suffix) => Ok(Some(suffix)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.to_string()),
    }
}

#[tauri::command]
pub fn set_preset_suffix(
    state: State<AppState>,
    preset: String,
    suffix: String,
) -> Result<(), String> {
    let db = state.db.lock().map_err(|e| e.to_string())?;
    db.execute(
        "INSERT OR REPLACE INTO preset_suffixes (preset_name, suffix) VALUES (?1, ?2)",
        params![preset, suffix],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}
```

**Step 3: Register commands in lib.rs**

Add to `src-tauri/src/lib.rs`:
```rust
mod commands;
mod types;
```

Register in the Tauri builder `.invoke_handler()`:
```rust
.invoke_handler(tauri::generate_handler![
    commands::settings::get_settings,
    commands::settings::update_setting,
    commands::settings::get_preset_suffix,
    commands::settings::set_preset_suffix,
])
```

**Step 4: Verify compiles**

Run: `cd src-tauri && cargo build`
Expected: Compiles without errors.

**Step 5: Commit**

```bash
git add src-tauri/src/types.rs src-tauri/src/commands/
git commit -m "feat: add data types and settings Tauri commands"
```

---

## Task 4: HandBrakeCLI Detection and Preset Listing

**Goal:** Detect HandBrakeCLI path and list available presets.

**Files:**
- Create: `src-tauri/src/handbrake.rs`
- Create: `src-tauri/src/commands/handbrake.rs`
- Modify: `src-tauri/src/commands/mod.rs`
- Modify: `src-tauri/src/lib.rs`

**Step 1: Create handbrake utility module**

Create `src-tauri/src/handbrake.rs`:

```rust
use std::path::PathBuf;
use std::process::Command;

const KNOWN_PATHS: &[&str] = &[
    "/usr/local/bin/HandBrakeCLI",
    "/opt/homebrew/bin/HandBrakeCLI",
];

pub fn detect_handbrake_path() -> Option<String> {
    // Try `which` first
    if let Ok(output) = Command::new("which").arg("HandBrakeCLI").output() {
        if output.status.success() {
            let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !path.is_empty() && PathBuf::from(&path).exists() {
                return Some(path);
            }
        }
    }
    // Fall back to known paths
    for path in KNOWN_PATHS {
        if PathBuf::from(path).exists() {
            return Some(path.to_string());
        }
    }
    None
}

pub fn list_presets(handbrake_path: &str) -> Result<Vec<String>, String> {
    let output = Command::new(handbrake_path)
        .arg("--preset-list")
        .output()
        .map_err(|e| format!("Failed to run HandBrakeCLI: {}", e))?;

    // HandBrakeCLI outputs preset list to stderr
    let stderr = String::from_utf8_lossy(&output.stderr);
    let mut presets = Vec::new();

    for line in stderr.lines() {
        let trimmed = line.trim();
        // Preset lines are indented and end with the preset name
        // Format: "    + PresetName" under category headers
        if let Some(name) = trimmed.strip_prefix("+ ") {
            // Skip category headers (they have a colon at the end)
            if !name.ends_with('/') && !name.is_empty() {
                presets.push(name.to_string());
            }
        }
    }
    Ok(presets)
}
```

**Step 2: Create handbrake commands**

Create `src-tauri/src/commands/handbrake.rs`:

```rust
use crate::handbrake;
use crate::AppState;
use rusqlite::params;
use tauri::State;

#[tauri::command]
pub fn detect_handbrake(state: State<AppState>) -> Result<Option<String>, String> {
    // Check user-configured path first
    let db = state.db.lock().map_err(|e| e.to_string())?;
    let configured: Option<String> = db
        .query_row(
            "SELECT value FROM settings WHERE key = 'handbrake_path'",
            [],
            |row| row.get(0),
        )
        .ok()
        .filter(|s: &String| !s.is_empty() && std::path::PathBuf::from(s).exists());

    if let Some(path) = configured {
        return Ok(Some(path));
    }
    Ok(handbrake::detect_handbrake_path())
}

#[tauri::command]
pub fn list_handbrake_presets(state: State<AppState>) -> Result<Vec<String>, String> {
    let db = state.db.lock().map_err(|e| e.to_string())?;
    let handbrake_path: String = db
        .query_row(
            "SELECT value FROM settings WHERE key = 'handbrake_path'",
            [],
            |row| row.get(0),
        )
        .unwrap_or_default();

    let path = if handbrake_path.is_empty() {
        handbrake::detect_handbrake_path()
            .ok_or("HandBrakeCLI not found. Please install it or set the path in settings.")?
    } else {
        handbrake_path
    };

    handbrake::list_presets(&path)
}
```

**Step 3: Register and wire up**

Add to `src-tauri/src/commands/mod.rs`:
```rust
pub mod handbrake;
```

Add `mod handbrake;` to `lib.rs` and register commands:
```rust
commands::handbrake::detect_handbrake,
commands::handbrake::list_handbrake_presets,
```

**Step 4: Verify compiles**

Run: `cd src-tauri && cargo build`

**Step 5: Commit**

```bash
git add src-tauri/src/handbrake.rs src-tauri/src/commands/handbrake.rs src-tauri/src/commands/mod.rs src-tauri/src/lib.rs
git commit -m "feat: add HandBrakeCLI detection and preset listing"
```

---

## Task 5: Job Queue Management Commands

**Goal:** Implement adding files, scanning folders, removing/reordering jobs.

**Files:**
- Create: `src-tauri/src/commands/queue.rs`
- Modify: `src-tauri/src/commands/mod.rs`
- Modify: `src-tauri/src/lib.rs`

**Step 1: Create queue commands**

Create `src-tauri/src/commands/queue.rs`:

```rust
use crate::types::{FolderScanResult, JobInfo};
use crate::AppState;
use rusqlite::params;
use std::path::Path;
use tauri::State;
use uuid::Uuid;

const VIDEO_EXTENSIONS: &[&str] = &[
    "mp4", "mkv", "avi", "mov", "wmv", "flv", "webm", "m4v", "ts",
];

fn is_video_file(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| VIDEO_EXTENSIONS.contains(&ext.to_lowercase().as_str()))
        .unwrap_or(false)
}

fn scan_folder_recursive(path: &Path) -> Vec<String> {
    let mut files = Vec::new();
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            let entry_path = entry.path();
            if entry_path.is_dir() {
                files.extend(scan_folder_recursive(&entry_path));
            } else if is_video_file(&entry_path) {
                if let Some(path_str) = entry_path.to_str() {
                    files.push(path_str.to_string());
                }
            }
        }
    }
    files
}

fn get_suffix_for_preset(state: &AppState, preset: &str) -> Result<String, String> {
    let db = state.db.lock().map_err(|e| e.to_string())?;
    db.query_row(
        "SELECT suffix FROM preset_suffixes WHERE preset_name = ?1",
        params![preset],
        |row| row.get::<_, String>(0),
    )
    .map_err(|_| format!("No suffix configured for preset '{}'. Please set one in settings.", preset))
}

fn get_current_preset(state: &AppState) -> Result<String, String> {
    let db = state.db.lock().map_err(|e| e.to_string())?;
    db.query_row(
        "SELECT value FROM settings WHERE key = 'preset'",
        [],
        |row| row.get::<_, String>(0),
    )
    .map_err(|e| e.to_string())
}

fn should_skip_file(path: &Path, suffix: &str) -> bool {
    let filename = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    // Skip if already has the suffix
    if filename.ends_with(&format!("{}.mp4", suffix)) {
        return true;
    }
    // Skip if output already exists
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
    let output_name = format!("{}{}.mp4", stem, suffix);
    let output_path = path.parent().map(|p| p.join(&output_name));
    if let Some(out) = output_path {
        if out.exists() {
            return true;
        }
    }
    true == false // only skip for the above reasons
}

// Fix: the last line above is wrong. Let me correct the logic:
// should_skip returns true only if one of the two conditions above matches.
// The function already returns true in those cases and falls through to false.

#[tauri::command]
pub fn add_files(state: State<AppState>, paths: Vec<String>) -> Result<Vec<JobInfo>, String> {
    let preset = get_current_preset(&state)?;
    let suffix = get_suffix_for_preset(&state, &preset)?;
    let db = state.db.lock().map_err(|e| e.to_string())?;

    let max_order: i32 = db
        .query_row(
            "SELECT COALESCE(MAX(queue_order), 0) FROM jobs WHERE status IN ('queued', 'encoding', 'paused')",
            [],
            |row| row.get(0),
        )
        .unwrap_or(0);

    let mut added = Vec::new();
    let mut order = max_order;

    for path_str in &paths {
        let path = Path::new(path_str);
        if !path.exists() || !is_video_file(path) {
            continue;
        }

        let filename = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        // Skip if already converted
        if filename.ends_with(&format!("{}.mp4", suffix)) {
            continue;
        }

        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        let parent = path.parent().unwrap_or(Path::new("."));
        let output_name = format!("{}{}.mp4", stem, suffix);
        let output_path = parent.join(&output_name);

        // Skip if output exists
        if output_path.exists() {
            continue;
        }

        let id = Uuid::new_v4().to_string();
        let now = chrono::Utc::now().to_rfc3339();
        let original_size = std::fs::metadata(path).map(|m| m.len() as i64).ok();
        order += 1;

        db.execute(
            "INSERT INTO jobs (id, source_path, output_path, preset, status, original_size, queue_order, created_at)
             VALUES (?1, ?2, ?3, ?4, 'queued', ?5, ?6, ?7)",
            params![
                id,
                path_str,
                output_path.to_str().unwrap_or(""),
                preset,
                original_size,
                order,
                now,
            ],
        )
        .map_err(|e| e.to_string())?;

        added.push(JobInfo {
            id,
            source_path: path_str.clone(),
            output_path: output_path.to_string_lossy().to_string(),
            preset: preset.clone(),
            status: "queued".to_string(),
            original_size,
            converted_size: None,
            kept_file: None,
            space_saved: None,
            error_message: None,
            queue_order: order,
            created_at: now,
            completed_at: None,
        });
    }

    Ok(added)
}

#[tauri::command]
pub fn scan_folder(path: String) -> Result<FolderScanResult, String> {
    let folder = Path::new(&path);
    if !folder.is_dir() {
        return Err(format!("{} is not a directory", path));
    }
    let files = scan_folder_recursive(folder);
    let folder_name = folder
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("Unknown")
        .to_string();
    Ok(FolderScanResult {
        file_count: files.len(),
        folder_name,
        folder_path: path,
    })
}

#[tauri::command]
pub fn confirm_folder_add(state: State<AppState>, path: String) -> Result<Vec<JobInfo>, String> {
    let folder = Path::new(&path);
    let files = scan_folder_recursive(folder);
    add_files(state, files)
}

#[tauri::command]
pub fn get_queue(state: State<AppState>) -> Result<Vec<JobInfo>, String> {
    let db = state.db.lock().map_err(|e| e.to_string())?;
    let mut stmt = db
        .prepare(
            "SELECT id, source_path, output_path, preset, status, original_size, converted_size,
                    kept_file, space_saved, error_message, queue_order, created_at, completed_at
             FROM jobs
             WHERE status IN ('queued', 'encoding', 'paused', 'error')
             ORDER BY queue_order ASC",
        )
        .map_err(|e| e.to_string())?;

    let jobs = stmt
        .query_map([], |row| {
            Ok(JobInfo {
                id: row.get(0)?,
                source_path: row.get(1)?,
                output_path: row.get(2)?,
                preset: row.get(3)?,
                status: row.get(4)?,
                original_size: row.get(5)?,
                converted_size: row.get(6)?,
                kept_file: row.get(7)?,
                space_saved: row.get(8)?,
                error_message: row.get(9)?,
                queue_order: row.get(10)?,
                created_at: row.get(11)?,
                completed_at: row.get(12)?,
            })
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;

    Ok(jobs)
}

#[tauri::command]
pub fn remove_job(state: State<AppState>, id: String) -> Result<(), String> {
    let db = state.db.lock().map_err(|e| e.to_string())?;
    db.execute(
        "DELETE FROM jobs WHERE id = ?1 AND status = 'queued'",
        params![id],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub fn reorder_queue(state: State<AppState>, job_ids: Vec<String>) -> Result<(), String> {
    let db = state.db.lock().map_err(|e| e.to_string())?;
    for (i, id) in job_ids.iter().enumerate() {
        db.execute(
            "UPDATE jobs SET queue_order = ?1 WHERE id = ?2",
            params![i as i32 + 1, id],
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
pub fn clear_completed(state: State<AppState>) -> Result<(), String> {
    let db = state.db.lock().map_err(|e| e.to_string())?;
    db.execute("DELETE FROM jobs WHERE status IN ('done', 'skipped')", [])
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub fn get_history(
    state: State<AppState>,
    limit: u32,
    offset: u32,
) -> Result<crate::types::HistoryPage, String> {
    let db = state.db.lock().map_err(|e| e.to_string())?;
    let total: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM jobs WHERE status IN ('done', 'error', 'skipped')",
            [],
            |row| row.get(0),
        )
        .map_err(|e| e.to_string())?;

    let mut stmt = db
        .prepare(
            "SELECT id, source_path, output_path, preset, status, original_size, converted_size,
                    kept_file, space_saved, error_message, queue_order, created_at, completed_at
             FROM jobs
             WHERE status IN ('done', 'error', 'skipped')
             ORDER BY completed_at DESC
             LIMIT ?1 OFFSET ?2",
        )
        .map_err(|e| e.to_string())?;

    let jobs = stmt
        .query_map(params![limit, offset], |row| {
            Ok(JobInfo {
                id: row.get(0)?,
                source_path: row.get(1)?,
                output_path: row.get(2)?,
                preset: row.get(3)?,
                status: row.get(4)?,
                original_size: row.get(5)?,
                converted_size: row.get(6)?,
                kept_file: row.get(7)?,
                space_saved: row.get(8)?,
                error_message: row.get(9)?,
                queue_order: row.get(10)?,
                created_at: row.get(11)?,
                completed_at: row.get(12)?,
            })
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;

    Ok(crate::types::HistoryPage { jobs, total })
}

#[tauri::command]
pub fn get_history_summary(state: State<AppState>) -> Result<crate::types::HistorySummary, String> {
    let db = state.db.lock().map_err(|e| e.to_string())?;
    let (total_saved, total_files): (i64, i64) = db
        .query_row(
            "SELECT COALESCE(SUM(space_saved), 0), COUNT(*) FROM jobs WHERE status = 'done'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(|e| e.to_string())?;

    Ok(crate::types::HistorySummary {
        total_saved_bytes: total_saved,
        total_files,
    })
}
```

**Note:** This uses `chrono` — add `chrono = { version = "0.4", features = ["serde"] }` to `Cargo.toml`.

**Step 2: Register commands**

Add to `commands/mod.rs`: `pub mod queue;`

Register all queue commands in the Tauri builder's `invoke_handler`.

**Step 3: Verify compiles**

Run: `cd src-tauri && cargo build`

**Step 4: Commit**

```bash
git add src-tauri/src/commands/queue.rs src-tauri/src/commands/mod.rs src-tauri/src/lib.rs src-tauri/Cargo.toml
git commit -m "feat: add job queue management commands (add, scan, remove, reorder, history)"
```

---

## Task 6: Conversion Engine (HandBrakeCLI Process Management)

**Goal:** Spawn HandBrakeCLI, parse progress from stderr, handle completion with size comparison and file cleanup.

**Files:**
- Create: `src-tauri/src/converter.rs`
- Create: `src-tauri/src/commands/converter.rs`
- Modify: `src-tauri/src/lib.rs`
- Modify: `src-tauri/src/commands/mod.rs`

**Step 1: Create converter engine**

Create `src-tauri/src/converter.rs`:

```rust
use regex::Regex;
use rusqlite::{params, Connection};
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter};

pub struct ConversionProgress {
    pub percent: f64,
    pub eta_seconds: u64,
    pub fps: f64,
    pub avg_fps: f64,
}

pub struct ConverterState {
    pub current_process: Option<Child>,
    pub current_job_id: Option<String>,
    pub is_paused: bool,
    pub should_stop: bool,
}

impl ConverterState {
    pub fn new() -> Self {
        Self {
            current_process: None,
            current_job_id: None,
            is_paused: false,
            should_stop: false,
        }
    }
}

pub fn parse_progress(line: &str) -> Option<ConversionProgress> {
    let re = Regex::new(
        r"(\d+\.?\d*)\s*%\s*\((\d+\.?\d*)\s*fps,\s*avg\s*(\d+\.?\d*)\s*fps,\s*ETA\s*(\d+)h(\d+)m(\d+)s\)"
    ).ok()?;

    let caps = re.captures(line)?;
    let percent: f64 = caps.get(1)?.as_str().parse().ok()?;
    let fps: f64 = caps.get(2)?.as_str().parse().ok()?;
    let avg_fps: f64 = caps.get(3)?.as_str().parse().ok()?;
    let hours: u64 = caps.get(4)?.as_str().parse().ok()?;
    let minutes: u64 = caps.get(5)?.as_str().parse().ok()?;
    let seconds: u64 = caps.get(6)?.as_str().parse().ok()?;
    let eta_seconds = hours * 3600 + minutes * 60 + seconds;

    Some(ConversionProgress {
        percent,
        eta_seconds,
        fps,
        avg_fps,
    })
}

pub fn run_queue(
    app: AppHandle,
    db: Arc<Mutex<Connection>>,
    converter: Arc<Mutex<ConverterState>>,
) {
    std::thread::spawn(move || {
        loop {
            // Check if we should stop
            {
                let conv = converter.lock().unwrap();
                if conv.should_stop {
                    break;
                }
            }

            // Get next queued job
            let job = {
                let db = db.lock().unwrap();
                db.query_row(
                    "SELECT id, source_path, output_path, preset FROM jobs
                     WHERE status = 'queued' ORDER BY queue_order ASC LIMIT 1",
                    [],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, String>(3)?,
                        ))
                    },
                )
                .ok()
            };

            let (job_id, source_path, output_path, preset) = match job {
                Some(j) => j,
                None => {
                    // No more jobs — emit idle state and exit loop
                    let _ = app.emit("menu-bar-update", serde_json::json!({
                        "text": "◇",
                        "tooltip": "ConvertBar — No active conversions"
                    }));
                    break;
                }
            };

            // Mark as encoding
            {
                let db = db.lock().unwrap();
                let _ = db.execute(
                    "UPDATE jobs SET status = 'encoding' WHERE id = ?1",
                    params![job_id],
                );
            }
            let _ = app.emit("job-status-changed", serde_json::json!({
                "job_id": job_id,
                "old_status": "queued",
                "new_status": "encoding"
            }));

            // Resolve HandBrakeCLI path
            let handbrake_path = {
                let db = db.lock().unwrap();
                let configured: String = db
                    .query_row(
                        "SELECT value FROM settings WHERE key = 'handbrake_path'",
                        [],
                        |row| row.get(0),
                    )
                    .unwrap_or_default();
                if configured.is_empty() {
                    crate::handbrake::detect_handbrake_path()
                        .unwrap_or_else(|| "HandBrakeCLI".to_string())
                } else {
                    configured
                }
            };

            // Spawn HandBrakeCLI
            let child_result = Command::new(&handbrake_path)
                .arg("-Z")
                .arg(&preset)
                .arg("-O")
                .arg("-i")
                .arg(&source_path)
                .arg("-o")
                .arg(&output_path)
                .stderr(Stdio::piped())
                .stdout(Stdio::null())
                .spawn();

            let mut child = match child_result {
                Ok(c) => c,
                Err(e) => {
                    let db = db.lock().unwrap();
                    let _ = db.execute(
                        "UPDATE jobs SET status = 'error', error_message = ?1, completed_at = ?2 WHERE id = ?3",
                        params![e.to_string(), chrono::Utc::now().to_rfc3339(), job_id],
                    );
                    let _ = app.emit("job-error", serde_json::json!({
                        "job_id": job_id,
                        "message": e.to_string()
                    }));
                    continue;
                }
            };

            let pid = child.id();

            // Store process in converter state
            {
                let mut conv = converter.lock().unwrap();
                conv.current_job_id = Some(job_id.clone());
                conv.current_process = None; // We manage via PID for signals
                conv.is_paused = false;
            }

            // Parse stderr for progress
            if let Some(stderr) = child.stderr.take() {
                let reader = BufReader::new(stderr);
                let app_clone = app.clone();
                let job_id_clone = job_id.clone();

                std::thread::spawn(move || {
                    for line in reader.lines().flatten() {
                        if let Some(progress) = parse_progress(&line) {
                            let filename = std::path::Path::new(&source_path)
                                .file_name()
                                .and_then(|n| n.to_str())
                                .unwrap_or("Unknown");

                            let _ = app_clone.emit(
                                "conversion-progress",
                                serde_json::json!({
                                    "job_id": job_id_clone,
                                    "percent": progress.percent,
                                    "eta_seconds": progress.eta_seconds,
                                    "fps": progress.fps,
                                    "avg_fps": progress.avg_fps,
                                }),
                            );

                            let _ = app_clone.emit("menu-bar-update", serde_json::json!({
                                "text": format!("◇ {:.0}%", progress.percent),
                                "tooltip": format!(
                                    "Converting {} — {:.0}% — ETA {}",
                                    filename,
                                    progress.percent,
                                    format_eta(progress.eta_seconds)
                                )
                            }));
                        }
                    }
                });
            }

            // Wait for process to finish
            let status = child.wait();

            // Clear converter state
            {
                let mut conv = converter.lock().unwrap();
                conv.current_job_id = None;
                conv.current_process = None;
            }

            match status {
                Ok(exit) if exit.success() => {
                    // Post-conversion: compare sizes
                    let original_size = std::fs::metadata(&source_path)
                        .map(|m| m.len() as i64)
                        .unwrap_or(0);
                    let converted_size = std::fs::metadata(&output_path)
                        .map(|m| m.len() as i64)
                        .unwrap_or(0);

                    let (kept_file, file_to_remove) = if converted_size <= original_size {
                        ("converted", source_path.as_str())
                    } else {
                        ("original", output_path.as_str())
                    };

                    let space_saved = original_size - converted_size; // negative if converted was larger

                    // Trash or delete the larger file
                    let cleanup_mode = {
                        let db = db.lock().unwrap();
                        db.query_row(
                            "SELECT value FROM settings WHERE key = 'cleanup_mode'",
                            [],
                            |row| row.get::<_, String>(0),
                        )
                        .unwrap_or_else(|_| "trash".to_string())
                    };

                    let cleanup_result = if cleanup_mode == "delete" {
                        std::fs::remove_file(file_to_remove).map_err(|e| e.to_string())
                    } else {
                        trash::delete(file_to_remove).map_err(|e| e.to_string())
                    };

                    if let Err(e) = cleanup_result {
                        eprintln!("Warning: failed to clean up {}: {}", file_to_remove, e);
                    }

                    // Update database
                    {
                        let db = db.lock().unwrap();
                        let _ = db.execute(
                            "UPDATE jobs SET status = 'done', converted_size = ?1, kept_file = ?2,
                             space_saved = ?3, completed_at = ?4 WHERE id = ?5",
                            params![
                                converted_size,
                                kept_file,
                                space_saved,
                                chrono::Utc::now().to_rfc3339(),
                                job_id
                            ],
                        );
                    }

                    let _ = app.emit("job-completed", serde_json::json!({
                        "job_id": job_id,
                        "original_size": original_size,
                        "converted_size": converted_size,
                        "kept_file": kept_file,
                        "space_saved": space_saved,
                    }));
                }
                Ok(_) | Err(_) => {
                    // Process failed or was killed — clean up partial output
                    let _ = std::fs::remove_file(&output_path);

                    let db = db.lock().unwrap();
                    let _ = db.execute(
                        "UPDATE jobs SET status = 'error', error_message = 'HandBrakeCLI failed',
                         completed_at = ?1 WHERE id = ?2",
                        params![chrono::Utc::now().to_rfc3339(), job_id],
                    );

                    let _ = app.emit("job-error", serde_json::json!({
                        "job_id": job_id,
                        "message": "HandBrakeCLI failed"
                    }));
                }
            }
        }
    });
}

fn format_eta(seconds: u64) -> String {
    let h = seconds / 3600;
    let m = (seconds % 3600) / 60;
    let s = seconds % 60;
    if h > 0 {
        format!("{}h{:02}m{:02}s", h, m, s)
    } else {
        format!("{}m{:02}s", m, s)
    }
}
```

**Step 2: Create converter commands**

Create `src-tauri/src/commands/converter.rs`:

```rust
use crate::converter::{self, ConverterState};
use crate::AppState;
use rusqlite::{params, Connection};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, State};

pub struct ConverterManager {
    pub state: Arc<Mutex<ConverterState>>,
    pub db: Arc<Mutex<rusqlite::Connection>>,
}

#[tauri::command]
pub fn start_queue(app: AppHandle, state: State<AppState>, converter_mgr: State<ConverterManager>) {
    let db = converter_mgr.db.clone();
    let conv_state = converter_mgr.state.clone();

    // Only start if not already running
    let conv = conv_state.lock().unwrap();
    if conv.current_job_id.is_some() {
        return;
    }
    drop(conv);

    converter::run_queue(app, db, conv_state);
}

#[tauri::command]
pub fn pause_conversion(converter_mgr: State<ConverterManager>) -> Result<(), String> {
    let mut conv = converter_mgr.state.lock().map_err(|e| e.to_string())?;
    if let Some(ref job_id) = conv.current_job_id {
        // Send SIGSTOP via unsafe libc call
        // The PID is tracked when the process is spawned
        conv.is_paused = true;

        let db = converter_mgr.db.lock().map_err(|e| e.to_string())?;
        db.execute(
            "UPDATE jobs SET status = 'paused' WHERE id = ?1",
            params![job_id],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    } else {
        Err("No active conversion".to_string())
    }
}

#[tauri::command]
pub fn resume_conversion(
    app: AppHandle,
    converter_mgr: State<ConverterManager>,
) -> Result<(), String> {
    let mut conv = converter_mgr.state.lock().map_err(|e| e.to_string())?;
    if conv.is_paused {
        conv.is_paused = false;
        // SIGCONT will be sent
        let db = converter_mgr.db.lock().map_err(|e| e.to_string())?;
        if let Some(ref job_id) = conv.current_job_id {
            db.execute(
                "UPDATE jobs SET status = 'encoding' WHERE id = ?1",
                params![job_id],
            )
            .map_err(|e| e.to_string())?;
        }
        Ok(())
    } else {
        Err("Not paused".to_string())
    }
}

#[tauri::command]
pub fn cancel_conversion(converter_mgr: State<ConverterManager>) -> Result<(), String> {
    let mut conv = converter_mgr.state.lock().map_err(|e| e.to_string())?;
    conv.should_stop = true;
    // SIGTERM will be handled in the converter loop
    Ok(())
}
```

**Important note:** The SIGSTOP/SIGCONT implementation needs the PID stored properly. During implementation, ensure the child PID is captured and `libc::kill(pid, libc::SIGSTOP)` / `libc::kill(pid, libc::SIGCONT)` are used. This requires adding `libc = "0.2"` to Cargo.toml.

**Step 3: Register and wire up**

Add `pub mod converter;` to `commands/mod.rs`. Add `mod converter;` to `lib.rs`. Create the `ConverterManager` in the builder setup and register it with `.manage()`.

Register commands: `start_queue`, `pause_conversion`, `resume_conversion`, `cancel_conversion`.

**Step 4: Verify compiles**

Run: `cd src-tauri && cargo build`

**Step 5: Commit**

```bash
git add src-tauri/src/converter.rs src-tauri/src/commands/converter.rs
git commit -m "feat: add conversion engine with progress parsing and file cleanup"
```

---

## Task 7: System Tray and Menu Bar Setup

**Goal:** Configure the Tauri system tray icon, popover window, and menu bar text updates.

**Files:**
- Modify: `src-tauri/src/lib.rs` (tray setup)
- Modify: `src-tauri/tauri.conf.json` (tray configuration)
- Add icon: `src-tauri/icons/tray-icon.png`

**Step 1: Configure tray in lib.rs**

In `lib.rs`, set up the system tray with Tauri v2's tray API:

```rust
use tauri::{
    tray::{TrayIconBuilder, TrayIconEvent, MouseButton, MouseButtonState},
    Manager, WebviewWindowBuilder, WebviewUrl,
};

// In the run() function, after builder setup:
let tray = TrayIconBuilder::new()
    .icon(app.default_window_icon().unwrap().clone())
    .tooltip("ConvertBar — No active conversions")
    .on_tray_icon_event(|tray, event| {
        if let TrayIconEvent::Click {
            button: MouseButton::Left,
            button_state: MouseButtonState::Up,
            ..
        } = event
        {
            let app = tray.app_handle();
            if let Some(window) = app.get_webview_window("main") {
                if window.is_visible().unwrap_or(false) {
                    let _ = window.hide();
                } else {
                    let _ = window.show();
                    let _ = window.set_focus();
                }
            }
        }
    })
    .build(app)?;
```

**Step 2: Configure window as popover**

In `tauri.conf.json`, configure the main window:
```json
{
  "app": {
    "windows": [
      {
        "label": "main",
        "title": "ConvertBar",
        "width": 400,
        "height": 500,
        "visible": false,
        "decorations": false,
        "resizable": false,
        "alwaysOnTop": true,
        "skipTaskbar": true
      }
    ]
  }
}
```

**Step 3: Handle menu bar text updates**

Listen for `menu-bar-update` events from the converter and update the tray title/tooltip. In Tauri v2, use `tray.set_title()` and `tray.set_tooltip()`.

**Step 4: Add tray icon**

Create a simple diamond icon (◇) as a 22x22 PNG template image for the menu bar. Place at `src-tauri/icons/tray-icon.png`.

**Step 5: Verify tray appears**

Run: `npm run tauri dev`
Expected: Menu bar icon appears. Clicking toggles a small window.

**Step 6: Commit**

```bash
git add src-tauri/src/lib.rs src-tauri/tauri.conf.json src-tauri/icons/
git commit -m "feat: add system tray with popover window toggle"
```

---

## Task 8: Auto-Resume on Launch

**Goal:** On app startup, clean up partial files and resume the queue.

**Files:**
- Modify: `src-tauri/src/lib.rs`

**Step 1: Add startup cleanup logic**

After database initialization in `lib.rs`, add:

```rust
// Clean up interrupted jobs
{
    let db = app_state.db.lock().unwrap();

    // Find jobs that were encoding when app quit
    let mut stmt = db
        .prepare("SELECT id, output_path FROM jobs WHERE status IN ('encoding', 'paused')")
        .unwrap();
    let interrupted: Vec<(String, String)> = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .flatten()
        .collect();

    for (id, output_path) in &interrupted {
        // Delete partial output files
        let _ = std::fs::remove_file(output_path);
        // Reset to queued
        let _ = db.execute(
            "UPDATE jobs SET status = 'queued' WHERE id = ?1",
            params![id],
        );
    }
}

// Auto-start queue processing
// (trigger start_queue after app is fully set up)
```

**Step 2: Verify cleanup works**

Run the app, check logs for cleanup messages.

**Step 3: Commit**

```bash
git add src-tauri/src/lib.rs
git commit -m "feat: auto-resume queue and clean up partial files on launch"
```

---

## Task 9: Frontend — App Shell and Routing

**Goal:** Set up the React app shell with tab navigation (Queue, History, Settings).

**Files:**
- Modify: `src/App.tsx`
- Modify: `src/App.css` (or replace with Tailwind/CSS modules)
- Create: `src/components/TabBar.tsx`
- Create: `src/pages/QueuePage.tsx`
- Create: `src/pages/HistoryPage.tsx`
- Create: `src/pages/SettingsPage.tsx`

**Step 1: Set up app shell**

Replace `src/App.tsx`:

```tsx
import { useState } from "react";
import TabBar from "./components/TabBar";
import QueuePage from "./pages/QueuePage";
import HistoryPage from "./pages/HistoryPage";
import SettingsPage from "./pages/SettingsPage";
import "./App.css";

type Tab = "queue" | "history" | "settings";

function App() {
  const [activeTab, setActiveTab] = useState<Tab>("queue");

  return (
    <div className="app">
      <TabBar activeTab={activeTab} onTabChange={setActiveTab} />
      <div className="page">
        {activeTab === "queue" && <QueuePage />}
        {activeTab === "history" && <HistoryPage />}
        {activeTab === "settings" && <SettingsPage />}
      </div>
    </div>
  );
}

export default App;
```

**Step 2: Create TabBar component**

Create `src/components/TabBar.tsx`:
```tsx
type Tab = "queue" | "history" | "settings";

interface Props {
  activeTab: Tab;
  onTabChange: (tab: Tab) => void;
}

export default function TabBar({ activeTab, onTabChange }: Props) {
  const tabs: { id: Tab; label: string }[] = [
    { id: "queue", label: "Queue" },
    { id: "history", label: "History" },
    { id: "settings", label: "Settings" },
  ];

  return (
    <div className="tab-bar">
      {tabs.map((tab) => (
        <button
          key={tab.id}
          className={`tab ${activeTab === tab.id ? "active" : ""}`}
          onClick={() => onTabChange(tab.id)}
        >
          {tab.label}
        </button>
      ))}
    </div>
  );
}
```

**Step 3: Create placeholder pages**

Create `src/pages/QueuePage.tsx`, `HistoryPage.tsx`, `SettingsPage.tsx` with placeholder content (just the page name in a div).

**Step 4: Basic CSS**

Style `App.css` with:
- Dark theme (matches macOS menu bar aesthetic)
- No window chrome (decorations are off)
- Tab bar at top, content area below
- 400x500px layout

**Step 5: Verify renders**

Run: `npm run tauri dev`
Expected: Popover shows tab bar with three tabs, switching between placeholder pages.

**Step 6: Commit**

```bash
git add src/
git commit -m "feat: add React app shell with tab navigation"
```

---

## Task 10: Frontend — Queue Page with Drop Zone

**Goal:** Implement the Queue tab with drag-and-drop, progress display, and queue management.

**Files:**
- Modify: `src/pages/QueuePage.tsx`
- Create: `src/components/DropZone.tsx`
- Create: `src/components/ActiveJob.tsx`
- Create: `src/components/QueueItem.tsx`
- Create: `src/hooks/useQueue.ts`
- Create: `src/lib/tauri.ts` (Tauri command wrappers)

**Step 1: Create Tauri command wrapper**

Create `src/lib/tauri.ts`:

```typescript
import { invoke } from "@tauri-apps/api/core";

export interface JobInfo {
  id: string;
  source_path: string;
  output_path: string;
  preset: string;
  status: string;
  original_size: number | null;
  converted_size: number | null;
  kept_file: string | null;
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

export const commands = {
  addFiles: (paths: string[]) => invoke<JobInfo[]>("add_files", { paths }),
  scanFolder: (path: string) => invoke<FolderScanResult>("scan_folder", { path }),
  confirmFolderAdd: (path: string) => invoke<JobInfo[]>("confirm_folder_add", { path }),
  getQueue: () => invoke<JobInfo[]>("get_queue"),
  removeJob: (id: string) => invoke<void>("remove_job", { id }),
  reorderQueue: (jobIds: string[]) => invoke<void>("reorder_queue", { jobIds }),
  startQueue: () => invoke<void>("start_queue"),
  pauseConversion: () => invoke<void>("pause_conversion"),
  resumeConversion: () => invoke<void>("resume_conversion"),
  cancelConversion: () => invoke<void>("cancel_conversion"),
  clearCompleted: () => invoke<void>("clear_completed"),
};
```

**Step 2: Create useQueue hook**

Create `src/hooks/useQueue.ts` — manages queue state, listens to Tauri events for progress/status updates.

```typescript
import { useState, useEffect, useCallback } from "react";
import { listen } from "@tauri-apps/api/event";
import { commands, JobInfo, ConversionProgress } from "../lib/tauri";

export function useQueue() {
  const [queue, setQueue] = useState<JobInfo[]>([]);
  const [progress, setProgress] = useState<ConversionProgress | null>(null);

  const refresh = useCallback(async () => {
    const jobs = await commands.getQueue();
    setQueue(jobs);
  }, []);

  useEffect(() => {
    refresh();

    const unlisten1 = listen<ConversionProgress>("conversion-progress", (e) => {
      setProgress(e.payload);
    });
    const unlisten2 = listen("job-status-changed", () => refresh());
    const unlisten3 = listen("job-completed", () => {
      setProgress(null);
      refresh();
    });
    const unlisten4 = listen("job-error", () => {
      setProgress(null);
      refresh();
    });

    return () => {
      unlisten1.then((f) => f());
      unlisten2.then((f) => f());
      unlisten3.then((f) => f());
      unlisten4.then((f) => f());
    };
  }, [refresh]);

  return { queue, progress, refresh };
}
```

**Step 3: Create DropZone component**

Create `src/components/DropZone.tsx` — handles HTML5 drag-and-drop, extracts file paths, calls `addFiles` or `scanFolder` + confirmation.

**Step 4: Create ActiveJob and QueueItem components**

- `ActiveJob.tsx`: shows current conversion with progress bar, ETA, pause/resume button
- `QueueItem.tsx`: shows pending job with remove button

**Step 5: Wire up QueuePage**

Assemble components in `QueuePage.tsx`:
```tsx
export default function QueuePage() {
  const { queue, progress, refresh } = useQueue();
  const activeJob = queue.find((j) => j.status === "encoding" || j.status === "paused");
  const pendingJobs = queue.filter((j) => j.status === "queued");
  const recentDone = queue.filter((j) => j.status === "done" || j.status === "error").slice(0, 3);

  return (
    <div className="queue-page">
      <DropZone onFilesAdded={refresh} />
      {activeJob && <ActiveJob job={activeJob} progress={progress} />}
      {pendingJobs.map((job) => (
        <QueueItem key={job.id} job={job} onRemove={refresh} />
      ))}
    </div>
  );
}
```

**Step 6: Verify drop zone works**

Run: `npm run tauri dev`
Test: Drag a video file onto the drop zone. Verify it appears in the queue.

**Step 7: Commit**

```bash
git add src/
git commit -m "feat: add Queue page with drop zone, progress display, and queue management"
```

---

## Task 11: Frontend — History Page

**Goal:** Show conversion history with space savings summary.

**Files:**
- Modify: `src/pages/HistoryPage.tsx`
- Create: `src/components/HistoryItem.tsx`
- Create: `src/hooks/useHistory.ts`
- Add to: `src/lib/tauri.ts` (history commands)

**Step 1: Add history commands to tauri.ts**

```typescript
export interface HistorySummary {
  total_saved_bytes: number;
  total_files: number;
}
export interface HistoryPage {
  jobs: JobInfo[];
  total: number;
}

// Add to commands object:
getHistory: (limit: number, offset: number) =>
  invoke<HistoryPage>("get_history", { limit, offset }),
getHistorySummary: () =>
  invoke<HistorySummary>("get_history_summary"),
```

**Step 2: Create useHistory hook**

Handles pagination and auto-refresh on `job-completed` events.

**Step 3: Create HistoryItem component**

Shows: filename, size before → after, percentage saved, badge (kept smaller / kept original).

**Step 4: Build HistoryPage**

Summary header + scrollable list with load-more pagination.

**Step 5: Verify**

Run app, check history tab shows completed conversions.

**Step 6: Commit**

```bash
git add src/
git commit -m "feat: add History page with space savings tracking"
```

---

## Task 12: Frontend — Settings Page

**Goal:** Implement the settings UI with preset selection, suffix config, cleanup mode, and launch at login.

**Files:**
- Modify: `src/pages/SettingsPage.tsx`
- Create: `src/hooks/useSettings.ts`
- Add to: `src/lib/tauri.ts` (settings + handbrake commands)

**Step 1: Add remaining commands to tauri.ts**

```typescript
export interface AppSettings {
  preset: string;
  cleanup_mode: string;
  launch_at_login: boolean;
  handbrake_path: string;
}

// Add to commands:
getSettings: () => invoke<AppSettings>("get_settings"),
updateSetting: (key: string, value: string) =>
  invoke<void>("update_setting", { key, value }),
getPresetSuffix: (preset: string) =>
  invoke<string | null>("get_preset_suffix", { preset }),
setPresetSuffix: (preset: string, suffix: string) =>
  invoke<void>("set_preset_suffix", { preset, suffix }),
listHandbrakePresets: () => invoke<string[]>("list_handbrake_presets"),
detectHandbrake: () => invoke<string | null>("detect_handbrake"),
```

**Step 2: Create useSettings hook**

Loads settings on mount, provides update function that calls `updateSetting` and refreshes.

**Step 3: Build SettingsPage**

- Preset dropdown (populated from `listHandbrakePresets`)
- Suffix text field (auto-populated, editable)
- Cleanup mode radio buttons
- Launch at login checkbox
- HandBrakeCLI path with auto-detect button

**Step 4: Implement launch at login**

Use Tauri's `autostart` plugin or macOS `SMLoginItemSetEnabled` via a Rust command.

Add to Cargo.toml: `tauri-plugin-autostart = "2"`

**Step 5: Verify settings persist**

Change a setting, restart app, verify it persists.

**Step 6: Commit**

```bash
git add src/
git commit -m "feat: add Settings page with preset, suffix, cleanup mode, and autostart"
```

---

## Task 13: Styling and Polish

**Goal:** Style the app with a clean dark theme that fits macOS menu bar aesthetic.

**Files:**
- Modify: `src/App.css`
- Create: `src/styles/variables.css` (CSS custom properties)

**Step 1: Define design tokens**

```css
:root {
  --bg-primary: #1e1e1e;
  --bg-secondary: #2a2a2a;
  --bg-hover: #333;
  --text-primary: #e0e0e0;
  --text-secondary: #888;
  --accent: #4a9eff;
  --success: #4caf50;
  --warning: #ff9800;
  --error: #f44336;
  --border: #3a3a3a;
  --radius: 8px;
}
```

**Step 2: Style all components**

- Rounded popover with subtle shadow
- Drop zone with dashed border, highlight on drag over
- Progress bar with accent color
- Clean list items with hover states
- Compact settings form

**Step 3: Add utility formatters**

Create `src/lib/format.ts`:
```typescript
export function formatBytes(bytes: number): string {
  if (bytes === 0) return "0 B";
  const k = 1024;
  const sizes = ["B", "KB", "MB", "GB", "TB"];
  const i = Math.floor(Math.log(Math.abs(bytes)) / Math.log(k));
  return `${(bytes / Math.pow(k, i)).toFixed(1)} ${sizes[i]}`;
}

export function formatEta(seconds: number): string {
  const h = Math.floor(seconds / 3600);
  const m = Math.floor((seconds % 3600) / 60);
  const s = seconds % 60;
  if (h > 0) return `${h}h${m.toString().padStart(2, "0")}m`;
  return `${m}m${s.toString().padStart(2, "0")}s`;
}

export function formatPercent(saved: number, original: number): string {
  if (original === 0) return "0%";
  return `${Math.round((saved / original) * 100)}%`;
}
```

**Step 4: Verify visual quality**

Run app, check all three tabs look polished.

**Step 5: Commit**

```bash
git add src/
git commit -m "feat: add dark theme styling and utility formatters"
```

---

## Task 14: Integration Testing

**Goal:** Manually test the full workflow end-to-end.

**Step 1: Test file drop and conversion**

1. Launch app with `npm run tauri dev`
2. Drop a video file onto the drop zone
3. Verify it appears in queue and conversion starts
4. Verify progress updates in the popover and menu bar
5. Verify completion: correct file kept, history entry created

**Step 2: Test folder drop with confirmation**

1. Drop a folder containing multiple video files
2. Verify confirmation dialog appears with file count
3. Confirm — verify all files added to queue

**Step 3: Test pause/resume**

1. Start a conversion
2. Click Pause — verify HandBrakeCLI freezes (check Activity Monitor)
3. Click Resume — verify encoding continues from same point

**Step 4: Test settings**

1. Change cleanup mode to "Delete permanently"
2. Convert a file — verify the larger file is deleted (not in Trash)
3. Change preset — verify suffix updates

**Step 5: Test edge cases**

1. Drop an already-converted file (`.1080p-h265.mp4`) — should be skipped
2. Drop a non-video file — should be ignored
3. Kill the app mid-conversion, relaunch — should auto-resume

**Step 6: Fix any issues found, commit**

```bash
git add -A
git commit -m "fix: address issues found during integration testing"
```

---

## Task 15: Build and Package

**Goal:** Create a distributable `.app` bundle.

**Step 1: Build release**

```bash
npm run tauri build
```

Expected: Creates `.app` bundle in `src-tauri/target/release/bundle/macos/`

**Step 2: Test the built app**

Open the `.app` from Finder. Verify all functionality works outside dev mode.

**Step 3: Commit any build configuration changes**

```bash
git add -A
git commit -m "chore: finalize build configuration for macOS release"
```

---

## Summary

| Task | Description | Key Output |
|------|-------------|------------|
| 1 | Project scaffolding | Tauri v2 + React + TS project |
| 2 | Database layer | SQLite schema + init |
| 3 | Settings commands | CRUD for settings + preset suffixes |
| 4 | HandBrake detection | Path detection + preset listing |
| 5 | Queue management | Add/remove/reorder jobs |
| 6 | Conversion engine | HandBrakeCLI spawn, progress, cleanup |
| 7 | System tray | Menu bar icon + popover |
| 8 | Auto-resume | Startup cleanup + queue restart |
| 9 | Frontend shell | Tab navigation |
| 10 | Queue page | Drop zone + progress + queue list |
| 11 | History page | Conversion history + savings |
| 12 | Settings page | All user preferences |
| 13 | Styling | Dark theme polish |
| 14 | Integration testing | Full workflow verification |
| 15 | Build & package | Distributable .app |
