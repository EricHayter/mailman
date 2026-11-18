use std::sync::Arc;

use crate::{selector::NodeSelector, stats::StatsCollector};

pub mod chat_completion;
pub mod stats;

#[derive(Clone)]
pub struct AppState {
    pub client: reqwest::Client,
    pub selector: Arc<dyn NodeSelector>,
    pub stats: Arc<StatsCollector>,
}
