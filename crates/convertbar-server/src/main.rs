mod auth;
mod config;
mod embed;
mod routes;
mod sink;
mod startup;
mod throttle;

use std::sync::Arc;

use convertbar_core::dispose::DeleteDisposer;
use tokio::sync::{broadcast, watch};

use config::{ConfigError, ServerConfig};
use routes::ServerState;
use sink::ServerSink;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    let config = match ServerConfig::from_env() {
        Ok(config) => config,
        Err(ConfigError::MissingAuth) => {
            eprintln!(
                "convertbar-server: set CONVERTBAR_AUTH_TOKEN or CONVERTBAR_NO_AUTH=1 (see docs)"
            );
            std::process::exit(1);
        }
        Err(ConfigError::WeakToken) => {
            eprintln!(
                "convertbar-server: CONVERTBAR_AUTH_TOKEN is too weak — it must be at least 16 \
                 characters long and use at least 8 distinct characters.\n\
                 Generate one with:  openssl rand -base64 24"
            );
            std::process::exit(1);
        }
        Err(ConfigError::BadTrustedProxy(entry)) => {
            eprintln!(
                "convertbar-server: invalid CONVERTBAR_TRUSTED_PROXIES entry: {entry} \
                 (expected an IP address or CIDR range, e.g. 172.18.0.5 or 10.0.0.0/8)"
            );
            std::process::exit(1);
        }
        Err(ConfigError::BadBind(value)) => {
            eprintln!("convertbar-server: invalid bind address or port: {value}");
            std::process::exit(1);
        }
    };

    let db_path = convertbar_core::db::get_db_path();
    let conn = rusqlite::Connection::open(&db_path).expect("open database");
    convertbar_core::db::init_db(&conn).expect("initialize database");

    let (events_tx, _rx) = broadcast::channel(256);
    let ctx = convertbar_core::ctx::Ctx::new(
        conn,
        Arc::new(ServerSink(events_tx.clone())),
        Arc::new(DeleteDisposer),
        Arc::new(convertbar_core::handbrake::PathLocator),
    );

    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let bind = config.bind;

    // Bind FIRST, before anything that can spawn a HandBrake child: `boot` below can
    // auto-resume the queue, and nothing serves until `axum::serve` runs anyway, so binding
    // early is free. If the port is taken (e.g. another instance already running on a NAS),
    // this must exit before any encoder is spawned — otherwise a bind failure would orphan a
    // just-started HandBrake process with no listener left to ever kill it.
    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .unwrap_or_else(|err| panic!("bind {bind}: {err}"));
    tracing::info!("convertbar-server listening on {bind}");

    // Server-only settings correction, then the desktop setup block's auto-resume sequence
    // (recover interrupted jobs, resume the queue if warranted, arm watchers) — both must
    // run before the app starts serving requests.
    startup::normalize_server_settings(&ctx);
    startup::boot(&ctx);

    let state = ServerState {
        ctx: ctx.clone(),
        config: Arc::new(config),
        events_tx,
        shutdown_rx,
        login_throttle: Arc::new(throttle::LoginThrottle::new(
            throttle::ThrottlePolicy::default(),
        )),
    };

    let app = routes::app(state);

    let shutdown_ctx = ctx.clone();
    startup::serve(listener, app, async move {
        startup::shutdown_signal().await;
        // Kill the active child AT signal time (not after `serve` returns): this is
        // what flips every SSE stream's shutdown watch (via the send below) after the
        // child is already down, so the graceful drain doesn't wait on an in-flight
        // encode as well as the open connections.
        convertbar_core::converter::kill_active_child(&shutdown_ctx.converter);
        let _ = shutdown_tx.send(true);
    })
    .await
    .expect("server error");

    // Belt: harmless even if the signal-time kill above already ran (kill_active_child is
    // idempotent — a no-op when no child is active, and a second kill()/wait() on an
    // already-reaped child is a fast no-op error, never a hang). Covers any shutdown path
    // that reaches here without going through the graceful-shutdown future above.
    convertbar_core::converter::kill_active_child(&ctx.converter);
}
