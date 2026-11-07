use std::sync::Arc;

use crate::selector::NodeSelector;

pub mod chat_completion;

#[derive(Clone)]
pub struct AppState {
    pub client: reqwest::Client,
    pub selector: Arc<dyn NodeSelector>,
}
