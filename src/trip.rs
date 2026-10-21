//! Trip Lumbung (diisi penuh di PSR-BE-005).

use serde_json::Value;

use crate::state::AppState;

/// Trip tersedia untuk detail produk; (daftar, basi).
pub async fn trip_untuk_produk(_s: &AppState) -> (Vec<Value>, bool) {
    (Vec::new(), false)
}
