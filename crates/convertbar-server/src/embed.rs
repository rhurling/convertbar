//! Embeds the built frontend (`dist-web/`, produced by `npm run build:web`) into the
//! server binary and serves it as a single-page app: real assets by path, `index.html`
//! for `/` and any unknown path without an extension (client-side routing), 404 for a
//! path that looks like a missing asset.

use axum::http::{header, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use rust_embed::Embed;

#[derive(Embed)]
#[folder = "../../dist-web"]
pub struct WebAssets;

pub async fn fallback(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');

    if let Some(response) = serve(path) {
        return response;
    }
    if has_extension(path) {
        return not_found();
    }
    serve("index.html").unwrap_or_else(not_found)
}

fn serve(path: &str) -> Option<Response> {
    let asset = WebAssets::get(path)?;
    let mime = mime_guess::from_path(path).first_or_octet_stream();
    Some(
        (
            [(header::CONTENT_TYPE, mime.as_ref().to_string())],
            asset.data,
        )
            .into_response(),
    )
}

fn not_found() -> Response {
    (StatusCode::NOT_FOUND, "not found").into_response()
}

/// A trailing path segment with a `.` is treated as a real asset request (`app.js`,
/// `logo.png`); anything else (`/`, `/queue`, `/settings/general`) is a client route.
fn has_extension(path: &str) -> bool {
    path.rsplit('/')
        .next()
        .is_some_and(|segment| segment.contains('.'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_and_directory_like_paths_have_no_extension() {
        assert!(!has_extension(""));
        assert!(!has_extension("queue"));
        assert!(!has_extension("settings/general"));
    }

    #[test]
    fn file_like_paths_have_an_extension() {
        assert!(has_extension("app.js"));
        assert!(has_extension("assets/logo.png"));
    }

    #[test]
    fn trailing_slash_after_a_file_name_is_not_an_extension() {
        assert!(!has_extension("assets/logo.png/"));
    }
}
