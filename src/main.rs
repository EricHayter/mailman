use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::Arc,
    time::Duration,
};

use axum::Router;
use tokio::{sync::mpsc, task::JoinHandle};
use tracing::info;

use discovery::{
    Discoverer, FixedDiscoverer, HealthCheckConfig, KubernetesConfig, KubernetesDiscoverer,
};
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

    let discovery = match kubernetes_config()? {
        Some(config) => spawn_discovery(
            KubernetesDiscoverer::new(config, client.clone(), HealthCheckConfig::default()),
            selector.clone(),
        ),
        None => {
            info!(backends = ?BACKENDS, "using fixed backends");
            spawn_discovery(
                FixedDiscoverer::new(
                    BACKENDS.to_vec(),
                    client.clone(),
                    HealthCheckConfig::default(),
                ),
                selector.clone(),
            )
        }
    };

    let app = telemetry::http_layers(Router::new().merge(chat_completion::routes()))
        .with_state(AppState { client, selector });
    let listener = tokio::net::TcpListener::bind(ADDRESS).await?;
    info!(address = ADDRESS, "listening");

    tokio::select! {
        result = axum::serve(listener, app).into_future() => result?,
        result = discovery => {
            result??;
            anyhow::bail!("discovery stopped unexpectedly");
        }
    }

    Ok(())
}

/// Kubernetes discovery is used when `MAILMAN_K8S_SERVICE` is set, otherwise
/// the fixed `BACKENDS`.
fn kubernetes_config() -> anyhow::Result<Option<KubernetesConfig>> {
    let var = |name| {
        std::env::var(name)
            .ok()
            .filter(|value: &String| !value.is_empty())
    };
    let Some(service) = var("MAILMAN_K8S_SERVICE") else {
        return Ok(None);
    };
    let pod_labels = var("MAILMAN_K8S_POD_LABELS").ok_or_else(|| {
        anyhow::anyhow!("MAILMAN_K8S_POD_LABELS must be set with MAILMAN_K8S_SERVICE")
    })?;
    Ok(Some(KubernetesConfig {
        namespace: var("MAILMAN_K8S_NAMESPACE"),
        service,
        pod_labels,
        port_name: var("MAILMAN_K8S_PORT_NAME"),
    }))
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
