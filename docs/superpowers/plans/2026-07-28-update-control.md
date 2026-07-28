# Update Control Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give the user three update modes (Automatic / Notify / Off), show release notes before or after an install, and stop any install from landing while a conversion is running.

**Architecture:** All update policy moves into a new Rust module `src-tauri/src/updater.rs`; the frontend becomes pure presentation driven by an `update-state` event. Policy decisions are pure functions over an injected `Installer` trait so the idle gate is testable without a network or a real Tauri app. Release notes are generated in CI into the GitHub release body, which `tauri-action` copies verbatim into `latest.json`'s `notes` field.

**Tech Stack:** Rust (Tauri 2, rusqlite, tokio), React 19 + TypeScript, Vitest + React Testing Library, Bash + GitHub Actions.

**Spec:** `docs/superpowers/specs/2026-07-28-update-control-design.md`

## Global Constraints

- Rust source must stay `cargo fmt`-clean; a rustfmt hook runs on `.rs` writes.
- Every new frontend Tauri API call needs an ACL grant in `src-tauri/capabilities/default.json` — but app-defined `#[tauri::command]` functions are **ACL-exempt**. This plan adds only app commands, so `default.json` only ever *loses* entries.
- Windows `Update::install()` calls `std::process::exit(0)` and never returns (`tauri-plugin-updater-2.10.1/src/updater.rs:865`). No code may assume the post-install path runs.
- Platform-specific Rust must use `#[cfg(target_os = "...")]` attributes, never the `cfg!()` macro.
- New settings rows must be additive. Only `update_mode` is seeded and user-facing; `update_skipped_version`, `update_notified_version`, and `update_installed` are read-with-default backend state — **not** in `ALLOWED_KEYS`, not in the UI (the `queue_paused` precedent, `converter.rs:448-467`).
- Tests use Pest-style naming conventions already in the repo: descriptive snake_case Rust test fn names stating the invariant.
- Conventional commits (`feat:`, `fix:`, `chore:`, `refactor:`, `test:`, `docs:`).
- Run Rust tests from `src-tauri/`: `cargo test`. Run frontend tests from the repo root: `npm test`.

**Refinement of the spec:** the spec describes `update_installed` as holding "`version|notes`". This plan stores it as **JSON** (`{"version":"1.2.0","notes":"..."}`) because release notes can contain any character including `|`. `serde_json` is already a dependency.

---

### Task 1: Persisted update state — `update_mode` setting and three internal keys

**Files:**
- Create: `src-tauri/src/updater.rs`
- Modify: `src-tauri/src/lib.rs` (add `mod updater;`)
- Modify: `src-tauri/src/db.rs:195-201` (seed block), `src-tauri/src/db.rs:260-263` (count guard)
- Modify: `src-tauri/src/types.rs:22-40` (`Settings` struct)
- Modify: `src-tauri/src/commands/settings.rs` (`get_settings`, `ALLOWED_KEYS`, `update_setting`)
- Test: inline `#[cfg(test)] mod tests` in `src-tauri/src/updater.rs`; existing tests in `src-tauri/src/db.rs`

**Interfaces:**
- Consumes: nothing.
- Produces:
  ```rust
  pub enum UpdateMode { Automatic, Notify, Off }
  pub fn normalize_update_mode(value: &str) -> UpdateMode
  impl UpdateMode { pub fn as_str(&self) -> &'static str }

  pub(crate) fn read_skipped_version(db: &Connection) -> Option<String>
  pub(crate) fn set_skipped_version(db: &Connection, version: &str)
  pub(crate) fn read_notified_version(db: &Connection) -> Option<String>
  pub(crate) fn set_notified_version(db: &Connection, version: &str)
  pub(crate) fn read_installed(db: &Connection) -> Option<InstalledUpdate>
  pub(crate) fn set_installed(db: &Connection, version: &str, notes: Option<&str>)
  pub(crate) fn clear_installed(db: &Connection)

  pub struct InstalledUpdate { pub version: String, pub notes: Option<String> }
  ```
  `Settings` gains `pub update_mode: String`.

- [ ] **Step 1: Write the failing tests**

Create `src-tauri/src/updater.rs` with only the test module plus stub declarations:

```rust
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

#[cfg(test)]
mod tests {
    use super::*;

    fn test_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::init_db(&conn).unwrap();
        conn
    }

    #[test]
    fn unknown_update_modes_fall_back_to_automatic() {
        // Unlike bad_source_action (where an unreadable value must not escalate to permanent
        // deletion), an unreadable update_mode must not silently strand the user on an old
        // version — so the fallback here is the DEFAULT, not the most conservative option.
        assert!(matches!(normalize_update_mode("notify"), UpdateMode::Notify));
        assert!(matches!(normalize_update_mode("off"), UpdateMode::Off));
        assert!(matches!(normalize_update_mode("automatic"), UpdateMode::Automatic));
        assert!(matches!(normalize_update_mode(""), UpdateMode::Automatic));
        assert!(matches!(normalize_update_mode("NOTIFY"), UpdateMode::Automatic));
        assert!(matches!(normalize_update_mode("nonsense"), UpdateMode::Automatic));
    }

    #[test]
    fn skipped_and_notified_versions_round_trip_and_default_to_none() {
        let conn = test_conn();
        // Read-with-default: absent rows, so existing databases need no migration.
        assert_eq!(read_skipped_version(&conn), None);
        assert_eq!(read_notified_version(&conn), None);

        set_skipped_version(&conn, "1.2.0");
        set_notified_version(&conn, "1.3.0");
        assert_eq!(read_skipped_version(&conn).as_deref(), Some("1.2.0"));
        assert_eq!(read_notified_version(&conn).as_deref(), Some("1.3.0"));

        // Overwrite, not append.
        set_skipped_version(&conn, "1.4.0");
        assert_eq!(read_skipped_version(&conn).as_deref(), Some("1.4.0"));
    }

    #[test]
    fn installed_update_survives_notes_containing_delimiters() {
        let conn = test_conn();
        assert!(read_installed(&conn).is_none());

        // Release notes are arbitrary text. A pipe- or newline-delimited encoding would
        // corrupt on exactly the markdown bullets this feature exists to display.
        let notes = "### Fixes\n- fixed a | pipe\n- and a \"quote\"";
        set_installed(&conn, "1.5.0", Some(notes));

        let got = read_installed(&conn).unwrap();
        assert_eq!(got.version, "1.5.0");
        assert_eq!(got.notes.as_deref(), Some(notes));

        clear_installed(&conn);
        assert!(read_installed(&conn).is_none());
    }

    #[test]
    fn installed_update_tolerates_absent_notes() {
        let conn = test_conn();
        set_installed(&conn, "1.6.0", None);
        let got = read_installed(&conn).unwrap();
        assert_eq!(got.version, "1.6.0");
        assert_eq!(got.notes, None);
    }
}
```

Add `mod updater;` to `src-tauri/src/lib.rs` next to the other `mod` declarations (near `mod types;`).

Update the count guard in `src-tauri/src/db.rs:263`:

```rust
        assert_eq!(count, 18);
```

and add an assertion below the other seeded-default assertions in the same test:

```rust
        assert_eq!(setting(&conn, "update_mode").as_deref(), Some("automatic"));
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cd src-tauri && cargo test updater::`
Expected: compile failure — `cannot find function normalize_update_mode`, `cannot find type UpdateMode`, etc.

Run: `cd src-tauri && cargo test init_db_seeds_defaults`
Expected: FAIL — `assertion failed: left: 17, right: 18`.

- [ ] **Step 3: Implement the state layer**

In `src-tauri/src/updater.rs`, above the test module:

```rust
/// How the app behaves when an update exists. Stored in the settings table as a string,
/// like `cleanup_mode` and `bad_source_action`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UpdateMode {
    Automatic,
    Notify,
    Off,
}

impl UpdateMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            UpdateMode::Automatic => "automatic",
            UpdateMode::Notify => "notify",
            UpdateMode::Off => "off",
        }
    }
}

/// Coerce a stored `update_mode` to a known value. Anything other than an exact "notify" or
/// "off" reads as "automatic": a corrupted, empty, or future value must leave the user
/// receiving updates rather than silently stranding them on an old version.
pub fn normalize_update_mode(value: &str) -> UpdateMode {
    match value {
        "notify" => UpdateMode::Notify,
        "off" => UpdateMode::Off,
        _ => UpdateMode::Automatic,
    }
}

/// The update that was installed but whose notes the user has not seen yet. Persisted
/// because on every platform the install is followed by a restart — and on Windows the
/// process is terminated outright — so in-memory notes would be gone at exactly the moment
/// the user is running the new version and would read them.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstalledUpdate {
    pub version: String,
    pub notes: Option<String>,
}

/// Backend-only settings rows. Read-with-default (no seed) so existing databases need no
/// migration and the settings-count guard is untouched. NOT in ALLOWED_KEYS, NOT in the UI —
/// the frontend reaches these only through updater commands. Same discipline as `queue_paused`.
fn read_key(db: &Connection, key: &str) -> Option<String> {
    db.query_row(
        "SELECT value FROM settings WHERE key = ?1",
        params![key],
        |r| r.get::<_, String>(0),
    )
    .ok()
    .filter(|v| !v.is_empty())
}

fn write_key(db: &Connection, key: &str, value: &str) {
    let _ = db.execute(
        "INSERT INTO settings (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = ?2",
        params![key, value],
    );
}

pub(crate) fn read_skipped_version(db: &Connection) -> Option<String> {
    read_key(db, "update_skipped_version")
}

pub(crate) fn set_skipped_version(db: &Connection, version: &str) {
    write_key(db, "update_skipped_version", version);
}

pub(crate) fn read_notified_version(db: &Connection) -> Option<String> {
    read_key(db, "update_notified_version")
}

pub(crate) fn set_notified_version(db: &Connection, version: &str) {
    write_key(db, "update_notified_version", version);
}

/// JSON-encoded, not delimiter-separated: release notes are arbitrary markdown and would
/// corrupt any `|`- or newline-delimited encoding.
pub(crate) fn read_installed(db: &Connection) -> Option<InstalledUpdate> {
    let raw = read_key(db, "update_installed")?;
    serde_json::from_str(&raw).ok()
}

pub(crate) fn set_installed(db: &Connection, version: &str, notes: Option<&str>) {
    let payload = InstalledUpdate {
        version: version.to_string(),
        notes: notes.map(str::to_string),
    };
    if let Ok(json) = serde_json::to_string(&payload) {
        write_key(db, "update_installed", &json);
    }
}

pub(crate) fn clear_installed(db: &Connection) {
    write_key(db, "update_installed", "");
}
```

In `src-tauri/src/db.rs`, add to the `defaults` array after `("bad_source_action", "trash"),`:

```rust
        ("update_mode", "automatic"),
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cd src-tauri && cargo test updater:: && cargo test init_db_seeds_defaults`
Expected: PASS — 4 updater tests, and the seed test green at 18.

- [ ] **Step 5: Wire `update_mode` through the settings command**

In `src-tauri/src/types.rs`, add to `Settings` after `bad_source_action`:

```rust
    pub update_mode: String,
```

In `src-tauri/src/commands/settings.rs` `get_settings`, add the local next to the others:

```rust
    let mut update_mode = String::from("automatic");
```

add the match arm alongside `bad_source_action`:

```rust
            "update_mode" => {
                update_mode = crate::updater::normalize_update_mode(&value).as_str().to_string()
            }
```

add `update_mode` to the returned `Settings { .. }` literal, and add `"update_mode"` to `ALLOWED_KEYS`.

- [ ] **Step 6: Add the settings-plumbing test**

Append to the `tests` module in `src-tauri/src/commands/settings.rs`:

```rust
    #[test]
    fn update_mode_is_writable_and_unknown_values_fall_back_to_automatic() {
        // The Settings UI writes this key via update_setting; the three internal updater keys
        // deliberately are NOT writable this way, so the frontend cannot forge update policy.
        assert!(ALLOWED_KEYS.contains(&"update_mode"));
        assert!(!ALLOWED_KEYS.contains(&"update_skipped_version"));
        assert!(!ALLOWED_KEYS.contains(&"update_notified_version"));
        assert!(!ALLOWED_KEYS.contains(&"update_installed"));
    }
```

- [ ] **Step 7: Run the full Rust suite**

Run: `cd src-tauri && cargo test`
Expected: PASS, 238 + 5 new = 243 passed, 4 ignored.

- [ ] **Step 8: Commit**

```bash
git add src-tauri/src/updater.rs src-tauri/src/lib.rs src-tauri/src/db.rs src-tauri/src/types.rs src-tauri/src/commands/settings.rs
git commit -m "feat: add update_mode setting and persisted updater state"
```

---

### Task 2: The decision core and the `Installer` seam

**Files:**
- Modify: `src-tauri/src/updater.rs`
- Test: inline `#[cfg(test)] mod tests` in `src-tauri/src/updater.rs`

**Interfaces:**
- Consumes: `UpdateMode`, `normalize_update_mode`, the four state accessors from Task 1.
- Produces:
  ```rust
  pub struct AvailableUpdate { pub version: String, pub date: Option<String>, pub notes: Option<String> }

  pub enum UpdateStatus { Idle, Checking, Available, Downloading, WaitingForIdle, ReadyToRestart, Error }

  pub struct UpdateState {
      pub mode: UpdateMode,
      pub status: UpdateStatus,
      pub current_version: String,
      pub available: Option<AvailableUpdate>,
      pub just_installed: Option<InstalledUpdate>,
      pub last_checked: Option<i64>,
      pub last_error: Option<String>,
  }

  pub enum CheckOutcome { Nothing, Notify(AvailableUpdate), Install(AvailableUpdate) }

  pub fn decide(
      mode: UpdateMode,
      found: Option<AvailableUpdate>,
      skipped: Option<&str>,
      notified: Option<&str>,
  ) -> CheckOutcome

  pub fn decide_manual(found: Option<AvailableUpdate>) -> CheckOutcome

  pub trait Installer: Send + Sync {
      fn install(&self, update: &AvailableUpdate) -> Result<(), String>;
  }
  ```

- [ ] **Step 1: Write the failing tests**

Append to the `tests` module in `src-tauri/src/updater.rs`:

```rust
    fn upd(version: &str) -> AvailableUpdate {
        AvailableUpdate {
            version: version.to_string(),
            date: None,
            notes: Some(format!("notes for {version}")),
        }
    }

    #[test]
    fn no_update_found_means_do_nothing_in_every_mode() {
        for mode in [UpdateMode::Automatic, UpdateMode::Notify, UpdateMode::Off] {
            assert!(matches!(decide(mode, None, None, None), CheckOutcome::Nothing));
        }
    }

    #[test]
    fn automatic_installs_and_ignores_a_skipped_version() {
        // Skip is a Notify-mode concept. In Automatic the user has delegated the decision,
        // so honouring a skip list here would silently stop updates they asked to be automatic.
        let outcome = decide(UpdateMode::Automatic, Some(upd("2.0.0")), Some("2.0.0"), None);
        assert!(matches!(outcome, CheckOutcome::Install(u) if u.version == "2.0.0"));
    }

    #[test]
    fn notify_suppresses_a_skipped_version_but_a_newer_one_resurfaces() {
        // "Skip" means "not this one", not "stop telling me".
        let skipped = decide(UpdateMode::Notify, Some(upd("2.0.0")), Some("2.0.0"), None);
        assert!(matches!(skipped, CheckOutcome::Nothing));

        let newer = decide(UpdateMode::Notify, Some(upd("2.1.0")), Some("2.0.0"), None);
        assert!(matches!(newer, CheckOutcome::Notify(u) if u.version == "2.1.0"));
    }

    #[test]
    fn notify_reports_a_version_only_once_even_across_restarts() {
        // `notified` comes from the persisted update_notified_version row, so this holds
        // across process restarts — an in-memory marker would re-notify on every launch.
        let first = decide(UpdateMode::Notify, Some(upd("2.0.0")), None, None);
        assert!(matches!(first, CheckOutcome::Notify(_)));

        let repeat = decide(UpdateMode::Notify, Some(upd("2.0.0")), None, Some("2.0.0"));
        assert!(matches!(repeat, CheckOutcome::Nothing));

        let next_release = decide(UpdateMode::Notify, Some(upd("2.2.0")), None, Some("2.0.0"));
        assert!(matches!(next_release, CheckOutcome::Notify(u) if u.version == "2.2.0"));
    }

    #[test]
    fn off_never_acts_on_a_scheduled_check() {
        let outcome = decide(UpdateMode::Off, Some(upd("2.0.0")), None, None);
        assert!(matches!(outcome, CheckOutcome::Nothing));
    }

    #[test]
    fn a_manual_check_reports_but_never_installs_in_any_mode() {
        // U7. Pressing a button labelled "check" must not commit the user to an install —
        // this is the one behaviour change from the pre-1.0 updater, so it is pinned by a
        // test rather than left implicit in the scheduler's control flow.
        assert!(matches!(decide_manual(None), CheckOutcome::Nothing));

        let outcome = decide_manual(Some(upd("2.0.0")));
        assert!(matches!(outcome, CheckOutcome::Notify(u) if u.version == "2.0.0"));
    }
```

`decide_manual` takes no `skipped`/`notified` arguments at all — the signature is what
guarantees a manual check cannot be silenced by the skip list or the once-per-version marker.
Do not add a test that passes those values in; there is no parameter to pass them to, and a
test asserting a behaviour its own call cannot vary is worse than no test.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cd src-tauri && cargo test updater::`
Expected: compile failure — `cannot find function decide`, `cannot find type CheckOutcome`.

- [ ] **Step 3: Implement the decision core**

Add to `src-tauri/src/updater.rs`:

```rust
/// An update the endpoint is offering. `notes` is `Update::body` — the GitHub release body,
/// copied verbatim into latest.json by tauri-action.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AvailableUpdate {
    pub version: String,
    pub date: Option<String>,
    pub notes: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum UpdateStatus {
    Idle,
    Checking,
    Available,
    Downloading,
    /// Downloaded; the install is held until the queue drains.
    WaitingForIdle,
    /// Installed and awaiting a restart. Unreachable on Windows, where `install()` exits
    /// the process and the installer relaunches the app itself.
    ReadyToRestart,
    Error,
}

/// Everything the Settings panel renders. Emitted whole on the `update-state` event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateState {
    pub mode: UpdateMode,
    pub status: UpdateStatus,
    pub current_version: String,
    pub available: Option<AvailableUpdate>,
    pub just_installed: Option<InstalledUpdate>,
    pub last_checked: Option<i64>,
    pub last_error: Option<String>,
}

/// What a completed check should cause. Pure, so the policy table in the spec is directly
/// testable without a network, a webview, or a real Tauri app.
#[derive(Debug)]
pub enum CheckOutcome {
    Nothing,
    Notify(AvailableUpdate),
    Install(AvailableUpdate),
}

pub fn decide(
    mode: UpdateMode,
    found: Option<AvailableUpdate>,
    skipped: Option<&str>,
    notified: Option<&str>,
) -> CheckOutcome {
    let Some(update) = found else {
        return CheckOutcome::Nothing;
    };

    match mode {
        // Skip is deliberately ignored: in Automatic the user delegated the decision.
        UpdateMode::Automatic => CheckOutcome::Install(update),
        UpdateMode::Off => CheckOutcome::Nothing,
        UpdateMode::Notify => {
            if skipped == Some(update.version.as_str()) {
                return CheckOutcome::Nothing;
            }
            if notified == Some(update.version.as_str()) {
                return CheckOutcome::Nothing;
            }
            CheckOutcome::Notify(update)
        }
    }
}

/// A user-initiated check. Always reports, never installs (U7), and deliberately ignores
/// both the skip list and the once-per-version marker: the user just asked, so hiding the
/// answer would make "Check now" look broken.
pub fn decide_manual(found: Option<AvailableUpdate>) -> CheckOutcome {
    match found {
        Some(update) => CheckOutcome::Notify(update),
        None => CheckOutcome::Nothing,
    }
}

/// The install action, injected so tests drive the real decision-to-action path with a
/// recorder. `Update` cannot be constructed in a test (private fields, updater.rs:602-644)
/// and `check()` needs the network, so without this seam the idle-gate mutation check would
/// only ever exercise a detached predicate and would pass vacuously.
pub trait Installer: Send + Sync {
    fn install(&self, update: &AvailableUpdate) -> Result<(), String>;
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cd src-tauri && cargo test updater::`
Expected: PASS — 9 updater tests.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/updater.rs
git commit -m "feat: add update decision core and Installer seam"
```

---

### Task 3: The idle gate and the queue interlock

**Files:**
- Modify: `src-tauri/src/converter.rs:153-168` (`ConverterState`), `:170-185` (`new`), `:1379-1400` (`run_queue`)
- Modify: `src-tauri/src/updater.rs`
- Test: inline test modules in both files

**Interfaces:**
- Consumes: `AvailableUpdate`, `Installer` from Task 2; `ConverterState` from `converter.rs`.
- Produces:
  ```rust
  // converter.rs — on ConverterState
  pub installing: std::sync::atomic::AtomicBool

  // updater.rs
  pub enum InstallAttempt { Installed, Deferred, Failed(String) }
  pub fn try_install_now(
      conv: &crate::converter::ConverterState,
      installer: &dyn Installer,
      update: &AvailableUpdate,
  ) -> InstallAttempt
  ```

- [ ] **Step 1: Write the failing tests**

Append to the `tests` module in `src-tauri/src/updater.rs`:

```rust
    use crate::converter::ConverterState;
    use std::sync::Mutex as StdMutex;

    /// Records every install it is asked to perform, so a test can assert on what the real
    /// scheduler path actually did — not on what a predicate said it would do.
    struct RecordingInstaller {
        installed: StdMutex<Vec<String>>,
        result: Result<(), String>,
    }

    impl RecordingInstaller {
        fn ok() -> Self {
            Self { installed: StdMutex::new(Vec::new()), result: Ok(()) }
        }
        fn failing() -> Self {
            Self {
                installed: StdMutex::new(Vec::new()),
                result: Err("bundle is corrupt".to_string()),
            }
        }
        fn calls(&self) -> Vec<String> {
            self.installed.lock().unwrap().clone()
        }
    }

    impl Installer for RecordingInstaller {
        fn install(&self, update: &AvailableUpdate) -> Result<(), String> {
            self.installed.lock().unwrap().push(update.version.clone());
            self.result.clone()
        }
    }

    #[test]
    fn an_install_never_runs_while_a_job_is_encoding() {
        // LOAD-BEARING. On Windows, install() calls std::process::exit(0), bypassing the
        // ExitRequested handler that kills HandBrakeCLI — orphaning the encoder, which then
        // keeps writing into the partial output while the next launch's auto-resume deletes
        // that file and starts a second encoder against the same path.
        // Verify by deleting the gate in try_install_now and confirming this goes red.
        let conv = ConverterState::new();
        *conv.is_running.lock().unwrap() = true;

        let installer = RecordingInstaller::ok();
        let attempt = try_install_now(&conv, &installer, &upd("2.0.0"));

        assert!(matches!(attempt, InstallAttempt::Deferred));
        assert!(
            installer.calls().is_empty(),
            "install must not be attempted while the queue is running"
        );
        assert!(
            !conv.installing.load(std::sync::atomic::Ordering::SeqCst),
            "a deferred attempt must not leave the interlock latched"
        );
    }

    #[test]
    fn an_install_runs_when_the_queue_is_idle() {
        let conv = ConverterState::new();
        let installer = RecordingInstaller::ok();

        let attempt = try_install_now(&conv, &installer, &upd("2.0.0"));

        assert!(matches!(attempt, InstallAttempt::Installed));
        assert_eq!(installer.calls(), vec!["2.0.0".to_string()]);
    }

    #[test]
    fn the_interlock_is_held_during_the_install_and_blocks_run_queue() {
        // A download takes minutes and watcher.rs:462 starts run_queue whenever a watched
        // file lands, so "idle when the download began" says nothing about idle at install
        // time. The interlock closes that check-then-act window.
        struct StartsAJobMidInstall<'a>(&'a ConverterState);
        impl Installer for StartsAJobMidInstall<'_> {
            fn install(&self, _u: &AvailableUpdate) -> Result<(), String> {
                assert!(
                    self.0.installing.load(std::sync::atomic::Ordering::SeqCst),
                    "installing must be latched for the whole install"
                );
                // run_queue's claim must refuse while installing is latched.
                assert!(
                    !crate::converter::claim_queue_slot(self.0),
                    "run_queue must not start a job during an install"
                );
                Ok(())
            }
        }

        let conv = ConverterState::new();
        let installer = StartsAJobMidInstall(&conv);
        let attempt = try_install_now(&conv, &installer, &upd("2.0.0"));

        assert!(matches!(attempt, InstallAttempt::Installed));
        assert!(
            !conv.installing.load(std::sync::atomic::Ordering::SeqCst),
            "interlock must be released after the install returns"
        );
    }

    #[test]
    fn a_failed_install_releases_the_interlock_and_reports_why() {
        // On macOS/Linux install() returns Err. If the interlock leaked here, the queue
        // would be permanently wedged by one bad download.
        let conv = ConverterState::new();
        let installer = RecordingInstaller::failing();

        let attempt = try_install_now(&conv, &installer, &upd("2.0.0"));

        assert!(matches!(attempt, InstallAttempt::Failed(e) if e.contains("corrupt")));
        assert!(!conv.installing.load(std::sync::atomic::Ordering::SeqCst));
        assert!(crate::converter::claim_queue_slot(&conv), "queue must be startable again");
    }
```

Append to the `tests` module in `src-tauri/src/converter.rs`:

```rust
    #[test]
    fn claim_queue_slot_refuses_while_an_update_is_installing() {
        // Both sides serialize on the same is_running mutex, so the gate is atomic rather
        // than check-then-act.
        let converter = ConverterState::new();
        converter.installing.store(true, std::sync::atomic::Ordering::SeqCst);
        assert!(!claim_queue_slot(&converter));
        assert!(
            !*converter.is_running.lock().unwrap(),
            "a refused claim must not leave is_running set"
        );

        converter.installing.store(false, std::sync::atomic::Ordering::SeqCst);
        assert!(claim_queue_slot(&converter));
        assert!(*converter.is_running.lock().unwrap());
        assert!(!claim_queue_slot(&converter), "second claim is refused while running");
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cd src-tauri && cargo test claim_queue_slot && cargo test updater::`
Expected: compile failure — `no field installing on ConverterState`, `cannot find function claim_queue_slot`, `cannot find function try_install_now`.

- [ ] **Step 3: Add the interlock field and extract the queue claim**

In `src-tauri/src/converter.rs`, add to `ConverterState` after `shutdown`:

```rust
    /// Latched while an update is installing, so the queue cannot start a job underneath it.
    /// Claimed and released under the `is_running` lock, making the gate atomic against
    /// `run_queue` rather than a check-then-act race.
    pub installing: std::sync::atomic::AtomicBool,
```

and to `ConverterState::new()`:

```rust
            installing: std::sync::atomic::AtomicBool::new(false),
```

Replace the inline claim block in `run_queue` (`converter.rs:1384-1395`) with a call to a new extracted function, and add that function above `run_queue`:

```rust
/// Atomically claims the right to run the queue. Returns false when the queue is already
/// running or an update install holds the interlock. Poison-tolerant: if a prior queue thread
/// panicked while briefly holding this lock, recover the flag rather than propagating the
/// poison and permanently wedging starts.
pub(crate) fn claim_queue_slot(converter: &ConverterState) -> bool {
    let mut running = converter
        .is_running
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if *running || converter.installing.load(std::sync::atomic::Ordering::SeqCst) {
        return false;
    }
    *running = true;
    true
}
```

```rust
pub fn run_queue<R: tauri::Runtime>(
    app: AppHandle<R>,
    db: Arc<Mutex<Connection>>,
    converter: Arc<ConverterState>,
) {
    if !claim_queue_slot(&converter) {
        return;
    }

    std::thread::spawn(move || {
        process_queue(&app, &db, &converter);
    });
}
```

- [ ] **Step 4: Implement `try_install_now`**

Add to `src-tauri/src/updater.rs`:

```rust
/// Outcome of one install attempt.
#[derive(Debug)]
pub enum InstallAttempt {
    Installed,
    /// The queue was busy; the caller keeps the update pending and retries when it drains.
    Deferred,
    Failed(String),
}

/// Installs only when the queue is genuinely idle, holding `installing` for the whole
/// operation so no job can start underneath. Claims under the same `is_running` lock
/// `run_queue` uses, so the two cannot interleave.
///
/// On Windows this never returns — `Update::install()` calls `std::process::exit(0)`. The
/// caller must therefore persist anything the user needs after the restart BEFORE calling.
pub fn try_install_now(
    conv: &crate::converter::ConverterState,
    installer: &dyn Installer,
    update: &AvailableUpdate,
) -> InstallAttempt {
    use std::sync::atomic::Ordering;

    {
        let running = conv.is_running.lock().unwrap_or_else(|e| e.into_inner());
        if *running {
            return InstallAttempt::Deferred;
        }
        conv.installing.store(true, Ordering::SeqCst);
    }

    let result = installer.install(update);
    conv.installing.store(false, Ordering::SeqCst);

    match result {
        Ok(()) => InstallAttempt::Installed,
        Err(e) => InstallAttempt::Failed(e),
    }
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cd src-tauri && cargo test`
Expected: PASS — 243 + 5 = 248 passed, 4 ignored.

- [ ] **Step 6: Commit the green state — required before the mutation check**

The mutation check in Step 7 restores the file with `git checkout`, which reverts to the
**last commit**. Committing here is what makes that restore safe; neutering first would
silently destroy every uncommitted change in this task.

```bash
git add src-tauri/src/converter.rs src-tauri/src/updater.rs
git commit -m "feat: gate update installs on an idle queue with an atomic interlock"
```

- [ ] **Step 7: Mutation-check the idle gate**

Temporarily delete the gate from `try_install_now`:

```rust
    {
        let running = conv.is_running.lock().unwrap_or_else(|e| e.into_inner());
        if *running {
            return InstallAttempt::Deferred;   // <-- delete these three lines
        }
        conv.installing.store(true, Ordering::SeqCst);
    }
```

Run: `cd src-tauri && cargo test an_install_never_runs_while_a_job_is_encoding`
Expected: **FAIL**. If it passes, the test is not exercising the real path — fix the test and
re-run this step before moving on.

Restore the gate:

```bash
git checkout src-tauri/src/updater.rs
cd src-tauri && cargo test
```

Expected: green again, and `git status` clean (Step 6 already committed the work).

Report the mutation-check result — "deleted the gate, test went red, restored, green" — in
the task report. A mutation check whose outcome is not reported did not happen.

---

### Task 4: Scheduler, real installer, commands, and app wiring

**Files:**
- Modify: `src-tauri/src/updater.rs`
- Create: `src-tauri/src/commands/updater.rs`
- Modify: `src-tauri/src/commands/mod.rs`
- Modify: `src-tauri/src/lib.rs` (plugin builder ~:62, `invoke_handler` ~:72, startup block :382-406, `menu-bar-update` listener :239)
- Modify: `src-tauri/src/commands/settings.rs` (`update_setting` hook)
- Test: inline tests in `src-tauri/src/updater.rs`

**Interfaces:**
- Consumes: everything from Tasks 1-3.
- Produces:
  ```rust
  // updater.rs
  pub fn should_check_now(last_checked: Option<i64>, now: i64) -> bool   // 24h wall clock
  pub fn start(app: tauri::AppHandle)

  // commands/updater.rs — all #[tauri::command]
  get_update_state() -> Result<UpdateState, String>
  check_for_update() -> Result<(), String>
  install_update() -> Result<(), String>
  skip_update_version(version: String) -> Result<(), String>
  restart_app()
  ```
  Frontend event: `update-state` with an `UpdateState` payload.

- [ ] **Step 1: Write the failing test for the wall-clock interval**

Append to the `tests` module in `src-tauri/src/updater.rs`:

```rust
    #[test]
    fn checks_are_paced_by_wall_clock_not_uptime() {
        // A tokio sleep is Instant-backed (CLOCK_UPTIME_RAW on macOS) and stops while the
        // machine sleeps, so a 24h timer on a nightly-sleeping laptop stretches into days —
        // defeating the periodic check entirely. Hourly tick + wall-clock comparison instead.
        const DAY: i64 = 24 * 60 * 60;
        let now = 1_800_000_000;

        assert!(should_check_now(None, now), "never checked -> check");
        assert!(!should_check_now(Some(now), now));
        assert!(!should_check_now(Some(now - DAY + 1), now));
        assert!(should_check_now(Some(now - DAY), now));
        assert!(should_check_now(Some(now - 30 * DAY), now));

        // A clock that jumped backwards must not wedge checking forever.
        assert!(should_check_now(Some(now + 5 * DAY), now));
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd src-tauri && cargo test checks_are_paced_by_wall_clock`
Expected: compile failure — `cannot find function should_check_now`.

- [ ] **Step 3: Implement the interval predicate**

Add to `src-tauri/src/updater.rs`:

```rust
const CHECK_INTERVAL_SECS: i64 = 24 * 60 * 60;

/// Whether enough wall-clock time has passed to check again. A future `last_checked` (a
/// backwards clock jump, or a restored backup) also checks, so a bad timestamp cannot
/// permanently disable updates.
pub fn should_check_now(last_checked: Option<i64>, now: i64) -> bool {
    match last_checked {
        None => true,
        Some(t) => now >= t + CHECK_INTERVAL_SECS || t > now,
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cd src-tauri && cargo test checks_are_paced_by_wall_clock`
Expected: PASS.

- [ ] **Step 5: Implement the runtime scheduler and real installer**

Add to `src-tauri/src/updater.rs`:

```rust
use std::sync::atomic::Ordering;
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_updater::UpdaterExt;

/// Live, non-persisted updater state. Everything here is re-derivable after a restart.
#[derive(Default)]
pub struct UpdaterRuntime {
    pub status: std::sync::Mutex<Option<UpdateStatus>>,
    pub available: std::sync::Mutex<Option<AvailableUpdate>>,
    pub last_checked: std::sync::Mutex<Option<i64>>,
    pub last_error: std::sync::Mutex<Option<String>>,
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Wraps the plugin's real install. Separated behind `Installer` so tests never need a
/// constructible `Update`.
struct PluginInstaller {
    update: tauri_plugin_updater::Update,
    bytes: Vec<u8>,
}

impl Installer for PluginInstaller {
    fn install(&self, _update: &AvailableUpdate) -> Result<(), String> {
        // Windows: this never returns — it launches the installer and exits the process.
        self.update.install(&self.bytes).map_err(|e| e.to_string())
    }
}

pub fn emit_state(app: &AppHandle) {
    if let Ok(state) = build_state(app) {
        let _ = app.emit("update-state", state);
    }
}

fn build_state(app: &AppHandle) -> Result<UpdateState, String> {
    let app_state = app.state::<crate::AppState>();
    let conn = app_state.db.lock().map_err(|e| e.to_string())?;

    let mode = normalize_update_mode(
        &read_key(&conn, "update_mode").unwrap_or_else(|| "automatic".into()),
    );
    let runtime = app.state::<Arc<UpdaterRuntime>>();

    Ok(UpdateState {
        mode,
        status: runtime
            .status
            .lock()
            .ok()
            .and_then(|s| *s)
            .unwrap_or(UpdateStatus::Idle),
        current_version: app.package_info().version.to_string(),
        available: runtime.available.lock().ok().and_then(|a| a.clone()),
        just_installed: read_installed(&conn),
        last_checked: runtime.last_checked.lock().ok().and_then(|t| *t),
        last_error: runtime.last_error.lock().ok().and_then(|e| e.clone()),
    })
}

fn set_status(app: &AppHandle, status: UpdateStatus) {
    if let Some(runtime) = app.try_state::<Arc<UpdaterRuntime>>() {
        if let Ok(mut s) = runtime.status.lock() {
            *s = Some(status);
        }
    }
    emit_state(app);
}

/// One full check-and-act cycle. `manual` forces the check regardless of mode and never
/// installs (U7): a button labelled "check" must not commit the user to anything.
pub async fn run_cycle(app: AppHandle, manual: bool) {
    let mode = {
        let Some(app_state) = app.try_state::<crate::AppState>() else {
            return;
        };
        let Ok(conn) = app_state.db.lock() else { return };
        normalize_update_mode(&read_key(&conn, "update_mode").unwrap_or_else(|| "automatic".into()))
    };

    if !manual && mode == UpdateMode::Off {
        return;
    }

    set_status(&app, UpdateStatus::Checking);

    let Ok(updater) = app.updater() else {
        set_status(&app, UpdateStatus::Idle);
        return;
    };

    let found = match updater.check().await {
        Ok(Some(u)) => Some((
            AvailableUpdate {
                version: u.version.clone(),
                date: u.date.map(|d| d.to_string()),
                notes: u.body.clone(),
            },
            u,
        )),
        Ok(None) => None,
        Err(e) => {
            // An offline check is normal; record it without shouting.
            if let Some(runtime) = app.try_state::<Arc<UpdaterRuntime>>() {
                if let Ok(mut err) = runtime.last_error.lock() {
                    *err = Some(e.to_string());
                }
                if let Ok(mut t) = runtime.last_checked.lock() {
                    *t = Some(now_secs());
                }
            }
            set_status(&app, UpdateStatus::Error);
            return;
        }
    };

    if let Some(runtime) = app.try_state::<Arc<UpdaterRuntime>>() {
        if let Ok(mut t) = runtime.last_checked.lock() {
            *t = Some(now_secs());
        }
        if let Ok(mut err) = runtime.last_error.lock() {
            *err = None;
        }
    }

    let (skipped, notified) = {
        let app_state = app.state::<crate::AppState>();
        let Ok(conn) = app_state.db.lock() else { return };
        (read_skipped_version(&conn), read_notified_version(&conn))
    };

    let available = found.as_ref().map(|(a, _)| a.clone());
    let outcome = if manual {
        decide_manual(available.clone())
    } else {
        decide(mode, available.clone(), skipped.as_deref(), notified.as_deref())
    };

    match outcome {
        CheckOutcome::Nothing => {
            if let Some(runtime) = app.try_state::<Arc<UpdaterRuntime>>() {
                if let Ok(mut a) = runtime.available.lock() {
                    *a = None;
                }
            }
            set_status(&app, UpdateStatus::Idle);
        }
        CheckOutcome::Notify(u) => {
            if let Some(runtime) = app.try_state::<Arc<UpdaterRuntime>>() {
                if let Ok(mut a) = runtime.available.lock() {
                    *a = Some(u.clone());
                }
            }
            if !manual {
                let app_state = app.state::<crate::AppState>();
                if let Ok(conn) = app_state.db.lock() {
                    set_notified_version(&conn, &u.version);
                }
                use tauri_plugin_notification::NotificationExt;
                let _ = app
                    .notification()
                    .builder()
                    .title("ConvertBar")
                    .body(format!("ConvertBar {} is available", u.version))
                    .show();
            }
            set_status(&app, UpdateStatus::Available);
        }
        CheckOutcome::Install(u) => {
            if let Some((_, raw)) = found {
                perform_install(app.clone(), u, raw).await;
            }
        }
    }
}

/// Downloads, then installs behind the idle gate. Persists `update_installed` BEFORE the
/// install call, because on Windows the call terminates the process and the installer
/// relaunches the app — nothing after it would ever run.
pub async fn perform_install(app: AppHandle, meta: AvailableUpdate, raw: tauri_plugin_updater::Update) {
    set_status(&app, UpdateStatus::Downloading);

    let bytes = match raw.download(|_, _| {}, || {}).await {
        Ok(b) => b,
        Err(e) => {
            if let Some(runtime) = app.try_state::<Arc<UpdaterRuntime>>() {
                if let Ok(mut err) = runtime.last_error.lock() {
                    *err = Some(e.to_string());
                }
            }
            set_status(&app, UpdateStatus::Error);
            return;
        }
    };

    {
        let app_state = app.state::<crate::AppState>();
        if let Ok(conn) = app_state.db.lock() {
            set_installed(&conn, &meta.version, meta.notes.as_deref());
        }
    }

    let conv = app.state::<Arc<crate::converter::ConverterState>>();
    let installer = PluginInstaller { update: raw, bytes };

    match try_install_now(&conv, &installer, &meta) {
        InstallAttempt::Installed => {
            use tauri_plugin_notification::NotificationExt;
            let _ = app
                .notification()
                .builder()
                .title("ConvertBar")
                .body(format!("Updated to {} — restart ConvertBar to apply", meta.version))
                .show();
            set_status(&app, UpdateStatus::ReadyToRestart);
        }
        InstallAttempt::Deferred => {
            set_status(&app, UpdateStatus::WaitingForIdle);
        }
        InstallAttempt::Failed(e) => {
            let app_state = app.state::<crate::AppState>();
            if let Ok(conn) = app_state.db.lock() {
                clear_installed(&conn);
            }
            if let Some(runtime) = app.try_state::<Arc<UpdaterRuntime>>() {
                if let Ok(mut err) = runtime.last_error.lock() {
                    *err = Some(e.clone());
                }
            }
            eprintln!("updater: install of {} failed: {e}", meta.version);
            use tauri_plugin_notification::NotificationExt;
            let _ = app
                .notification()
                .builder()
                .title("ConvertBar")
                .body(format!(
                    "Update to {} failed to install — still on the current version",
                    meta.version
                ))
                .show();
            set_status(&app, UpdateStatus::Error);
        }
    }
}

/// Startup check plus an hourly tick that only acts once 24h of wall clock have passed.
pub fn start(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        run_cycle(app.clone(), false).await;

        loop {
            tokio::time::sleep(std::time::Duration::from_secs(60 * 60)).await;

            let last = app
                .try_state::<Arc<UpdaterRuntime>>()
                .and_then(|r| r.last_checked.lock().ok().and_then(|t| *t));

            if should_check_now(last, now_secs()) {
                run_cycle(app.clone(), false).await;
            }
        }
    });
}

/// Retries a deferred install when the queue drains. Wakes on both "idle" and "error" —
/// `final_run_status` (converter.rs:704-710) emits "error", never "idle", for any run in
/// which a job failed — and re-checks actual state rather than trusting the event, because
/// the emit happens before `RunningGuard` clears `is_running` and because
/// `pause_after_current` and the low-disk pause both emit "idle" with jobs still queued.
pub fn on_queue_status(app: &AppHandle, status: &str) {
    if status != "idle" && status != "error" {
        return;
    }
    let Some(runtime) = app.try_state::<Arc<UpdaterRuntime>>() else {
        return;
    };
    let waiting = matches!(
        runtime.status.lock().ok().and_then(|s| *s),
        Some(UpdateStatus::WaitingForIdle)
    );
    if !waiting {
        return;
    }

    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        // Re-derive the update rather than trusting the stale event.
        run_cycle(app, false).await;
    });
}
```

- [ ] **Step 6: Create the command module**

Create `src-tauri/src/commands/updater.rs`:

```rust
use std::sync::Arc;
use tauri::{AppHandle, Manager};

use crate::updater::{self, UpdateState, UpdaterRuntime};

#[tauri::command]
pub fn get_update_state(app: AppHandle) -> Result<UpdateState, String> {
    updater::build_state_public(&app)
}

#[tauri::command]
pub async fn check_for_update(app: AppHandle) -> Result<(), String> {
    // Manual: forced regardless of mode, and never installs (U7).
    updater::run_cycle(app, true).await;
    Ok(())
}

#[tauri::command]
pub async fn install_update(app: AppHandle) -> Result<(), String> {
    updater::install_pending(app).await
}

#[tauri::command]
pub fn skip_update_version(app: AppHandle, version: String) -> Result<(), String> {
    let state = app.state::<crate::AppState>();
    let conn = state.db.lock().map_err(|e| e.to_string())?;
    updater::set_skipped_version_public(&conn, &version);
    drop(conn);

    if let Some(runtime) = app.try_state::<Arc<UpdaterRuntime>>() {
        if let Ok(mut a) = runtime.available.lock() {
            *a = None;
        }
    }
    updater::clear_status(&app);
    Ok(())
}

#[tauri::command]
pub fn restart_app(app: AppHandle) {
    // Routes through ExitRequested with RESTART_EXIT_CODE, so kill_active_child still runs.
    app.restart();
}
```

Add these thin public wrappers to `src-tauri/src/updater.rs` (the internal helpers stay `pub(crate)`):

```rust
pub fn build_state_public(app: &AppHandle) -> Result<UpdateState, String> {
    build_state(app)
}

pub fn set_skipped_version_public(db: &Connection, version: &str) {
    set_skipped_version(db, version);
}

pub fn clear_status(app: &AppHandle) {
    set_status(app, UpdateStatus::Idle);
}

/// Installs the update the last check found. Used by the panel's "Install and restart".
/// With a busy queue: pause after the current job, then let `on_queue_status` retry.
pub async fn install_pending(app: AppHandle) -> Result<(), String> {
    let Ok(updater) = app.updater() else {
        return Err("updater unavailable".into());
    };
    let Ok(Some(raw)) = updater.check().await else {
        return Err("no update available".into());
    };
    let meta = AvailableUpdate {
        version: raw.version.clone(),
        date: raw.date.map(|d| d.to_string()),
        notes: raw.body.clone(),
    };

    let conv = app.state::<Arc<crate::converter::ConverterState>>();
    let busy = *conv.is_running.lock().unwrap_or_else(|e| e.into_inner());
    if busy {
        if let Ok(mut flag) = conv.pause_after_current.lock() {
            *flag = true;
        }
    }
    drop(conv);

    perform_install(app, meta, raw).await;
    Ok(())
}
```

Add to `src-tauri/src/commands/mod.rs`:

```rust
pub mod updater;
```

- [ ] **Step 7: Wire into `lib.rs`**

Register the runtime state and the `on_before_exit` hook. Replace the plugin line at `lib.rs:62`:

```rust
        .plugin(
            tauri_plugin_updater::Builder::new()
                .on_before_exit(|| {
                    // Windows install() calls std::process::exit(0), bypassing the
                    // ExitRequested handler — without this, HandBrakeCLI is orphaned and
                    // keeps encoding into the partial output while the next launch's
                    // auto-resume deletes that file and starts a second encoder on it.
                    if let Some(app) = APP_HANDLE.get() {
                        let conv = app.state::<Arc<ConverterState>>();
                        converter::kill_active_child(&conv);
                    }
                })
                .build(),
        )
```

If no global handle exists yet, add one near the top of `lib.rs`:

```rust
static APP_HANDLE: std::sync::OnceLock<AppHandle> = std::sync::OnceLock::new();
```

and set it inside `setup`: `let _ = APP_HANDLE.set(app.handle().clone());`

Add the five commands to `invoke_handler` alongside the existing ones:

```rust
            commands::updater::get_update_state,
            commands::updater::check_for_update,
            commands::updater::install_update,
            commands::updater::skip_update_version,
            commands::updater::restart_app,
```

In `setup`, manage the runtime state before starting the scheduler:

```rust
            app.manage(Arc::new(updater::UpdaterRuntime::default()));
```

Replace the startup updater block at `lib.rs:382-406` entirely with:

```rust
            // All update policy lives in `updater` — mode, scheduling, skip, and the idle gate.
            updater::start(app.handle().clone());
```

Delete the now-unused `update_install_notification` helper at `lib.rs:37-46` and its test.

In the `menu-bar-update` listener (`lib.rs:239`), after the existing match on `update.status`, add:

```rust
                    updater::on_queue_status(&handle_for_updater, &update.status);
```

cloning an `AppHandle` into that closure as the existing code does for its own captures.

In `src-tauri/src/commands/settings.rs` `update_setting`, add the hook after the `watch_skip_marker` block:

```rust
    // Let a mode change take effect immediately: a user who sees "update available" and
    // switches to Automatic should not wait for the next hourly tick.
    if key == "update_mode" {
        crate::updater::emit_state(&app);
        if value == "automatic" {
            let handle = app.clone();
            tauri::async_runtime::spawn(async move {
                crate::updater::run_cycle(handle, false).await;
            });
        }
    }
```

- [ ] **Step 8: Build and run the full suite**

Run: `cd src-tauri && cargo build 2>&1 | tail -30`
Expected: compiles. Fix any borrow/lifetime errors from the `AppHandle` captures.

Run: `cd src-tauri && cargo test`
Expected: PASS — all prior tests plus `checks_are_paced_by_wall_clock_not_uptime`.

Run: `cd src-tauri && cargo fmt --check`
Expected: clean.

- [ ] **Step 9: Commit**

```bash
git add src-tauri/src/updater.rs src-tauri/src/commands/updater.rs src-tauri/src/commands/mod.rs src-tauri/src/lib.rs src-tauri/src/commands/settings.rs
git commit -m "feat: move update policy into a Rust scheduler with commands and events"
```

---

### Task 5: Frontend types and the `useUpdate` hook

**Files:**
- Modify: `src/lib/tauri.ts` (types + `commands`)
- Create: `src/hooks/useUpdate.ts`
- Test: create `src/hooks/useUpdate.test.ts`

**Interfaces:**
- Consumes: the five commands and the `update-state` event from Task 4.
- Produces:
  ```ts
  export type UpdateMode = "automatic" | "notify" | "off";
  export type UpdateStatus = "idle" | "checking" | "available" | "downloading"
    | "waitingForIdle" | "readyToRestart" | "error";
  export interface AvailableUpdate { version: string; date: string | null; notes: string | null }
  export interface InstalledUpdate { version: string; notes: string | null }
  export interface UpdateState {
    mode: UpdateMode; status: UpdateStatus; current_version: string;
    available: AvailableUpdate | null; just_installed: InstalledUpdate | null;
    last_checked: number | null; last_error: string | null;
  }
  // useUpdate() -> { state, checkNow, install, skip, restart }
  ```

- [ ] **Step 1: Write the failing test**

Create `src/hooks/useUpdate.test.ts`:

```ts
import { describe, it, expect, vi, beforeEach } from "vitest";
import { renderHook, act, waitFor } from "@testing-library/react";
import { useUpdate } from "./useUpdate";
import { commands } from "../lib/tauri";

let emit: ((payload: unknown) => void) | null = null;

vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async (_name: string, cb: (e: { payload: unknown }) => void) => {
    emit = (payload) => cb({ payload });
    return () => { emit = null; };
  }),
}));

const baseState = {
  mode: "automatic" as const,
  status: "idle" as const,
  current_version: "1.0.0",
  available: null,
  just_installed: null,
  last_checked: null,
  last_error: null,
};

describe("useUpdate", () => {
  beforeEach(() => {
    emit = null;
    vi.spyOn(commands, "getUpdateState").mockResolvedValue(baseState);
    vi.spyOn(commands, "checkForUpdate").mockResolvedValue(undefined);
    vi.spyOn(commands, "skipUpdateVersion").mockResolvedValue(undefined);
  });

  it("seeds from the backend so a tab remount shows current state", async () => {
    // The panel must not start blank and then pop — the backend is the source of truth.
    const { result } = renderHook(() => useUpdate());
    await waitFor(() => expect(result.current.state?.current_version).toBe("1.0.0"));
  });

  it("re-renders from the update-state event rather than polling", async () => {
    const { result } = renderHook(() => useUpdate());
    await waitFor(() => expect(result.current.state).not.toBeNull());

    act(() => {
      emit?.({
        ...baseState,
        status: "available",
        available: { version: "1.1.0", date: null, notes: "### Fixes\n- a fix" },
      });
    });

    await waitFor(() => {
      expect(result.current.state?.status).toBe("available");
      expect(result.current.state?.available?.version).toBe("1.1.0");
    });
  });

  it("passes the available version to skip so the backend records the right one", async () => {
    // Skip means "not this one" — sending the wrong version would silence a different release.
    const { result } = renderHook(() => useUpdate());
    await waitFor(() => expect(result.current.state).not.toBeNull());

    act(() => {
      emit?.({
        ...baseState,
        status: "available",
        available: { version: "1.1.0", date: null, notes: null },
      });
    });
    await waitFor(() => expect(result.current.state?.available).not.toBeNull());

    await act(async () => { await result.current.skip(); });
    expect(commands.skipUpdateVersion).toHaveBeenCalledWith("1.1.0");
  });

  it("does not call skip when nothing is available", async () => {
    const { result } = renderHook(() => useUpdate());
    await waitFor(() => expect(result.current.state).not.toBeNull());

    await act(async () => { await result.current.skip(); });
    expect(commands.skipUpdateVersion).not.toHaveBeenCalled();
  });
});
```

- [ ] **Step 2: Run test to verify it fails**

Run: `npm test -- useUpdate`
Expected: FAIL — `Failed to resolve import "./useUpdate"`.

- [ ] **Step 3: Add types and commands**

Append to `src/lib/tauri.ts` before `export const commands`:

```ts
export type UpdateMode = "automatic" | "notify" | "off";

export type UpdateStatus =
  | "idle"
  | "checking"
  | "available"
  | "downloading"
  | "waitingForIdle"
  | "readyToRestart"
  | "error";

export interface AvailableUpdate {
  version: string;
  date: string | null;
  notes: string | null;
}

export interface InstalledUpdate {
  version: string;
  notes: string | null;
}

// Mirrors src-tauri/src/updater.rs UpdateState exactly.
export interface UpdateState {
  mode: UpdateMode;
  status: UpdateStatus;
  current_version: string;
  available: AvailableUpdate | null;
  just_installed: InstalledUpdate | null;
  last_checked: number | null;
  last_error: string | null;
}
```

and inside the `commands` object, before the closing brace:

```ts
  getUpdateState: () => invoke<UpdateState>("get_update_state"),
  checkForUpdate: () => invoke<void>("check_for_update"),
  installUpdate: () => invoke<void>("install_update"),
  skipUpdateVersion: (version: string) =>
    invoke<void>("skip_update_version", { version }),
  restartApp: () => invoke<void>("restart_app"),
```

- [ ] **Step 4: Implement the hook**

Create `src/hooks/useUpdate.ts`:

```ts
import { useState, useEffect, useCallback, useRef } from "react";
import { listen } from "@tauri-apps/api/event";
import { commands, type UpdateState } from "../lib/tauri";

export function useUpdate() {
  const [state, setState] = useState<UpdateState | null>(null);
  // The panel's actions read the freshest state without re-creating callbacks on every event.
  const latest = useRef<UpdateState | null>(null);
  latest.current = state;

  useEffect(() => {
    let alive = true;
    commands
      .getUpdateState()
      .then((s) => { if (alive) setState(s); })
      .catch(() => { /* backend not ready yet; the event will seed us */ });

    const unlisten = listen<UpdateState>("update-state", (e) => {
      if (alive) setState(e.payload);
    });

    return () => {
      alive = false;
      unlisten.then((fn) => fn()).catch(() => {});
    };
  }, []);

  const checkNow = useCallback(() => commands.checkForUpdate(), []);
  const install = useCallback(() => commands.installUpdate(), []);
  const restart = useCallback(() => commands.restartApp(), []);

  const skip = useCallback(async () => {
    const version = latest.current?.available?.version;
    if (!version) return;
    await commands.skipUpdateVersion(version);
  }, []);

  return { state, checkNow, install, skip, restart };
}
```

- [ ] **Step 5: Run test to verify it passes**

Run: `npm test -- useUpdate`
Expected: PASS — 4 tests.

- [ ] **Step 6: Commit**

```bash
git add src/lib/tauri.ts src/hooks/useUpdate.ts src/hooks/useUpdate.test.ts
git commit -m "feat: add useUpdate hook and update IPC types"
```

---

### Task 6: `UpdatePanel`, `SettingsPage` extraction, and the tab badge

**Files:**
- Create: `src/components/UpdatePanel.tsx`, `src/components/UpdatePanel.test.tsx`
- Modify: `src/pages/SettingsPage.tsx` (remove lines 4-5 imports, 42 state, 446-489 group)
- Modify: `src/pages/SettingsPage.test.tsx` (remove mocks at 11-12)
- Modify: `src/components/TabBar.tsx`, `src/components/TabBar.test.tsx`
- Modify: `src/App.tsx` (pass the badge flag)
- Modify: `src/App.css` (panel styles)

**Interfaces:**
- Consumes: `useUpdate` from Task 5; `AppSettings.update_mode` from Task 1.
- Produces: `<UpdatePanel />` (self-contained, no props); `TabBarProps` gains `updateAvailable: boolean`.

- [ ] **Step 1: Write the failing tests**

Create `src/components/UpdatePanel.test.tsx`:

```tsx
import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import UpdatePanel from "./UpdatePanel";

const mockUpdate = {
  state: null as unknown,
  checkNow: vi.fn(),
  install: vi.fn(),
  skip: vi.fn(),
  restart: vi.fn(),
};

vi.mock("../hooks/useUpdate", () => ({ useUpdate: () => mockUpdate }));
vi.mock("../lib/tauri", () => ({
  commands: { updateSetting: vi.fn().mockResolvedValue(undefined) },
}));

const base = {
  mode: "automatic",
  status: "idle",
  current_version: "1.0.0",
  available: null,
  just_installed: null,
  last_checked: null,
  last_error: null,
};

describe("UpdatePanel", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    mockUpdate.state = base;
  });

  it("renders release notes as plain text, not markup", async () => {
    // Notes are arbitrary release-body markdown from a remote endpoint. Rendering them as
    // HTML would be an injection surface for content the app does not control.
    mockUpdate.state = {
      ...base,
      status: "available",
      available: { version: "1.1.0", date: null, notes: "<img src=x onerror=alert(1)>" },
    };
    render(<UpdatePanel />);
    expect(await screen.findByText("<img src=x onerror=alert(1)>")).toBeInTheDocument();
    expect(document.querySelector("img")).toBeNull();
  });

  it("shows a deferred install instead of appearing to do nothing", async () => {
    // Without this the user presses Install during an encode and sees no change at all.
    mockUpdate.state = {
      ...base,
      status: "waitingForIdle",
      available: { version: "1.1.0", date: null, notes: null },
    };
    render(<UpdatePanel />);
    expect(await screen.findByText(/will install when the queue finishes/i)).toBeInTheDocument();
  });

  it("offers Install and Skip only when an update is available", async () => {
    render(<UpdatePanel />);
    expect(screen.queryByRole("button", { name: /install/i })).toBeNull();
    expect(screen.queryByRole("button", { name: /skip/i })).toBeNull();

    mockUpdate.state = {
      ...base,
      status: "available",
      available: { version: "1.1.0", date: null, notes: "notes" },
    };
    render(<UpdatePanel />);
    await userEvent.click(await screen.findByRole("button", { name: /install/i }));
    expect(mockUpdate.install).toHaveBeenCalled();
  });

  it("shows what's new after an automatic install so the changelog is not lost", async () => {
    mockUpdate.state = {
      ...base,
      status: "readyToRestart",
      just_installed: { version: "1.1.0", notes: "### Fixes\n- a fix" },
    };
    render(<UpdatePanel />);
    expect(await screen.findByText(/what's new in 1\.1\.0/i)).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: /restart/i }));
    expect(mockUpdate.restart).toHaveBeenCalled();
  });

  it("keeps an error visible instead of clearing it on a timer", async () => {
    mockUpdate.state = { ...base, status: "error", last_error: "network unreachable" };
    render(<UpdatePanel />);
    expect(await screen.findByText(/network unreachable/i)).toBeInTheDocument();
  });
});
```

Append to `src/components/TabBar.test.tsx`:

```tsx
  it("badges the Settings tab when an update is pending", () => {
    // A missed OS notification must not be the only signal that an update is waiting.
    const { rerender } = render(
      <TabBar activeTab="queue" onTabChange={() => {}} isAdding={false} updateAvailable={false} />,
    );
    expect(screen.queryByLabelText(/update available/i)).toBeNull();

    rerender(
      <TabBar activeTab="queue" onTabChange={() => {}} isAdding={false} updateAvailable={true} />,
    );
    expect(screen.getByLabelText(/update available/i)).toBeInTheDocument();
  });
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `npm test -- UpdatePanel TabBar`
Expected: FAIL — cannot resolve `./UpdatePanel`; TabBar test fails on the missing badge.

- [ ] **Step 3: Implement `UpdatePanel`**

Create `src/components/UpdatePanel.tsx`:

```tsx
import { useUpdate } from "../hooks/useUpdate";
import { commands, type UpdateMode } from "../lib/tauri";

const MODES: { value: UpdateMode; label: string }[] = [
  { value: "automatic", label: "Automatic" },
  { value: "notify", label: "Notify me" },
  { value: "off", label: "Manual only" },
];

function relativeTime(unixSeconds: number): string {
  const mins = Math.max(0, Math.round((Date.now() / 1000 - unixSeconds) / 60));
  if (mins < 1) return "just now";
  if (mins < 60) return `${mins} minute${mins === 1 ? "" : "s"} ago`;
  const hours = Math.round(mins / 60);
  if (hours < 24) return `${hours} hour${hours === 1 ? "" : "s"} ago`;
  const days = Math.round(hours / 24);
  return `${days} day${days === 1 ? "" : "s"} ago`;
}

export default function UpdatePanel() {
  const { state, checkNow, install, skip, restart } = useUpdate();
  if (!state) return null;

  const busy = state.status === "checking" || state.status === "downloading";

  return (
    <div className="setting-group">
      <label className="setting-label">
        Updates <span className="version-label">v{state.current_version}</span>
      </label>

      <div className="setting-radios">
        {MODES.map((m) => (
          <label key={m.value} className="radio-label">
            <input
              type="radio"
              name="update_mode"
              checked={state.mode === m.value}
              onChange={() => commands.updateSetting("update_mode", m.value)}
            />
            {m.label}
          </label>
        ))}
      </div>

      <div className="setting-row">
        <button className="btn btn-small" onClick={() => checkNow()} disabled={busy}>
          Check now
        </button>
        <span className="update-status">
          {state.status === "checking" && "Checking…"}
          {state.status === "downloading" && "Downloading…"}
          {state.status !== "checking" &&
            state.status !== "downloading" &&
            state.last_checked !== null &&
            `Last checked ${relativeTime(state.last_checked)}`}
        </span>
      </div>

      {state.last_error && <div className="update-error">{state.last_error}</div>}

      {state.status === "waitingForIdle" && (
        <div className="update-deferred">
          Update downloaded — will install when the queue finishes
        </div>
      )}

      {state.status === "available" && state.available && (
        <div className="update-available">
          <div className="update-version">
            Version {state.available.version}
            {state.available.date && <span className="update-date"> · {state.available.date}</span>}
          </div>
          {state.available.notes && <pre className="update-notes">{state.available.notes}</pre>}
          <div className="setting-row">
            <button className="btn btn-small" onClick={() => install()}>
              Install and restart
            </button>
            <button className="btn btn-small" onClick={() => skip()}>
              Skip this version
            </button>
          </div>
        </div>
      )}

      {state.just_installed && state.status !== "available" && (
        <div className="update-available">
          <div className="update-version">What's new in {state.just_installed.version}</div>
          {state.just_installed.notes && (
            <pre className="update-notes">{state.just_installed.notes}</pre>
          )}
          {state.status === "readyToRestart" && (
            <button className="btn btn-small" onClick={() => restart()}>
              Restart now
            </button>
          )}
        </div>
      )}
    </div>
  );
}
```

`<pre>{...}</pre>` renders text content, never markup — that is what satisfies the injection test.

- [ ] **Step 4: Replace the old Updates group**

In `src/pages/SettingsPage.tsx`:
- Delete the imports on lines 4-5 (`check` from `@tauri-apps/plugin-updater`, `relaunch` from `@tauri-apps/plugin-process`).
- Delete the `updateStatus` state (line 42) and the `appVersion` state and its `useEffect` (lines 43, 53) — `UpdatePanel` owns both now.
- Replace the whole `setting-group` block at lines 446-489 with `<UpdatePanel />`, importing it at the top.
- Remove the now-unused `listen` and `getVersion` imports **only if** nothing else in the file uses them. Check first:

```bash
grep -n "listen(\|getVersion(" src/pages/SettingsPage.tsx
```

If that prints nothing after the block is replaced, delete both import lines.

In `src/pages/SettingsPage.test.tsx`, delete the two `vi.mock` lines (11-12) and add:

```tsx
vi.mock("../components/UpdatePanel", () => ({ default: () => null }));
```

- [ ] **Step 5: Add the badge**

In `src/components/TabBar.tsx`, add `updateAvailable: boolean` to `TabBarProps`, accept it in the signature, and render inside the tab button:

```tsx
          {tab.label}
          {tab.id === "settings" && updateAvailable && (
            <span className="tab-badge" aria-label="Update available" />
          )}
```

In `src/App.tsx`, call `useUpdate()` and pass `updateAvailable={state?.status === "available"}` to `<TabBar />`.

Add to `src/App.css`:

```css
.tab-badge {
  display: inline-block;
  width: 6px;
  height: 6px;
  margin-left: 4px;
  border-radius: 50%;
  background: var(--accent, #4a9eff);
  vertical-align: super;
}

.update-notes {
  max-height: 140px;
  overflow-y: auto;
  white-space: pre-wrap;
  word-break: break-word;
  font-size: 11px;
  margin: 6px 0;
  padding: 6px;
  background: rgba(127, 127, 127, 0.1);
  border-radius: 4px;
}

.update-error {
  color: var(--error, #ff6b6b);
  font-size: 11px;
  margin-top: 4px;
}

.update-deferred {
  font-size: 11px;
  opacity: 0.8;
  margin-top: 4px;
}
```

- [ ] **Step 6: Run the full frontend suite**

Run: `npm test`
Expected: PASS — 142 baseline + 4 (Task 5) + 5 + 1 = 152.

Run: `npm run build`
Expected: clean `tsc` + vite build.

- [ ] **Step 7: Commit**

```bash
git add src/components/UpdatePanel.tsx src/components/UpdatePanel.test.tsx src/components/TabBar.tsx src/components/TabBar.test.tsx src/pages/SettingsPage.tsx src/pages/SettingsPage.test.tsx src/App.tsx src/App.css
git commit -m "feat: add update panel with release notes and mode control"
```

---

### Task 7: Drop the now-unused JS plugins and ACL grants

**Files:**
- Modify: `src-tauri/capabilities/default.json:19-21`
- Modify: `package.json`, `package-lock.json`

**Interfaces:**
- Consumes: Task 6 (the frontend must already be free of both imports).
- Produces: nothing new.

- [ ] **Step 1: Verify nothing still imports the plugins**

Run: `grep -rn "plugin-process\|plugin-updater" src/`
Expected: **no output**. If anything matches, Task 6 is incomplete — stop and finish it.

- [ ] **Step 2: Remove the ACL grants**

In `src-tauri/capabilities/default.json`, delete these three lines from `permissions`:

```json
    "updater:allow-check",
    "updater:allow-download-and-install",
    "process:allow-restart"
```

Leave `core:event:allow-listen` and `core:event:allow-unlisten` — `useUpdate` listens for `update-state`. Leave `core:app:allow-version` if anything still calls `getVersion()`; grep first:

Run: `grep -rn "getVersion" src/`
If there are no matches, also remove `"core:app:allow-version"`.

- [ ] **Step 3: Uninstall the JS halves only**

Run: `npm uninstall @tauri-apps/plugin-updater @tauri-apps/plugin-process`

Do **not** run `npm run tauri remove` — the Rust crates `tauri-plugin-updater` and `tauri-plugin-process` are still required and that command would remove them too.

- [ ] **Step 4: Verify both sides still build**

Run: `npm run build`
Expected: clean.

Run: `cd src-tauri && cargo build 2>&1 | tail -5`
Expected: compiles — the Rust `tauri-plugin-updater` dependency is untouched.

Run: `npm test`
Expected: PASS, 152.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/capabilities/default.json package.json package-lock.json
git commit -m "chore: drop frontend updater/process plugins and their ACL grants"
```

---

### Task 8: Generate real release notes in CI

Independent of Tasks 1-7 — it can be implemented in any order relative to them.

**Files:**
- Create: `scripts/release-notes.sh`
- Modify: `.github/workflows/build.yml:43-52`
- Test: create `src/test/release-notes.test.ts`

**Interfaces:**
- Consumes: nothing.
- Produces: `scripts/release-notes.sh <prev-tag> <current-tag>` printing markdown to stdout.

- [ ] **Step 1: Write the failing test**

Create `src/test/release-notes.test.ts`, modelled on the existing `src/test/release-script.test.ts`:

```ts
import { describe, it, expect } from "vitest";
import { execFileSync } from "node:child_process";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";

const SCRIPT = resolve(__dirname, "../../scripts/release-notes.sh");

function repoWithCommits(subjects: string[], tagAt: number): string {
  const dir = mkdtempSync(join(tmpdir(), "notes-"));
  const git = (...args: string[]) =>
    execFileSync("git", args, { cwd: dir, encoding: "utf8" });

  git("init", "-q", "-b", "main");
  git("config", "user.email", "t@example.com");
  git("config", "user.name", "T");
  git("config", "commit.gpgsign", "false");

  git("commit", "-q", "--allow-empty", "-m", "chore: base");
  if (tagAt === 0) git("tag", "v0.1.0");

  subjects.forEach((s, i) => {
    git("commit", "-q", "--allow-empty", "-m", s);
    if (tagAt === i + 1) git("tag", "v0.1.0");
  });
  git("tag", "v0.2.0");
  return dir;
}

function run(dir: string, prev: string, current: string): string {
  return execFileSync("bash", [SCRIPT, prev, current], { cwd: dir, encoding: "utf8" });
}

describe("release-notes.sh", () => {
  it("groups feat, fix and perf under headings and names each change", () => {
    // These are what a user actually wants to read before deciding to install.
    const dir = repoWithCommits(
      ["feat: add dark mode", "fix: stop crash on empty queue", "perf: cache probes"],
      0,
    );
    try {
      const out = run(dir, "v0.1.0", "v0.2.0");
      expect(out).toContain("### Features");
      expect(out).toContain("add dark mode");
      expect(out).toContain("### Fixes");
      expect(out).toContain("stop crash on empty queue");
      expect(out).toContain("### Performance");
      expect(out).toContain("cache probes");
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  });

  it("collapses maintenance commits to a count instead of listing them", () => {
    // Without the collapse, dependabot subjects dominate every release body and bury the
    // handful of changes a user cares about.
    const dir = repoWithCommits(
      [
        "feat: add dark mode",
        "chore(deps): bump react",
        "chore(deps): bump vite",
        "docs: tweak readme",
        "refactor: tidy converter",
      ],
      0,
    );
    try {
      const out = run(dir, "v0.1.0", "v0.2.0");
      expect(out).toMatch(/4 maintenance (change|commit)/i);
      expect(out).not.toContain("bump react");
      expect(out).not.toContain("tweak readme");
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  });

  it("always appends the full-changelog compare link", () => {
    const dir = repoWithCommits(["feat: add dark mode"], 0);
    try {
      const out = run(dir, "v0.1.0", "v0.2.0");
      expect(out).toContain(
        "**Full changelog**: https://github.com/rhurling/convertbar/compare/v0.1.0...v0.2.0",
      );
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  });

  it("prints Initial release. when there is no previous tag", () => {
    // Preserves the existing build.yml behaviour for the very first release.
    const dir = repoWithCommits(["feat: first"], -1);
    try {
      const out = run(dir, "", "v0.2.0");
      expect(out.trim()).toBe("Initial release.");
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  });

  it("still emits the compare link when the range is empty", () => {
    // A retagged or empty range must not produce a blank release body.
    const dir = repoWithCommits([], 0);
    try {
      const out = run(dir, "v0.1.0", "v0.1.0");
      expect(out).toContain("**Full changelog**");
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  });

  it("emits no bare line that GitHub Actions would reject as an output", () => {
    // $GITHUB_OUTPUT parses `key=value`; a heredoc protects multi-line values, but a line
    // equal to the delimiter would terminate it early.
    const dir = repoWithCommits(["feat: add dark mode"], 0);
    try {
      const out = run(dir, "v0.1.0", "v0.2.0");
      expect(out.split("\n")).not.toContain("CONVERTBAR_NOTES_EOF");
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  });
});
```

- [ ] **Step 2: Run test to verify it fails**

Run: `npm test -- release-notes`
Expected: FAIL — `ENOENT` on `scripts/release-notes.sh`.

- [ ] **Step 3: Implement the script**

Create `scripts/release-notes.sh`:

```bash
#!/usr/bin/env bash
# Generates the GitHub release body from conventional-commit subjects.
# tauri-action copies this body verbatim into latest.json's "notes" field, which is what
# the in-app update panel displays — so this is user-facing text, not a changelog for us.
set -euo pipefail

PREV="${1:-}"
CURRENT="${2:-}"
REPO_URL="https://github.com/rhurling/convertbar"

if [ -z "$PREV" ]; then
  echo "Initial release."
  exit 0
fi

subjects=$(git log --pretty=format:%s "${PREV}..${CURRENT}" 2>/dev/null || true)

emit_group() {
  local heading="$1" pattern="$2" matched
  matched=$(printf '%s\n' "$subjects" | grep -E "$pattern" || true)
  [ -z "$matched" ] && return 0
  printf '### %s\n' "$heading"
  # Strip the type prefix and any scope: "feat(ui): x" -> "x"
  printf '%s\n' "$matched" | sed -E 's/^[a-z]+(\([^)]*\))?!?: *//' | sed 's/^/- /'
  printf '\n'
}

emit_group "Features"    '^feat(\([^)]*\))?!?: '
emit_group "Fixes"       '^fix(\([^)]*\))?!?: '
emit_group "Performance" '^perf(\([^)]*\))?!?: '

# Everything else is collapsed to a count: dependabot subjects would otherwise dominate
# the body and bury the changes a user actually cares about.
other=$(printf '%s\n' "$subjects" \
  | grep -vE '^(feat|fix|perf)(\([^)]*\))?!?: ' \
  | grep -c . || true)
if [ "${other:-0}" -gt 0 ]; then
  printf '%s maintenance changes (dependencies, docs, tests, internals).\n\n' "$other"
fi

printf '**Full changelog**: %s/compare/%s...%s\n' "$REPO_URL" "$PREV" "$CURRENT"
```

Run: `chmod +x scripts/release-notes.sh`

- [ ] **Step 4: Run test to verify it passes**

Run: `npm test -- release-notes`
Expected: PASS — 6 tests.

- [ ] **Step 5: Wire it into the workflow**

Replace `.github/workflows/build.yml:43-52` with:

```yaml
      - name: Build release body
        id: release_body
        shell: bash
        run: |
          PREV=$(git tag --sort=-version:refname | grep '^v' | sed -n '2p')
          CURRENT=$(git describe --tags --abbrev=0)
          # Heredoc, not `echo "body=..."`: a multi-line value written as key=value makes the
          # runner fail the step with "Invalid format", and any notes line containing `=`
          # would be parsed as a further output.
          {
            echo "body<<CONVERTBAR_NOTES_EOF"
            ./scripts/release-notes.sh "$PREV" "$CURRENT"
            echo "CONVERTBAR_NOTES_EOF"
          } >> "$GITHUB_OUTPUT"
```

`releaseBody: ${{ steps.release_body.outputs.body }}` at line 94 needs no change — expressions substitute after YAML parse, so multi-line values pass through intact.

- [ ] **Step 6: Verify the script against this repo's real history**

Run: `./scripts/release-notes.sh v0.19.1 v1.0.0`
Expected: a Maintenance count plus the compare link (that range is docs-only), and no crash.

Run: `npm test`
Expected: PASS — 158.

- [ ] **Step 7: Commit**

```bash
git add scripts/release-notes.sh src/test/release-notes.test.ts .github/workflows/build.yml
git commit -m "feat: generate release notes from conventional commits in CI"
```

---

## Final verification

- [ ] Run `cd src-tauri && cargo test` — expect all green, 4 ignored.
- [ ] Run `cd src-tauri && cargo fmt --check` — expect clean.
- [ ] Run `npm test` — expect all green.
- [ ] Run `npm run build` — expect clean.
- [ ] Run `grep -rn "plugin-updater\|plugin-process" src/` — expect no matches.
- [ ] Confirm `src-tauri/capabilities/default.json` no longer grants `updater:*` or `process:allow-restart`.
- [ ] Manual smoke test on macOS: open Settings, switch each mode, press Check now, confirm the status line updates and no install starts.

## Known gaps, deliberately not covered

- **The Windows and Linux install paths cannot be tested locally.** Task 4's Windows-specific behavior (no return from `install()`, installer-driven relaunch, `on_before_exit` firing) is verified by code inspection against `tauri-plugin-updater-2.10.1/src/updater.rs:787-865` only. A real Windows smoke test remains outstanding.
- **`update-state` event delivery is not covered by a Rust test** — the mock runtime has no updater config. `useUpdate.test.ts` covers the frontend half of that contract.
- **"`update_installed` is written before `install()`" is enforced by code placement, not by a test.** The spec's testing section asks for a test here, but the ordering lives in `perform_install`, which needs a real `AppHandle` and a constructible `Update` — neither is available in the mock runtime. A test that only asserted the ordering of two calls in a hand-rolled double would not be exercising `perform_install`, so it would pass vacuously. The safeguard is instead the explanatory comment at the call site; a reviewer moving that write below the install must be caught in review. **This is the weakest link in the Windows path — flag it if a better seam becomes available.**
- **`PREV` tag derivation** is unchanged and still assumes the tag being built is the highest version (see the spec's CI section for why that is acceptable).
