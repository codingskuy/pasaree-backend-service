mod common;

use axum::http::{Method, StatusCode};
use common::*;
use pasaree::auth::Peran;
use serde_json::json;
use sqlx::PgPool;
use std::sync::atomic::Ordering;

#[sqlx::test]
async fn keranjang_qty_stok_dan_hapus(db: PgPool) {
    let d = dunia(db.clone()).await;
    let (s, j) = d.keranjang(d.va, 2).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(j["subtotal_idr"], 36000);
    assert_eq!(j["item"][0]["harga_idr"], 18000);
    // ubah qty, tambah varian lain
    d.keranjang(d.va, 3).await;
    let (_, j) = d.keranjang(d.vb, 1).await;
    assert_eq!(j["subtotal_idr"], 3 * 18000 + 12000);
    assert_eq!(j["item"].as_array().unwrap().len(), 2);
    // melebihi stok -> 409 STOK_HABIS dengan detail
    let (s, j) = d.keranjang(d.va, 11).await;
    assert_eq!(
        (s, j["kode"].as_str()),
        (StatusCode::CONFLICT, Some("STOK_HABIS"))
    );
    assert_eq!(j["detail"]["stok_tersedia"], 10);
    // qty tidak valid
    assert_eq!(
        d.keranjang(d.va, 100).await.0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(
        d.keranjang(d.va, -1).await.0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    // qty 0 menghapus
    let (_, j) = d.keranjang(d.vb, 0).await;
    assert_eq!(j["item"].as_array().unwrap().len(), 1);
    // GET keranjang (sinkron akun)
    let (s, j) = kirim(&d.app, Method::GET, "/keranjang", Some(&d.pembeli), None).await;
    assert_eq!(
        (s, j["subtotal_idr"].as_i64()),
        (StatusCode::OK, Some(54000))
    );
    // varian tidak ada -> 404; tanpa token 401; produsen 403
    assert_eq!(
        d.keranjang(uuid::Uuid::now_v7(), 1).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        kirim(
            &d.app,
            Method::PUT,
            "/keranjang",
            None,
            Some(json!({"varian_id": u(d.va), "qty": 1}))
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        kirim(
            &d.app,
            Method::PUT,
            "/keranjang",
            Some(&d.prod_a),
            Some(json!({"varian_id": u(d.va), "qty": 1}))
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
}

#[sqlx::test]
async fn stok_nol_tidak_dapat_masuk_keranjang(db: PgPool) {
    let d = dunia(db.clone()).await;
    sqlx::query("UPDATE varian_produk SET stok = 0 WHERE id = $1")
        .bind(d.va)
        .execute(&db)
        .await
        .unwrap();
    let (s, j) = d.keranjang(d.va, 1).await;
    assert_eq!(
        (s, j["kode"].as_str()),
        (StatusCode::CONFLICT, Some("STOK_HABIS"))
    );
}

#[sqlx::test]
async fn pratinjau_hitung_server_komisi_tidak_dibebankan(db: PgPool) {
    let d = dunia(db.clone()).await;
    d.keranjang(d.va, 2).await;
    d.keranjang(d.vb, 1).await;
    let (s, j) = kirim(
        &d.app,
        Method::POST,
        "/checkout/pratinjau",
        Some(&d.pembeli),
        Some(json!({"trip_id": "TRP-1", "titik_id": "TITIK-05"})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{j}");
    assert_eq!(j["subtotal_idr"], 48000);
    assert_eq!(j["ongkir_idr"], 8000, "ongkir per trip, bukan per lapak");
    assert_eq!(j["biaya_layanan_idr"], 2000);
    assert_eq!(
        j["total_idr"], 58000,
        "komisi lapak tidak masuk total pembeli"
    );
    assert!(j["kunci_stok_hingga"].is_string());
    assert_eq!(j["peringatan"].as_array().unwrap().len(), 0);
    // pratinjau tidak mengurangi stok
    assert_eq!(d.stok(d.va).await, 10);
}

#[sqlx::test]
async fn pratinjau_validasi_trip_keranjang_stok_harga(db: PgPool) {
    // ttl cache 0 agar perubahan data Lumbung palsu langsung terbaca
    let d = dunia_dengan(db.clone(), |c| c.trip_cache_detik = 0).await;
    let req = |trip: &str| json!({"trip_id": trip, "titik_id": "TITIK-05"});
    // keranjang kosong
    assert_eq!(
        kirim(
            &d.app,
            Method::POST,
            "/checkout/pratinjau",
            Some(&d.pembeli),
            Some(req("TRP-1"))
        )
        .await
        .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    d.keranjang(d.va, 2).await;
    // trip tidak dikenal Lumbung -> diblokir
    let (s, j) = kirim(
        &d.app,
        Method::POST,
        "/checkout/pratinjau",
        Some(&d.pembeli),
        Some(req("TRP-TIDAK-ADA")),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(j["detail"]["kesalahan"][0]["kolom"], "trip_id");
    // trip penuh
    d.lumbung.trip.lock().unwrap()[0]["sisa_kapasitas"] = json!(0);
    let (s, j) = kirim(
        &d.app,
        Method::POST,
        "/checkout/pratinjau",
        Some(&d.pembeli),
        Some(req("TRP-1")),
    )
    .await;
    assert_eq!(
        (s, j["kode"].as_str()),
        (StatusCode::CONFLICT, Some("TRIP_PENUH"))
    );
    d.lumbung.trip.lock().unwrap()[0]["sisa_kapasitas"] = json!(5);

    // stok berubah setelah masuk keranjang -> 409 STOK_HABIS
    sqlx::query("UPDATE varian_produk SET stok = 1 WHERE id = $1")
        .bind(d.va)
        .execute(&db)
        .await
        .unwrap();
    let (s, j) = kirim(
        &d.app,
        Method::POST,
        "/checkout/pratinjau",
        Some(&d.pembeli),
        Some(req("TRP-1")),
    )
    .await;
    assert_eq!(
        (s, j["kode"].as_str()),
        (StatusCode::CONFLICT, Some("STOK_HABIS"))
    );
    sqlx::query("UPDATE varian_produk SET stok = 10 WHERE id = $1")
        .bind(d.va)
        .execute(&db)
        .await
        .unwrap();

    // harga berubah -> 409 HARGA_BERUBAH dengan angka baru; percobaan berikutnya lolos dengan harga baru
    sqlx::query("UPDATE varian_produk SET harga_idr = 20000 WHERE id = $1")
        .bind(d.va)
        .execute(&db)
        .await
        .unwrap();
    let (s, j) = kirim(
        &d.app,
        Method::POST,
        "/checkout/pratinjau",
        Some(&d.pembeli),
        Some(req("TRP-1")),
    )
    .await;
    assert_eq!(
        (s, j["kode"].as_str()),
        (StatusCode::CONFLICT, Some("HARGA_BERUBAH"))
    );
    assert_eq!(j["detail"]["varian"][0]["harga_baru_idr"], 20000);
    assert_eq!(j["detail"]["varian"][0]["harga_lama_idr"], 18000);
    let (s, j) = kirim(
        &d.app,
        Method::POST,
        "/checkout/pratinjau",
        Some(&d.pembeli),
        Some(req("TRP-1")),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(j["subtotal_idr"], 40000);

    // produk diturunkan -> 409 PRODUK_TIDAK_TERSEDIA
    sqlx::query("UPDATE produk SET status = 'TAKEDOWN' WHERE id = $1")
        .bind(d.produk_a)
        .execute(&db)
        .await
        .unwrap();
    let (s, j) = kirim(
        &d.app,
        Method::POST,
        "/checkout/pratinjau",
        Some(&d.pembeli),
        Some(req("TRP-1")),
    )
    .await;
    assert_eq!(
        (s, j["kode"].as_str()),
        (StatusCode::CONFLICT, Some("PRODUK_TIDAK_TERSEDIA"))
    );
}

#[sqlx::test]
async fn trip_sudah_berangkat_diblokir(db: PgPool) {
    let d = dunia_dengan(db.clone(), |c| c.trip_cache_detik = 0).await;
    d.keranjang(d.va, 1).await;
    d.lumbung.trip.lock().unwrap()[0]["jadwal_berangkat"] =
        json!((chrono::Utc::now() - chrono::Duration::hours(1)).to_rfc3339());
    let (s, j) = kirim(
        &d.app,
        Method::POST,
        "/checkout/pratinjau",
        Some(&d.pembeli),
        Some(json!({"trip_id": "TRP-1", "titik_id": "TITIK-05"})),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{j}");
}

#[sqlx::test]
async fn lumbung_mati_tanpa_cache_memblokir_checkout(db: PgPool) {
    let d = dunia(db.clone()).await;
    d.keranjang(d.va, 1).await;
    d.lumbung.mati.store(true, Ordering::SeqCst);
    let (s, _) = kirim(
        &d.app,
        Method::POST,
        "/checkout/pratinjau",
        Some(&d.pembeli),
        Some(json!({"trip_id": "TRP-1", "titik_id": "TITIK-05"})),
    )
    .await;
    assert_eq!(
        s,
        StatusCode::UNPROCESSABLE_ENTITY,
        "tanpa trip valid, checkout diblokir"
    );
    assert_eq!(d.pesan().await.0, StatusCode::UNPROCESSABLE_ENTITY);
}

#[sqlx::test]
async fn buat_pesanan_multi_lapak_kunci_stok_30_menit(db: PgPool) {
    let d = dunia(db.clone()).await;
    d.keranjang(d.va, 2).await;
    d.keranjang(d.vb, 1).await;
    let sebelum = chrono::Utc::now();
    let (s, j) = d.pesan().await;
    assert_eq!(s, StatusCode::CREATED, "{j}");
    let id = j["pesanan_id"].as_str().unwrap();
    assert!(id.starts_with("PSR-") && id.len() == 4 + 26);
    assert_eq!(j["total_idr"], 58000);
    let batas: chrono::DateTime<chrono::Utc> =
        j["batas_bayar_hingga"].as_str().unwrap().parse().unwrap();
    let menit = (batas - sebelum).num_seconds() as f64 / 60.0;
    assert!(
        (29.9..=30.2).contains(&menit),
        "batas bayar 30 menit, dapat {menit}"
    );

    // stok terkunci, keranjang kosong, dua sub-pesanan
    assert_eq!(d.stok(d.va).await, 8);
    assert_eq!(d.stok(d.vb).await, 9);
    assert_eq!(d.sub_ids(id).await.len(), 2);
    let (_, k) = kirim(&d.app, Method::GET, "/keranjang", Some(&d.pembeli), None).await;
    assert_eq!(k["item"].as_array().unwrap().len(), 0);
    // pesanan kedua dari keranjang kosong -> 422
    assert_eq!(d.pesan().await.0, StatusCode::UNPROCESSABLE_ENTITY);

    let (s, det) = kirim(
        &d.app,
        Method::GET,
        &format!("/pesanan/{id}"),
        Some(&d.pembeli),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(det["status"], "DIPESAN");
    assert_eq!(det["sub_pesanan"].as_array().unwrap().len(), 2);
    assert_eq!(det["timeline"][0]["status"], "DIPESAN");
    assert!(det["escrow_id"].is_null());
    assert_eq!(det["aksi_tersedia"].as_array().unwrap().len(), 0);
}

#[sqlx::test]
async fn catatan_per_lapak_divalidasi_dan_disimpan(db: PgPool) {
    let d = dunia(db.clone()).await;
    d.keranjang(d.va, 1).await;
    let badan = |k: &str| json!({"trip_id": "TRP-1", "titik_id": "TITIK-05", "catatan_per_lapak": {k: "tanpa bawang putih"}});
    let (s, _) = kirim(
        &d.app,
        Method::POST,
        "/pesanan",
        Some(&d.pembeli),
        Some(badan(&u(d.lapak_b))),
    )
    .await;
    assert_eq!(
        s,
        StatusCode::UNPROCESSABLE_ENTITY,
        "lapak tidak ada di keranjang"
    );
    let (s, _) = kirim(
        &d.app,
        Method::POST,
        "/pesanan",
        Some(&d.pembeli),
        Some(badan(&u(d.lapak_a))),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);
    let c: String = sqlx::query_scalar("SELECT catatan FROM sub_pesanan")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(c, "tanpa bawang putih");
}

#[sqlx::test]
async fn stok_nol_setelah_dipesan_menonaktifkan_lalu_kembali_saat_batal(db: PgPool) {
    let d = dunia(db.clone()).await;
    d.keranjang(d.va, 10).await;
    let (s, j) = d.pesan().await;
    assert_eq!(s, StatusCode::CREATED);
    let id = j["pesanan_id"].as_str().unwrap().to_string();
    let st: String = sqlx::query_scalar("SELECT status FROM produk WHERE id = $1")
        .bind(d.produk_a)
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(st, "NONAKTIF_STOK_0");
    // lewat batas bayar -> DIBATALKAN, stok kembali, produk tayang lagi
    sqlx::query("UPDATE pesanan SET batas_bayar_hingga = now() - interval '1 minute'")
        .execute(&db)
        .await
        .unwrap();
    assert_eq!(
        pasaree::pesanan::batalkan_kedaluwarsa(&db).await.unwrap(),
        1
    );
    assert_eq!(d.status_induk(&id).await, "DIBATALKAN");
    assert_eq!(d.stok(d.va).await, 10);
    let st: String = sqlx::query_scalar("SELECT status FROM produk WHERE id = $1")
        .bind(d.produk_a)
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(st, "TAYANG");
    // idempoten: tugas kedua tidak memproses ulang
    assert_eq!(
        pasaree::pesanan::batalkan_kedaluwarsa(&db).await.unwrap(),
        0
    );
    assert_eq!(d.stok(d.va).await, 10);
    // timeline mencatat pembatalan
    let (_, det) = kirim(
        &d.app,
        Method::GET,
        &format!("/pesanan/{id}"),
        Some(&d.pembeli),
        None,
    )
    .await;
    assert_eq!(
        det["timeline"].as_array().unwrap().last().unwrap()["status"],
        "DIBATALKAN"
    );
    // tidak dapat dibayar setelah batal
    assert_eq!(
        d.bayar(&id, "6f1c1a52-5b40-4a53-a9a3-0f3b8e4f9c11").await.0,
        StatusCode::CONFLICT
    );
}

#[sqlx::test]
async fn bayar_idempoten_tanpa_tagihan_ganda(db: PgPool) {
    let d = dunia(db.clone()).await;
    let id = d.pesanan_dua_lapak().await;
    let kunci = "6f1c1a52-5b40-4a53-a9a3-0f3b8e4f9c11";
    let (s1, j1) = d.bayar(&id, kunci).await;
    assert_eq!(s1, StatusCode::OK, "{j1}");
    assert_eq!(j1["status"], "DIBAYAR");
    let escrow = j1["escrow_id"].as_str().unwrap().to_string();
    assert!(escrow.starts_with("ESC-"));
    // kirim ulang dengan kunci sama: respons identik, tanpa tagihan ganda
    let (s2, j2) = d.bayar(&id, kunci).await;
    assert_eq!((s2, &j2), (StatusCode::OK, &j1));
    let n: (i64, i64) =
        sqlx::query_as("SELECT (SELECT count(*) FROM pembayaran), (SELECT count(*) FROM escrow)")
            .fetch_one(&db)
            .await
            .unwrap();
    assert_eq!(n, (1, 1));
    let jumlah: i64 = sqlx::query_scalar("SELECT jumlah_idr FROM escrow")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(jumlah, 58000);
    // kunci berbeda setelah dibayar -> 409 (bukan tagihan baru)
    let (s, j) = d.bayar(&id, "11111111-1111-4111-8111-111111111111").await;
    assert_eq!(
        (s, j["kode"].as_str()),
        (StatusCode::CONFLICT, Some("STATUS_TIDAK_VALID"))
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM pembayaran")
            .fetch_one(&db)
            .await
            .unwrap(),
        1
    );

    // status induk dan sub DIBAYAR; escrow_id tampil; batas konfirmasi terisi
    assert_eq!(d.status_induk(&id).await, "DIBAYAR");
    for (sub, _) in d.sub_ids(&id).await {
        assert_eq!(d.status_sub(sub).await, "DIBAYAR");
    }
    let batas: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM sub_pesanan WHERE batas_konfirmasi_hingga IS NOT NULL",
    )
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(batas, 2);
    let (_, det) = kirim(
        &d.app,
        Method::GET,
        &format!("/pesanan/{id}"),
        Some(&d.pembeli),
        None,
    )
    .await;
    assert_eq!(det["escrow_id"], escrow);
}

#[sqlx::test]
async fn bayar_serentak_kunci_sama_hanya_satu_pembayaran(db: PgPool) {
    let d = dunia(db.clone()).await;
    let id = d.pesanan_dua_lapak().await;
    let kunci = "6f1c1a52-5b40-4a53-a9a3-0f3b8e4f9c11";
    let (a, b, c) = tokio::join!(
        d.bayar(&id, kunci),
        d.bayar(&id, kunci),
        d.bayar(&id, kunci)
    );
    assert_eq!(a.0, StatusCode::OK);
    assert_eq!(b, a);
    assert_eq!(c, a);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM pembayaran")
            .fetch_one(&db)
            .await
            .unwrap(),
        1
    );
}

#[sqlx::test]
async fn bayar_validasi_kepemilikan_dan_batas_waktu(db: PgPool) {
    let d = dunia(db.clone()).await;
    let id = d.pesanan_dua_lapak().await;
    let (_, lain) = seed_pengguna(&db, "lain@contoh.id", Peran::Pembeli).await;
    let badan =
        json!({"metode": "QRIS", "idempotency_key": "6f1c1a52-5b40-4a53-a9a3-0f3b8e4f9c11"});
    assert_eq!(
        kirim(
            &d.app,
            Method::POST,
            &format!("/pesanan/{id}/bayar"),
            Some(&lain),
            Some(badan.clone())
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        kirim(
            &d.app,
            Method::GET,
            &format!("/pesanan/{id}"),
            Some(&lain),
            None
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        kirim(
            &d.app,
            Method::POST,
            &format!("/pesanan/{id}/bayar"),
            None,
            Some(badan)
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    // kunci bukan uuid / metode tak dikenal -> 422
    assert_eq!(
        d.bayar(&id, "bukan-uuid").await.0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let (s, _) = kirim(
        &d.app,
        Method::POST,
        &format!("/pesanan/{id}/bayar"),
        Some(&d.pembeli),
        Some(json!({"metode": "CASH", "idempotency_key": "6f1c1a52-5b40-4a53-a9a3-0f3b8e4f9c11"})),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        d.bayar("PSR-TIDAKVALID", "6f1c1a52-5b40-4a53-a9a3-0f3b8e4f9c11")
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    // lewat batas bayar tetapi tugas belum jalan -> 409 PESANAN_KEDALUWARSA
    sqlx::query("UPDATE pesanan SET batas_bayar_hingga = now() - interval '1 second'")
        .execute(&db)
        .await
        .unwrap();
    let (s, j) = d.bayar(&id, "6f1c1a52-5b40-4a53-a9a3-0f3b8e4f9c11").await;
    assert_eq!(
        (s, j["kode"].as_str()),
        (StatusCode::CONFLICT, Some("PESANAN_KEDALUWARSA"))
    );
}

#[sqlx::test]
async fn daftar_pesanan_filter_dan_paginasi(db: PgPool) {
    let d = dunia(db.clone()).await;
    let mut ids = Vec::new();
    for _ in 0..3 {
        d.keranjang(d.va, 1).await;
        let (_, j) = d.pesan().await;
        ids.push(j["pesanan_id"].as_str().unwrap().to_string());
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    }
    d.bayar(&ids[0], "6f1c1a52-5b40-4a53-a9a3-0f3b8e4f9c11")
        .await;
    let (s, j) = kirim(
        &d.app,
        Method::GET,
        "/pesanan?limit=2",
        Some(&d.pembeli),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(j["data"].as_array().unwrap().len(), 2);
    assert_eq!(j["data"][0]["id"], ids[2], "terbaru dulu");
    let c = j["cursor_lanjut"].as_str().unwrap();
    let (_, j2) = kirim(
        &d.app,
        Method::GET,
        &format!("/pesanan?limit=2&cursor={c}"),
        Some(&d.pembeli),
        None,
    )
    .await;
    assert_eq!(j2["data"].as_array().unwrap().len(), 1);
    assert!(j2["cursor_lanjut"].is_null());
    let (_, j3) = kirim(
        &d.app,
        Method::GET,
        "/pesanan?status=DIBAYAR",
        Some(&d.pembeli),
        None,
    )
    .await;
    assert_eq!(j3["data"].as_array().unwrap().len(), 1);
    assert_eq!(
        kirim(
            &d.app,
            Method::GET,
            "/pesanan?status=ASAL",
            Some(&d.pembeli),
            None
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        kirim(
            &d.app,
            Method::GET,
            "/pesanan?limit=51",
            Some(&d.pembeli),
            None
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    // pembeli lain tidak melihat pesanan ini
    let (_, lain) = seed_pengguna(&db, "lain@contoh.id", Peran::Pembeli).await;
    let (_, j4) = kirim(&d.app, Method::GET, "/pesanan", Some(&lain), None).await;
    assert_eq!(j4["data"].as_array().unwrap().len(), 0);
}
