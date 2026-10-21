//! Lapak dan manajemen produk (peran produsen/operator; admin penuh).

use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    routing::{get, patch, post},
    Json, Router,
};
use chrono::{NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;
use validator::{Validate, ValidationError};

use crate::{
    auth::{Pengguna, Peran},
    cek,
    dukungan::{
        decode_cursor, encode_cursor, idempotensi_awal, idempotensi_simpan, potong, Halaman,
    },
    error::{AppError, AppResult},
    ids,
    json::ApiJson,
    model::{
        data_cek, judul_lain, muat_media, muat_produk, muat_varian, sinkron_status_stok,
        JenisMedia, JenisProdusen, KategoriKode, ProdukStatus, Satuan, Tag,
    },
    state::AppState,
};

const PERAN_LAPAK: &[Peran] = &[Peran::Produsen, Peran::Operator, Peran::Admin];

fn nf_lapak() -> AppError {
    AppError::tidak_ditemukan("LAPAK_TIDAK_DITEMUKAN", "Lapak tidak ditemukan.")
}
fn nf_produk() -> AppError {
    AppError::tidak_ditemukan("PRODUK_TIDAK_DITEMUKAN", "Produk tidak ditemukan.")
}
fn nf_varian() -> AppError {
    AppError::tidak_ditemukan("VARIAN_TIDAK_DITEMUKAN", "Varian tidak ditemukan.")
}

fn parse_id(s: &str, nf: fn() -> AppError) -> AppResult<Uuid> {
    ids::dari_ulid(s, "").ok_or_else(nf)
}

/// Apakah pengguna boleh mengelola lapak ini. `profil` = ubah profil (operator tidak boleh).
pub async fn lapak_boleh(
    db: impl sqlx::PgExecutor<'_>,
    p: &Pengguna,
    lapak_id: Uuid,
    profil: bool,
) -> AppResult<()> {
    p.wajib(PERAN_LAPAK)?;
    let ok: bool = match p.peran {
        Peran::Admin => sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM lapak WHERE id = $1)").bind(lapak_id).fetch_one(db).await?,
        Peran::Produsen => {
            sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM lapak l JOIN produsen d ON d.id = l.produsen_id WHERE l.id = $1 AND d.pengguna_id = $2)",
            )
            .bind(lapak_id)
            .bind(p.id)
            .fetch_one(db)
            .await?
        }
        _ if profil => return Err(AppError::Terlarang("Operator tidak dapat mengubah profil lapak.".into())),
        _ => {
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM lapak_operator WHERE lapak_id = $1 AND pengguna_id = $2)")
                .bind(lapak_id)
                .bind(p.id)
                .fetch_one(db)
                .await?
        }
    };
    if ok {
        Ok(())
    } else {
        Err(nf_lapak())
    }
}

/// Skor fulfillment (konfirmasi tepat waktu dalam 90 hari), 100 bila belum ada pesanan.
pub async fn skor_fulfillment(db: &PgPool, lapak_id: Uuid) -> AppResult<i32> {
    crate::pesanan::skor_fulfillment(db, lapak_id).await
}

// ---------------------------------------------------------------- lapak

#[derive(Deserialize, Validate)]
pub struct LapakBuat {
    #[validate(length(min = 3, max = 80, message = "nama produsen 3-80 karakter"))]
    pub nama_produsen: String,
    pub jenis: JenisProdusen,
    #[validate(custom(function = "valid_e164"))]
    pub kontak_wa: String,
    #[validate(length(min = 3, max = 60, message = "nama lapak 3-60 karakter"))]
    pub nama_lapak: String,
    #[validate(custom(function = "valid_slug"))]
    pub slug: String,
    #[validate(length(min = 50, max = 500, message = "deskripsi 50-500 karakter"))]
    pub deskripsi: String,
    #[validate(length(min = 1, max = 60, message = "titik serah Lumbung wajib diisi"))]
    pub titik_serah_lumbung_id: String,
    #[validate(length(min = 6, max = 40, message = "rekening payout 6-40 karakter"))]
    pub rekening_payout: String,
}

fn valid_e164(s: &str) -> Result<(), ValidationError> {
    let b = s.as_bytes();
    let ok = (9..=16).contains(&b.len())
        && b[0] == b'+'
        && b[1] != b'0'
        && b[1..].iter().all(u8::is_ascii_digit);
    ok.then_some(()).ok_or_else(|| {
        ValidationError::new("e164").with_message("kontak WA harus format E.164 (+62...)".into())
    })
}

fn valid_slug(s: &str) -> Result<(), ValidationError> {
    let ok = (3..=60).contains(&s.len())
        && s.bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-');
    ok.then_some(()).ok_or_else(|| {
        ValidationError::new("slug").with_message("slug harus ^[a-z0-9-]{3,60}$".into())
    })
}

fn valid_harga(h: i32) -> Result<(), ValidationError> {
    cek::harga_valid(h).then_some(()).ok_or_else(|| {
        ValidationError::new("harga").with_message("harga harus > 0 dan kelipatan Rp500".into())
    })
}

fn valid_sku(s: &str) -> Result<(), ValidationError> {
    let b = s.as_bytes();
    let ok = b.len() == 13
        && s.starts_with("LPK-")
        && b[8] == b'-'
        && b[4..8]
            .iter()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
        && b[9..].iter().all(u8::is_ascii_digit);
    ok.then_some(()).ok_or_else(|| {
        ValidationError::new("sku").with_message("SKU harus format LPK-XXXX-####".into())
    })
}

fn valid_https(s: &str) -> Result<(), ValidationError> {
    s.starts_with("https://")
        .then_some(())
        .ok_or_else(|| ValidationError::new("url").with_message("URL harus https".into()))
}

fn sensor_rekening(r: &str) -> String {
    let digit: String = r.chars().filter(char::is_ascii_digit).collect();
    let akhir: String = digit
        .chars()
        .rev()
        .take(4)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!("****{akhir}")
}

#[derive(Serialize, sqlx::FromRow)]
struct LapakSayaRow {
    #[serde(serialize_with = "crate::model::ser_uuid")]
    id: Uuid,
    nama_lapak: String,
    slug: String,
    status: String,
}

async fn lapak_json(db: &PgPool, r: &LapakSayaRow) -> AppResult<Value> {
    let skor = skor_fulfillment(db, r.id).await?;
    let saldo = crate::keuangan::saldo_tertahan(db, r.id).await?;
    Ok(json!({
        "id": ids::ke_ulid(r.id), "nama_lapak": r.nama_lapak, "slug": r.slug, "status": r.status,
        "skor_fulfillment_persen": skor, "saldo_tertahan_idr": saldo,
    }))
}

async fn buat_lapak(
    State(s): State<AppState>,
    p: Pengguna,
    ApiJson(req): ApiJson<LapakBuat>,
) -> AppResult<(StatusCode, Json<Value>)> {
    p.wajib(&[Peran::Produsen])?;
    let mut tx = s.db.begin().await?;
    let produsen_id: Uuid = match sqlx::query_scalar::<_, Uuid>(
        "SELECT id FROM produsen WHERE pengguna_id = $1",
    )
    .bind(p.id)
    .fetch_optional(&mut *tx)
    .await?
    {
        Some(id) => id,
        None => {
            let id = ids::baru();
            sqlx::query("INSERT INTO produsen (id, pengguna_id, nama, jenis, kontak_wa) VALUES ($1,$2,$3,$4,$5)")
                .bind(id)
                .bind(p.id)
                .bind(req.nama_produsen.trim())
                .bind(req.jenis.sebagai_str())
                .bind(&req.kontak_wa)
                .execute(&mut *tx)
                .await?;
            id
        }
    };
    let id = ids::baru();
    sqlx::query(
        "INSERT INTO lapak (id, produsen_id, nama_lapak, slug, deskripsi, titik_serah_lumbung_id, rekening_payout_tersensor) VALUES ($1,$2,$3,$4,$5,$6,$7)",
    )
    .bind(id)
    .bind(produsen_id)
    .bind(req.nama_lapak.trim())
    .bind(&req.slug)
    .bind(req.deskripsi.trim())
    .bind(&req.titik_serah_lumbung_id)
    .bind(sensor_rekening(&req.rekening_payout))
    .execute(&mut *tx)
    .await
    .map_err(|e| {
        if let sqlx::Error::Database(d) = &e {
            if d.is_unique_violation() {
                return match d.constraint() {
                    Some("lapak_produsen_id_key") => AppError::konflik("LAPAK_SUDAH_ADA", "Satu produsen hanya boleh punya satu lapak aktif."),
                    Some("lapak_slug_key") => AppError::konflik("SLUG_SUDAH_DIPAKAI", "Slug lapak sudah dipakai."),
                    _ => AppError::konflik("NAMA_LAPAK_SUDAH_DIPAKAI", "Nama lapak sudah dipakai."),
                };
            }
        }
        AppError::from(e)
    })?;
    tx.commit().await?;
    let row: LapakSayaRow =
        sqlx::query_as("SELECT id, nama_lapak, slug, status FROM lapak WHERE id = $1")
            .bind(id)
            .fetch_one(&s.db)
            .await?;
    Ok((StatusCode::CREATED, Json(lapak_json(&s.db, &row).await?)))
}

async fn lapak_saya(State(s): State<AppState>, p: Pengguna) -> AppResult<Json<Value>> {
    p.wajib(PERAN_LAPAK)?;
    let rows: Vec<LapakSayaRow> = match p.peran {
        Peran::Admin => sqlx::query_as("SELECT id, nama_lapak, slug, status FROM lapak ORDER BY id").fetch_all(&s.db).await?,
        Peran::Produsen => {
            sqlx::query_as(
                "SELECT l.id, l.nama_lapak, l.slug, l.status FROM lapak l JOIN produsen d ON d.id = l.produsen_id WHERE d.pengguna_id = $1 ORDER BY l.id",
            )
            .bind(p.id)
            .fetch_all(&s.db)
            .await?
        }
        _ => {
            sqlx::query_as(
                "SELECT l.id, l.nama_lapak, l.slug, l.status FROM lapak l JOIN lapak_operator o ON o.lapak_id = l.id WHERE o.pengguna_id = $1 ORDER BY l.id",
            )
            .bind(p.id)
            .fetch_all(&s.db)
            .await?
        }
    };
    let mut data = Vec::new();
    for r in &rows {
        data.push(lapak_json(&s.db, r).await?);
    }
    Ok(Json(json!({ "data": data })))
}

#[derive(Deserialize, Validate)]
#[serde(deny_unknown_fields)]
pub struct LapakUbah {
    #[validate(length(min = 3, max = 60, message = "nama lapak 3-60 karakter"))]
    pub nama_lapak: Option<String>,
    #[validate(length(min = 50, max = 500, message = "deskripsi 50-500 karakter"))]
    pub deskripsi: Option<String>,
    #[validate(length(min = 1, max = 60, message = "titik serah Lumbung wajib diisi"))]
    pub titik_serah_lumbung_id: Option<String>,
}

async fn ubah_lapak(
    State(s): State<AppState>,
    p: Pengguna,
    Path(id): Path<String>,
    ApiJson(req): ApiJson<LapakUbah>,
) -> AppResult<Json<Value>> {
    let lid = parse_id(&id, nf_lapak)?;
    lapak_boleh(&s.db, &p, lid, true).await?;
    sqlx::query(
        "UPDATE lapak SET nama_lapak = COALESCE($2, nama_lapak), deskripsi = COALESCE($3, deskripsi), \
         titik_serah_lumbung_id = COALESCE($4, titik_serah_lumbung_id) WHERE id = $1",
    )
    .bind(lid)
    .bind(req.nama_lapak.as_deref().map(str::trim))
    .bind(req.deskripsi.as_deref().map(str::trim))
    .bind(req.titik_serah_lumbung_id.as_deref())
    .execute(&s.db)
    .await
    .map_err(|e| match AppError::from(e) {
        AppError::Konflik { .. } => AppError::konflik("NAMA_LAPAK_SUDAH_DIPAKAI", "Nama lapak sudah dipakai."),
        l => l,
    })?;
    let row: LapakSayaRow =
        sqlx::query_as("SELECT id, nama_lapak, slug, status FROM lapak WHERE id = $1")
            .bind(lid)
            .fetch_one(&s.db)
            .await?;
    Ok(Json(lapak_json(&s.db, &row).await?))
}

#[derive(Deserialize, Validate)]
pub struct OperatorBuat {
    #[validate(email(message = "format email tidak valid"))]
    pub email: String,
}

async fn tambah_operator(
    State(s): State<AppState>,
    p: Pengguna,
    Path(id): Path<String>,
    ApiJson(req): ApiJson<OperatorBuat>,
) -> AppResult<(StatusCode, Json<Value>)> {
    let lid = parse_id(&id, nf_lapak)?;
    p.wajib(&[Peran::Produsen, Peran::Admin])?;
    lapak_boleh(&s.db, &p, lid, true).await?;
    let (uid, peran): (Uuid, String) =
        sqlx::query_as("SELECT id, peran FROM pengguna WHERE lower(email) = lower($1)")
            .bind(&req.email)
            .fetch_optional(&s.db)
            .await?
            .ok_or_else(|| {
                AppError::tidak_ditemukan("PENGGUNA_TIDAK_DITEMUKAN", "Pengguna tidak ditemukan.")
            })?;
    if peran != "OPERATOR" {
        return Err(AppError::validasi(
            "email",
            "Akun harus berperan OPERATOR (minta admin menetapkan peran).",
        ));
    }
    sqlx::query(
        "INSERT INTO lapak_operator (lapak_id, pengguna_id) VALUES ($1,$2) ON CONFLICT DO NOTHING",
    )
    .bind(lid)
    .bind(uid)
    .execute(&s.db)
    .await?;
    Ok((
        StatusCode::CREATED,
        Json(json!({"lapak_id": id, "pengguna_id": ids::ke_ulid(uid)})),
    ))
}

// ---------------------------------------------------------------- produk: input

#[derive(Deserialize, Validate)]
pub struct DrafProdukPayload {
    pub lapak_id: String,
    #[validate(length(min = 10, max = 80, message = "judul 10-80 karakter"))]
    pub judul: String,
    pub kategori_kode: KategoriKode,
    #[validate(length(max = 2, message = "tag maksimal 2"))]
    pub tag: Vec<Tag>,
    #[validate(length(min = 50, max = 2000, message = "deskripsi 50-2000 karakter"))]
    pub deskripsi: String,
    pub komposisi: Option<String>,
    pub alergen: Option<Vec<String>>,
}

#[derive(Deserialize, Validate)]
pub struct VarianInput {
    pub id: Option<String>,
    #[validate(length(min = 2, max = 40, message = "nama varian 2-40 karakter"))]
    pub nama_varian: String,
    #[validate(custom(function = "valid_sku"))]
    pub sku: String,
    #[validate(custom(function = "valid_harga"))]
    pub harga_idr: i32,
    #[validate(range(min = 0, message = "stok tidak boleh negatif"))]
    pub stok: i32,
    pub satuan: Satuan,
    #[validate(range(min = 1, message = "berat kirim harus > 0"))]
    pub berat_kirim_gram: i32,
    pub tanggal_produksi: Option<NaiveDate>,
    pub kedaluwarsa: Option<NaiveDate>,
}

#[derive(Deserialize, Validate)]
pub struct MediaInput {
    #[validate(custom(function = "valid_https"))]
    pub url: String,
    pub jenis: JenisMedia,
    pub utama: bool,
    #[validate(range(min = 1))]
    pub lebar: Option<i32>,
    #[validate(range(min = 1))]
    pub tinggi: Option<i32>,
    #[validate(range(min = 1))]
    pub ukuran_byte: Option<i64>,
}

#[derive(Deserialize, Validate)]
pub struct ProdukUbah {
    #[validate(length(min = 10, max = 80, message = "judul 10-80 karakter"))]
    pub judul: Option<String>,
    #[validate(length(min = 50, max = 2000, message = "deskripsi 50-2000 karakter"))]
    pub deskripsi: Option<String>,
    #[validate(length(max = 2, message = "tag maksimal 2"))]
    pub tag: Option<Vec<Tag>>,
    pub komposisi: Option<String>,
    pub alergen: Option<Vec<String>>,
    #[validate(nested)]
    pub varian: Option<Vec<VarianInput>>,
    #[validate(nested)]
    pub media: Option<Vec<MediaInput>>,
}

fn kesalahan_cek(list: Vec<crate::error::KesalahanKolom>) -> Value {
    json!({ "lolos": list.is_empty(), "kesalahan": list.iter().map(|e| json!({"kolom": e.kolom, "pesan": e.pesan})).collect::<Vec<_>>() })
}

async fn lapak_aktif(c: &mut PgConnection, lapak_id: Uuid) -> AppResult<()> {
    let st: String = sqlx::query_scalar("SELECT status FROM lapak WHERE id = $1")
        .bind(lapak_id)
        .fetch_one(c)
        .await?;
    if st == "AKTIF" {
        Ok(())
    } else {
        Err(AppError::konflik("LAPAK_TIDAK_AKTIF", "Lapak tidak aktif."))
    }
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

async fn buat_draf(
    State(s): State<AppState>,
    p: Pengguna,
    headers: HeaderMap,
    ApiJson(req): ApiJson<DrafProdukPayload>,
) -> AppResult<(StatusCode, Json<Value>)> {
    let lid = parse_id(&req.lapak_id, nf_lapak)?;
    lapak_boleh(&s.db, &p, lid, false).await?;
    let kunci = header_kunci(&headers)?;
    let mut tx = s.db.begin().await?;
    let cakupan = format!("produk-draf:{}", ids::ke_ulid(p.id));
    if let Some(k) = &kunci {
        if let Some((st, resp)) = idempotensi_awal(&mut tx, &cakupan, k).await? {
            return Ok((
                StatusCode::from_u16(st as u16).unwrap_or(StatusCode::CREATED),
                Json(resp),
            ));
        }
    }
    lapak_aktif(&mut tx, lid).await?;
    let lain = judul_lain(&mut tx, lid, None).await?;
    if let Some(mirip) = cek::judul_duplikat(&req.judul, &lain) {
        return Err(AppError::validasi(
            "judul",
            format!("Judul terlalu mirip (>= 85%) dengan produk lain di lapak: \"{mirip}\"."),
        ));
    }
    let id = ids::baru();
    sqlx::query("INSERT INTO produk (id, lapak_id, judul, kategori_kode, tag, deskripsi, komposisi, alergen) VALUES ($1,$2,$3,$4,$5,$6,$7,$8)")
        .bind(id)
        .bind(lid)
        .bind(req.judul.trim())
        .bind(req.kategori_kode.sebagai_str())
        .bind(req.tag.iter().map(|t| t.sebagai_str().to_string()).collect::<Vec<_>>())
        .bind(req.deskripsi.trim())
        .bind(req.komposisi.as_deref())
        .bind(req.alergen.as_deref())
        .execute(&mut *tx)
        .await?;
    let resp = json!({"produk_id": ids::ke_ulid(id), "status": "DRAF"});
    if let Some(k) = &kunci {
        idempotensi_simpan(&mut tx, &cakupan, k, p.id, 201, &resp).await?;
    }
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(resp)))
}

async fn muat_produk_akses(
    c: &mut PgConnection,
    p: &Pengguna,
    id: &str,
    profil: bool,
) -> AppResult<crate::model::ProdukRow> {
    let pid = parse_id(id, nf_produk)?;
    let pr = muat_produk(c, pid).await?;
    lapak_boleh(&mut *c, p, pr.lapak_id, profil)
        .await
        .map_err(|e| match e {
            AppError::TidakDitemukan { .. } => nf_produk(),
            l => l,
        })?;
    Ok(pr)
}

async fn detail_web(
    State(s): State<AppState>,
    p: Pengguna,
    Path(id): Path<String>,
) -> AppResult<Json<Value>> {
    let mut c = s.db.acquire().await?;
    let pr = muat_produk_akses(&mut c, &p, &id, false).await?;
    let varian = muat_varian(&mut c, pr.id).await?;
    let media = muat_media(&mut c, pr.id).await?;
    let (data, kat) = data_cek(&mut c, &pr).await?;
    let lain = judul_lain(&mut c, pr.lapak_id, Some(pr.id)).await?;
    Ok(Json(json!({
        "id": ids::ke_ulid(pr.id), "lapak_id": ids::ke_ulid(pr.lapak_id), "judul": pr.judul, "kategori_kode": pr.kategori_kode,
        "tag": pr.tag, "deskripsi": pr.deskripsi, "komposisi": pr.komposisi, "alergen": pr.alergen, "status": pr.status,
        "perlu_revisi_catatan": pr.perlu_revisi_catatan, "varian": varian, "media": media,
        "cek_otomatis": kesalahan_cek(cek::cek_otomatis(&data, &kat, &lain)),
    })))
}

const STATUS_EDIT: &[&str] = &["DRAF", "PERLU_REVISI", "TAYANG", "NONAKTIF_STOK_0"];

async fn ubah_produk(
    State(s): State<AppState>,
    p: Pengguna,
    Path(id): Path<String>,
    ApiJson(req): ApiJson<ProdukUbah>,
) -> AppResult<Json<Value>> {
    let mut tx = s.db.begin().await?;
    let pr = muat_produk_akses(&mut tx, &p, &id, false).await?;
    // Kunci baris produk selama transaksi.
    sqlx::query("SELECT 1 FROM produk WHERE id = $1 FOR UPDATE")
        .bind(pr.id)
        .execute(&mut *tx)
        .await?;
    if !STATUS_EDIT.contains(&pr.status.as_str()) {
        return Err(AppError::konflik(
            "STATUS_TIDAK_VALID",
            format!("Produk berstatus {} tidak dapat diubah.", pr.status),
        ));
    }
    let sudah_tayang = matches!(pr.status.as_str(), "TAYANG" | "NONAKTIF_STOK_0");

    if let Some(j) = &req.judul {
        let lain = judul_lain(&mut tx, pr.lapak_id, Some(pr.id)).await?;
        if let Some(mirip) = cek::judul_duplikat(j, &lain) {
            return Err(AppError::validasi(
                "judul",
                format!("Judul terlalu mirip (>= 85%) dengan produk lain di lapak: \"{mirip}\"."),
            ));
        }
    }
    sqlx::query(
        "UPDATE produk SET judul = COALESCE($2, judul), deskripsi = COALESCE($3, deskripsi), tag = COALESCE($4, tag), \
         komposisi = COALESCE($5, komposisi), alergen = COALESCE($6, alergen), diperbarui_pada = now() WHERE id = $1",
    )
    .bind(pr.id)
    .bind(req.judul.as_deref().map(str::trim))
    .bind(req.deskripsi.as_deref().map(str::trim))
    .bind(req.tag.as_ref().map(|t| t.iter().map(|x| x.sebagai_str().to_string()).collect::<Vec<_>>()))
    .bind(req.komposisi.as_deref())
    .bind(req.alergen.as_deref())
    .execute(&mut *tx)
    .await?;

    if let Some(varian) = &req.varian {
        for v in varian {
            match &v.id {
                Some(vid) => {
                    let vuid = parse_id(vid, nf_varian)?;
                    let (harga, stok): (i32, i32) = sqlx::query_as("SELECT harga_idr, stok FROM varian_produk WHERE id = $1 AND produk_id = $2 FOR UPDATE")
                        .bind(vuid)
                        .bind(pr.id)
                        .fetch_optional(&mut *tx)
                        .await?
                        .ok_or_else(nf_varian)?;
                    if sudah_tayang && (harga != v.harga_idr || stok != v.stok) {
                        return Err(AppError::validasi(
                            "varian",
                            "Harga dan stok produk tayang diubah lewat PATCH /web/varian/{id}/harga dan /stok.",
                        ));
                    }
                    sqlx::query(
                        "UPDATE varian_produk SET nama_varian=$2, sku=$3, harga_idr=$4, stok=$5, satuan=$6, berat_kirim_gram=$7, \
                         tanggal_produksi=$8, kedaluwarsa=$9, diperbarui_pada=now() WHERE id=$1",
                    )
                    .bind(vuid)
                    .bind(v.nama_varian.trim())
                    .bind(&v.sku)
                    .bind(v.harga_idr)
                    .bind(v.stok)
                    .bind(v.satuan.sebagai_str())
                    .bind(v.berat_kirim_gram)
                    .bind(v.tanggal_produksi)
                    .bind(v.kedaluwarsa)
                    .execute(&mut *tx)
                    .await
                    .map_err(sku_konflik)?;
                    if harga != v.harga_idr {
                        catat_harga(&mut tx, pr.id, vuid, Some(harga), v.harga_idr, p.id).await?;
                    }
                }
                None => {
                    let vuid = ids::baru();
                    sqlx::query(
                        "INSERT INTO varian_produk (id, produk_id, nama_varian, sku, harga_idr, stok, satuan, berat_kirim_gram, tanggal_produksi, kedaluwarsa) \
                         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)",
                    )
                    .bind(vuid)
                    .bind(pr.id)
                    .bind(v.nama_varian.trim())
                    .bind(&v.sku)
                    .bind(v.harga_idr)
                    .bind(v.stok)
                    .bind(v.satuan.sebagai_str())
                    .bind(v.berat_kirim_gram)
                    .bind(v.tanggal_produksi)
                    .bind(v.kedaluwarsa)
                    .execute(&mut *tx)
                    .await
                    .map_err(sku_konflik)?;
                    catat_harga(&mut tx, pr.id, vuid, None, v.harga_idr, p.id).await?;
                }
            }
        }
    }

    let mut kurasi_ulang = false;
    if let Some(media) = &req.media {
        if media.len() > 7 {
            return Err(AppError::validasi("media", "Media maksimal 7."));
        }
        if media.iter().filter(|m| m.utama).count() > 1 {
            return Err(AppError::validasi("media", "Tepat satu media utama."));
        }
        let lama: Option<String> =
            sqlx::query_scalar("SELECT url FROM media_produk WHERE produk_id = $1 AND utama")
                .bind(pr.id)
                .fetch_optional(&mut *tx)
                .await?;
        sqlx::query("DELETE FROM media_produk WHERE produk_id = $1")
            .bind(pr.id)
            .execute(&mut *tx)
            .await?;
        for (i, m) in media.iter().enumerate() {
            sqlx::query("INSERT INTO media_produk (id, produk_id, url, jenis, utama, lebar, tinggi, ukuran_byte, urutan) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9)")
                .bind(ids::baru())
                .bind(pr.id)
                .bind(&m.url)
                .bind(m.jenis.sebagai_str())
                .bind(m.utama)
                .bind(m.lebar)
                .bind(m.tinggi)
                .bind(m.ukuran_byte)
                .bind(i as i32)
                .execute(&mut *tx)
                .await?;
        }
        let baru = media.iter().find(|m| m.utama).map(|m| m.url.clone());
        kurasi_ulang = sudah_tayang && lama != baru;
    }
    if kurasi_ulang {
        // Ganti foto utama pada produk tayang = kurasi ulang ringan.
        sqlx::query(
            "UPDATE produk SET status = 'MENUNGGU_KURASI', diajukan_pada = now() WHERE id = $1",
        )
        .bind(pr.id)
        .execute(&mut *tx)
        .await?;
    }

    let pr2 = muat_produk(&mut tx, pr.id).await?;
    let (data, kat) = data_cek(&mut tx, &pr2).await?;
    let lain = judul_lain(&mut tx, pr2.lapak_id, Some(pr2.id)).await?;
    let hasil = cek::cek_otomatis(&data, &kat, &lain);
    tx.commit().await?;
    Ok(Json(
        json!({"produk_id": id, "status": pr2.status, "cek_otomatis": kesalahan_cek(hasil)}),
    ))
}

fn sku_konflik(e: sqlx::Error) -> AppError {
    match AppError::from(e) {
        AppError::Konflik { .. } => {
            AppError::konflik("SKU_SUDAH_DIPAKAI", "SKU sudah dipakai varian lain.")
        }
        l => l,
    }
}

async fn catat_harga(
    c: &mut PgConnection,
    produk_id: Uuid,
    varian_id: Uuid,
    lama: Option<i32>,
    baru: i32,
    oleh: Uuid,
) -> AppResult<()> {
    sqlx::query("INSERT INTO harga_riwayat (id, produk_id, varian_id, harga_lama, harga_baru, diubah_oleh) VALUES ($1,$2,$3,$4,$5,$6)")
        .bind(ids::baru())
        .bind(produk_id)
        .bind(varian_id)
        .bind(lama)
        .bind(baru)
        .bind(oleh)
        .execute(c)
        .await?;
    Ok(())
}

async fn ajukan_kurasi(
    State(s): State<AppState>,
    p: Pengguna,
    Path(id): Path<String>,
) -> AppResult<Json<Value>> {
    let mut tx = s.db.begin().await?;
    let pr = muat_produk_akses(&mut tx, &p, &id, false).await?;
    sqlx::query("SELECT 1 FROM produk WHERE id = $1 FOR UPDATE")
        .bind(pr.id)
        .execute(&mut *tx)
        .await?;
    let pr = muat_produk(&mut tx, pr.id).await?;
    if !matches!(pr.status.as_str(), "DRAF" | "PERLU_REVISI") {
        return Err(AppError::konflik(
            "STATUS_TIDAK_VALID",
            format!("Produk berstatus {} tidak dapat diajukan.", pr.status),
        ));
    }
    lapak_aktif(&mut tx, pr.lapak_id).await?;
    let (data, kat) = data_cek(&mut tx, &pr).await?;
    let lain = judul_lain(&mut tx, pr.lapak_id, Some(pr.id)).await?;
    let galat = cek::cek_otomatis(&data, &kat, &lain);
    if !galat.is_empty() {
        return Err(AppError::Validasi(galat));
    }
    sqlx::query("UPDATE produk SET status = 'MENUNGGU_KURASI', diajukan_pada = now(), diperbarui_pada = now() WHERE id = $1").bind(pr.id).execute(&mut *tx).await?;
    tx.commit().await?;
    let sla = cek::tambah_hari_kerja(Utc::now(), 2);
    Ok(Json(
        json!({"status": "MENUNGGU_KURASI", "estimasi_sla": sla}),
    ))
}

async fn arsipkan(
    State(s): State<AppState>,
    p: Pengguna,
    Path(id): Path<String>,
) -> AppResult<Json<Value>> {
    let mut tx = s.db.begin().await?;
    let pr = muat_produk_akses(&mut tx, &p, &id, false).await?;
    if pr.status == "ARSIP" {
        return Ok(Json(json!({"status": "ARSIP"})));
    }
    sqlx::query("UPDATE produk SET status = 'ARSIP', diperbarui_pada = now() WHERE id = $1")
        .bind(pr.id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(Json(json!({"status": "ARSIP"})))
}

#[derive(Deserialize)]
pub struct ProdukWebQuery {
    pub status: Option<String>,
    pub lapak_id: Option<String>,
    pub limit: Option<i64>,
    pub cursor: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct KursorId {
    id: String,
}

async fn daftar_web(
    State(s): State<AppState>,
    p: Pengguna,
    Query(q): Query<ProdukWebQuery>,
) -> AppResult<Json<Value>> {
    p.wajib(PERAN_LAPAK)?;
    let limit = Halaman {
        limit: q.limit,
        cursor: None,
    }
    .limit()?;
    if let Some(st) = &q.status {
        ProdukStatus::dari_str(st)
            .ok_or_else(|| AppError::Permintaan("status tidak dikenal.".into()))?;
    }
    let lapak_id = q
        .lapak_id
        .as_deref()
        .map(|l| parse_id(l, nf_lapak))
        .transpose()?;
    if let Some(l) = lapak_id {
        lapak_boleh(&s.db, &p, l, false).await?;
    }
    let kursor = match q.cursor.as_deref() {
        Some(c) => Some(
            ids::dari_ulid(&decode_cursor::<KursorId>(c)?.id, "")
                .ok_or_else(|| AppError::Permintaan("cursor tidak valid.".into()))?,
        ),
        None => None,
    };
    let rows: Vec<(Uuid, String, String, Option<String>)> = sqlx::query_as(
        "SELECT p.id, p.judul, p.status, p.perlu_revisi_catatan FROM produk p \
         WHERE ($1::text IS NULL OR p.status = $1) AND ($2::uuid IS NULL OR p.lapak_id = $2) AND ($3::uuid IS NULL OR p.id < $3) \
           AND ($4 OR p.lapak_id IN ( \
               SELECT l.id FROM lapak l JOIN produsen d ON d.id = l.produsen_id WHERE d.pengguna_id = $5 \
               UNION SELECT o.lapak_id FROM lapak_operator o WHERE o.pengguna_id = $5)) \
         ORDER BY p.id DESC LIMIT $6",
    )
    .bind(q.status.as_deref())
    .bind(lapak_id)
    .bind(kursor)
    .bind(p.peran == Peran::Admin)
    .bind(p.id)
    .bind(limit + 1)
    .fetch_all(&s.db)
    .await?;
    let (rows, lanjut) = potong(rows, limit);
    let cursor_lanjut = if lanjut {
        rows.last().map(|r| {
            encode_cursor(&KursorId {
                id: ids::ke_ulid(r.0),
            })
        })
    } else {
        None
    };
    Ok(Json(json!({
        "data": rows.iter().map(|r| json!({"id": ids::ke_ulid(r.0), "judul": r.1, "status": r.2, "perlu_revisi_catatan": r.3})).collect::<Vec<_>>(),
        "cursor_lanjut": cursor_lanjut,
    })))
}

// ---------------------------------------------------------------- varian: stok & harga

#[derive(Deserialize, Validate)]
pub struct StokUbah {
    #[validate(range(min = 0, message = "stok tidak boleh negatif"))]
    pub stok: i32,
    #[validate(length(min = 1, max = 200, message = "alasan wajib diisi"))]
    pub alasan: String,
}

#[derive(Deserialize, Validate)]
pub struct HargaUbah {
    #[validate(custom(function = "valid_harga"))]
    pub harga_idr: i32,
}

async fn varian_akses(c: &mut PgConnection, p: &Pengguna, id: &str) -> AppResult<(Uuid, Uuid)> {
    let vid = parse_id(id, nf_varian)?;
    let produk_id: Uuid =
        sqlx::query_scalar("SELECT produk_id FROM varian_produk WHERE id = $1 FOR UPDATE")
            .bind(vid)
            .fetch_optional(&mut *c)
            .await?
            .ok_or_else(nf_varian)?;
    let lapak_id: Uuid = sqlx::query_scalar("SELECT lapak_id FROM produk WHERE id = $1")
        .bind(produk_id)
        .fetch_one(&mut *c)
        .await?;
    lapak_boleh(&mut *c, p, lapak_id, false)
        .await
        .map_err(|e| match e {
            AppError::TidakDitemukan { .. } => nf_varian(),
            l => l,
        })?;
    Ok((vid, produk_id))
}

async fn ubah_stok(
    State(s): State<AppState>,
    p: Pengguna,
    Path(id): Path<String>,
    ApiJson(req): ApiJson<StokUbah>,
) -> AppResult<Json<Value>> {
    let mut tx = s.db.begin().await?;
    let (vid, produk_id) = varian_akses(&mut tx, &p, &id).await?;
    sqlx::query("UPDATE varian_produk SET stok = $2, diperbarui_pada = now() WHERE id = $1")
        .bind(vid)
        .bind(req.stok)
        .execute(&mut *tx)
        .await?;
    let status = sinkron_status_stok(&mut tx, produk_id).await?;
    tx.commit().await?;
    Ok(Json(json!({"stok": req.stok, "status_produk": status})))
}

async fn ubah_harga(
    State(s): State<AppState>,
    p: Pengguna,
    Path(id): Path<String>,
    ApiJson(req): ApiJson<HargaUbah>,
) -> AppResult<Json<Value>> {
    let mut tx = s.db.begin().await?;
    let (vid, produk_id) = varian_akses(&mut tx, &p, &id).await?;
    let lama: i32 = sqlx::query_scalar("SELECT harga_idr FROM varian_produk WHERE id = $1")
        .bind(vid)
        .fetch_one(&mut *tx)
        .await?;
    let mut kurasi_ulang = false;
    if lama != req.harga_idr {
        sqlx::query(
            "UPDATE varian_produk SET harga_idr = $2, diperbarui_pada = now() WHERE id = $1",
        )
        .bind(vid)
        .bind(req.harga_idr)
        .execute(&mut *tx)
        .await?;
        catat_harga(&mut tx, produk_id, vid, Some(lama), req.harga_idr, p.id).await?;
        // Perubahan > 20% (naik atau turun) pada produk tayang memicu kurasi ulang ringan.
        let selisih = (req.harga_idr as i64 - lama as i64).abs() * 100;
        if selisih > 20 * lama as i64 {
            let n = sqlx::query(
                "UPDATE produk SET status = 'MENUNGGU_KURASI', diajukan_pada = now(), diperbarui_pada = now() \
                 WHERE id = $1 AND status IN ('TAYANG','NONAKTIF_STOK_0')",
            )
            .bind(produk_id)
            .execute(&mut *tx)
            .await?
            .rows_affected();
            kurasi_ulang = n > 0;
        }
    }
    tx.commit().await?;
    Ok(Json(
        json!({"harga_idr": req.harga_idr, "kurasi_ulang_ringan": kurasi_ulang}),
    ))
}

async fn riwayat_harga(
    State(s): State<AppState>,
    p: Pengguna,
    Path(id): Path<String>,
) -> AppResult<Json<Value>> {
    let mut tx = s.db.begin().await?;
    let (vid, _) = varian_akses(&mut tx, &p, &id).await?;
    let rows: Vec<(Option<i32>, i32, Uuid, chrono::DateTime<Utc>)> =
        sqlx::query_as("SELECT harga_lama, harga_baru, diubah_oleh, diubah_pada FROM harga_riwayat WHERE varian_id = $1 ORDER BY diubah_pada, id")
            .bind(vid)
            .fetch_all(&mut *tx)
            .await?;
    Ok(Json(
        json!({"data": rows.iter().map(|r| json!({"harga_lama_idr": r.0, "harga_baru_idr": r.1, "diubah_oleh": ids::ke_ulid(r.2), "diubah_pada": r.3})).collect::<Vec<_>>()}),
    ))
}

// ---------------------------------------------------------------- media presigned

#[derive(Deserialize, Validate)]
pub struct PresignedReq {
    #[validate(length(min = 1, max = 120, message = "nama berkas 1-120 karakter"))]
    pub nama_berkas: String,
    pub tipe_mime: String,
    #[validate(range(min = 1, max = 52428800, message = "ukuran 1 byte - 50 MB"))]
    pub ukuran_byte: i64,
}

async fn presigned(
    State(s): State<AppState>,
    p: Pengguna,
    ApiJson(req): ApiJson<PresignedReq>,
) -> AppResult<Json<Value>> {
    p.wajib(PERAN_LAPAK)?;
    let (jenis, maks) = match req.tipe_mime.as_str() {
        "image/jpeg" | "image/png" | "image/webp" => ("foto", cek::FOTO_MAKS_BYTE),
        "video/mp4" => ("video", cek::VIDEO_MAKS_BYTE),
        _ => {
            return Err(AppError::validasi(
                "tipe_mime",
                "Tipe harus image/jpeg, image/png, image/webp, atau video/mp4.",
            ))
        }
    };
    if req.ukuran_byte > maks {
        return Err(AppError::validasi(
            "ukuran_byte",
            if jenis == "foto" {
                "Foto maksimal 5 MB."
            } else {
                "Video maksimal 50 MB."
            },
        ));
    }
    let aman: String = req
        .nama_berkas
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let jalur = format!("{}/{}/{}", jenis, ids::ke_ulid(ids::baru()), aman);
    let kedaluwarsa = Utc::now() + chrono::Duration::minutes(15);
    let tanda =
        crate::auth::tanda_tangan(&s.config, &format!("{jalur}|{}", kedaluwarsa.timestamp()));
    let dasar = s.config.media_base_url.trim_end_matches('/');
    Ok(Json(json!({
        "unggah_url": format!("{dasar}/unggah/{jalur}?exp={}&tanda={tanda}", kedaluwarsa.timestamp()),
        "media_url": format!("{dasar}/{jalur}"),
        "kedaluwarsa": kedaluwarsa,
    })))
}

/// Arsipkan draf PERLU_REVISI yang tidak diajukan ulang dalam 7 hari (SOP Tayang §2).
pub async fn arsipkan_revisi_kedaluwarsa(db: &PgPool) -> AppResult<u64> {
    Ok(sqlx::query(
        "UPDATE produk SET status = 'ARSIP', diperbarui_pada = now() \
         WHERE status = 'PERLU_REVISI' AND perlu_revisi_pada < now() - interval '7 days'",
    )
    .execute(db)
    .await?
    .rows_affected())
}

pub fn rute() -> Router<AppState> {
    Router::new()
        .route("/web/lapak", post(buat_lapak))
        .route("/web/lapak-saya", get(lapak_saya))
        .route("/web/lapak/{id}", patch(ubah_lapak))
        .route("/web/lapak/{id}/operator", post(tambah_operator))
        .route("/web/produk", get(daftar_web))
        .route("/web/produk/draf", post(buat_draf))
        .route("/web/produk/{id}", get(detail_web).put(ubah_produk))
        .route("/web/produk/{id}/ajukan-kurasi", post(ajukan_kurasi))
        .route("/web/produk/{id}/arsipkan", post(arsipkan))
        .route("/web/media/presigned", post(presigned))
        .route("/web/varian/{id}/stok", patch(ubah_stok))
        .route("/web/varian/{id}/harga", patch(ubah_harga))
        .route("/web/varian/{id}/harga-riwayat", get(riwayat_harga))
}
