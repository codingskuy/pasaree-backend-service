//! Pembantu bersama: paginasi cursor dan idempotensi.

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::Value;
use sqlx::PgConnection;
use uuid::Uuid;

use crate::error::{AppError, AppResult};

pub const LIMIT_DEFAULT: i64 = 20;
pub const LIMIT_MAKS: i64 = 50;

#[derive(Debug, Deserialize, Default)]
pub struct Halaman {
    pub limit: Option<i64>,
    pub cursor: Option<String>,
}

impl Halaman {
    /// limit default 20, maksimum 50; di luar 1..=50 -> 400.
    pub fn limit(&self) -> AppResult<i64> {
        match self.limit {
            None => Ok(LIMIT_DEFAULT),
            Some(l) if (1..=LIMIT_MAKS).contains(&l) => Ok(l),
            Some(_) => Err(AppError::Permintaan(format!("limit harus 1-{LIMIT_MAKS}."))),
        }
    }
}

pub fn encode_cursor<T: Serialize>(v: &T) -> String {
    URL_SAFE_NO_PAD.encode(serde_json::to_vec(v).unwrap_or_default())
}

pub fn decode_cursor<T: DeserializeOwned>(s: &str) -> AppResult<T> {
    URL_SAFE_NO_PAD
        .decode(s)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .ok_or_else(|| AppError::Permintaan("cursor tidak valid.".into()))
}

/// Pecah hasil `limit + 1` baris menjadi (baris, ada_lanjut).
pub fn potong<T>(mut baris: Vec<T>, limit: i64) -> (Vec<T>, bool) {
    let lanjut = baris.len() as i64 > limit;
    baris.truncate(limit as usize);
    (baris, lanjut)
}

/// Kunci idempotensi: ambil kunci advisory transaksi, lalu cari respons tersimpan.
/// Panggil di awal transaksi; bila ada hasil, kembalikan apa adanya tanpa efek ganda.
pub async fn idempotensi_awal(
    c: &mut PgConnection,
    cakupan: &str,
    kunci: &str,
) -> AppResult<Option<(i32, Value)>> {
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(format!("{cakupan}|{kunci}"))
        .execute(&mut *c)
        .await?;
    let r: Option<(i32, Value)> = sqlx::query_as(
        "SELECT status_http, respons FROM idempotensi WHERE cakupan = $1 AND kunci = $2",
    )
    .bind(cakupan)
    .bind(kunci)
    .fetch_optional(&mut *c)
    .await?;
    Ok(r)
}

pub async fn idempotensi_simpan(
    c: &mut PgConnection,
    cakupan: &str,
    kunci: &str,
    pengguna: Uuid,
    status: u16,
    respons: &Value,
) -> AppResult<()> {
    sqlx::query("INSERT INTO idempotensi (cakupan, kunci, pengguna_id, status_http, respons) VALUES ($1,$2,$3,$4,$5)")
        .bind(cakupan)
        .bind(kunci)
        .bind(pengguna)
        .bind(status as i32)
        .bind(respons)
        .execute(c)
        .await?;
    Ok(())
}

#[cfg(test)]
mod uji {
    use super::*;

    #[test]
    fn limit_default_dan_batas() {
        assert_eq!(Halaman::default().limit().unwrap(), 20);
        assert_eq!(
            Halaman {
                limit: Some(50),
                cursor: None
            }
            .limit()
            .unwrap(),
            50
        );
        assert!(Halaman {
            limit: Some(51),
            cursor: None
        }
        .limit()
        .is_err());
        assert!(Halaman {
            limit: Some(0),
            cursor: None
        }
        .limit()
        .is_err());
    }

    #[test]
    fn cursor_bolak_balik_dan_rusak() {
        let c = encode_cursor(&serde_json::json!({"id": "x"}));
        let v: Value = decode_cursor(&c).unwrap();
        assert_eq!(v["id"], "x");
        assert!(decode_cursor::<Value>("###").is_err());
    }

    #[test]
    fn potong_menandai_lanjut() {
        let (b, l) = potong(vec![1, 2, 3], 2);
        assert_eq!((b, l), (vec![1, 2], true));
        let (b, l) = potong(vec![1, 2], 2);
        assert_eq!((b, l), (vec![1, 2], false));
    }
}
