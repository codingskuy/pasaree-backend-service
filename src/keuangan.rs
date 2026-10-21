//! Keuangan Pasaree: komisi transparan (dipotong dari payout lapak, bukan dari pembeli), escrow,
//! payout T+1 hari kerja (dieksekusi Lumbung), riwayat tarif append-only, dan rekonsiliasi harian.
//!
//! Aturan (`keuangan/10-komisi-transparan.md`, BR-301..BR-306): `komisi_baris = floor(harga x qty x tarif / 100)`;
//! tarif 8%, paket-hampers 10%, lapak baru 5% selama 30 hari sejak produk pertama tayang; biaya layanan Rp2.000
//! masuk operasional Pasaree dan ongkir 100% ke kas trip Lumbung (tidak mengurangi payout lapak).

use std::collections::{HashMap, HashSet};

use axum::{
    extract::{Path, Query, State},
    routing::{get, patch, post},
    Json, Router,
};
use chrono::{DateTime, Datelike, Duration, FixedOffset, NaiveDate, Utc, Weekday};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;
use validator::Validate;

use crate::{
    auth::{Pengguna, Peran},
    error::{AppError, AppResult},
    ids,
    json::ApiJson,
    lapak::lapak_boleh,
    pesanan::{AWALAN_ESCROW, AWALAN_PESANAN},
    state::AppState,
};

pub const AMBANG_SELISIH_IDR: i64 = 1000;
pub const AWALAN_PAYOUT: &str = "PAY-";
const PROMO_HARI: i64 = 30;
const PENGUMUMAN_MIN_HARI: i64 = 14;

// ------------------------------------------------------------------ fungsi murni

/// `floor(harga x qty x tarif / 100)`; semua bilangan bulat positif sehingga pembagian integer = floor.
pub fn hitung_komisi_baris(harga_idr: i64, qty: i64, tarif_persen: i64) -> i64 {
    harga_idr * qty * tarif_persen / 100
}

/// Promo lapak baru (5%) mengalahkan tarif lain selama masa promo; selain itu hampers 10%, lainnya tarif default.
pub fn pilih_tarif(
    kategori: &str,
    promo_aktif: bool,
    tarif_default: i32,
    tarif_hampers: i32,
    tarif_promo: i32,
) -> i32 {
    if promo_aktif {
        tarif_promo
    } else if kategori == "paket-hampers" {
        tarif_hampers
    } else {
        tarif_default
    }
}

pub fn tanggal_wib(t: DateTime<Utc>) -> NaiveDate {
    t.with_timezone(&FixedOffset::east_opt(7 * 3600).expect("offset WIB"))
        .date_naive()
}

/// Hari kerja (Sen-Jum) berikutnya setelah `d` (T+1 hari kerja). Hari libur nasional belum dimodelkan.
pub fn hari_kerja_berikutnya(d: NaiveDate) -> NaiveDate {
    let mut n = d + Duration::days(1);
    while matches!(n.weekday(), Weekday::Sat | Weekday::Sun) {
        n += Duration::days(1);
    }
    n
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BarisRekon {
    pub pesanan_id: String,
    pub escrow_id: Option<String>,
    pub subtotal_idr: i64,
    pub komisi_idr: i64,
    pub ongkir_idr: i64,
    pub payout_idr: i64,
    pub status: String,
}

/// Selisih rekonsiliasi = jumlah selisih mutlak subtotal, komisi, ongkir, dan payout.
pub fn selisih(a: &BarisRekon, b: &BarisRekon) -> i64 {
    (a.subtotal_idr - b.subtotal_idr).abs()
        + (a.komisi_idr - b.komisi_idr).abs()
        + (a.ongkir_idr - b.ongkir_idr).abs()
        + (a.payout_idr - b.payout_idr).abs()
}

pub fn status_turunan(escrow: &str, n_payout: i64, n_terbayar: i64) -> &'static str {
    if escrow == "DIREFUND" {
        "DIREFUND"
    } else if n_payout > 0 && n_payout == n_terbayar {
        "TERBAYAR"
    } else if n_payout > 0 {
        "PAYOUT_BERJALAN"
    } else if escrow == "TERTAHAN" {
        "TERTAHAN"
    } else {
        "DILEPAS"
    }
}

// ------------------------------------------------------------------ audit & tarif

pub async fn catat_audit(
    c: &mut PgConnection,
    jenis: &str,
    pesanan_id: Option<Uuid>,
    sub_id: Option<Uuid>,
    lapak_id: Option<Uuid>,
    jumlah_idr: Option<i64>,
    data: Value,
) -> AppResult<()> {
    sqlx::query("INSERT INTO audit_keuangan (id, jenis, pesanan_id, sub_pesanan_id, lapak_id, jumlah_idr, data) VALUES ($1,$2,$3,$4,$5,$6,$7)")
        .bind(ids::baru())
        .bind(jenis)
        .bind(pesanan_id)
        .bind(sub_id)
        .bind(lapak_id)
        .bind(jumlah_idr)
        .bind(data)
        .execute(c)
        .await?;
    Ok(())
}

pub async fn tarif_efektif(c: &mut PgConnection, kode: &str, tanggal: NaiveDate) -> AppResult<i32> {
    Ok(sqlx::query_scalar("SELECT tarif_persen FROM tarif_komisi WHERE kode = $1 AND berlaku_mulai <= $2 ORDER BY berlaku_mulai DESC, dibuat_pada DESC, id DESC LIMIT 1")
        .bind(kode)
        .bind(tanggal)
        .fetch_one(c)
        .await?)
}

#[derive(sqlx::FromRow)]
struct BarisItemKomisi {
    id: Uuid,
    harga_idr: i32,
    qty: i32,
    kategori_kode: String,
}

/// Hitung dan bekukan komisi per baris item serta total per sub-pesanan (dipanggil saat DIBAYAR).
pub async fn hitung_dan_simpan_komisi(c: &mut PgConnection, pesanan_id: Uuid) -> AppResult<()> {
    let sekarang = Utc::now();
    let hari = tanggal_wib(sekarang);
    let (t_default, t_hampers, t_promo) = (
        tarif_efektif(c, "DEFAULT", hari).await?,
        tarif_efektif(c, "HAMPERS", hari).await?,
        tarif_efektif(c, "LAPAK_BARU", hari).await?,
    );
    let subs: Vec<(Uuid, Uuid, Option<DateTime<Utc>>)> = sqlx::query_as(
        "SELECT sp.id, sp.lapak_id, l.produk_pertama_tayang_pada FROM sub_pesanan sp JOIN lapak l ON l.id = sp.lapak_id WHERE sp.pesanan_id = $1 ORDER BY sp.id",
    )
    .bind(pesanan_id)
    .fetch_all(&mut *c)
    .await?;
    for (sub_id, lapak_id, pertama) in subs {
        let promo = pertama.is_some_and(|t| sekarang < t + Duration::days(PROMO_HARI));
        let items: Vec<BarisItemKomisi> = sqlx::query_as("SELECT id, harga_idr, qty, kategori_kode FROM item_pesanan WHERE sub_pesanan_id = $1 ORDER BY id").bind(sub_id).fetch_all(&mut *c).await?;
        let mut total = 0i64;
        for it in items {
            let tarif = pilih_tarif(&it.kategori_kode, promo, t_default, t_hampers, t_promo);
            let komisi = hitung_komisi_baris(it.harga_idr as i64, it.qty as i64, tarif as i64);
            total += komisi;
            sqlx::query("UPDATE item_pesanan SET tarif_persen = $2, komisi_idr = $3 WHERE id = $1")
                .bind(it.id)
                .bind(tarif)
                .bind(komisi)
                .execute(&mut *c)
                .await?;
        }
        sqlx::query("UPDATE sub_pesanan SET komisi_idr = $2 WHERE id = $1")
            .bind(sub_id)
            .bind(total)
            .execute(&mut *c)
            .await?;
        catat_audit(
            c,
            "KOMISI_DIHITUNG",
            Some(pesanan_id),
            Some(sub_id),
            Some(lapak_id),
            Some(total),
            json!({"promo_lapak_baru": promo, "tarif_default": t_default}),
        )
        .await?;
    }
    Ok(())
}

// ------------------------------------------------------------------ escrow & payout

#[derive(sqlx::FromRow)]
struct InfoRilis {
    pesanan_id: Uuid,
    lapak_id: Uuid,
    subtotal_idr: i64,
    komisi_idr: Option<i64>,
}

/// Lepas escrow untuk sub-pesanan saat serah-terima trip tercatat: jadwalkan payout T+1 hari kerja (idempoten).
pub async fn lepas_escrow_sub(c: &mut PgConnection, sub_id: Uuid) -> AppResult<()> {
    let s: InfoRilis = sqlx::query_as(
        "SELECT pesanan_id, lapak_id, subtotal_idr, komisi_idr FROM sub_pesanan WHERE id = $1",
    )
    .bind(sub_id)
    .fetch_one(&mut *c)
    .await?;
    let komisi = s
        .komisi_idr
        .ok_or_else(|| AppError::Internal("komisi belum dihitung".into()))?;
    let jumlah = s.subtotal_idr - komisi;
    let jadwal = hari_kerja_berikutnya(tanggal_wib(Utc::now()));
    let payout_id = ids::baru();
    let n = sqlx::query(
        "INSERT INTO payout (id, sub_pesanan_id, pesanan_id, lapak_id, subtotal_idr, komisi_idr, jumlah_idr, jadwal_payout) VALUES ($1,$2,$3,$4,$5,$6,$7,$8) ON CONFLICT (sub_pesanan_id) DO NOTHING",
    )
    .bind(payout_id)
    .bind(sub_id)
    .bind(s.pesanan_id)
    .bind(s.lapak_id)
    .bind(s.subtotal_idr)
    .bind(komisi)
    .bind(jumlah)
    .bind(jadwal)
    .execute(&mut *c)
    .await?
    .rows_affected();
    if n == 0 {
        return Ok(());
    }
    // Escrow: DILEPAS bila semua sub aktif sudah punya payout, selain itu sebagian.
    let belum: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM sub_pesanan sp WHERE sp.pesanan_id = $1 AND sp.status NOT IN ('DIBATALKAN','DIKEMBALIKAN') \
         AND NOT EXISTS (SELECT 1 FROM payout p WHERE p.sub_pesanan_id = sp.id)",
    )
    .bind(s.pesanan_id)
    .fetch_one(&mut *c)
    .await?;
    sqlx::query("UPDATE escrow SET status = $2 WHERE pesanan_id = $1 AND status IN ('TERTAHAN','DILEPAS_SEBAGIAN')")
        .bind(s.pesanan_id)
        .bind(if belum == 0 { "DILEPAS" } else { "DILEPAS_SEBAGIAN" })
        .execute(&mut *c)
        .await?;
    catat_audit(
        c,
        "ESCROW_DILEPAS",
        Some(s.pesanan_id),
        Some(sub_id),
        Some(s.lapak_id),
        Some(s.subtotal_idr),
        json!({"dasar": "serah-terima trip"}),
    )
    .await?;
    catat_audit(
        c,
        "PAYOUT_DIJADWALKAN",
        Some(s.pesanan_id),
        Some(sub_id),
        Some(s.lapak_id),
        Some(jumlah),
        json!({"payout_id": ids::dengan_awalan(AWALAN_PAYOUT, payout_id), "jadwal_payout": jadwal, "subtotal_idr": s.subtotal_idr, "komisi_idr": komisi}),
    )
    .await?;
    Ok(())
}

/// Penalti pembatalan pembeli pasca-konfirmasi diteruskan ke lapak (tanpa komisi), T+1 hari kerja.
pub async fn buat_payout_penalti(
    c: &mut PgConnection,
    sub_id: Uuid,
    penalti_idr: i64,
) -> AppResult<()> {
    if penalti_idr <= 0 {
        return Ok(());
    }
    let (pesanan_id, lapak_id): (Uuid, Uuid) =
        sqlx::query_as("SELECT pesanan_id, lapak_id FROM sub_pesanan WHERE id = $1")
            .bind(sub_id)
            .fetch_one(&mut *c)
            .await?;
    let jadwal = hari_kerja_berikutnya(tanggal_wib(Utc::now()));
    sqlx::query(
        "INSERT INTO payout (id, sub_pesanan_id, pesanan_id, lapak_id, jenis, subtotal_idr, komisi_idr, jumlah_idr, jadwal_payout) VALUES ($1,$2,$3,$4,'PENALTI',$5,0,$5,$6) ON CONFLICT (sub_pesanan_id) DO NOTHING",
    )
    .bind(ids::baru())
    .bind(sub_id)
    .bind(pesanan_id)
    .bind(lapak_id)
    .bind(penalti_idr)
    .bind(jadwal)
    .execute(&mut *c)
    .await?;
    catat_audit(
        c,
        "PENALTI_KE_LAPAK",
        Some(pesanan_id),
        Some(sub_id),
        Some(lapak_id),
        Some(penalti_idr),
        json!({"jadwal_payout": jadwal}),
    )
    .await?;
    Ok(())
}

/// Retur: bila payout belum dikirim dibatalkan (dana kembali dari escrow); bila sudah, nilainya dipotong dari payout berikutnya.
pub async fn tangani_retur(c: &mut PgConnection, sub_id: Uuid) -> AppResult<()> {
    let p: Option<(Uuid, Uuid, Uuid, i64, i64, String)> = sqlx::query_as("SELECT id, pesanan_id, lapak_id, jumlah_idr, potongan_idr, status FROM payout WHERE sub_pesanan_id = $1 FOR UPDATE")
        .bind(sub_id)
        .fetch_optional(&mut *c)
        .await?;
    let Some((payout_id, pesanan_id, lapak_id, jumlah, potongan, status)) = p else {
        return Ok(());
    };
    if status == "MENUNGGU" {
        sqlx::query("UPDATE payout SET status = 'DIBATALKAN' WHERE id = $1")
            .bind(payout_id)
            .execute(&mut *c)
            .await?;
        catat_audit(
            c,
            "PAYOUT_DIBATALKAN_RETUR",
            Some(pesanan_id),
            Some(sub_id),
            Some(lapak_id),
            Some(jumlah),
            json!({}),
        )
        .await?;
    } else if status != "DIBATALKAN" {
        let diterima_lapak = jumlah - potongan;
        if diterima_lapak > 0 {
            sqlx::query("INSERT INTO potongan_lapak (id, lapak_id, sub_pesanan_id, jumlah_idr, sisa_idr, alasan) VALUES ($1,$2,$3,$4,$4,'RETUR_SETELAH_PAYOUT') ON CONFLICT (sub_pesanan_id) DO NOTHING")
                .bind(ids::baru())
                .bind(lapak_id)
                .bind(sub_id)
                .bind(diterima_lapak)
                .execute(&mut *c)
                .await?;
            catat_audit(
                c,
                "POTONGAN_PAYOUT_BERIKUTNYA",
                Some(pesanan_id),
                Some(sub_id),
                Some(lapak_id),
                Some(diterima_lapak),
                json!({}),
            )
            .await?;
        }
    }
    Ok(())
}

/// Saldo tertahan lapak: bagian lapak (subtotal - komisi) dari escrow yang belum dilepas ditambah payout yang belum terbayar.
pub async fn saldo_tertahan(db: &PgPool, lapak_id: Uuid) -> AppResult<i64> {
    Ok(sqlx::query_scalar(
        "SELECT (COALESCE((SELECT SUM(subtotal_idr - COALESCE(komisi_idr, 0)) FROM sub_pesanan WHERE lapak_id = $1 AND status IN ('DIBAYAR','DIKONFIRMASI_LAPAK')), 0) \
              + COALESCE((SELECT SUM(jumlah_idr) FROM payout WHERE lapak_id = $1 AND status IN ('MENUNGGU','MENGIRIM','DIKIRIM')), 0))::bigint",
    )
    .bind(lapak_id)
    .fetch_one(db)
    .await?)
}

#[derive(sqlx::FromRow)]
struct PayoutJatuhTempo {
    id: Uuid,
    lapak_id: Uuid,
    pesanan_id: Uuid,
    sub_pesanan_id: Uuid,
    jenis: String,
    jumlah_idr: i64,
    subtotal_idr: i64,
    komisi_idr: i64,
    potongan_diterapkan: bool,
}

/// Kirim payout jatuh tempo ke Lumbung (eksekutor payout). Idempoten di sisi Lumbung lewat `payout_id`.
pub async fn eksekusi_payout_jatuh_tempo(s: &AppState) -> AppResult<u64> {
    let hari_ini = tanggal_wib(Utc::now());
    // Payout yang tertahan MENGIRIM > 10 menit (proses mati di tengah) dicoba ulang; aman karena Lumbung idempoten.
    sqlx::query("UPDATE payout SET status = 'MENUNGGU' WHERE status = 'MENGIRIM' AND mengirim_sejak < now() - interval '10 minutes'").execute(&s.db).await?;
    let mut sudah: HashSet<Uuid> = HashSet::new();
    let mut berhasil = 0u64;
    loop {
        let mut tx = s.db.begin().await?;
        let diskip: Vec<Uuid> = sudah.iter().copied().collect();
        let baris: Option<PayoutJatuhTempo> = sqlx::query_as(
            "SELECT id, lapak_id, pesanan_id, sub_pesanan_id, jenis, jumlah_idr, subtotal_idr, komisi_idr, potongan_diterapkan FROM payout \
             WHERE status = 'MENUNGGU' AND jadwal_payout <= $1 AND id <> ALL($2) ORDER BY jadwal_payout, id LIMIT 1 FOR UPDATE SKIP LOCKED",
        )
        .bind(hari_ini)
        .bind(&diskip)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(PayoutJatuhTempo {
            id,
            lapak_id,
            pesanan_id,
            sub_pesanan_id: sub_id,
            jenis,
            jumlah_idr: jumlah,
            subtotal_idr: subtotal,
            komisi_idr: komisi,
            potongan_diterapkan: diterapkan,
        }) = baris
        else {
            break;
        };
        sudah.insert(id);
        let mut potongan_total: i64 = if diterapkan {
            sqlx::query_scalar("SELECT potongan_idr FROM payout WHERE id = $1")
                .bind(id)
                .fetch_one(&mut *tx)
                .await?
        } else {
            0
        };
        if !diterapkan {
            let mut sisa_bayar = jumlah;
            let pots: Vec<(Uuid, i64)> = sqlx::query_as("SELECT id, sisa_idr FROM potongan_lapak WHERE lapak_id = $1 AND sisa_idr > 0 ORDER BY dibuat_pada, id FOR UPDATE").bind(lapak_id).fetch_all(&mut *tx).await?;
            for (pid, sisa) in pots {
                if sisa_bayar == 0 {
                    break;
                }
                let ambil = sisa.min(sisa_bayar);
                sqlx::query("UPDATE potongan_lapak SET sisa_idr = sisa_idr - $2 WHERE id = $1")
                    .bind(pid)
                    .bind(ambil)
                    .execute(&mut *tx)
                    .await?;
                sisa_bayar -= ambil;
                potongan_total += ambil;
            }
            sqlx::query(
                "UPDATE payout SET potongan_idr = $2, potongan_diterapkan = true WHERE id = $1",
            )
            .bind(id)
            .bind(potongan_total)
            .execute(&mut *tx)
            .await?;
            if potongan_total > 0 {
                catat_audit(
                    &mut tx,
                    "POTONGAN_DITERAPKAN",
                    Some(pesanan_id),
                    Some(sub_id),
                    Some(lapak_id),
                    Some(potongan_total),
                    json!({"payout_id": ids::dengan_awalan(AWALAN_PAYOUT, id)}),
                )
                .await?;
            }
        }
        let bersih = jumlah - potongan_total;
        if bersih == 0 {
            sqlx::query(
                "UPDATE payout SET status = 'TERBAYAR', dieksekusi_pada = now() WHERE id = $1",
            )
            .bind(id)
            .execute(&mut *tx)
            .await?;
            catat_audit(
                &mut tx,
                "PAYOUT_LUNAS_DIPOTONG",
                Some(pesanan_id),
                Some(sub_id),
                Some(lapak_id),
                Some(0),
                json!({}),
            )
            .await?;
            tx.commit().await?;
            berhasil += 1;
            continue;
        }
        sqlx::query("UPDATE payout SET status = 'MENGIRIM', mengirim_sejak = now(), percobaan = percobaan + 1 WHERE id = $1").bind(id).execute(&mut *tx).await?;
        tx.commit().await?;

        let badan = json!({
            "payout_id": ids::dengan_awalan(AWALAN_PAYOUT, id), "lapak_id": ids::ke_ulid(lapak_id),
            "pesanan_id": ids::dengan_awalan(AWALAN_PESANAN, pesanan_id), "sub_id": ids::ke_ulid(sub_id), "jenis": jenis,
            "jumlah_idr": bersih, "rincian": {"subtotal_idr": subtotal, "komisi_idr": komisi, "potongan_idr": potongan_total},
        });
        let hasil: Result<Value, _> = s
            .lumbung
            .post_json("/internal/pasaree/payout", &badan)
            .await;
        let mut tx = s.db.begin().await?;
        match hasil {
            Ok(r) => {
                let ref_l = r
                    .get("lumbung_ref")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                sqlx::query("UPDATE payout SET status = 'DIKIRIM', lumbung_ref = $2, dieksekusi_pada = now(), gagal_terakhir = NULL WHERE id = $1").bind(id).bind(&ref_l).execute(&mut *tx).await?;
                catat_audit(
                    &mut tx,
                    "PAYOUT_DIKIRIM",
                    Some(pesanan_id),
                    Some(sub_id),
                    Some(lapak_id),
                    Some(bersih),
                    json!({"lumbung_ref": ref_l}),
                )
                .await?;
                berhasil += 1;
            }
            Err(e) => {
                tracing::warn!(?e, payout = %ids::ke_ulid(id), "payout ke Lumbung gagal; dicoba lagi");
                sqlx::query(
                    "UPDATE payout SET status = 'MENUNGGU', gagal_terakhir = $2 WHERE id = $1",
                )
                .bind(id)
                .bind(e.pesan)
                .execute(&mut *tx)
                .await?;
            }
        }
        tx.commit().await?;
    }
    Ok(berhasil)
}

// ------------------------------------------------------------------ rekonsiliasi

#[derive(Debug, Serialize)]
pub struct RingkasanRekon {
    pub tanggal: NaiveDate,
    pub status: String,
    pub jumlah_baris: i32,
    pub tiket_baru: i32,
}

#[derive(sqlx::FromRow)]
struct BarisPasaree {
    pesanan_id: Uuid,
    escrow_id: Uuid,
    subtotal_idr: i64,
    komisi_idr: i64,
    ongkir_idr: i64,
    payout_idr: i64,
    status_escrow: String,
    n_payout: i64,
    n_terbayar: i64,
    dibuat_pada: DateTime<Utc>,
    aktivitas: bool,
}

#[derive(Deserialize)]
struct DaftarRekon {
    data: Vec<BarisRekon>,
}

async fn buat_tiket(
    c: &mut PgConnection,
    jenis: &str,
    pesanan_id: Uuid,
    escrow_id: Option<Uuid>,
    selisih_idr: i64,
    rincian: Value,
) -> AppResult<bool> {
    let n = sqlx::query(
        "INSERT INTO tiket_rekonsiliasi (id, jenis, pesanan_id, escrow_id, selisih_idr, rincian, lapak_diberi_tahu_pada) VALUES ($1,$2,$3,$4,$5,$6, now()) \
         ON CONFLICT (pesanan_id, jenis) WHERE status = 'TERBUKA' DO NOTHING",
    )
    .bind(ids::baru())
    .bind(jenis)
    .bind(pesanan_id)
    .bind(escrow_id)
    .bind(selisih_idr)
    .bind(&rincian)
    .execute(&mut *c)
    .await?
    .rows_affected();
    if n > 0 {
        catat_audit(
            c,
            "TIKET_REKONSILIASI",
            Some(pesanan_id),
            None,
            None,
            Some(selisih_idr),
            json!({"jenis": jenis, "prioritas": "TINGGI", "rincian": rincian}),
        )
        .await?;
    }
    Ok(n > 0)
}

/// Rekonsiliasi harian Pasaree-Lumbung untuk `tanggal` (WIB). Selisih > Rp1.000 atau status menggantung > 2x24 jam
/// membuat tiket prioritas tinggi. Idempoten: tanggal yang sudah SELESAI tidak diulang.
pub async fn jalankan_rekonsiliasi(s: &AppState, tanggal: NaiveDate) -> AppResult<RingkasanRekon> {
    if let Some((status, baris, tiket)) = sqlx::query_as::<_, (String, i32, i32)>(
        "SELECT status, jumlah_baris, jumlah_tiket FROM rekonsiliasi_jalan WHERE tanggal = $1",
    )
    .bind(tanggal)
    .fetch_optional(&s.db)
    .await?
    {
        if status == "SELESAI" {
            return Ok(RingkasanRekon {
                tanggal,
                status,
                jumlah_baris: baris,
                tiket_baru: 0,
            }
            .tap_tiket(tiket));
        }
    }
    let pasaree: Vec<BarisPasaree> = sqlx::query_as(
        "SELECT p.id AS pesanan_id, e.id AS escrow_id, \
            COALESCE(SUM(sp.subtotal_idr) FILTER (WHERE sp.status NOT IN ('DIBATALKAN','DIKEMBALIKAN')), 0)::bigint AS subtotal_idr, \
            COALESCE(SUM(sp.komisi_idr) FILTER (WHERE sp.status NOT IN ('DIBATALKAN','DIKEMBALIKAN')), 0)::bigint AS komisi_idr, \
            (CASE WHEN count(*) FILTER (WHERE sp.status <> 'DIBATALKAN') = 0 THEN 0 ELSE p.ongkir_idr END)::bigint AS ongkir_idr, \
            COALESCE((SELECT SUM(jumlah_idr) FROM payout WHERE pesanan_id = p.id AND status <> 'DIBATALKAN'), 0)::bigint AS payout_idr, \
            e.status AS status_escrow, \
            (SELECT count(*) FROM payout WHERE pesanan_id = p.id AND status <> 'DIBATALKAN')::bigint AS n_payout, \
            (SELECT count(*) FROM payout WHERE pesanan_id = p.id AND status = 'TERBAYAR')::bigint AS n_terbayar, \
            e.dibuat_pada, \
            EXISTS (SELECT 1 FROM audit_keuangan a WHERE a.pesanan_id = p.id AND (a.waktu AT TIME ZONE 'Asia/Jakarta')::date = $1) AS aktivitas \
         FROM escrow e JOIN pesanan p ON p.id = e.pesanan_id JOIN sub_pesanan sp ON sp.pesanan_id = p.id \
         WHERE (e.dibuat_pada AT TIME ZONE 'Asia/Jakarta')::date <= $1 GROUP BY p.id, e.id ORDER BY p.id",
    )
    .bind(tanggal)
    .fetch_all(&s.db)
    .await?;
    let baris_p: Vec<(BarisPasaree, BarisRekon)> = pasaree
        .into_iter()
        .filter(|b| {
            let final_ =
                b.status_escrow == "DIREFUND" || (b.n_payout > 0 && b.n_payout == b.n_terbayar);
            !final_ || b.aktivitas
        })
        .map(|b| {
            let r = BarisRekon {
                pesanan_id: ids::dengan_awalan(AWALAN_PESANAN, b.pesanan_id),
                escrow_id: Some(ids::dengan_awalan(AWALAN_ESCROW, b.escrow_id)),
                subtotal_idr: b.subtotal_idr,
                komisi_idr: b.komisi_idr,
                ongkir_idr: b.ongkir_idr,
                payout_idr: b.payout_idr,
                status: status_turunan(&b.status_escrow, b.n_payout, b.n_terbayar).to_string(),
            };
            (b, r)
        })
        .collect();

    let lumbung: Result<DaftarRekon, _> = s
        .lumbung
        .get_json(
            "/internal/pasaree/rekonsiliasi",
            &[("tanggal", tanggal.to_string())],
        )
        .await;
    let mut tx = s.db.begin().await?;
    sqlx::query("DELETE FROM rekonsiliasi_jalan WHERE tanggal = $1")
        .bind(tanggal)
        .execute(&mut *tx)
        .await?;
    let jalan_id = ids::baru();
    let lumbung = match lumbung {
        Ok(l) => l,
        Err(e) => {
            tracing::warn!(?e, "rekonsiliasi: Lumbung tidak merespons");
            sqlx::query("INSERT INTO rekonsiliasi_jalan (id, tanggal, status) VALUES ($1,$2,'GAGAL_LUMBUNG')").bind(jalan_id).bind(tanggal).execute(&mut *tx).await?;
            tx.commit().await?;
            return Ok(RingkasanRekon {
                tanggal,
                status: "GAGAL_LUMBUNG".into(),
                jumlah_baris: 0,
                tiket_baru: 0,
            });
        }
    };
    let peta_l: HashMap<&str, &BarisRekon> = lumbung
        .data
        .iter()
        .map(|r| (r.pesanan_id.as_str(), r))
        .collect();
    let sekarang = Utc::now();
    let mut tiket = 0;
    let mut dilihat: HashSet<String> = HashSet::new();
    sqlx::query("INSERT INTO rekonsiliasi_jalan (id, tanggal, status) VALUES ($1,$2,'SELESAI')")
        .bind(jalan_id)
        .bind(tanggal)
        .execute(&mut *tx)
        .await?;
    for (b, r) in &baris_p {
        dilihat.insert(r.pesanan_id.clone());
        let (sel, status_l) = match peta_l.get(r.pesanan_id.as_str()) {
            Some(l) => (Some(selisih(r, l)), Some(l.status.clone())),
            None => (None, None),
        };
        sqlx::query("INSERT INTO rekonsiliasi_baris (id, jalan_id, pesanan_id, escrow_id, subtotal_idr, komisi_idr, ongkir_idr, payout_idr, status, status_lumbung, selisih_idr) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)")
            .bind(ids::baru())
            .bind(jalan_id)
            .bind(b.pesanan_id)
            .bind(b.escrow_id)
            .bind(r.subtotal_idr)
            .bind(r.komisi_idr)
            .bind(r.ongkir_idr)
            .bind(r.payout_idr)
            .bind(&r.status)
            .bind(&status_l)
            .bind(sel)
            .execute(&mut *tx)
            .await?;
        match sel {
            Some(x) if x > AMBANG_SELISIH_IDR => {
                let l = peta_l[r.pesanan_id.as_str()];
                if buat_tiket(&mut tx, "SELISIH", b.pesanan_id, Some(b.escrow_id), x, json!({"pasaree": r, "lumbung": l, "tanggal": tanggal})).await? {
                    tiket += 1;
                }
            }
            None if sekarang - b.dibuat_pada > Duration::hours(48)
                && buat_tiket(&mut tx, "GANTUNG", b.pesanan_id, Some(b.escrow_id), r.payout_idr.max(r.subtotal_idr), json!({"alasan": "tidak tercatat di Lumbung > 2x24 jam", "pasaree": r, "tanggal": tanggal})).await? =>
            {
                tiket += 1;
            }
            _ => {}
        }
    }
    // Baris yang hanya ada di Lumbung.
    for l in &lumbung.data {
        if dilihat.contains(&l.pesanan_id) {
            continue;
        }
        let total =
            l.subtotal_idr.abs() + l.komisi_idr.abs() + l.ongkir_idr.abs() + l.payout_idr.abs();
        if total > AMBANG_SELISIH_IDR {
            if let Some(pid) = ids::dari_ulid(&l.pesanan_id, AWALAN_PESANAN) {
                if buat_tiket(
                    &mut tx,
                    "SELISIH",
                    pid,
                    None,
                    total,
                    json!({"pasaree": null, "lumbung": l, "tanggal": tanggal}),
                )
                .await?
                {
                    tiket += 1;
                }
            }
        }
    }
    // Payout menggantung > 2x24 jam setelah jadwal.
    let gantung: Vec<(Uuid, Uuid, i64)> = sqlx::query_as(
        "SELECT p.pesanan_id, e.id, p.jumlah_idr FROM payout p LEFT JOIN escrow e ON e.pesanan_id = p.pesanan_id \
         WHERE p.status IN ('MENUNGGU','MENGIRIM','DIKIRIM') AND p.jadwal_payout < $1 - 2",
    )
    .bind(tanggal)
    .fetch_all(&mut *tx)
    .await?;
    for (pesanan_id, escrow_id, jumlah) in gantung {
        if buat_tiket(&mut tx, "GANTUNG", pesanan_id, Some(escrow_id), jumlah, json!({"alasan": "payout belum terbayar > 2x24 jam setelah jadwal", "tanggal": tanggal})).await? {
            tiket += 1;
        }
    }
    sqlx::query("UPDATE rekonsiliasi_jalan SET jumlah_baris = $2, jumlah_tiket = $3 WHERE id = $1")
        .bind(jalan_id)
        .bind(baris_p.len() as i32)
        .bind(tiket)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(RingkasanRekon {
        tanggal,
        status: "SELESAI".into(),
        jumlah_baris: baris_p.len() as i32,
        tiket_baru: tiket,
    })
}

impl RingkasanRekon {
    fn tap_tiket(self, _tiket: i32) -> Self {
        self
    }
}

/// Jalankan rekonsiliasi untuk kemarin (WIB) bila belum SELESAI; dipanggil berkala.
pub async fn rekonsiliasi_kemarin_jika_perlu(s: &AppState) -> AppResult<u64> {
    let kemarin = tanggal_wib(Utc::now()) - Duration::days(1);
    let selesai: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM rekonsiliasi_jalan WHERE tanggal = $1 AND status = 'SELESAI')").bind(kemarin).fetch_one(&s.db).await?;
    if selesai {
        return Ok(0);
    }
    let r = jalankan_rekonsiliasi(s, kemarin).await?;
    Ok(u64::from(r.status == "SELESAI"))
}

// ------------------------------------------------------------------ endpoint

#[derive(Deserialize, Validate)]
pub struct KomisiUbah {
    #[validate(range(min = 0, max = 100, message = "tarif 0-100"))]
    pub tarif_default_persen: i32,
    pub berlaku_mulai: NaiveDate,
    #[validate(length(min = 3, max = 280, message = "alasan 3-280 karakter"))]
    pub alasan: Option<String>,
}

/// Perubahan tarif default diumumkan >= 14 hari sebelum berlaku; riwayat append-only.
async fn ubah_komisi(
    State(s): State<AppState>,
    p: Pengguna,
    ApiJson(req): ApiJson<KomisiUbah>,
) -> AppResult<Json<Value>> {
    p.wajib(&[Peran::Admin])?;
    let paling_cepat = tanggal_wib(Utc::now()) + Duration::days(PENGUMUMAN_MIN_HARI);
    if req.berlaku_mulai < paling_cepat {
        return Err(AppError::validasi("berlaku_mulai", format!("Perubahan tarif diumumkan minimal 14 hari sebelum berlaku (paling cepat {paling_cepat}).")));
    }
    let mut tx = s.db.begin().await?;
    sqlx::query("INSERT INTO tarif_komisi (id, kode, tarif_persen, berlaku_mulai, diubah_oleh, alasan) VALUES ($1,'DEFAULT',$2,$3,$4,$5)")
        .bind(ids::baru())
        .bind(req.tarif_default_persen)
        .bind(req.berlaku_mulai)
        .bind(p.id)
        .bind(req.alasan.as_deref().unwrap_or("Perubahan tarif default"))
        .execute(&mut *tx)
        .await?;
    catat_audit(&mut tx, "TARIF_DIUBAH", None, None, None, None, json!({"tarif_default_persen": req.tarif_default_persen, "berlaku_mulai": req.berlaku_mulai, "diubah_oleh": ids::ke_ulid(p.id)})).await?;
    tx.commit().await?;
    Ok(Json(
        json!({"tarif_persen": req.tarif_default_persen, "berlaku_mulai": req.berlaku_mulai}),
    ))
}

#[derive(Deserialize)]
pub struct TarifQuery {
    pub lapak_id: Option<String>,
}

async fn info_tarif(
    State(s): State<AppState>,
    p: Pengguna,
    Query(q): Query<TarifQuery>,
) -> AppResult<Json<Value>> {
    p.wajib(&[Peran::Produsen, Peran::Operator, Peran::Admin])?;
    let mut c = s.db.acquire().await?;
    let hari = tanggal_wib(Utc::now());
    let mut promo_hingga: Option<DateTime<Utc>> = None;
    if let Some(l) = &q.lapak_id {
        let lid = ids::dari_ulid(l, "").ok_or_else(|| {
            AppError::tidak_ditemukan("LAPAK_TIDAK_DITEMUKAN", "Lapak tidak ditemukan.")
        })?;
        lapak_boleh(&mut *c, &p, lid, false).await?;
        let pertama: Option<DateTime<Utc>> =
            sqlx::query_scalar("SELECT produk_pertama_tayang_pada FROM lapak WHERE id = $1")
                .bind(lid)
                .fetch_one(&mut *c)
                .await?;
        promo_hingga = pertama
            .map(|t| t + Duration::days(PROMO_HARI))
            .filter(|t| *t > Utc::now());
    }
    let terjadwal: Vec<(String, i32, NaiveDate)> = sqlx::query_as("SELECT kode, tarif_persen, berlaku_mulai FROM tarif_komisi WHERE berlaku_mulai > $1 ORDER BY berlaku_mulai, dibuat_pada").bind(hari).fetch_all(&mut *c).await?;
    Ok(Json(json!({
        "tarif_default_persen": tarif_efektif(&mut c, "DEFAULT", hari).await?,
        "tarif_hampers_persen": tarif_efektif(&mut c, "HAMPERS", hari).await?,
        "tarif_lapak_baru_persen": tarif_efektif(&mut c, "LAPAK_BARU", hari).await?,
        "promo_lapak_baru_hingga": promo_hingga,
        "perubahan_terjadwal": terjadwal.iter().map(|t| json!({"kode": t.0, "tarif_persen": t.1, "berlaku_mulai": t.2})).collect::<Vec<_>>(),
    })))
}

#[derive(Deserialize, Validate)]
pub struct RekonReq {
    pub tanggal: Option<NaiveDate>,
}

async fn jalankan_rekon(
    State(s): State<AppState>,
    p: Pengguna,
    ApiJson(req): ApiJson<RekonReq>,
) -> AppResult<Json<Value>> {
    p.wajib(&[Peran::Admin])?;
    let tanggal = req
        .tanggal
        .unwrap_or_else(|| tanggal_wib(Utc::now()) - Duration::days(1));
    if tanggal > tanggal_wib(Utc::now()) {
        return Err(AppError::validasi(
            "tanggal",
            "Tanggal tidak boleh di masa depan.",
        ));
    }
    let r = jalankan_rekonsiliasi(&s, tanggal).await?;
    Ok(Json(
        serde_json::to_value(r).map_err(|e| AppError::Internal(e.to_string()))?,
    ))
}

#[derive(Deserialize)]
pub struct TiketQuery {
    pub status: Option<String>,
}

async fn daftar_tiket(
    State(s): State<AppState>,
    p: Pengguna,
    Query(q): Query<TiketQuery>,
) -> AppResult<Json<Value>> {
    p.wajib(&[Peran::Admin])?;
    if let Some(st) = &q.status {
        if !["TERBUKA", "SELESAI"].contains(&st.as_str()) {
            return Err(AppError::Permintaan(
                "status harus TERBUKA atau SELESAI.".into(),
            ));
        }
    }
    #[derive(sqlx::FromRow)]
    struct BarisTiket {
        id: Uuid,
        jenis: String,
        prioritas: String,
        pesanan_id: Uuid,
        selisih_idr: i64,
        rincian: Value,
        status: String,
        dibuat_pada: DateTime<Utc>,
    }
    let rows: Vec<BarisTiket> = sqlx::query_as(
        "SELECT id, jenis, prioritas, pesanan_id, selisih_idr, rincian, status, dibuat_pada FROM tiket_rekonsiliasi \
         WHERE ($1::text IS NULL OR status = $1) ORDER BY dibuat_pada DESC, id DESC LIMIT 200",
    )
    .bind(q.status.as_deref())
    .fetch_all(&s.db)
    .await?;
    Ok(Json(json!({"data": rows.iter().map(|r| json!({
        "id": ids::ke_ulid(r.id), "jenis": r.jenis, "prioritas": r.prioritas, "pesanan_id": ids::dengan_awalan(AWALAN_PESANAN, r.pesanan_id),
        "selisih_idr": r.selisih_idr, "rincian": r.rincian, "status": r.status, "dibuat_pada": r.dibuat_pada,
    })).collect::<Vec<_>>()})))
}

async fn payout_lapak(
    State(s): State<AppState>,
    p: Pengguna,
    Path(id): Path<String>,
) -> AppResult<Json<Value>> {
    let lid = ids::dari_ulid(&id, "").ok_or_else(|| {
        AppError::tidak_ditemukan("LAPAK_TIDAK_DITEMUKAN", "Lapak tidak ditemukan.")
    })?;
    lapak_boleh(&s.db, &p, lid, false).await?;
    #[derive(sqlx::FromRow)]
    struct BarisPayout {
        id: Uuid,
        sub_pesanan_id: Uuid,
        pesanan_id: Uuid,
        jenis: String,
        subtotal_idr: i64,
        komisi_idr: i64,
        potongan_idr: i64,
        jumlah_idr: i64,
        status: String,
        jadwal_payout: NaiveDate,
    }
    let rows: Vec<BarisPayout> = sqlx::query_as(
        "SELECT id, sub_pesanan_id, pesanan_id, jenis, subtotal_idr, komisi_idr, potongan_idr, jumlah_idr, status, jadwal_payout \
         FROM payout WHERE lapak_id = $1 ORDER BY dilepas_pada DESC, id DESC LIMIT 200",
    )
    .bind(lid)
    .fetch_all(&s.db)
    .await?;
    Ok(Json(json!({"data": rows.iter().map(|r| json!({
        "payout_id": ids::dengan_awalan(AWALAN_PAYOUT, r.id), "sub_id": ids::ke_ulid(r.sub_pesanan_id),
        "pesanan_id": ids::dengan_awalan(AWALAN_PESANAN, r.pesanan_id), "jenis": r.jenis, "subtotal_idr": r.subtotal_idr,
        "komisi_idr": r.komisi_idr, "potongan_idr": r.potongan_idr, "jumlah_idr": r.jumlah_idr, "status": r.status,
        "jadwal_payout": r.jadwal_payout,
    })).collect::<Vec<_>>()})))
}

pub fn rute() -> Router<AppState> {
    Router::new()
        .route("/web/admin/komisi", patch(ubah_komisi))
        .route("/web/komisi/tarif", get(info_tarif))
        .route("/web/admin/rekonsiliasi", post(jalankan_rekon))
        .route("/web/admin/tiket-rekonsiliasi", get(daftar_tiket))
        .route("/web/lapak/{id}/payout", get(payout_lapak))
}

#[cfg(test)]
mod uji {
    use super::*;

    #[test]
    fn komisi_baris_floor_integer() {
        assert_eq!(hitung_komisi_baris(18000, 2, 8), 2880);
        assert_eq!(hitung_komisi_baris(18500, 1, 8), 1480);
        assert_eq!(hitung_komisi_baris(500, 1, 8), 40);
        assert_eq!(hitung_komisi_baris(1500, 1, 5), 75);
        assert_eq!(hitung_komisi_baris(2500, 3, 10), 750);
        assert_eq!(hitung_komisi_baris(500, 3, 5), 75);
        // sisa pecahan dibuang (floor): 99 x 8% = 7,92 -> 7
        assert_eq!(hitung_komisi_baris(99, 1, 8), 7);
        assert_eq!(hitung_komisi_baris(1999, 1, 8), 159);
        assert_eq!(hitung_komisi_baris(0, 5, 8), 0);
        assert_eq!(hitung_komisi_baris(18000, 2, 0), 0);
    }

    #[test]
    fn pilih_tarif_promo_hampers_default() {
        assert_eq!(pilih_tarif("bumbu-dapur", false, 8, 10, 5), 8);
        assert_eq!(pilih_tarif("paket-hampers", false, 8, 10, 5), 10);
        assert_eq!(pilih_tarif("bumbu-dapur", true, 8, 10, 5), 5);
        assert_eq!(pilih_tarif("paket-hampers", true, 8, 10, 5), 5);
    }

    #[test]
    fn t_plus_1_hari_kerja() {
        let d = |y, m, dd| NaiveDate::from_ymd_opt(y, m, dd).unwrap();
        assert_eq!(hari_kerja_berikutnya(d(2026, 10, 21)), d(2026, 10, 22)); // Rabu -> Kamis
        assert_eq!(hari_kerja_berikutnya(d(2026, 10, 23)), d(2026, 10, 26)); // Jumat -> Senin
        assert_eq!(hari_kerja_berikutnya(d(2026, 10, 24)), d(2026, 10, 26)); // Sabtu -> Senin
        assert_eq!(hari_kerja_berikutnya(d(2026, 10, 25)), d(2026, 10, 26)); // Minggu -> Senin
    }

    fn baris(sub: i64, kom: i64, ong: i64, pay: i64) -> BarisRekon {
        BarisRekon {
            pesanan_id: "PSR-X".into(),
            escrow_id: None,
            subtotal_idr: sub,
            komisi_idr: kom,
            ongkir_idr: ong,
            payout_idr: pay,
            status: "TERTAHAN".into(),
        }
    }

    #[test]
    fn selisih_rekonsiliasi_dijumlah_mutlak() {
        assert_eq!(
            selisih(
                &baris(48000, 3840, 8000, 44160),
                &baris(48000, 3840, 8000, 44160)
            ),
            0
        );
        assert_eq!(
            selisih(
                &baris(48000, 3840, 8000, 44160),
                &baris(48000, 3840, 8000, 43160)
            ),
            1000
        );
        assert_eq!(
            selisih(
                &baris(48000, 3840, 8000, 44160),
                &baris(47000, 3840, 9000, 44160)
            ),
            2000
        );
    }

    #[test]
    fn status_turunan_escrow() {
        assert_eq!(status_turunan("TERTAHAN", 0, 0), "TERTAHAN");
        assert_eq!(status_turunan("DILEPAS", 0, 0), "DILEPAS");
        assert_eq!(status_turunan("DILEPAS", 2, 1), "PAYOUT_BERJALAN");
        assert_eq!(status_turunan("DILEPAS", 2, 2), "TERBAYAR");
        assert_eq!(status_turunan("DIREFUND", 0, 0), "DIREFUND");
    }
}
