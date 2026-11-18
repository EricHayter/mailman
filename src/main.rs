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
use routes::{AppState, chat_completion, stats as stats_routes};
use selector::{
    NodeSelector,
    cache_aware::{CacheAwareConfig, CacheAwareSelector},
    round_robin::RoundRobinSelector,
};
use stats::{StatsCollector, StatsConfig};

mod discovery;
mod models;
mod routes;
mod selector;
mod stats;
mod telemetry;

const ADDRESS: &str = "0.0.0.0:3000";
/// Used when neither `MAILMAN_K8S_SERVICE` nor `MAILMAN_BACKENDS` is set.
const DEFAULT_BACKEND: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 8000);
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
    let selector = selector_from_env()?;
    let stats = Arc::new(StatsCollector::new(client.clone(), StatsConfig::default()));
    tokio::spawn({
        let stats = stats.clone();
        async move { stats.run_scraper().await }
    });

    let discovery = match kubernetes_config()? {
        Some(config) => spawn_discovery(
            KubernetesDiscoverer::new(config, client.clone(), HealthCheckConfig::default()),
            selector.clone(),
            stats.clone(),
        ),
        None => {
            let backends = fixed_backends().await?;
            info!(?backends, "using fixed backends");
            spawn_discovery(
                FixedDiscoverer::new(backends, client.clone(), HealthCheckConfig::default()),
                selector.clone(),
                stats.clone(),
            )
        }
    };

    let app = telemetry::http_layers(
        Router::new()
            .merge(chat_completion::routes())
            .merge(stats_routes::routes()),
    )
    .with_state(AppState {
        client,
        selector,
        stats,
    });
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

/// `MAILMAN_SELECTOR` picks how requests are spread over nodes:
/// `cache_aware` (the default) or `round_robin`.
fn selector_from_env() -> anyhow::Result<Arc<dyn NodeSelector>> {
    let name = std::env::var("MAILMAN_SELECTOR").unwrap_or_default();
    let selector: Arc<dyn NodeSelector> = match name.as_str() {
        "" | "cache_aware" => Arc::new(CacheAwareSelector::new(CacheAwareConfig::default())),
        "round_robin" => Arc::new(RoundRobinSelector::default()),
        other => {
            anyhow::bail!("unknown MAILMAN_SELECTOR {other:?}, expected cache_aware or round_robin")
        }
    };
    info!(
        selector = if name.is_empty() {
            "cache_aware"
        } else {
            &name
        },
        "routing"
    );
    Ok(selector)
}

/// `MAILMAN_BACKENDS`, a comma-separated list of `host:port`, each host
/// resolved once at startup (e.g. Docker Compose service names), or
/// `DEFAULT_BACKEND` if it isn't set.
async fn fixed_backends() -> anyhow::Result<Vec<SocketAddr>> {
    let list = std::env::var("MAILMAN_BACKENDS").unwrap_or_default();
    if list.trim().is_empty() {
        return Ok(vec![DEFAULT_BACKEND]);
    }
    let mut backends = Vec::new();
    for entry in list
        .split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
    {
        let addr = tokio::net::lookup_host(entry)
            .await
            .map_err(|err| {
                anyhow::anyhow!("couldn't resolve MAILMAN_BACKENDS entry {entry:?}: {err}")
            })?
            // vLLM listens on IPv4 by default, so prefer it for names like
            // `localhost` that resolve to both.
            .min_by_key(SocketAddr::is_ipv6)
            .ok_or_else(|| {
                anyhow::anyhow!("MAILMAN_BACKENDS entry {entry:?} resolved to nothing")
            })?;
        backends.push(addr);
    }
    Ok(backends)
}

/// Kubernetes discovery is used when `MAILMAN_K8S_SERVICE` is set, otherwise
/// fixed backends.
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

/// Feeds discovery events to the selector and stats in the background. The
/// returned handle resolves if discovery stops.
fn spawn_discovery(
    discoverer: impl Discoverer,
    selector: Arc<dyn NodeSelector>,
    stats: Arc<StatsCollector>,
) -> JoinHandle<anyhow::Result<()>> {
    let (tx, mut rx) = mpsc::channel(32);

    tokio::spawn(async move {
        while let Some(event) = rx.recv().await {
            selector.handle_discovery_event(event);
            stats.handle_discovery_event(event);
        }
    });

    tokio::spawn(discoverer.run(tx))
}
