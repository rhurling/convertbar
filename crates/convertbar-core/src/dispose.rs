/// The trash primitive, injected per head: desktop = OS trash, server = permanent delete.
/// The cleanup_mode / bad_source_action DECISION logic stays in core and is unchanged.
pub trait FileDisposer: Send + Sync {
    /// Returns true on success (matches the bool contract of trash_delete_primitive).
    fn dispose(&self, path: &str) -> bool;
}

pub struct DeleteDisposer;
impl FileDisposer for DeleteDisposer {
    fn dispose(&self, path: &str) -> bool {
        std::fs::remove_file(path).is_ok()
    }
}

/// Test disposer: records what was disposed, then deletes — the test-harness default,
/// and the behavior queue_ops' old #[cfg(test)] trash stub relied on (Task 5).
#[derive(Default)]
pub struct RecordingDisposer(pub std::sync::Mutex<Vec<String>>);
impl FileDisposer for RecordingDisposer {
    fn dispose(&self, path: &str) -> bool {
        self.0.lock().unwrap().push(path.to_string());
        std::fs::remove_file(path).is_ok()
    }
}
