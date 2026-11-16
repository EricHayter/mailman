use reqwest::Client;
use std::{collections::HashMap, net::SocketAddr};
use tokio::sync::mpsc;

use super::{
    Discoverer, DiscoveryEvent,
    healthcheck::{HealthCheckConfig, HealthChecker},
};

pub struct FixedDiscoverer {
    addrs: Vec<SocketAddr>,
    http_client: Client,
    health_check_cfg: HealthCheckConfig,
}

impl FixedDiscoverer {
    pub fn new(
        addrs: Vec<SocketAddr>,
        http_client: Client,
        health_check_cfg: HealthCheckConfig,
    ) -> Self {
        Self {
            addrs,
            http_client,
            health_check_cfg,
        }
    }
}

impl Discoverer for FixedDiscoverer {
    async fn run(self, tx: mpsc::Sender<DiscoveryEvent>) -> anyhow::Result<()> {
        let mut health_checker =
            HealthChecker::new(self.http_client, self.health_check_cfg, tx.clone());
        let desired = self
            .addrs
            .into_iter()
            .map(|addr| (addr, 0))
            .collect::<HashMap<_, _>>();
        health_checker.set_nodes(&desired).await;

        tx.closed().await;
        Ok(())
    }
}
