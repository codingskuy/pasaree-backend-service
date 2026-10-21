//! pasaree-backend-service: API katalog, lapak, transaksi, dan keuangan Pasaree.

pub mod auth;
pub mod config;
pub mod error;
pub mod ids;
pub mod json;
pub mod lumbung;
pub mod state;

use std::time::Duration;

use axum::{extract::State, routing::get, Json, Router};
use serde_json::{json, Value};
use tower_http::{timeout::TimeoutLayer, trace::TraceLayer};

use crate::{error::AppResult, state::AppState};

async fn kesehatan(State(s): State<AppState>) -> AppResult<Json<Value>> {
    sqlx::query_scalar::<_, i32>("SELECT 1")
        .fetch_one(&s.db)
        .await?;
    Ok(Json(
        json!({"layanan": "pasaree-backend-service", "status": "baik", "db": "pasaree"}),
    ))
}

pub fn app(state: AppState) -> Router {
    Router::new()
        .route("/kesehatan", get(kesehatan))
        .merge(auth::rute())
        .layer(TraceLayer::new_for_http())
        .layer(TimeoutLayer::with_status_code(
            axum::http::StatusCode::REQUEST_TIMEOUT,
            Duration::from_secs(15),
        ))
        .with_state(state)
}
