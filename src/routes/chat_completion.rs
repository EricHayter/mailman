use std::{net::SocketAddr, sync::Arc};

use axum::{
    Router,
    body::{Body, Bytes},
    extract::{DefaultBodyLimit, State},
    http::{
        HeaderMap, HeaderValue, StatusCode,
        header::{AUTHORIZATION, CONTENT_TYPE},
    },
    response::{IntoResponse, Response},
    routing::{get, post},
};
use futures_util::TryStreamExt;

use super::AppState;
use crate::{
    discovery::DiscoveryEvent, models::CreateChatCompletionRequest, selector::NodeSelector,
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

/// Tries each responsive node in turn until one gives a response that isn't a
/// server error. Nodes that fail are marked unresponsive so other requests
/// skip them until their next passing health check.
async fn create_chat_completion(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, StatusCode> {
    let req: CreateChatCompletionRequest =
        serde_json::from_slice(&body).map_err(|_| StatusCode::BAD_REQUEST)?;
    if !req.is_valid() {
        return Err(StatusCode::BAD_REQUEST);
    }

    let authorization = headers.get(AUTHORIZATION);
    let mut tried = Vec::new();
    // The last 5xx response, returned if every node fails.
    let mut fallback = None;

    while let Some(addr) = state.selector.select(&req, &tried) {
        tried.push(addr);

        match send_to_node(&state.client, addr, body.clone(), authorization).await {
            Attempt::Done(response) => return Ok(proxy_response(response, addr, &state.selector)),
            Attempt::NodeFailed(response) => {
                mark_unresponsive(&state.selector, addr);
                if let Some(response) = response {
                    fallback = Some((addr, response));
                }
            }
            Attempt::Failed => return Err(StatusCode::BAD_GATEWAY),
        }
    }

    match fallback {
        Some((addr, response)) => Ok(proxy_response(response, addr, &state.selector)),
        None if tried.is_empty() => Err(StatusCode::SERVICE_UNAVAILABLE),
        None => Err(StatusCode::BAD_GATEWAY),
    }
}

enum Attempt {
    /// Pass this response to the client, including 4xx, which are the
    /// request's fault rather than the node's.
    Done(reqwest::Response),
    /// The node failed, so try another. Holds the response if it answered
    /// with a server error.
    NodeFailed(Option<reqwest::Response>),
    /// The request couldn't be built, which isn't the node's fault.
    Failed,
}

async fn send_to_node(
    client: &reqwest::Client,
    addr: SocketAddr,
    body: Bytes,
    authorization: Option<&HeaderValue>,
) -> Attempt {
    // Forward the original bytes so fields we don't parse still reach vLLM.
    let mut upstream = client
        .post(format!("http://{addr}/v1/chat/completions"))
        .header(CONTENT_TYPE, "application/json")
        .body(body);
    if let Some(authorization) = authorization {
        upstream = upstream.header(AUTHORIZATION, authorization);
    }

    match upstream.send().await {
        Ok(response) if response.status().is_server_error() => Attempt::NodeFailed(Some(response)),
        Ok(response) => Attempt::Done(response),
        Err(err) if err.is_builder() => Attempt::Failed,
        // Connection errors, timeouts, and the node dropping the connection
        // before responding.
        Err(_) => Attempt::NodeFailed(None),
    }
}

/// Passes chunks through as they arrive so `stream: true` responses aren't
/// held back until generation finishes. A failure partway through marks the
/// node unresponsive, but can't be retried because the response has started.
fn proxy_response(
    response: reqwest::Response,
    addr: SocketAddr,
    selector: &Arc<dyn NodeSelector>,
) -> Response {
    let status = response.status();
    let mut headers = HeaderMap::new();
    if let Some(content_type) = response.headers().get(CONTENT_TYPE) {
        headers.insert(CONTENT_TYPE, content_type.clone());
    }

    let selector = selector.clone();
    let body = response
        .bytes_stream()
        .inspect_err(move |_| mark_unresponsive(&selector, addr));

    (status, headers, Body::from_stream(body)).into_response()
}

fn mark_unresponsive(selector: &Arc<dyn NodeSelector>, addr: SocketAddr) {
    selector.handle_discovery_event(DiscoveryEvent::Unresponsive(addr));
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
