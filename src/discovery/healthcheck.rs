use reqwest::Client;
use std::{net::SocketAddr, time::Duration};
use tokio::{sync::mpsc, time::MissedTickBehavior};

use tracing::{debug, info, warn};

use super::DiscoveryEvent;
use crate::telemetry::error_chain;

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
    info!(%addr, interval = ?config.interval, timeout = ?config.timeout, "monitoring node");

    // Only this task knows why a check failed, so it warns with the reason
    // when checks start failing, and logs the rest of a run of failures at
    // debug. The selector logs the node's state changes.
    let mut failing = false;

    loop {
        interval.tick().await;

        let event = match check_health(&client, addr, config.timeout).await {
            Ok(()) => {
                failing = false;
                DiscoveryEvent::Responsive(addr)
            }
            Err(error) => {
                if failing {
                    debug!(%addr, %error, "health check failed");
                } else {
                    warn!(%addr, %error, "health check failed");
                }
                failing = true;
                DiscoveryEvent::Unresponsive(addr)
            }
        };

        if tx.send(event).await.is_err() {
            return; // receiver gone, shutting down
        }
    }
}

/// Returns why the check failed, for logging.
async fn check_health(client: &Client, addr: SocketAddr, timeout: Duration) -> Result<(), String> {
    let response = client
        .get(format!("http://{addr}/health"))
        .timeout(timeout)
        .send()
        .await
        .map_err(|err| error_chain(&err))?;
    if response.status().is_success() {
        Ok(())
    } else {
        Err(format!("status {}", response.status()))
    }
}
