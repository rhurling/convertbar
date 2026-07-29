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

/// Test disposer for the denied-Trash world: reports failure and leaves the file where it
/// is — what `trash::delete` does on macOS when the Apple Event to Finder is refused.
#[derive(Default)]
pub struct FailingDisposer;
impl FileDisposer for FailingDisposer {
    fn dispose(&self, _path: &str) -> bool {
        false
    }
}

/// Test disposer that performs the delete but reports failure anyway. Pins that the cleanup
/// verdict is read from the filesystem rather than from this bool: a source that is gone by
/// the time we look satisfies the contract no matter what the primitive claimed.
#[derive(Default)]
pub struct LyingDisposer;
impl FileDisposer for LyingDisposer {
    fn dispose(&self, path: &str) -> bool {
        let _ = std::fs::remove_file(path);
        false
    }
}
