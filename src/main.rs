use axum::Router;

mod routes;
use routes::chat_completion;

const ADDRESS: &str = "0.0.0.0:3000";

#[tokio::main]
async fn main() {
    let app = Router::new()
        .merge(chat_completion::routes());

    let listener = tokio::net::TcpListener::bind(ADDRESS).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}
