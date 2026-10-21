//! Ekstraktor JSON yang memetakan kegagalan ke `AppError` (400 sintaks, 422 data) dan menjalankan `validator`.

use axum::{
    extract::{rejection::JsonRejection, FromRequest, Request},
    Json,
};
use serde::de::DeserializeOwned;
use validator::Validate;

use crate::error::AppError;

pub struct ApiJson<T>(pub T);

impl<S, T> FromRequest<S> for ApiJson<T>
where
    S: Send + Sync,
    T: DeserializeOwned + Validate,
{
    type Rejection = AppError;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        match Json::<T>::from_request(req, state).await {
            Ok(Json(v)) => {
                v.validate()?;
                Ok(ApiJson(v))
            }
            Err(JsonRejection::JsonDataError(e)) => Err(AppError::validasi("badan", e.body_text())),
            Err(e) => Err(AppError::Permintaan(e.body_text())),
        }
    }
}
