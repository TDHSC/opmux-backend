//! Protected-endpoint extractors that emit the canonical error envelope.

use axum::{
    extract::{
        rejection::{JsonRejection, PathRejection},
        FromRequest, FromRequestParts, Path,
    },
    http::{request::Parts, Request, StatusCode},
    Json,
};
use serde::de::DeserializeOwned;

use super::http_error::{ErrorCode, HttpError};

/// JSON body extractor that maps framework rejections to the API envelope.
pub struct ApiJson<T>(pub T);

/// Path extractor that maps framework rejections to the API envelope.
pub struct ApiPath<T>(pub T);

impl<S, T> FromRequest<S> for ApiJson<T>
where
    T: DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = HttpError;

    async fn from_request(
        req: Request<axum::body::Body>,
        state: &S,
    ) -> Result<Self, Self::Rejection> {
        match Json::<T>::from_request(req, state).await {
            Ok(Json(value)) => Ok(Self(value)),
            Err(rejection) => Err(map_json_rejection(rejection)),
        }
    }
}

impl<S, T> FromRequestParts<S> for ApiPath<T>
where
    T: DeserializeOwned + Send,
    S: Send + Sync,
{
    type Rejection = HttpError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &S,
    ) -> Result<Self, Self::Rejection> {
        match Path::<T>::from_request_parts(parts, state).await {
            Ok(Path(value)) => Ok(Self(value)),
            Err(rejection) => Err(map_path_rejection(rejection)),
        }
    }
}

fn map_json_rejection(rejection: JsonRejection) -> HttpError {
    match rejection {
        JsonRejection::MissingJsonContentType(_) => HttpError::new(
            ErrorCode::UnsupportedMediaType,
            "Content-Type must be application/json",
        ),
        JsonRejection::JsonSyntaxError(_) | JsonRejection::JsonDataError(_) => {
            HttpError::new(ErrorCode::InvalidJson, "Request body is not valid JSON")
        }
        JsonRejection::BytesRejection(inner) => {
            if inner.status() == StatusCode::PAYLOAD_TOO_LARGE {
                HttpError::new(ErrorCode::PayloadTooLarge, "Request body is too large")
            } else {
                HttpError::new(ErrorCode::InvalidJson, "Request body is not valid JSON")
            }
        }
        other => {
            let _ = other;
            HttpError::new(ErrorCode::InvalidJson, "Request body is not valid JSON")
        }
    }
}

fn map_path_rejection(rejection: PathRejection) -> HttpError {
    match rejection {
        PathRejection::FailedToDeserializePathParams(_)
        | PathRejection::MissingPathParams(_) => {
            HttpError::new(ErrorCode::InvalidPath, "Request path is not valid")
        }
        other => {
            let _ = other;
            HttpError::new(ErrorCode::InvalidPath, "Request path is not valid")
        }
    }
}
