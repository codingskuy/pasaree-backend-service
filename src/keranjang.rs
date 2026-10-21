//! Keranjang, pratinjau checkout, dan pembuatan pesanan (status awal DIPESAN, stok dikunci 30 menit).

use std::collections::{BTreeMap, HashMap};

use axum::{
    extract::State,
    http::StatusCode,
    routing::{post, put},
    Json, Router,
};
use chrono::{DateTime, Duration, Utc};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;
use validator::Validate;

use crate::{
    auth::{Pengguna, Peran},
    error::{AppError, AppResult},
    ids,
    json::ApiJson,
    model::sinkron_status_stok,
    pesanan::{catat_timeline, AWALAN_PESANAN},
    state::AppState,
    trip,
};

pub const BIAYA_LAYANAN_IDR: i64 = 2000;
pub const BATAS_BAYAR_MENIT: i64 = 30;

#[derive(Deserialize, Validate)]
pub struct KeranjangUbah {
    pub varian_id: String,
    #[validate(range(min = 0, max = 99, message = "qty 0-99"))]
    pub qty: i32,
}

#[derive(sqlx::FromRow)]
struct BarisKeranjang {
    varian_id: Uuid,
    qty: i32,
    harga_snapshot: i32,
    harga_idr: i32,
    stok: i32,
    nama_varian: String,
    sku: String,
    produk_id: Uuid,
    judul: String,
    kategori_kode: String,
    produk_status: String,
    lapak_id: Uuid,
    lapak_status: String,
}

impl BarisKeranjang {
    fn tersedia(&self) -> bool {
        self.produk_status == "TAYANG" && self.lapak_status == "AKTIF"
    }
}

const SELEKSI_KERANJANG: &str = "SELECT k.varian_id, k.qty, k.harga_snapshot, v.harga_idr, v.stok, v.nama_varian, v.sku, \
    p.id AS produk_id, p.judul, p.kategori_kode, p.status AS produk_status, l.id AS lapak_id, l.status AS lapak_status \
    FROM keranjang_item k JOIN varian_produk v ON v.id = k.varian_id JOIN produk p ON p.id = v.produk_id \
    JOIN lapak l ON l.id = p.lapak_id WHERE k.pengguna_id = $1 ORDER BY v.id";

async fn keranjang_terhitung(db: &PgPool, pengguna: Uuid) -> AppResult<Value> {
    let baris: Vec<BarisKeranjang> = sqlx::query_as(SELEKSI_KERANJANG)
        .bind(pengguna)
        .fetch_all(db)
        .await?;
    let mut item = Vec::new();
    let mut peringatan = Vec::new();
    let mut subtotal: i64 = 0;
    for b in &baris {
        if !b.tersedia() {
            peringatan.push(format!("{} ({}) tidak tersedia lagi.", b.judul, b.sku));
            continue;
        }
        if b.stok < b.qty {
            peringatan.push(format!("Stok {} tinggal {}.", b.sku, b.stok));
        }
        if b.harga_idr != b.harga_snapshot {
            peringatan.push(format!(
                "Harga {} berubah dari Rp{} menjadi Rp{}.",
                b.sku, b.harga_snapshot, b.harga_idr
            ));
        }
        subtotal += b.harga_idr as i64 * b.qty as i64;
        item.push(
            json!({"varian_id": ids::ke_ulid(b.varian_id), "qty": b.qty, "harga_idr": b.harga_idr}),
        );
    }
    Ok(json!({"item": item, "subtotal_idr": subtotal, "peringatan_stok": peringatan}))
}

fn nf_varian() -> AppError {
    AppError::tidak_ditemukan("VARIAN_TIDAK_DITEMUKAN", "Varian tidak ditemukan.")
}

async fn ubah_keranjang(
    State(s): State<AppState>,
    p: Pengguna,
    ApiJson(req): ApiJson<KeranjangUbah>,
) -> AppResult<Json<Value>> {
    p.wajib(&[Peran::Pembeli])?;
    let vid = ids::dari_ulid(&req.varian_id, "").ok_or_else(nf_varian)?;
    if req.qty == 0 {
        sqlx::query("DELETE FROM keranjang_item WHERE pengguna_id = $1 AND varian_id = $2")
            .bind(p.id)
            .bind(vid)
            .execute(&s.db)
            .await?;
        return Ok(Json(keranjang_terhitung(&s.db, p.id).await?));
    }
    let v: Option<(i32, i32, String, String)> = sqlx::query_as(
        "SELECT v.harga_idr, v.stok, pr.status, l.status FROM varian_produk v JOIN produk pr ON pr.id = v.produk_id \
         JOIN lapak l ON l.id = pr.lapak_id WHERE v.id = $1",
    )
    .bind(vid)
    .fetch_optional(&s.db)
    .await?;
    let (harga, stok, pstatus, lstatus) = v.ok_or_else(nf_varian)?;
    if !matches!(pstatus.as_str(), "TAYANG" | "NONAKTIF_STOK_0") || lstatus != "AKTIF" {
        return Err(nf_varian());
    }
    if stok < req.qty {
        return Err(stok_habis(vid, stok, req.qty));
    }
    sqlx::query(
        "INSERT INTO keranjang_item (pengguna_id, varian_id, qty, harga_snapshot) VALUES ($1,$2,$3,$4) \
         ON CONFLICT (pengguna_id, varian_id) DO UPDATE SET qty = EXCLUDED.qty, harga_snapshot = EXCLUDED.harga_snapshot, diperbarui_pada = now()",
    )
    .bind(p.id)
    .bind(vid)
    .bind(req.qty)
    .bind(harga)
    .execute(&s.db)
    .await?;
    Ok(Json(keranjang_terhitung(&s.db, p.id).await?))
}

async fn lihat_keranjang(State(s): State<AppState>, p: Pengguna) -> AppResult<Json<Value>> {
    p.wajib(&[Peran::Pembeli])?;
    Ok(Json(keranjang_terhitung(&s.db, p.id).await?))
}

fn stok_habis(varian: Uuid, tersedia: i32, diminta: i32) -> AppError {
    AppError::Konflik {
        kode: "STOK_HABIS",
        pesan: "Stok varian berubah.".into(),
        detail: Some(
            json!({"varian_id": ids::ke_ulid(varian), "stok_tersedia": tersedia, "diminta": diminta}),
        ),
    }
}

#[derive(Deserialize, Validate)]
pub struct CheckoutRequest {
    #[validate(length(min = 1, max = 60, message = "trip_id wajib diisi"))]
    pub trip_id: String,
    #[validate(length(min = 1, max = 60, message = "titik_id wajib diisi"))]
    pub titik_id: String,
    pub voucher: Option<String>,
}

#[derive(Deserialize, Validate)]
pub struct PesananBuat {
    #[validate(length(min = 1, max = 60, message = "trip_id wajib diisi"))]
    pub trip_id: String,
    #[validate(length(min = 1, max = 60, message = "titik_id wajib diisi"))]
    pub titik_id: String,
    pub voucher: Option<String>,
    pub catatan_per_lapak: Option<HashMap<String, String>>,
}

struct Siap {
    baris: Vec<BarisKeranjang>,
    subtotal: i64,
    ongkir: i64,
    peringatan: Vec<String>,
}

enum Hasil {
    Siap(Siap),
    HargaBerubah(Vec<Value>),
}

/// Validasi keranjang + trip (dipakai pratinjau dan pembuatan pesanan). `kunci` = kunci baris varian (FOR UPDATE).
async fn siapkan(
    s: &AppState,
    c: &mut PgConnection,
    pembeli: Uuid,
    trip_id: &str,
    titik_id: &str,
    voucher: &Option<String>,
    kunci: bool,
) -> AppResult<Hasil> {
    let sql = if kunci {
        format!("{SELEKSI_KERANJANG} FOR UPDATE OF v")
    } else {
        SELEKSI_KERANJANG.to_string()
    };
    let baris: Vec<BarisKeranjang> = sqlx::query_as(&sql)
        .bind(pembeli)
        .fetch_all(&mut *c)
        .await?;
    if baris.is_empty() {
        return Err(AppError::validasi("keranjang", "Keranjang kosong."));
    }
    let mut berubah = Vec::new();
    for b in &baris {
        if !b.tersedia() {
            return Err(AppError::Konflik {
                kode: "PRODUK_TIDAK_TERSEDIA",
                pesan: format!("{} tidak tersedia lagi.", b.judul),
                detail: Some(json!({"varian_id": ids::ke_ulid(b.varian_id)})),
            });
        }
        if b.stok < b.qty {
            return Err(stok_habis(b.varian_id, b.stok, b.qty));
        }
        if b.harga_idr != b.harga_snapshot {
            berubah.push(json!({"varian_id": ids::ke_ulid(b.varian_id), "harga_lama_idr": b.harga_snapshot, "harga_baru_idr": b.harga_idr}));
        }
    }
    if !berubah.is_empty() {
        // Muktakhirkan snapshot supaya percobaan berikutnya (setelah klien menampilkan ulang) lolos.
        for b in &baris {
            sqlx::query("UPDATE keranjang_item SET harga_snapshot = $3 WHERE pengguna_id = $1 AND varian_id = $2")
                .bind(pembeli)
                .bind(b.varian_id)
                .bind(b.harga_idr)
                .execute(&mut *c)
                .await?;
        }
        return Ok(Hasil::HargaBerubah(berubah));
    }

    let (daftar, basi) = trip::ambil(s, None, None, Some(titik_id)).await;
    let t = daftar
        .iter()
        .find(|t| t.trip_id == trip_id)
        .ok_or_else(|| {
            AppError::validasi(
                "trip_id",
                "Trip tidak valid untuk titik ini (checkout tanpa trip valid diblokir).",
            )
        })?;
    if t.jadwal_berangkat <= Utc::now() {
        return Err(AppError::validasi("trip_id", "Trip sudah berangkat."));
    }
    if t.sisa_kapasitas == Some(0) {
        return Err(AppError::Konflik {
            kode: "TRIP_PENUH",
            pesan: "Trip penuh; pilih trip lain.".into(),
            detail: Some(json!({"trip_id": trip_id})),
        });
    }
    let subtotal = baris
        .iter()
        .map(|b| b.harga_idr as i64 * b.qty as i64)
        .sum();
    let mut peringatan = Vec::new();
    if basi {
        peringatan
            .push("Jadwal trip berasal dari cache karena Lumbung tidak merespons.".to_string());
    }
    if voucher.as_deref().is_some_and(|v| !v.trim().is_empty()) {
        peringatan.push("Voucher belum didukung dan diabaikan.".to_string());
    }
    Ok(Hasil::Siap(Siap {
        baris,
        subtotal,
        ongkir: t.ongkir_idr,
        peringatan,
    }))
}

fn konflik_harga(detail: Vec<Value>) -> AppError {
    AppError::Konflik {
        kode: "HARGA_BERUBAH",
        pesan: "Harga berubah; tampilkan ulang pratinjau.".into(),
        detail: Some(json!({"varian": detail})),
    }
}

async fn pratinjau(
    State(s): State<AppState>,
    p: Pengguna,
    ApiJson(req): ApiJson<CheckoutRequest>,
) -> AppResult<Json<Value>> {
    p.wajib(&[Peran::Pembeli])?;
    let mut c = s.db.acquire().await?;
    match siapkan(
        &s,
        &mut c,
        p.id,
        &req.trip_id,
        &req.titik_id,
        &req.voucher,
        false,
    )
    .await?
    {
        Hasil::HargaBerubah(d) => Err(konflik_harga(d)),
        Hasil::Siap(h) => {
            // Pratinjau tidak mereservasi stok; kunci 30 menit mulai saat pesanan dibuat (DIPESAN).
            let kunci_hingga = Utc::now() + Duration::minutes(BATAS_BAYAR_MENIT);
            Ok(Json(json!({
                "subtotal_idr": h.subtotal,
                "ongkir_idr": h.ongkir,
                "biaya_layanan_idr": BIAYA_LAYANAN_IDR,
                "total_idr": h.subtotal + h.ongkir + BIAYA_LAYANAN_IDR,
                "kunci_stok_hingga": kunci_hingga,
                "peringatan": h.peringatan,
            })))
        }
    }
}

async fn buat_pesanan(
    State(s): State<AppState>,
    p: Pengguna,
    ApiJson(req): ApiJson<PesananBuat>,
) -> AppResult<(StatusCode, Json<Value>)> {
    p.wajib(&[Peran::Pembeli])?;
    let mut tx = s.db.begin().await?;
    let h = match siapkan(
        &s,
        &mut tx,
        p.id,
        &req.trip_id,
        &req.titik_id,
        &req.voucher,
        true,
    )
    .await?
    {
        Hasil::HargaBerubah(d) => {
            tx.commit().await?; // simpan snapshot harga terbaru
            return Err(konflik_harga(d));
        }
        Hasil::Siap(h) => h,
    };
    let mut per_lapak: BTreeMap<Uuid, Vec<&BarisKeranjang>> = BTreeMap::new();
    for b in &h.baris {
        per_lapak.entry(b.lapak_id).or_default().push(b);
    }
    let catatan = req.catatan_per_lapak.clone().unwrap_or_default();
    for k in catatan.keys() {
        let ok = ids::dari_ulid(k, "").is_some_and(|l| per_lapak.contains_key(&l));
        if !ok {
            return Err(AppError::validasi(
                "catatan_per_lapak",
                format!("lapak {k} tidak ada di keranjang"),
            ));
        }
    }

    let sekarang = Utc::now();
    let batas_bayar: DateTime<Utc> = sekarang + Duration::minutes(BATAS_BAYAR_MENIT);
    let total = h.subtotal + h.ongkir + BIAYA_LAYANAN_IDR;
    let pesanan_id = ids::baru();
    sqlx::query(
        "INSERT INTO pesanan (id, pembeli_id, trip_id, titik_id, voucher, status, subtotal_idr, ongkir_idr, biaya_layanan_idr, total_idr, batas_bayar_hingga) \
         VALUES ($1,$2,$3,$4,$5,'DIPESAN',$6,$7,$8,$9,$10)",
    )
    .bind(pesanan_id)
    .bind(p.id)
    .bind(&req.trip_id)
    .bind(&req.titik_id)
    .bind(req.voucher.as_deref())
    .bind(h.subtotal)
    .bind(h.ongkir)
    .bind(BIAYA_LAYANAN_IDR as i32)
    .bind(total)
    .bind(batas_bayar)
    .execute(&mut *tx)
    .await?;
    for (lapak_id, items) in &per_lapak {
        let sub_id = ids::baru();
        let sub_total: i64 = items
            .iter()
            .map(|b| b.harga_idr as i64 * b.qty as i64)
            .sum();
        sqlx::query("INSERT INTO sub_pesanan (id, pesanan_id, lapak_id, trip_id, status, subtotal_idr, catatan) VALUES ($1,$2,$3,$4,'DIPESAN',$5,$6)")
            .bind(sub_id)
            .bind(pesanan_id)
            .bind(lapak_id)
            .bind(&req.trip_id)
            .bind(sub_total)
            .bind(catatan.get(&ids::ke_ulid(*lapak_id)).map(String::as_str))
            .execute(&mut *tx)
            .await?;
        for b in items {
            // Kunci stok: kurangi stok tersedia; dikembalikan saat batal/kedaluwarsa.
            let n = sqlx::query("UPDATE varian_produk SET stok = stok - $2, diperbarui_pada = now() WHERE id = $1 AND stok >= $2")
                .bind(b.varian_id)
                .bind(b.qty)
                .execute(&mut *tx)
                .await?
                .rows_affected();
            if n == 0 {
                return Err(stok_habis(b.varian_id, b.stok, b.qty));
            }
            sqlx::query(
                "INSERT INTO item_pesanan (id, sub_pesanan_id, varian_id, produk_id, kategori_kode, judul, nama_varian, sku, harga_idr, qty) \
                 VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)",
            )
            .bind(ids::baru())
            .bind(sub_id)
            .bind(b.varian_id)
            .bind(b.produk_id)
            .bind(&b.kategori_kode)
            .bind(&b.judul)
            .bind(&b.nama_varian)
            .bind(&b.sku)
            .bind(b.harga_idr)
            .bind(b.qty)
            .execute(&mut *tx)
            .await?;
            sinkron_status_stok(&mut tx, b.produk_id).await?;
        }
    }
    catat_timeline(&mut tx, pesanan_id, "DIPESAN").await?;
    sqlx::query("DELETE FROM keranjang_item WHERE pengguna_id = $1")
        .bind(p.id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok((
        StatusCode::CREATED,
        Json(
            json!({"pesanan_id": ids::dengan_awalan(AWALAN_PESANAN, pesanan_id), "batas_bayar_hingga": batas_bayar, "total_idr": total}),
        ),
    ))
}

pub fn rute() -> Router<AppState> {
    Router::new()
        .route("/keranjang", put(ubah_keranjang).get(lihat_keranjang))
        .route("/checkout/pratinjau", post(pratinjau))
        .route(
            "/pesanan",
            post(buat_pesanan).get(crate::pesanan::daftar_pesanan),
        )
}
