use pasaree::{app, config::Config, state::AppState};
use sqlx::postgres::PgPoolOptions;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();
    let config = Config::dari_env().unwrap_or_else(|e| panic!("konfigurasi: {e}"));
    let db = PgPoolOptions::new()
        .max_connections(10)
        .connect(&config.database_url)
        .await
        .expect("koneksi DB pasaree");
    sqlx::migrate!("./migrations")
        .run(&db)
        .await
        .expect("migrasi");
    let port = config.port;
    let state = AppState::baru(db, config);
    tokio::spawn(pasaree::tugas::jalankan_berkala(state.clone()));
    let pendengar = tokio::net::TcpListener::bind(("0.0.0.0", port))
        .await
        .expect("bind port");
    tracing::info!(port, "pasaree-backend-service jalan");
    axum::serve(pendengar, app(state)).await.expect("serve");
}
