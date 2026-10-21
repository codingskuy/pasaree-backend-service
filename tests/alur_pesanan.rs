mod common;

use axum::http::{Method, StatusCode};
use common::*;
use pasaree::auth::Peran;
use serde_json::json;
use sqlx::PgPool;
use std::sync::atomic::Ordering;

const KUNCI: &str = "6f1c1a52-5b40-4a53-a9a3-0f3b8e4f9c11";

/// Pesanan dua lapak yang sudah dibayar (subtotal A 36000, B 12000).
async fn dibayar(d: &Dunia) -> String {
    let id = d.pesanan_dua_lapak().await;
    assert_eq!(d.bayar(&id, KUNCI).await.0, StatusCode::OK);
    id
}

#[sqlx::test]
async fn pesanan_masuk_tampil_untuk_lapak_pemilik(db: PgPool) {
    let d = dunia(db.clone()).await;
    let id = dibayar(&d).await;
    let (s, j) = kirim(
        &d.app,
        Method::GET,
        "/web/pesanan-masuk?status=DIKONFIRMASI_MENUNGGU",
        Some(&d.prod_a),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{j}");
    assert_eq!(
        j["data"].as_array().unwrap().len(),
        1,
        "hanya lapak sendiri"
    );
    let m = &j["data"][0];
    assert_eq!(m["pesanan_id"], id);
    assert_eq!(m["total_idr"], 36000);
    assert_eq!(m["trip_id"], "TRP-1");
    assert_eq!(m["varian"][0]["qty"], 2);
    assert!(m["varian"][0]["sku"].as_str().unwrap().starts_with("LPK-"));
    assert!(m["batas_konfirmasi_hingga"].is_string());
    // tanpa filter: sub berjalan; status tidak dikenal 400; lapak orang lain 404; pembeli 403
    assert_eq!(
        kirim(
            &d.app,
            Method::GET,
            "/web/pesanan-masuk",
            Some(&d.prod_b),
            None
        )
        .await
        .1["data"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        kirim(
            &d.app,
            Method::GET,
            "/web/pesanan-masuk?status=ASAL",
            Some(&d.prod_a),
            None
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    let (s, _) = kirim(
        &d.app,
        Method::GET,
        &format!("/web/pesanan-masuk?lapak_id={}", u(d.lapak_b)),
        Some(&d.prod_a),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    assert_eq!(
        kirim(
            &d.app,
            Method::GET,
            "/web/pesanan-masuk",
            Some(&d.pembeli),
            None
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
}

#[sqlx::test]
async fn alur_lengkap_sampai_selesai_dengan_timeline(db: PgPool) {
    let d = dunia(db.clone()).await;
    let id = dibayar(&d).await;
    let sa = d.sub_untuk(&id, d.lapak_a).await;
    let sb = d.sub_untuk(&id, d.lapak_b).await;

    // konfirmasi kedua lapak (idempoten dengan header Idempotency-Key)
    let (s, j) = kirim_dengan_header(
        &d.app,
        Method::POST,
        &format!("/web/pesanan-masuk/{}/konfirmasi", u(sa)),
        Some(&d.prod_a),
        &[("idempotency-key", KUNCI)],
        Some(json!({"terima": true})),
    )
    .await;
    assert_eq!(
        (s, j["status"].as_str()),
        (StatusCode::OK, Some("DIKONFIRMASI_LAPAK"))
    );
    let (_, j2) = kirim_dengan_header(
        &d.app,
        Method::POST,
        &format!("/web/pesanan-masuk/{}/konfirmasi", u(sa)),
        Some(&d.prod_a),
        &[("idempotency-key", KUNCI)],
        Some(json!({"terima": true})),
    )
    .await;
    assert_eq!(j2, j, "kunci sama = respons identik");
    assert_eq!(
        d.status_induk(&id).await,
        "DIBAYAR",
        "induk mengikuti sub paling awal"
    );
    assert_eq!(
        d.konfirmasi(&d.prod_b, sb, true).await.1["status"],
        "DIKONFIRMASI_LAPAK"
    );
    assert_eq!(d.status_induk(&id).await, "DIKONFIRMASI_LAPAK");
    // tidak bisa dikonfirmasi dua kali
    assert_eq!(
        d.konfirmasi(&d.prod_b, sb, true).await.0,
        StatusCode::CONFLICT
    );

    // serah ke trip: kode salah -> 422; benar -> DISERAHKAN_KE_TRIP + bukti
    let (s, j) = d.serah(&d.prod_a, sa, "KODE-SALAH").await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{j}");
    assert_eq!(d.status_sub(sa).await, "DIKONFIRMASI_LAPAK");
    let (s, j) = d.serah(&d.prod_a, sa, "KODE-BENAR").await;
    assert_eq!(
        (s, j["status"].as_str()),
        (StatusCode::OK, Some("DISERAHKAN_KE_TRIP"))
    );
    assert_eq!(
        d.lumbung.salah_kunci.load(Ordering::SeqCst),
        0,
        "Lumbung menerima service key, bukan token pengguna"
    );
    assert_eq!(d.status_induk(&id).await, "DIKONFIRMASI_LAPAK");
    d.serah(&d.prod_b, sb, "KODE-BENAR").await;
    assert_eq!(d.status_induk(&id).await, "DISERAHKAN_KE_TRIP");

    let (_, det) = kirim(
        &d.app,
        Method::GET,
        &format!("/pesanan/{id}"),
        Some(&d.pembeli),
        None,
    )
    .await;
    assert_eq!(det["bukti_serah_trip"], "BUKTI-LMB-1");
    assert_eq!(det["aksi_tersedia"], json!(["konfirmasi_terima"]));

    // pembeli konfirmasi terima: B (pangan segar) langsung SELESAI, A (dapat diretur) DITERIMA_PEMBELI
    let (s, j) = kirim(
        &d.app,
        Method::POST,
        &format!("/pesanan/{id}/konfirmasi-terima"),
        Some(&d.pembeli),
        None,
    )
    .await;
    assert_eq!(
        (s, j["status"].as_str()),
        (StatusCode::OK, Some("DITERIMA_PEMBELI"))
    );
    assert_eq!(d.status_sub(sb).await, "SELESAI");
    assert_eq!(d.status_sub(sa).await, "DITERIMA_PEMBELI");
    let (_, det) = kirim(
        &d.app,
        Method::GET,
        &format!("/pesanan/{id}"),
        Some(&d.pembeli),
        None,
    )
    .await;
    assert_eq!(det["aksi_tersedia"], json!(["ajukan_retur"]));
    // konfirmasi ulang -> 409
    assert_eq!(
        kirim(
            &d.app,
            Method::POST,
            &format!("/pesanan/{id}/konfirmasi-terima"),
            Some(&d.pembeli),
            None
        )
        .await
        .0,
        StatusCode::CONFLICT
    );

    // masa retur 24 jam habis -> SELESAI
    sqlx::query("UPDATE sub_pesanan SET diterima_pada = now() - interval '25 hours' WHERE id = $1")
        .bind(sa)
        .execute(&db)
        .await
        .unwrap();
    assert_eq!(
        pasaree::pesanan::selesaikan_masa_retur(&db).await.unwrap(),
        1
    );
    assert_eq!(d.status_induk(&id).await, "SELESAI");

    let (_, det) = kirim(
        &d.app,
        Method::GET,
        &format!("/pesanan/{id}"),
        Some(&d.pembeli),
        None,
    )
    .await;
    let urut: Vec<&str> = det["timeline"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["status"].as_str().unwrap())
        .collect();
    assert_eq!(
        urut,
        vec![
            "DIPESAN",
            "DIBAYAR",
            "DIKONFIRMASI_LAPAK",
            "DISERAHKAN_KE_TRIP",
            "DITERIMA_PEMBELI",
            "SELESAI"
        ]
    );
}

#[sqlx::test]
async fn pesanan_satu_lapak_pangan_segar_langsung_selesai(db: PgPool) {
    let d = dunia(db.clone()).await;
    d.keranjang(d.vb, 1).await;
    let id = d.pesan().await.1["pesanan_id"]
        .as_str()
        .unwrap()
        .to_string();
    d.bayar(&id, KUNCI).await;
    let sb = d.sub_untuk(&id, d.lapak_b).await;
    d.konfirmasi(&d.prod_b, sb, true).await;
    d.serah(&d.prod_b, sb, "KODE-BENAR").await;
    let (_, j) = kirim(
        &d.app,
        Method::POST,
        &format!("/pesanan/{id}/konfirmasi-terima"),
        Some(&d.pembeli),
        None,
    )
    .await;
    assert_eq!(j["status"], "SELESAI");
}

#[sqlx::test]
async fn serah_trip_validasi(db: PgPool) {
    let d = dunia(db.clone()).await;
    let id = dibayar(&d).await;
    let sa = d.sub_untuk(&id, d.lapak_a).await;
    // belum dikonfirmasi -> 409
    assert_eq!(
        d.serah(&d.prod_a, sa, "KODE-BENAR").await.0,
        StatusCode::CONFLICT
    );
    d.konfirmasi(&d.prod_a, sa, true).await;
    // trip berbeda -> 422
    let (s, _) = kirim(
        &d.app,
        Method::POST,
        &format!("/web/pesanan-masuk/{}/serah-trip", u(sa)),
        Some(&d.prod_a),
        Some(json!({"trip_id": "TRP-LAIN", "kode_serah": "KODE-BENAR"})),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    // lapak lain tidak boleh -> 404; pembeli -> 403
    assert_eq!(
        d.serah(&d.prod_b, sa, "KODE-BENAR").await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        d.serah(&d.pembeli, sa, "KODE-BENAR").await.0,
        StatusCode::FORBIDDEN
    );
    // Lumbung mati -> 503, status tidak berubah
    d.lumbung.mati.store(true, Ordering::SeqCst);
    let (s, j) = d.serah(&d.prod_a, sa, "KODE-BENAR").await;
    assert_eq!(
        (s, j["kode"].as_str()),
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Some("LUMBUNG_TIDAK_TERSEDIA")
        )
    );
    assert_eq!(d.status_sub(sa).await, "DIKONFIRMASI_LAPAK");
    d.lumbung.mati.store(false, Ordering::SeqCst);
    assert_eq!(d.serah(&d.prod_a, sa, "KODE-BENAR").await.0, StatusCode::OK);
    // serah ulang -> 409
    assert_eq!(
        d.serah(&d.prod_a, sa, "KODE-BENAR").await.0,
        StatusCode::CONFLICT
    );
}

#[sqlx::test]
async fn lapak_menolak_refund_dan_ongkir_hanya_bila_seluruh_checkout_batal(db: PgPool) {
    let d = dunia(db.clone()).await;
    let id = dibayar(&d).await;
    let sa = d.sub_untuk(&id, d.lapak_a).await;
    let sb = d.sub_untuk(&id, d.lapak_b).await;
    // tolak wajib alasan
    let (s, _) = kirim(
        &d.app,
        Method::POST,
        &format!("/web/pesanan-masuk/{}/konfirmasi", u(sa)),
        Some(&d.prod_a),
        Some(json!({"terima": false})),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);

    let (s, j) = d.konfirmasi(&d.prod_a, sa, false).await;
    assert_eq!(
        (s, j["status"].as_str()),
        (StatusCode::OK, Some("DIBATALKAN"))
    );
    assert_eq!(d.stok(d.va).await, 10, "stok kembali");
    let (refund, escrow): (i64, i64) = sqlx::query_as(
        "SELECT refund_idr, (SELECT refund_idr FROM escrow) FROM sub_pesanan WHERE id = $1",
    )
    .bind(sa)
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(
        (refund, escrow),
        (36000, 36000),
        "ongkir tidak kembali bila hanya sebagian batal"
    );
    assert_eq!(d.status_induk(&id).await, "DIBAYAR", "sub B masih berjalan");

    d.konfirmasi(&d.prod_b, sb, false).await;
    assert_eq!(d.status_induk(&id).await, "DIBATALKAN");
    // seluruh checkout batal: subtotal B + ongkir + biaya layanan = 12000 + 8000 + 2000
    let escrow: (i64, String) = sqlx::query_as("SELECT refund_idr, status FROM escrow")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(escrow, (58000, "DIREFUND".to_string()));
}

#[sqlx::test]
async fn konfirmasi_kedaluwarsa_batal_otomatis_dan_skor_turun(db: PgPool) {
    let d = dunia(db.clone()).await;
    let id = dibayar(&d).await;
    let sa = d.sub_untuk(&id, d.lapak_a).await;
    let sb = d.sub_untuk(&id, d.lapak_b).await;
    let (_, p) = kirim(&d.app, Method::GET, "/katalog/lapak/lapak-a", None, None).await;
    assert_eq!(
        p["skor_fulfillment_persen"], 100,
        "belum ada keputusan yang jatuh tempo"
    );
    d.konfirmasi(&d.prod_b, sb, true).await;
    sqlx::query("UPDATE sub_pesanan SET batas_konfirmasi_hingga = now() - interval '1 minute' WHERE id = $1").bind(sa).execute(&db).await.unwrap();
    // konfirmasi setelah batas (tugas belum jalan) -> 409
    assert_eq!(
        d.konfirmasi(&d.prod_a, sa, true).await.0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        pasaree::pesanan::konfirmasi_kedaluwarsa(&db).await.unwrap(),
        1
    );
    assert_eq!(d.status_sub(sa).await, "DIBATALKAN");
    assert_eq!(d.stok(d.va).await, 10);
    let alasan: String = sqlx::query_scalar("SELECT alasan_batal FROM sub_pesanan WHERE id = $1")
        .bind(sa)
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(alasan, "KONFIRMASI_KEDALUWARSA");
    // dana kembali penuh (subtotal) dan skor lapak turun; lapak B tetap 100
    let refund: i64 = sqlx::query_scalar("SELECT refund_idr FROM sub_pesanan WHERE id = $1")
        .bind(sa)
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(refund, 36000);
    let (_, p) = kirim(&d.app, Method::GET, "/katalog/lapak/lapak-a", None, None).await;
    assert_eq!(p["skor_fulfillment_persen"], 0);
    let (_, p) = kirim(&d.app, Method::GET, "/katalog/lapak/lapak-b", None, None).await;
    assert_eq!(p["skor_fulfillment_persen"], 100);
    let (_, saya) = kirim(
        &d.app,
        Method::GET,
        "/web/lapak-saya",
        Some(&d.prod_a),
        None,
    )
    .await;
    assert_eq!(saya["data"][0]["skor_fulfillment_persen"], 0);
}

#[sqlx::test]
async fn pembeli_batal_praktis_penuh_dan_pasca_konfirmasi_penalti_10_persen(db: PgPool) {
    let d = dunia(db.clone()).await;
    // 1) batal sebelum bayar: tanpa gerakan dana, stok kembali
    d.keranjang(d.va, 2).await;
    let id1 = d.pesan().await.1["pesanan_id"]
        .as_str()
        .unwrap()
        .to_string();
    let (s, j) = kirim(
        &d.app,
        Method::POST,
        &format!("/pesanan/{id1}/batalkan"),
        Some(&d.pembeli),
        None,
    )
    .await;
    assert_eq!(
        (s, j["status"].as_str()),
        (StatusCode::OK, Some("DIBATALKAN"))
    );
    assert_eq!(d.stok(d.va).await, 10);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM escrow")
            .fetch_one(&db)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        kirim(
            &d.app,
            Method::POST,
            &format!("/pesanan/{id1}/batalkan"),
            Some(&d.pembeli),
            None
        )
        .await
        .0,
        StatusCode::CONFLICT
    );

    // 2) batal setelah bayar sebelum konfirmasi: refund 100% (36000 + ongkir 8000 + layanan 2000)
    d.keranjang(d.va, 2).await;
    let id2 = d.pesan().await.1["pesanan_id"]
        .as_str()
        .unwrap()
        .to_string();
    d.bayar(&id2, KUNCI).await;
    kirim(
        &d.app,
        Method::POST,
        &format!("/pesanan/{id2}/batalkan"),
        Some(&d.pembeli),
        None,
    )
    .await;
    let r: i64 = sqlx::query_scalar("SELECT e.refund_idr FROM escrow e JOIN pesanan p ON p.id = e.pesanan_id WHERE p.status = 'DIBATALKAN' AND e.jumlah_idr = 46000").fetch_one(&db).await.unwrap();
    assert_eq!(r, 46000);

    // 3) batal setelah konfirmasi: subtotal - 10% (+ ongkir, karena belum diserahkan); biaya layanan tidak kembali
    d.keranjang(d.va, 2).await;
    let id3 = d.pesan().await.1["pesanan_id"]
        .as_str()
        .unwrap()
        .to_string();
    d.bayar(&id3, "22222222-2222-4222-8222-222222222222").await;
    let sa = d.sub_untuk(&id3, d.lapak_a).await;
    d.konfirmasi(&d.prod_a, sa, true).await;
    let (s, _) = kirim(
        &d.app,
        Method::POST,
        &format!("/pesanan/{id3}/batalkan"),
        Some(&d.pembeli),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let (refund, penalti): (i64, i64) =
        sqlx::query_as("SELECT refund_idr, penalti_idr FROM sub_pesanan WHERE id = $1")
            .bind(sa)
            .fetch_one(&db)
            .await
            .unwrap();
    assert_eq!((refund, penalti), (36000 - 3600 + 8000, 3600));

    // 4) setelah diserahkan ke trip tidak dapat dibatalkan pembeli
    d.keranjang(d.va, 1).await;
    let id4 = d.pesan().await.1["pesanan_id"]
        .as_str()
        .unwrap()
        .to_string();
    d.bayar(&id4, "33333333-3333-4333-8333-333333333333").await;
    let s4 = d.sub_untuk(&id4, d.lapak_a).await;
    d.konfirmasi(&d.prod_a, s4, true).await;
    d.serah(&d.prod_a, s4, "KODE-BENAR").await;
    assert_eq!(
        kirim(
            &d.app,
            Method::POST,
            &format!("/pesanan/{id4}/batalkan"),
            Some(&d.pembeli),
            None
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
}

#[sqlx::test]
async fn retur_dalam_24_jam_non_pangan_segar(db: PgPool) {
    let d = dunia(db.clone()).await;
    let id = dibayar(&d).await;
    let sa = d.sub_untuk(&id, d.lapak_a).await;
    let sb = d.sub_untuk(&id, d.lapak_b).await;
    for (tok, sub) in [(&d.prod_a, sa), (&d.prod_b, sb)] {
        d.konfirmasi(tok, sub, true).await;
        d.serah(tok, sub, "KODE-BENAR").await;
    }
    // sebelum diterima -> 409
    let retur = |sub: uuid::Uuid| json!({"sub_id": u(sub), "alasan": "kemasan rusak saat tiba"});
    assert_eq!(
        kirim(
            &d.app,
            Method::POST,
            &format!("/pesanan/{id}/retur"),
            Some(&d.pembeli),
            Some(retur(sa))
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    kirim(
        &d.app,
        Method::POST,
        &format!("/pesanan/{id}/konfirmasi-terima"),
        Some(&d.pembeli),
        None,
    )
    .await;
    // pangan segar sudah SELESAI -> tidak dapat diretur (409)
    assert_eq!(
        kirim(
            &d.app,
            Method::POST,
            &format!("/pesanan/{id}/retur"),
            Some(&d.pembeli),
            Some(retur(sb))
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    // lewat 24 jam -> MASA_RETUR_HABIS
    sqlx::query("UPDATE sub_pesanan SET diterima_pada = now() - interval '25 hours' WHERE id = $1")
        .bind(sa)
        .execute(&db)
        .await
        .unwrap();
    let (s, j) = kirim(
        &d.app,
        Method::POST,
        &format!("/pesanan/{id}/retur"),
        Some(&d.pembeli),
        Some(retur(sa)),
    )
    .await;
    assert_eq!(
        (s, j["kode"].as_str()),
        (StatusCode::CONFLICT, Some("MASA_RETUR_HABIS"))
    );
    // dalam 24 jam -> DIKEMBALIKAN, refund subtotal (ongkir tidak kembali)
    sqlx::query("UPDATE sub_pesanan SET diterima_pada = now() - interval '2 hours' WHERE id = $1")
        .bind(sa)
        .execute(&db)
        .await
        .unwrap();
    let (s, j) = kirim(
        &d.app,
        Method::POST,
        &format!("/pesanan/{id}/retur"),
        Some(&d.pembeli),
        Some(retur(sa)),
    )
    .await;
    assert_eq!(
        (s, j["status"].as_str()),
        (StatusCode::OK, Some("DIKEMBALIKAN"))
    );
    assert_eq!(d.status_sub(sa).await, "DIKEMBALIKAN");
    let r: i64 = sqlx::query_scalar("SELECT refund_idr FROM sub_pesanan WHERE id = $1")
        .bind(sa)
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(r, 36000);
    // alasan terlalu pendek -> 422
    assert_eq!(
        kirim(
            &d.app,
            Method::POST,
            &format!("/pesanan/{id}/retur"),
            Some(&d.pembeli),
            Some(json!({"sub_id": u(sa), "alasan": "x"}))
        )
        .await
        .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
}

#[sqlx::test]
async fn terima_otomatis_setelah_48_jam(db: PgPool) {
    let d = dunia(db.clone()).await;
    d.keranjang(d.vb, 1).await;
    let id = d.pesan().await.1["pesanan_id"]
        .as_str()
        .unwrap()
        .to_string();
    d.bayar(&id, KUNCI).await;
    let sb = d.sub_untuk(&id, d.lapak_b).await;
    d.konfirmasi(&d.prod_b, sb, true).await;
    d.serah(&d.prod_b, sb, "KODE-BENAR").await;
    assert_eq!(pasaree::pesanan::terima_otomatis(&db).await.unwrap(), 0);
    sqlx::query("UPDATE sub_pesanan SET diserahkan_pada = now() - interval '49 hours'")
        .execute(&db)
        .await
        .unwrap();
    assert_eq!(pasaree::pesanan::terima_otomatis(&db).await.unwrap(), 1);
    assert_eq!(d.status_induk(&id).await, "SELESAI");
}

async fn selesai_a(d: &Dunia) -> (String, uuid::Uuid) {
    d.keranjang(d.va, 1).await;
    let id = d.pesan().await.1["pesanan_id"]
        .as_str()
        .unwrap()
        .to_string();
    d.bayar(&id, KUNCI).await;
    let sa = d.sub_untuk(&id, d.lapak_a).await;
    d.konfirmasi(&d.prod_a, sa, true).await;
    d.serah(&d.prod_a, sa, "KODE-BENAR").await;
    kirim(
        &d.app,
        Method::POST,
        &format!("/pesanan/{id}/konfirmasi-terima"),
        Some(&d.pembeli),
        None,
    )
    .await;
    (id, sa)
}

#[sqlx::test]
async fn ulasan_aturan_dan_rating(db: PgPool) {
    let d = dunia(db.clone()).await;
    d.keranjang(d.va, 1).await;
    let id0 = d.pesan().await.1["pesanan_id"]
        .as_str()
        .unwrap()
        .to_string();
    let badan = |id: &str, bintang: i64| json!({"pesanan_id": id, "produk_id": u(d.produk_a), "bintang": bintang, "teks": "Enak sekali", "foto_url": []});
    // belum diterima -> 409
    assert_eq!(
        kirim(
            &d.app,
            Method::POST,
            "/ulasan",
            Some(&d.pembeli),
            Some(badan(&id0, 5))
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    kirim(
        &d.app,
        Method::POST,
        &format!("/pesanan/{id0}/batalkan"),
        Some(&d.pembeli),
        None,
    )
    .await;

    let (id, sa) = selesai_a(&d).await;
    assert_eq!(
        kirim(
            &d.app,
            Method::POST,
            "/ulasan",
            Some(&d.pembeli),
            Some(badan(&id, 6))
        )
        .await
        .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(
        kirim(
            &d.app,
            Method::POST,
            "/ulasan",
            Some(&d.pembeli),
            Some(badan(&id, 0))
        )
        .await
        .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let (s, _) = kirim(
        &d.app,
        Method::POST,
        "/ulasan",
        Some(&d.pembeli),
        Some(badan(&id, 4)),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);
    // rating produk diperbarui
    let (r, n): (f64, i32) =
        sqlx::query_as("SELECT rating_rata, total_ulasan FROM produk WHERE id = $1")
            .bind(d.produk_a)
            .fetch_one(&db)
            .await
            .unwrap();
    assert_eq!((r, n), (4.0, 1));
    let (_, l) = kirim(&d.app, Method::GET, "/katalog/produk", None, None).await;
    assert!(l["data"]
        .as_array()
        .unwrap()
        .iter()
        .any(|p| p["rating_rata"] == 4.0));
    // satu ulasan per produk per pesanan
    let (s, j) = kirim(
        &d.app,
        Method::POST,
        "/ulasan",
        Some(&d.pembeli),
        Some(badan(&id, 5)),
    )
    .await;
    assert_eq!(
        (s, j["kode"].as_str()),
        (StatusCode::CONFLICT, Some("ULASAN_SUDAH_ADA"))
    );
    // pembeli lain -> 404; produk yang tidak ada di pesanan -> 404
    let (_, lain) = seed_pengguna(&db, "lain@contoh.id", Peran::Pembeli).await;
    assert_eq!(
        kirim(
            &d.app,
            Method::POST,
            "/ulasan",
            Some(&lain),
            Some(badan(&id, 5))
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    let (s, _) = kirim(
        &d.app,
        Method::POST,
        "/ulasan",
        Some(&d.pembeli),
        Some(json!({"pesanan_id": id, "produk_id": u(d.produk_b), "bintang": 5})),
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    // lewat 7 hari setelah selesai -> 409 ULASAN_KEDALUWARSA
    let (id2, sa2) = selesai_a(&d).await;
    sqlx::query("UPDATE sub_pesanan SET diterima_pada = now() - interval '8 days', selesai_pada = now() - interval '8 days', status = 'SELESAI' WHERE id = $1").bind(sa2).execute(&db).await.unwrap();
    let (s, j) = kirim(
        &d.app,
        Method::POST,
        "/ulasan",
        Some(&d.pembeli),
        Some(badan(&id2, 5)),
    )
    .await;
    assert_eq!(
        (s, j["kode"].as_str()),
        (StatusCode::CONFLICT, Some("ULASAN_KEDALUWARSA"))
    );
    let _ = sa;
}

// ------------------------------------------------------------------ trip

#[sqlx::test]
async fn trip_cache_60_detik_dan_flag_basi(db: PgPool) {
    let d = dunia(db.clone()).await;
    let (s, j) = kirim(&d.app, Method::GET, "/trip", Some(&d.pembeli), None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(j["basi"], false);
    assert_eq!(j["data"][0]["trip_id"], "TRP-1");
    assert_eq!(j["data"][0]["ongkir_idr"], 8000);
    kirim(&d.app, Method::GET, "/trip", Some(&d.pembeli), None).await;
    assert_eq!(
        d.lumbung.hit_trip.load(Ordering::SeqCst),
        1,
        "panggilan kedua dilayani cache"
    );
    // parameter lain = kunci cache lain
    kirim(
        &d.app,
        Method::GET,
        "/trip?titik_id=TITIK-05",
        Some(&d.pembeli),
        None,
    )
    .await;
    assert_eq!(d.lumbung.hit_trip.load(Ordering::SeqCst), 2);
    assert_eq!(
        d.lumbung.salah_kunci.load(Ordering::SeqCst),
        0,
        "service key dipakai, bukan token pengguna"
    );
    // validasi + auth
    assert_eq!(
        kirim(&d.app, Method::GET, "/trip", None, None).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        kirim(
            &d.app,
            Method::GET,
            "/trip?dari=kemarin",
            Some(&d.pembeli),
            None
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
}

#[sqlx::test]
async fn trip_lumbung_mati_cache_terakhir_basi(db: PgPool) {
    // ttl 0: setiap panggilan mencoba Lumbung; bila gagal, cache terakhir dengan basi=true
    let d = dunia_dengan(db.clone(), |c| c.trip_cache_detik = 0).await;
    let (_, j) = kirim(&d.app, Method::GET, "/trip", Some(&d.pembeli), None).await;
    assert_eq!(j["basi"], false);
    d.lumbung.mati.store(true, Ordering::SeqCst);
    let (s, j) = kirim(&d.app, Method::GET, "/trip", Some(&d.pembeli), None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(j["basi"], true);
    assert_eq!(
        j["data"][0]["trip_id"], "TRP-1",
        "cache terakhir dikembalikan"
    );
    // tanpa cache sama sekali: daftar kosong + basi
    let (_, j) = kirim(
        &d.app,
        Method::GET,
        "/trip?titik_id=BARU",
        Some(&d.pembeli),
        None,
    )
    .await;
    assert_eq!(
        (j["basi"].as_bool(), j["data"].as_array().unwrap().len()),
        (Some(true), 0)
    );
    // pulih
    d.lumbung.mati.store(false, Ordering::SeqCst);
    let (_, j) = kirim(&d.app, Method::GET, "/trip", Some(&d.pembeli), None).await;
    assert_eq!(j["basi"], false);
}

#[sqlx::test]
async fn detail_produk_memuat_trip_tersedia(db: PgPool) {
    let d = dunia(db.clone()).await;
    let (s, j) = kirim(
        &d.app,
        Method::GET,
        &format!("/katalog/produk/{}", u(d.produk_a)),
        None,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(j["trip_tersedia"][0]["trip_id"], "TRP-1");
    assert_eq!(j["basi"], false);
    d.lumbung.trip.lock().unwrap().clear();
    // cache masih berlaku 60 detik; produk tetap terbaca meski Lumbung bermasalah
    d.lumbung.mati.store(true, Ordering::SeqCst);
    let (s, _) = kirim(
        &d.app,
        Method::GET,
        &format!("/katalog/produk/{}", u(d.produk_a)),
        None,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
}
