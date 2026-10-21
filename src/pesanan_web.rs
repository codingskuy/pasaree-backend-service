//! Sisi lapak atas pesanan: pesanan masuk, konfirmasi (maks 2 jam jam kerja), serah ke trip Lumbung.

use axum::{
    extract::{Path, Query, State},
    http::HeaderMap,
    routing::{get, post},
    Json, Router,
};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;
use validator::Validate;

use crate::{
    auth::{Pengguna, Peran},
    dukungan::{idempotensi_awal, idempotensi_simpan},
    error::{AppError, AppResult},
    ids,
    json::ApiJson,
    lapak::lapak_boleh,
    pesanan::{batalkan_sub, sinkron_induk, ubah_status_sub, Fase, Status, AWALAN_PESANAN},
    state::AppState,
};

const PERAN_LAPAK: &[Peran] = &[Peran::Produsen, Peran::Operator, Peran::Admin];

fn nf_sub() -> AppError {
    AppError::tidak_ditemukan(
        "SUB_PESANAN_TIDAK_DITEMUKAN",
        "Sub-pesanan tidak ditemukan.",
    )
}

#[derive(Deserialize)]
pub struct MasukQuery {
    pub status: Option<String>,
    pub lapak_id: Option<String>,
}

#[derive(sqlx::FromRow)]
struct BarisMasuk {
    id: Uuid,
    pesanan_id: Uuid,
    subtotal_idr: i64,
    batas_konfirmasi_hingga: Option<DateTime<Utc>>,
    trip_id: String,
}

async fn pesanan_masuk(
    State(s): State<AppState>,
    p: Pengguna,
    Query(q): Query<MasukQuery>,
) -> AppResult<Json<Value>> {
    p.wajib(PERAN_LAPAK)?;
    // `DIKONFIRMASI_MENUNGGU` (contoh pada kontrak) = menunggu konfirmasi lapak = DIBAYAR.
    let status: Option<String> = match q.status.as_deref() {
        None => None,
        Some("DIKONFIRMASI_MENUNGGU") => Some("DIBAYAR".into()),
        Some(st) => Some(
            Status::dari_str(st)
                .ok_or_else(|| AppError::Permintaan("status tidak dikenal.".into()))?
                .sebagai_str()
                .to_string(),
        ),
    };
    let lapak_id = match q.lapak_id.as_deref() {
        Some(l) => {
            let id = ids::dari_ulid(l, "").ok_or_else(|| {
                AppError::tidak_ditemukan("LAPAK_TIDAK_DITEMUKAN", "Lapak tidak ditemukan.")
            })?;
            lapak_boleh(&s.db, &p, id, false).await?;
            Some(id)
        }
        None => None,
    };
    // Tanpa filter status: sub-pesanan yang masih berjalan sisi lapak (DIBAYAR, DIKONFIRMASI_LAPAK).
    let rows: Vec<BarisMasuk> = sqlx::query_as(
        "SELECT sp.id, sp.pesanan_id, sp.subtotal_idr, sp.batas_konfirmasi_hingga, sp.trip_id \
         FROM sub_pesanan sp WHERE sp.status <> 'DIPESAN' \
           AND (($1::text IS NULL AND sp.status IN ('DIBAYAR','DIKONFIRMASI_LAPAK')) OR sp.status = $1) \
           AND ($2::uuid IS NULL OR sp.lapak_id = $2) \
           AND ($3 OR sp.lapak_id IN ( \
               SELECT l.id FROM lapak l JOIN produsen d ON d.id = l.produsen_id WHERE d.pengguna_id = $4 \
               UNION SELECT o.lapak_id FROM lapak_operator o WHERE o.pengguna_id = $4)) \
         ORDER BY sp.batas_konfirmasi_hingga NULLS LAST, sp.id LIMIT 200",
    )
    .bind(status.as_deref())
    .bind(lapak_id)
    .bind(p.peran == Peran::Admin)
    .bind(p.id)
    .fetch_all(&s.db)
    .await?;
    let mut data = Vec::new();
    for BarisMasuk {
        id: sub,
        pesanan_id: pesanan,
        subtotal_idr: total,
        batas_konfirmasi_hingga: batas,
        trip_id: trip,
    } in rows
    {
        let varian: Vec<(String, i32)> = sqlx::query_as(
            "SELECT sku, qty FROM item_pesanan WHERE sub_pesanan_id = $1 ORDER BY id",
        )
        .bind(sub)
        .fetch_all(&s.db)
        .await?;
        data.push(json!({
            "sub_id": ids::ke_ulid(sub),
            "pesanan_id": ids::dengan_awalan(AWALAN_PESANAN, pesanan),
            "varian": varian.iter().map(|v| json!({"sku": v.0, "qty": v.1})).collect::<Vec<_>>(),
            "total_idr": total,
            "batas_konfirmasi_hingga": batas,
            "trip_id": trip,
        }));
    }
    Ok(Json(json!({ "data": data })))
}

#[derive(sqlx::FromRow)]
struct BarisSub {
    id: Uuid,
    pesanan_id: Uuid,
    lapak_id: Uuid,
    trip_id: String,
    status: String,
    batas_konfirmasi_hingga: Option<DateTime<Utc>>,
}

async fn sub_akses(
    c: &mut sqlx::PgConnection,
    p: &Pengguna,
    sub_id: &str,
    kunci: bool,
) -> AppResult<BarisSub> {
    p.wajib(PERAN_LAPAK)?;
    let id = ids::dari_ulid(sub_id, "").ok_or_else(nf_sub)?;
    let sql = format!(
        "SELECT id, pesanan_id, lapak_id, trip_id, status, batas_konfirmasi_hingga FROM sub_pesanan WHERE id = $1{}",
        if kunci { " FOR UPDATE" } else { "" }
    );
    let b: BarisSub = sqlx::query_as(&sql)
        .bind(id)
        .fetch_optional(&mut *c)
        .await?
        .ok_or_else(nf_sub)?;
    lapak_boleh(&mut *c, p, b.lapak_id, false)
        .await
        .map_err(|e| match e {
            AppError::TidakDitemukan { .. } => nf_sub(),
            l => l,
        })?;
    Ok(b)
}

fn header_kunci(h: &HeaderMap) -> AppResult<Option<String>> {
    match h.get("idempotency-key") {
        None => Ok(None),
        Some(v) => {
            let t = v
                .to_str()
                .ok()
                .filter(|t| Uuid::parse_str(t).is_ok())
                .ok_or_else(|| AppError::Permintaan("Idempotency-Key harus UUID.".into()))?;
            Ok(Some(t.to_string()))
        }
    }
}

#[derive(Deserialize, Validate)]
pub struct KonfirmasiReq {
    pub terima: bool,
    #[validate(length(max = 200, message = "alasan maksimal 200 karakter"))]
    pub alasan: Option<String>,
}

async fn konfirmasi(
    State(s): State<AppState>,
    p: Pengguna,
    Path(sub_id): Path<String>,
    headers: HeaderMap,
    ApiJson(req): ApiJson<KonfirmasiReq>,
) -> AppResult<Json<Value>> {
    let kunci = header_kunci(&headers)?;
    let alasan = req
        .alasan
        .as_deref()
        .map(str::trim)
        .filter(|a| !a.is_empty());
    if !req.terima && alasan.is_none() {
        return Err(AppError::validasi("alasan", "Alasan wajib bila menolak."));
    }
    let mut tx = s.db.begin().await?;
    let cakupan = format!("konfirmasi:{sub_id}");
    if let Some(k) = &kunci {
        if let Some((_, resp)) = idempotensi_awal(&mut tx, &cakupan, k).await? {
            return Ok(Json(resp));
        }
    }
    let sub = sub_akses(&mut tx, &p, &sub_id, true).await?;
    if sub.status != "DIBAYAR" {
        return Err(AppError::konflik(
            "STATUS_TIDAK_VALID",
            format!("Status {} tidak mengizinkan konfirmasi.", sub.status),
        ));
    }
    if sub.batas_konfirmasi_hingga.is_some_and(|b| b < Utc::now()) {
        return Err(AppError::konflik(
            "KONFIRMASI_KEDALUWARSA",
            "Batas konfirmasi 2 jam jam kerja terlewati; pesanan dibatalkan otomatis.",
        ));
    }
    sqlx::query("UPDATE sub_pesanan SET direspons_pada = now() WHERE id = $1")
        .bind(sub.id)
        .execute(&mut *tx)
        .await?;
    let status = if req.terima {
        ubah_status_sub(
            &mut tx,
            sub.id,
            Status::Dibayar,
            Status::DikonfirmasiLapak,
            Some("dikonfirmasi_pada"),
        )
        .await?;
        Status::DikonfirmasiLapak
    } else {
        batalkan_sub(
            &mut tx,
            sub.id,
            alasan.unwrap_or_default(),
            Fase::PraKonfirmasi,
        )
        .await?;
        Status::Dibatalkan
    };
    sinkron_induk(&mut tx, sub.pesanan_id).await?;
    let resp = json!({"status": status.sebagai_str()});
    if let Some(k) = &kunci {
        idempotensi_simpan(&mut tx, &cakupan, k, p.id, 200, &resp).await?;
    }
    tx.commit().await?;
    Ok(Json(resp))
}

#[derive(Deserialize, Validate)]
pub struct SerahTripReq {
    #[validate(length(min = 1, max = 60, message = "trip_id wajib diisi"))]
    pub trip_id: String,
    #[validate(length(min = 4, max = 64, message = "kode_serah 4-64 karakter"))]
    pub kode_serah: String,
}

#[derive(Deserialize)]
struct VerifikasiSerah {
    cocok: bool,
    bukti_id: Option<String>,
}

/// Kontrak asumsi ke Lumbung: `POST /internal/pasaree/serah-terima/verifikasi` -> `{"cocok": bool, "bukti_id": "..."}`.
async fn serah_trip(
    State(s): State<AppState>,
    p: Pengguna,
    Path(sub_id): Path<String>,
    headers: HeaderMap,
    ApiJson(req): ApiJson<SerahTripReq>,
) -> AppResult<Json<Value>> {
    let kunci = header_kunci(&headers)?;
    let cakupan = format!("serah-trip:{sub_id}");
    if let Some(k) = &kunci {
        let mut c = s.db.begin().await?;
        if let Some((_, resp)) = idempotensi_awal(&mut c, &cakupan, k).await? {
            return Ok(Json(resp));
        }
    }
    let sub = {
        let mut c = s.db.acquire().await?;
        sub_akses(&mut c, &p, &sub_id, false).await?
    };
    if sub.status != "DIKONFIRMASI_LAPAK" {
        return Err(AppError::konflik(
            "STATUS_TIDAK_VALID",
            format!("Status {} tidak mengizinkan serah ke trip.", sub.status),
        ));
    }
    if sub.trip_id != req.trip_id {
        return Err(AppError::validasi(
            "trip_id",
            "Trip tidak sama dengan trip yang dipilih pembeli.",
        ));
    }
    // Panggilan ke Lumbung di luar transaksi (jangan menahan koneksi DB selama HTTP).
    let verifikasi: Result<VerifikasiSerah, _> = s
        .lumbung
        .post_json(
            "/internal/pasaree/serah-terima/verifikasi",
            &json!({"trip_id": req.trip_id, "kode_serah": req.kode_serah, "sub_id": ids::ke_ulid(sub.id), "pesanan_id": ids::dengan_awalan(AWALAN_PESANAN, sub.pesanan_id)}),
        )
        .await;
    let bukti = match verifikasi {
        Ok(v) if v.cocok => v.bukti_id.unwrap_or_else(|| req.kode_serah.clone()),
        Ok(_) => {
            return Err(AppError::validasi(
                "kode_serah",
                "Kode serah tidak cocok dengan pindaian kurir Lumbung.",
            ))
        }
        Err(e) if matches!(e.status, Some(404 | 409 | 422)) => {
            return Err(AppError::validasi(
                "kode_serah",
                "Kode serah tidak cocok dengan pindaian kurir Lumbung.",
            ))
        }
        Err(e) => {
            tracing::warn!(?e, "verifikasi serah-terima ke Lumbung gagal");
            return Err(AppError::Hulu(
                "Lumbung tidak dapat dihubungi; coba lagi.".into(),
            ));
        }
    };

    let mut tx = s.db.begin().await?;
    if let Some(k) = &kunci {
        if let Some((_, resp)) = idempotensi_awal(&mut tx, &cakupan, k).await? {
            return Ok(Json(resp));
        }
    }
    sub_akses(&mut tx, &p, &sub_id, true).await?;
    ubah_status_sub(
        &mut tx,
        sub.id,
        Status::DikonfirmasiLapak,
        Status::DiserahkanKeTrip,
        Some("diserahkan_pada"),
    )
    .await?;
    sqlx::query("UPDATE sub_pesanan SET bukti_serah_trip = $2 WHERE id = $1")
        .bind(sub.id)
        .bind(&bukti)
        .execute(&mut *tx)
        .await?;
    crate::keuangan::lepas_escrow_sub(&mut tx, sub.id).await?;
    sinkron_induk(&mut tx, sub.pesanan_id).await?;
    let resp = json!({"status": "DISERAHKAN_KE_TRIP"});
    if let Some(k) = &kunci {
        idempotensi_simpan(&mut tx, &cakupan, k, p.id, 200, &resp).await?;
    }
    tx.commit().await?;
    Ok(Json(resp))
}

pub fn rute() -> Router<AppState> {
    Router::new()
        .route("/web/pesanan-masuk", get(pesanan_masuk))
        .route("/web/pesanan-masuk/{sub_id}/konfirmasi", post(konfirmasi))
        .route("/web/pesanan-masuk/{sub_id}/serah-trip", post(serah_trip))
}
