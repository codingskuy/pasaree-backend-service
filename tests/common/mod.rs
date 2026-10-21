#![allow(dead_code)]
//! Pembantu uji: bangun app di atas pool `#[sqlx::test]`, kirim permintaan, seed pengguna.

use axum::{
    body::Body,
    http::{header, Method, Request, StatusCode},
    Router,
};
use http_body_util::BodyExt;
use pasaree::{
    auth::{self, Peran},
    config::Config,
    ids,
    state::AppState,
};
use serde_json::Value;
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

pub fn app_dengan(db: PgPool, config: Config) -> Router {
    pasaree::app(AppState::baru(db, config))
}

pub fn app(db: PgPool) -> Router {
    app_dengan(db, Config::untuk_uji())
}

pub async fn kirim(
    app: &Router,
    metode: Method,
    uri: &str,
    token: Option<&str>,
    badan: Option<Value>,
) -> (StatusCode, Value) {
    kirim_dengan_header(app, metode, uri, token, &[], badan).await
}

pub async fn kirim_dengan_header(
    app: &Router,
    metode: Method,
    uri: &str,
    token: Option<&str>,
    header_tambahan: &[(&str, &str)],
    badan: Option<Value>,
) -> (StatusCode, Value) {
    let mut b = Request::builder().method(metode).uri(uri);
    if let Some(t) = token {
        b = b.header(header::AUTHORIZATION, format!("Bearer {t}"));
    }
    for (k, v) in header_tambahan {
        b = b.header(*k, *v);
    }
    let req = match badan {
        Some(j) => b
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(j.to_string()))
            .unwrap(),
        None => b.body(Body::empty()).unwrap(),
    };
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let json = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, json)
}

/// Seed pengguna langsung ke DB, kembalikan (id, access token).
pub async fn seed_pengguna(db: &PgPool, email: &str, peran: Peran) -> (Uuid, String) {
    let id = ids::baru();
    sqlx::query(
        "INSERT INTO pengguna (id, email, kata_sandi_hash, nama, peran) VALUES ($1,$2,$3,$4,$5)",
    )
    .bind(id)
    .bind(email)
    .bind(auth::hash_sandi("rahasia-123").unwrap())
    .bind("Uji")
    .bind(peran.sebagai_str())
    .execute(db)
    .await
    .unwrap();
    let token = auth::terbitkan_akses(&Config::untuk_uji(), id, peran).unwrap();
    (id, token)
}

/// Seed produsen + lapak langsung ke DB. Kembalikan lapak_id.
pub async fn seed_lapak(db: &PgPool, pengguna_id: Uuid, slug: &str) -> Uuid {
    let produsen = ids::baru();
    sqlx::query("INSERT INTO produsen (id, pengguna_id, nama, jenis, kontak_wa) VALUES ($1,$2,'Mbok D','UMKM','+6281234567890')")
        .bind(produsen)
        .bind(pengguna_id)
        .execute(db)
        .await
        .unwrap();
    let lapak = ids::baru();
    sqlx::query(
        "INSERT INTO lapak (id, produsen_id, nama_lapak, slug, deskripsi, titik_serah_lumbung_id, rekening_payout_tersensor) \
         VALUES ($1,$2,$3,$4,$5,'TITIK-05','****1234')",
    )
    .bind(lapak)
    .bind(produsen)
    .bind(format!("Lapak {slug}"))
    .bind(slug)
    .bind("Lapak uji dengan deskripsi yang cukup panjang untuk memenuhi batas minimal lima puluh karakter.")
    .execute(db)
    .await
    .unwrap();
    lapak
}

/// Seed produk (default TAYANG) dengan satu varian dan satu foto utama. Kembalikan (produk_id, varian_id).
pub async fn seed_produk(
    db: &PgPool,
    lapak_id: Uuid,
    judul: &str,
    kategori: &str,
    harga: i32,
    stok: i32,
    status: &str,
) -> (Uuid, Uuid) {
    let pid = ids::baru();
    sqlx::query(
        "INSERT INTO produk (id, lapak_id, judul, kategori_kode, tag, deskripsi, komposisi, alergen, status, diajukan_pada) \
         VALUES ($1,$2,$3,$4,'{pedas}',$5,'Bawang, cabai','{}',$6, now())",
    )
    .bind(pid)
    .bind(lapak_id)
    .bind(judul)
    .bind(kategori)
    .bind("Produk uji dengan deskripsi yang cukup panjang untuk memenuhi batas minimal lima puluh karakter.")
    .bind(status)
    .execute(db)
    .await
    .unwrap();
    let vid = ids::baru();
    let ulid = ids::ke_ulid(vid);
    let sku = format!("LPK-{}-0001", &ulid[ulid.len() - 4..]);
    sqlx::query(
        "INSERT INTO varian_produk (id, produk_id, nama_varian, sku, harga_idr, stok, satuan, berat_kirim_gram, tanggal_produksi, kedaluwarsa) \
         VALUES ($1,$2,'Original',$3,$4,$5,'g',200, '2026-10-01', '2027-06-01')",
    )
    .bind(vid)
    .bind(pid)
    .bind(sku)
    .bind(harga)
    .bind(stok)
    .execute(db)
    .await
    .unwrap();
    sqlx::query("INSERT INTO media_produk (id, produk_id, url, jenis, utama, lebar, tinggi, ukuran_byte) VALUES ($1,$2,'https://cdn.uji/foto.jpg','FOTO',true,1200,1200,500000)")
        .bind(ids::baru())
        .bind(pid)
        .execute(db)
        .await
        .unwrap();
    (pid, vid)
}

pub fn u(id: Uuid) -> String {
    ids::ke_ulid(id)
}

// ------------------------------------------------------------------ Lumbung palsu

use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc, Mutex,
};

pub struct LumbungPalsu {
    pub url: String,
    pub hit_trip: Arc<AtomicUsize>,
    pub mati: Arc<AtomicBool>,
    pub salah_kunci: Arc<AtomicUsize>,
    pub trip: Arc<Mutex<Vec<Value>>>,
}

pub async fn lumbung_palsu(kunci_benar: &str) -> LumbungPalsu {
    use axum::{
        extract::State,
        http::HeaderMap,
        routing::{get, post},
        Json,
    };
    #[derive(Clone)]
    struct S {
        hit: Arc<AtomicUsize>,
        mati: Arc<AtomicBool>,
        salah: Arc<AtomicUsize>,
        trip: Arc<Mutex<Vec<Value>>>,
        kunci: String,
    }
    fn cek(h: &HeaderMap, s: &S) {
        let ok = h.get("authorization").and_then(|v| v.to_str().ok())
            == Some(&format!("Bearer {}", s.kunci));
        if !ok {
            s.salah.fetch_add(1, Ordering::SeqCst);
        }
    }
    async fn trip(State(s): State<S>, h: HeaderMap) -> Result<Json<Value>, axum::http::StatusCode> {
        cek(&h, &s);
        s.hit.fetch_add(1, Ordering::SeqCst);
        if s.mati.load(Ordering::SeqCst) {
            return Err(axum::http::StatusCode::INTERNAL_SERVER_ERROR);
        }
        Ok(Json(
            serde_json::json!({"data": s.trip.lock().unwrap().clone()}),
        ))
    }
    async fn verifikasi(
        State(s): State<S>,
        h: HeaderMap,
        Json(b): Json<Value>,
    ) -> Result<Json<Value>, axum::http::StatusCode> {
        cek(&h, &s);
        if s.mati.load(Ordering::SeqCst) {
            return Err(axum::http::StatusCode::INTERNAL_SERVER_ERROR);
        }
        if b["kode_serah"] == "KODE-BENAR" {
            Ok(Json(
                serde_json::json!({"cocok": true, "bukti_id": "BUKTI-LMB-1"}),
            ))
        } else {
            Err(axum::http::StatusCode::CONFLICT)
        }
    }
    let st = S {
        hit: Arc::new(AtomicUsize::new(0)),
        mati: Arc::new(AtomicBool::new(false)),
        salah: Arc::new(AtomicUsize::new(0)),
        trip: Arc::new(Mutex::new(vec![serde_json::json!({
            "trip_id": "TRP-1", "jadwal_berangkat": (chrono::Utc::now() + chrono::Duration::days(1)).to_rfc3339(),
            "rute": "Pasar-Titik05", "titik_serah": "TITIK-05", "sisa_kapasitas": 10, "ongkir_idr": 8000
        })])),
        kunci: kunci_benar.to_string(),
    };
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", l.local_addr().unwrap());
    let router = axum::Router::new()
        .route("/internal/pasaree/trip", get(trip))
        .route(
            "/internal/pasaree/serah-terima/verifikasi",
            post(verifikasi),
        )
        .with_state(st.clone());
    tokio::spawn(async move { axum::serve(l, router).await.unwrap() });
    LumbungPalsu {
        url,
        hit_trip: st.hit,
        mati: st.mati,
        salah_kunci: st.salah,
        trip: st.trip,
    }
}

pub struct Dunia {
    pub app: Router,
    pub db: PgPool,
    pub lumbung: LumbungPalsu,
    pub pembeli: String,
    pub pembeli_id: Uuid,
    pub prod_a: String,
    pub prod_b: String,
    pub lapak_a: Uuid,
    pub lapak_b: Uuid,
    pub produk_a: Uuid,
    pub produk_b: Uuid,
    /// varian A: bumbu-dapur 18000 (dapat diretur); varian B: pangan-segar 12000 (tidak dapat diretur)
    pub va: Uuid,
    pub vb: Uuid,
}

pub async fn dunia_dengan(db: PgPool, ubah: impl FnOnce(&mut Config)) -> Dunia {
    let mut cfg = Config::untuk_uji();
    let lumbung = lumbung_palsu(&cfg.lumbung_service_key).await;
    cfg.lumbung_base_url = lumbung.url.clone();
    ubah(&mut cfg);
    let app = app_dengan(db.clone(), cfg);
    let (pa, prod_a) = seed_pengguna(&db, "a@contoh.id", Peran::Produsen).await;
    let (pb, prod_b) = seed_pengguna(&db, "b@contoh.id", Peran::Produsen).await;
    let (pembeli_id, pembeli) = seed_pengguna(&db, "pembeli@contoh.id", Peran::Pembeli).await;
    let lapak_a = seed_lapak(&db, pa, "lapak-a").await;
    let lapak_b = seed_lapak(&db, pb, "lapak-b").await;
    let (produk_a, va) = seed_produk(
        &db,
        lapak_a,
        "Sambal Bawang Mbok D 150 g",
        "bumbu-dapur",
        18000,
        10,
        "TAYANG",
    )
    .await;
    let (produk_b, vb) = seed_produk(
        &db,
        lapak_b,
        "Sayur Segar Ikat 500 g",
        "pangan-segar",
        12000,
        10,
        "TAYANG",
    )
    .await;
    Dunia {
        app,
        db,
        lumbung,
        pembeli,
        pembeli_id,
        prod_a,
        prod_b,
        lapak_a,
        lapak_b,
        produk_a,
        produk_b,
        va,
        vb,
    }
}

pub async fn dunia(db: PgPool) -> Dunia {
    dunia_dengan(db, |_| {}).await
}

impl Dunia {
    pub async fn keranjang(&self, varian: Uuid, qty: i64) -> (StatusCode, Value) {
        kirim(
            &self.app,
            Method::PUT,
            "/keranjang",
            Some(&self.pembeli),
            Some(serde_json::json!({"varian_id": u(varian), "qty": qty})),
        )
        .await
    }

    pub async fn pesan(&self) -> (StatusCode, Value) {
        kirim(
            &self.app,
            Method::POST,
            "/pesanan",
            Some(&self.pembeli),
            Some(serde_json::json!({"trip_id": "TRP-1", "titik_id": "TITIK-05"})),
        )
        .await
    }

    /// Isi keranjang A x2 (36000) dan B x1 (12000) lalu buat pesanan. Subtotal 48000, ongkir 8000, layanan 2000, total 58000.
    pub async fn pesanan_dua_lapak(&self) -> String {
        assert_eq!(self.keranjang(self.va, 2).await.0, StatusCode::OK);
        assert_eq!(self.keranjang(self.vb, 1).await.0, StatusCode::OK);
        let (s, j) = self.pesan().await;
        assert_eq!(s, StatusCode::CREATED, "{j}");
        j["pesanan_id"].as_str().unwrap().to_string()
    }

    pub async fn bayar(&self, id: &str, kunci: &str) -> (StatusCode, Value) {
        kirim(
            &self.app,
            Method::POST,
            &format!("/pesanan/{id}/bayar"),
            Some(&self.pembeli),
            Some(serde_json::json!({"metode": "QRIS", "idempotency_key": kunci})),
        )
        .await
    }

    pub async fn sub_ids(&self, pesanan: &str) -> Vec<(Uuid, Uuid)> {
        let uid = pasaree::ids::dari_ulid(pesanan, "PSR-").unwrap();
        sqlx::query_as(
            "SELECT id, lapak_id FROM sub_pesanan WHERE pesanan_id = $1 ORDER BY lapak_id",
        )
        .bind(uid)
        .fetch_all(&self.db)
        .await
        .unwrap()
    }

    pub async fn sub_untuk(&self, pesanan: &str, lapak: Uuid) -> Uuid {
        self.sub_ids(pesanan)
            .await
            .into_iter()
            .find(|s| s.1 == lapak)
            .unwrap()
            .0
    }

    pub async fn status_induk(&self, pesanan: &str) -> String {
        let uid = pasaree::ids::dari_ulid(pesanan, "PSR-").unwrap();
        sqlx::query_scalar("SELECT status FROM pesanan WHERE id = $1")
            .bind(uid)
            .fetch_one(&self.db)
            .await
            .unwrap()
    }

    pub async fn status_sub(&self, sub: Uuid) -> String {
        sqlx::query_scalar("SELECT status FROM sub_pesanan WHERE id = $1")
            .bind(sub)
            .fetch_one(&self.db)
            .await
            .unwrap()
    }

    pub async fn stok(&self, varian: Uuid) -> i32 {
        sqlx::query_scalar("SELECT stok FROM varian_produk WHERE id = $1")
            .bind(varian)
            .fetch_one(&self.db)
            .await
            .unwrap()
    }

    pub async fn konfirmasi(&self, token: &str, sub: Uuid, terima: bool) -> (StatusCode, Value) {
        kirim(
            &self.app,
            Method::POST,
            &format!("/web/pesanan-masuk/{}/konfirmasi", u(sub)),
            Some(token),
            Some(serde_json::json!({"terima": terima, "alasan": "stok_rusak"})),
        )
        .await
    }

    pub async fn serah(&self, token: &str, sub: Uuid, kode: &str) -> (StatusCode, Value) {
        kirim(
            &self.app,
            Method::POST,
            &format!("/web/pesanan-masuk/{}/serah-trip", u(sub)),
            Some(token),
            Some(serde_json::json!({"trip_id": "TRP-1", "kode_serah": kode})),
        )
        .await
    }
}
