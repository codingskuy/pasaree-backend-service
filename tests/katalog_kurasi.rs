mod common;

use axum::http::{Method, StatusCode};
use common::*;
use pasaree::auth::Peran;
use serde_json::{json, Value};
use sqlx::PgPool;

async fn seed_banyak(db: &PgPool) -> (String, String) {
    let (pid, _) = seed_pengguna(db, "a@contoh.id", Peran::Produsen).await;
    let (pid2, _) = seed_pengguna(db, "b@contoh.id", Peran::Produsen).await;
    let l1 = seed_lapak(db, pid, "lapak-a").await;
    let l2 = seed_lapak(db, pid2, "lapak-b").await;
    let judul = [
        ("Sambal Bawang Mbok D 150 g", "bumbu-dapur", 18000),
        ("Keripik Singkong Pedas 200 g", "pangan-olahan", 12000),
        ("Madu Hutan Murni 500 ml", "pangan-olahan", 90000),
        ("Teh Melati Premium 100 g", "minuman", 25000),
        ("Anyaman Bambu Hias 1 pcs", "kerajinan-pangan", 45000),
    ];
    for (i, (j, k, h)) in judul.iter().enumerate() {
        let lapak = if i < 3 { l1 } else { l2 };
        seed_produk(db, lapak, j, k, *h, 5, "TAYANG").await;
        tokio::time::sleep(std::time::Duration::from_millis(2)).await; // urutan ULID stabil
    }
    ("lapak-a".into(), "lapak-b".into())
}

fn ids_dari(j: &Value) -> Vec<String> {
    j["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["id"].as_str().unwrap().to_string())
        .collect()
}

#[sqlx::test]
async fn paginasi_cursor_semua_urutan_tanpa_duplikat(db: PgPool) {
    let app = app(db.clone());
    seed_banyak(&db).await;
    for sort in ["terbaru", "harga_asc", "harga_desc", "rating"] {
        let mut semua = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..10 {
            let uri = match &cursor {
                Some(c) => format!("/katalog/produk?sort={sort}&limit=2&cursor={c}"),
                None => format!("/katalog/produk?sort={sort}&limit=2"),
            };
            let (s, j) = kirim(&app, Method::GET, &uri, None, None).await;
            assert_eq!(s, StatusCode::OK, "{j}");
            assert!(j["data"].as_array().unwrap().len() <= 2);
            semua.extend(ids_dari(&j));
            match j["cursor_lanjut"].as_str() {
                Some(c) => cursor = Some(c.to_string()),
                None => break,
            }
        }
        assert_eq!(semua.len(), 5, "{sort}");
        let mut unik = semua.clone();
        unik.sort();
        unik.dedup();
        assert_eq!(unik.len(), 5, "{sort}: tidak boleh duplikat");
    }
    // urutan harga benar
    let (_, j) = kirim(
        &app,
        Method::GET,
        "/katalog/produk?sort=harga_asc",
        None,
        None,
    )
    .await;
    let harga: Vec<i64> = j["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["harga_mulai_idr"].as_i64().unwrap())
        .collect();
    assert_eq!(harga, vec![12000, 18000, 25000, 45000, 90000]);
    let (_, j) = kirim(
        &app,
        Method::GET,
        "/katalog/produk?sort=harga_desc",
        None,
        None,
    )
    .await;
    assert_eq!(j["data"][0]["harga_mulai_idr"], 90000);
}

#[sqlx::test]
async fn limit_default_20_maks_50_dan_validasi(db: PgPool) {
    let app = app(db.clone());
    seed_banyak(&db).await;
    let (s, j) = kirim(&app, Method::GET, "/katalog/produk", None, None).await;
    assert_eq!(s, StatusCode::OK);
    assert!(j["cursor_lanjut"].is_null());
    assert_eq!(
        kirim(&app, Method::GET, "/katalog/produk?limit=50", None, None)
            .await
            .0,
        StatusCode::OK
    );
    for q in [
        "limit=51",
        "limit=0",
        "limit=-1",
        "sort=acak",
        "kategori=tidak-ada",
        "tag=aneh",
        "cursor=###",
    ] {
        let (s, j) = kirim(
            &app,
            Method::GET,
            &format!("/katalog/produk?{q}"),
            None,
            None,
        )
        .await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{q}");
        assert_eq!(j["kode"], "VALIDASI_GAGAL");
    }
}

#[sqlx::test]
async fn filter_q_kategori_tag_lapak(db: PgPool) {
    let app = app(db.clone());
    seed_banyak(&db).await;
    let (_, j) = kirim(&app, Method::GET, "/katalog/produk?q=sambal", None, None).await;
    assert_eq!(j["data"].as_array().unwrap().len(), 1);
    let (_, j) = kirim(&app, Method::GET, "/katalog/produk?q=%25", None, None).await;
    assert_eq!(j["data"].as_array().unwrap().len(), 0, "wildcard di-escape");
    let (_, j) = kirim(
        &app,
        Method::GET,
        "/katalog/produk?kategori=pangan-olahan",
        None,
        None,
    )
    .await;
    assert_eq!(j["data"].as_array().unwrap().len(), 2);
    let (_, j) = kirim(&app, Method::GET, "/katalog/produk?tag=pedas", None, None).await;
    assert_eq!(j["data"].as_array().unwrap().len(), 5);
    let (_, j) = kirim(&app, Method::GET, "/katalog/produk?tag=vegan", None, None).await;
    assert_eq!(j["data"].as_array().unwrap().len(), 0);
    let (_, j) = kirim(
        &app,
        Method::GET,
        "/katalog/produk?lapak_slug=lapak-b",
        None,
        None,
    )
    .await;
    assert_eq!(j["data"].as_array().unwrap().len(), 2);
    assert_eq!(j["data"][0]["lapak"]["slug"], "lapak-b");
    assert_eq!(j["data"][0]["media_utama"], "https://cdn.uji/foto.jpg");
}

#[sqlx::test]
async fn katalog_hanya_tayang_dan_token_rusak_401(db: PgPool) {
    let app = app(db.clone());
    let (pid, _) = seed_pengguna(&db, "a@contoh.id", Peran::Produsen).await;
    let lapak = seed_lapak(&db, pid, "lapak-a").await;
    for st in [
        "DRAF",
        "MENUNGGU_KURASI",
        "PERLU_REVISI",
        "TAKEDOWN",
        "ARSIP",
    ] {
        seed_produk(
            &db,
            lapak,
            &format!("Produk {st} Uji 100 g"),
            "bumbu-dapur",
            10000,
            5,
            st,
        )
        .await;
    }
    let (tayang, _) = seed_produk(
        &db,
        lapak,
        "Produk Tayang Uji 100 g",
        "bumbu-dapur",
        10000,
        5,
        "TAYANG",
    )
    .await;
    let (s, j) = kirim(&app, Method::GET, "/katalog/produk", None, None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(ids_dari(&j), vec![u(tayang)]);
    // detail produk non-tayang = 404
    let tdk: String = sqlx::query_scalar("SELECT id::text FROM produk WHERE status = 'DRAF'")
        .fetch_one(&db)
        .await
        .unwrap();
    let uid = uuid::Uuid::parse_str(&tdk).unwrap();
    assert_eq!(
        kirim(
            &app,
            Method::GET,
            &format!("/katalog/produk/{}", u(uid)),
            None,
            None
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        kirim(&app, Method::GET, "/katalog/produk/bukan-ulid", None, None)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    // token rusak tetap 401; token valid (auth opsional) boleh
    assert_eq!(
        kirim(
            &app,
            Method::GET,
            "/katalog/produk",
            Some("token-rusak"),
            None
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    let (_, pembeli) = seed_pengguna(&db, "p@contoh.id", Peran::Pembeli).await;
    assert_eq!(
        kirim(&app, Method::GET, "/katalog/produk", Some(&pembeli), None)
            .await
            .0,
        StatusCode::OK
    );
}

#[sqlx::test]
async fn detail_produk_dan_profil_lapak(db: PgPool) {
    let app = app(db.clone());
    seed_banyak(&db).await;
    let (_, j) = kirim(&app, Method::GET, "/katalog/produk?q=sambal", None, None).await;
    let id = j["data"][0]["id"].as_str().unwrap();
    let (s, d) = kirim(
        &app,
        Method::GET,
        &format!("/katalog/produk/{id}"),
        None,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(d["lapak"]["slug"], "lapak-a");
    assert_eq!(d["varian"][0]["harga_idr"], 18000);
    assert_eq!(d["tag"][0], "pedas");
    assert!(d["trip_tersedia"].is_array());
    assert_eq!(d["media"][0]["utama"], true);

    let (s, l) = kirim(&app, Method::GET, "/katalog/lapak/lapak-a", None, None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(l["produk"].as_array().unwrap().len(), 3);
    assert_eq!(l["skor_fulfillment_persen"], 100);
    assert_eq!(
        kirim(&app, Method::GET, "/katalog/lapak/tidak-ada", None, None)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    // lapak BEKU tidak tampil
    sqlx::query("UPDATE lapak SET status = 'BEKU' WHERE slug = 'lapak-a'")
        .execute(&db)
        .await
        .unwrap();
    assert_eq!(
        kirim(&app, Method::GET, "/katalog/lapak/lapak-a", None, None)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    let (_, j) = kirim(&app, Method::GET, "/katalog/produk", None, None).await;
    assert_eq!(j["data"].as_array().unwrap().len(), 2);
}

// ------------------------------------------------------------------ kurasi

#[sqlx::test]
async fn antrean_fifo_prioritas_pangan_segar_dan_paginasi(db: PgPool) {
    let app = app(db.clone());
    let (pid, _) = seed_pengguna(&db, "a@contoh.id", Peran::Produsen).await;
    let (_, kurator) = seed_pengguna(&db, "k@contoh.id", Peran::Kurator).await;
    let (_, pembeli) = seed_pengguna(&db, "p@contoh.id", Peran::Pembeli).await;
    let lapak = seed_lapak(&db, pid, "lapak-a").await;
    // urutan diajukan: A(olahan) B(segar) C(olahan) D(segar)
    let mut ids = Vec::new();
    for (j, k) in [
        ("A Olahan Satu 100 g", "pangan-olahan"),
        ("B Segar Satu 100 g", "pangan-segar"),
        ("C Olahan Dua 100 g", "pangan-olahan"),
        ("D Segar Dua 100 g", "pangan-segar"),
    ] {
        let (p, _) = seed_produk(&db, lapak, j, k, 10000, 5, "MENUNGGU_KURASI").await;
        sqlx::query(
            "UPDATE produk SET diajukan_pada = now() - ($2 || ' hours')::interval WHERE id = $1",
        )
        .bind(p)
        .bind((10 - ids.len()).to_string())
        .execute(&db)
        .await
        .unwrap();
        ids.push(p);
    }
    let (s, j) = kirim(
        &app,
        Method::GET,
        "/web/kurasi/antrean",
        Some(&kurator),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let urut: Vec<&str> = j["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["judul"].as_str().unwrap().split(' ').next().unwrap())
        .collect();
    assert_eq!(urut, vec!["B", "D", "A", "C"], "segar dulu, lalu FIFO");
    assert_eq!(j["data"][0]["prioritas_pangan_segar"], true);
    assert!(j["data"][0]["umur_antrean_jam"].as_i64().unwrap() >= 8);

    // paginasi 1 per halaman menghasilkan urutan yang sama
    let mut gabung = Vec::new();
    let mut cursor: Option<String> = None;
    for _ in 0..6 {
        let uri = match &cursor {
            Some(c) => format!("/web/kurasi/antrean?limit=1&cursor={c}"),
            None => "/web/kurasi/antrean?limit=1".to_string(),
        };
        let (_, j) = kirim(&app, Method::GET, &uri, Some(&kurator), None).await;
        gabung.push(
            j["data"][0]["judul"]
                .as_str()
                .unwrap()
                .chars()
                .next()
                .unwrap(),
        );
        match j["cursor_lanjut"].as_str() {
            Some(c) => cursor = Some(c.to_string()),
            None => break,
        }
    }
    assert_eq!(gabung, vec!['B', 'D', 'A', 'C']);

    let (_, j) = kirim(
        &app,
        Method::GET,
        "/web/kurasi/antrean?kategori=pangan-olahan",
        Some(&kurator),
        None,
    )
    .await;
    assert_eq!(j["data"].as_array().unwrap().len(), 2);
    assert_eq!(
        kirim(
            &app,
            Method::GET,
            "/web/kurasi/antrean",
            Some(&pembeli),
            None
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        kirim(&app, Method::GET, "/web/kurasi/antrean", None, None)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
}

#[sqlx::test]
async fn keputusan_kurasi_setuju_revisi_tolak_beraudit(db: PgPool) {
    let app = app(db.clone());
    let (pid, produsen) = seed_pengguna(&db, "a@contoh.id", Peran::Produsen).await;
    let (kid, kurator) = seed_pengguna(&db, "k@contoh.id", Peran::Kurator).await;
    let (_, operator) = seed_pengguna(&db, "op@contoh.id", Peran::Operator).await;
    let lapak = seed_lapak(&db, pid, "lapak-a").await;
    let (p1, _) = seed_produk(
        &db,
        lapak,
        "Sambal Bawang Mbok D 150 g",
        "bumbu-dapur",
        18000,
        5,
        "MENUNGGU_KURASI",
    )
    .await;
    let (p2, _) = seed_produk(
        &db,
        lapak,
        "Keripik Singkong Pedas 200 g",
        "bumbu-dapur",
        12000,
        5,
        "MENUNGGU_KURASI",
    )
    .await;
    let (p3, _) = seed_produk(
        &db,
        lapak,
        "Madu Hutan Murni 500 ml",
        "bumbu-dapur",
        90000,
        5,
        "MENUNGGU_KURASI",
    )
    .await;
    let (p4, _) = seed_produk(
        &db,
        lapak,
        "Teh Melati Premium 100 g",
        "bumbu-dapur",
        25000,
        0,
        "MENUNGGU_KURASI",
    )
    .await;

    // peran: produsen/operator tidak boleh memutuskan
    for tok in [&produsen, &operator] {
        assert_eq!(
            kirim(
                &app,
                Method::POST,
                &format!("/web/kurasi/{}/keputusan", u(p1)),
                Some(tok),
                Some(json!({"keputusan": "SETUJU"}))
            )
            .await
            .0,
            StatusCode::FORBIDDEN
        );
    }

    // SETUJU -> TAYANG dan muncul di katalog; promo lapak baru dimulai
    let (s, j) = kirim(
        &app,
        Method::POST,
        &format!("/web/kurasi/{}/keputusan", u(p1)),
        Some(&kurator),
        Some(json!({"keputusan": "SETUJU"})),
    )
    .await;
    assert_eq!((s, j["status"].as_str()), (StatusCode::OK, Some("TAYANG")));
    let (_, l) = kirim(&app, Method::GET, "/katalog/produk", None, None).await;
    assert_eq!(ids_dari(&l), vec![u(p1)]);
    let mulai: Option<chrono::DateTime<chrono::Utc>> =
        sqlx::query_scalar("SELECT produk_pertama_tayang_pada FROM lapak")
            .fetch_one(&db)
            .await
            .unwrap();
    assert!(mulai.is_some());
    // sudah diputuskan -> 409
    assert_eq!(
        kirim(
            &app,
            Method::POST,
            &format!("/web/kurasi/{}/keputusan", u(p1)),
            Some(&kurator),
            Some(json!({"keputusan": "SETUJU"}))
        )
        .await
        .0,
        StatusCode::CONFLICT
    );

    // SETUJU pada produk stok 0 -> langsung NONAKTIF_STOK_0
    let (_, j) = kirim(
        &app,
        Method::POST,
        &format!("/web/kurasi/{}/keputusan", u(p4)),
        Some(&kurator),
        Some(json!({"keputusan": "SETUJU"})),
    )
    .await;
    assert_eq!(j["status"], "NONAKTIF_STOK_0");

    // REVISI wajib alasan baku; catatan <= 280
    let uri2 = format!("/web/kurasi/{}/keputusan", u(p2));
    assert_eq!(
        kirim(
            &app,
            Method::POST,
            &uri2,
            Some(&kurator),
            Some(json!({"keputusan": "REVISI"}))
        )
        .await
        .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(
        kirim(
            &app,
            Method::POST,
            &uri2,
            Some(&kurator),
            Some(json!({"keputusan": "REVISI", "alasan_baku": "foto tidak asli"}))
        )
        .await
        .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(kirim(&app, Method::POST, &uri2, Some(&kurator), Some(json!({"keputusan": "REVISI", "alasan_baku": "LABEL_KURANG", "catatan": "x".repeat(281)}))).await.0, StatusCode::UNPROCESSABLE_ENTITY);
    let (s, j) = kirim(&app, Method::POST, &uri2, Some(&kurator), Some(json!({"keputusan": "REVISI", "alasan_baku": "LABEL_KURANG", "catatan": "Tambahkan label berat bersih."}))).await;
    assert_eq!(
        (s, j["status"].as_str()),
        (StatusCode::OK, Some("PERLU_REVISI"))
    );
    let (_, w) = kirim(
        &app,
        Method::GET,
        "/web/produk?status=PERLU_REVISI",
        Some(&produsen),
        None,
    )
    .await;
    assert_eq!(
        w["data"][0]["perlu_revisi_catatan"],
        "Tambahkan label berat bersih."
    );

    // TOLAK wajib alasan baku + catatan (pasal piagam) -> TAKEDOWN
    let uri3 = format!("/web/kurasi/{}/keputusan", u(p3));
    assert_eq!(
        kirim(
            &app,
            Method::POST,
            &uri3,
            Some(&kurator),
            Some(json!({"keputusan": "TOLAK", "alasan_baku": "PELANGGARAN_LARANGAN_MUTLAK"}))
        )
        .await
        .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let (s, j) = kirim(&app, Method::POST, &uri3, Some(&kurator), Some(json!({"keputusan": "TOLAK", "alasan_baku": "PELANGGARAN_LARANGAN_MUTLAK", "catatan": "Piagam Kurasi pasal 5.1"}))).await;
    assert_eq!(
        (s, j["status"].as_str()),
        (StatusCode::OK, Some("TAKEDOWN"))
    );

    // audit: siapa, kapan, alasan, snapshot konten; append-only
    let (n, kurator_id): (i64, uuid::Uuid) =
        sqlx::query_as("SELECT count(*), min(kurator_id::text)::uuid FROM kurasi_keputusan")
            .fetch_one(&db)
            .await
            .unwrap();
    assert_eq!((n, kurator_id), (4, kid));
    let snap: Value =
        sqlx::query_scalar("SELECT snapshot FROM kurasi_keputusan WHERE produk_id = $1")
            .bind(p1)
            .fetch_one(&db)
            .await
            .unwrap();
    assert_eq!(snap["judul"], "Sambal Bawang Mbok D 150 g");
    assert_eq!(snap["varian"][0]["harga_idr"], 18000);
    assert!(sqlx::query("DELETE FROM kurasi_keputusan")
        .execute(&db)
        .await
        .is_err());

    // produk tidak ada -> 404; body rusak -> 400/422
    assert_eq!(
        kirim(
            &app,
            Method::POST,
            "/web/kurasi/01ARZ3NDEKTSV4RRFFQ69G5FAV/keputusan",
            Some(&kurator),
            Some(json!({"keputusan": "SETUJU"}))
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        kirim(
            &app,
            Method::POST,
            &uri3,
            Some(&kurator),
            Some(json!({"keputusan": "MUNGKIN"}))
        )
        .await
        .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
}
