//! Route-contract test (Task 9 of the server-head plan): pins `routes.json` to the
//! router actually registered in `routes::app`. `routes.json` is the shared contract that
//! the TS-side test (`src/test/ipc-contract.test.ts`, Task 13) also consumes — so a route
//! landing in one but not the other is a real drift, not a style nit.
//!
//! `GET /api/events` (SSE) is deliberately NOT a row here — it's transport, not a command
//! (see the plan doc's routes.json section) — so this test never fires it and never has to
//! worry about reading (or hanging on) its streaming body.

use std::collections::HashSet;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde::Deserialize;
use tower::ServiceExt;

use crate::routes::{app, tests::test_state};

#[derive(Deserialize)]
struct RouteRow {
    command: String,
    method: String,
    path: String,
}

const ROUTES_JSON: &str = include_str!("../../routes.json");

/// Human-visible tripwire: `routes.json`'s row count must match this literal. Bump it
/// deliberately (as its own reviewed change) whenever a route is added or removed —
/// never let it drift silently.
const EXPECTED_ROUTE_COUNT: usize = 39;

fn parsed_routes() -> Vec<RouteRow> {
    serde_json::from_str(ROUTES_JSON).expect("routes.json must be valid JSON")
}

/// Replaces every `{param}` path segment with a dummy value (`x`) — enough for the router
/// to match the route on shape; what the handler does with that value isn't this test's
/// concern.
fn fill_params(path: &str) -> String {
    path.split('/')
        .map(|segment| {
            if segment.starts_with('{') && segment.ends_with('}') {
                "x"
            } else {
                segment
            }
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// For every `routes.json` row, fires a request with that row's method+path (path params
/// filled with a dummy value) at the full `app()` router — guards included, `AuthMode::Open`
/// (via `test_state()`) so auth can't mask a registration gap — and asserts the response is
/// neither 404 (route not registered at all) nor 405 (path exists, but not for this method).
/// Anything else (200/204/400/415/422/500/...) proves the route is wired up.
///
/// `host_guard` runs unconditionally (even in Open mode), so every request carries a `Host`
/// header naming an IP literal (`127.0.0.1:8080`), which `host_allowed` always accepts.
/// `json_content_guard` requires `Content-Type: application/json` on POST/PUT/DELETE, so those
/// get the header plus a `{}` body — a 400/422 from a missing field still proves registration,
/// and sending the correct content type means the test asserts what it means rather than
/// merely surviving a 415.
#[tokio::test]
async fn every_routes_json_row_is_registered_with_its_method() {
    let router = app(test_state());

    for row in parsed_routes() {
        let uri = fill_params(&row.path);
        let is_write = matches!(row.method.as_str(), "POST" | "PUT" | "DELETE");

        let mut builder = Request::builder()
            .method(row.method.as_str())
            .uri(&uri)
            .header("Host", "127.0.0.1:8080");
        let body = if is_write {
            builder = builder.header("Content-Type", "application/json");
            Body::from("{}")
        } else {
            Body::empty()
        };

        let response = router
            .clone()
            .oneshot(builder.body(body).expect("valid request"))
            .await
            .expect("request must complete");

        let status = response.status();
        assert_ne!(
            status,
            StatusCode::NOT_FOUND,
            "{} {} (command `{}`) returned 404 — route not registered",
            row.method,
            row.path,
            row.command
        );
        assert_ne!(
            status,
            StatusCode::METHOD_NOT_ALLOWED,
            "{} {} (command `{}`) returned 405 — path exists but not for this method",
            row.method,
            row.path,
            row.command
        );
    }
}

#[test]
fn routes_json_has_no_duplicate_commands() {
    let rows = parsed_routes();
    let mut seen = HashSet::new();
    for row in &rows {
        assert!(
            seen.insert(row.command.clone()),
            "duplicate command `{}` in routes.json",
            row.command
        );
    }
}

#[test]
fn routes_json_row_count_matches_the_pinned_literal() {
    let rows = parsed_routes();
    assert_eq!(
        rows.len(),
        EXPECTED_ROUTE_COUNT,
        "routes.json row count changed ({} rows) — update EXPECTED_ROUTE_COUNT deliberately",
        rows.len()
    );
}
