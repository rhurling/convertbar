//! Security middleware, applied (outermost first) in `routes::app`: `host_guard` (anti
//! DNS-rebinding, always on), `auth_guard` (bearer/cookie token check), `json_content_guard`
//! (a CSRF belt — see its doc comment). Each is a plain axum middleware fn so the layering
//! order is explicit and total: every request, including static assets, passes through all
//! three (`auth_guard`/`json_content_guard` no-op where they don't apply).

use axum::extract::{Request, State};
use axum::http::{header, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;
use axum_extra::extract::cookie::CookieJar;
use serde_json::json;

use crate::config::AuthMode;
use crate::routes::ServerState;

/// Name of the cookie `login` sets and `auth_guard`/SSE reads back. `EventSource` can't send
/// an `Authorization` header, so the cookie is the only way `/api/events` can authenticate.
pub const TOKEN_COOKIE: &str = "convertbar_token";

fn json_err(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({ "error": message }))).into_response()
}

/// True if `candidate` matches `expected`, compared in constant time (`constant_time_eq`
/// itself returns `false` — not a panic — for differing lengths, which is exactly the
/// behavior wanted here: a wrong-length token is simply not a match).
pub fn token_matches(candidate: &str, expected: &str) -> bool {
    constant_time_eq::constant_time_eq(candidate.as_bytes(), expected.as_bytes())
}

/// True if `host` (a raw `Host` header or URI-authority value, e.g. `192.168.1.5:8080` or
/// `[::1]:8080`) may reach the server: an IPv4/IPv6 literal (any port — a literal address
/// can't be "rebound" to point elsewhere, so it's always safe regardless of `allowed`),
/// `localhost` (any port, case-insensitive), or a case-insensitive, port-stripped match
/// against `allowed`. Anything else — including an empty/missing host — is rejected.
pub fn host_allowed(host: &str, allowed: &[String]) -> bool {
    let stripped = strip_port(host.trim());
    if stripped.is_empty() {
        return false;
    }
    if stripped.eq_ignore_ascii_case("localhost") {
        return true;
    }
    if stripped.parse::<std::net::IpAddr>().is_ok() {
        return true;
    }
    allowed
        .iter()
        .any(|a| strip_port(a).eq_ignore_ascii_case(stripped))
}

/// Strips a trailing `:port`, bracket-aware for IPv6 literals: `[::1]:8080` -> `::1`, `[::1]`
/// -> `::1` (no port), `nas.local:8080` -> `nas.local`. Left untouched if there's no port
/// (`nas.local`) or the host is malformed (an unclosed `[`).
fn strip_port(host: &str) -> &str {
    if let Some(rest) = host.strip_prefix('[') {
        return match rest.find(']') {
            Some(end) => &rest[..end],
            None => host,
        };
    }
    match host.rsplit_once(':') {
        Some((h, port)) if !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) => h,
        _ => host,
    }
}

/// ALWAYS on, even in `AuthMode::Open` — this is the anti DNS-rebinding check, orthogonal to
/// authentication. Must run before `auth_guard` (see `routes::app`'s layer order) so a
/// rebinding attempt is rejected with 421 rather than leaking a 401 that confirms a real
/// server is listening.
pub async fn host_guard(State(s): State<ServerState>, req: Request, next: Next) -> Response {
    let host = req
        .headers()
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
        // HTTP/2 has no Host header; the authority lives in the URI (`:authority` pseudo-header).
        .or_else(|| req.uri().authority().map(ToString::to_string));

    let allowed = host
        .as_deref()
        .is_some_and(|h| host_allowed(h, &s.config.allowed_hosts));

    if !allowed {
        return json_err(StatusCode::MISDIRECTED_REQUEST, "host not allowed");
    }

    next.run(req).await
}

fn bearer_token(req: &Request) -> Option<String> {
    req.headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::to_string)
}

fn cookie_token(req: &Request) -> Option<String> {
    CookieJar::from_headers(req.headers())
        .get(TOKEN_COOKIE)
        .map(|c| c.value().to_string())
}

/// `AuthMode::Open` passes everything through unconditionally. Otherwise: exempt `POST
/// /api/login` (that's the route that hands out the credential) and every non-`/api` path
/// (static assets — the login page itself must render unauthenticated). Everything else needs
/// a matching bearer token or cookie, compared in constant time.
pub async fn auth_guard(State(s): State<ServerState>, req: Request, next: Next) -> Response {
    let AuthMode::Token(expected) = &s.config.auth else {
        return next.run(req).await;
    };

    let path = req.uri().path().to_string();
    let is_login = path == "/api/login" && req.method() == Method::POST;
    if is_login || !path.starts_with("/api") {
        return next.run(req).await;
    }

    let provided = bearer_token(&req).or_else(|| cookie_token(&req));
    let ok = provided.is_some_and(|t| token_matches(&t, expected));

    if !ok {
        return json_err(StatusCode::UNAUTHORIZED, "unauthorized");
    }

    next.run(req).await
}

/// CSRF belt: for POST/PUT/DELETE under `/api` (login included), require a `Content-Type`
/// starting `application/json`, except a DELETE with no body (`Content-Length` absent or 0)
/// needs none. A cross-site HTML form cannot send `application/json` without a CORS
/// preflight, and this server never answers one — so a same-site fetch is the only way to
/// satisfy this guard.
pub async fn json_content_guard(req: Request, next: Next) -> Response {
    let path = req.uri().path().to_string();
    let method = req.method().clone();
    let is_write = method == Method::POST || method == Method::PUT || method == Method::DELETE;

    if path.starts_with("/api") && is_write {
        let content_length = req
            .headers()
            .get(header::CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0);

        let delete_with_no_body = method == Method::DELETE && content_length == 0;

        let content_type_is_json = req
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|ct| ct.starts_with("application/json"));

        if !content_type_is_json && !delete_with_no_body {
            return json_err(
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "content-type must be application/json",
            );
        }
    }

    next.run(req).await
}

#[cfg(test)]
mod tests {
    use super::*;

    mod host_allowed_tests {
        use super::*;

        #[test]
        fn ipv4_literal_with_port_is_always_allowed() {
            assert!(host_allowed("192.168.1.5:8080", &[]));
        }

        #[test]
        fn bracketed_ipv6_literal_with_port_is_always_allowed() {
            assert!(host_allowed("[::1]:8080", &[]));
        }

        #[test]
        fn bracketed_ipv6_literal_without_port_is_always_allowed() {
            assert!(host_allowed("[::1]", &[]));
        }

        #[test]
        fn localhost_is_allowed_with_any_port_case_insensitively() {
            assert!(host_allowed("localhost:1234", &[]));
            assert!(host_allowed("LocalHost", &[]));
        }

        #[test]
        fn configured_host_is_allowed_only_when_listed() {
            assert!(!host_allowed("nas.local", &[]));
            assert!(host_allowed("nas.local", &["nas.local".to_string()]));
        }

        #[test]
        fn configured_host_match_is_case_insensitive_and_port_stripped_on_both_sides() {
            assert!(host_allowed(
                "NAS.Local:9999",
                &["nas.local:8080".to_string()]
            ));
        }

        #[test]
        fn unlisted_host_is_rejected() {
            assert!(!host_allowed("evil.example.com", &[]));
            assert!(!host_allowed(
                "evil.example.com",
                &["nas.local".to_string()]
            ));
        }

        #[test]
        fn missing_or_empty_host_is_rejected() {
            assert!(!host_allowed("", &[]));
            assert!(!host_allowed("   ", &[]));
        }
    }

    mod token_matches_tests {
        use super::*;

        #[test]
        fn equal_tokens_match() {
            assert!(token_matches("secret", "secret"));
        }

        #[test]
        fn different_tokens_of_equal_length_do_not_match() {
            assert!(!token_matches("secreu", "secret"));
        }

        #[test]
        fn tokens_of_different_length_do_not_match() {
            assert!(!token_matches("secret-but-longer", "secret"));
        }
    }

    /// Full-stack guard tests, via `routes::app` (the layered production composition) rather
    /// than the bare `api_router` most other route tests use — these are specifically about
    /// the guards' behavior and ordering, which only exist at that outer layer.
    mod guard_integration_tests {
        use axum::body::Body;
        use axum::http::header::SET_COOKIE;
        use axum::http::{Request, Response, StatusCode};
        use serde_json::{json, Value};
        use tower::ServiceExt;

        use crate::config::AuthMode;
        use crate::routes::{app, tests::test_state, ServerState};

        fn state_with(auth: AuthMode, allowed_hosts: Vec<String>) -> ServerState {
            let mut state = test_state();
            let mut config = (*state.config).clone();
            config.auth = auth;
            config.allowed_hosts = allowed_hosts;
            state.config = std::sync::Arc::new(config);
            state
        }

        fn token_state(token: &str) -> ServerState {
            state_with(AuthMode::Token(token.to_string()), vec![])
        }

        fn open_state() -> ServerState {
            state_with(AuthMode::Open, vec![])
        }

        async fn send(
            app: axum::Router,
            method: &str,
            uri: &str,
            headers: &[(&str, &str)],
            body: Option<Value>,
        ) -> Response<Body> {
            let mut builder = Request::builder().method(method).uri(uri);
            for (k, v) in headers {
                builder = builder.header(*k, *v);
            }
            let body = match body {
                Some(v) => Body::from(v.to_string()),
                None => Body::empty(),
            };
            app.oneshot(builder.body(body).unwrap()).await.unwrap()
        }

        async fn json_body(response: Response<Body>) -> Value {
            let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap();
            if bytes.is_empty() {
                Value::Null
            } else {
                serde_json::from_slice(&bytes).expect("body must be valid JSON")
            }
        }

        #[tokio::test]
        async fn token_mode_no_credential_is_unauthorized() {
            let response = send(
                app(token_state("secret")),
                "GET",
                "/api/queue",
                &[("Host", "localhost")],
                None,
            )
            .await;
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
            assert_eq!(json_body(response).await, json!({"error": "unauthorized"}));
        }

        /// Self-review spot-check: `/api/fs/list` is a file-browser endpoint that can read
        /// arbitrary paths under `browse_roots` — of every route, this is the one that hurts
        /// most if the guard stack silently doesn't cover it.
        #[tokio::test]
        async fn fs_list_requires_auth_in_token_mode() {
            let response = send(
                app(token_state("secret")),
                "GET",
                "/api/fs/list?path=/",
                &[("Host", "localhost")],
                None,
            )
            .await;
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        }

        /// Self-review spot-check: `/api/events` is the other route that hurts most if
        /// missed — an unauthenticated SSE stream would leak every conversion/queue event.
        #[tokio::test]
        async fn events_requires_auth_in_token_mode() {
            let response = send(
                app(token_state("secret")),
                "GET",
                "/api/events",
                &[("Host", "localhost")],
                None,
            )
            .await;
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        }

        #[tokio::test]
        async fn token_mode_bad_bearer_is_unauthorized() {
            let response = send(
                app(token_state("secret")),
                "GET",
                "/api/queue",
                &[("Host", "localhost"), ("Authorization", "Bearer wrong")],
                None,
            )
            .await;
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        }

        #[tokio::test]
        async fn token_mode_good_bearer_is_authorized() {
            let response = send(
                app(token_state("secret")),
                "GET",
                "/api/queue",
                &[("Host", "localhost"), ("Authorization", "Bearer secret")],
                None,
            )
            .await;
            assert_eq!(response.status(), StatusCode::OK);
        }

        #[tokio::test]
        async fn login_with_bad_token_is_unauthorized_and_sets_no_cookie() {
            let response = send(
                app(token_state("secret")),
                "POST",
                "/api/login",
                &[("Host", "localhost"), ("Content-Type", "application/json")],
                Some(json!({"token": "wrong"})),
            )
            .await;
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
            assert!(response.headers().get(SET_COOKIE).is_none());
        }

        #[tokio::test]
        async fn login_with_good_token_returns_204_and_a_literal_cookie() {
            let response = send(
                app(token_state("secret")),
                "POST",
                "/api/login",
                &[("Host", "localhost"), ("Content-Type", "application/json")],
                Some(json!({"token": "secret"})),
            )
            .await;
            assert_eq!(response.status(), StatusCode::NO_CONTENT);
            let cookie = response
                .headers()
                .get(SET_COOKIE)
                .expect("Set-Cookie header")
                .to_str()
                .unwrap()
                .to_string();
            assert!(
                cookie.contains("convertbar_token=secret"),
                "cookie missing token value: {cookie}"
            );
            assert!(
                cookie.contains("HttpOnly"),
                "cookie missing HttpOnly: {cookie}"
            );
            assert!(
                cookie.contains("SameSite=Strict"),
                "cookie missing SameSite=Strict: {cookie}"
            );
            assert!(cookie.contains("Path=/"), "cookie missing Path=/: {cookie}");
        }

        #[tokio::test]
        async fn cookie_authenticated_request_is_authorized() {
            let response = send(
                app(token_state("secret")),
                "GET",
                "/api/queue",
                &[("Host", "localhost"), ("Cookie", "convertbar_token=secret")],
                None,
            )
            .await;
            assert_eq!(response.status(), StatusCode::OK);
        }

        #[tokio::test]
        async fn cookie_authenticated_sse_is_authorized() {
            // EventSource can't send an Authorization header, so this is the only way the
            // browser can authenticate a `/api/events` connection.
            let response = send(
                app(token_state("secret")),
                "GET",
                "/api/events",
                &[("Host", "localhost"), ("Cookie", "convertbar_token=secret")],
                None,
            )
            .await;
            assert_eq!(response.status(), StatusCode::OK);
        }

        #[tokio::test]
        async fn host_guard_runs_before_auth_guard() {
            // Both a bad host AND a missing credential are true here; 421 (not 401) proves
            // host_guard rejects first, before auth_guard is ever reached.
            let response = send(
                app(token_state("secret")),
                "GET",
                "/api/queue",
                &[("Host", "evil.example.com")],
                None,
            )
            .await;
            assert_eq!(response.status(), StatusCode::MISDIRECTED_REQUEST);
        }

        #[tokio::test]
        async fn configured_allowed_host_is_wired_through_to_the_guard() {
            let state = state_with(AuthMode::Open, vec!["nas.local".to_string()]);
            let response = send(
                app(state.clone()),
                "GET",
                "/api/queue",
                &[("Host", "nas.local:8080")],
                None,
            )
            .await;
            assert_eq!(response.status(), StatusCode::OK);

            let response = send(
                app(state),
                "GET",
                "/api/queue",
                &[("Host", "other.local")],
                None,
            )
            .await;
            assert_eq!(response.status(), StatusCode::MISDIRECTED_REQUEST);
        }

        #[tokio::test]
        async fn open_mode_no_credential_is_ok_but_bad_host_is_misdirected() {
            let response = send(
                app(open_state()),
                "GET",
                "/api/queue",
                &[("Host", "localhost")],
                None,
            )
            .await;
            assert_eq!(response.status(), StatusCode::OK);

            let response = send(
                app(open_state()),
                "GET",
                "/api/queue",
                &[("Host", "evil.example.com")],
                None,
            )
            .await;
            assert_eq!(response.status(), StatusCode::MISDIRECTED_REQUEST);
        }

        #[tokio::test]
        async fn post_without_json_content_type_is_unsupported_media_type() {
            let response = send(
                app(open_state()),
                "POST",
                "/api/queue/files",
                &[("Host", "localhost")],
                Some(json!({"paths": []})),
            )
            .await;
            assert_eq!(response.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
        }

        #[tokio::test]
        async fn delete_with_no_body_is_exempt_from_the_content_type_guard() {
            let response = send(
                app(open_state()),
                "DELETE",
                "/api/queue",
                &[("Host", "localhost")],
                None,
            )
            .await;
            assert_eq!(response.status(), StatusCode::NO_CONTENT);
        }

        #[tokio::test]
        async fn delete_with_a_body_still_requires_the_json_content_type() {
            let response = send(
                app(open_state()),
                "DELETE",
                "/api/queue/jobs/some-id",
                &[("Host", "localhost"), ("Content-Length", "2")],
                Some(json!({})),
            )
            .await;
            assert_eq!(response.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
        }

        #[tokio::test]
        async fn static_asset_root_is_reachable_without_credentials_even_in_token_mode() {
            // dist-web's contents are build-dependent: an empty placeholder under `cargo
            // test`, real assets once `npm run build:web` has run (rust-embed reads from disk
            // in debug builds, no `debug-embed` feature here). So this asserts ONLY what
            // auth_guard's exemption promises — the request is not auth_guard's 401/JSON-error
            // response — never the fallback's actual status/body, which routes/mod.rs's
            // `unregistered_api_path_returns_json_404_not_the_spa_fallback` established the
            // convention of leaving unasserted for exactly this reason.
            let response = send(
                app(token_state("secret")),
                "GET",
                "/",
                &[("Host", "localhost")],
                None,
            )
            .await;
            assert_ne!(response.status(), StatusCode::UNAUTHORIZED);
            let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap();
            assert_ne!(
                bytes, r#"{"error":"unauthorized"}"#,
                "must not be auth_guard's JSON error body"
            );
        }

        #[tokio::test]
        async fn missing_host_header_is_misdirected() {
            let response = send(app(open_state()), "GET", "/api/queue", &[], None).await;
            assert_eq!(response.status(), StatusCode::MISDIRECTED_REQUEST);
        }
    }
}
