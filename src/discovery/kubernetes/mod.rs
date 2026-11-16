//! Discovers vLLM nodes from a Kubernetes Service's EndpointSlices.
//!
//! EndpointSlices say which pods back the Service and whether they're
//! terminating. Pods supply each node's identity, its container IDs, since a
//! container restart empties the vLLM cache without changing the pod or its
//! address. Routing still follows our own health checks, so if the API server
//! becomes unreachable the last-known nodes keep being checked and routed to;
//! only discovery pauses.

mod endpoints;

use std::fmt::Debug;

use futures_util::{
    StreamExt,
    stream::{self, BoxStream},
};
use k8s_openapi::api::{core::v1::Pod, discovery::v1::EndpointSlice};
use kube::{
    Api, Client, Resource,
    runtime::{WatchStreamExt, reflector, reflector::Store, watcher},
};
use reqwest::Client as HttpClient;
use serde::de::DeserializeOwned;
use tokio::sync::mpsc;
use tracing::{info, warn};

use super::{
    Discoverer, DiscoveryEvent,
    healthcheck::{HealthCheckConfig, HealthChecker},
};
use crate::telemetry::error_chain;
use endpoints::desired_endpoints;

#[derive(Debug, Clone)]
pub struct KubernetesConfig {
    /// Defaults to the client's namespace, which in a cluster is mailman's own.
    pub namespace: Option<String>,
    /// The Service whose endpoints are the vLLM nodes.
    pub service: String,
    /// Label selector for the Service's pods, e.g. `app=vllm`. Only used to
    /// limit the pod watch; membership comes from the EndpointSlices.
    pub pod_labels: String,
    /// The Service port to use. May be left out if the Service has one port.
    pub port_name: Option<String>,
}

pub struct KubernetesDiscoverer {
    config: KubernetesConfig,
    http_client: HttpClient,
    health_check_cfg: HealthCheckConfig,
}

impl KubernetesDiscoverer {
    pub fn new(
        config: KubernetesConfig,
        http_client: HttpClient,
        health_check_cfg: HealthCheckConfig,
    ) -> Self {
        Self {
            config,
            http_client,
            health_check_cfg,
        }
    }

    /// Starts watching the Service's EndpointSlices and its pods, each into a
    /// store.
    fn watch_service(&self, client: Client) -> (Store<EndpointSlice>, Store<Pod>, StoreChanges) {
        let namespace = self
            .config
            .namespace
            .clone()
            .unwrap_or_else(|| client.default_namespace().to_string());
        info!(
            %namespace,
            service = %self.config.service,
            pod_labels = %self.config.pod_labels,
            "watching kubernetes endpoints"
        );

        let service_label = format!("kubernetes.io/service-name={}", self.config.service);
        let (slices, slice_changes) =
            watch_into_store(Api::namespaced(client.clone(), &namespace), &service_label);
        let (pods, pod_changes) =
            watch_into_store(Api::namespaced(client, &namespace), &self.config.pod_labels);
        let changes = stream::select(slice_changes, pod_changes).boxed();
        (slices, pods, changes)
    }
}

impl Discoverer for KubernetesDiscoverer {
    async fn run(self, tx: mpsc::Sender<DiscoveryEvent>) -> anyhow::Result<()> {
        let client = Client::try_default().await?;
        let (slices, pods, mut changes) = self.watch_service(client);

        let mut health_checker =
            HealthChecker::new(self.http_client, self.health_check_cfg, tx.clone());
        loop {
            let change = tokio::select! {
                _ = tx.closed() => return Ok(()),
                change = changes.next() => change,
            };
            match change {
                // With backoff, the watchers retry forever rather than end.
                None => anyhow::bail!("kubernetes watch ended"),
                Some(Err(err)) => {
                    warn!(
                        error = error_chain(&err),
                        "kubernetes watch failed, keeping current nodes"
                    );
                }
                Some(Ok(false)) => {}
                Some(Ok(true)) => {
                    let desired = desired_endpoints(
                        &slices.state(),
                        &pods.state(),
                        self.config.port_name.as_deref(),
                    );
                    health_checker.set_nodes(&desired).await;
                }
            }
        }
    }
}

/// For each watch event, whether it changed the store.
type StoreChanges = BoxStream<'static, Result<bool, watcher::Error>>;

/// Watches the resources matching `labels` into a store.
fn watch_into_store<K>(api: Api<K>, labels: &str) -> (Store<K>, StoreChanges)
where
    K: Resource<DynamicType = ()> + Clone + DeserializeOwned + Debug + Send + Sync + 'static,
{
    let (store, writer) = reflector::store();
    let changes = watcher(api, watcher::Config::default().labels(labels))
        .default_backoff()
        .reflect(writer)
        .map(|event| event.map(|event| changes_store(&event)))
        .boxed();
    (store, changes)
}

/// Whether the store has changed. During a relist the store keeps its old
/// contents until `InitDone`, so a relist never looks like everything vanished.
fn changes_store<K>(event: &watcher::Event<K>) -> bool {
    matches!(
        event,
        watcher::Event::Apply(_) | watcher::Event::Delete(_) | watcher::Event::InitDone
    )
}
