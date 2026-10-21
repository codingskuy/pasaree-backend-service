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

/// Seed produsen + lapak langsung ke DB. Kembalikan lapak_id.
pub async fn seed_lapak(db: &PgPool, pengguna_id: Uuid, slug: &str) -> Uuid {
    let produsen = ids::baru();
    sqlx::query("INSERT INTO produsen (id, pengguna_id, nama, jenis, kontak_wa) VALUES ($1,$2,'Mbok D','UMKM','+6281234567890')")
        .bind(produsen)
        .bind(pengguna_id)
        .execute(db)
        .await
        .unwrap();
    let lapak = ids::baru();
    sqlx::query(
        "INSERT INTO lapak (id, produsen_id, nama_lapak, slug, deskripsi, titik_serah_lumbung_id, rekening_payout_tersensor) \
         VALUES ($1,$2,$3,$4,$5,'TITIK-05','****1234')",
    )
    .bind(lapak)
    .bind(produsen)
    .bind(format!("Lapak {slug}"))
    .bind(slug)
    .bind("Lapak uji dengan deskripsi yang cukup panjang untuk memenuhi batas minimal lima puluh karakter.")
    .execute(db)
    .await
    .unwrap();
    lapak
}

/// Seed produk (default TAYANG) dengan satu varian dan satu foto utama. Kembalikan (produk_id, varian_id).
pub async fn seed_produk(
    db: &PgPool,
    lapak_id: Uuid,
    judul: &str,
    kategori: &str,
    harga: i32,
    stok: i32,
    status: &str,
) -> (Uuid, Uuid) {
    let pid = ids::baru();
    sqlx::query(
        "INSERT INTO produk (id, lapak_id, judul, kategori_kode, tag, deskripsi, komposisi, alergen, status, diajukan_pada) \
         VALUES ($1,$2,$3,$4,'{pedas}',$5,'Bawang, cabai','{}',$6, now())",
    )
    .bind(pid)
    .bind(lapak_id)
    .bind(judul)
    .bind(kategori)
    .bind("Produk uji dengan deskripsi yang cukup panjang untuk memenuhi batas minimal lima puluh karakter.")
    .bind(status)
    .execute(db)
    .await
    .unwrap();
    let vid = ids::baru();
    let ulid = ids::ke_ulid(vid);
    let sku = format!("LPK-{}-0001", &ulid[ulid.len() - 4..]);
    sqlx::query(
        "INSERT INTO varian_produk (id, produk_id, nama_varian, sku, harga_idr, stok, satuan, berat_kirim_gram, tanggal_produksi, kedaluwarsa) \
         VALUES ($1,$2,'Original',$3,$4,$5,'g',200, '2026-10-01', '2027-06-01')",
    )
    .bind(vid)
    .bind(pid)
    .bind(sku)
    .bind(harga)
    .bind(stok)
    .execute(db)
    .await
    .unwrap();
    sqlx::query("INSERT INTO media_produk (id, produk_id, url, jenis, utama, lebar, tinggi, ukuran_byte) VALUES ($1,$2,'https://cdn.uji/foto.jpg','FOTO',true,1200,1200,500000)")
        .bind(ids::baru())
        .bind(pid)
        .execute(db)
        .await
        .unwrap();
    (pid, vid)
}

pub fn u(id: Uuid) -> String {
    ids::ke_ulid(id)
}
