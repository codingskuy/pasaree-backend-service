use axum::{http::HeaderMap, response::IntoResponse, routing::get, Json, Router};
use http_body_util::BodyExt;
use pasaree::{error::AppError, lumbung::LumbungClient};
use serde_json::{json, Value};

async fn baca(e: AppError) -> (u16, Value) {
    let r = e.into_response();
    let s = r.status().as_u16();
    let b = r.into_body().collect().await.unwrap().to_bytes();
    (s, serde_json::from_slice(&b).unwrap())
}

#[tokio::test]
async fn app_error_memetakan_status_dan_kode() {
    assert_eq!(baca(AppError::Permintaan("x".into())).await.0, 400);
    assert_eq!(baca(AppError::TidakTerotentikasi("x".into())).await.0, 401);
    assert_eq!(baca(AppError::Terlarang("x".into())).await.0, 403);
    let (s, j) = baca(AppError::tidak_ditemukan("PESANAN_TIDAK_DITEMUKAN", "x")).await;
    assert_eq!(
        (s, j["kode"].as_str()),
        (404, Some("PESANAN_TIDAK_DITEMUKAN"))
    );
    let (s, j) = baca(AppError::konflik("STOK_HABIS", "x")).await;
    assert_eq!((s, j["kode"].as_str()), (409, Some("STOK_HABIS")));
    let (s, j) = baca(AppError::validasi("judul", "terlalu pendek")).await;
    assert_eq!(s, 422);
    assert_eq!(j["detail"]["kesalahan"][0]["kolom"], "judul");
    let (s, j) = baca(AppError::Internal("rahasia internal".into())).await;
    assert_eq!(s, 500);
    assert!(
        !j.to_string().contains("rahasia internal"),
        "detail internal tidak boleh bocor"
    );
}

#[tokio::test]
async fn klien_lumbung_memakai_service_key_bukan_token_pengguna() {
    async fn gema(h: HeaderMap) -> Json<Value> {
        Json(json!({
            "auth": h.get("authorization").and_then(|v| v.to_str().ok()),
            "asal": h.get("x-layanan-asal").and_then(|v| v.to_str().ok()),
        }))
    }
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(l, Router::new().route("/ping", get(gema)))
            .await
            .unwrap()
    });

    let k = LumbungClient::baru(&format!("http://{addr}/"), "kunci-layanan-uji");
    let j: Value = k.get_json("/ping", &[]).await.unwrap();
    assert_eq!(j["auth"], "Bearer kunci-layanan-uji");
    assert_eq!(j["asal"], "pasaree");

    let mati = LumbungClient::baru("http://127.0.0.1:1", "k");
    assert!(mati.get_json::<Value>("/ping", &[]).await.is_err());
}
