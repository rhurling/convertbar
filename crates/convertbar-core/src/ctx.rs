use std::sync::{Arc, Mutex};

/// Head-agnostic shared state for the converter engine. Desktop and server heads each
/// construct one and hand it to `converter::run_queue`/`process_queue` instead of threading
/// `AppHandle`/`db`/`converter` separately.
pub struct Ctx {
    pub db: Arc<Mutex<rusqlite::Connection>>,
    pub converter: Arc<crate::converter::ConverterState>,
    pub events: Arc<dyn crate::events::EventSink>,
    pub disposer: Arc<dyn crate::dispose::FileDisposer>,
}

impl Ctx {
    pub fn new(
        conn: rusqlite::Connection,
        events: Arc<dyn crate::events::EventSink>,
        disposer: Arc<dyn crate::dispose::FileDisposer>,
    ) -> Arc<Self> {
        Arc::new(Self {
            db: Arc::new(Mutex::new(conn)),
            converter: Arc::new(crate::converter::ConverterState::new()),
            events,
            disposer,
        })
    }
}
