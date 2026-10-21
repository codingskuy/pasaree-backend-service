//! Katalog baca (auth opsional): harga & stok real-time langsung dari DB.

use axum::{
    extract::{Path, Query, State},
    routing::get,
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::{FromRow, Postgres, QueryBuilder};
use uuid::Uuid;

use crate::{
    auth::PenggunaOpsional,
    dukungan::{decode_cursor, encode_cursor, potong, Halaman},
    error::{AppError, AppResult},
    ids,
    model::{muat_media, muat_varian, KategoriKode, Tag},
    state::AppState,
};

#[derive(Debug, Deserialize, Default)]
pub struct KatalogQuery {
    pub q: Option<String>,
    pub kategori: Option<String>,
    pub tag: Option<String>,
    pub lapak_slug: Option<String>,
    pub sort: Option<String>,
    pub limit: Option<i64>,
    pub cursor: Option<String>,
}

#[derive(Serialize, Deserialize, Default)]
struct KursorKatalog {
    h: Option<i64>,
    r: Option<f64>,
    id: String,
}

#[derive(FromRow)]
struct BarisRingkas {
    id: Uuid,
    judul: String,
    kategori_kode: String,
    rating_rata: f64,
    slug: String,
    nama_lapak: String,
    harga_mulai: i64,
    media_utama: Option<String>,
    harga_baru_berubah: bool,
}

fn ringkas_json(b: &BarisRingkas) -> Value {
    json!({
        "id": ids::ke_ulid(b.id),
        "judul": b.judul,
        "kategori_kode": b.kategori_kode,
        "harga_mulai_idr": b.harga_mulai,
        "rating_rata": b.rating_rata,
        "lapak": {"slug": b.slug, "nama_lapak": b.nama_lapak},
        "media_utama": b.media_utama,
    })
}

const SELEKSI_RINGKAS: &str = "SELECT p.id, p.judul, p.kategori_kode, p.rating_rata, l.slug, l.nama_lapak, \
    (SELECT MIN(v.harga_idr) FROM varian_produk v WHERE v.produk_id = p.id)::bigint AS harga_mulai, \
    (SELECT m.url FROM media_produk m WHERE m.produk_id = p.id AND m.utama LIMIT 1) AS media_utama, \
    EXISTS (SELECT 1 FROM harga_riwayat h WHERE h.produk_id = p.id AND h.harga_lama IS NOT NULL \
            AND h.diubah_pada > now() - interval '10 minutes') AS harga_baru_berubah \
    FROM produk p JOIN lapak l ON l.id = p.lapak_id \
    WHERE p.status = 'TAYANG' AND l.status = 'AKTIF'";

fn escape_like(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

async fn daftar_produk(
    State(s): State<AppState>,
    _p: PenggunaOpsional,
    Query(q): Query<KatalogQuery>,
) -> AppResult<Json<Value>> {
    let limit = Halaman {
        limit: q.limit,
        cursor: None,
    }
    .limit()?;
    if let Some(k) = &q.kategori {
        KategoriKode::dari_str(k)
            .ok_or_else(|| AppError::Permintaan("kategori tidak dikenal.".into()))?;
    }
    if let Some(t) = &q.tag {
        Tag::dari_str(t).ok_or_else(|| AppError::Permintaan("tag tidak dikenal.".into()))?;
    }
    let sort = q.sort.as_deref().unwrap_or("terbaru");
    if !["harga_asc", "harga_desc", "rating", "terbaru"].contains(&sort) {
        return Err(AppError::Permintaan("sort tidak dikenal.".into()));
    }
    let kursor: Option<KursorKatalog> = q.cursor.as_deref().map(decode_cursor).transpose()?;
    let kursor_id = match &kursor {
        Some(k) => Some(
            ids::dari_ulid(&k.id, "")
                .ok_or_else(|| AppError::Permintaan("cursor tidak valid.".into()))?,
        ),
        None => None,
    };

    let mut qb: QueryBuilder<Postgres> = QueryBuilder::new("SELECT * FROM (");
    qb.push(SELEKSI_RINGKAS);
    if let Some(t) = q.q.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
        let pola = format!("%{}%", escape_like(t));
        qb.push(" AND (p.judul ILIKE ")
            .push_bind(pola.clone())
            .push(" OR p.deskripsi ILIKE ")
            .push_bind(pola)
            .push(")");
    }
    if let Some(k) = &q.kategori {
        qb.push(" AND p.kategori_kode = ").push_bind(k.clone());
    }
    if let Some(t) = &q.tag {
        qb.push(" AND ").push_bind(t.clone()).push(" = ANY(p.tag)");
    }
    if let Some(slug) = &q.lapak_slug {
        qb.push(" AND l.slug = ").push_bind(slug.clone());
    }
    qb.push(") t WHERE TRUE");
    if let (Some(k), Some(cid)) = (&kursor, kursor_id) {
        match sort {
            "terbaru" => {
                qb.push(" AND t.id < ").push_bind(cid);
            }
            "harga_asc" => {
                let h = k.h.ok_or_else(|| {
                    AppError::Permintaan("cursor tidak cocok dengan sort.".into())
                })?;
                qb.push(" AND (t.harga_mulai > ")
                    .push_bind(h)
                    .push(" OR (t.harga_mulai = ")
                    .push_bind(h)
                    .push(" AND t.id > ")
                    .push_bind(cid)
                    .push("))");
            }
            "harga_desc" => {
                let h = k.h.ok_or_else(|| {
                    AppError::Permintaan("cursor tidak cocok dengan sort.".into())
                })?;
                qb.push(" AND (t.harga_mulai < ")
                    .push_bind(h)
                    .push(" OR (t.harga_mulai = ")
                    .push_bind(h)
                    .push(" AND t.id > ")
                    .push_bind(cid)
                    .push("))");
            }
            _ => {
                let r = k.r.ok_or_else(|| {
                    AppError::Permintaan("cursor tidak cocok dengan sort.".into())
                })?;
                qb.push(" AND (t.rating_rata < ")
                    .push_bind(r)
                    .push(" OR (t.rating_rata = ")
                    .push_bind(r)
                    .push(" AND t.id > ")
                    .push_bind(cid)
                    .push("))");
            }
        }
    }
    qb.push(match sort {
        "harga_asc" => " ORDER BY t.harga_mulai ASC, t.id ASC",
        "harga_desc" => " ORDER BY t.harga_mulai DESC, t.id ASC",
        "rating" => " ORDER BY t.rating_rata DESC, t.id ASC",
        _ => " ORDER BY t.id DESC",
    });
    qb.push(" LIMIT ").push_bind(limit + 1);

    let baris: Vec<BarisRingkas> = qb.build_query_as().fetch_all(&s.db).await?;
    let (baris, lanjut) = potong(baris, limit);
    let cursor_lanjut = if lanjut {
        baris.last().map(|b| {
            encode_cursor(&KursorKatalog {
                h: Some(b.harga_mulai),
                r: Some(b.rating_rata),
                id: ids::ke_ulid(b.id),
            })
        })
    } else {
        None
    };
    Ok(Json(json!({
        "data": baris.iter().map(ringkas_json).collect::<Vec<_>>(),
        "cursor_lanjut": cursor_lanjut,
        "harga_berubah": baris.iter().any(|b| b.harga_baru_berubah),
    })))
}

#[derive(FromRow)]
struct BarisDetail {
    id: Uuid,
    judul: String,
    kategori_kode: String,
    tag: Vec<String>,
    deskripsi: String,
    rating_rata: f64,
    total_ulasan: i32,
    slug: String,
    nama_lapak: String,
}

async fn detail_produk(
    State(s): State<AppState>,
    _p: PenggunaOpsional,
    Path(id): Path<String>,
) -> AppResult<Json<Value>> {
    let nf = || AppError::tidak_ditemukan("PRODUK_TIDAK_DITEMUKAN", "Produk tidak ditemukan.");
    let uid = ids::dari_ulid(&id, "").ok_or_else(nf)?;
    let mut c = s.db.acquire().await?;
    let p: BarisDetail = sqlx::query_as(
        "SELECT p.id, p.judul, p.kategori_kode, p.tag, p.deskripsi, p.rating_rata, p.total_ulasan, l.slug, l.nama_lapak \
         FROM produk p JOIN lapak l ON l.id = p.lapak_id \
         WHERE p.id = $1 AND p.status IN ('TAYANG','NONAKTIF_STOK_0') AND l.status = 'AKTIF'",
    )
    .bind(uid)
    .fetch_optional(&mut *c)
    .await?
    .ok_or_else(nf)?;
    let varian = muat_varian(&mut c, uid).await?;
    let media = muat_media(&mut c, uid).await?;
    let stok: serde_json::Map<String, Value> = varian
        .iter()
        .map(|v| (ids::ke_ulid(v.id), json!(v.stok)))
        .collect();
    let (trip_tersedia, basi) = crate::trip::trip_untuk_produk(&s).await;
    Ok(Json(json!({
        "id": ids::ke_ulid(p.id),
        "judul": p.judul,
        "kategori_kode": p.kategori_kode,
        "tag": p.tag,
        "deskripsi": p.deskripsi,
        "lapak": {"slug": p.slug, "nama_lapak": p.nama_lapak},
        "varian": varian,
        "media": media,
        "rating_rata": p.rating_rata,
        "total_ulasan": p.total_ulasan,
        "stok_per_varian": stok,
        "trip_tersedia": trip_tersedia,
        "basi": basi,
    })))
}

async fn profil_lapak(
    State(s): State<AppState>,
    _p: PenggunaOpsional,
    Path(slug): Path<String>,
) -> AppResult<Json<Value>> {
    let nf = || AppError::tidak_ditemukan("LAPAK_TIDAK_DITEMUKAN", "Lapak tidak ditemukan.");
    let l: (Uuid, String, String, String) = sqlx::query_as(
        "SELECT id, slug, nama_lapak, deskripsi FROM lapak WHERE slug = $1 AND status = 'AKTIF'",
    )
    .bind(&slug)
    .fetch_optional(&s.db)
    .await?
    .ok_or_else(nf)?;
    let mut qb: QueryBuilder<Postgres> = QueryBuilder::new(SELEKSI_RINGKAS);
    qb.push(" AND l.id = ")
        .push_bind(l.0)
        .push(" ORDER BY p.id DESC LIMIT 50");
    let baris: Vec<BarisRingkas> = qb.build_query_as().fetch_all(&s.db).await?;
    let skor = crate::lapak::skor_fulfillment(&s.db, l.0).await?;
    Ok(Json(json!({
        "slug": l.1,
        "nama_lapak": l.2,
        "deskripsi": l.3,
        "produk": baris.iter().map(ringkas_json).collect::<Vec<_>>(),
        "skor_fulfillment_persen": skor,
    })))
}

pub fn rute() -> Router<AppState> {
    Router::new()
        .route("/katalog/produk", get(daftar_produk))
        .route("/katalog/produk/{id}", get(detail_produk))
        .route("/katalog/lapak/{slug}", get(profil_lapak))
}
