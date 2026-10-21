//! Keuangan (diisi penuh di PSR-BE-006).

use sqlx::PgPool;
use uuid::Uuid;

use crate::error::AppResult;

/// Saldo escrow tertahan milik lapak (IDR).
pub async fn saldo_tertahan(_db: &PgPool, _lapak_id: Uuid) -> AppResult<i64> {
    Ok(0)
}
