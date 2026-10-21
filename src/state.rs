use std::sync::Arc;

use sqlx::PgPool;

use crate::{config::Config, lumbung::LumbungClient, trip::TripCache};

#[derive(Clone)]
pub struct AppState {
    pub db: PgPool,
    pub config: Arc<Config>,
    pub lumbung: LumbungClient,
    pub trip_cache: Arc<TripCache>,
}

impl AppState {
    pub fn baru(db: PgPool, config: Config) -> Self {
        let lumbung = LumbungClient::baru(&config.lumbung_base_url, &config.lumbung_service_key);
        let trip_cache = Arc::new(TripCache::baru(std::time::Duration::from_secs(
            config.trip_cache_detik,
        )));
        Self {
            db,
            config: Arc::new(config),
            lumbung,
            trip_cache,
        }
    }
}
