pub mod round_robin;

use std::net::SocketAddr;

use crate::discovery::DiscoveryEvent;

use crate::models::CreateChatCompletionRequest;

pub trait NodeSelector: Sync + Send {
    fn handle_discovery_event(&self, event: DiscoveryEvent);
    fn select(&self, req: &CreateChatCompletionRequest) -> Option<SocketAddr>;
}
