use reqwest::Client;
use std::{collections::HashMap, net::SocketAddr, time::Duration};
use tokio::{sync::mpsc, task::JoinHandle, time::MissedTickBehavior};

use tracing::{debug, error, info, warn};

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

/// What makes a node the same node across discovery updates, such as a hash
/// of its pod's container IDs. When it changes the node is treated as new,
/// since its cache is gone. 0 for nodes with no such identity, like fixed ones.
pub type Identity = u64;

/// Health-checks the nodes a discoverer currently knows about, one monitor
/// task per node.
pub struct HealthChecker {
    client: Client,
    config: HealthCheckConfig,
    tx: mpsc::Sender<DiscoveryEvent>,
    running: HashMap<SocketAddr, (Identity, JoinHandle<()>)>,
}

impl HealthChecker {
    pub fn new(
        client: Client,
        config: HealthCheckConfig,
        tx: mpsc::Sender<DiscoveryEvent>,
    ) -> Self {
        Self {
            client,
            config,
            tx,
            running: HashMap::new(),
        }
    }

    /// Stops monitoring nodes that are gone or whose identity changed, sending
    /// `Removed` for each, then starts monitoring the new ones.
    pub async fn set_nodes(&mut self, desired: &HashMap<SocketAddr, Identity>) {
        let stale: Vec<SocketAddr> = self
            .running
            .iter()
            .filter(|&(addr, (identity, _))| desired.get(addr) != Some(identity))
            .map(|(&addr, _)| addr)
            .collect();
        for addr in stale {
            let (_, handle) = self.running.remove(&addr).expect("collected from running");
            self.stop(addr, handle).await;
        }

        for (&addr, identity) in desired {
            if self.running.contains_key(&addr) {
                continue;
            }
            let handle = tokio::spawn(monitor_node(
                self.client.clone(),
                addr,
                self.config,
                self.tx.clone(),
            ));
            self.running.insert(addr, (*identity, handle));
        }
    }

    async fn stop(&self, addr: SocketAddr, handle: JoinHandle<()>) {
        handle.abort();
        // Waiting for the task to end means none of its events can arrive
        // after `Removed`.
        if let Err(err) = handle.await
            && err.is_panic()
        {
            error!(%addr, error = %err, "node monitor panicked");
        }
        info!(%addr, "stopped monitoring node");
        // An error means the receiver is gone, i.e. shutting down.
        let _ = self.tx.send(DiscoveryEvent::Removed(addr)).await;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn addr() -> SocketAddr {
        // Nothing listens on port 1, so checks fail fast.
        SocketAddr::from(([127, 0, 0, 1], 1))
    }

    fn health_checker(tx: mpsc::Sender<DiscoveryEvent>) -> HealthChecker {
        let config = HealthCheckConfig {
            interval: Duration::from_millis(10),
            timeout: Duration::from_millis(100),
        };
        HealthChecker::new(Client::new(), config, tx)
    }

    /// Receives events until `Removed`, returning how many came before it.
    async fn events_before_removed(rx: &mut mpsc::Receiver<DiscoveryEvent>) -> usize {
        let mut count = 0;
        loop {
            match rx.recv().await.unwrap() {
                DiscoveryEvent::Removed(removed) => {
                    assert_eq!(removed, addr());
                    return count;
                }
                event => {
                    assert_eq!(event, DiscoveryEvent::Unresponsive(addr()));
                    count += 1;
                }
            }
        }
    }

    #[tokio::test]
    async fn identity_change_removes_then_restarts_monitor() {
        let (tx, mut rx) = mpsc::channel(64);
        let mut health_checker = health_checker(tx);

        health_checker
            .set_nodes(&HashMap::from([(addr(), 1)]))
            .await;
        assert_eq!(rx.recv().await, Some(DiscoveryEvent::Unresponsive(addr())));

        // The same identity again changes nothing.
        health_checker
            .set_nodes(&HashMap::from([(addr(), 1)]))
            .await;
        health_checker
            .set_nodes(&HashMap::from([(addr(), 2)]))
            .await;
        events_before_removed(&mut rx).await;
        // The new monitor runs.
        assert_eq!(rx.recv().await, Some(DiscoveryEvent::Unresponsive(addr())));
    }

    #[tokio::test]
    async fn removed_node_sends_nothing_after_removed() {
        let (tx, mut rx) = mpsc::channel(64);
        let mut health_checker = health_checker(tx);

        health_checker
            .set_nodes(&HashMap::from([(addr(), 0)]))
            .await;
        health_checker.set_nodes(&HashMap::new()).await;
        events_before_removed(&mut rx).await;

        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(rx.try_recv().is_err());
    }
}
