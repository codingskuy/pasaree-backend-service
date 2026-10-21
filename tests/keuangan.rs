mod common;

use axum::http::{Method, StatusCode};
use chrono::{Duration, NaiveDate, Utc};
use common::*;
use pasaree::{
    auth::Peran,
    keuangan::{
        eksekusi_payout_jatuh_tempo, hari_kerja_berikutnya, jalankan_rekonsiliasi, tanggal_wib,
    },
};
use serde_json::{json, Value};
use sqlx::PgPool;
use std::sync::atomic::Ordering;
use uuid::Uuid;

const KUNCI: &str = "6f1c1a52-5b40-4a53-a9a3-0f3b8e4f9c11";

fn hari_ini() -> NaiveDate {
    tanggal_wib(Utc::now())
}

/// Pesanan dua lapak (A 36000, B 12000) dibayar, dikonfirmasi, dan diserahkan ke trip.
/// Komisi 8%: A 2880 (payout 33120), B 960 (payout 11040).
async fn serah_semua(d: &Dunia) -> (String, Uuid, Uuid) {
    let id = d.pesanan_dua_lapak().await;
    assert_eq!(d.bayar(&id, KUNCI).await.0, StatusCode::OK);
    let sa = d.sub_untuk(&id, d.lapak_a).await;
    let sb = d.sub_untuk(&id, d.lapak_b).await;
    for (tok, sub) in [(&d.prod_a, sa), (&d.prod_b, sb)] {
        assert_eq!(d.konfirmasi(tok, sub, true).await.0, StatusCode::OK);
        assert_eq!(d.serah(tok, sub, "KODE-BENAR").await.0, StatusCode::OK);
    }
    (id, sa, sb)
}

async fn payout_status(d: &Dunia, sub: Uuid) -> (String, i64, i64) {
    sqlx::query_as("SELECT status, jumlah_idr, potongan_idr FROM payout WHERE sub_pesanan_id = $1")
        .bind(sub)
        .fetch_one(&d.db)
        .await
        .unwrap()
}

async fn jatuhkan_tempo(d: &Dunia) {
    sqlx::query("UPDATE payout SET jadwal_payout = $1 WHERE status = 'MENUNGGU'")
        .bind(hari_ini())
        .execute(&d.db)
        .await
        .unwrap();
}

#[sqlx::test]
async fn komisi_dibekukan_saat_bayar_tidak_dibebankan_ke_pembeli(db: PgPool) {
    let d = dunia(db.clone()).await;
    let id = d.pesanan_dua_lapak().await;
    let (_, det) = kirim(
        &d.app,
        Method::GET,
        &format!("/pesanan/{id}"),
        Some(&d.pembeli),
        None,
    )
    .await;
    assert_eq!(
        det["total_idr"], 58000,
        "total = subtotal + ongkir + layanan, tanpa komisi"
    );
    // sebelum bayar komisi belum dihitung
    let belum: i64 =
        sqlx::query_scalar("SELECT count(*) FROM sub_pesanan WHERE komisi_idr IS NOT NULL")
            .fetch_one(&db)
            .await
            .unwrap();
    assert_eq!(belum, 0);
    d.bayar(&id, KUNCI).await;
    let rows: Vec<(i32, i64)> =
        sqlx::query_as("SELECT tarif_persen, komisi_idr FROM item_pesanan ORDER BY harga_idr DESC")
            .fetch_all(&db)
            .await
            .unwrap();
    assert_eq!(rows, vec![(8, 2880), (8, 960)]);
    let (_, det) = kirim(
        &d.app,
        Method::GET,
        &format!("/pesanan/{id}"),
        Some(&d.pembeli),
        None,
    )
    .await;
    assert_eq!(det["total_idr"], 58000);
    // lapak melihat komisi dan estimasi payout terpisah
    let (_, j) = kirim(
        &d.app,
        Method::GET,
        "/web/pesanan-masuk",
        Some(&d.prod_a),
        None,
    )
    .await;
    assert_eq!(j["data"][0]["total_idr"], 36000);
    assert_eq!(j["data"][0]["komisi_idr"], 2880);
    assert_eq!(j["data"][0]["estimasi_payout_idr"], 33120);
    // audit: escrow ditahan + komisi dihitung per sub
    let jenis: Vec<String> =
        sqlx::query_scalar("SELECT jenis FROM audit_keuangan ORDER BY waktu, id")
            .fetch_all(&db)
            .await
            .unwrap();
    assert_eq!(jenis.iter().filter(|j| *j == "KOMISI_DIHITUNG").count(), 2);
    assert!(jenis.contains(&"ESCROW_DITAHAN".to_string()));
}

#[sqlx::test]
async fn tarif_hampers_10_dan_promo_lapak_baru_5_persen_30_hari(db: PgPool) {
    let d = dunia(db.clone()).await;
    let (_, vh) = seed_produk(
        &db,
        d.lapak_a,
        "Paket Hampers Lebaran Isi 6",
        "paket-hampers",
        50000,
        10,
        "TAYANG",
    )
    .await;
    let komisi_hampers = |db: PgPool| async move {
        sqlx::query_scalar::<_, i64>(
            "SELECT komisi_idr FROM item_pesanan WHERE varian_id = $1 ORDER BY id DESC LIMIT 1",
        )
        .bind(vh)
        .fetch_one(&db)
        .await
        .unwrap()
    };
    let pesan = || async {
        d.keranjang(vh, 1).await;
        let id = d.pesan().await.1["pesanan_id"]
            .as_str()
            .unwrap()
            .to_string();
        d.bayar(&id, &Uuid::new_v4().to_string()).await;
    };
    // hampers 10% tanpa promo
    pesan().await;
    assert_eq!(komisi_hampers(db.clone()).await, 5000);
    // promo aktif (produk pertama tayang 10 hari lalu): 5% untuk semua baris
    sqlx::query(
        "UPDATE lapak SET produk_pertama_tayang_pada = now() - interval '10 days' WHERE id = $1",
    )
    .bind(d.lapak_a)
    .execute(&db)
    .await
    .unwrap();
    pesan().await;
    assert_eq!(komisi_hampers(db.clone()).await, 2500);
    // promo habis (31 hari): kembali 10%
    sqlx::query(
        "UPDATE lapak SET produk_pertama_tayang_pada = now() - interval '31 days' WHERE id = $1",
    )
    .bind(d.lapak_a)
    .execute(&db)
    .await
    .unwrap();
    pesan().await;
    assert_eq!(komisi_hampers(db.clone()).await, 5000);
    // info tarif untuk lapak
    sqlx::query(
        "UPDATE lapak SET produk_pertama_tayang_pada = now() - interval '10 days' WHERE id = $1",
    )
    .bind(d.lapak_a)
    .execute(&db)
    .await
    .unwrap();
    let (s, j) = kirim(
        &d.app,
        Method::GET,
        &format!("/web/komisi/tarif?lapak_id={}", u(d.lapak_a)),
        Some(&d.prod_a),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{j}");
    assert_eq!(
        (
            j["tarif_default_persen"].as_i64(),
            j["tarif_hampers_persen"].as_i64(),
            j["tarif_lapak_baru_persen"].as_i64()
        ),
        (Some(8), Some(10), Some(5))
    );
    assert!(j["promo_lapak_baru_hingga"].is_string());
    assert_eq!(
        kirim(
            &d.app,
            Method::GET,
            &format!("/web/komisi/tarif?lapak_id={}", u(d.lapak_b)),
            Some(&d.prod_a),
            None
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
}

#[sqlx::test]
async fn perubahan_tarif_default_14_hari_append_only_dan_audit(db: PgPool) {
    let d = dunia(db.clone()).await;
    let (_, admin) = seed_pengguna(&db, "admin@contoh.id", Peran::Admin).await;
    let tgl = |n: i64| (hari_ini() + Duration::days(n)).to_string();
    // < 14 hari -> 422; tepat 14 -> 200
    let (s, j) = kirim(
        &d.app,
        Method::PATCH,
        "/web/admin/komisi",
        Some(&admin),
        Some(json!({"tarif_default_persen": 9, "berlaku_mulai": tgl(13)})),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{j}");
    assert_eq!(j["detail"]["kesalahan"][0]["kolom"], "berlaku_mulai");
    let (s, j) = kirim(&d.app, Method::PATCH, "/web/admin/komisi", Some(&admin), Some(json!({"tarif_default_persen": 9, "berlaku_mulai": tgl(14), "alasan": "Penyesuaian biaya operasional"}))).await;
    assert_eq!((s, j["tarif_persen"].as_i64()), (StatusCode::OK, Some(9)));
    // tarif lama tetap berlaku sampai tanggal efektif
    let (_, info) = kirim(
        &d.app,
        Method::GET,
        "/web/komisi/tarif",
        Some(&d.prod_a),
        None,
    )
    .await;
    assert_eq!(info["tarif_default_persen"], 8);
    assert_eq!(info["perubahan_terjadwal"][0]["tarif_persen"], 9);
    // validasi dan peran
    for badan in [
        json!({"tarif_default_persen": 101, "berlaku_mulai": tgl(20)}),
        json!({"tarif_default_persen": -1, "berlaku_mulai": tgl(20)}),
        json!({"tarif_default_persen": 8, "berlaku_mulai": "kemarin"}),
    ] {
        assert_eq!(
            kirim(
                &d.app,
                Method::PATCH,
                "/web/admin/komisi",
                Some(&admin),
                Some(badan)
            )
            .await
            .0,
            StatusCode::UNPROCESSABLE_ENTITY
        );
    }
    assert_eq!(
        kirim(
            &d.app,
            Method::PATCH,
            "/web/admin/komisi",
            Some(&d.prod_a),
            Some(json!({"tarif_default_persen": 9, "berlaku_mulai": tgl(20)}))
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        kirim(
            &d.app,
            Method::PATCH,
            "/web/admin/komisi",
            Some(&d.pembeli),
            Some(json!({"tarif_default_persen": 9, "berlaku_mulai": tgl(20)}))
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );

    // riwayat append-only: pencatat, alasan, tanggal berlaku
    let (kode, tarif, oleh, alasan): (String, i32, Option<Uuid>, String) = sqlx::query_as("SELECT kode, tarif_persen, diubah_oleh, alasan FROM tarif_komisi ORDER BY dibuat_pada DESC LIMIT 1").fetch_one(&db).await.unwrap();
    assert_eq!(
        (kode.as_str(), tarif, alasan.as_str()),
        ("DEFAULT", 9, "Penyesuaian biaya operasional")
    );
    assert!(oleh.is_some());
    assert!(sqlx::query("UPDATE tarif_komisi SET tarif_persen = 1")
        .execute(&db)
        .await
        .is_err());
    assert!(sqlx::query("DELETE FROM tarif_komisi")
        .execute(&db)
        .await
        .is_err());

    // tarif efektif hari ini berubah: pesanan baru memakai 6%
    sqlx::query("INSERT INTO tarif_komisi (id, kode, tarif_persen, berlaku_mulai, alasan) VALUES (gen_random_uuid(), 'DEFAULT', 6, $1, 'uji')").bind(hari_ini()).execute(&db).await.unwrap();
    let id = d.pesanan_dua_lapak().await;
    d.bayar(&id, KUNCI).await;
    let komisi: i64 =
        sqlx::query_scalar("SELECT komisi_idr FROM item_pesanan WHERE harga_idr = 18000")
            .fetch_one(&db)
            .await
            .unwrap();
    assert_eq!(komisi, 2160, "18000 x 2 x 6%");
    // pesanan lama tidak berubah (komisi dibekukan): audit mencatat perubahan tarif
    let n: i64 =
        sqlx::query_scalar("SELECT count(*) FROM audit_keuangan WHERE jenis = 'TARIF_DIUBAH'")
            .fetch_one(&db)
            .await
            .unwrap();
    assert_eq!(n, 1);
}

#[sqlx::test]
async fn escrow_dilepas_saat_serah_trip_dan_payout_t_plus_1(db: PgPool) {
    let d = dunia(db.clone()).await;
    let id = d.pesanan_dua_lapak().await;
    d.bayar(&id, KUNCI).await;
    let sa = d.sub_untuk(&id, d.lapak_a).await;
    let sb = d.sub_untuk(&id, d.lapak_b).await;
    // sebelum serah-terima: escrow tertahan, belum ada payout
    let st: String = sqlx::query_scalar("SELECT status FROM escrow")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(st, "TERTAHAN");
    d.konfirmasi(&d.prod_a, sa, true).await;
    d.konfirmasi(&d.prod_b, sb, true).await;
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM payout")
            .fetch_one(&db)
            .await
            .unwrap(),
        0,
        "konfirmasi lapak belum melepas escrow"
    );

    d.serah(&d.prod_a, sa, "KODE-BENAR").await;
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT status FROM escrow")
            .fetch_one(&db)
            .await
            .unwrap(),
        "DILEPAS_SEBAGIAN"
    );
    assert_eq!(
        payout_status(&d, sa).await,
        ("MENUNGGU".to_string(), 33120, 0)
    );
    d.serah(&d.prod_b, sb, "KODE-BENAR").await;
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT status FROM escrow")
            .fetch_one(&db)
            .await
            .unwrap(),
        "DILEPAS"
    );
    assert_eq!(
        payout_status(&d, sb).await,
        ("MENUNGGU".to_string(), 11040, 0)
    );
    // payout = subtotal - komisi; dijadwalkan T+1 hari kerja
    let (jadwal, komisi, subtotal): (NaiveDate, i64, i64) = sqlx::query_as(
        "SELECT jadwal_payout, komisi_idr, subtotal_idr FROM payout WHERE sub_pesanan_id = $1",
    )
    .bind(sa)
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(jadwal, hari_kerja_berikutnya(hari_ini()));
    assert_eq!((subtotal, komisi), (36000, 2880));
    // serah ulang tidak membuat payout ganda
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM payout")
            .fetch_one(&db)
            .await
            .unwrap(),
        2
    );
    // audit trail lengkap
    let jenis: Vec<String> = sqlx::query_scalar("SELECT jenis FROM audit_keuangan")
        .fetch_all(&db)
        .await
        .unwrap();
    for j in [
        "ESCROW_DITAHAN",
        "KOMISI_DIHITUNG",
        "ESCROW_DILEPAS",
        "PAYOUT_DIJADWALKAN",
    ] {
        assert!(jenis.contains(&j.to_string()), "{j}");
    }
    // belum jatuh tempo: tidak dikirim ke Lumbung
    assert_eq!(eksekusi_payout_jatuh_tempo(&d.state).await.unwrap(), 0);
    assert!(d.lumbung.payout.lock().unwrap().is_empty());
}

#[sqlx::test]
async fn payout_dieksekusi_lumbung_idempoten_dan_retry_saat_gagal(db: PgPool) {
    let d = dunia(db.clone()).await;
    let (_, sa, sb) = serah_semua(&d).await;
    jatuhkan_tempo(&d).await;

    // Lumbung gagal: payout tetap MENUNGGU dan dicoba lagi
    d.lumbung.payout_gagal.store(true, Ordering::SeqCst);
    assert_eq!(eksekusi_payout_jatuh_tempo(&d.state).await.unwrap(), 0);
    let (st, percobaan, gagal): (String, i32, Option<String>) = sqlx::query_as(
        "SELECT status, percobaan, gagal_terakhir FROM payout WHERE sub_pesanan_id = $1",
    )
    .bind(sa)
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!((st.as_str(), percobaan), ("MENUNGGU", 1));
    assert!(gagal.is_some());

    d.lumbung.payout_gagal.store(false, Ordering::SeqCst);
    assert_eq!(eksekusi_payout_jatuh_tempo(&d.state).await.unwrap(), 2);
    assert_eq!(payout_status(&d, sa).await.0, "DIKIRIM");
    assert_eq!(payout_status(&d, sb).await.0, "DIKIRIM");
    let diterima = d.lumbung.payout.lock().unwrap().clone();
    assert_eq!(diterima.len(), 2);
    let a = diterima.iter().find(|p| p["sub_id"] == u(sa)).unwrap();
    assert_eq!(a["jumlah_idr"], 33120);
    assert_eq!(a["rincian"]["komisi_idr"], 2880);
    assert!(a["payout_id"].as_str().unwrap().starts_with("PAY-"));
    assert_eq!(
        d.lumbung.salah_kunci.load(Ordering::SeqCst),
        0,
        "service key dipakai"
    );
    let ref_l: String =
        sqlx::query_scalar("SELECT lumbung_ref FROM payout WHERE sub_pesanan_id = $1")
            .bind(sa)
            .fetch_one(&db)
            .await
            .unwrap();
    assert!(ref_l.starts_with("LMB-PAY-"));

    // jalan lagi: tidak ada pengiriman ganda
    assert_eq!(eksekusi_payout_jatuh_tempo(&d.state).await.unwrap(), 0);
    assert_eq!(d.lumbung.payout.lock().unwrap().len(), 2);
    // payout MENGIRIM yang macet > 10 menit dicoba ulang
    sqlx::query("UPDATE payout SET status = 'MENGIRIM', mengirim_sejak = now() - interval '11 minutes' WHERE sub_pesanan_id = $1").bind(sb).execute(&db).await.unwrap();
    assert_eq!(eksekusi_payout_jatuh_tempo(&d.state).await.unwrap(), 1);
    assert_eq!(payout_status(&d, sb).await.0, "DIKIRIM");
}

#[sqlx::test]
async fn refund_setelah_payout_dipotong_dari_payout_berikutnya(db: PgPool) {
    let d = dunia(db.clone()).await;
    let (id, sa, _sb) = serah_semua(&d).await;
    jatuhkan_tempo(&d).await;
    eksekusi_payout_jatuh_tempo(&d.state).await.unwrap();
    assert_eq!(payout_status(&d, sa).await.0, "DIKIRIM");

    // pembeli terima, lalu retur sub A (bumbu-dapur) setelah payout terkirim
    kirim(
        &d.app,
        Method::POST,
        &format!("/pesanan/{id}/konfirmasi-terima"),
        Some(&d.pembeli),
        None,
    )
    .await;
    let (s, _) = kirim(
        &d.app,
        Method::POST,
        &format!("/pesanan/{id}/retur"),
        Some(&d.pembeli),
        Some(json!({"sub_id": u(sa), "alasan": "kemasan bocor saat tiba"})),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let (jumlah, sisa): (i64, i64) =
        sqlx::query_as("SELECT jumlah_idr, sisa_idr FROM potongan_lapak")
            .fetch_one(&db)
            .await
            .unwrap();
    assert_eq!(
        (jumlah, sisa),
        (33120, 33120),
        "lapak mengembalikan bagian yang sudah diterimanya"
    );
    let escrow: (i64, i64) = sqlx::query_as("SELECT refund_idr, jumlah_idr FROM escrow")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(escrow.0, 36000, "pembeli menerima kembali subtotal");

    // pesanan berikutnya A: 1 x 18000 -> payout 16560, seluruhnya terpotong; sisa potongan 16560
    d.keranjang(d.va, 1).await;
    let id2 = d.pesan().await.1["pesanan_id"]
        .as_str()
        .unwrap()
        .to_string();
    d.bayar(&id2, "22222222-2222-4222-8222-222222222222").await;
    let s2 = d.sub_untuk(&id2, d.lapak_a).await;
    d.konfirmasi(&d.prod_a, s2, true).await;
    d.serah(&d.prod_a, s2, "KODE-BENAR").await;
    jatuhkan_tempo(&d).await;
    let kirim_sebelum = d.lumbung.payout.lock().unwrap().len();
    eksekusi_payout_jatuh_tempo(&d.state).await.unwrap();
    assert_eq!(
        payout_status(&d, s2).await,
        ("TERBAYAR".to_string(), 16560, 16560),
        "dipotong penuh, tidak ada kiriman ke Lumbung"
    );
    assert_eq!(d.lumbung.payout.lock().unwrap().len(), kirim_sebelum);
    let sisa: i64 = sqlx::query_scalar("SELECT sisa_idr FROM potongan_lapak")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(sisa, 16560);

    // pesanan ketiga A: 2 x 18000 -> payout 33120; terpotong 16560, dikirim 16560
    d.keranjang(d.va, 2).await;
    let id3 = d.pesan().await.1["pesanan_id"]
        .as_str()
        .unwrap()
        .to_string();
    d.bayar(&id3, "33333333-3333-4333-8333-333333333333").await;
    let s3 = d.sub_untuk(&id3, d.lapak_a).await;
    d.konfirmasi(&d.prod_a, s3, true).await;
    d.serah(&d.prod_a, s3, "KODE-BENAR").await;
    jatuhkan_tempo(&d).await;
    eksekusi_payout_jatuh_tempo(&d.state).await.unwrap();
    assert_eq!(
        payout_status(&d, s3).await,
        ("DIKIRIM".to_string(), 33120, 16560)
    );
    let terakhir = d.lumbung.payout.lock().unwrap().last().unwrap().clone();
    assert_eq!(terakhir["jumlah_idr"], 16560);
    assert_eq!(terakhir["rincian"]["potongan_idr"], 16560);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT sisa_idr FROM potongan_lapak")
            .fetch_one(&db)
            .await
            .unwrap(),
        0
    );
}

#[sqlx::test]
async fn retur_sebelum_payout_dikirim_membatalkan_payout(db: PgPool) {
    let d = dunia(db.clone()).await;
    let (id, sa, _) = serah_semua(&d).await;
    kirim(
        &d.app,
        Method::POST,
        &format!("/pesanan/{id}/konfirmasi-terima"),
        Some(&d.pembeli),
        None,
    )
    .await;
    let (s, _) = kirim(
        &d.app,
        Method::POST,
        &format!("/pesanan/{id}/retur"),
        Some(&d.pembeli),
        Some(json!({"sub_id": u(sa), "alasan": "barang tidak sesuai"})),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(payout_status(&d, sa).await.0, "DIBATALKAN");
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM potongan_lapak")
            .fetch_one(&db)
            .await
            .unwrap(),
        0,
        "dana kembali dari escrow, bukan potongan"
    );
    jatuhkan_tempo(&d).await;
    eksekusi_payout_jatuh_tempo(&d.state).await.unwrap();
    assert_eq!(
        d.lumbung.payout.lock().unwrap().len(),
        1,
        "hanya payout B yang dikirim"
    );
}

#[sqlx::test]
async fn penalti_batal_pembeli_pasca_konfirmasi_menjadi_payout_lapak(db: PgPool) {
    let d = dunia(db.clone()).await;
    d.keranjang(d.va, 2).await;
    let id = d.pesan().await.1["pesanan_id"]
        .as_str()
        .unwrap()
        .to_string();
    d.bayar(&id, KUNCI).await;
    let sa = d.sub_untuk(&id, d.lapak_a).await;
    d.konfirmasi(&d.prod_a, sa, true).await;
    kirim(
        &d.app,
        Method::POST,
        &format!("/pesanan/{id}/batalkan"),
        Some(&d.pembeli),
        None,
    )
    .await;
    let (jenis, jumlah, komisi): (String, i64, i64) = sqlx::query_as(
        "SELECT jenis, jumlah_idr, komisi_idr FROM payout WHERE sub_pesanan_id = $1",
    )
    .bind(sa)
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(
        (jenis.as_str(), jumlah, komisi),
        ("PENALTI", 3600, 0),
        "10% dari 36000 ke lapak, tanpa komisi"
    );
    let a: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_keuangan WHERE jenis IN ('REFUND_BATAL','PENALTI_KE_LAPAK')",
    )
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(a, 2);
}

#[sqlx::test]
async fn saldo_tertahan_lapak(db: PgPool) {
    let d = dunia(db.clone()).await;
    let id = d.pesanan_dua_lapak().await;
    let saldo = |tok: String| {
        let app = d.app.clone();
        async move {
            kirim(&app, Method::GET, "/web/lapak-saya", Some(&tok), None)
                .await
                .1["data"][0]["saldo_tertahan_idr"]
                .as_i64()
                .unwrap()
        }
    };
    assert_eq!(saldo(d.prod_a.clone()).await, 0, "belum dibayar");
    d.bayar(&id, KUNCI).await;
    assert_eq!(
        saldo(d.prod_a.clone()).await,
        33120,
        "subtotal - komisi tertahan di escrow"
    );
    assert_eq!(saldo(d.prod_b.clone()).await, 11040);
    let sa = d.sub_untuk(&id, d.lapak_a).await;
    d.konfirmasi(&d.prod_a, sa, true).await;
    d.serah(&d.prod_a, sa, "KODE-BENAR").await;
    assert_eq!(
        saldo(d.prod_a.clone()).await,
        33120,
        "payout menunggu masih tertahan"
    );
    sqlx::query("UPDATE payout SET status = 'TERBAYAR'")
        .execute(&db)
        .await
        .unwrap();
    assert_eq!(saldo(d.prod_a.clone()).await, 0);
    // daftar payout lapak
    let (s, j) = kirim(
        &d.app,
        Method::GET,
        &format!("/web/lapak/{}/payout", u(d.lapak_a)),
        Some(&d.prod_a),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(j["data"][0]["jumlah_idr"], 33120);
    assert_eq!(
        kirim(
            &d.app,
            Method::GET,
            &format!("/web/lapak/{}/payout", u(d.lapak_a)),
            Some(&d.prod_b),
            None
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
}

#[sqlx::test]
async fn audit_keuangan_append_only_retensi_2_tahun(db: PgPool) {
    let d = dunia(db.clone()).await;
    serah_semua(&d).await;
    let (n, min_retensi): (i64, chrono::DateTime<Utc>) =
        sqlx::query_as("SELECT count(*), min(retensi_hingga) FROM audit_keuangan")
            .fetch_one(&db)
            .await
            .unwrap();
    assert_eq!(n, 7);
    let hari = (min_retensi - Utc::now()).num_days();
    assert!(
        (729..=731).contains(&hari),
        "retensi 2 tahun, dapat {hari} hari"
    );
    assert!(sqlx::query("UPDATE audit_keuangan SET jumlah_idr = 0")
        .execute(&db)
        .await
        .is_err());
    assert!(
        sqlx::query("DELETE FROM audit_keuangan")
            .execute(&db)
            .await
            .is_err(),
        "tidak dapat dihapus sebelum 2 tahun"
    );
    // setelah masa retensi lewat, baris dapat dihapus
    sqlx::query("ALTER TABLE audit_keuangan DISABLE TRIGGER audit_keuangan_guard")
        .execute(&db)
        .await
        .unwrap();
    sqlx::query("UPDATE audit_keuangan SET retensi_hingga = now() - interval '1 day'")
        .execute(&db)
        .await
        .unwrap();
    sqlx::query("ALTER TABLE audit_keuangan ENABLE TRIGGER audit_keuangan_guard")
        .execute(&db)
        .await
        .unwrap();
    assert!(sqlx::query("DELETE FROM audit_keuangan")
        .execute(&db)
        .await
        .is_ok());
}

// ------------------------------------------------------------------ rekonsiliasi

fn baris_lumbung(pesanan: &str, escrow: &str, payout: i64, status: &str) -> Value {
    json!({"pesanan_id": pesanan, "escrow_id": escrow, "subtotal_idr": 48000, "komisi_idr": 3840, "ongkir_idr": 8000, "payout_idr": payout, "status": status})
}

async fn escrow_id(d: &Dunia, pesanan: &str) -> String {
    let (_, det) = kirim(
        &d.app,
        Method::GET,
        &format!("/pesanan/{pesanan}"),
        Some(&d.pembeli),
        None,
    )
    .await;
    det["escrow_id"].as_str().unwrap().to_string()
}

#[sqlx::test]
async fn rekonsiliasi_cocok_tanpa_tiket_dan_idempoten(db: PgPool) {
    let d = dunia(db.clone()).await;
    let (id, _, _) = serah_semua(&d).await;
    let esc = escrow_id(&d, &id).await;
    *d.lumbung.rekon.lock().unwrap() = vec![baris_lumbung(&id, &esc, 44160, "PAYOUT_BERJALAN")];
    let r = jalankan_rekonsiliasi(&d.state, hari_ini()).await.unwrap();
    assert_eq!(
        (r.status.as_str(), r.jumlah_baris, r.tiket_baru),
        ("SELESAI", 1, 0)
    );
    let (sel, sp): (Option<i64>, String) =
        sqlx::query_as("SELECT selisih_idr, status FROM rekonsiliasi_baris")
            .fetch_one(&db)
            .await
            .unwrap();
    assert_eq!((sel, sp.as_str()), (Some(0), "PAYOUT_BERJALAN"));
    let baris: (i64, i64, i64, i64) = sqlx::query_as(
        "SELECT subtotal_idr, komisi_idr, ongkir_idr, payout_idr FROM rekonsiliasi_baris",
    )
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(baris, (48000, 3840, 8000, 44160));
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM tiket_rekonsiliasi")
            .fetch_one(&db)
            .await
            .unwrap(),
        0
    );
    // jalan ulang untuk tanggal yang sama tidak menggandakan
    let r2 = jalankan_rekonsiliasi(&d.state, hari_ini()).await.unwrap();
    assert_eq!((r2.status.as_str(), r2.jumlah_baris), ("SELESAI", 1));
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM rekonsiliasi_jalan")
            .fetch_one(&db)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM rekonsiliasi_baris")
            .fetch_one(&db)
            .await
            .unwrap(),
        1
    );
}

#[sqlx::test]
async fn rekonsiliasi_selisih_di_atas_1000_membuat_tiket_prioritas_tinggi(db: PgPool) {
    let d = dunia(db.clone()).await;
    let (id, _, _) = serah_semua(&d).await;
    let esc = escrow_id(&d, &id).await;
    // selisih tepat Rp1.000: tidak ada tiket
    *d.lumbung.rekon.lock().unwrap() = vec![baris_lumbung(&id, &esc, 43160, "PAYOUT_BERJALAN")];
    let r = jalankan_rekonsiliasi(&d.state, hari_ini()).await.unwrap();
    assert_eq!(r.tiket_baru, 0);
    // Rp1.001: tiket
    sqlx::query("DELETE FROM rekonsiliasi_jalan")
        .execute(&db)
        .await
        .unwrap();
    *d.lumbung.rekon.lock().unwrap() = vec![baris_lumbung(&id, &esc, 43159, "PAYOUT_BERJALAN")];
    let r = jalankan_rekonsiliasi(&d.state, hari_ini()).await.unwrap();
    assert_eq!(r.tiket_baru, 1);
    let (jenis, prioritas, sel, status, lapak_tahu): (String, String, i64, String, bool) =
        sqlx::query_as("SELECT jenis, prioritas, selisih_idr, status, lapak_diberi_tahu_pada IS NOT NULL FROM tiket_rekonsiliasi").fetch_one(&db).await.unwrap();
    assert_eq!(
        (
            jenis.as_str(),
            prioritas.as_str(),
            sel,
            status.as_str(),
            lapak_tahu
        ),
        ("SELISIH", "TINGGI", 1001, "TERBUKA", true)
    );
    // tiket yang sama tidak digandakan pada jalan berikutnya
    sqlx::query("DELETE FROM rekonsiliasi_jalan")
        .execute(&db)
        .await
        .unwrap();
    let r = jalankan_rekonsiliasi(&d.state, hari_ini()).await.unwrap();
    assert_eq!(r.tiket_baru, 0);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM tiket_rekonsiliasi")
            .fetch_one(&db)
            .await
            .unwrap(),
        1
    );
}

#[sqlx::test]
async fn rekonsiliasi_baris_hanya_di_lumbung_dan_gantung_2x24_jam(db: PgPool) {
    let d = dunia(db.clone()).await;
    // (1) pesanan dibayar 3 hari lalu, tidak tercatat di Lumbung -> GANTUNG
    let id = d.pesanan_dua_lapak().await;
    d.bayar(&id, KUNCI).await;
    sqlx::query("UPDATE escrow SET dibuat_pada = now() - interval '3 days'")
        .execute(&db)
        .await
        .unwrap();
    // (2) baris hanya di Lumbung dengan nilai > Rp1.000 -> SELISIH
    let asing = format!("PSR-{}", pasaree::ids::ke_ulid(Uuid::now_v7()));
    *d.lumbung.rekon.lock().unwrap() = vec![baris_lumbung(&asing, "ESC-X", 5000, "TERTAHAN")];
    let r = jalankan_rekonsiliasi(&d.state, hari_ini()).await.unwrap();
    assert_eq!(r.tiket_baru, 2);
    let jenis: Vec<String> =
        sqlx::query_scalar("SELECT jenis FROM tiket_rekonsiliasi ORDER BY jenis")
            .fetch_all(&db)
            .await
            .unwrap();
    assert_eq!(jenis, vec!["GANTUNG", "SELISIH"]);
}

#[sqlx::test]
async fn rekonsiliasi_belum_2x24_jam_tidak_digantung_tetapi_payout_telat_digantung(db: PgPool) {
    let d = dunia(db.clone()).await;
    let (id, sa, _) = serah_semua(&d).await;
    // baru dibayar, belum ada di Lumbung: belum gantung
    *d.lumbung.rekon.lock().unwrap() = vec![];
    assert_eq!(
        jalankan_rekonsiliasi(&d.state, hari_ini())
            .await
            .unwrap()
            .tiket_baru,
        0
    );
    // payout menunggu lebih dari 2 hari sejak jadwal -> GANTUNG
    sqlx::query("DELETE FROM rekonsiliasi_jalan")
        .execute(&db)
        .await
        .unwrap();
    sqlx::query("UPDATE payout SET jadwal_payout = $1 WHERE sub_pesanan_id = $2")
        .bind(hari_ini() - Duration::days(3))
        .bind(sa)
        .execute(&db)
        .await
        .unwrap();
    let esc = escrow_id(&d, &id).await;
    *d.lumbung.rekon.lock().unwrap() = vec![baris_lumbung(&id, &esc, 44160, "PAYOUT_BERJALAN")];
    let r = jalankan_rekonsiliasi(&d.state, hari_ini()).await.unwrap();
    assert_eq!(r.tiket_baru, 1);
    let (jenis, rincian): (String, Value) =
        sqlx::query_as("SELECT jenis, rincian FROM tiket_rekonsiliasi")
            .fetch_one(&db)
            .await
            .unwrap();
    assert_eq!(jenis, "GANTUNG");
    assert!(rincian["alasan"].as_str().unwrap().contains("2x24"));
}

#[sqlx::test]
async fn rekonsiliasi_lumbung_mati_dicatat_gagal_lalu_diulang(db: PgPool) {
    let d = dunia(db.clone()).await;
    serah_semua(&d).await;
    d.lumbung.rekon_mati.store(true, Ordering::SeqCst);
    let r = jalankan_rekonsiliasi(&d.state, hari_ini()).await.unwrap();
    assert_eq!((r.status.as_str(), r.tiket_baru), ("GAGAL_LUMBUNG", 0));
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM tiket_rekonsiliasi")
            .fetch_one(&db)
            .await
            .unwrap(),
        0
    );
    d.lumbung.rekon_mati.store(false, Ordering::SeqCst);
    let r = jalankan_rekonsiliasi(&d.state, hari_ini()).await.unwrap();
    assert_eq!(r.status, "SELESAI");
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM rekonsiliasi_jalan")
            .fetch_one(&db)
            .await
            .unwrap(),
        1
    );
}

#[sqlx::test]
async fn endpoint_rekonsiliasi_dan_tiket_khusus_admin(db: PgPool) {
    let d = dunia(db.clone()).await;
    let (id, _, _) = serah_semua(&d).await;
    let esc = escrow_id(&d, &id).await;
    let (_, admin) = seed_pengguna(&db, "admin@contoh.id", Peran::Admin).await;
    *d.lumbung.rekon.lock().unwrap() = vec![baris_lumbung(&id, &esc, 40000, "PAYOUT_BERJALAN")];
    let badan = json!({"tanggal": hari_ini().to_string()});
    let (s, j) = kirim(
        &d.app,
        Method::POST,
        "/web/admin/rekonsiliasi",
        Some(&admin),
        Some(badan.clone()),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{j}");
    assert_eq!(
        (
            j["status"].as_str(),
            j["tiket_baru"].as_i64(),
            j["jumlah_baris"].as_i64()
        ),
        (Some("SELESAI"), Some(1), Some(1))
    );
    let (s, j) = kirim(
        &d.app,
        Method::GET,
        "/web/admin/tiket-rekonsiliasi?status=TERBUKA",
        Some(&admin),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(j["data"][0]["jenis"], "SELISIH");
    assert_eq!(j["data"][0]["prioritas"], "TINGGI");
    assert_eq!(j["data"][0]["selisih_idr"], 4160);
    assert_eq!(j["data"][0]["pesanan_id"], id);
    // peran, validasi
    for tok in [&d.prod_a, &d.pembeli] {
        assert_eq!(
            kirim(
                &d.app,
                Method::POST,
                "/web/admin/rekonsiliasi",
                Some(tok),
                Some(badan.clone())
            )
            .await
            .0,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            kirim(
                &d.app,
                Method::GET,
                "/web/admin/tiket-rekonsiliasi",
                Some(tok),
                None
            )
            .await
            .0,
            StatusCode::FORBIDDEN
        );
    }
    let besok = (hari_ini() + Duration::days(2)).to_string();
    assert_eq!(
        kirim(
            &d.app,
            Method::POST,
            "/web/admin/rekonsiliasi",
            Some(&admin),
            Some(json!({"tanggal": besok}))
        )
        .await
        .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(
        kirim(
            &d.app,
            Method::GET,
            "/web/admin/tiket-rekonsiliasi?status=ASAL",
            Some(&admin),
            None
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
}
