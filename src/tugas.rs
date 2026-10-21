//! Tugas latar berkala (dipanggil dari `main`; tiap fungsi juga dapat diuji langsung).

use std::time::Duration;

use sqlx::PgPool;

fn lapor(nama: &str, hasil: crate::error::AppResult<u64>) {
    match hasil {
        Ok(n) if n > 0 => tracing::info!(n, tugas = nama, "tugas berkala memproses data"),
        Ok(_) => {}
        Err(e) => tracing::error!(?e, tugas = nama, "tugas berkala gagal"),
    }
}

/// Tiap menit: batas bayar 30 menit, batas konfirmasi lapak, terima otomatis, masa retur.
/// Tiap jam: arsip draf PERLU_REVISI > 7 hari.
pub async fn jalankan_berkala(db: PgPool) {
    let mut tiap_menit = tokio::time::interval(Duration::from_secs(60));
    let mut tick: u64 = 0;
    loop {
        tiap_menit.tick().await;
        lapor(
            "batalkan_kedaluwarsa",
            crate::pesanan::batalkan_kedaluwarsa(&db).await,
        );
        lapor(
            "konfirmasi_kedaluwarsa",
            crate::pesanan::konfirmasi_kedaluwarsa(&db).await,
        );
        lapor(
            "terima_otomatis",
            crate::pesanan::terima_otomatis(&db).await,
        );
        lapor(
            "selesaikan_masa_retur",
            crate::pesanan::selesaikan_masa_retur(&db).await,
        );
        if tick.is_multiple_of(60) {
            lapor(
                "arsipkan_revisi_kedaluwarsa",
                crate::lapak::arsipkan_revisi_kedaluwarsa(&db).await,
            );
        }
        tick += 1;
    }
}
