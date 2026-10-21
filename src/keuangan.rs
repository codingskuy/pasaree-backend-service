//! Keuangan (diisi penuh di PSR-BE-006).

use sqlx::PgPool;
use uuid::Uuid;

use crate::error::AppResult;

/// Saldo escrow tertahan milik lapak (IDR).
pub async fn saldo_tertahan(_db: &PgPool, _lapak_id: Uuid) -> AppResult<i64> {
    Ok(0)
}

/// Pelepasan escrow saat serah-terima trip tercatat (diisi penuh di PSR-BE-006).
pub async fn lepas_escrow_sub(_c: &mut sqlx::PgConnection, _sub_id: Uuid) -> AppResult<()> {
    Ok(())
}
