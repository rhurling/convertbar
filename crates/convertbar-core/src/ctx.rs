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
}

impl Ctx {
    pub fn new(
        conn: rusqlite::Connection,
        events: Arc<dyn crate::events::EventSink>,
        disposer: Arc<dyn crate::dispose::FileDisposer>,
        handbrake: Arc<dyn crate::handbrake::HandbrakeLocator>,
    ) -> Arc<Self> {
        Arc::new(Self {
            db: Arc::new(Mutex::new(conn)),
            converter: Arc::new(crate::converter::ConverterState::new()),
            events,
            disposer,
            handbrake,
            preset_cache: Mutex::new(HashMap::new()),
            watcher: crate::watcher::WatcherState::new(),
        })
    }
}
