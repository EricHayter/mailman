use axum::{
    Router,
    routing::method_routing::{delete, get, post},
};

use serde::Deserialize;

pub fn routes() -> Router {
    Router::new()
        .route("/chat/completions", post(create_chat_completion))
        .route(
            "/chat/completions/{*completion_id}",
            delete(delete_chat_completion),
        )
        .route("/chat/completions", get(list_chat_completion))
        .route(
            "/chat/completions/{*completion_id}",
            get(get_chat_completion),
        )
        .route(
            "/chat/completions/{*completion_id}",
            post(update_chat_completion),
        )
}

/* Only the create endpoint is implemented for now. The other endpoints will
 * need to track which node each completion was sent to.
 *
 * API reference:
 * https://developers.openai.com/api/reference/resources/chat/subresources/completions/methods/create
 *
 * Fields we read from the create request:
 * - "model": not needed yet, but useful if we end up serving multiple models.
 * - "messages": at least one is required. Each has a "content" that is either
 *   a string or a non-empty array of content parts.
 */

/// Unknown fields are ignored, so only what we read needs to be listed here.
#[derive(Debug, Deserialize)]
struct CreateChatCompletionRequest {
    model: String,
    messages: Vec<Message>,
}

#[derive(Debug, Deserialize)]
struct Message {
    /// Assistant messages may send null or omit this when they have tool calls.
    content: Option<Content>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum Content {
    TextContent(String),
    ArrayOfContentParts(Vec<ContentPart>),
}

/// Text parts only: image, audio, and file parts have no `text`, so they fail
/// to deserialize and the request is rejected.
#[derive(Debug, Deserialize)]
struct ContentPart {
    text: String,
}

async fn create_chat_completion() -> String {
    "create chat".to_string()
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
