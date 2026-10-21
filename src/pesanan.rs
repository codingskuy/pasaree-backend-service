//! Pesanan (diisi penuh di PSR-BE-005).

use sqlx::PgPool;
use uuid::Uuid;

use crate::error::AppResult;

/// Skor fulfillment: 100 bila belum ada pesanan.
pub async fn skor_fulfillment(_db: &PgPool, _lapak_id: Uuid) -> AppResult<i32> {
    Ok(100)
}
