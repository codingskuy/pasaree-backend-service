//! pasaree-backend-service: API katalog, lapak, transaksi, dan keuangan Pasaree.

pub mod auth;
pub mod cek;
pub mod config;
pub mod dukungan;
pub mod error;
pub mod ids;
pub mod json;
pub mod katalog;
pub mod keranjang;
pub mod keuangan;
pub mod kurasi;
pub mod lapak;
pub mod lumbung;
pub mod model;
pub mod pesanan;
pub mod pesanan_web;
pub mod state;
pub mod trip;
pub mod tugas;
pub mod ulasan;

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
        .merge(katalog::rute())
        .merge(lapak::rute())
        .merge(kurasi::rute())
        .merge(trip::rute())
        .merge(keranjang::rute())
        .merge(pesanan::rute())
        .merge(pesanan_web::rute())
        .merge(ulasan::rute())
        .layer(TraceLayer::new_for_http())
        .layer(TimeoutLayer::with_status_code(
            axum::http::StatusCode::REQUEST_TIMEOUT,
            Duration::from_secs(15),
        ))
        .with_state(state)
}
