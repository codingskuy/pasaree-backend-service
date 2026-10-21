//! Ulasan: 1 per produk per pesanan, maksimal 7 hari setelah selesai; memperbarui rating produk.

use axum::{extract::State, http::StatusCode, routing::post, Router};
use chrono::{DateTime, Duration, Utc};
use serde::Deserialize;
use uuid::Uuid;
use validator::{Validate, ValidationError};

use crate::{
    auth::{Pengguna, Peran},
    error::{AppError, AppResult},
    ids,
    json::ApiJson,
    pesanan::AWALAN_PESANAN,
    state::AppState,
};

#[derive(Deserialize, Validate)]
pub struct UlasanBuat {
    pub pesanan_id: String,
    pub produk_id: String,
    #[validate(range(min = 1, max = 5, message = "bintang 1-5"))]
    pub bintang: i32,
    #[validate(length(max = 1000, message = "teks maksimal 1000 karakter"))]
    pub teks: Option<String>,
    #[validate(
        length(max = 5, message = "foto maksimal 5"),
        custom(function = "valid_foto")
    )]
    pub foto_url: Option<Vec<String>>,
}

#[allow(clippy::ptr_arg)]
fn valid_foto(f: &Vec<String>) -> Result<(), ValidationError> {
    if f.iter().all(|u| u.starts_with("https://")) {
        Ok(())
    } else {
        Err(ValidationError::new("url").with_message("foto_url harus https".into()))
    }
}

#[derive(sqlx::FromRow)]
struct BarisSubUlasan {
    status: String,
    selesai_pada: Option<DateTime<Utc>>,
    diterima_pada: Option<DateTime<Utc>>,
}

async fn buat_ulasan(
    State(s): State<AppState>,
    p: Pengguna,
    ApiJson(req): ApiJson<UlasanBuat>,
) -> AppResult<StatusCode> {
    p.wajib(&[Peran::Pembeli])?;
    let nf = || {
        AppError::tidak_ditemukan(
            "PESANAN_TIDAK_DITEMUKAN",
            "Pesanan atau produk tidak ditemukan.",
        )
    };
    let pesanan = ids::dari_ulid(&req.pesanan_id, AWALAN_PESANAN).ok_or_else(nf)?;
    let produk = ids::dari_ulid(&req.produk_id, "").ok_or_else(nf)?;
    let mut tx = s.db.begin().await?;
    sqlx::query_scalar::<_, Uuid>(
        "SELECT id FROM pesanan WHERE id = $1 AND pembeli_id = $2 FOR UPDATE",
    )
    .bind(pesanan)
    .bind(p.id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(nf)?;
    // Sub-pesanan yang memuat produk ini harus sudah diterima/selesai.
    let sub: Option<BarisSubUlasan> = sqlx::query_as(
        "SELECT sp.status, sp.selesai_pada, sp.diterima_pada FROM sub_pesanan sp JOIN item_pesanan ip ON ip.sub_pesanan_id = sp.id \
         WHERE sp.pesanan_id = $1 AND ip.produk_id = $2 LIMIT 1",
    )
    .bind(pesanan)
    .bind(produk)
    .fetch_optional(&mut *tx)
    .await?;
    let BarisSubUlasan {
        status,
        selesai_pada: selesai,
        diterima_pada: diterima,
    } = sub.ok_or_else(nf)?;
    if status != "SELESAI" && status != "DITERIMA_PEMBELI" {
        return Err(AppError::konflik(
            "STATUS_TIDAK_VALID",
            format!("Ulasan hanya setelah pesanan diterima (status {status})."),
        ));
    }
    let acuan = selesai.or(diterima).unwrap_or_else(Utc::now);
    if Utc::now() - acuan > Duration::days(7) {
        return Err(AppError::konflik(
            "ULASAN_KEDALUWARSA",
            "Ulasan maksimal 7 hari setelah selesai.",
        ));
    }
    let n = sqlx::query("INSERT INTO ulasan (id, pesanan_id, produk_id, pembeli_id, bintang, teks, foto_url) VALUES ($1,$2,$3,$4,$5,$6,$7) ON CONFLICT (pesanan_id, produk_id) DO NOTHING")
        .bind(ids::baru())
        .bind(pesanan)
        .bind(produk)
        .bind(p.id)
        .bind(req.bintang)
        .bind(req.teks.as_deref())
        .bind(req.foto_url.clone().unwrap_or_default())
        .execute(&mut *tx)
        .await?
        .rows_affected();
    if n == 0 {
        return Err(AppError::konflik(
            "ULASAN_SUDAH_ADA",
            "Satu ulasan per produk per pesanan.",
        ));
    }
    sqlx::query(
        "UPDATE produk SET rating_rata = COALESCE((SELECT avg(bintang)::float8 FROM ulasan WHERE produk_id = $1), 0), \
         total_ulasan = (SELECT count(*) FROM ulasan WHERE produk_id = $1)::int WHERE id = $1",
    )
    .bind(produk)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(StatusCode::CREATED)
}

pub fn rute() -> Router<AppState> {
    Router::new().route("/ulasan", post(buat_ulasan))
}
