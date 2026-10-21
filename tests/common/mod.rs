#![allow(dead_code)]
//! Pembantu uji: bangun app di atas pool `#[sqlx::test]`, kirim permintaan, seed pengguna.

use axum::{
    body::Body,
    http::{header, Method, Request, StatusCode},
    Router,
};
use http_body_util::BodyExt;
use pasaree::{
    auth::{self, Peran},
    config::Config,
    ids,
    state::AppState,
};
use serde_json::Value;
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

pub fn app_dengan(db: PgPool, config: Config) -> Router {
    pasaree::app(AppState::baru(db, config))
}

pub fn app(db: PgPool) -> Router {
    app_dengan(db, Config::untuk_uji())
}

pub async fn kirim(
    app: &Router,
    metode: Method,
    uri: &str,
    token: Option<&str>,
    badan: Option<Value>,
) -> (StatusCode, Value) {
    kirim_dengan_header(app, metode, uri, token, &[], badan).await
}

pub async fn kirim_dengan_header(
    app: &Router,
    metode: Method,
    uri: &str,
    token: Option<&str>,
    header_tambahan: &[(&str, &str)],
    badan: Option<Value>,
) -> (StatusCode, Value) {
    let mut b = Request::builder().method(metode).uri(uri);
    if let Some(t) = token {
        b = b.header(header::AUTHORIZATION, format!("Bearer {t}"));
    }
    for (k, v) in header_tambahan {
        b = b.header(*k, *v);
    }
    let req = match badan {
        Some(j) => b
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(j.to_string()))
            .unwrap(),
        None => b.body(Body::empty()).unwrap(),
    };
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let json = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, json)
}

/// Seed pengguna langsung ke DB, kembalikan (id, access token).
pub async fn seed_pengguna(db: &PgPool, email: &str, peran: Peran) -> (Uuid, String) {
    let id = ids::baru();
    sqlx::query(
        "INSERT INTO pengguna (id, email, kata_sandi_hash, nama, peran) VALUES ($1,$2,$3,$4,$5)",
    )
    .bind(id)
    .bind(email)
    .bind(auth::hash_sandi("rahasia-123").unwrap())
    .bind("Uji")
    .bind(peran.sebagai_str())
    .execute(db)
    .await
    .unwrap();
    let token = auth::terbitkan_akses(&Config::untuk_uji(), id, peran).unwrap();
    (id, token)
}
