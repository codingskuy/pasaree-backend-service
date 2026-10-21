//! Tugas latar berkala (dipanggil dari `main`; tiap fungsi juga dapat diuji langsung).

use std::time::Duration;

use sqlx::PgPool;

pub async fn jalankan_berkala(db: PgPool) {
    let mut tiap_jam = tokio::time::interval(Duration::from_secs(3600));
    loop {
        tiap_jam.tick().await;
        match crate::lapak::arsipkan_revisi_kedaluwarsa(&db).await {
            Ok(n) if n > 0 => tracing::info!(n, "draf PERLU_REVISI > 7 hari diarsipkan"),
            Ok(_) => {}
            Err(e) => tracing::error!(?e, "gagal mengarsipkan draf kedaluwarsa"),
        }
    }
}
