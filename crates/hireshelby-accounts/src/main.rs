//! HireShelby control plane.
//!
//! Replaces the upstream Builderlab dependency. It owns accounts, Nostr
//! identity binding, community provisioning, and plan quotas — the surfaces
//! the desktop client previously reached at `app.builderlab.xyz`.
//!
//! Provisioning itself is delegated to the relay's `/operator/communities`
//! API, which already implements NIP-98 auth, an operator allowlist, and
//! replay protection. This service holds the operator key and calls it.

mod api;
mod auth;
mod billing;
mod config;
mod db;
mod identities;
mod operator;
mod plan;

use std::sync::Arc;

use axum::{
    extract::State,
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use config::Config;
use operator::OperatorClient;

pub struct AppState {
    pub config: Config,
    pub operator: OperatorClient,
    pub db: sqlx::PgPool,
    /// Shared client for outbound calls (WorkOS authenticate).
    pub http: reqwest::Client,
}

#[derive(serde::Serialize)]
struct Health {
    status: &'static str,
    /// Advertised so an operator can confirm the deployment is running the key
    /// the relay allowlists, without exposing the secret.
    operator_pubkey: String,
    database: &'static str,
}

async fn health(State(state): State<Arc<AppState>>) -> (StatusCode, Json<Health>) {
    let database = match sqlx::query("SELECT 1").fetch_one(&state.db).await {
        Ok(_) => "up",
        Err(error) => {
            tracing::warn!(%error, "control plane: database health check failed");
            "down"
        }
    };
    let status_code = if database == "up" {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (
        status_code,
        Json(Health {
            status: if database == "up" { "ok" } else { "degraded" },
            operator_pubkey: state.config.operator_public_key_hex(),
            database,
        }),
    )
}

fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/v1/auth/login", get(auth::login))
        .route("/v1/auth/dev-login", post(auth::dev_login))
        .route("/v1/auth/login/exchange", post(auth::exchange))
        .route("/v1/auth/me", get(auth::me))
        .route("/v1/auth/logout", post(auth::logout))
        .route("/v1/billing/webhook", post(billing::webhook))
        .route("/v1/billing/checkout", post(billing::checkout))
        .route("/v1/billing/portal", post(billing::portal))
        .route("/v1/quota/seats", post(api::quota_seats))
        .route("/v1/communities", post(api::create_community))
        // The desktop calls list as POST with an empty body; GET kept for curl.
        .route(
            "/v1/communities/list",
            get(api::list_communities).post(api::list_communities),
        )
        .route("/v1/communities/availability", post(api::availability))
        .route("/v1/communities/archive", post(api::archive))
        .route("/v1/communities/unarchive", post(api::unarchive))
        .route("/v1/communities/transfer", post(api::transfer))
        .route(
            "/v1/nostr-identities/challenge",
            post(identities::challenge),
        )
        .route("/v1/nostr-identities/verify", post(identities::verify))
        // current is called as POST by the desktop; GET kept for curl.
        .route(
            "/v1/nostr-identities/current",
            get(identities::current).post(identities::current),
        )
        .route("/v1/nostr-identities/delete", post(identities::delete))
        .route(
            "/v1/communities/{community_id}/seats/check",
            post(api::check_seats),
        )
        .with_state(state)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    // Redis TLS pulls in both aws-lc-rs and ring, so rustls cannot pick a
    // provider on its own. Mirrors buzz-relay and buzz-admin.
    let _ = rustls::crypto::ring::default_provider().install_default();

    let config = Config::from_env()?;
    // Debug is redacted; see config::Config's Debug impl.
    tracing::info!(?config, "control plane: configuration loaded");

    let db = sqlx::postgres::PgPoolOptions::new()
        .max_connections(16)
        .connect(&config.database_url)
        .await?;
    tracing::info!("control plane: Postgres connected");

    sqlx::migrate!("./migrations").run(&db).await?;
    tracing::info!("control plane: migrations applied");

    let operator = OperatorClient::new(
        config.relay_api_base_url.clone(),
        config.operator_secret_key.clone(),
    );

    let bind_addr = config.bind_addr.clone();
    let state = Arc::new(AppState {
        config,
        operator,
        db,
        http: reqwest::Client::new(),
    });

    let listener = tokio::net::TcpListener::bind(&bind_addr).await?;
    tracing::info!(addr = %bind_addr, "control plane: listening");
    axum::serve(listener, router(state)).await?;
    Ok(())
}
