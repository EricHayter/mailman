use axum::{
    Router,
    routing::method_routing::{delete, get, post},
};

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
