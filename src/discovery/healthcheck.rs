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

pub async fn monitor_node(client: Client, addr: SocketAddr, tx: mpsc::Sender<DiscoveryEvent>) {
    let mut interval = tokio::time::interval(CHECK_INTERVAL);
    interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut failures = MAX_FAILURES; // down until a check passes
    let mut was_alive = false;

    loop {
        interval.tick().await;

        if is_node_alive(&client, addr).await {
            failures = 0;
        } else {
            failures += 1;
        }

        let alive = failures < MAX_FAILURES;
        if alive == was_alive {
            continue;
        }
        was_alive = alive;

        let event = if alive {
            DiscoveryEvent::Added(addr)
        } else {
            DiscoveryEvent::Removed(addr)
        };
        if tx.send(event).await.is_err() {
            return; // receiver gone, shutting down
        }
    }
}
