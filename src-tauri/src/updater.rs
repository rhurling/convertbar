use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_updater::UpdaterExt;

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

/// Outcome of one install attempt.
#[derive(Debug)]
pub enum InstallAttempt {
    Installed,
    /// The queue was busy; the caller keeps the update pending and retries when it drains.
    Deferred,
    Failed(String),
}

/// Clears `installing` on every exit from an install attempt, including an unwinding panic
/// from `installer.install()`. Without this, a panicking installer would leave `installing`
/// stuck true forever — `claim_queue_slot` would then refuse to start the queue for the rest
/// of the process's life. Mirrors `RunningGuard` (converter.rs), the same fix for the sibling
/// `is_running` flag.
struct InstallingGuard<'a>(&'a crate::converter::ConverterState);

impl Drop for InstallingGuard<'_> {
    fn drop(&mut self) {
        self.0
            .installing
            .store(false, std::sync::atomic::Ordering::SeqCst);
    }
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

    let result = {
        // Clears `installing` when this scope ends, whether install() returns or panics.
        let _installing = InstallingGuard(conv);
        installer.install(update)
    };

    match result {
        Ok(()) => InstallAttempt::Installed,
        Err(e) => InstallAttempt::Failed(e),
    }
}

/// Live, non-persisted updater state. Everything here is re-derivable after a restart.
#[derive(Default)]
pub struct UpdaterRuntime {
    pub status: std::sync::Mutex<Option<UpdateStatus>>,
    pub available: std::sync::Mutex<Option<AvailableUpdate>>,
    pub last_checked: std::sync::Mutex<Option<i64>>,
    pub last_error: std::sync::Mutex<Option<String>>,
    /// Single-flight latch over the whole check → download → install sequence.
    /// `try_install_now` only samples `is_running` on entry, so two concurrent callers could
    /// both clear its gate; this makes every path that can reach it mutually exclusive.
    busy: std::sync::atomic::AtomicBool,
}

/// Releases the single-flight latch on every exit from a cycle, including an unwinding panic.
/// Owns an `Arc` rather than borrowing, so it can be held across `.await` points. Its existence
/// is also the proof `perform_install` demands that the latch is held.
struct CycleGuard(Arc<UpdaterRuntime>);

impl Drop for CycleGuard {
    fn drop(&mut self) {
        self.0
            .busy
            .store(false, std::sync::atomic::Ordering::SeqCst);
    }
}

/// Claims the right to run one update cycle, or `None` when one is already in flight. Refuses
/// rather than queues: an hourly tick must not pile up behind a slow download.
fn try_begin_cycle(runtime: &Arc<UpdaterRuntime>) -> Option<CycleGuard> {
    if runtime.busy.swap(true, std::sync::atomic::Ordering::SeqCst) {
        None
    } else {
        Some(CycleGuard(runtime.clone()))
    }
}

fn runtime_of(app: &AppHandle) -> Option<Arc<UpdaterRuntime>> {
    app.try_state::<Arc<UpdaterRuntime>>()
        .map(|s| s.inner().clone())
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn describe(update: &tauri_plugin_updater::Update) -> AvailableUpdate {
    AvailableUpdate {
        version: update.version.clone(),
        date: update.date.as_ref().map(|d| d.to_string()),
        notes: update.body.clone(),
    }
}

fn notify(app: &AppHandle, body: String) {
    use tauri_plugin_notification::NotificationExt;
    let _ = app
        .notification()
        .builder()
        .title("ConvertBar")
        .body(body)
        .show();
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

/// The plugin updater with our teardown hook attached.
///
/// The hook lives on the `UpdaterBuilder`, not on the plugin `Builder` (which has no
/// `on_before_exit`): the builder's hook is what `Update::install` invokes immediately before
/// `std::process::exit(0)` on Windows.
fn build_updater(app: &AppHandle) -> Option<tauri_plugin_updater::Updater> {
    let handle = app.clone();
    app.updater_builder()
        .on_before_exit(move || {
            // Windows `install()` calls std::process::exit(0), bypassing the ExitRequested
            // handler — without this, HandBrakeCLI is orphaned and keeps encoding into the
            // partial output while the next launch's auto-resume deletes that file and starts
            // a second encoder against the same path.
            if let Some(conv) = handle.try_state::<Arc<crate::converter::ConverterState>>() {
                crate::converter::kill_active_child(&conv);
            }
            // Setting a hook replaces the one `updater_builder()` installs by default, so the
            // default teardown has to be re-done here.
            handle.cleanup_before_exit();
        })
        .build()
        .ok()
}

pub fn emit_state(app: &AppHandle) {
    if let Ok(state) = build_state(app) {
        let _ = app.emit("update-state", state);
    }
}

fn build_state(app: &AppHandle) -> Result<UpdateState, String> {
    let app_state = app
        .try_state::<crate::AppState>()
        .ok_or_else(|| "app state unavailable".to_string())?;

    let (mode, just_installed) = {
        let conn = app_state.db.lock().map_err(|e| e.to_string())?;
        (
            normalize_update_mode(
                &read_key(&conn, "update_mode").unwrap_or_else(|| "automatic".into()),
            ),
            read_installed(&conn),
        )
    };

    let runtime = runtime_of(app).ok_or_else(|| "updater runtime unavailable".to_string())?;

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
        just_installed,
        last_checked: runtime.last_checked.lock().ok().and_then(|t| *t),
        last_error: runtime.last_error.lock().ok().and_then(|e| e.clone()),
    })
}

fn set_status(app: &AppHandle, status: UpdateStatus) {
    if let Some(runtime) = runtime_of(app) {
        if let Ok(mut s) = runtime.status.lock() {
            *s = Some(status);
        }
    }
    emit_state(app);
}

/// One full check-and-act cycle. `manual` forces the check regardless of mode and never
/// installs (U7): a button labelled "check" must not commit the user to anything.
pub async fn run_cycle(app: AppHandle, manual: bool) {
    let Some(runtime) = runtime_of(&app) else {
        return;
    };
    let Some(cycle) = try_begin_cycle(&runtime) else {
        return;
    };

    let mode = {
        let Some(app_state) = app.try_state::<crate::AppState>() else {
            return;
        };
        let Ok(conn) = app_state.db.lock() else {
            return;
        };
        normalize_update_mode(&read_key(&conn, "update_mode").unwrap_or_else(|| "automatic".into()))
    };

    if !manual && mode == UpdateMode::Off {
        return;
    }

    set_status(&app, UpdateStatus::Checking);

    let Some(updater) = build_updater(&app) else {
        set_status(&app, UpdateStatus::Idle);
        return;
    };

    let found = match updater.check().await {
        Ok(Some(raw)) => Some((describe(&raw), raw)),
        Ok(None) => None,
        Err(e) => {
            // An offline check is normal; record it without shouting.
            if let Ok(mut err) = runtime.last_error.lock() {
                *err = Some(e.to_string());
            }
            if let Ok(mut t) = runtime.last_checked.lock() {
                *t = Some(now_secs());
            }
            set_status(&app, UpdateStatus::Error);
            return;
        }
    };

    if let Ok(mut t) = runtime.last_checked.lock() {
        *t = Some(now_secs());
    }
    if let Ok(mut err) = runtime.last_error.lock() {
        *err = None;
    }

    let (skipped, notified) = {
        let Some(app_state) = app.try_state::<crate::AppState>() else {
            return;
        };
        let Ok(conn) = app_state.db.lock() else {
            return;
        };
        (read_skipped_version(&conn), read_notified_version(&conn))
    };

    let available = found.as_ref().map(|(meta, _)| meta.clone());
    let outcome = if manual {
        decide_manual(available)
    } else {
        decide(mode, available, skipped.as_deref(), notified.as_deref())
    };

    match outcome {
        CheckOutcome::Nothing => {
            if let Ok(mut a) = runtime.available.lock() {
                *a = None;
            }
            set_status(&app, UpdateStatus::Idle);
        }
        CheckOutcome::Notify(u) => {
            if let Ok(mut a) = runtime.available.lock() {
                *a = Some(u.clone());
            }
            if !manual {
                // Only a scheduled check burns the once-per-version marker; a manual check
                // must stay repeatable.
                if let Some(app_state) = app.try_state::<crate::AppState>() {
                    if let Ok(conn) = app_state.db.lock() {
                        set_notified_version(&conn, &u.version);
                    }
                }
                notify(&app, format!("ConvertBar {} is available", u.version));
            }
            set_status(&app, UpdateStatus::Available);
        }
        CheckOutcome::Install(u) => {
            if let Ok(mut a) = runtime.available.lock() {
                *a = Some(u.clone());
            }
            if let Some((_, raw)) = found {
                perform_install(&app, &cycle, u, raw).await;
            }
        }
    }
}

/// Downloads, then installs behind the idle gate. Persists `update_installed` BEFORE the
/// install call, because on Windows the call terminates the process and the installer
/// relaunches the app — nothing after it would ever run.
///
/// `_cycle` is proof the caller holds the single-flight latch. `try_install_now` does not
/// self-serialize (it only samples `is_running` on entry), so this must never run twice
/// concurrently — taking the guard by reference makes that unrepresentable.
async fn perform_install(
    app: &AppHandle,
    _cycle: &CycleGuard,
    meta: AvailableUpdate,
    raw: tauri_plugin_updater::Update,
) {
    set_status(app, UpdateStatus::Downloading);

    let bytes = match raw.download(|_, _| {}, || {}).await {
        Ok(b) => b,
        Err(e) => {
            if let Some(runtime) = runtime_of(app) {
                if let Ok(mut err) = runtime.last_error.lock() {
                    *err = Some(e.to_string());
                }
            }
            set_status(app, UpdateStatus::Error);
            return;
        }
    };

    // LOAD-BEARING ORDERING: written BEFORE the install. On Windows `Update::install` launches
    // the installer and calls std::process::exit(0) — it never returns, so a write placed after
    // it would never happen and the post-restart "What's new" panel would have nothing to show.
    // Rolled back below on every path where the install did not actually happen.
    // Untestable automatically: `install()` cannot be exercised in a unit test.
    if let Some(app_state) = app.try_state::<crate::AppState>() {
        if let Ok(conn) = app_state.db.lock() {
            set_installed(&conn, &meta.version, meta.notes.as_deref());
        }
    }

    let Some(conv) = app.try_state::<Arc<crate::converter::ConverterState>>() else {
        return;
    };
    let installer = PluginInstaller { update: raw, bytes };

    match try_install_now(&conv, &installer, &meta) {
        InstallAttempt::Installed => {
            notify(
                app,
                format!("Updated to {} — restart ConvertBar to apply", meta.version),
            );
            set_status(app, UpdateStatus::ReadyToRestart);
        }
        InstallAttempt::Deferred => {
            // Nothing was installed, so the pre-written marker must not survive: a restart for
            // any other reason would otherwise show "What's new" for a version not running.
            rollback_installed(app);
            set_status(app, UpdateStatus::WaitingForIdle);
        }
        InstallAttempt::Failed(e) => {
            rollback_installed(app);
            if let Some(runtime) = runtime_of(app) {
                if let Ok(mut err) = runtime.last_error.lock() {
                    *err = Some(e.clone());
                }
            }
            eprintln!("updater: install of {} failed: {e}", meta.version);
            notify(
                app,
                format!(
                    "Update to {} failed to install — still on the current version",
                    meta.version
                ),
            );
            set_status(app, UpdateStatus::Error);
        }
    }
}

fn rollback_installed(app: &AppHandle) {
    if let Some(app_state) = app.try_state::<crate::AppState>() {
        if let Ok(conn) = app_state.db.lock() {
            clear_installed(&conn);
        }
    }
}

/// Installs the update the last check found. Backs the panel's "Install and restart" and the
/// idle retry. With a busy queue: pause after the current job, then let `on_queue_status` retry
/// once it drains.
pub async fn install_pending(app: AppHandle) -> Result<(), String> {
    let Some(runtime) = runtime_of(&app) else {
        return Err("updater unavailable".into());
    };
    let Some(cycle) = try_begin_cycle(&runtime) else {
        return Err("an update operation is already running".into());
    };

    let Some(updater) = build_updater(&app) else {
        return Err("updater unavailable".into());
    };
    let raw = match updater.check().await {
        Ok(Some(raw)) => raw,
        Ok(None) => return Err("no update available".into()),
        Err(e) => return Err(e.to_string()),
    };
    let meta = describe(&raw);

    {
        let Some(conv) = app.try_state::<Arc<crate::converter::ConverterState>>() else {
            return Err("converter unavailable".into());
        };
        let busy = *conv.is_running.lock().unwrap_or_else(|e| e.into_inner());
        if busy {
            // Drain rather than interrupt: the running job finishes, then `on_queue_status`
            // retries the install.
            if let Ok(mut flag) = conv.pause_after_current.lock() {
                *flag = true;
            }
        }
    }

    perform_install(&app, &cycle, meta, raw).await;
    Ok(())
}

const CHECK_INTERVAL_SECS: i64 = 24 * 60 * 60;
const TICK_INTERVAL_SECS: u64 = 60 * 60;

/// Whether enough wall-clock time has passed to check again. A future `last_checked` (a
/// backwards clock jump, or a restored backup) also checks, so a bad timestamp cannot
/// permanently disable updates.
pub fn should_check_now(last_checked: Option<i64>, now: i64) -> bool {
    match last_checked {
        None => true,
        Some(t) => now >= t + CHECK_INTERVAL_SECS || t > now,
    }
}

pub fn build_state_public(app: &AppHandle) -> Result<UpdateState, String> {
    build_state(app)
}

pub fn set_skipped_version_public(db: &Connection, version: &str) {
    set_skipped_version(db, version);
}

pub fn clear_status(app: &AppHandle) {
    set_status(app, UpdateStatus::Idle);
}

fn spawn_cycle(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        run_cycle(app, false).await;
    });
}

fn current_status(app: &AppHandle) -> Option<UpdateStatus> {
    runtime_of(app).and_then(|r| r.status.lock().ok().and_then(|s| *s))
}

fn spawn_install_retry(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        // `install_pending`, not `run_cycle`: WaitingForIdle is only ever reached from a
        // decided install, and re-running the mode policy here would demote a user's explicit
        // "Install and restart" in Notify mode back into a notification.
        let _ = install_pending(app).await;
    });
}

/// Startup check plus an hourly tick that only acts once 24h of wall clock have passed.
///
/// The tick is a coarse poll on purpose: the pacing lives in `should_check_now`, which compares
/// wall-clock timestamps. A 24h timer would be `Instant`-backed and stop while the machine
/// sleeps, stretching "daily" into whatever a nightly-sleeping laptop makes of it.
pub fn start(app: AppHandle) {
    spawn_cycle(app.clone());

    std::thread::spawn(move || loop {
        std::thread::sleep(std::time::Duration::from_secs(TICK_INTERVAL_SECS));

        match current_status(&app) {
            // Installed, waiting on the user to restart. Checking again would find the same
            // version (this process still reports the old one) and reinstall it, dropping the
            // panel out of its "restart to apply" state and re-downloading for nothing.
            Some(UpdateStatus::ReadyToRestart) => continue,
            // Backstop for an install whose drain event never arrived — the queue picked up
            // another job before it went idle — so a pending install can't sit forever.
            Some(UpdateStatus::WaitingForIdle) => {
                spawn_install_retry(app.clone());
                continue;
            }
            _ => {}
        }

        let last = runtime_of(&app).and_then(|r| r.last_checked.lock().ok().and_then(|t| *t));
        if should_check_now(last, now_secs()) {
            spawn_cycle(app.clone());
        }
    });
}

/// Retries a deferred install when the queue drains. Wakes on both "idle" and "error" —
/// `final_run_status` (converter.rs) emits "error", never "idle", for any run in which a job
/// failed — and re-derives the update from the endpoint rather than trusting the stale event.
pub fn on_queue_status(app: &AppHandle, status: &str) {
    if status != "idle" && status != "error" {
        return;
    }
    if current_status(app) != Some(UpdateStatus::WaitingForIdle) {
        return;
    }

    let app = app.clone();
    std::thread::spawn(move || {
        // The queue emits its terminal status BEFORE `RunningGuard` clears `is_running`, and
        // the low-disk / pause-after-current paths emit it with jobs still queued. Waiting for
        // the flag to actually clear stops the retry from immediately re-deferring against a
        // queue that has not finished unwinding — which would leave nothing to wake it again.
        if !wait_for_idle_queue(&app) {
            return;
        }
        spawn_install_retry(app);
    });
}

/// Polls `is_running` until the queue is genuinely idle. Bounded: a queue that immediately
/// picks up another job leaves the install pending for the next drain (or the hourly backstop)
/// rather than blocking here.
fn wait_for_idle_queue(app: &AppHandle) -> bool {
    let Some(conv) = app.try_state::<Arc<crate::converter::ConverterState>>() else {
        return false;
    };
    for _ in 0..40 {
        if !*conv.is_running.lock().unwrap_or_else(|e| e.into_inner()) {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    false
}

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
        assert!(matches!(
            normalize_update_mode("notify"),
            UpdateMode::Notify
        ));
        assert!(matches!(normalize_update_mode("off"), UpdateMode::Off));
        assert!(matches!(
            normalize_update_mode("automatic"),
            UpdateMode::Automatic
        ));
        assert!(matches!(normalize_update_mode(""), UpdateMode::Automatic));
        assert!(matches!(
            normalize_update_mode("NOTIFY"),
            UpdateMode::Automatic
        ));
        assert!(matches!(
            normalize_update_mode("nonsense"),
            UpdateMode::Automatic
        ));
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
            assert!(matches!(
                decide(mode, None, None, None),
                CheckOutcome::Nothing
            ));
        }
    }

    #[test]
    fn automatic_installs_and_ignores_a_skipped_version() {
        // Skip is a Notify-mode concept. In Automatic the user has delegated the decision,
        // so honouring a skip list here would silently stop updates they asked to be automatic.
        let outcome = decide(
            UpdateMode::Automatic,
            Some(upd("2.0.0")),
            Some("2.0.0"),
            None,
        );
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
            Self {
                installed: StdMutex::new(Vec::new()),
                result: Ok(()),
            }
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
        assert!(
            crate::converter::claim_queue_slot(&conv),
            "queue must be startable again"
        );
    }

    #[test]
    fn only_one_update_cycle_runs_at_a_time() {
        // LOAD-BEARING. `try_install_now` does not self-serialize — it samples `is_running`
        // once on entry, so two concurrent callers could both clear its gate and install at
        // the same time. This latch is what makes it a single call site: every path that can
        // reach an install (startup, hourly tick, mode change, manual check, manual install,
        // idle retry) claims it first, and `perform_install` cannot be called without the
        // resulting guard.
        let runtime = Arc::new(UpdaterRuntime::default());

        let first = try_begin_cycle(&runtime).expect("the first cycle claims the latch");
        assert!(
            try_begin_cycle(&runtime).is_none(),
            "a concurrent cycle must be refused, not queued behind a slow download"
        );

        drop(first);
        assert!(
            try_begin_cycle(&runtime).is_some(),
            "the guard must release the latch when the cycle ends"
        );
    }

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

    #[test]
    fn installing_is_released_even_if_the_installer_panics() {
        // Same failure mode RunningGuard (converter.rs) protects is_running against, for the
        // sibling `installing` flag. Without InstallingGuard, a panicking installer would leave
        // `installing` latched forever and claim_queue_slot would refuse the queue for the rest
        // of the process's life.
        struct PanickingInstaller;
        impl Installer for PanickingInstaller {
            fn install(&self, _update: &AvailableUpdate) -> Result<(), String> {
                panic!("simulated installer crash");
            }
        }

        let conv = ConverterState::new();

        // Suppress the expected panic's default stderr print so test output stays pristine.
        let prev_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            try_install_now(&conv, &PanickingInstaller, &upd("2.0.0"))
        }));
        std::panic::set_hook(prev_hook);

        assert!(result.is_err(), "the installer did panic");
        assert!(
            !conv.installing.load(std::sync::atomic::Ordering::SeqCst),
            "the guard must clear installing even though install() panicked"
        );
        assert!(
            crate::converter::claim_queue_slot(&conv),
            "queue must be startable again after a panicking install"
        );
    }
}
