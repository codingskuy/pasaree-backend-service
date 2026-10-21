//! Pesanan: state machine, bayar idempoten, konfirmasi terima, batal, retur, tugas waktu.
//!
//! Status sub-pesanan (per lapak) dan status induk (pesanan) memakai kosakata yang sama:
//! DIPESAN -> DIBAYAR -> DIKONFIRMASI_LAPAK -> DISERAHKAN_KE_TRIP -> DITERIMA_PEMBELI -> SELESAI,
//! cabang DIBATALKAN dan DIKEMBALIKAN (`produk/10-alur-beli.md` §2).

use axum::{
    extract::{Path, Query, State},
    routing::{get, post},
    Json, Router,
};
use chrono::{DateTime, Duration, FixedOffset, NaiveDate, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;
use validator::Validate;

use crate::{
    auth::{Pengguna, Peran},
    dukungan::{
        decode_cursor, encode_cursor, idempotensi_awal, idempotensi_simpan, potong, Halaman,
    },
    error::{AppError, AppResult},
    ids,
    json::ApiJson,
    model::sinkron_status_stok,
    state::AppState,
};

pub const AWALAN_PESANAN: &str = "PSR-";
pub const AWALAN_ESCROW: &str = "ESC-";

// ------------------------------------------------------------------ state machine

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Dipesan,
    Dibayar,
    DikonfirmasiLapak,
    DiserahkanKeTrip,
    DiterimaPembeli,
    Selesai,
    Dibatalkan,
    Dikembalikan,
}

impl Status {
    pub const SEMUA: [Status; 8] = [
        Status::Dipesan,
        Status::Dibayar,
        Status::DikonfirmasiLapak,
        Status::DiserahkanKeTrip,
        Status::DiterimaPembeli,
        Status::Selesai,
        Status::Dibatalkan,
        Status::Dikembalikan,
    ];

    pub fn sebagai_str(self) -> &'static str {
        match self {
            Status::Dipesan => "DIPESAN",
            Status::Dibayar => "DIBAYAR",
            Status::DikonfirmasiLapak => "DIKONFIRMASI_LAPAK",
            Status::DiserahkanKeTrip => "DISERAHKAN_KE_TRIP",
            Status::DiterimaPembeli => "DITERIMA_PEMBELI",
            Status::Selesai => "SELESAI",
            Status::Dibatalkan => "DIBATALKAN",
            Status::Dikembalikan => "DIKEMBALIKAN",
        }
    }

    pub fn dari_str(s: &str) -> Option<Status> {
        Status::SEMUA.into_iter().find(|x| x.sebagai_str() == s)
    }

    fn peringkat(self) -> u8 {
        match self {
            Status::Dipesan => 1,
            Status::Dibayar => 2,
            Status::DikonfirmasiLapak => 3,
            Status::DiserahkanKeTrip => 4,
            Status::DiterimaPembeli => 5,
            Status::Selesai | Status::Dikembalikan => 6,
            Status::Dibatalkan => 0,
        }
    }
}

/// Transisi sah sub-pesanan. DISERAHKAN_KE_TRIP -> SELESAI langsung untuk pesanan tanpa item yang dapat diretur
/// (pangan segar); selain itu lewat DITERIMA_PEMBELI (masa retur 1x24 jam).
pub fn boleh_transisi(dari: Status, ke: Status) -> bool {
    use Status::*;
    matches!(
        (dari, ke),
        (Dipesan, Dibayar)
            | (Dipesan, Dibatalkan)
            | (Dibayar, DikonfirmasiLapak)
            | (Dibayar, Dibatalkan)
            | (DikonfirmasiLapak, DiserahkanKeTrip)
            | (DikonfirmasiLapak, Dibatalkan)
            | (DiserahkanKeTrip, DiterimaPembeli)
            | (DiserahkanKeTrip, Selesai)
            | (DiterimaPembeli, Selesai)
            | (DiterimaPembeli, Dikembalikan)
    )
}

/// Status induk dari status sub-pesanan.
pub fn status_induk(subs: &[Status]) -> Status {
    let aktif: Vec<Status> = subs
        .iter()
        .copied()
        .filter(|s| *s != Status::Dibatalkan)
        .collect();
    if aktif.is_empty() {
        return Status::Dibatalkan;
    }
    if aktif
        .iter()
        .all(|s| matches!(s, Status::Selesai | Status::Dikembalikan))
    {
        return if aktif.iter().all(|s| *s == Status::Dikembalikan) {
            Status::Dikembalikan
        } else {
            Status::Selesai
        };
    }
    if aktif.iter().any(|s| {
        matches!(
            s,
            Status::DiterimaPembeli | Status::Selesai | Status::Dikembalikan
        )
    }) {
        return Status::DiterimaPembeli;
    }
    aktif
        .into_iter()
        .min_by_key(|s| s.peringkat())
        .unwrap_or(Status::Dibatalkan)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fase {
    /// Dibatalkan sebelum konfirmasi lapak (pembeli, penolakan lapak, atau konfirmasi kedaluwarsa).
    PraKonfirmasi,
    /// Pembeli membatalkan setelah lapak konfirmasi.
    PascaKonfirmasiPembeli,
    /// Lapak membatalkan setelah konfirmasi (stok/produksi gagal).
    PascaKonfirmasiLapak,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Refund {
    pub refund_idr: i64,
    pub penalti_idr: i64,
}

/// Hitung refund pembatalan sub-pesanan (`produk/10-alur-beli.md` §4-§5). Uang integer, `floor()` untuk penalti.
/// Ongkir (dan biaya layanan pada fase pra-konfirmasi) kembali hanya bila seluruh checkout batal.
pub fn hitung_refund(
    subtotal: i64,
    ongkir: i64,
    biaya_layanan: i64,
    fase: Fase,
    seluruh_checkout: bool,
) -> Refund {
    match fase {
        Fase::PraKonfirmasi | Fase::PascaKonfirmasiLapak => Refund {
            refund_idr: subtotal
                + if seluruh_checkout {
                    ongkir + biaya_layanan
                } else {
                    0
                },
            penalti_idr: 0,
        },
        Fase::PascaKonfirmasiPembeli => {
            let penalti = subtotal * 10 / 100;
            Refund {
                refund_idr: subtotal - penalti + if seluruh_checkout { ongkir } else { 0 },
                penalti_idr: penalti,
            }
        }
    }
}

/// Tambah `jam` jam kerja (07.00-19.00 WIB, setiap hari) pada `dari`.
pub fn tambah_jam_kerja(dari: DateTime<Utc>, jam: i64) -> DateTime<Utc> {
    let wib = FixedOffset::east_opt(7 * 3600).expect("offset WIB");
    let jam_wib = |tgl: NaiveDate, h: u32| {
        wib.from_local_datetime(&tgl.and_hms_opt(h, 0, 0).expect("jam valid"))
            .single()
            .expect("waktu lokal")
    };
    let mut t = dari.with_timezone(&wib);
    let mut sisa = Duration::hours(jam);
    loop {
        let (awal, akhir) = (jam_wib(t.date_naive(), 7), jam_wib(t.date_naive(), 19));
        if t < awal {
            t = awal;
        }
        if t >= akhir {
            t = jam_wib(t.date_naive() + Duration::days(1), 7);
            continue;
        }
        if sisa <= akhir - t {
            return (t + sisa).with_timezone(&Utc);
        }
        sisa -= akhir - t;
        t = jam_wib(t.date_naive() + Duration::days(1), 7);
    }
}

// ------------------------------------------------------------------ operasi dasar

pub async fn catat_timeline(c: &mut PgConnection, pesanan_id: Uuid, status: &str) -> AppResult<()> {
    sqlx::query("INSERT INTO pesanan_timeline (id, pesanan_id, status) VALUES ($1,$2,$3)")
        .bind(ids::baru())
        .bind(pesanan_id)
        .bind(status)
        .execute(c)
        .await?;
    Ok(())
}

fn salah_status(sekarang: &str) -> AppError {
    AppError::konflik(
        "STATUS_TIDAK_VALID",
        format!("Status {sekarang} tidak mengizinkan aksi ini."),
    )
}

/// Ubah status sub-pesanan secara atomik (hanya bila masih `dari`). `kolom_waktu` dari daftar tetap.
pub async fn ubah_status_sub(
    c: &mut PgConnection,
    sub_id: Uuid,
    dari: Status,
    ke: Status,
    kolom_waktu: Option<&str>,
) -> AppResult<()> {
    if !boleh_transisi(dari, ke) {
        return Err(AppError::Internal(format!(
            "transisi tidak sah {dari:?} -> {ke:?}"
        )));
    }
    let tambahan = match kolom_waktu {
        Some(k @ ("dikonfirmasi_pada" | "diserahkan_pada" | "diterima_pada" | "selesai_pada")) => {
            format!(", {k} = now()")
        }
        None => String::new(),
        Some(k) => {
            return Err(AppError::Internal(format!(
                "kolom waktu tidak dikenal: {k}"
            )))
        }
    };
    let n = sqlx::query(&format!("UPDATE sub_pesanan SET status = $3, diperbarui_pada = now(){tambahan} WHERE id = $1 AND status = $2"))
        .bind(sub_id)
        .bind(dari.sebagai_str())
        .bind(ke.sebagai_str())
        .execute(c)
        .await?
        .rows_affected();
    if n == 1 {
        Ok(())
    } else {
        Err(salah_status("sub-pesanan telah berubah"))
    }
}

/// Hitung ulang status induk dari sub-pesanan; catat timeline bila berubah. Kembalikan status induk.
pub async fn sinkron_induk(c: &mut PgConnection, pesanan_id: Uuid) -> AppResult<Status> {
    let subs: Vec<String> =
        sqlx::query_scalar("SELECT status FROM sub_pesanan WHERE pesanan_id = $1")
            .bind(pesanan_id)
            .fetch_all(&mut *c)
            .await?;
    let subs: Vec<Status> = subs.iter().filter_map(|s| Status::dari_str(s)).collect();
    let baru = status_induk(&subs);
    let lama: String = sqlx::query_scalar("SELECT status FROM pesanan WHERE id = $1")
        .bind(pesanan_id)
        .fetch_one(&mut *c)
        .await?;
    if lama != baru.sebagai_str() {
        sqlx::query("UPDATE pesanan SET status = $2, diperbarui_pada = now() WHERE id = $1")
            .bind(pesanan_id)
            .bind(baru.sebagai_str())
            .execute(&mut *c)
            .await?;
        catat_timeline(c, pesanan_id, baru.sebagai_str()).await?;
    }
    Ok(baru)
}

async fn kembalikan_stok_sub(c: &mut PgConnection, sub_id: Uuid) -> AppResult<()> {
    let items: Vec<(Uuid, Uuid, i32)> = sqlx::query_as(
        "SELECT varian_id, produk_id, qty FROM item_pesanan WHERE sub_pesanan_id = $1",
    )
    .bind(sub_id)
    .fetch_all(&mut *c)
    .await?;
    for (varian, produk, qty) in items {
        sqlx::query(
            "UPDATE varian_produk SET stok = stok + $2, diperbarui_pada = now() WHERE id = $1",
        )
        .bind(varian)
        .bind(qty)
        .execute(&mut *c)
        .await?;
        sinkron_status_stok(c, produk).await?;
    }
    Ok(())
}

pub async fn catat_refund(
    c: &mut PgConnection,
    pesanan_id: Uuid,
    sub_id: Option<Uuid>,
    r: &Refund,
) -> AppResult<()> {
    if let Some(sub) = sub_id {
        sqlx::query("UPDATE sub_pesanan SET refund_idr = refund_idr + $2, penalti_idr = penalti_idr + $3 WHERE id = $1").bind(sub).bind(r.refund_idr).bind(r.penalti_idr).execute(&mut *c).await?;
    }
    sqlx::query(
        "UPDATE escrow SET refund_idr = refund_idr + $2, penalti_idr = penalti_idr + $3, \
         status = CASE WHEN refund_idr + $2 >= jumlah_idr THEN 'DIREFUND' ELSE status END WHERE pesanan_id = $1",
    )
    .bind(pesanan_id)
    .bind(r.refund_idr)
    .bind(r.penalti_idr)
    .execute(c)
    .await?;
    Ok(())
}

#[derive(sqlx::FromRow)]
struct InfoSub {
    pesanan_id: Uuid,
    status: String,
    subtotal_idr: i64,
}

/// Batalkan satu sub-pesanan: stok kembali, refund sesuai fase (bila sudah dibayar), status induk disinkronkan.
pub async fn batalkan_sub(
    c: &mut PgConnection,
    sub_id: Uuid,
    alasan: &str,
    fase: Fase,
) -> AppResult<()> {
    let info: InfoSub = sqlx::query_as(
        "SELECT pesanan_id, status, subtotal_idr FROM sub_pesanan WHERE id = $1 FOR UPDATE",
    )
    .bind(sub_id)
    .fetch_one(&mut *c)
    .await?;
    let dari = Status::dari_str(&info.status)
        .ok_or_else(|| AppError::Internal("status sub tidak dikenal".into()))?;
    ubah_status_sub(c, sub_id, dari, Status::Dibatalkan, None).await?;
    sqlx::query("UPDATE sub_pesanan SET alasan_batal = $2 WHERE id = $1")
        .bind(sub_id)
        .bind(alasan)
        .execute(&mut *c)
        .await?;
    kembalikan_stok_sub(c, sub_id).await?;
    if dari != Status::Dipesan {
        let (ongkir, biaya): (i64, i32) =
            sqlx::query_as("SELECT ongkir_idr, biaya_layanan_idr FROM pesanan WHERE id = $1")
                .bind(info.pesanan_id)
                .fetch_one(&mut *c)
                .await?;
        let sisa: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM sub_pesanan WHERE pesanan_id = $1 AND status <> 'DIBATALKAN'",
        )
        .bind(info.pesanan_id)
        .fetch_one(&mut *c)
        .await?;
        let r = hitung_refund(info.subtotal_idr, ongkir, biaya as i64, fase, sisa == 0);
        catat_refund(c, info.pesanan_id, Some(sub_id), &r).await?;
    }
    sinkron_induk(c, info.pesanan_id).await?;
    Ok(())
}

/// Sub-pesanan dapat diretur bila memuat item non-pangan segar (retur pangan segar hanya salah kirim/basi, di luar cakupan).
async fn dapat_diretur(c: &mut PgConnection, sub_id: Uuid) -> AppResult<bool> {
    Ok(sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM item_pesanan WHERE sub_pesanan_id = $1 AND kategori_kode <> 'pangan-segar')").bind(sub_id).fetch_one(c).await?)
}

/// Pembeli menerima sub-pesanan (manual atau otomatis): SELESAI bila tak dapat diretur, selain itu DITERIMA_PEMBELI.
pub async fn terima_sub(c: &mut PgConnection, sub_id: Uuid) -> AppResult<()> {
    if dapat_diretur(c, sub_id).await? {
        ubah_status_sub(
            c,
            sub_id,
            Status::DiserahkanKeTrip,
            Status::DiterimaPembeli,
            Some("diterima_pada"),
        )
        .await
    } else {
        ubah_status_sub(
            c,
            sub_id,
            Status::DiserahkanKeTrip,
            Status::Selesai,
            Some("selesai_pada"),
        )
        .await?;
        sqlx::query("UPDATE sub_pesanan SET diterima_pada = now() WHERE id = $1")
            .bind(sub_id)
            .execute(c)
            .await?;
        Ok(())
    }
}

// ------------------------------------------------------------------ endpoint pembeli

fn nf_pesanan() -> AppError {
    AppError::tidak_ditemukan("PESANAN_TIDAK_DITEMUKAN", "Pesanan tidak ditemukan.")
}

fn parse_pesanan(id: &str) -> AppResult<Uuid> {
    ids::dari_ulid(id, AWALAN_PESANAN).ok_or_else(nf_pesanan)
}

#[derive(Deserialize)]
pub struct DaftarQuery {
    pub status: Option<String>,
    pub limit: Option<i64>,
    pub cursor: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct KursorId {
    id: String,
}

pub async fn daftar_pesanan(
    State(s): State<AppState>,
    p: Pengguna,
    Query(q): Query<DaftarQuery>,
) -> AppResult<Json<Value>> {
    p.wajib(&[Peran::Pembeli])?;
    let limit = Halaman {
        limit: q.limit,
        cursor: None,
    }
    .limit()?;
    if let Some(st) = &q.status {
        Status::dari_str(st).ok_or_else(|| AppError::Permintaan("status tidak dikenal.".into()))?;
    }
    let kursor = match q.cursor.as_deref() {
        Some(c) => Some(
            ids::dari_ulid(&decode_cursor::<KursorId>(c)?.id, "")
                .ok_or_else(|| AppError::Permintaan("cursor tidak valid.".into()))?,
        ),
        None => None,
    };
    let rows: Vec<(Uuid, i64, String, DateTime<Utc>)> = sqlx::query_as(
        "SELECT id, total_idr, status, dibuat_pada FROM pesanan WHERE pembeli_id = $1 AND ($2::text IS NULL OR status = $2) \
         AND ($3::uuid IS NULL OR id < $3) ORDER BY id DESC LIMIT $4",
    )
    .bind(p.id)
    .bind(q.status.as_deref())
    .bind(kursor)
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
        "data": rows.iter().map(|r| json!({"id": ids::dengan_awalan(AWALAN_PESANAN, r.0), "total_idr": r.1, "status": r.2, "dibuat_pada": r.3})).collect::<Vec<_>>(),
        "cursor_lanjut": cursor_lanjut,
    })))
}

#[derive(sqlx::FromRow)]
struct BarisPesanan {
    id: Uuid,
    total_idr: i64,
    status: String,
    dibuat_pada: DateTime<Utc>,
}

async fn muat_pesanan_milik(
    c: &mut PgConnection,
    p: &Pengguna,
    id: &str,
) -> AppResult<BarisPesanan> {
    p.wajib(&[Peran::Pembeli])?;
    let uid = parse_pesanan(id)?;
    sqlx::query_as(
        "SELECT id, total_idr, status, dibuat_pada FROM pesanan WHERE id = $1 AND pembeli_id = $2",
    )
    .bind(uid)
    .bind(p.id)
    .fetch_optional(c)
    .await?
    .ok_or_else(nf_pesanan)
}

#[derive(sqlx::FromRow)]
struct BarisDetailSub {
    id: Uuid,
    slug: String,
    nama_lapak: String,
    status: String,
    subtotal_idr: i64,
    bukti_serah_trip: Option<String>,
    diterima_pada: Option<DateTime<Utc>>,
}

async fn detail_pesanan(
    State(s): State<AppState>,
    p: Pengguna,
    Path(id): Path<String>,
) -> AppResult<Json<Value>> {
    let mut c = s.db.acquire().await?;
    let b = muat_pesanan_milik(&mut c, &p, &id).await?;
    let subs: Vec<BarisDetailSub> = sqlx::query_as(
        "SELECT sp.id, l.slug, l.nama_lapak, sp.status, sp.subtotal_idr, sp.bukti_serah_trip, sp.diterima_pada \
         FROM sub_pesanan sp JOIN lapak l ON l.id = sp.lapak_id WHERE sp.pesanan_id = $1 ORDER BY sp.id",
    )
    .bind(b.id)
    .fetch_all(&mut *c)
    .await?;
    let timeline: Vec<(String, DateTime<Utc>)> = sqlx::query_as(
        "SELECT status, waktu FROM pesanan_timeline WHERE pesanan_id = $1 ORDER BY waktu, id",
    )
    .bind(b.id)
    .fetch_all(&mut *c)
    .await?;
    let escrow: Option<Uuid> = sqlx::query_scalar("SELECT id FROM escrow WHERE pesanan_id = $1")
        .bind(b.id)
        .fetch_optional(&mut *c)
        .await?;
    let mut aksi = Vec::new();
    if subs.iter().any(|s| s.status == "DISERAHKAN_KE_TRIP") {
        aksi.push("konfirmasi_terima");
    }
    let sekarang = Utc::now();
    let mut bisa_retur = false;
    for sub in &subs {
        if sub.status == "DITERIMA_PEMBELI"
            && sub
                .diterima_pada
                .is_some_and(|t| sekarang - t <= Duration::hours(24))
            && dapat_diretur(&mut c, sub.id).await?
        {
            bisa_retur = true;
        }
    }
    if bisa_retur {
        aksi.push("ajukan_retur");
    }
    let bukti = subs.iter().find_map(|s| s.bukti_serah_trip.clone());
    Ok(Json(json!({
        "id": ids::dengan_awalan(AWALAN_PESANAN, b.id),
        "total_idr": b.total_idr, "status": b.status, "dibuat_pada": b.dibuat_pada,
        "sub_pesanan": subs.iter().map(|s| json!({
            "sub_id": ids::ke_ulid(s.id), "lapak": {"slug": s.slug, "nama_lapak": s.nama_lapak}, "status": s.status, "subtotal_idr": s.subtotal_idr,
        })).collect::<Vec<_>>(),
        "timeline": timeline.iter().map(|t| json!({"status": t.0, "waktu": t.1})).collect::<Vec<_>>(),
        "bukti_serah_trip": bukti,
        "escrow_id": escrow.map(|e| ids::dengan_awalan(AWALAN_ESCROW, e)),
        "aksi_tersedia": aksi,
    })))
}

#[derive(Deserialize, Validate)]
pub struct BayarRequest {
    pub metode: Metode,
    #[validate(custom(function = "valid_uuid"))]
    pub idempotency_key: String,
}

#[derive(Deserialize, Clone, Copy)]
pub enum Metode {
    #[serde(rename = "VA_BCA")]
    VaBca,
    #[serde(rename = "QRIS")]
    Qris,
    #[serde(rename = "EWALLET_DANA")]
    EwalletDana,
}

impl Metode {
    fn sebagai_str(self) -> &'static str {
        match self {
            Metode::VaBca => "VA_BCA",
            Metode::Qris => "QRIS",
            Metode::EwalletDana => "EWALLET_DANA",
        }
    }
}

fn valid_uuid(s: &str) -> Result<(), validator::ValidationError> {
    Uuid::parse_str(s).map(|_| ()).map_err(|_| {
        validator::ValidationError::new("uuid").with_message("idempotency_key harus UUID".into())
    })
}

/// Otorisasi kasir Pasaree. Integrasi gateway nyata belum ada: mengembalikan referensi sintetis.
fn otorisasi_kasir(_metode: Metode, _jumlah_idr: i64) -> AppResult<String> {
    Ok(format!("KSR-{}", ids::ke_ulid(ids::baru())))
}

async fn bayar(
    State(s): State<AppState>,
    p: Pengguna,
    Path(id): Path<String>,
    ApiJson(req): ApiJson<BayarRequest>,
) -> AppResult<Json<Value>> {
    p.wajib(&[Peran::Pembeli])?;
    let pid = parse_pesanan(&id)?;
    let mut tx = s.db.begin().await?;
    // Kunci baris pesanan: pembayaran paralel untuk pesanan yang sama diserialisasi.
    let row: Option<(String, i64, DateTime<Utc>)> =
        sqlx::query_as("SELECT status, total_idr, batas_bayar_hingga FROM pesanan WHERE id = $1 AND pembeli_id = $2 FOR UPDATE").bind(pid).bind(p.id).fetch_optional(&mut *tx).await?;
    let (status, total, batas) = row.ok_or_else(nf_pesanan)?;
    let cakupan = format!("bayar:{}", ids::ke_ulid(pid));
    if let Some((_, resp)) = idempotensi_awal(&mut tx, &cakupan, &req.idempotency_key).await? {
        return Ok(Json(resp)); // kunci sama: respons identik, tanpa tagihan ganda
    }
    if status != "DIPESAN" {
        return Err(salah_status(&status));
    }
    if batas <= Utc::now() {
        return Err(AppError::konflik(
            "PESANAN_KEDALUWARSA",
            "Batas bayar terlewati; pesanan akan dibatalkan.",
        ));
    }
    let referensi = otorisasi_kasir(req.metode, total)?;
    let escrow_id = ids::baru();
    sqlx::query("INSERT INTO pembayaran (id, pesanan_id, idempotency_key, metode, jumlah_idr, referensi_kasir) VALUES ($1,$2,$3,$4,$5,$6)")
        .bind(ids::baru())
        .bind(pid)
        .bind(&req.idempotency_key)
        .bind(req.metode.sebagai_str())
        .bind(total)
        .bind(&referensi)
        .execute(&mut *tx)
        .await?;
    sqlx::query("INSERT INTO escrow (id, pesanan_id, jumlah_idr) VALUES ($1,$2,$3)")
        .bind(escrow_id)
        .bind(pid)
        .bind(total)
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE pesanan SET dibayar_pada = now(), metode_bayar = $2 WHERE id = $1")
        .bind(pid)
        .bind(req.metode.sebagai_str())
        .execute(&mut *tx)
        .await?;
    let batas_konfirmasi = tambah_jam_kerja(Utc::now(), 2);
    let subs: Vec<Uuid> =
        sqlx::query_scalar("SELECT id FROM sub_pesanan WHERE pesanan_id = $1 ORDER BY id")
            .bind(pid)
            .fetch_all(&mut *tx)
            .await?;
    for sub in subs {
        ubah_status_sub(&mut tx, sub, Status::Dipesan, Status::Dibayar, None).await?;
        sqlx::query("UPDATE sub_pesanan SET batas_konfirmasi_hingga = $2 WHERE id = $1")
            .bind(sub)
            .bind(batas_konfirmasi)
            .execute(&mut *tx)
            .await?;
    }
    sinkron_induk(&mut tx, pid).await?;
    let resp =
        json!({"status": "DIBAYAR", "escrow_id": ids::dengan_awalan(AWALAN_ESCROW, escrow_id)});
    idempotensi_simpan(&mut tx, &cakupan, &req.idempotency_key, p.id, 200, &resp).await?;
    tx.commit().await?;
    Ok(Json(resp))
}

async fn konfirmasi_terima(
    State(s): State<AppState>,
    p: Pengguna,
    Path(id): Path<String>,
) -> AppResult<Json<Value>> {
    let mut tx = s.db.begin().await?;
    let b = muat_pesanan_milik(&mut tx, &p, &id).await?;
    sqlx::query("SELECT 1 FROM pesanan WHERE id = $1 FOR UPDATE")
        .bind(b.id)
        .execute(&mut *tx)
        .await?;
    let subs: Vec<Uuid> = sqlx::query_scalar("SELECT id FROM sub_pesanan WHERE pesanan_id = $1 AND status = 'DISERAHKAN_KE_TRIP' ORDER BY id").bind(b.id).fetch_all(&mut *tx).await?;
    if subs.is_empty() {
        let st: String = sqlx::query_scalar("SELECT status FROM pesanan WHERE id = $1")
            .bind(b.id)
            .fetch_one(&mut *tx)
            .await?;
        return Err(salah_status(&st));
    }
    for sub in subs {
        terima_sub(&mut tx, sub).await?;
    }
    let st = sinkron_induk(&mut tx, b.id).await?;
    tx.commit().await?;
    Ok(Json(json!({"status": st.sebagai_str()})))
}

async fn batalkan(
    State(s): State<AppState>,
    p: Pengguna,
    Path(id): Path<String>,
) -> AppResult<Json<Value>> {
    let mut tx = s.db.begin().await?;
    let b = muat_pesanan_milik(&mut tx, &p, &id).await?;
    sqlx::query("SELECT 1 FROM pesanan WHERE id = $1 FOR UPDATE")
        .bind(b.id)
        .execute(&mut *tx)
        .await?;
    let subs: Vec<(Uuid, String)> = sqlx::query_as("SELECT id, status FROM sub_pesanan WHERE pesanan_id = $1 AND status <> 'DIBATALKAN' ORDER BY id").bind(b.id).fetch_all(&mut *tx).await?;
    if subs.is_empty() {
        return Err(salah_status("DIBATALKAN"));
    }
    // Pembatalan pembeli hanya sebelum serah ke trip.
    if let Some((_, st)) = subs
        .iter()
        .find(|(_, st)| !matches!(st.as_str(), "DIPESAN" | "DIBAYAR" | "DIKONFIRMASI_LAPAK"))
    {
        return Err(salah_status(st));
    }
    for (sub, st) in subs {
        let fase = if st == "DIKONFIRMASI_LAPAK" {
            Fase::PascaKonfirmasiPembeli
        } else {
            Fase::PraKonfirmasi
        };
        batalkan_sub(&mut tx, sub, "DIBATALKAN_PEMBELI", fase).await?;
    }
    tx.commit().await?;
    Ok(Json(json!({"status": "DIBATALKAN"})))
}

#[derive(Deserialize, Validate)]
pub struct ReturRequest {
    pub sub_id: String,
    #[validate(length(min = 5, max = 280, message = "alasan 5-280 karakter"))]
    pub alasan: String,
}

async fn retur(
    State(s): State<AppState>,
    p: Pengguna,
    Path(id): Path<String>,
    ApiJson(req): ApiJson<ReturRequest>,
) -> AppResult<Json<Value>> {
    let mut tx = s.db.begin().await?;
    let b = muat_pesanan_milik(&mut tx, &p, &id).await?;
    let sub = ids::dari_ulid(&req.sub_id, "").ok_or_else(|| {
        AppError::tidak_ditemukan(
            "SUB_PESANAN_TIDAK_DITEMUKAN",
            "Sub-pesanan tidak ditemukan.",
        )
    })?;
    let row: Option<(String, Option<DateTime<Utc>>, i64)> =
        sqlx::query_as("SELECT status, diterima_pada, subtotal_idr FROM sub_pesanan WHERE id = $1 AND pesanan_id = $2 FOR UPDATE").bind(sub).bind(b.id).fetch_optional(&mut *tx).await?;
    let (st, diterima, subtotal) = row.ok_or_else(|| {
        AppError::tidak_ditemukan(
            "SUB_PESANAN_TIDAK_DITEMUKAN",
            "Sub-pesanan tidak ditemukan.",
        )
    })?;
    if st != "DITERIMA_PEMBELI" {
        return Err(salah_status(&st));
    }
    if diterima.is_none_or(|t| Utc::now() - t > Duration::hours(24)) {
        return Err(AppError::konflik(
            "MASA_RETUR_HABIS",
            "Retur maksimal 1x24 jam setelah diterima.",
        ));
    }
    if !dapat_diretur(&mut tx, sub).await? {
        return Err(AppError::konflik(
            "TIDAK_DAPAT_DIRETUR",
            "Pangan segar tidak dapat diretur.",
        ));
    }
    ubah_status_sub(
        &mut tx,
        sub,
        Status::DiterimaPembeli,
        Status::Dikembalikan,
        None,
    )
    .await?;
    sqlx::query("UPDATE sub_pesanan SET alasan_batal = $2 WHERE id = $1")
        .bind(sub)
        .bind(&req.alasan)
        .execute(&mut *tx)
        .await?;
    catat_refund(
        &mut tx,
        b.id,
        Some(sub),
        &Refund {
            refund_idr: subtotal,
            penalti_idr: 0,
        },
    )
    .await?;
    sinkron_induk(&mut tx, b.id).await?;
    tx.commit().await?;
    Ok(Json(json!({"status": "DIKEMBALIKAN"})))
}

// ------------------------------------------------------------------ tugas waktu

/// Pesanan DIPESAN yang lewat batas bayar dibatalkan dan stok kembali.
pub async fn batalkan_kedaluwarsa(db: &PgPool) -> AppResult<u64> {
    let mut tx = db.begin().await?;
    let ids_p: Vec<Uuid> = sqlx::query_scalar("SELECT id FROM pesanan WHERE status = 'DIPESAN' AND batas_bayar_hingga < now() ORDER BY id FOR UPDATE SKIP LOCKED").fetch_all(&mut *tx).await?;
    for pid in &ids_p {
        let subs: Vec<Uuid> = sqlx::query_scalar(
            "SELECT id FROM sub_pesanan WHERE pesanan_id = $1 AND status = 'DIPESAN'",
        )
        .bind(pid)
        .fetch_all(&mut *tx)
        .await?;
        for sub in subs {
            batalkan_sub(&mut tx, sub, "BATAS_BAYAR_TERLEWATI", Fase::PraKonfirmasi).await?;
        }
    }
    tx.commit().await?;
    Ok(ids_p.len() as u64)
}

/// Sub-pesanan DIBAYAR yang tak dikonfirmasi lapak sampai batas jam kerja dibatalkan, dana kembali penuh (penalti skor lapak).
pub async fn konfirmasi_kedaluwarsa(db: &PgPool) -> AppResult<u64> {
    let mut tx = db.begin().await?;
    let subs: Vec<Uuid> = sqlx::query_scalar("SELECT id FROM sub_pesanan WHERE status = 'DIBAYAR' AND batas_konfirmasi_hingga < now() ORDER BY id FOR UPDATE SKIP LOCKED").fetch_all(&mut *tx).await?;
    for sub in &subs {
        batalkan_sub(&mut tx, *sub, "KONFIRMASI_KEDALUWARSA", Fase::PraKonfirmasi).await?;
    }
    tx.commit().await?;
    Ok(subs.len() as u64)
}

/// Terima otomatis 2x24 jam setelah serah ke trip (aproksimasi "setelah trip tiba").
pub async fn terima_otomatis(db: &PgPool) -> AppResult<u64> {
    let mut tx = db.begin().await?;
    let subs: Vec<(Uuid, Uuid)> =
        sqlx::query_as("SELECT id, pesanan_id FROM sub_pesanan WHERE status = 'DISERAHKAN_KE_TRIP' AND diserahkan_pada < now() - interval '48 hours' ORDER BY id FOR UPDATE SKIP LOCKED")
            .fetch_all(&mut *tx)
            .await?;
    for (sub, pid) in &subs {
        terima_sub(&mut tx, *sub).await?;
        sinkron_induk(&mut tx, *pid).await?;
    }
    tx.commit().await?;
    Ok(subs.len() as u64)
}

/// Masa retur 1x24 jam habis: DITERIMA_PEMBELI -> SELESAI.
pub async fn selesaikan_masa_retur(db: &PgPool) -> AppResult<u64> {
    let mut tx = db.begin().await?;
    let subs: Vec<(Uuid, Uuid)> =
        sqlx::query_as("SELECT id, pesanan_id FROM sub_pesanan WHERE status = 'DITERIMA_PEMBELI' AND diterima_pada < now() - interval '24 hours' ORDER BY id FOR UPDATE SKIP LOCKED")
            .fetch_all(&mut *tx)
            .await?;
    for (sub, pid) in &subs {
        ubah_status_sub(
            &mut tx,
            *sub,
            Status::DiterimaPembeli,
            Status::Selesai,
            Some("selesai_pada"),
        )
        .await?;
        sinkron_induk(&mut tx, *pid).await?;
    }
    tx.commit().await?;
    Ok(subs.len() as u64)
}

/// Skor fulfillment lapak: persentase respons (terima/tolak) tepat waktu dalam 90 hari; 100 bila belum ada data.
pub async fn skor_fulfillment(db: &PgPool, lapak_id: Uuid) -> AppResult<i32> {
    let (tepat, total): (i64, i64) = sqlx::query_as(
        "SELECT count(*) FILTER (WHERE direspons_pada IS NOT NULL AND direspons_pada <= batas_konfirmasi_hingga), count(*) \
         FROM sub_pesanan WHERE lapak_id = $1 AND batas_konfirmasi_hingga IS NOT NULL \
           AND batas_konfirmasi_hingga > now() - interval '90 days' \
           AND (direspons_pada IS NOT NULL OR batas_konfirmasi_hingga < now()) \
           AND COALESCE(alasan_batal, '') <> 'DIBATALKAN_PEMBELI'",
    )
    .bind(lapak_id)
    .fetch_one(db)
    .await?;
    Ok(if total == 0 {
        100
    } else {
        ((tepat * 100 + total / 2) / total) as i32
    })
}

pub fn rute() -> Router<AppState> {
    Router::new()
        .route("/pesanan/{id}", get(detail_pesanan))
        .route("/pesanan/{id}/bayar", post(bayar))
        .route("/pesanan/{id}/konfirmasi-terima", post(konfirmasi_terima))
        .route("/pesanan/{id}/batalkan", post(batalkan))
        .route("/pesanan/{id}/retur", post(retur))
}

#[cfg(test)]
mod uji {
    use super::*;
    use Status::*;

    #[test]
    fn matriks_transisi_lengkap() {
        let sah = [
            (Dipesan, Dibayar),
            (Dipesan, Dibatalkan),
            (Dibayar, DikonfirmasiLapak),
            (Dibayar, Dibatalkan),
            (DikonfirmasiLapak, DiserahkanKeTrip),
            (DikonfirmasiLapak, Dibatalkan),
            (DiserahkanKeTrip, DiterimaPembeli),
            (DiserahkanKeTrip, Selesai),
            (DiterimaPembeli, Selesai),
            (DiterimaPembeli, Dikembalikan),
        ];
        for dari in Status::SEMUA {
            for ke in Status::SEMUA {
                assert_eq!(
                    boleh_transisi(dari, ke),
                    sah.contains(&(dari, ke)),
                    "{dari:?} -> {ke:?}"
                );
            }
        }
    }

    #[test]
    fn status_terminal_tidak_bisa_berubah() {
        for ke in Status::SEMUA {
            assert!(!boleh_transisi(Selesai, ke));
            assert!(!boleh_transisi(Dibatalkan, ke));
            assert!(!boleh_transisi(Dikembalikan, ke));
        }
    }

    #[test]
    fn status_induk_dari_sub() {
        assert_eq!(status_induk(&[Dipesan, Dipesan]), Dipesan);
        assert_eq!(status_induk(&[Dibayar, DikonfirmasiLapak]), Dibayar);
        assert_eq!(
            status_induk(&[DikonfirmasiLapak, DiserahkanKeTrip]),
            DikonfirmasiLapak
        );
        assert_eq!(status_induk(&[Dibatalkan, Dibatalkan]), Dibatalkan);
        assert_eq!(status_induk(&[Dibatalkan, Dibayar]), Dibayar);
        assert_eq!(status_induk(&[Selesai, DikonfirmasiLapak]), DiterimaPembeli);
        assert_eq!(status_induk(&[Selesai, Selesai]), Selesai);
        assert_eq!(status_induk(&[Selesai, Dibatalkan]), Selesai);
        assert_eq!(status_induk(&[Dikembalikan, Dibatalkan]), Dikembalikan);
        assert_eq!(status_induk(&[DiterimaPembeli, Selesai]), DiterimaPembeli);
    }

    #[test]
    fn refund_per_fase_integer_floor() {
        // pra-konfirmasi, sebagian: hanya subtotal
        assert_eq!(
            hitung_refund(40_000, 8_000, 2_000, Fase::PraKonfirmasi, false),
            Refund {
                refund_idr: 40_000,
                penalti_idr: 0
            }
        );
        // pra-konfirmasi, seluruh checkout: subtotal + ongkir + biaya layanan (100%)
        assert_eq!(
            hitung_refund(40_000, 8_000, 2_000, Fase::PraKonfirmasi, true),
            Refund {
                refund_idr: 50_000,
                penalti_idr: 0
            }
        );
        // pembeli batal setelah konfirmasi: penalti 10% (floor) ke lapak
        assert_eq!(
            hitung_refund(40_000, 8_000, 2_000, Fase::PascaKonfirmasiPembeli, false),
            Refund {
                refund_idr: 36_000,
                penalti_idr: 4_000
            }
        );
        assert_eq!(
            hitung_refund(40_000, 8_000, 2_000, Fase::PascaKonfirmasiPembeli, true),
            Refund {
                refund_idr: 44_000,
                penalti_idr: 4_000
            }
        );
        assert_eq!(
            hitung_refund(18_500, 0, 0, Fase::PascaKonfirmasiPembeli, false),
            Refund {
                refund_idr: 16_650,
                penalti_idr: 1_850
            }
        );
        assert_eq!(
            hitung_refund(1_999, 0, 0, Fase::PascaKonfirmasiPembeli, false).penalti_idr,
            199,
            "floor"
        );
        // lapak batal setelah konfirmasi: 100%
        assert_eq!(
            hitung_refund(40_000, 8_000, 2_000, Fase::PascaKonfirmasiLapak, false),
            Refund {
                refund_idr: 40_000,
                penalti_idr: 0
            }
        );
    }

    fn wib(y: i32, m: u32, d: u32, h: u32, mi: u32) -> DateTime<Utc> {
        FixedOffset::east_opt(7 * 3600)
            .unwrap()
            .with_ymd_and_hms(y, m, d, h, mi, 0)
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn jam_kerja_07_sampai_19_wib() {
        assert_eq!(
            tambah_jam_kerja(wib(2026, 10, 21, 10, 0), 2),
            wib(2026, 10, 21, 12, 0)
        );
        assert_eq!(
            tambah_jam_kerja(wib(2026, 10, 21, 18, 0), 2),
            wib(2026, 10, 22, 8, 0)
        );
        assert_eq!(
            tambah_jam_kerja(wib(2026, 10, 21, 22, 0), 2),
            wib(2026, 10, 22, 9, 0)
        );
        assert_eq!(
            tambah_jam_kerja(wib(2026, 10, 21, 5, 0), 2),
            wib(2026, 10, 21, 9, 0)
        );
        assert_eq!(
            tambah_jam_kerja(wib(2026, 10, 21, 17, 0), 2),
            wib(2026, 10, 21, 19, 0)
        );
    }
}
