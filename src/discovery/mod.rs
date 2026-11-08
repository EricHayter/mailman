pub mod fixed;
mod healthcheck;
pub use fixed::FixedDiscoverer;
pub use healthcheck::HealthCheckConfig;

use std::net::SocketAddr;
use tokio::sync::mpsc;

/// Changes to which nodes exist and whether they can take requests.
/// Discoverers send these, and request handlers send `Unresponsive` when a
/// request to a node fails.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscoveryEvent {
    /// The node exists and can take requests.
    Added(SocketAddr),
    /// A known node can take requests again.
    Responsive(SocketAddr),
    /// Stop routing to the node, but keep it (and any state about it), since
    /// it may come back.
    Unresponsive(SocketAddr),
    /// The node is gone.
    Removed(SocketAddr),
}

/// Trait for discovering instances
pub trait Discoverer: Send + 'static {
    fn run(
        self,
        tx: mpsc::Sender<DiscoveryEvent>,
    ) -> impl Future<Output = anyhow::Result<()>> + Send;
}
