use axum::body::{Body, Bytes};
use axum::http::{header, HeaderValue, Response, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use codeseex_store::{RequestStatus, Store};
use futures_util::StreamExt;
use serde_json::json;

pub(crate) fn response_from_stream(
    status: reqwest::StatusCode,
    content_type: Option<HeaderValue>,
    body: Body,
) -> axum::response::Response {
    let mut builder = Response::builder()
        .status(StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY));
    if let Some(value) = content_type {
        builder = builder.header(header::CONTENT_TYPE, value);
    }
    builder
        .body(body)
        .unwrap_or_else(|_| Response::new(Body::empty()))
}

pub(crate) fn passthrough_stream_with_completion(
    response: reqwest::Response,
    store: Store,
    request_id: String,
) -> impl futures_util::Stream<Item = Result<Bytes, std::io::Error>> {
    async_stream::try_stream! {
        let mut upstream = response.bytes_stream();
        while let Some(chunk) = upstream.next().await {
            match chunk {
                Ok(bytes) => yield bytes,
                Err(error) => {
                    let detail = json!({ "error": error.to_string() });
                    let _ = store.finish_request(&request_id, RequestStatus::Failed, None, Some(&detail)).await;
                    let _ = store
                        .record_event(
                            "error",
                            "request_failed",
                            "Streaming chat completion failed.",
                            Some(&json!({ "id": request_id, "error": error.to_string() })),
                        )
                        .await;
                    Err(std::io::Error::other(error))?;
                }
            }
        }
        let _ = store.finish_request(&request_id, RequestStatus::Completed, None, None).await;
        let _ = store
            .record_event(
                "info",
                "request_completed",
                "Streaming chat completion completed.",
                Some(&json!({ "id": request_id })),
            )
            .await;
    }
}

pub(crate) fn response_from_bytes(
    status: reqwest::StatusCode,
    content_type: Option<HeaderValue>,
    bytes: Vec<u8>,
) -> axum::response::Response {
    let mut builder = Response::builder()
        .status(StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY));
    if let Some(value) = content_type {
        builder = builder.header(header::CONTENT_TYPE, value);
    }
    builder
        .body(Body::from(bytes))
        .unwrap_or_else(|_| Response::new(Body::empty()))
}

pub(crate) fn json_error(
    status: StatusCode,
    code: &str,
    message: String,
) -> axum::response::Response {
    (
        status,
        Json(json!({ "error": { "code": code, "message": message, "type": "api_error" } })),
    )
        .into_response()
}

pub(crate) fn response_content_type_json() -> Option<HeaderValue> {
    Some(HeaderValue::from_static("application/json"))
}
