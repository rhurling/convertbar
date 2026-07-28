//! `POST /api/login`: exchanges `{"token": "..."}` for a `convertbar_token` cookie so the
//! browser's `EventSource` (which can't send an `Authorization` header) can authenticate
//! `/api/events`. The route is total — it never 404s/500s on the auth-mode split: in
//! `AuthMode::Open` it always returns 204 with no cookie (the login screen never shows, but
//! the frontend never needs to special-case the mode).

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};
use serde::Deserialize;
use serde_json::json;

use crate::auth::{token_matches, TOKEN_COOKIE};
use crate::config::AuthMode;

use super::ServerState;

#[derive(Deserialize)]
pub struct LoginBody {
    token: String,
}

pub async fn login(
    State(s): State<ServerState>,
    jar: CookieJar,
    Json(body): Json<LoginBody>,
) -> Response {
    let expected = match &s.config.auth {
        AuthMode::Open => return StatusCode::NO_CONTENT.into_response(),
        AuthMode::Token(t) => t,
    };

    if !token_matches(&body.token, expected) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "unauthorized"})),
        )
            .into_response();
    }

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
}
