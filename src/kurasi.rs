//! Kurasi: antrean FIFO (prioritas pangan segar) dan keputusan beraudit (peran kurator/admin).

use axum::{
    extract::{Path, Query, State},
    routing::{get, post},
    Json, Router,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    auth::{Pengguna, Peran},
    dukungan::{decode_cursor, encode_cursor, potong, Halaman},
    error::{AppError, AppResult},
    ids,
    json::ApiJson,
    model::{muat_media, muat_produk, muat_varian, sinkron_status_stok, KategoriKode},
    state::AppState,
};

const PERAN_KURASI: &[Peran] = &[Peran::Kurator, Peran::Admin];

#[derive(Deserialize)]
pub struct AntreanQuery {
    pub kategori: Option<String>,
    pub limit: Option<i64>,
    pub cursor: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct KursorAntrean {
    segar: bool,
    waktu: DateTime<Utc>,
    id: String,
}

async fn antrean(
    State(s): State<AppState>,
    p: Pengguna,
    Query(q): Query<AntreanQuery>,
) -> AppResult<Json<Value>> {
    p.wajib(PERAN_KURASI)?;
    let limit = Halaman {
        limit: q.limit,
        cursor: None,
    }
    .limit()?;
    if let Some(k) = &q.kategori {
        KategoriKode::dari_str(k)
            .ok_or_else(|| AppError::Permintaan("kategori tidak dikenal.".into()))?;
    }
    let kursor: Option<KursorAntrean> = q.cursor.as_deref().map(decode_cursor).transpose()?;
    let (k_segar, k_waktu, k_id) = match &kursor {
        Some(k) => (
            Some(k.segar),
            Some(k.waktu),
            Some(
                ids::dari_ulid(&k.id, "")
                    .ok_or_else(|| AppError::Permintaan("cursor tidak valid.".into()))?,
            ),
        ),
        None => (None, None, None),
    };
    // Urutan: pangan segar dulu (segar DESC), lalu FIFO (diajukan_pada ASC), id ASC.
    let rows: Vec<(Uuid, String, DateTime<Utc>, bool, i64)> = sqlx::query_as(
        "SELECT id, judul, waktu, segar, umur FROM ( \
            SELECT p.id, p.judul, COALESCE(p.diajukan_pada, p.dibuat_pada) AS waktu, \
                   (p.kategori_kode = 'pangan-segar') AS segar, \
                   GREATEST(0, EXTRACT(EPOCH FROM (now() - COALESCE(p.diajukan_pada, p.dibuat_pada))) / 3600)::bigint AS umur \
            FROM produk p WHERE p.status = 'MENUNGGU_KURASI' AND ($1::text IS NULL OR p.kategori_kode = $1)) t \
         WHERE ($2::boolean IS NULL \
                OR (NOT t.segar AND $2::boolean) \
                OR (t.segar = $2::boolean AND (t.waktu > $3 OR (t.waktu = $3 AND t.id > $4)))) \
         ORDER BY t.segar DESC, t.waktu ASC, t.id ASC LIMIT $5",
    )
    .bind(q.kategori.as_deref())
    .bind(k_segar)
    .bind(k_waktu)
    .bind(k_id)
    .bind(limit + 1)
    .fetch_all(&s.db)
    .await?;
    let (rows, lanjut) = potong(rows, limit);
    let cursor_lanjut = if lanjut {
        rows.last().map(|r| {
            encode_cursor(&KursorAntrean {
                segar: r.3,
                waktu: r.2,
                id: ids::ke_ulid(r.0),
            })
        })
    } else {
        None
    };
    Ok(Json(json!({
        "data": rows.iter().map(|r| json!({
            "produk_id": ids::ke_ulid(r.0), "judul": r.1, "umur_antrean_jam": r.4, "prioritas_pangan_segar": r.3,
        })).collect::<Vec<_>>(),
        "cursor_lanjut": cursor_lanjut,
    })))
}

#[derive(Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Keputusan {
    Setuju,
    Revisi,
    Tolak,
}

impl Keputusan {
    fn sebagai_str(&self) -> &'static str {
        match self {
            Keputusan::Setuju => "SETUJU",
            Keputusan::Revisi => "REVISI",
            Keputusan::Tolak => "TOLAK",
        }
    }
}

#[derive(Deserialize)]
pub struct KeputusanReq {
    pub keputusan: Keputusan,
    pub alasan_baku: Option<String>,
    pub catatan: Option<String>,
}

impl validator::Validate for KeputusanReq {
    fn validate(&self) -> Result<(), validator::ValidationErrors> {
        Ok(())
    }
}

fn alasan_baku_valid(a: &str) -> bool {
    (3..=40).contains(&a.len()) && a.bytes().all(|c| c.is_ascii_uppercase() || c == b'_')
}

async fn keputusan(
    State(s): State<AppState>,
    p: Pengguna,
    Path(produk_id): Path<String>,
    ApiJson(req): ApiJson<KeputusanReq>,
) -> AppResult<Json<Value>> {
    p.wajib(PERAN_KURASI)?;
    let nf = || AppError::tidak_ditemukan("PRODUK_TIDAK_DITEMUKAN", "Produk tidak ditemukan.");
    let pid = ids::dari_ulid(&produk_id, "").ok_or_else(nf)?;
    let catatan = req
        .catatan
        .as_deref()
        .map(str::trim)
        .filter(|c| !c.is_empty());
    let mut galat = Vec::new();
    if catatan.is_some_and(|c| c.chars().count() > 280) {
        galat.push(crate::error::KesalahanKolom::baru(
            "catatan",
            "Catatan maksimal 280 karakter.",
        ));
    }
    match req.keputusan {
        Keputusan::Setuju => {}
        Keputusan::Revisi | Keputusan::Tolak => match req.alasan_baku.as_deref() {
            None => galat.push(crate::error::KesalahanKolom::baru(
                "alasan_baku",
                "Alasan baku wajib untuk REVISI/TOLAK.",
            )),
            Some(a) if !alasan_baku_valid(a) => galat.push(crate::error::KesalahanKolom::baru(
                "alasan_baku",
                "Alasan baku berupa kode HURUF_BESAR_DENGAN_GARIS (contoh FOTO_TIDAK_ASLI).",
            )),
            _ => {}
        },
    }
    if matches!(req.keputusan, Keputusan::Tolak) && catatan.is_none() {
        galat.push(crate::error::KesalahanKolom::baru(
            "catatan",
            "TOLAK wajib menautkan pasal piagam kurasi pada catatan.",
        ));
    }
    if !galat.is_empty() {
        return Err(AppError::Validasi(galat));
    }

    let mut tx = s.db.begin().await?;
    sqlx::query("SELECT 1 FROM produk WHERE id = $1 FOR UPDATE")
        .bind(pid)
        .execute(&mut *tx)
        .await?;
    let pr = muat_produk(&mut tx, pid).await?;
    if pr.status != "MENUNGGU_KURASI" {
        return Err(AppError::konflik(
            "STATUS_TIDAK_VALID",
            format!("Produk berstatus {} bukan antrean kurasi.", pr.status),
        ));
    }
    let varian = muat_varian(&mut tx, pid).await?;
    let media = muat_media(&mut tx, pid).await?;
    let snapshot = json!({
        "judul": pr.judul, "kategori_kode": pr.kategori_kode, "tag": pr.tag, "deskripsi": pr.deskripsi,
        "komposisi": pr.komposisi, "alergen": pr.alergen, "varian": varian, "media": media,
    });
    sqlx::query("INSERT INTO kurasi_keputusan (id, produk_id, kurator_id, keputusan, alasan_baku, catatan, snapshot) VALUES ($1,$2,$3,$4,$5,$6,$7)")
        .bind(ids::baru())
        .bind(pid)
        .bind(p.id)
        .bind(req.keputusan.sebagai_str())
        .bind(req.alasan_baku.as_deref())
        .bind(catatan)
        .bind(&snapshot)
        .execute(&mut *tx)
        .await?;
    let status = match req.keputusan {
        Keputusan::Setuju => {
            sqlx::query("UPDATE produk SET status = 'TAYANG', perlu_revisi_catatan = NULL, perlu_revisi_pada = NULL, diperbarui_pada = now() WHERE id = $1")
                .bind(pid)
                .execute(&mut *tx)
                .await?;
            // Mulai promo lapak baru (5% selama 30 hari) pada penayangan pertama.
            sqlx::query("UPDATE lapak SET produk_pertama_tayang_pada = now() WHERE id = $1 AND produk_pertama_tayang_pada IS NULL")
                .bind(pr.lapak_id)
                .execute(&mut *tx)
                .await?;
            sinkron_status_stok(&mut tx, pid).await?
        }
        Keputusan::Revisi => {
            let isi = catatan.map(str::to_string).or(req.alasan_baku.clone());
            sqlx::query("UPDATE produk SET status = 'PERLU_REVISI', perlu_revisi_catatan = $2, perlu_revisi_pada = now(), diperbarui_pada = now() WHERE id = $1")
                .bind(pid)
                .bind(isi)
                .execute(&mut *tx)
                .await?;
            "PERLU_REVISI".to_string()
        }
        Keputusan::Tolak => {
            sqlx::query("UPDATE produk SET status = 'TAKEDOWN', perlu_revisi_catatan = $2, diperbarui_pada = now() WHERE id = $1")
                .bind(pid)
                .bind(catatan)
                .execute(&mut *tx)
                .await?;
            "TAKEDOWN".to_string()
        }
    };
    tx.commit().await?;
    Ok(Json(json!({"status": status})))
}

pub fn rute() -> Router<AppState> {
    Router::new()
        .route("/web/kurasi/antrean", get(antrean))
        .route("/web/kurasi/{produk_id}/keputusan", post(keputusan))
}
