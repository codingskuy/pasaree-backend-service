//! Satu `AppError` untuk seluruh layanan; dipetakan ke 400/401/403/404/409/422/500.

use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::{json, Value};

#[derive(Debug)]
pub enum AppError {
    /// 400 — permintaan rusak (JSON tidak valid, parameter salah).
    Permintaan(String),
    /// 401 — token hilang/kedaluwarsa/audiens salah.
    TidakTerotentikasi(String),
    /// 403 — peran tidak berhak.
    Terlarang(String),
    /// 404 — kode spesifik, mis. `PESANAN_TIDAK_DITEMUKAN`.
    TidakDitemukan { kode: &'static str, pesan: String },
    /// 409 — konflik, mis. `STOK_HABIS`.
    Konflik {
        kode: &'static str,
        pesan: String,
        detail: Option<Value>,
    },
    /// 422 — validasi per kolom: `[{kolom, pesan}]`.
    Validasi(Vec<KesalahanKolom>),
    /// 503 — Lumbung tidak dapat dihubungi.
    Hulu(String),
    /// 500 — kesalahan server (detail hanya di log).
    Internal(String),
}

#[derive(Debug, Clone)]
pub struct KesalahanKolom {
    pub kolom: String,
    pub pesan: String,
}

impl KesalahanKolom {
    pub fn baru(kolom: impl Into<String>, pesan: impl Into<String>) -> Self {
        Self {
            kolom: kolom.into(),
            pesan: pesan.into(),
        }
    }
}

impl AppError {
    pub fn validasi(kolom: impl Into<String>, pesan: impl Into<String>) -> Self {
        Self::Validasi(vec![KesalahanKolom::baru(kolom, pesan)])
    }
    pub fn konflik(kode: &'static str, pesan: impl Into<String>) -> Self {
        Self::Konflik {
            kode,
            pesan: pesan.into(),
            detail: None,
        }
    }
    pub fn tidak_ditemukan(kode: &'static str, pesan: impl Into<String>) -> Self {
        Self::TidakDitemukan {
            kode,
            pesan: pesan.into(),
        }
    }
}

pub type AppResult<T> = Result<T, AppError>;

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, kode, pesan, detail): (StatusCode, &str, String, Option<Value>) = match self {
            AppError::Permintaan(p) => (StatusCode::BAD_REQUEST, "VALIDASI_GAGAL", p, None),
            AppError::TidakTerotentikasi(p) => {
                (StatusCode::UNAUTHORIZED, "AUTH_KEDALUWARSA", p, None)
            }
            AppError::Terlarang(p) => (StatusCode::FORBIDDEN, "AKSES_DITOLAK", p, None),
            AppError::TidakDitemukan { kode, pesan } => (StatusCode::NOT_FOUND, kode, pesan, None),
            AppError::Konflik {
                kode,
                pesan,
                detail,
            } => (StatusCode::CONFLICT, kode, pesan, detail),
            AppError::Validasi(k) => (
                StatusCode::UNPROCESSABLE_ENTITY,
                "VALIDASI_GAGAL",
                "Validasi gagal.".to_string(),
                Some(
                    json!({ "kesalahan": k.iter().map(|e| json!({"kolom": e.kolom, "pesan": e.pesan})).collect::<Vec<_>>() }),
                ),
            ),
            AppError::Hulu(p) => (
                StatusCode::SERVICE_UNAVAILABLE,
                "LUMBUNG_TIDAK_TERSEDIA",
                p,
                None,
            ),
            AppError::Internal(log) => {
                tracing::error!(kesalahan = %log, "kesalahan internal");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "KESALAHAN_SERVER",
                    "Terjadi kesalahan pada server.".to_string(),
                    None,
                )
            }
        };
        let mut badan = json!({ "kode": kode, "pesan": pesan });
        if let Some(d) = detail {
            badan["detail"] = d;
        }
        (status, Json(badan)).into_response()
    }
}

impl From<sqlx::Error> for AppError {
    fn from(e: sqlx::Error) -> Self {
        if let sqlx::Error::Database(db) = &e {
            if db.is_unique_violation() {
                return AppError::konflik("DATA_SUDAH_ADA", "Data sudah ada.");
            }
        }
        AppError::Internal(e.to_string())
    }
}

impl From<validator::ValidationErrors> for AppError {
    fn from(e: validator::ValidationErrors) -> Self {
        let mut daftar = Vec::new();
        for (kolom, errs) in e.field_errors() {
            for err in errs {
                let pesan = err
                    .message
                    .as_ref()
                    .map(|m| m.to_string())
                    .unwrap_or_else(|| format!("tidak valid ({})", err.code));
                daftar.push(KesalahanKolom::baru(kolom.to_string(), pesan));
            }
        }
        daftar.sort_by(|a, b| a.kolom.cmp(&b.kolom));
        AppError::Validasi(daftar)
    }
}
