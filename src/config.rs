//! Konfigurasi dari lingkungan.

#[derive(Clone, Debug)]
pub struct Config {
    pub database_url: String,
    pub jwt_secret: String,
    pub jwt_aud: String,
    pub jwt_issuer: String,
    pub port: u16,
    pub lumbung_base_url: String,
    pub lumbung_service_key: String,
}

impl Config {
    pub fn dari_env() -> Result<Self, String> {
        let wajib = |k: &str| std::env::var(k).map_err(|_| format!("variabel {k} wajib diisi"));
        let jwt_secret = wajib("JWT_SECRET")?;
        if jwt_secret.len() < 32 {
            return Err("JWT_SECRET minimal 32 karakter".into());
        }
        Ok(Self {
            database_url: wajib("DATABASE_URL")?,
            jwt_secret,
            jwt_aud: std::env::var("JWT_AUD").unwrap_or_else(|_| "pasaree".into()),
            jwt_issuer: std::env::var("JWT_ISSUER").unwrap_or_else(|_| "pasaree-auth".into()),
            port: std::env::var("PORT")
                .ok()
                .and_then(|p| p.parse().ok())
                .unwrap_or(8101),
            lumbung_base_url: wajib("LUMBUNG_BASE_URL")?,
            lumbung_service_key: wajib("LUMBUNG_SERVICE_KEY")?,
        })
    }

    /// Konfigurasi tetap untuk uji.
    pub fn untuk_uji() -> Self {
        Self {
            database_url: String::new(),
            jwt_secret: "rahasia-uji-yang-panjangnya-lebih-dari-32-karakter".into(),
            jwt_aud: "pasaree".into(),
            jwt_issuer: "pasaree-auth".into(),
            port: 0,
            lumbung_base_url: "http://127.0.0.1:1".into(),
            lumbung_service_key: "kunci-layanan-uji".into(),
        }
    }
}
