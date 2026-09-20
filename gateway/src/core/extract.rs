//! Protected-endpoint extractors that emit the canonical error envelope.

use axum::{
    extract::{
        rejection::{JsonRejection, PathRejection},
        FromRequest, FromRequestParts, Path,
    },
    http::{header, request::Parts, HeaderMap, Request, StatusCode},
    Json,
};
use serde::de::DeserializeOwned;

use super::http_error::{ErrorCode, HttpError};

/// Inclusive raw HTTP body limit for protected JSON extractors.
///
/// Inserted on the protected production router so `ApiJson` can reject an
/// advertised `Content-Length` above the configured bound before buffering.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RequestBodyLimit(pub u64);

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
        if let Some(RequestBodyLimit(limit)) =
            req.extensions().get::<RequestBodyLimit>().copied()
        {
            if advertised_content_length_exceeds(req.headers(), limit) {
                return Err(payload_too_large());
            }
        }
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

fn advertised_content_length_exceeds(headers: &HeaderMap, limit: u64) -> bool {
    headers
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .is_some_and(|len| len > limit)
}

fn payload_too_large() -> HttpError {
    HttpError::new(ErrorCode::PayloadTooLarge, "Request body is too large")
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
                payload_too_large()
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

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::Body, extract::DefaultBodyLimit, http::Request, routing::post, Extension,
        Router,
    };
    use serde_json::Value;
    use tower::ServiceExt;

    async fn accept(ApiJson(_): ApiJson<Value>) -> StatusCode {
        StatusCode::OK
    }

    fn router(limit: u64) -> Router {
        Router::new()
            .route("/", post(accept))
            .layer(DefaultBodyLimit::max(limit as usize))
            .layer(Extension(RequestBodyLimit(limit)))
    }

    #[test]
    fn advertised_content_length_bound_is_inclusive() {
        let mut headers = HeaderMap::new();
        headers.insert(header::CONTENT_LENGTH, "8".parse().unwrap());
        assert!(!advertised_content_length_exceeds(&headers, 8));
        headers.insert(header::CONTENT_LENGTH, "9".parse().unwrap());
        assert!(advertised_content_length_exceeds(&headers, 8));
        assert!(!advertised_content_length_exceeds(&HeaderMap::new(), 8));
    }

    #[tokio::test]
    async fn advertised_and_streamed_oversize_bodies_map_to_payload_too_large() {
        let exact = router(8)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::CONTENT_LENGTH, "8")
                    .body(Body::from(r#"{"a":1} "#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(exact.status(), StatusCode::OK);

        let advertised = router(8)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::CONTENT_LENGTH, "9")
                    .body(Body::from(r#"{"a":1}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(advertised.status(), StatusCode::PAYLOAD_TOO_LARGE);

        let streamed = router(8)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"a":1234}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(streamed.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }
}
