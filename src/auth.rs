//! Autentikasi: JWT akses 15 menit (`aud=pasaree`), refresh 7 hari sekali-pakai dengan
//! reuse-detection per keluarga token, argon2 untuk kata sandi, peran per route.

use argon2::{
    password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
    Argon2,
};
use axum::{
    extract::{FromRequestParts, State},
    http::{header::AUTHORIZATION, request::Parts, StatusCode},
    routing::post,
    Json, Router,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use chrono::{Duration, Utc};
use jsonwebtoken::{decode, encode, Algorithm, DecodingKey, EncodingKey, Header, Validation};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;
use validator::Validate;

use crate::{
    config::Config,
    error::{AppError, AppResult},
    ids,
    json::ApiJson,
    state::AppState,
};

pub const AKSES_DETIK: i64 = 15 * 60;
pub const REFRESH_HARI: i64 = 7;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Peran {
    Produsen,
    Operator,
    Kurator,
    Admin,
    Pembeli,
}

impl Peran {
    pub fn sebagai_str(self) -> &'static str {
        match self {
            Peran::Produsen => "PRODUSEN",
            Peran::Operator => "OPERATOR",
            Peran::Kurator => "KURATOR",
            Peran::Admin => "ADMIN",
            Peran::Pembeli => "PEMBELI",
        }
    }
    pub fn dari_str(s: &str) -> Option<Self> {
        Some(match s {
            "PRODUSEN" => Peran::Produsen,
            "OPERATOR" => Peran::Operator,
            "KURATOR" => Peran::Kurator,
            "ADMIN" => Peran::Admin,
            "PEMBELI" => Peran::Pembeli,
            _ => return None,
        })
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Klaim {
    pub sub: String,
    pub aud: String,
    pub iss: String,
    pub exp: i64,
    pub iat: i64,
    pub peran: Peran,
}

/// Pengguna terotentikasi hasil verifikasi JWT.
#[derive(Debug, Clone)]
pub struct Pengguna {
    pub id: Uuid,
    pub peran: Peran,
}

impl Pengguna {
    /// Terapkan pembatasan peran per route; selain itu 403.
    pub fn wajib(&self, boleh: &[Peran]) -> AppResult<()> {
        if boleh.contains(&self.peran) {
            Ok(())
        } else {
            Err(AppError::Terlarang(
                "Peran tidak berhak mengakses sumber daya ini.".into(),
            ))
        }
    }
}

pub fn terbitkan_akses(cfg: &Config, id: Uuid, peran: Peran) -> AppResult<String> {
    terbitkan_akses_dengan_exp(cfg, id, peran, Utc::now().timestamp() + AKSES_DETIK)
}

pub fn terbitkan_akses_dengan_exp(
    cfg: &Config,
    id: Uuid,
    peran: Peran,
    exp: i64,
) -> AppResult<String> {
    let klaim = Klaim {
        sub: id.to_string(),
        aud: cfg.jwt_aud.clone(),
        iss: cfg.jwt_issuer.clone(),
        exp,
        iat: Utc::now().timestamp(),
        peran,
    };
    encode(
        &Header::new(Algorithm::HS256),
        &klaim,
        &EncodingKey::from_secret(cfg.jwt_secret.as_bytes()),
    )
    .map_err(|e| AppError::Internal(e.to_string()))
}

pub fn verifikasi_akses(cfg: &Config, token: &str) -> AppResult<Pengguna> {
    let mut v = Validation::new(Algorithm::HS256);
    v.set_audience(&[cfg.jwt_aud.as_str()]);
    v.set_issuer(&[cfg.jwt_issuer.as_str()]);
    v.set_required_spec_claims(&["exp", "aud", "iss", "sub"]);
    v.leeway = 0;
    let data = decode::<Klaim>(
        token,
        &DecodingKey::from_secret(cfg.jwt_secret.as_bytes()),
        &v,
    )
    .map_err(|_| {
        AppError::TidakTerotentikasi(
            "Token tidak valid, kedaluwarsa, atau audiens bukan pasaree.".into(),
        )
    })?;
    let id = Uuid::parse_str(&data.claims.sub)
        .map_err(|_| AppError::TidakTerotentikasi("Token rusak.".into()))?;
    Ok(Pengguna {
        id,
        peran: data.claims.peran,
    })
}

fn token_dari_header(parts: &Parts) -> Option<&str> {
    parts
        .headers
        .get(AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
}

impl FromRequestParts<AppState> for Pengguna {
    type Rejection = AppError;
    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let token = token_dari_header(parts)
            .ok_or_else(|| AppError::TidakTerotentikasi("Token tidak ada.".into()))?;
        verifikasi_akses(&state.config, token)
    }
}

/// Auth opsional untuk endpoint baca katalog: tanpa header = anonim; token rusak tetap 401.
pub struct PenggunaOpsional(pub Option<Pengguna>);

impl FromRequestParts<AppState> for PenggunaOpsional {
    type Rejection = AppError;
    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        match token_dari_header(parts) {
            None => Ok(PenggunaOpsional(None)),
            Some(t) => Ok(PenggunaOpsional(Some(verifikasi_akses(&state.config, t)?))),
        }
    }
}

pub fn hash_sandi(sandi: &str) -> AppResult<String> {
    let garam = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(sandi.as_bytes(), &garam)
        .map(|h| h.to_string())
        .map_err(|e| AppError::Internal(e.to_string()))
}

fn cocok_sandi(sandi: &str, hash: &str) -> bool {
    PasswordHash::new(hash)
        .map(|h| {
            Argon2::default()
                .verify_password(sandi.as_bytes(), &h)
                .is_ok()
        })
        .unwrap_or(false)
}

fn hash_refresh(token: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(token.as_bytes()))
}

#[derive(Serialize)]
pub struct TokenResponse {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_in: i64,
}

async fn terbitkan_pasangan(
    state: &AppState,
    id: Uuid,
    peran: Peran,
    keluarga: Uuid,
) -> AppResult<TokenResponse> {
    let mut mentah = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut mentah);
    let refresh = URL_SAFE_NO_PAD.encode(mentah);
    sqlx::query("INSERT INTO refresh_token (id, pengguna_id, keluarga, token_hash, kedaluwarsa) VALUES ($1,$2,$3,$4,$5)")
        .bind(Uuid::now_v7())
        .bind(id)
        .bind(keluarga)
        .bind(hash_refresh(&refresh))
        .bind(Utc::now() + Duration::days(REFRESH_HARI))
        .execute(&state.db)
        .await?;
    Ok(TokenResponse {
        access_token: terbitkan_akses(&state.config, id, peran)?,
        refresh_token: refresh,
        expires_in: AKSES_DETIK,
    })
}

#[derive(Deserialize, Validate)]
pub struct LoginRequest {
    #[validate(email(message = "format email tidak valid"))]
    pub email: String,
    #[validate(length(min = 1, message = "kata sandi wajib diisi"))]
    pub kata_sandi: String,
}

#[derive(Deserialize, Validate)]
pub struct DaftarRequest {
    #[validate(email(message = "format email tidak valid"))]
    pub email: String,
    #[validate(length(min = 8, max = 128, message = "kata sandi 8-128 karakter"))]
    pub kata_sandi: String,
    #[validate(length(min = 2, max = 80, message = "nama 2-80 karakter"))]
    pub nama: String,
}

#[derive(Deserialize, Validate)]
pub struct RefreshRequest {
    #[validate(length(min = 1, message = "refresh_token wajib diisi"))]
    pub refresh_token: String,
}

async fn login(
    State(s): State<AppState>,
    ApiJson(req): ApiJson<LoginRequest>,
) -> AppResult<Json<TokenResponse>> {
    let baris: Option<(Uuid, String, String)> = sqlx::query_as(
        "SELECT id, kata_sandi_hash, peran FROM pengguna WHERE lower(email) = lower($1)",
    )
    .bind(&req.email)
    .fetch_optional(&s.db)
    .await?;
    let gagal = || AppError::TidakTerotentikasi("Email atau kata sandi salah.".into());
    let (id, hash, peran) = baris.ok_or_else(gagal)?;
    if !cocok_sandi(&req.kata_sandi, &hash) {
        return Err(gagal());
    }
    let peran =
        Peran::dari_str(&peran).ok_or_else(|| AppError::Internal("peran tidak dikenal".into()))?;
    Ok(Json(
        terbitkan_pasangan(&s, id, peran, Uuid::now_v7()).await?,
    ))
}

/// Pendaftaran publik hanya membuat akun PEMBELI. Peran lain diberikan admin/operasional.
async fn daftar(
    State(s): State<AppState>,
    ApiJson(req): ApiJson<DaftarRequest>,
) -> AppResult<(StatusCode, Json<TokenResponse>)> {
    let id = ids::baru();
    sqlx::query("INSERT INTO pengguna (id, email, kata_sandi_hash, nama, peran) VALUES ($1,$2,$3,$4,'PEMBELI')")
        .bind(id)
        .bind(req.email.trim())
        .bind(hash_sandi(&req.kata_sandi)?)
        .bind(req.nama.trim())
        .execute(&s.db)
        .await
        .map_err(|e| match AppError::from(e) {
            AppError::Konflik { .. } => AppError::konflik("EMAIL_SUDAH_TERDAFTAR", "Email sudah terdaftar."),
            lain => lain,
        })?;
    Ok((
        StatusCode::CREATED,
        Json(terbitkan_pasangan(&s, id, Peran::Pembeli, Uuid::now_v7()).await?),
    ))
}

#[derive(sqlx::FromRow)]
struct BarisRefresh {
    id: Uuid,
    pengguna_id: Uuid,
    keluarga: Uuid,
    kedaluwarsa: chrono::DateTime<Utc>,
    dipakai_pada: Option<chrono::DateTime<Utc>>,
    dicabut_pada: Option<chrono::DateTime<Utc>>,
    peran: String,
}

async fn refresh(
    State(s): State<AppState>,
    ApiJson(req): ApiJson<RefreshRequest>,
) -> AppResult<Json<TokenResponse>> {
    let gagal =
        || AppError::TidakTerotentikasi("Refresh token tidak valid atau kedaluwarsa.".into());
    let baris: Option<BarisRefresh> =
        sqlx::query_as(
            "SELECT r.id, r.pengguna_id, r.keluarga, r.kedaluwarsa, r.dipakai_pada, r.dicabut_pada, p.peran \
             FROM refresh_token r JOIN pengguna p ON p.id = r.pengguna_id WHERE r.token_hash = $1",
        )
        .bind(hash_refresh(&req.refresh_token))
        .fetch_optional(&s.db)
        .await?;
    let BarisRefresh {
        id: rid,
        pengguna_id: pid,
        keluarga,
        kedaluwarsa,
        dipakai_pada: dipakai,
        dicabut_pada: dicabut,
        peran,
    } = baris.ok_or_else(gagal)?;
    if dipakai.is_some() || dicabut.is_some() {
        // Reuse-detection: token lama dipakai ulang -> cabut seluruh keluarga.
        sqlx::query("UPDATE refresh_token SET dicabut_pada = now() WHERE keluarga = $1 AND dicabut_pada IS NULL")
            .bind(keluarga)
            .execute(&s.db)
            .await?;
        return Err(gagal());
    }
    if kedaluwarsa <= Utc::now() {
        return Err(gagal());
    }
    // Penandaan atomik: hanya satu permintaan yang menang bila ada balapan.
    let n = sqlx::query("UPDATE refresh_token SET dipakai_pada = now() WHERE id = $1 AND dipakai_pada IS NULL AND dicabut_pada IS NULL")
        .bind(rid)
        .execute(&s.db)
        .await?
        .rows_affected();
    if n == 0 {
        sqlx::query("UPDATE refresh_token SET dicabut_pada = now() WHERE keluarga = $1 AND dicabut_pada IS NULL")
            .bind(keluarga)
            .execute(&s.db)
            .await?;
        return Err(gagal());
    }
    let peran =
        Peran::dari_str(&peran).ok_or_else(|| AppError::Internal("peran tidak dikenal".into()))?;
    Ok(Json(terbitkan_pasangan(&s, pid, peran, keluarga).await?))
}

#[derive(Serialize)]
pub struct Saya {
    pub id: String,
    pub email: String,
    pub nama: String,
    pub peran: Peran,
}

/// Profil akun yang sedang masuk (semua peran).
async fn saya(State(s): State<AppState>, p: Pengguna) -> AppResult<Json<Saya>> {
    let (email, nama): (String, String) =
        sqlx::query_as("SELECT email, nama FROM pengguna WHERE id = $1")
            .bind(p.id)
            .fetch_optional(&s.db)
            .await?
            .ok_or_else(|| AppError::TidakTerotentikasi("Akun tidak ditemukan.".into()))?;
    Ok(Json(Saya {
        id: ids::ke_ulid(p.id),
        email,
        nama,
        peran: p.peran,
    }))
}

#[derive(Deserialize)]
pub struct UbahPeranRequest {
    pub peran: Peran,
}

/// Admin menetapkan peran akun (produsen/operator/kurator/admin/pembeli).
async fn ubah_peran(
    State(s): State<AppState>,
    p: Pengguna,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(req): Json<UbahPeranRequest>,
) -> AppResult<Json<serde_json::Value>> {
    p.wajib(&[Peran::Admin])?;
    let uid = ids::dari_ulid(&id, "").ok_or_else(|| {
        AppError::tidak_ditemukan("PENGGUNA_TIDAK_DITEMUKAN", "Pengguna tidak ditemukan.")
    })?;
    let n = sqlx::query("UPDATE pengguna SET peran = $2 WHERE id = $1")
        .bind(uid)
        .bind(req.peran.sebagai_str())
        .execute(&s.db)
        .await?
        .rows_affected();
    if n == 0 {
        return Err(AppError::tidak_ditemukan(
            "PENGGUNA_TIDAK_DITEMUKAN",
            "Pengguna tidak ditemukan.",
        ));
    }
    Ok(Json(serde_json::json!({"id": id, "peran": req.peran})))
}

pub fn rute() -> Router<AppState> {
    Router::new()
        .route("/saya", axum::routing::get(saya))
        .route(
            "/web/admin/pengguna/{id}/peran",
            axum::routing::patch(ubah_peran),
        )
        .route("/auth/login", post(login))
        .route("/auth/daftar", post(daftar))
        .route("/auth/refresh", post(refresh))
}
