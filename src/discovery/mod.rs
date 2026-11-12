pub mod fixed;
mod healthcheck;
pub use fixed::FixedDiscoverer;
pub use healthcheck::HealthCheckConfig;

use std::net::SocketAddr;
use tokio::sync::mpsc;

/// Whether a node can take requests. Discoverers send these after each health
/// check, and request handlers send `Unresponsive` when a request to a node
/// fails. The selector adds a node the first time it hears about it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscoveryEvent {
    /// Route requests to the node.
    Responsive(SocketAddr),
    /// Stop routing to the node. Selectors keep it (and any state about it),
    /// since it may come back, but may drop it if it stays unresponsive.
    Unresponsive(SocketAddr),
}

/// Trait for discovering instances
pub trait Discoverer: Send + 'static {
    fn run(
        self,
        tx: mpsc::Sender<DiscoveryEvent>,
    ) -> impl Future<Output = anyhow::Result<()>> + Send;
}
