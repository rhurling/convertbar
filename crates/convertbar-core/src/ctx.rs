use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// Head-agnostic shared state for the converter engine. Desktop and server heads each
/// construct one and hand it to `converter::run_queue`/`process_queue` instead of threading
/// `AppHandle`/`db`/`converter` separately.
pub struct Ctx {
    pub db: Arc<Mutex<rusqlite::Connection>>,
    pub converter: Arc<crate::converter::ConverterState>,
    pub events: Arc<dyn crate::events::EventSink>,
    pub disposer: Arc<dyn crate::dispose::FileDisposer>,
    /// How HandBrakeCLI is discovered when no usable path is configured. Injected so tests can
    /// declare whether HandBrake is installed instead of inheriting the host's answer.
    pub handbrake: Arc<dyn crate::handbrake::HandbrakeLocator>,
    /// Preset metadata cache, shared by `handbrake::cached_preset_metadata` across every caller
    /// (settings preview, add_files' suffix resolution) so a preset's metadata is fetched from
    /// HandBrakeCLI at most once per process lifetime.
    pub preset_cache: Mutex<HashMap<String, crate::handbrake::PresetMetadata>>,
    pub watcher: crate::watcher::WatcherState,
    pub hooks: crate::hooks::HookSetup,
}

impl Ctx {
    pub fn new(
        conn: rusqlite::Connection,
        events: Arc<dyn crate::events::EventSink>,
        disposer: Arc<dyn crate::dispose::FileDisposer>,
        handbrake: Arc<dyn crate::handbrake::HandbrakeLocator>,
        hooks: crate::hooks::HookSetup,
    ) -> Arc<Self> {
        Arc::new(Self {
            db: Arc::new(Mutex::new(conn)),
            converter: Arc::new(crate::converter::ConverterState::new()),
            events,
            disposer,
            handbrake,
            preset_cache: Mutex::new(HashMap::new()),
            watcher: crate::watcher::WatcherState::new(),
            hooks,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ctx_carries_the_hook_setup_it_was_given() {
        let ctx = Ctx::new(
            rusqlite::Connection::open_in_memory().unwrap(),
            std::sync::Arc::new(crate::events::TestSink::default()),
            std::sync::Arc::new(crate::dispose::RecordingDisposer::default()),
            std::sync::Arc::new(crate::handbrake::AbsentLocator),
            crate::hooks::HookSetup {
                runner: std::sync::Arc::new(crate::hooks::RecordingHookRunner::default()),
                allow_stored_command: false,
            },
        );
        assert!(!ctx.hooks.allow_stored_command);
    }
}
