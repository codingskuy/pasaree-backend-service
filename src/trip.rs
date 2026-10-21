//! Trip Lumbung: proksi baca dengan cache 60 detik dan flag `basi` bila Lumbung tidak merespons.
//! Pasaree tidak membuat trip sendiri; checkout tanpa trip valid diblokir.
//!
//! Kontrak yang diasumsikan ke Lumbung (belum ada di OpenAPI Lumbung):
//! `GET {LUMBUNG_BASE_URL}/internal/pasaree/trip?dari=&sampai=&titik_id=` -> `{"data":[Trip...]}`.

use std::{collections::HashMap, sync::Mutex, time::Duration};

use axum::{
    extract::{Query, State},
    routing::get,
    Json, Router,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::{
    auth::Pengguna,
    error::{AppError, AppResult},
    state::AppState,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Trip {
    pub trip_id: String,
    pub jadwal_berangkat: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rute: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub titik_serah: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sisa_kapasitas: Option<i64>,
    pub ongkir_idr: i64,
}

#[derive(Deserialize)]
struct DaftarTrip {
    data: Vec<Trip>,
}

type EntriCache = (DateTime<Utc>, Vec<Trip>);

pub struct TripCache {
    ttl: Duration,
    peta: Mutex<HashMap<String, EntriCache>>,
}

impl TripCache {
    pub fn baru(ttl: Duration) -> Self {
        Self {
            ttl,
            peta: Mutex::new(HashMap::new()),
        }
    }

    fn segar(&self, kunci: &str) -> Option<Vec<Trip>> {
        let peta = self.peta.lock().ok()?;
        let (waktu, data) = peta.get(kunci)?;
        let umur = Utc::now()
            .signed_duration_since(*waktu)
            .to_std()
            .unwrap_or_default();
        (umur < self.ttl).then(|| data.clone())
    }

    fn terakhir(&self, kunci: &str) -> Option<Vec<Trip>> {
        self.peta.lock().ok()?.get(kunci).map(|(_, d)| d.clone())
    }

    fn simpan(&self, kunci: &str, data: &[Trip]) {
        if let Ok(mut peta) = self.peta.lock() {
            peta.insert(kunci.to_string(), (Utc::now(), data.to_vec()));
        }
    }
}

/// Ambil trip dari cache (<60 dtk) atau Lumbung; bila Lumbung gagal kembalikan cache terakhir dengan `basi = true`
/// (daftar kosong bila belum pernah ada cache).
pub async fn ambil(
    s: &AppState,
    dari: Option<&str>,
    sampai: Option<&str>,
    titik_id: Option<&str>,
) -> (Vec<Trip>, bool) {
    let kunci = format!(
        "{}|{}|{}",
        dari.unwrap_or(""),
        sampai.unwrap_or(""),
        titik_id.unwrap_or("")
    );
    if let Some(d) = s.trip_cache.segar(&kunci) {
        return (d, false);
    }
    let mut query: Vec<(&str, String)> = Vec::new();
    if let Some(v) = dari {
        query.push(("dari", v.to_string()));
    }
    if let Some(v) = sampai {
        query.push(("sampai", v.to_string()));
    }
    if let Some(v) = titik_id {
        query.push(("titik_id", v.to_string()));
    }
    match s
        .lumbung
        .get_json::<DaftarTrip>("/internal/pasaree/trip", &query)
        .await
    {
        Ok(hasil) => {
            s.trip_cache.simpan(&kunci, &hasil.data);
            (hasil.data, false)
        }
        Err(e) => {
            tracing::warn!(?e, "Lumbung tidak merespons; memakai cache trip terakhir");
            (s.trip_cache.terakhir(&kunci).unwrap_or_default(), true)
        }
    }
}

/// Trip untuk detail produk; kegagalan tidak memblokir detail produk.
pub async fn trip_untuk_produk(s: &AppState) -> (Vec<Value>, bool) {
    let (trip, basi) = ambil(s, None, None, None).await;
    let sekarang = Utc::now();
    let v = trip
        .into_iter()
        .filter(|t| t.jadwal_berangkat > sekarang && t.sisa_kapasitas != Some(0))
        .filter_map(|t| serde_json::to_value(t).ok())
        .collect();
    (v, basi)
}

#[derive(Deserialize)]
pub struct TripQuery {
    pub dari: Option<String>,
    pub sampai: Option<String>,
    pub titik_id: Option<String>,
}

fn valid_waktu(nama: &str, v: &Option<String>) -> AppResult<()> {
    match v {
        Some(t) if DateTime::parse_from_rfc3339(t).is_err() => Err(AppError::Permintaan(format!(
            "{nama} harus date-time ISO-8601."
        ))),
        _ => Ok(()),
    }
}

async fn daftar_trip(
    State(s): State<AppState>,
    _p: Pengguna,
    Query(q): Query<TripQuery>,
) -> AppResult<Json<Value>> {
    valid_waktu("dari", &q.dari)?;
    valid_waktu("sampai", &q.sampai)?;
    let (data, basi) = ambil(
        &s,
        q.dari.as_deref(),
        q.sampai.as_deref(),
        q.titik_id.as_deref(),
    )
    .await;
    Ok(Json(json!({"data": data, "basi": basi})))
}

pub fn rute() -> Router<AppState> {
    Router::new().route("/trip", get(daftar_trip))
}
