use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::Arc,
    time::Duration,
};

use axum::Router;
use tokio::{sync::mpsc, task::JoinHandle};
use tracing::info;

use discovery::{Discoverer, FixedDiscoverer, HealthCheckConfig};
use routes::{AppState, chat_completion};
use selector::{
    NodeSelector,
    cache_aware::{CacheAwareConfig, CacheAwareSelector},
};

mod discovery;
mod models;
mod routes;
mod selector;
mod telemetry;

const ADDRESS: &str = "0.0.0.0:3000";
const BACKENDS: [SocketAddr; 1] = [SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 8000)];
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// Maximum gap between reads, not a limit on the whole request. A non-streaming
/// completion sends nothing until it finishes, so this has to outlast the
/// longest generation.
const READ_TIMEOUT: Duration = Duration::from_secs(300);

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    telemetry::init();

    let client = reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .read_timeout(READ_TIMEOUT)
        .build()?;
    let selector: Arc<dyn NodeSelector> =
        Arc::new(CacheAwareSelector::new(CacheAwareConfig::default()));

    let discoverer = FixedDiscoverer::new(
        BACKENDS.to_vec(),
        client.clone(),
        HealthCheckConfig::default(),
    );
    let discovery = spawn_discovery(discoverer, selector.clone());

    let app = telemetry::http_layers(Router::new().merge(chat_completion::routes()))
        .with_state(AppState { client, selector });
    let listener = tokio::net::TcpListener::bind(ADDRESS).await?;
    info!(address = ADDRESS, backends = ?BACKENDS, "listening");

    tokio::select! {
        result = axum::serve(listener, app).into_future() => result?,
        result = discovery => {
            result??;
            anyhow::bail!("discovery stopped unexpectedly");
        }
    }

    Ok(())
}

/// Feeds discovery events to the selector in the background. The returned
/// handle resolves if discovery stops.
fn spawn_discovery(
    discoverer: impl Discoverer,
    selector: Arc<dyn NodeSelector>,
) -> JoinHandle<anyhow::Result<()>> {
    let (tx, mut rx) = mpsc::channel(32);

    tokio::spawn(async move {
        while let Some(event) = rx.recv().await {
            selector.handle_discovery_event(event);
        }
    });

    tokio::spawn(discoverer.run(tx))
}
