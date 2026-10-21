use std::sync::Arc;

use sqlx::PgPool;

use crate::{config::Config, lumbung::LumbungClient};

#[derive(Clone)]
pub struct AppState {
    pub db: PgPool,
    pub config: Arc<Config>,
    pub lumbung: LumbungClient,
}

impl AppState {
    pub fn baru(db: PgPool, config: Config) -> Self {
        let lumbung = LumbungClient::baru(&config.lumbung_base_url, &config.lumbung_service_key);
        Self {
            db,
            config: Arc::new(config),
            lumbung,
        }
    }
}
