use reqwest::Client;
use std::{net::SocketAddr, time::Duration};
use tokio::{sync::mpsc, time::MissedTickBehavior};

use super::DiscoveryEvent;

#[derive(Debug, Clone, Copy)]
pub struct HealthCheckConfig {
    /// Time between checks. Must be non-zero (tokio's `interval` panics on zero).
    pub interval: Duration,
    /// How long a check waits for a response before counting as failed.
    pub timeout: Duration,
}

impl Default for HealthCheckConfig {
    fn default() -> Self {
        Self {
            interval: Duration::from_secs(2),
            timeout: Duration::from_secs(2),
        }
    }
}

/// Checks a node every `interval` until the receiver is dropped, sending
/// `Responsive` or `Unresponsive` after every check rather than only when
/// the result changes, so a node that a request handler marked `Unresponsive`
/// comes back on its next passing check.
pub async fn monitor_node(
    client: Client,
    addr: SocketAddr,
    config: HealthCheckConfig,
    tx: mpsc::Sender<DiscoveryEvent>,
) {
    let mut interval = tokio::time::interval(config.interval);
    interval.set_missed_tick_behavior(MissedTickBehavior::Delay);

    loop {
        interval.tick().await;

        let event = if is_healthy(&client, addr, config.timeout).await {
            DiscoveryEvent::Responsive(addr)
        } else {
            DiscoveryEvent::Unresponsive(addr)
        };

        if tx.send(event).await.is_err() {
            return; // receiver gone, shutting down
        }
    }
}

async fn is_healthy(client: &Client, addr: SocketAddr, timeout: Duration) -> bool {
    client
        .get(format!("http://{addr}/health"))
        .timeout(timeout)
        .send()
        .await
        .is_ok_and(|response| response.status().is_success())
}
