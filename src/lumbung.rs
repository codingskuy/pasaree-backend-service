//! Klien HTTP ke Lumbung. Memakai SERVICE KEY least-privilege, bukan token pengguna
//! (docs/auth.md). Pasaree tidak membaca/menulis DB Lumbung; hanya lewat API ini.

use std::time::Duration;

use serde::de::DeserializeOwned;

#[derive(Clone)]
pub struct LumbungClient {
    http: reqwest::Client,
    base_url: String,
    service_key: String,
}

#[derive(Debug)]
pub struct LumbungGagal {
    pub status: Option<u16>,
    pub pesan: String,
}

impl LumbungGagal {
    fn jaringan(e: impl ToString) -> Self {
        Self {
            status: None,
            pesan: e.to_string(),
        }
    }
}

impl LumbungClient {
    pub fn baru(base_url: &str, service_key: &str) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(3))
            .build()
            .expect("klien http");
        Self {
            http,
            base_url: base_url.trim_end_matches('/').to_string(),
            service_key: service_key.to_string(),
        }
    }

    fn minta(&self, metode: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        self.http
            .request(metode, format!("{}{}", self.base_url, path))
            .bearer_auth(&self.service_key)
            .header("x-layanan-asal", "pasaree")
    }

    pub async fn get_json<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, String)],
    ) -> Result<T, LumbungGagal> {
        let resp = self
            .minta(reqwest::Method::GET, path)
            .query(query)
            .send()
            .await
            .map_err(LumbungGagal::jaringan)?;
        if !resp.status().is_success() {
            return Err(LumbungGagal {
                status: Some(resp.status().as_u16()),
                pesan: format!("status {}", resp.status()),
            });
        }
        resp.json::<T>().await.map_err(LumbungGagal::jaringan)
    }

    pub async fn post_json<B: serde::Serialize, T: DeserializeOwned>(
        &self,
        path: &str,
        badan: &B,
    ) -> Result<T, LumbungGagal> {
        let resp = self
            .minta(reqwest::Method::POST, path)
            .json(badan)
            .send()
            .await
            .map_err(LumbungGagal::jaringan)?;
        if !resp.status().is_success() {
            return Err(LumbungGagal {
                status: Some(resp.status().as_u16()),
                pesan: format!("status {}", resp.status()),
            });
        }
        resp.json::<T>().await.map_err(LumbungGagal::jaringan)
    }
}
