use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

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
