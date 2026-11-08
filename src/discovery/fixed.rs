use reqwest::Client;
use std::net::SocketAddr;
use tokio::{sync::mpsc, task::JoinSet};

use super::{
    Discoverer, DiscoveryEvent,
    healthcheck::{HealthCheckConfig, monitor_node},
};

pub struct FixedDiscoverer {
    addrs: Vec<SocketAddr>,
    client: Client,
    health_check: HealthCheckConfig,
}

impl FixedDiscoverer {
    pub fn new(addrs: Vec<SocketAddr>, client: Client, health_check: HealthCheckConfig) -> Self {
        Self {
            addrs,
            client,
            health_check,
        }
    }
}

impl Discoverer for FixedDiscoverer {
    async fn run(self, tx: mpsc::Sender<DiscoveryEvent>) -> anyhow::Result<()> {
        // TODO: look at stopping monitors individually (e.g. a map of address to
        // `AbortHandle`), which discoverers with changing nodes like Kubernetes
        // will need. Also, this set is never joined, so a panicking monitor
        // goes unnoticed and its node stays in its last state.
        let mut monitors = JoinSet::new();
        for &addr in &self.addrs {
            monitors.spawn(monitor_node(
                self.client.clone(),
                addr,
                self.health_check,
                tx.clone(),
            ));
        }

        tx.closed().await;
        Ok(())
    }
}
