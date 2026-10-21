mod common;

use axum::http::{Method, StatusCode};
use common::*;
use pasaree::auth::Peran;
use serde_json::{json, Value};
use sqlx::PgPool;

fn lapak_payload(slug: &str, nama: &str) -> Value {
    json!({
        "nama_produsen": "Mbok D", "jenis": "UMKM", "kontak_wa": "+6281234567890",
        "nama_lapak": nama, "slug": slug,
        "deskripsi": "Sambal dan bumbu rumahan buatan Mbok D, dibuat setiap pagi dengan bahan segar dari pasar lokal.",
        "titik_serah_lumbung_id": "TITIK-05", "rekening_payout": "1234567890"
    })
}

#[sqlx::test]
async fn produsen_membuat_lapak_dan_batasannya(db: PgPool) {
    let app = app(db.clone());
    let (_, produsen) = seed_pengguna(&db, "mbokd@contoh.id", Peran::Produsen).await;
    let (_, pembeli) = seed_pengguna(&db, "pembeli@contoh.id", Peran::Pembeli).await;

    let (s, j) = kirim(
        &app,
        Method::POST,
        "/web/lapak",
        Some(&produsen),
        Some(lapak_payload("mbok-d", "Mbok D")),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{j}");
    assert_eq!(j["slug"], "mbok-d");
    assert_eq!(j["status"], "AKTIF");
    assert_eq!(j["skor_fulfillment_persen"], 100);
    assert!(
        !j.to_string().contains("1234567890"),
        "nomor rekening tidak boleh bocor"
    );
    let tersensor: String = sqlx::query_scalar("SELECT rekening_payout_tersensor FROM lapak")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(tersensor, "****7890");

    // satu produsen = satu lapak
    let (s, j) = kirim(
        &app,
        Method::POST,
        "/web/lapak",
        Some(&produsen),
        Some(lapak_payload("mbok-d-2", "Mbok D Dua")),
    )
    .await;
    assert_eq!(
        (s, j["kode"].as_str()),
        (StatusCode::CONFLICT, Some("LAPAK_SUDAH_ADA"))
    );
    // slug dipakai produsen lain
    let (_, p2) = seed_pengguna(&db, "p2@contoh.id", Peran::Produsen).await;
    let (s, j) = kirim(
        &app,
        Method::POST,
        "/web/lapak",
        Some(&p2),
        Some(lapak_payload("mbok-d", "Nama Lain")),
    )
    .await;
    assert_eq!(
        (s, j["kode"].as_str()),
        (StatusCode::CONFLICT, Some("SLUG_SUDAH_DIPAKAI"))
    );
    // validasi
    let mut bad = lapak_payload("Slug Salah!", "Mbok E");
    bad["kontak_wa"] = json!("08123");
    let (s, j) = kirim(&app, Method::POST, "/web/lapak", Some(&p2), Some(bad)).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(j["detail"]["kesalahan"].as_array().unwrap().len() >= 2);
    // peran
    assert_eq!(
        kirim(
            &app,
            Method::POST,
            "/web/lapak",
            Some(&pembeli),
            Some(lapak_payload("x-yz", "Xyz"))
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        kirim(
            &app,
            Method::POST,
            "/web/lapak",
            None,
            Some(lapak_payload("x-yz", "Xyz"))
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
}

#[sqlx::test]
async fn lapak_saya_ubah_profil_dan_operator(db: PgPool) {
    let app = app(db.clone());
    let (pid, produsen) = seed_pengguna(&db, "a@contoh.id", Peran::Produsen).await;
    let lapak = seed_lapak(&db, pid, "lapak-a").await;
    let (_, lain) = seed_pengguna(&db, "b@contoh.id", Peran::Produsen).await;
    let (_, operator) = seed_pengguna(&db, "op@contoh.id", Peran::Operator).await;

    let (s, j) = kirim(&app, Method::GET, "/web/lapak-saya", Some(&produsen), None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(j["data"][0]["slug"], "lapak-a");
    assert_eq!(j["data"][0]["saldo_tertahan_idr"], 0);
    assert_eq!(
        kirim(&app, Method::GET, "/web/lapak-saya", Some(&lain), None)
            .await
            .1["data"]
            .as_array()
            .unwrap()
            .len(),
        0
    );

    let uri = format!("/web/lapak/{}", u(lapak));
    let (s, j) = kirim(
        &app,
        Method::PATCH,
        &uri,
        Some(&produsen),
        Some(json!({"nama_lapak": "Lapak A Baru"})),
    )
    .await;
    assert_eq!(
        (s, j["nama_lapak"].as_str()),
        (StatusCode::OK, Some("Lapak A Baru"))
    );
    // slug tidak dapat diubah
    assert_eq!(
        kirim(
            &app,
            Method::PATCH,
            &uri,
            Some(&produsen),
            Some(json!({"slug": "baru-baru"}))
        )
        .await
        .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    // produsen lain tidak melihat lapak ini
    assert_eq!(
        kirim(
            &app,
            Method::PATCH,
            &uri,
            Some(&lain),
            Some(json!({"nama_lapak": "Curi"}))
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );

    // operator ditambahkan produsen; operator tidak boleh ubah profil
    let (s, _) = kirim(
        &app,
        Method::POST,
        &format!("{uri}/operator"),
        Some(&produsen),
        Some(json!({"email": "op@contoh.id"})),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);
    assert_eq!(
        kirim(
            &app,
            Method::PATCH,
            &uri,
            Some(&operator),
            Some(json!({"nama_lapak": "Op Ubah"}))
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let (_, j) = kirim(&app, Method::GET, "/web/lapak-saya", Some(&operator), None).await;
    assert_eq!(j["data"][0]["slug"], "lapak-a");
    // email non-operator ditolak
    let (s, _) = kirim(
        &app,
        Method::POST,
        &format!("{uri}/operator"),
        Some(&produsen),
        Some(json!({"email": "b@contoh.id"})),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
}

fn draf(lapak: &str, judul: &str) -> Value {
    json!({
        "lapak_id": lapak, "judul": judul, "kategori_kode": "bumbu-dapur", "tag": ["pedas"],
        "deskripsi": "Sambal bawang buatan rumahan, pedas gurih, tanpa pengawet, dikemas higienis dalam botol kaca.",
    })
}

#[sqlx::test]
async fn draf_idempoten_validasi_dan_duplikat(db: PgPool) {
    let app = app(db.clone());
    let (pid, produsen) = seed_pengguna(&db, "a@contoh.id", Peran::Produsen).await;
    let lapak = u(seed_lapak(&db, pid, "lapak-a").await);
    let (_, lain) = seed_pengguna(&db, "b@contoh.id", Peran::Produsen).await;

    let kunci = "6f1c1a52-5b40-4a53-a9a3-0f3b8e4f9c11";
    let (s, j1) = kirim_dengan_header(
        &app,
        Method::POST,
        "/web/produk/draf",
        Some(&produsen),
        &[("idempotency-key", kunci)],
        Some(draf(&lapak, "Sambal Bawang Mbok D — 150 g")),
    )
    .await;
    assert_eq!(
        (s, j1["status"].as_str()),
        (StatusCode::CREATED, Some("DRAF"))
    );
    let (s, j2) = kirim_dengan_header(
        &app,
        Method::POST,
        "/web/produk/draf",
        Some(&produsen),
        &[("idempotency-key", kunci)],
        Some(draf(&lapak, "Sambal Bawang Mbok D — 150 g")),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);
    assert_eq!(j1, j2, "kunci sama = respons identik");
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM produk")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(n, 1);

    // judul terlalu pendek -> 422; kemiripan >= 85% -> 422
    let (s, _) = kirim(
        &app,
        Method::POST,
        "/web/produk/draf",
        Some(&produsen),
        Some(draf(&lapak, "Sambal")),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    let (s, j) = kirim(
        &app,
        Method::POST,
        "/web/produk/draf",
        Some(&produsen),
        Some(draf(&lapak, "Sambal Bawang Mbok D — 150 gr")),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(j["detail"]["kesalahan"][0]["kolom"], "judul");
    // Idempotency-Key bukan UUID -> 400
    let (s, _) = kirim_dengan_header(
        &app,
        Method::POST,
        "/web/produk/draf",
        Some(&produsen),
        &[("idempotency-key", "bukan-uuid")],
        Some(draf(&lapak, "Keripik Singkong Pedas 200 g")),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    // produsen lain tidak boleh membuat draf di lapak ini
    let (s, _) = kirim(
        &app,
        Method::POST,
        "/web/produk/draf",
        Some(&lain),
        Some(draf(&lapak, "Keripik Singkong Pedas 200 g")),
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

fn varian(sku: &str, harga: i64, stok: i64) -> Value {
    json!({"nama_varian": "Original", "sku": sku, "harga_idr": harga, "stok": stok, "satuan": "g", "berat_kirim_gram": 200,
           "tanggal_produksi": "2026-10-01", "kedaluwarsa": "2027-06-01"})
}

fn foto(url: &str, lebar: i64) -> Value {
    json!({"url": url, "jenis": "FOTO", "utama": true, "lebar": lebar, "tinggi": lebar, "ukuran_byte": 800000})
}

#[sqlx::test]
async fn alur_draf_ubah_cek_ajukan(db: PgPool) {
    let app = app(db.clone());
    let (pid, produsen) = seed_pengguna(&db, "a@contoh.id", Peran::Produsen).await;
    let lapak = u(seed_lapak(&db, pid, "lapak-a").await);
    let (_, j) = kirim(
        &app,
        Method::POST,
        "/web/produk/draf",
        Some(&produsen),
        Some(draf(&lapak, "Sambal Bawang Mbok D — 150 g")),
    )
    .await;
    let id = j["produk_id"].as_str().unwrap().to_string();
    let uri = format!("/web/produk/{id}");

    // ajukan sebelum lengkap -> 422 dengan pesan per kolom
    let (s, j) = kirim(
        &app,
        Method::POST,
        &format!("{uri}/ajukan-kurasi"),
        Some(&produsen),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    let kolom: Vec<&str> = j["detail"]["kesalahan"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["kolom"].as_str().unwrap())
        .collect();
    assert!(
        kolom.contains(&"varian") && kolom.contains(&"media"),
        "{kolom:?}"
    );

    // harga bukan kelipatan 500 -> 422 saat simpan
    let (s, _) = kirim(
        &app,
        Method::PUT,
        &uri,
        Some(&produsen),
        Some(json!({"varian": [varian("LPK-ABCD-0001", 18200, 5)]})),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    // SKU salah format -> 422
    let (s, _) = kirim(
        &app,
        Method::PUT,
        &uri,
        Some(&produsen),
        Some(json!({"varian": [varian("sku-salah", 18000, 5)]})),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);

    // foto 799px lolos simpan tetapi gagal cek otomatis
    let (s, j) = kirim(
        &app, Method::PUT, &uri, Some(&produsen),
        Some(json!({"komposisi": "Bawang, cabai, garam", "alergen": [], "varian": [varian("LPK-ABCD-0001", 18000, 5)], "media": [foto("https://cdn.uji/a.jpg", 799)]})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{j}");
    assert_eq!(j["cek_otomatis"]["lolos"], false);
    assert!(j["cek_otomatis"]["kesalahan"]
        .to_string()
        .contains("800x800"));

    let (s, j) = kirim(
        &app,
        Method::PUT,
        &uri,
        Some(&produsen),
        Some(json!({"media": [foto("https://cdn.uji/a.jpg", 1200)]})),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(j["cek_otomatis"]["lolos"], true, "{j}");

    let (s, j) = kirim(
        &app,
        Method::POST,
        &format!("{uri}/ajukan-kurasi"),
        Some(&produsen),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{j}");
    assert_eq!(j["status"], "MENUNGGU_KURASI");
    assert!(j["estimasi_sla"].is_string());
    // sedang dikurasi: tidak dapat diubah / diajukan lagi
    assert_eq!(
        kirim(
            &app,
            Method::PUT,
            &uri,
            Some(&produsen),
            Some(json!({"deskripsi": "x".repeat(60)}))
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        kirim(
            &app,
            Method::POST,
            &format!("{uri}/ajukan-kurasi"),
            Some(&produsen),
            None
        )
        .await
        .0,
        StatusCode::CONFLICT
    );

    // daftar web + detail
    let (_, j) = kirim(
        &app,
        Method::GET,
        "/web/produk?status=MENUNGGU_KURASI",
        Some(&produsen),
        None,
    )
    .await;
    assert_eq!(j["data"][0]["id"], id);
    let (s, j) = kirim(&app, Method::GET, &uri, Some(&produsen), None).await;
    assert_eq!(
        (s, j["varian"][0]["sku"].as_str()),
        (StatusCode::OK, Some("LPK-ABCD-0001"))
    );
    // riwayat harga awal tercatat
    let vid = j["varian"][0]["id"].as_str().unwrap();
    let (_, r) = kirim(
        &app,
        Method::GET,
        &format!("/web/varian/{vid}/harga-riwayat"),
        Some(&produsen),
        None,
    )
    .await;
    assert_eq!(r["data"][0]["harga_baru_idr"], 18000);
    assert!(r["data"][0]["harga_lama_idr"].is_null());
}

#[sqlx::test]
async fn sku_ganda_409(db: PgPool) {
    let app = app(db.clone());
    let (pid, produsen) = seed_pengguna(&db, "a@contoh.id", Peran::Produsen).await;
    let lapak = u(seed_lapak(&db, pid, "lapak-a").await);
    let (_, j) = kirim(
        &app,
        Method::POST,
        "/web/produk/draf",
        Some(&produsen),
        Some(draf(&lapak, "Sambal Bawang Mbok D — 150 g")),
    )
    .await;
    let uri = format!("/web/produk/{}", j["produk_id"].as_str().unwrap());
    let (s, _) = kirim(&app, Method::PUT, &uri, Some(&produsen), Some(json!({"varian": [varian("LPK-ABCD-0001", 18000, 5), varian("LPK-ABCD-0001", 20000, 5)]}))).await;
    assert_eq!(s, StatusCode::CONFLICT);
}

#[sqlx::test]
async fn stok_nol_menonaktifkan_dan_kembali_tayang(db: PgPool) {
    let app = app(db.clone());
    let (pid, produsen) = seed_pengguna(&db, "a@contoh.id", Peran::Produsen).await;
    let (_, lain) = seed_pengguna(&db, "b@contoh.id", Peran::Produsen).await;
    let lapak = seed_lapak(&db, pid, "lapak-a").await;
    let (produk, v) = seed_produk(
        &db,
        lapak,
        "Sambal Bawang Mbok D — 150 g",
        "bumbu-dapur",
        18000,
        3,
        "TAYANG",
    )
    .await;
    let uri = format!("/web/varian/{}/stok", u(v));

    let (s, j) = kirim(
        &app,
        Method::PATCH,
        &uri,
        Some(&produsen),
        Some(json!({"stok": 0, "alasan": "habis terjual"})),
    )
    .await;
    assert_eq!(
        (s, j["status_produk"].as_str()),
        (StatusCode::OK, Some("NONAKTIF_STOK_0"))
    );
    // hilang dari daftar katalog, detail tetap terbaca dengan stok 0
    let (_, l) = kirim(&app, Method::GET, "/katalog/produk", None, None).await;
    assert_eq!(l["data"].as_array().unwrap().len(), 0);
    let (s, d) = kirim(
        &app,
        Method::GET,
        &format!("/katalog/produk/{}", u(produk)),
        None,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(d["stok_per_varian"][u(v)], 0);

    let (s, j) = kirim(
        &app,
        Method::PATCH,
        &uri,
        Some(&produsen),
        Some(json!({"stok": 7, "alasan": "produksi baru"})),
    )
    .await;
    assert_eq!(
        (s, j["status_produk"].as_str()),
        (StatusCode::OK, Some("TAYANG"))
    );
    let (_, l) = kirim(&app, Method::GET, "/katalog/produk", None, None).await;
    assert_eq!(l["data"].as_array().unwrap().len(), 1);
    // stok real-time
    let (_, d) = kirim(
        &app,
        Method::GET,
        &format!("/katalog/produk/{}", u(produk)),
        None,
        None,
    )
    .await;
    assert_eq!(d["stok_per_varian"][u(v)], 7);

    assert_eq!(
        kirim(
            &app,
            Method::PATCH,
            &uri,
            Some(&produsen),
            Some(json!({"stok": -1, "alasan": "x"}))
        )
        .await
        .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(
        kirim(
            &app,
            Method::PATCH,
            &uri,
            Some(&lain),
            Some(json!({"stok": 1, "alasan": "x"}))
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        kirim(
            &app,
            Method::PATCH,
            &uri,
            None,
            Some(json!({"stok": 1, "alasan": "x"}))
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
}

#[sqlx::test]
async fn harga_riwayat_append_only_dan_kurasi_ulang_20_persen(db: PgPool) {
    let app = app(db.clone());
    let (pid, produsen) = seed_pengguna(&db, "a@contoh.id", Peran::Produsen).await;
    let lapak = seed_lapak(&db, pid, "lapak-a").await;
    let (_, v) = seed_produk(
        &db,
        lapak,
        "Sambal Bawang Mbok D — 150 g",
        "bumbu-dapur",
        10000,
        3,
        "TAYANG",
    )
    .await;
    let uri = format!("/web/varian/{}/harga", u(v));

    // tepat 20% (10000 -> 12000) tidak memicu kurasi ulang
    let (s, j) = kirim(
        &app,
        Method::PATCH,
        &uri,
        Some(&produsen),
        Some(json!({"harga_idr": 12000})),
    )
    .await;
    assert_eq!(
        (s, j["kurasi_ulang_ringan"].as_bool()),
        (StatusCode::OK, Some(false))
    );
    let (_, l) = kirim(&app, Method::GET, "/katalog/produk", None, None).await;
    assert_eq!(l["data"][0]["harga_mulai_idr"], 12000, "harga real-time");
    assert_eq!(l["harga_berubah"], true);

    // > 20% (12000 -> 15000 = 25%) memicu kurasi ulang ringan dan produk keluar dari katalog
    let (s, j) = kirim(
        &app,
        Method::PATCH,
        &uri,
        Some(&produsen),
        Some(json!({"harga_idr": 15000})),
    )
    .await;
    assert_eq!(
        (s, j["kurasi_ulang_ringan"].as_bool()),
        (StatusCode::OK, Some(true))
    );
    let status: String = sqlx::query_scalar("SELECT status FROM produk")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(status, "MENUNGGU_KURASI");
    let (_, l) = kirim(&app, Method::GET, "/katalog/produk", None, None).await;
    assert_eq!(l["data"].as_array().unwrap().len(), 0);

    // harga tidak valid
    for h in [0, -500, 12250] {
        assert_eq!(
            kirim(
                &app,
                Method::PATCH,
                &uri,
                Some(&produsen),
                Some(json!({"harga_idr": h}))
            )
            .await
            .0,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{h}"
        );
    }

    // riwayat append-only: dua perubahan tercatat; UPDATE/DELETE ditolak DB
    let (_, r) = kirim(
        &app,
        Method::GET,
        &format!("/web/varian/{}/harga-riwayat", u(v)),
        Some(&produsen),
        None,
    )
    .await;
    assert_eq!(r["data"].as_array().unwrap().len(), 2);
    assert_eq!(r["data"][1]["harga_lama_idr"], 12000);
    assert!(sqlx::query("UPDATE harga_riwayat SET harga_baru = 500")
        .execute(&db)
        .await
        .is_err());
    assert!(sqlx::query("DELETE FROM harga_riwayat")
        .execute(&db)
        .await
        .is_err());
}

#[sqlx::test]
async fn ganti_foto_utama_pada_produk_tayang_memicu_kurasi_ulang(db: PgPool) {
    let app = app(db.clone());
    let (pid, produsen) = seed_pengguna(&db, "a@contoh.id", Peran::Produsen).await;
    let lapak = seed_lapak(&db, pid, "lapak-a").await;
    let (produk, _) = seed_produk(
        &db,
        lapak,
        "Sambal Bawang Mbok D — 150 g",
        "bumbu-dapur",
        10000,
        3,
        "TAYANG",
    )
    .await;
    let uri = format!("/web/produk/{}", u(produk));
    // media sama -> tetap tayang
    let (_, j) = kirim(
        &app,
        Method::PUT,
        &uri,
        Some(&produsen),
        Some(json!({"media": [foto("https://cdn.uji/foto.jpg", 1200)]})),
    )
    .await;
    assert_eq!(j["status"], "TAYANG");
    let (_, j) = kirim(
        &app,
        Method::PUT,
        &uri,
        Some(&produsen),
        Some(json!({"media": [foto("https://cdn.uji/baru.jpg", 1200)]})),
    )
    .await;
    assert_eq!(j["status"], "MENUNGGU_KURASI");
    // harga/stok produk tayang tidak boleh diubah lewat PUT
    let (_, d) = kirim(&app, Method::GET, &uri, Some(&produsen), None).await;
    let v = &d["varian"][0];
    let mut payload = varian(v["sku"].as_str().unwrap(), 99000, 3);
    payload["id"] = v["id"].clone();
    // status MENUNGGU_KURASI -> 409 (tidak dapat diubah)
    assert_eq!(
        kirim(
            &app,
            Method::PUT,
            &uri,
            Some(&produsen),
            Some(json!({"varian": [payload]}))
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
}

#[sqlx::test]
async fn arsip_menggantikan_hapus_dan_revisi_kedaluwarsa_7_hari(db: PgPool) {
    let app = app(db.clone());
    let (pid, produsen) = seed_pengguna(&db, "a@contoh.id", Peran::Produsen).await;
    let lapak = seed_lapak(&db, pid, "lapak-a").await;
    let (p1, _) = seed_produk(
        &db,
        lapak,
        "Sambal Bawang Mbok D — 150 g",
        "bumbu-dapur",
        10000,
        3,
        "TAYANG",
    )
    .await;
    let (s, j) = kirim(
        &app,
        Method::POST,
        &format!("/web/produk/{}/arsipkan", u(p1)),
        Some(&produsen),
        None,
    )
    .await;
    assert_eq!((s, j["status"].as_str()), (StatusCode::OK, Some("ARSIP")));
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM produk WHERE id = $1")
        .bind(p1)
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(n, 1, "data dipertahankan");
    let (_, l) = kirim(&app, Method::GET, "/katalog/produk", None, None).await;
    assert_eq!(l["data"].as_array().unwrap().len(), 0);

    let (p2, _) = seed_produk(
        &db,
        lapak,
        "Keripik Singkong Pedas 200 g",
        "bumbu-dapur",
        10000,
        3,
        "PERLU_REVISI",
    )
    .await;
    sqlx::query("UPDATE produk SET perlu_revisi_pada = now() - interval '8 days' WHERE id = $1")
        .bind(p2)
        .execute(&db)
        .await
        .unwrap();
    let (p3, _) = seed_produk(
        &db,
        lapak,
        "Madu Hutan Murni 500 ml",
        "bumbu-dapur",
        10000,
        3,
        "PERLU_REVISI",
    )
    .await;
    sqlx::query("UPDATE produk SET perlu_revisi_pada = now() - interval '6 days' WHERE id = $1")
        .bind(p3)
        .execute(&db)
        .await
        .unwrap();
    assert_eq!(
        pasaree::lapak::arsipkan_revisi_kedaluwarsa(&db)
            .await
            .unwrap(),
        1
    );
    let st: String = sqlx::query_scalar("SELECT status FROM produk WHERE id = $1")
        .bind(p2)
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(st, "ARSIP");
}

#[sqlx::test]
async fn presigned_batas_ukuran_dan_tipe(db: PgPool) {
    let app = app(db.clone());
    let (_, produsen) = seed_pengguna(&db, "a@contoh.id", Peran::Produsen).await;
    let (_, pembeli) = seed_pengguna(&db, "p@contoh.id", Peran::Pembeli).await;
    let minta = |mime: &str, ukuran: i64| json!({"nama_berkas": "foto saya.jpg", "tipe_mime": mime, "ukuran_byte": ukuran});

    let (s, j) = kirim(
        &app,
        Method::POST,
        "/web/media/presigned",
        Some(&produsen),
        Some(minta("image/jpeg", 5 * 1024 * 1024)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{j}");
    assert!(j["unggah_url"].as_str().unwrap().contains("tanda="));
    assert!(j["media_url"].as_str().unwrap().starts_with("https://"));
    assert!(!j["media_url"].as_str().unwrap().contains(' '));
    assert_eq!(
        kirim(
            &app,
            Method::POST,
            "/web/media/presigned",
            Some(&produsen),
            Some(minta("image/jpeg", 5 * 1024 * 1024 + 1))
        )
        .await
        .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(
        kirim(
            &app,
            Method::POST,
            "/web/media/presigned",
            Some(&produsen),
            Some(minta("video/mp4", 50 * 1024 * 1024))
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        kirim(
            &app,
            Method::POST,
            "/web/media/presigned",
            Some(&produsen),
            Some(minta("video/mp4", 50 * 1024 * 1024 + 1))
        )
        .await
        .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(
        kirim(
            &app,
            Method::POST,
            "/web/media/presigned",
            Some(&produsen),
            Some(minta("application/pdf", 100))
        )
        .await
        .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(
        kirim(
            &app,
            Method::POST,
            "/web/media/presigned",
            Some(&pembeli),
            Some(minta("image/jpeg", 100))
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
}
