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

/// An install that has been decided on but is waiting for the queue to drain, and *who* decided
/// it. The `WaitingForIdle` status alone cannot carry this: every consumer would have to infer
/// intent from a bare status bit, and they infer it differently — the retry needs to know whether
/// pausing a running queue is permitted, and a mode change needs to drop a scheduler-decided
/// install without touching one the user explicitly asked for.
#[derive(Debug, Clone)]
pub struct PendingInstall {
    pub update: AvailableUpdate,
    /// True when the user pressed "Install and restart", false when the scheduler decided in
    /// Automatic mode.
    pub user_requested: bool,
}

/// Why an install is being attempted, and therefore whether it may *create* a pending install or
/// only continue one that already exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallTrigger {
    /// The user pressed "Install and restart" — a fresh decision, so it registers its own.
    UserRequested,
    /// A wake path continuing an existing deferral. Must not create one: the deferral it was
    /// dispatched for may have been cancelled while it waited for the queue.
    Retry,
}

/// What a retry of a pending install should do given the queue's current state.
#[derive(Debug, PartialEq, Eq)]
pub enum RetryAction {
    /// The queue is idle — install straight away.
    InstallNow,
    /// The user asked to install now, so the running job may be drained to make room.
    DrainThenInstall,
    /// The queue is busy and nobody asked to interrupt it. Stay pending and wait for a drain.
    StayPending,
}

/// Whether a retry may stop a running queue to make room for the install.
///
/// Only a user who pressed "Install and restart" gets that. `pause_after_current` is consumed by
/// `process_queue` (converter.rs:1277-1290) into a *persisted* `set_queue_paused(true)` plus a
/// `break` with jobs still queued — so arming it on the scheduler's behalf would stop a user's
/// batch mid-run and leave it paused across the update restart. The scheduler's job is to install
/// when the machine is free, never to free it.
pub fn retry_action(user_requested: bool, queue_running: bool) -> RetryAction {
    match (queue_running, user_requested) {
        (false, _) => RetryAction::InstallNow,
        (true, true) => RetryAction::DrainThenInstall,
        (true, false) => RetryAction::StayPending,
    }
}

/// Whether an already-pending install survives a change of update mode.
///
/// A deferral the scheduler decided on in Automatic mode must not outlive the user turning
/// automatic installs off: the retry paths install a pending update regardless of mode, so
/// keeping it would install the update on the next queue drain — exactly what Off exists to
/// prevent. A deferral the user asked for survives any mode change; they pressed the button.
pub fn pending_survives_mode(pending: &PendingInstall, mode: UpdateMode) -> bool {
    mode == UpdateMode::Automatic || pending.user_requested
}

/// Whether an already-pending install survives the user skipping `skipped_version`.
///
/// Version-matched rather than a blanket cancel: the retry paths install a pending update without
/// re-consulting the skip list, so skipping the pending version has to cancel it — but skipping an
/// older version must not cancel a pending install of a newer one.
pub fn pending_survives_skip(pending: &PendingInstall, skipped_version: &str) -> bool {
    pending.update.version != skipped_version
}

/// Why a user-initiated check is refused, if it is.
#[derive(Debug, PartialEq, Eq)]
pub enum ManualCheckBlock {
    /// An install is downloaded and waiting for the queue to drain.
    InstallPending,
    /// An install completed and the app is waiting to be restarted.
    AwaitingRestart,
}

impl ManualCheckBlock {
    pub fn message(&self) -> &'static str {
        match self {
            ManualCheckBlock::InstallPending => {
                "an update is already waiting to install once the queue is idle"
            }
            ManualCheckBlock::AwaitingRestart => {
                "an update is installed — restart ConvertBar to apply it"
            }
        }
    }
}

/// Whether a manual "Check now" may run.
///
/// It must not while an install is pending or already installed. A check walks the status through
/// `Checking` and out to `Available`/`Idle`, and that status is what the panel reads — so a check
/// would replace "restart to apply" with "update available" for a version already installed, and
/// would hide a pending install behind a banner. In Notify mode it would silently demote a user's
/// explicit "Install and restart" into a notification, which the spec forbids.
pub fn manual_check_block(
    status: Option<UpdateStatus>,
    pending: Option<&PendingInstall>,
) -> Option<ManualCheckBlock> {
    if status == Some(UpdateStatus::ReadyToRestart) {
        return Some(ManualCheckBlock::AwaitingRestart);
    }
    if pending.is_some() {
        return Some(ManualCheckBlock::InstallPending);
    }
    None
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
    /// The install waiting for an idle queue, if any. Held separately from `status` because the
    /// retry paths need the requester's intent, not just the fact of a deferral.
    pub pending: std::sync::Mutex<Option<PendingInstall>>,
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

fn runtime_of<R: tauri::Runtime>(app: &AppHandle<R>) -> Option<Arc<UpdaterRuntime>> {
    app.try_state::<Arc<UpdaterRuntime>>()
        .map(|s| s.inner().clone())
}

fn pending_of<R: tauri::Runtime>(app: &AppHandle<R>) -> Option<PendingInstall> {
    runtime_of(app).and_then(|r| r.pending.lock().ok().and_then(|p| p.clone()))
}

fn set_pending<R: tauri::Runtime>(app: &AppHandle<R>, value: Option<PendingInstall>) {
    if let Some(runtime) = runtime_of(app) {
        if let Ok(mut p) = runtime.pending.lock() {
            *p = value;
        }
    }
}

/// Whether the registered pending install is still live and still names `version`. The liveness
/// check every irreversible step re-reads, rather than trusting a value captured at dispatch.
fn pending_matches<R: tauri::Runtime>(app: &AppHandle<R>, version: &str) -> bool {
    pending_of(app).is_some_and(|p| p.update.version == version)
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

fn notify<R: tauri::Runtime>(app: &AppHandle<R>, body: String) {
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
fn build_updater<R: tauri::Runtime>(app: &AppHandle<R>) -> Option<tauri_plugin_updater::Updater> {
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

pub fn emit_state<R: tauri::Runtime>(app: &AppHandle<R>) {
    if let Ok(state) = build_state(app) {
        let _ = app.emit("update-state", state);
    }
}

fn build_state<R: tauri::Runtime>(app: &AppHandle<R>) -> Result<UpdateState, String> {
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

fn set_status<R: tauri::Runtime>(app: &AppHandle<R>, status: UpdateStatus) {
    if let Some(runtime) = runtime_of(app) {
        if let Ok(mut s) = runtime.status.lock() {
            *s = Some(status);
        }
    }
    emit_state(app);
}

/// One full check-and-act cycle. `manual` forces the check regardless of mode and never
/// installs (U7): a button labelled "check" must not commit the user to anything.
pub async fn run_cycle<R: tauri::Runtime>(app: AppHandle<R>, manual: bool) -> Result<(), String> {
    let Some(runtime) = runtime_of(&app) else {
        return Err("updater unavailable".into());
    };
    let Some(cycle) = try_begin_cycle(&runtime) else {
        return Err("an update operation is already running".into());
    };

    // Checked under the latch, so no concurrent cycle can be mutating either input. A manual
    // check that walked the status past WaitingForIdle / ReadyToRestart would orphan the pending
    // install: both wake paths key off exactly those states.
    if manual {
        if let Some(block) = manual_check_block(current_status(&app), pending_of(&app).as_ref()) {
            return Err(block.message().to_string());
        }
    }

    let mode = {
        let Some(app_state) = app.try_state::<crate::AppState>() else {
            return Err("app state unavailable".into());
        };
        let Ok(conn) = app_state.db.lock() else {
            return Err("settings unavailable".into());
        };
        normalize_update_mode(&read_key(&conn, "update_mode").unwrap_or_else(|| "automatic".into()))
    };

    if !manual && mode == UpdateMode::Off {
        return Ok(());
    }

    set_status(&app, UpdateStatus::Checking);

    let Some(updater) = build_updater(&app) else {
        set_status(&app, UpdateStatus::Idle);
        return Err("updater unavailable".into());
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
            return Err(e.to_string());
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
            return Err("app state unavailable".into());
        };
        let Ok(conn) = app_state.db.lock() else {
            return Err("settings unavailable".into());
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
                // Registered before the download starts, so a mode change or a skip arriving
                // mid-download can cancel it. The scheduler decided this, not the user, so it
                // may never pause a running queue.
                set_pending(
                    &app,
                    Some(PendingInstall {
                        update: u.clone(),
                        user_requested: false,
                    }),
                );
                perform_install(&app, &cycle, u, raw).await;
            }
        }
    }

    Ok(())
}

/// Downloads, then installs behind the idle gate. Persists `update_installed` BEFORE the
/// install call, because on Windows the call terminates the process and the installer
/// relaunches the app — nothing after it would ever run.
///
/// `_cycle` is proof the caller holds the single-flight latch. `try_install_now` does not
/// self-serialize (it only samples `is_running` on entry), so this must never run twice
/// concurrently — taking the guard by reference makes that unrepresentable.
/// The caller registers the pending install BEFORE calling this and it stays registered for the
/// whole sequence, so a cancellation landing mid-download (mode set to Off, version skipped) is
/// visible here and wins. `Update::install` is irreversible — on Windows it terminates the
/// process — so consent is re-read immediately before it, not merely sampled at dispatch.
async fn perform_install<R: tauri::Runtime>(
    app: &AppHandle<R>,
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
            // A download that failed is not an install waiting to happen; dropping it keeps the
            // hourly backstop from re-fetching the whole bundle against a broken endpoint. The
            // next scheduled check re-decides.
            set_pending(app, None);
            set_status(app, UpdateStatus::Error);
            return;
        }
    };

    // The download takes seconds to minutes — ample time for the user to switch updates off or
    // skip this version. Re-read their consent rather than acting on what was true when the
    // download started.
    if !pending_matches(app, &meta.version) {
        set_status(app, UpdateStatus::Idle);
        return;
    }

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
            set_pending(app, None);
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
            // The registration made before the download already carries the right version and
            // intent, so it is left exactly as it is. Deliberately NOT re-armed: re-writing it
            // here would resurrect a deferral the user cancelled while the download ran, and
            // there would be no further mode change to clear it again.
            if pending_matches(app, &meta.version) {
                set_status(app, UpdateStatus::WaitingForIdle);
            } else {
                set_status(app, UpdateStatus::Idle);
            }
        }
        InstallAttempt::Failed(e) => {
            rollback_installed(app);
            set_pending(app, None);
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

fn rollback_installed<R: tauri::Runtime>(app: &AppHandle<R>) {
    if let Some(app_state) = app.try_state::<crate::AppState>() {
        if let Ok(conn) = app_state.db.lock() {
            clear_installed(&conn);
        }
    }
}

/// Installs the update the last check found. Backs the panel's "Install and restart" and the
/// idle retry. With a busy queue: pause after the current job, then let `on_queue_status` retry
/// once it drains.
pub async fn install_pending<R: tauri::Runtime>(
    app: AppHandle<R>,
    trigger: InstallTrigger,
) -> Result<(), String> {
    let Some(runtime) = runtime_of(&app) else {
        return Err("updater unavailable".into());
    };
    let Some(cycle) = try_begin_cycle(&runtime) else {
        return Err("an update operation is already running".into());
    };

    // Re-read the intent under the latch instead of trusting what was true when this was
    // dispatched: a retry waits up to 2s for the queue to unwind, and the user can switch
    // updates off or skip the version in that window. A retry that found nothing pending must
    // NOT create one — that would resurrect the cancellation it just missed.
    let user_requested = match trigger {
        InstallTrigger::UserRequested => true,
        InstallTrigger::Retry => match pending_of(&app) {
            Some(pending) => pending.user_requested,
            None => return Err("the pending install was cancelled".into()),
        },
    };

    // Decided before the network round trip: a scheduler-decided retry against a still-busy
    // queue must cost nothing, or a long batch would re-fetch the whole bundle on every tick.
    {
        let Some(conv) = app.try_state::<Arc<crate::converter::ConverterState>>() else {
            return Err("converter unavailable".into());
        };
        let queue_running = *conv.is_running.lock().unwrap_or_else(|e| e.into_inner());
        match retry_action(user_requested, queue_running) {
            RetryAction::StayPending => return Err("the queue is busy".into()),
            RetryAction::DrainThenInstall => {
                // Drain rather than interrupt: the running job finishes, then `on_queue_status`
                // retries the install.
                if let Ok(mut flag) = conv.pause_after_current.lock() {
                    *flag = true;
                }
            }
            RetryAction::InstallNow => {}
        }
    }

    let Some(updater) = build_updater(&app) else {
        return Err("updater unavailable".into());
    };
    // Both failure exits drop the deferral they were dispatched for, because a pending install
    // that can no longer be fulfilled has no in-app escape: `manual_check_block` refuses every
    // "Check now" while one exists, the panel disables that button for `WaitingForIdle` anyway,
    // and Skip only renders at `Available`. Switching mode drops a scheduler-decided deferral, but
    // a user-requested one survives that too — so a yanked release would strand the panel on
    // "Downloaded — will install when the queue finishes" until the app is quit. Same call the
    // failed download in `perform_install` makes, for the same reason: the next scheduled check
    // re-decides. Guarded on there actually being one, so an ordinary "Install and restart" that
    // fails on a flaky network keeps its Available banner and its Install button.
    let raw = match updater.check().await {
        Ok(Some(raw)) => raw,
        Ok(None) => {
            if pending_of(&app).is_some() {
                // Mirrors `run_cycle`'s CheckOutcome::Nothing: the update is simply gone.
                if let Ok(mut a) = runtime.available.lock() {
                    *a = None;
                }
                set_pending(&app, None);
                set_status(&app, UpdateStatus::Idle);
            }
            return Err("no update available".into());
        }
        Err(e) => {
            let msg = e.to_string();
            if pending_of(&app).is_some() {
                // Recorded as the scheduler's own failure too: without it the status change below
                // would take the panel off the deferral line with nothing said about why.
                if let Ok(mut err) = runtime.last_error.lock() {
                    *err = Some(msg.clone());
                }
                set_pending(&app, None);
                set_status(&app, UpdateStatus::Error);
            }
            return Err(msg);
        }
    };
    let meta = describe(&raw);

    // Checked again after the network round trip, for the same reason as after the download.
    if trigger == InstallTrigger::Retry && pending_of(&app).is_none() {
        return Err("the pending install was cancelled".into());
    }
    // Registered for the whole download → install sequence, so a cancellation arriving mid-flight
    // has something to clear and `perform_install` can see that it did.
    set_pending(
        &app,
        Some(PendingInstall {
            update: meta.clone(),
            user_requested,
        }),
    );

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

pub fn build_state_public<R: tauri::Runtime>(app: &AppHandle<R>) -> Result<UpdateState, String> {
    build_state(app)
}

pub fn set_skipped_version_public(db: &Connection, version: &str) {
    set_skipped_version(db, version);
}

pub fn clear_status<R: tauri::Runtime>(app: &AppHandle<R>) {
    set_status(app, UpdateStatus::Idle);
}

/// Cancels a pending install of `version`, if that is the one pending.
pub fn cancel_pending_version<R: tauri::Runtime>(app: &AppHandle<R>, version: &str) {
    if let Some(pending) = pending_of(app) {
        if !pending_survives_skip(&pending, version) {
            set_pending(app, None);
        }
    }
}

fn spawn_cycle<R: tauri::Runtime>(app: AppHandle<R>) {
    tauri::async_runtime::spawn(async move {
        let _ = run_cycle(app, false).await;
    });
}

fn current_status<R: tauri::Runtime>(app: &AppHandle<R>) -> Option<UpdateStatus> {
    runtime_of(app).and_then(|r| r.status.lock().ok().and_then(|s| *s))
}

fn spawn_install_retry<R: tauri::Runtime>(app: AppHandle<R>) {
    tauri::async_runtime::spawn(async move {
        // `install_pending`, not `run_cycle`: a pending install is already decided, and
        // re-running the mode policy here would demote a user's explicit "Install and restart"
        // in Notify mode back into a notification. `Retry` re-reads the deferral rather than
        // carrying a stale copy of it from dispatch time.
        let _ = install_pending(app, InstallTrigger::Retry).await;
    });
}

/// Applies a change of update mode to whatever the updater is currently doing.
///
/// Owns the whole policy so `commands::settings` does not have to know any of it.
pub fn on_mode_changed<R: tauri::Runtime>(app: &AppHandle<R>, mode: UpdateMode) {
    if let Some(pending) = pending_of(app) {
        if !pending_survives_mode(&pending, mode) {
            // Dropped, not just hidden: the retry paths install a pending update regardless of
            // mode, so leaving it would install on the next queue drain — the exact thing the
            // user just switched away from Automatic to prevent.
            set_pending(app, None);
            set_status(
                app,
                if mode == UpdateMode::Off {
                    UpdateStatus::Idle
                } else {
                    UpdateStatus::Available
                },
            );
            return;
        }
    }

    emit_state(app);

    // A check would find the version already pending or installed and redo work that is already
    // decided — and, for ReadyToRestart, knock the panel out of "restart to apply".
    if mode == UpdateMode::Automatic
        && pending_of(app).is_none()
        && current_status(app) != Some(UpdateStatus::ReadyToRestart)
    {
        spawn_cycle(app.clone());
    }
}

/// Startup check plus an hourly tick that only acts once 24h of wall clock have passed.
///
/// The tick is a coarse poll on purpose: the pacing lives in `should_check_now`, which compares
/// wall-clock timestamps. A 24h timer would be `Instant`-backed and stop while the machine
/// sleeps, stretching "daily" into whatever a nightly-sleeping laptop makes of it.
pub fn start<R: tauri::Runtime>(app: AppHandle<R>) {
    spawn_cycle(app.clone());

    std::thread::spawn(move || loop {
        std::thread::sleep(std::time::Duration::from_secs(TICK_INTERVAL_SECS));

        // Installed, waiting on the user to restart. Checking again would find the same version
        // (this process still reports the old one) and reinstall it, dropping the panel out of
        // its "restart to apply" state and re-downloading for nothing.
        if current_status(&app) == Some(UpdateStatus::ReadyToRestart) {
            continue;
        }

        // Backstop for an install whose drain event never arrived — the queue picked up another
        // job before it went idle — so a pending install can't sit forever. Carries the original
        // requester's intent, so a scheduler-decided install still refuses to pause the queue.
        if pending_of(&app).is_some() {
            spawn_install_retry(app.clone());
            continue;
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
pub fn on_queue_status<R: tauri::Runtime>(app: &AppHandle<R>, status: &str) {
    if status != "idle" && status != "error" {
        return;
    }
    // Keyed on the pending install itself, not on the status: a manual check or a mode change
    // can legitimately move the status while the deferral is still outstanding.
    if pending_of(app).is_none() {
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
        // Re-checked after the wait: the user had up to 2s to switch updates off or skip this
        // version, and dispatching on the stale read would reinstate the deferral they cancelled.
        if pending_of(&app).is_none() {
            return;
        }
        spawn_install_retry(app);
    });
}

/// Polls `is_running` until the queue is genuinely idle. Bounded: a queue that immediately
/// picks up another job leaves the install pending for the next drain (or the hourly backstop)
/// rather than blocking here.
fn wait_for_idle_queue<R: tauri::Runtime>(app: &AppHandle<R>) -> bool {
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

    fn pending(user_requested: bool) -> PendingInstall {
        PendingInstall {
            update: upd("2.0.0"),
            user_requested,
        }
    }

    #[test]
    fn a_scheduled_install_never_pauses_a_running_queue() {
        // LOAD-BEARING. The hourly backstop retries a pending install while the queue may still
        // be running. Arming pause_after_current there stops the user's batch mid-run and
        // converter.rs:1277-1290 persists queue_paused=true, so the queue is still paused after
        // the update restart — an Automatic-mode update would silently halt their work.
        // Only an explicit "Install and restart" may drain the queue.
        assert_eq!(
            retry_action(false, true),
            RetryAction::StayPending,
            "the scheduler waits for the queue; it never clears it"
        );
        assert_eq!(retry_action(true, true), RetryAction::DrainThenInstall);

        // Idle queue: intent is irrelevant, install either way.
        assert_eq!(retry_action(false, false), RetryAction::InstallNow);
        assert_eq!(retry_action(true, false), RetryAction::InstallNow);
    }

    #[test]
    fn turning_updates_off_cancels_an_install_the_scheduler_decided_on() {
        // LOAD-BEARING. The retry paths install a pending update without re-reading the mode, so
        // a deferral created in Automatic mode would land on the next queue drain even after the
        // user set updates to Off — defeating the entire point of the setting.
        assert!(!pending_survives_mode(&pending(false), UpdateMode::Off));
        assert!(!pending_survives_mode(&pending(false), UpdateMode::Notify));
        assert!(pending_survives_mode(
            &pending(false),
            UpdateMode::Automatic
        ));

        // A user who pressed "Install and restart" asked for this one explicitly; changing the
        // policy for *future* updates must not cancel the install they already requested.
        assert!(pending_survives_mode(&pending(true), UpdateMode::Off));
        assert!(pending_survives_mode(&pending(true), UpdateMode::Notify));
    }

    #[test]
    fn skipping_the_pending_version_cancels_it_but_skipping_another_does_not() {
        // Same reason: the retry does not re-consult the skip list, so the skip has to reach the
        // pending install directly — and only the one it names.
        assert!(!pending_survives_skip(&pending(false), "2.0.0"));
        assert!(pending_survives_skip(&pending(false), "1.9.0"));
    }

    #[test]
    fn a_manual_check_cannot_orphan_a_pending_or_installed_update() {
        // LOAD-BEARING. A manual check walks the status Checking -> Available/Idle. Both wake
        // paths for a deferral used to key off the status being exactly WaitingForIdle, so a
        // "Check now" stranded the install with nothing left to retry it — in Notify mode that
        // turned the user's explicit "Install and restart" into a mere banner. It also replaced
        // "restart to apply" with "update available" for a version already installed.
        assert_eq!(
            manual_check_block(Some(UpdateStatus::WaitingForIdle), Some(&pending(true))),
            Some(ManualCheckBlock::InstallPending)
        );
        assert_eq!(
            manual_check_block(Some(UpdateStatus::ReadyToRestart), None),
            Some(ManualCheckBlock::AwaitingRestart)
        );
        // ReadyToRestart wins even with a pending install, because it is the more specific state.
        assert_eq!(
            manual_check_block(Some(UpdateStatus::ReadyToRestart), Some(&pending(false))),
            Some(ManualCheckBlock::AwaitingRestart)
        );

        // Nothing outstanding: a manual check is exactly what the button is for.
        assert_eq!(manual_check_block(None, None), None);
        assert_eq!(manual_check_block(Some(UpdateStatus::Idle), None), None);
        assert_eq!(
            manual_check_block(Some(UpdateStatus::Available), None),
            None
        );
        assert_eq!(manual_check_block(Some(UpdateStatus::Error), None), None);
    }

    // --- call-site harness (mock runtime) ---
    //
    // The predicate tests above pin the policy; these drive the real functions, so deleting a
    // call site turns them red. Everything reachable from `on_mode_changed`, `run_cycle`'s
    // refusal, and `install_pending`'s retry gate is generic over `R: Runtime`, which is what
    // makes a `MockRuntime` app able to stand in for the real one.

    fn mock_app() -> tauri::App<tauri::test::MockRuntime> {
        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        let conn = Connection::open_in_memory().unwrap();
        crate::db::init_db(&conn).unwrap();
        app.manage(crate::AppState {
            db: std::sync::Arc::new(StdMutex::new(conn)),
            preset_cache: StdMutex::new(Default::default()),
        });
        app.manage(std::sync::Arc::new(UpdaterRuntime::default()));
        app.manage(std::sync::Arc::new(ConverterState::new()));
        app
    }

    fn arm_pending(app: &tauri::AppHandle<tauri::test::MockRuntime>, user_requested: bool) {
        set_pending(app, Some(pending(user_requested)));
        set_status(app, UpdateStatus::WaitingForIdle);
    }

    #[test]
    fn switching_the_mode_off_actually_drops_the_pending_install() {
        // Drives the real `on_mode_changed`, so deleting its `pending_survives_mode` branch —
        // not just breaking the predicate — turns this red. Without it the deferral stays live
        // and the next queue drain installs the update the user just switched off.
        let app = mock_app();
        let handle = app.handle().clone();

        arm_pending(&handle, false);
        on_mode_changed(&handle, UpdateMode::Off);
        assert!(
            pending_of(&handle).is_none(),
            "a scheduler-decided deferral must not survive updates being turned off"
        );
        assert_eq!(current_status(&handle), Some(UpdateStatus::Idle));

        // The user's own "Install and restart" survives: the mode governs future updates, not
        // the one they already asked for.
        arm_pending(&handle, true);
        on_mode_changed(&handle, UpdateMode::Off);
        assert!(pending_of(&handle).is_some());
    }

    #[test]
    fn a_manual_check_is_refused_while_an_install_is_pending() {
        // Drives the real `run_cycle`, so deleting its `if manual { manual_check_block(..) }`
        // guard turns this red: without it the check proceeds, walks the status off
        // WaitingForIdle and orphans the deferral.
        //
        // Deliberately built WITHOUT `AppState`: the refusal fires before `run_cycle` reads the
        // mode, so the correct code never needs it, while the mutated code falls straight through
        // to that read and returns a different error. The mutation therefore fails on the message
        // assertion rather than panicking further down the check path.
        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        app.manage(std::sync::Arc::new(UpdaterRuntime::default()));
        let handle = app.handle().clone();
        arm_pending(&handle, true);

        let err = tauri::async_runtime::block_on(run_cycle(handle.clone(), true))
            .expect_err("a manual check must be refused while an install is pending");
        assert!(
            err.contains("waiting to install"),
            "the refusal must say why, got: {err}"
        );

        // Refused means untouched: the deferral and its status are exactly as they were, so the
        // drain retry and the hourly backstop can still find it.
        assert!(pending_of(&handle).is_some());
        assert_eq!(current_status(&handle), Some(UpdateStatus::WaitingForIdle));
    }

    #[test]
    fn a_scheduled_retry_against_a_busy_queue_leaves_the_queue_alone() {
        // Drives the real `install_pending`, so replacing its `retry_action` gate with the
        // unconditional `if busy { pause_after_current = true }` it replaced turns this red.
        // That arming is what stopped a user's batch mid-run and persisted queue_paused across
        // the update restart.
        let app = mock_app();
        let handle = app.handle().clone();
        arm_pending(&handle, false);

        let conv = handle
            .state::<std::sync::Arc<ConverterState>>()
            .inner()
            .clone();
        *conv.is_running.lock().unwrap() = true;

        let err =
            tauri::async_runtime::block_on(install_pending(handle.clone(), InstallTrigger::Retry))
                .expect_err("a scheduled retry must not proceed against a running queue");
        assert!(err.contains("busy"), "got: {err}");

        assert!(
            !*conv.pause_after_current.lock().unwrap(),
            "the scheduler must never arm pause_after_current — only an explicit \
             'Install and restart' may drain the queue"
        );
        assert!(
            pending_of(&handle).is_some(),
            "the deferral stays pending for the next drain"
        );
    }

    #[test]
    fn a_retry_does_not_resurrect_a_deferral_that_was_cancelled_while_it_waited() {
        // A retry is dispatched, then the user turns updates off before it runs. Re-reading the
        // deferral (rather than trusting the intent captured at dispatch) is what stops
        // `install_pending` from recreating it and installing anyway.
        let app = mock_app();
        let handle = app.handle().clone();
        arm_pending(&handle, false);

        // What on_mode_changed(Off) does while the retry is in flight.
        set_pending(&handle, None);

        let err =
            tauri::async_runtime::block_on(install_pending(handle.clone(), InstallTrigger::Retry))
                .expect_err("a retry whose deferral was cancelled must abort");
        assert!(err.contains("cancelled"), "got: {err}");
        assert!(
            pending_of(&handle).is_none(),
            "a retry must never recreate a deferral the user cancelled"
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
