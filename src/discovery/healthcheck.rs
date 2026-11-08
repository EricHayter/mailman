use reqwest::Client;
use std::{net::SocketAddr, time::Duration};
use tokio::{sync::mpsc, time::MissedTickBehavior};

use super::DiscoveryEvent;

const CHECK_INTERVAL: Duration = Duration::from_secs(2);
const CHECK_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_FAILURES: u32 = 3;

async fn is_node_alive(client: &Client, addr: SocketAddr) -> bool {
    client
        .get(format!("http://{addr}/health"))
        .timeout(CHECK_TIMEOUT)
        .send()
        .await
        .is_ok_and(|res| res.status().is_success())
}

/// Sends an event after every check, not just when the result changes, so a
/// node that a request handler marked `Unresponsive` comes back once it passes.
pub async fn monitor_node(client: Client, addr: SocketAddr, tx: mpsc::Sender<DiscoveryEvent>) {
    let mut interval = tokio::time::interval(CHECK_INTERVAL);
    interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut health = NodeHealth::default();

    loop {
        interval.tick().await;

        let alive = is_node_alive(&client, addr).await;
        if let Some(event) = health.record(alive, addr)
            && tx.send(event).await.is_err()
        {
            return; // receiver gone, shutting down
        }
    }
}

/// Turns health check results into events. One failed check stops traffic to
/// the node, and `MAX_FAILURES` in a row removes it.
#[derive(Default)]
struct NodeHealth {
    failures: u32,
    added: bool,
}

impl NodeHealth {
    fn record(&mut self, alive: bool, addr: SocketAddr) -> Option<DiscoveryEvent> {
        if alive {
            self.failures = 0;
            if self.added {
                return Some(DiscoveryEvent::Responsive(addr));
            }
            self.added = true;
            return Some(DiscoveryEvent::Added(addr));
        }

        if !self.added {
            return None;
        }
        self.failures += 1;
        if self.failures >= MAX_FAILURES {
            self.added = false;
            Some(DiscoveryEvent::Removed(addr))
        } else {
            Some(DiscoveryEvent::Unresponsive(addr))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use DiscoveryEvent::*;

    const ADDR: SocketAddr =
        SocketAddr::new(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST), 1);

    #[test]
    fn first_pass_adds_then_later_passes_mark_responsive() {
        let mut health = NodeHealth::default();
        assert_eq!(health.record(true, ADDR), Some(Added(ADDR)));
        assert_eq!(health.record(true, ADDR), Some(Responsive(ADDR)));
    }

    #[test]
    fn failures_before_first_pass_send_nothing() {
        let mut health = NodeHealth::default();
        for _ in 0..MAX_FAILURES + 1 {
            assert_eq!(health.record(false, ADDR), None);
        }
    }

    #[test]
    fn failures_mark_unresponsive_then_remove() {
        let mut health = NodeHealth::default();
        health.record(true, ADDR);

        for _ in 1..MAX_FAILURES {
            assert_eq!(health.record(false, ADDR), Some(Unresponsive(ADDR)));
        }
        assert_eq!(health.record(false, ADDR), Some(Removed(ADDR)));
        assert_eq!(health.record(false, ADDR), None);
    }

    #[test]
    fn pass_resets_failures() {
        let mut health = NodeHealth::default();
        health.record(true, ADDR);
        health.record(false, ADDR);
        assert_eq!(health.record(true, ADDR), Some(Responsive(ADDR)));

        for _ in 1..MAX_FAILURES {
            assert_eq!(health.record(false, ADDR), Some(Unresponsive(ADDR)));
        }
    }

    #[test]
    fn recovery_after_removal_adds_again() {
        let mut health = NodeHealth::default();
        health.record(true, ADDR);
        for _ in 0..MAX_FAILURES {
            health.record(false, ADDR);
        }
        assert_eq!(health.record(true, ADDR), Some(Added(ADDR)));
    }
}
