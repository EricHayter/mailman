pub mod cache_aware;
pub mod round_robin;

use std::net::SocketAddr;

use crate::{discovery::DiscoveryEvent, models::CreateChatCompletionRequest};

pub trait NodeSelector: Sync + Send {
    fn handle_discovery_event(&self, event: DiscoveryEvent);
    /// Picks a responsive node that isn't in `exclude`, which holds the nodes
    /// this request has already been tried on.
    fn select(
        &self,
        req: &CreateChatCompletionRequest,
        exclude: &[SocketAddr],
    ) -> Option<SocketAddr>;
}
