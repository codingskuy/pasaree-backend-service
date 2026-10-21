mod common;

use axum::http::{Method, StatusCode};
use chrono::Utc;
use common::*;
use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use pasaree::{
    auth::{self, Peran},
    config::Config,
};
use serde_json::json;
use sqlx::PgPool;

#[sqlx::test]
async fn kesehatan_menyentuh_db(db: PgPool) {
    let app = app(db);
    let (s, j) = kirim(&app, Method::GET, "/kesehatan", None, None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(j["db"], "pasaree");
}

#[sqlx::test]
async fn daftar_lalu_login_menerbitkan_token_15_menit(db: PgPool) {
    let app = app(db);
    let (s, j) = kirim(
        &app,
        Method::POST,
        "/auth/daftar",
        None,
        Some(json!({"email": "ani@contoh.id", "kata_sandi": "rahasia-123", "nama": "Ani"})),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);
    assert_eq!(j["expires_in"], 900);

    let (s, j) = kirim(
        &app,
        Method::POST,
        "/auth/login",
        None,
        Some(json!({"email": "ANI@contoh.id", "kata_sandi": "rahasia-123"})),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let akses = j["access_token"].as_str().unwrap();
    let p = auth::verifikasi_akses(&Config::untuk_uji(), akses).unwrap();
    assert_eq!(p.peran, Peran::Pembeli);
}

#[sqlx::test]
async fn daftar_email_ganda_409_dan_validasi_422(db: PgPool) {
    let app = app(db);
    let badan = json!({"email": "ani@contoh.id", "kata_sandi": "rahasia-123", "nama": "Ani"});
    assert_eq!(
        kirim(
            &app,
            Method::POST,
            "/auth/daftar",
            None,
            Some(badan.clone())
        )
        .await
        .0,
        StatusCode::CREATED
    );
    let (s, j) = kirim(&app, Method::POST, "/auth/daftar", None, Some(badan)).await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert_eq!(j["kode"], "EMAIL_SUDAH_TERDAFTAR");

    let (s, j) = kirim(
        &app,
        Method::POST,
        "/auth/daftar",
        None,
        Some(json!({"email": "bukan-email", "kata_sandi": "pendek", "nama": "A"})),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(j["kode"], "VALIDASI_GAGAL");
    assert!(j["detail"]["kesalahan"].as_array().unwrap().len() >= 3);
}

#[sqlx::test]
async fn login_salah_401_dan_json_rusak_400(db: PgPool) {
    let app = app(db.clone());
    seed_pengguna(&db, "budi@contoh.id", Peran::Pembeli).await;
    let (s, j) = kirim(
        &app,
        Method::POST,
        "/auth/login",
        None,
        Some(json!({"email": "budi@contoh.id", "kata_sandi": "salah"})),
    )
    .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    assert_eq!(j["kode"], "AUTH_KEDALUWARSA");
    let (s, _) = kirim(
        &app,
        Method::POST,
        "/auth/login",
        None,
        Some(json!({"email": "tidak-ada@contoh.id", "kata_sandi": "x"})),
    )
    .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);

    // JSON tidak lengkap -> 422, sintaks rusak -> 400.
    let (s, _) = kirim(
        &app,
        Method::POST,
        "/auth/login",
        None,
        Some(json!({"email": "budi@contoh.id"})),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    let req = axum::http::Request::builder()
        .method(Method::POST)
        .uri("/auth/login")
        .header("content-type", "application/json")
        .body(axum::body::Body::from("{rusak"))
        .unwrap();
    use tower::ServiceExt;
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

fn token_dengan(aud: &str, exp: i64) -> String {
    let cfg = Config::untuk_uji();
    let klaim = json!({"sub": uuid::Uuid::now_v7().to_string(), "aud": aud, "iss": cfg.jwt_issuer, "exp": exp, "iat": Utc::now().timestamp(), "peran": "PEMBELI"});
    encode(
        &Header::new(Algorithm::HS256),
        &klaim,
        &EncodingKey::from_secret(cfg.jwt_secret.as_bytes()),
    )
    .unwrap()
}

#[tokio::test]
async fn token_audiens_lintas_divisi_ditolak() {
    let cfg = Config::untuk_uji();
    let masa_depan = Utc::now().timestamp() + 600;
    assert!(auth::verifikasi_akses(&cfg, &token_dengan("pasaree", masa_depan)).is_ok());
    for aud in ["lumbung", "pawonee", "titeny", ""] {
        assert!(
            auth::verifikasi_akses(&cfg, &token_dengan(aud, masa_depan)).is_err(),
            "aud {aud} harus ditolak"
        );
    }
    // kedaluwarsa
    assert!(
        auth::verifikasi_akses(&cfg, &token_dengan("pasaree", Utc::now().timestamp() - 5)).is_err()
    );
}

#[sqlx::test]
async fn endpoint_terlindungi_menolak_audiens_salah_dengan_401(db: PgPool) {
    let app = app(db.clone());
    let (_, ok) = seed_pengguna(&db, "pembeli@contoh.id", Peran::Pembeli).await;
    let (s, _) = kirim(&app, Method::GET, "/saya", Some(&ok), None).await;
    assert_eq!(s, StatusCode::OK);
    let (s, j) = kirim(
        &app,
        Method::GET,
        "/saya",
        Some(&token_dengan("lumbung", Utc::now().timestamp() + 600)),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    assert_eq!(j["kode"], "AUTH_KEDALUWARSA");
    let (s, _) = kirim(&app, Method::GET, "/saya", None, None).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
}

#[sqlx::test]
async fn peran_diterapkan_per_route_403(db: PgPool) {
    let app = app(db.clone());
    let (_, pembeli) = seed_pengguna(&db, "p@contoh.id", Peran::Pembeli).await;
    let (_, admin) = seed_pengguna(&db, "a@contoh.id", Peran::Admin).await;
    let (target, _) = seed_pengguna(&db, "t@contoh.id", Peran::Pembeli).await;
    let uri = format!(
        "/web/admin/pengguna/{}/peran",
        pasaree::ids::ke_ulid(target)
    );
    let (s, j) = kirim(
        &app,
        Method::PATCH,
        &uri,
        Some(&pembeli),
        Some(json!({"peran": "KURATOR"})),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    assert_eq!(j["kode"], "AKSES_DITOLAK");
    let (s, j) = kirim(
        &app,
        Method::PATCH,
        &uri,
        Some(&admin),
        Some(json!({"peran": "KURATOR"})),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(j["peran"], "KURATOR");
    let (s, _) = kirim(
        &app,
        Method::PATCH,
        "/web/admin/pengguna/01ARZ3NDEKTSV4RRFFQ69G5FAV/peran",
        Some(&admin),
        Some(json!({"peran": "ADMIN"})),
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

#[sqlx::test]
async fn refresh_memutar_token_dan_mendeteksi_pemakaian_ulang(db: PgPool) {
    let app = app(db.clone());
    seed_pengguna(&db, "c@contoh.id", Peran::Produsen).await;
    let (_, j) = kirim(
        &app,
        Method::POST,
        "/auth/login",
        None,
        Some(json!({"email": "c@contoh.id", "kata_sandi": "rahasia-123"})),
    )
    .await;
    let r1 = j["refresh_token"].as_str().unwrap().to_string();

    let (s, j2) = kirim(
        &app,
        Method::POST,
        "/auth/refresh",
        None,
        Some(json!({"refresh_token": r1})),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let r2 = j2["refresh_token"].as_str().unwrap().to_string();
    assert_ne!(r1, r2);

    // pakai ulang r1 -> 401 dan seluruh keluarga dicabut (r2 ikut tidak berlaku)
    let (s, _) = kirim(
        &app,
        Method::POST,
        "/auth/refresh",
        None,
        Some(json!({"refresh_token": r1})),
    )
    .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let (s, _) = kirim(
        &app,
        Method::POST,
        "/auth/refresh",
        None,
        Some(json!({"refresh_token": r2})),
    )
    .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);

    let (s, _) = kirim(
        &app,
        Method::POST,
        "/auth/refresh",
        None,
        Some(json!({"refresh_token": "acak"})),
    )
    .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
}

#[sqlx::test]
async fn refresh_kedaluwarsa_ditolak(db: PgPool) {
    let app = app(db.clone());
    seed_pengguna(&db, "d@contoh.id", Peran::Pembeli).await;
    let (_, j) = kirim(
        &app,
        Method::POST,
        "/auth/login",
        None,
        Some(json!({"email": "d@contoh.id", "kata_sandi": "rahasia-123"})),
    )
    .await;
    sqlx::query("UPDATE refresh_token SET kedaluwarsa = now() - interval '1 second'")
        .execute(&db)
        .await
        .unwrap();
    let (s, _) = kirim(
        &app,
        Method::POST,
        "/auth/refresh",
        None,
        Some(json!({"refresh_token": j["refresh_token"]})),
    )
    .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn masa_berlaku_refresh_7_hari() {
    assert_eq!(auth::REFRESH_HARI, 7);
    assert_eq!(auth::AKSES_DETIK, 900);
}
