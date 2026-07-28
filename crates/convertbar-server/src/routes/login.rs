//! `POST /api/login`: exchanges `{"token": "..."}` for a `convertbar_token` cookie so the
//! browser's `EventSource` (which can't send an `Authorization` header) can authenticate
//! `/api/events`. The route is total — it never 404s/500s on the auth-mode split: in
//! `AuthMode::Open` it always returns 204 with no cookie (the login screen never shows, but
//! the frontend never needs to special-case the mode).

use axum::extract::{ConnectInfo, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};
use serde::Deserialize;
use serde_json::json;
use std::net::SocketAddr;

use crate::auth::{token_matches, TOKEN_COOKIE};
use crate::config::AuthMode;
use crate::throttle::{client_id, Gate};

use super::ServerState;

#[derive(Deserialize)]
pub struct LoginBody {
    token: String,
}

pub async fn login(
    State(s): State<ServerState>,
    connect: Option<axum::Extension<ConnectInfo<SocketAddr>>>,
    headers: HeaderMap,
    jar: CookieJar,
    Json(body): Json<LoginBody>,
) -> Response {
    let expected = match &s.config.auth {
        AuthMode::Open => return StatusCode::NO_CONTENT.into_response(),
        AuthMode::Token(t) => t,
    };

    let peer = connect.map(|axum::Extension(ConnectInfo(addr))| addr.ip());
    let id = client_id(peer, &headers, &s.config.trusted_proxies);
    let now = std::time::Instant::now();

    if s.login_throttle.check(id, now) == Gate::Deny {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "unauthorized"})),
        )
            .into_response();
    }

    if !token_matches(&body.token, expected) {
        s.login_throttle.record_failure(id, now);
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "unauthorized"})),
        )
            .into_response();
    }

    s.login_throttle.record_success(id);

    // No `Secure` flag: this is a plain-HTTP LAN server by design (see CLAUDE.md's threat
    // model), so requiring HTTPS here would just break the cookie entirely.
    let cookie = Cookie::build((TOKEN_COOKIE, body.token))
        .http_only(true)
        .same_site(SameSite::Strict)
        .path("/")
        .build();

    (jar.add(cookie), StatusCode::NO_CONTENT).into_response()
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::header::SET_COOKIE;
    use axum::http::Request;
    use serde_json::json;
    use tower::ServiceExt;

    use crate::config::AuthMode;
    use crate::routes::api_router;
    use crate::routes::tests::test_state;
    use crate::routes::ServerState;

    fn state_with_auth(auth: AuthMode) -> ServerState {
        let mut state = test_state();
        let mut config = (*state.config).clone();
        config.auth = auth;
        state.config = std::sync::Arc::new(config);
        state
    }

    async fn post_login(state: ServerState, body: serde_json::Value) -> axum::http::Response<Body> {
        api_router(state)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/login")
                    .header("Content-Type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn open_mode_always_returns_204_with_no_cookie() {
        let response = post_login(
            state_with_auth(AuthMode::Open),
            json!({"token": "anything"}),
        )
        .await;
        assert_eq!(response.status(), axum::http::StatusCode::NO_CONTENT);
        assert!(response.headers().get(SET_COOKIE).is_none());
    }

    #[tokio::test]
    async fn token_mode_bad_token_returns_401_with_no_cookie() {
        let response = post_login(
            state_with_auth(AuthMode::Token("secret".to_string())),
            json!({"token": "wrong"}),
        )
        .await;
        assert_eq!(response.status(), axum::http::StatusCode::UNAUTHORIZED);
        assert!(response.headers().get(SET_COOKIE).is_none());

        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json, json!({"error": "unauthorized"}));
    }

    #[tokio::test]
    async fn token_mode_good_token_returns_204_with_cookie() {
        let response = post_login(
            state_with_auth(AuthMode::Token("secret".to_string())),
            json!({"token": "secret"}),
        )
        .await;
        assert_eq!(response.status(), axum::http::StatusCode::NO_CONTENT);
        let cookie = response
            .headers()
            .get(SET_COOKIE)
            .expect("Set-Cookie header present")
            .to_str()
            .unwrap();
        assert!(cookie.starts_with("convertbar_token=secret"));
    }

    use axum::extract::ConnectInfo;
    use axum::Extension;
    use std::net::SocketAddr;
    use std::time::Duration;

    fn login_app(token: &str, base: Duration) -> axum::Router {
        let mut state = state_with_auth(AuthMode::Token(token.to_string()));
        state.login_throttle = std::sync::Arc::new(crate::throttle::LoginThrottle::new(
            crate::throttle::ThrottlePolicy {
                base,
                ..Default::default()
            },
        ));
        // Extension(ConnectInfo(..)), NOT MockConnectInfo — see the note in Task 5.
        crate::routes::app(state).layer(Extension(ConnectInfo(
            "10.0.0.1:5555".parse::<SocketAddr>().unwrap(),
        )))
    }

    async fn try_login(app: axum::Router, token: &str) -> axum::http::Response<Body> {
        use tower::ServiceExt;
        app.oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/login")
                .header("Host", "localhost")
                .header("Content-Type", "application/json")
                .body(Body::from(json!({ "token": token }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn the_correct_token_is_accepted_after_many_failed_logins() {
        // No lockout, by design: repeated failures must never deny the owner.
        let app = login_app("abcdefghijklmnop", Duration::ZERO);
        for _ in 0..40 {
            let response = try_login(app.clone(), "wrong").await;
            assert_eq!(response.status(), axum::http::StatusCode::UNAUTHORIZED);
        }
        let response = try_login(app, "abcdefghijklmnop").await;
        assert_eq!(response.status(), axum::http::StatusCode::NO_CONTENT);
        assert!(response.headers().get(SET_COOKIE).is_some());
    }

    #[tokio::test]
    async fn a_failed_login_still_sets_no_cookie() {
        let app = login_app("abcdefghijklmnop", Duration::ZERO);
        let response = try_login(app, "wrong").await;
        assert_eq!(response.status(), axum::http::StatusCode::UNAUTHORIZED);
        assert!(response.headers().get(SET_COOKIE).is_none());
    }

    fn gated_login_app(token: &str) -> axum::Router {
        let mut state = state_with_auth(AuthMode::Token(token.to_string()));
        state.login_throttle = std::sync::Arc::new(crate::throttle::LoginThrottle::new(
            crate::throttle::ThrottlePolicy {
                free: 0,
                base: Duration::from_secs(3600),
                ..Default::default()
            },
        ));
        crate::routes::app(state).layer(Extension(ConnectInfo(
            "10.0.0.1:5555".parse::<SocketAddr>().unwrap(),
        )))
    }

    #[tokio::test]
    async fn a_gated_login_is_refused_without_evaluating_the_token() {
        let app = gated_login_app("abcdefghijklmnop");
        assert_eq!(
            try_login(app.clone(), "wrong").await.status(),
            axum::http::StatusCode::UNAUTHORIZED
        );
        let response = try_login(app, "abcdefghijklmnop").await;
        assert_eq!(
            response.status(),
            axum::http::StatusCode::UNAUTHORIZED,
            "the gate was open, so the token was still evaluated"
        );
        assert!(response.headers().get(SET_COOKIE).is_none());
    }

    #[tokio::test]
    async fn login_and_api_attempts_share_one_gate() {
        // Otherwise an attacker guesses on whichever channel is still open.
        use tower::ServiceExt;
        let app = gated_login_app("abcdefghijklmnop");
        try_login(app.clone(), "wrong").await;
        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/queue")
                    .header("Host", "localhost")
                    .header("Authorization", "Bearer abcdefghijklmnop")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            axum::http::StatusCode::UNAUTHORIZED,
            "auth_guard did not inherit the login route's shut gate"
        );
    }

    #[tokio::test]
    async fn open_mode_never_gates() {
        let mut state = state_with_auth(AuthMode::Open);
        state.login_throttle = std::sync::Arc::new(crate::throttle::LoginThrottle::new(
            crate::throttle::ThrottlePolicy {
                free: 0,
                base: Duration::from_secs(3600),
                ..Default::default()
            },
        ));
        let app = crate::routes::app(state).layer(Extension(ConnectInfo(
            "10.0.0.1:5555".parse::<SocketAddr>().unwrap(),
        )));
        for _ in 0..5 {
            assert_eq!(
                try_login(app.clone(), "any-token-at-all").await.status(),
                axum::http::StatusCode::NO_CONTENT
            );
        }
    }
}
