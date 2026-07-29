//! `GET /api/fs/list`: the file-browser endpoint backing the web UI's folder/file picker.
//! Desktop never calls this — it uses Tauri's native dialog instead.
//!
//! Security contract: the requested path is canonicalized (`std::fs::canonicalize`, which
//! resolves symlinks) BEFORE any containment check runs, and containment is checked with
//! `Path::starts_with`, which compares path *components* rather than raw strings — a root of
//! `/media` never admits `/media2` (see `path_allowed`'s tests).
//!
//! Configured roots (`ServerConfig::browse_roots`) are canonicalized per-request here rather
//! than once at config load. A root can legitimately not exist yet at server startup (e.g. a
//! NAS mount that attaches after the container starts); canonicalizing at load would force a
//! choice between permanently dropping that root or keeping it as an uncanonicalized (and
//! therefore unsafe-to-compare) literal forever. Per-request canonicalization means a root
//! that fails to resolve is just excluded from that one request's check, and it starts working
//! again the moment the path exists — no restart required.
//!
//! Accepted TOCTOU window: nothing stops a directory entry from being swapped for a symlink
//! pointing outside an allowed root between our `canonicalize` call and the later
//! `read_dir`/`metadata` calls below. Closing that fully would need `openat`-style fd-relative
//! directory walking. Under this app's single-user LAN threat model — the only party who could
//! pull off that swap already has local write access to the browsed tree, i.e. already has
//! filesystem access equivalent to what this endpoint would leak — it's an accepted and
//! deliberately documented risk, not an oversight.
//!
//! Accepted existence/permission oracle: because canonicalization runs BEFORE the root check
//! (required for symlink-escape detection to work at all — reordering it after would let a
//! symlink hide an escape from the containment check), a request for a path outside every
//! configured root still distinguishes 404 (doesn't exist) from 403 (exists, resolved, just
//! not under a root) from 500 (an ancestor directory exists but isn't searchable). That leaks
//! existence/permission bits for arbitrary paths anywhere on the host, not just under
//! `browse_roots` — the io error strings returned never include the path itself, so nothing
//! beyond those bits is exposed. Accepted under the single-user LAN threat model, same as the
//! TOCTOU window above, and narrowed further by auth being required by default (Task 8).

use std::path::{Path, PathBuf};

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{join_err, ServerState};

#[derive(Deserialize)]
pub struct FsListQuery {
    path: String,
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
pub struct FsEntry {
    name: String,
    path: String,
    is_dir: bool,
    size: Option<u64>,
}

/// True if `canonical` is `root` itself or a descendant of it, for at least one of `roots`.
/// Both sides must already be canonical (symlinks resolved, no `..`) for this to mean
/// anything — `Path::starts_with` compares path components, not raw strings, so root
/// `/media` correctly never matches `/media2` (a naive `str::starts_with` would wrongly
/// admit it).
pub fn path_allowed(canonical: &Path, roots: &[PathBuf]) -> bool {
    roots.iter().any(|root| canonical.starts_with(root))
}

fn json_err(status: StatusCode, message: impl Into<String>) -> Response {
    (status, Json(json!({ "error": message.into() }))).into_response()
}

/// `NotFound` maps to 404; anything else (permission denied, not-a-directory, ...) maps to
/// 500. Either way: a JSON body, never a panic.
fn io_err_status(e: &std::io::Error) -> StatusCode {
    if e.kind() == std::io::ErrorKind::NotFound {
        StatusCode::NOT_FOUND
    } else {
        StatusCode::INTERNAL_SERVER_ERROR
    }
}

pub async fn fs_list(State(s): State<ServerState>, Query(q): Query<FsListQuery>) -> Response {
    let roots = s.config.browse_roots.clone();
    match tokio::task::spawn_blocking(move || fs_list_blocking(q.path, roots)).await {
        Ok(response) => response,
        // The shared mapping, not this module's local `json_err`: a join failure here is the
        // same server bug it is on every other route, and the tenth copy of the shape was the
        // one a `core_err` grep missed.
        Err(join) => join_err(join).into_response(),
    }
}

fn fs_list_blocking(requested: String, roots: Vec<PathBuf>) -> Response {
    let canonical = match std::fs::canonicalize(&requested) {
        Ok(p) => p,
        Err(e) => return json_err(io_err_status(&e), e.to_string()),
    };

    let canonical_roots: Vec<PathBuf> = roots
        .iter()
        .filter_map(|root| std::fs::canonicalize(root).ok())
        .collect();

    if !path_allowed(&canonical, &canonical_roots) {
        return json_err(StatusCode::FORBIDDEN, "path outside allowed roots");
    }

    let read_dir = match std::fs::read_dir(&canonical) {
        Ok(rd) => rd,
        Err(e) => return json_err(io_err_status(&e), e.to_string()),
    };

    let mut entries: Vec<FsEntry> = Vec::new();
    for dir_entry in read_dir {
        // A raw read_dir error mid-iteration (e.g. an entry disappearing concurrently)
        // is skipped rather than failing the whole listing.
        let Ok(dir_entry) = dir_entry else { continue };
        let path = dir_entry.path();
        // `std::fs::metadata` (unlike `DirEntry::metadata`) follows symlinks, so a symlink
        // to a directory correctly shows as `is_dir: true` and a broken symlink fails to
        // stat here and is skipped, same as any other unreadable entry.
        let Ok(metadata) = std::fs::metadata(&path) else {
            continue;
        };
        let is_dir = metadata.is_dir();
        entries.push(FsEntry {
            name: dir_entry.file_name().to_string_lossy().into_owned(),
            path: path.to_string_lossy().into_owned(),
            is_dir,
            size: if is_dir { None } else { Some(metadata.len()) },
        });
    }

    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });

    Json(json!({ "entries": entries })).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    mod path_allowed_tests {
        use super::*;

        #[test]
        fn exact_root_is_allowed() {
            let roots = vec![PathBuf::from("/media")];
            assert!(path_allowed(Path::new("/media"), &roots));
        }

        #[test]
        fn child_of_root_is_allowed() {
            let roots = vec![PathBuf::from("/media")];
            assert!(path_allowed(Path::new("/media/movies/foo.mp4"), &roots));
        }

        #[test]
        fn sibling_with_shared_string_prefix_is_not_allowed() {
            // The whole point of Path::starts_with over a string-prefix check: "/media2"
            // shares "/media" as a *string* prefix but is not a descendant *component*-wise.
            let roots = vec![PathBuf::from("/media")];
            assert!(!path_allowed(Path::new("/media2"), &roots));
            assert!(!path_allowed(Path::new("/media2/foo.mp4"), &roots));
        }

        #[test]
        fn root_slash_admits_everything() {
            let roots = vec![PathBuf::from("/")];
            assert!(path_allowed(Path::new("/"), &roots));
            assert!(path_allowed(Path::new("/etc/passwd"), &roots));
            assert!(path_allowed(Path::new("/media/anything"), &roots));
        }

        #[test]
        fn outside_every_configured_root_is_not_allowed() {
            let roots = vec![PathBuf::from("/media"), PathBuf::from("/data")];
            assert!(!path_allowed(Path::new("/etc/passwd"), &roots));
        }
    }

    mod handler_tests {
        use axum::body::Body;
        use axum::http::Request;
        use serde_json::json;
        use tower::ServiceExt;

        use crate::routes::api_router;
        use crate::routes::tests::test_state;
        use crate::routes::ServerState;

        /// `test_state()` gives `browse_roots: ["/"]` (no `CONVERTBAR_BROWSE_ROOTS` set);
        /// these tests need to confine browsing to a tempdir instead, so they override just
        /// that field on the shared test config. `ServerConfig` derives `Clone`, so this is a
        /// plain struct-update, not a reimplementation of config parsing.
        fn state_rooted_at(root: &std::path::Path) -> ServerState {
            let mut state = test_state();
            let mut config = (*state.config).clone();
            config.browse_roots = vec![root.to_path_buf()];
            state.config = std::sync::Arc::new(config);
            state
        }

        async fn get(app: axum::Router, uri: &str) -> (axum::http::StatusCode, serde_json::Value) {
            let response = app
                .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
                .await
                .unwrap();
            let status = response.status();
            let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap();
            let json = serde_json::from_slice(&bytes).expect("response body must be valid JSON");
            (status, json)
        }

        #[tokio::test]
        async fn lists_seeded_entries_dirs_first_then_case_insensitive_name() {
            let dir = tempfile::tempdir().expect("create tempdir");
            std::fs::write(dir.path().join("banana.txt"), b"").unwrap();
            std::fs::write(dir.path().join("Apple.txt"), b"12345").unwrap();
            std::fs::create_dir(dir.path().join("zzz-folder")).unwrap();

            let app = api_router(state_rooted_at(dir.path()));
            let uri = format!("/api/fs/list?path={}", dir.path().display());
            let (status, json) = get(app, &uri).await;

            assert_eq!(status, axum::http::StatusCode::OK);
            let entries = json["entries"].as_array().expect("entries array");
            let names: Vec<&str> = entries
                .iter()
                .map(|e| e["name"].as_str().unwrap())
                .collect();
            // Dirs first (only one here), then case-insensitive alphabetical among the rest.
            assert_eq!(names, vec!["zzz-folder", "Apple.txt", "banana.txt"]);

            let folder = &entries[0];
            assert_eq!(folder["is_dir"], true);
            assert!(folder["size"].is_null());

            let apple = &entries[1];
            assert_eq!(apple["is_dir"], false);
            assert_eq!(apple["size"], 5);
        }

        #[tokio::test]
        async fn dot_dot_traversal_out_of_the_root_is_forbidden() {
            let dir = tempfile::tempdir().expect("create tempdir");
            let app = api_router(state_rooted_at(dir.path()));

            // canonicalize() resolves the ".." itself, landing outside the configured root.
            let uri = format!("/api/fs/list?path={}/..", dir.path().display());
            let (status, json) = get(app, &uri).await;

            assert_eq!(status, axum::http::StatusCode::FORBIDDEN);
            assert_eq!(json, json!({"error": "path outside allowed roots"}));
        }

        #[tokio::test]
        async fn nonexistent_path_returns_404_json_and_the_process_stays_up() {
            let dir = tempfile::tempdir().expect("create tempdir");
            let app = api_router(state_rooted_at(dir.path()));

            let uri = format!("/api/fs/list?path={}/does-not-exist", dir.path().display());
            let (status, json) = get(app.clone(), &uri).await;

            assert_eq!(status, axum::http::StatusCode::NOT_FOUND);
            assert!(json["error"].is_string());

            // Process alive: the same router still serves a good request afterwards.
            let uri = format!("/api/fs/list?path={}", dir.path().display());
            let (status, _) = get(app, &uri).await;
            assert_eq!(status, axum::http::StatusCode::OK);
        }

        #[cfg(unix)]
        #[tokio::test]
        async fn symlink_inside_root_pointing_outside_is_forbidden() {
            let root = tempfile::tempdir().expect("create root tempdir");
            let outside = tempfile::tempdir().expect("create outside tempdir");
            let link = root.path().join("escape");
            std::os::unix::fs::symlink(outside.path(), &link).expect("create symlink");

            let app = api_router(state_rooted_at(root.path()));
            let uri = format!("/api/fs/list?path={}", link.display());
            let (status, json) = get(app, &uri).await;

            assert_eq!(status, axum::http::StatusCode::FORBIDDEN);
            assert_eq!(json, json!({"error": "path outside allowed roots"}));
        }

        #[cfg(unix)]
        #[tokio::test]
        async fn unreadable_directory_returns_non_200_json_and_the_process_stays_up() {
            use std::os::unix::fs::PermissionsExt;

            let root = tempfile::tempdir().expect("create tempdir");
            let locked = root.path().join("locked");
            std::fs::create_dir(&locked).unwrap();
            std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();

            let app = api_router(state_rooted_at(root.path()));
            let uri = format!("/api/fs/list?path={}", locked.display());
            let (status, json) = get(app.clone(), &uri).await;

            // Restore permissions immediately so the tempdir can clean itself up on drop,
            // regardless of the assertion outcome below.
            std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o700)).unwrap();

            assert_ne!(status, axum::http::StatusCode::OK);
            assert!(json["error"].is_string());

            // Process alive: the same router still serves a good request afterwards.
            let uri = format!("/api/fs/list?path={}", root.path().display());
            let (status, _) = get(app, &uri).await;
            assert_eq!(status, axum::http::StatusCode::OK);
        }
    }
}
