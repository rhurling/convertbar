mod config;
mod embed;
mod routes;

use std::sync::Arc;

use convertbar_core::dispose::DeleteDisposer;
use convertbar_core::events::EventSink;
use tokio::sync::broadcast;

use config::{ConfigError, ServerConfig};
use routes::ServerState;

/// Placeholder event sink until Task 3 wires the real broadcast-backed `ServerSink`.
struct NullSink;

impl EventSink for NullSink {
    fn emit(&self, _event: &str, _payload: serde_json::Value) {}
    fn notify(&self, _title: &str, _body: &str) {}
}

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

    let ctx = convertbar_core::ctx::Ctx::new(conn, Arc::new(NullSink), Arc::new(DeleteDisposer));
    let (events_tx, _rx) = broadcast::channel(256);
    let bind = config.bind;

    let state = ServerState {
        ctx,
        config: Arc::new(config),
        events_tx,
    };

    let app = routes::api_router(state).fallback(embed::fallback);

    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .unwrap_or_else(|err| panic!("bind {bind}: {err}"));
    tracing::info!("convertbar-server listening on {bind}");

    axum::serve(listener, app).await.expect("server error");
}
