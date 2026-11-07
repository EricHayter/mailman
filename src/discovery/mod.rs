pub mod fixed;
mod healthcheck;
pub use fixed::FixedDiscoverer;

use std::net::SocketAddr;
use tokio::sync::mpsc;

// TODO: figure out the best way to hint that a node might be down from failed
// client requests, so we don't rely exclusively on healthchecks. A single
// failed request should only mark the node as suspect, not remove it, so this
// probably needs a separate event from `Removed`. Failing healthchecks for ~6s
// can keep meaning the node is gone.
pub enum DiscoveryEvent {
    Added(SocketAddr),
    Removed(SocketAddr),
}

/// Trait for discovering instances
pub trait Discoverer: Send + 'static {
    fn run(
        self,
        tx: mpsc::Sender<DiscoveryEvent>,
    ) -> impl Future<Output = anyhow::Result<()>> + Send;
}
