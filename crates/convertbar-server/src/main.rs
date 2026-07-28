mod config;
mod embed;
mod routes;
mod sink;

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
    );
    // The sender is unused until a later task wires it into the shutdown-signal path
    // (SIGTERM/SIGINT handler flips it to `true`). It must stay alive for the server's
    // lifetime regardless: dropping it here would flip every SSE stream's shutdown watch
    // to "sender gone" immediately, ending them right away instead of on real shutdown.
    let (_shutdown_tx, shutdown_rx) = watch::channel(false);
    let bind = config.bind;

    let state = ServerState {
        ctx,
        config: Arc::new(config),
        events_tx,
        shutdown_rx,
    };

    let app = routes::app(state);

    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .unwrap_or_else(|err| panic!("bind {bind}: {err}"));
    tracing::info!("convertbar-server listening on {bind}");

    axum::serve(listener, app).await.expect("server error");
}
