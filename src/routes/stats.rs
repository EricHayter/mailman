use axum::{Json, Router, extract::State, routing::get};

use super::AppState;
use crate::stats::StatsReport;

pub fn routes() -> Router<AppState> {
    Router::new().route("/stats", get(get_stats))
}

async fn get_stats(State(state): State<AppState>) -> Json<StatsReport> {
    Json(state.stats.report())
}
