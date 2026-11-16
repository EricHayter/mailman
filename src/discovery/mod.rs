pub mod fixed;
mod healthcheck;
pub mod kubernetes;
pub use fixed::FixedDiscoverer;
pub use healthcheck::HealthCheckConfig;
pub use kubernetes::{KubernetesConfig, KubernetesDiscoverer};

use std::net::SocketAddr;
use tokio::sync::mpsc;

/// Whether a node can take requests. Discoverers send these after each health
/// check, and request handlers send `Unresponsive` when a request to a node
/// fails. The selector adds a node the first time it hears `Responsive`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscoveryEvent {
    /// Route requests to the node.
    Responsive(SocketAddr),
    /// Stop routing to the node. Selectors keep it (and any state about it),
    /// since it may come back, but may drop it if it stays unresponsive.
    Unresponsive(SocketAddr),
    /// The node is gone, or was replaced by one with an empty cache (e.g. its
    /// container restarted). Selectors drop it and any state about it. Its
    /// monitor has stopped, so no `Responsive` for this node follows unless it
    /// is discovered again.
    Removed(SocketAddr),
}

/// Trait for discovering instances
pub trait Discoverer: Send + 'static {
    fn run(
        self,
        tx: mpsc::Sender<DiscoveryEvent>,
    ) -> impl Future<Output = anyhow::Result<()>> + Send;
}
