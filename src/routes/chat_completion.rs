use super::AppState;
use crate::models::CreateChatCompletionRequest;
use axum::{
    Router,
    body::{Body, Bytes},
    extract::{DefaultBodyLimit, State},
    http::{
        HeaderMap, StatusCode,
        header::{AUTHORIZATION, CONTENT_TYPE},
    },
    response::IntoResponse,
    routing::{get, post},
};

const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/chat/completions",
            post(create_chat_completion).get(list_chat_completion),
        )
        .route(
            "/chat/completions/{*completion_id}",
            get(get_chat_completion)
                .post(update_chat_completion)
                .delete(delete_chat_completion),
        )
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
}

// Only the create endpoint is implemented for now. The other endpoints will
// need to track which node each completion was sent to.

async fn create_chat_completion(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<impl IntoResponse, StatusCode> {
    let req: CreateChatCompletionRequest =
        serde_json::from_slice(&body).map_err(|_| StatusCode::BAD_REQUEST)?;
    if !req.is_valid() {
        return Err(StatusCode::BAD_REQUEST);
    }

    let addr = state
        .selector
        .select(&req)
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)?;

    // Forward the original bytes so fields we don't parse still reach vLLM.
    let mut upstream = state
        .client
        .post(format!("http://{addr}/v1/chat/completions"))
        .header(CONTENT_TYPE, "application/json")
        .body(body);
    if let Some(authorization) = headers.get(AUTHORIZATION) {
        upstream = upstream.header(AUTHORIZATION, authorization);
    }

    let response = upstream.send().await.map_err(|_| StatusCode::BAD_GATEWAY)?;

    let status = response.status();
    let mut response_headers = HeaderMap::new();
    if let Some(content_type) = response.headers().get(CONTENT_TYPE) {
        response_headers.insert(CONTENT_TYPE, content_type.clone());
    }

    // Pass chunks through as they arrive so `stream: true` responses aren't
    // held back until generation finishes.
    let body = Body::from_stream(response.bytes_stream());

    Ok((status, response_headers, body))
}

async fn delete_chat_completion() -> String {
    "delete chat".to_string()
}

async fn list_chat_completion() -> String {
    "list chat".to_string()
}

async fn get_chat_completion() -> String {
    "get chat".to_string()
}

async fn update_chat_completion() -> String {
    "update chat".to_string()
}
