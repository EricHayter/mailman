//! Works out which nodes to health-check from the Service's EndpointSlices
//! and pods.

use std::{
    collections::HashMap,
    hash::{DefaultHasher, Hash, Hasher},
    net::{IpAddr, SocketAddr},
    sync::Arc,
};

use k8s_openapi::api::{
    core::v1::Pod,
    discovery::v1::{Endpoint, EndpointSlice},
};
use kube::ResourceExt;
use tracing::{debug, warn};

use crate::discovery::healthcheck::Identity;

/// A pod's namespace and name.
type PodKey = (String, String);

/// The nodes to monitor: every endpoint that isn't terminating, on the chosen
/// port, with its pod's container IDs as its identity. `ready` is ignored on
/// purpose, since our health checks decide routing, and dropping not-ready
/// endpoints would throw away a node's cache state whenever its readiness
/// probe flaps.
pub(super) fn desired_endpoints(
    slices: &[Arc<EndpointSlice>],
    pods: &[Arc<Pod>],
    port_name: Option<&str>,
) -> HashMap<SocketAddr, Identity> {
    let pods: HashMap<PodKey, &Pod> = pods
        .iter()
        .map(|pod| (pod_key(pod), pod.as_ref()))
        .collect();

    // Keyed by pod, so a dual-stack pod (one IPv4 and one IPv6 slice) is one
    // node, not two sharing a cache. Keeps the lowest address, so the choice
    // is stable.
    let mut by_pod: HashMap<PodKey, (SocketAddr, Identity)> = HashMap::new();
    for slice in slices {
        let Some(port) = slice_port(slice, port_name) else {
            continue;
        };
        for (key, addr) in live_pod_endpoints(slice, port) {
            let Some(identity) = pods.get(&key).and_then(|pod| container_ids(pod)) else {
                // Picked up when the pod watch catches up or its containers start.
                debug!(%addr, pod = %key.1, "pod not seen running yet, skipping");
                continue;
            };
            let entry = by_pod.entry(key).or_insert((addr, identity));
            entry.0 = entry.0.min(addr);
        }
    }
    by_pod.into_values().collect()
}

fn pod_key(pod: &Pod) -> PodKey {
    (pod.namespace().unwrap_or_default(), pod.name_any())
}

/// The slice's pod endpoints that aren't terminating, with their address on
/// `port`.
fn live_pod_endpoints(
    slice: &EndpointSlice,
    port: u16,
) -> impl Iterator<Item = (PodKey, SocketAddr)> + '_ {
    let namespace = slice.namespace().unwrap_or_default();
    slice
        .endpoints
        .iter()
        .filter(|endpoint| !is_terminating(endpoint))
        .filter_map(move |endpoint| pod_endpoint(endpoint, &namespace, port))
}

fn is_terminating(endpoint: &Endpoint) -> bool {
    endpoint.conditions.as_ref().and_then(|c| c.terminating) == Some(true)
}

/// The endpoint's pod and its address on `port`, or `None` if it isn't a pod.
fn pod_endpoint(
    endpoint: &Endpoint,
    slice_namespace: &str,
    port: u16,
) -> Option<(PodKey, SocketAddr)> {
    // FQDN slices have no IP, and aren't pods.
    let ip: IpAddr = endpoint.addresses.first()?.parse().ok()?;
    let addr = SocketAddr::new(ip, port);
    let Some(target) = endpoint
        .target_ref
        .as_ref()
        .filter(|target| target.kind.as_deref() == Some("Pod"))
    else {
        debug!(%addr, "endpoint isn't a pod, skipping");
        return None;
    };
    let namespace = target
        .namespace
        .clone()
        .unwrap_or_else(|| slice_namespace.to_string());
    Some(((namespace, target.name.clone().unwrap_or_default()), addr))
}

/// The port named `port_name`, or the slice's only port when no name is given.
fn slice_port(slice: &EndpointSlice, port_name: Option<&str>) -> Option<u16> {
    let ports = slice.ports.as_deref().unwrap_or_default();
    let port = match port_name {
        Some(name) => ports.iter().find(|port| port.name.as_deref() == Some(name)),
        None if ports.len() == 1 => ports.first(),
        None => None,
    };
    let port = port
        .and_then(|port| port.port)
        .and_then(|port| u16::try_from(port).ok());
    if port.is_none() && !slice.endpoints.is_empty() {
        warn!(
            slice = %slice.name_any(),
            ?port_name,
            ports = ports.len(),
            "no usable port in endpoint slice, set a port name if the service has several"
        );
    }
    port
}

/// A hash of the pod's container IDs. `None` until a container has started.
fn container_ids(pod: &Pod) -> Option<Identity> {
    let statuses = pod.status.as_ref()?.container_statuses.as_deref()?;
    let ids: Vec<&str> = statuses
        .iter()
        .filter_map(|s| s.container_id.as_deref())
        .collect();
    (!ids.is_empty()).then(|| hash_ids(ids))
}

/// Order-independent, since the API doesn't promise an order. Only compared
/// within this process, so `DefaultHasher` not being stable across Rust
/// versions doesn't matter.
fn hash_ids(mut ids: Vec<&str>) -> Identity {
    ids.sort_unstable();
    let mut hasher = DefaultHasher::new();
    ids.hash(&mut hasher);
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use k8s_openapi::{
        api::{
            core::v1::{ContainerStatus, ObjectReference, PodStatus},
            discovery::v1::{EndpointConditions, EndpointPort},
        },
        apimachinery::pkg::apis::meta::v1::ObjectMeta,
    };

    const NS: &str = "default";

    fn meta(name: &str) -> ObjectMeta {
        ObjectMeta {
            name: Some(name.into()),
            namespace: Some(NS.into()),
            ..Default::default()
        }
    }

    fn pod(name: &str, container_ids: &[&str]) -> Arc<Pod> {
        Arc::new(Pod {
            metadata: meta(name),
            status: Some(PodStatus {
                container_statuses: Some(
                    container_ids
                        .iter()
                        .map(|id| ContainerStatus {
                            container_id: Some(id.to_string()),
                            ..Default::default()
                        })
                        .collect(),
                ),
                ..Default::default()
            }),
            ..Default::default()
        })
    }

    fn endpoint(ip: &str, pod: &str, terminating: bool) -> Endpoint {
        Endpoint {
            addresses: vec![ip.into()],
            conditions: Some(EndpointConditions {
                terminating: Some(terminating),
                ..Default::default()
            }),
            target_ref: Some(ObjectReference {
                kind: Some("Pod".into()),
                name: Some(pod.into()),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn slice(name: &str, ports: &[(&str, i32)], endpoints: Vec<Endpoint>) -> Arc<EndpointSlice> {
        Arc::new(EndpointSlice {
            metadata: meta(name),
            address_type: "IPv4".into(),
            endpoints,
            ports: Some(
                ports
                    .iter()
                    .map(|&(name, port)| EndpointPort {
                        name: Some(name.into()),
                        port: Some(port),
                        ..Default::default()
                    })
                    .collect(),
            ),
        })
    }

    fn addr(s: &str) -> SocketAddr {
        s.parse().unwrap()
    }

    fn ids(ids: &[&str]) -> Identity {
        hash_ids(ids.to_vec())
    }

    #[test]
    fn merges_slices_and_skips_terminating() {
        let slices = [
            slice(
                "a",
                &[("http", 8000)],
                vec![endpoint("10.0.0.1", "p1", false)],
            ),
            slice(
                "b",
                &[("http", 8000)],
                vec![
                    endpoint("10.0.0.2", "p2", false),
                    endpoint("10.0.0.3", "p3", true),
                ],
            ),
        ];
        let pods = [pod("p1", &["c1"]), pod("p2", &["c2"]), pod("p3", &["c3"])];

        assert_eq!(
            desired_endpoints(&slices, &pods, None),
            HashMap::from([
                (addr("10.0.0.1:8000"), ids(&["c1"])),
                (addr("10.0.0.2:8000"), ids(&["c2"])),
            ])
        );
    }

    #[test]
    fn picks_port_by_name_or_only_port() {
        let pods = [pod("p1", &["c1"])];
        let two_ports = [slice(
            "a",
            &[("metrics", 9000), ("http", 8000)],
            vec![endpoint("10.0.0.1", "p1", false)],
        )];

        assert_eq!(
            desired_endpoints(&two_ports, &pods, Some("http")),
            HashMap::from([(addr("10.0.0.1:8000"), ids(&["c1"]))])
        );
        assert!(desired_endpoints(&two_ports, &pods, None).is_empty());
        assert!(desired_endpoints(&two_ports, &pods, Some("grpc")).is_empty());
    }

    #[test]
    fn skips_endpoints_without_a_running_pod() {
        let slices = [slice(
            "a",
            &[("http", 8000)],
            vec![
                endpoint("10.0.0.1", "p1", false),
                endpoint("10.0.0.2", "p2", false),
            ],
        )];
        // p1 is unknown, p2 has no started containers.
        let pods = [pod("p2", &[])];

        assert!(desired_endpoints(&slices, &pods, None).is_empty());
    }

    #[test]
    fn identity_follows_container_ids() {
        let slices = [slice(
            "a",
            &[("http", 8000)],
            vec![endpoint("10.0.0.1", "p1", false)],
        )];

        let before = desired_endpoints(&slices, &[pod("p1", &["b", "a"])], None);
        let after = desired_endpoints(&slices, &[pod("p1", &["a", "c"])], None);
        assert_eq!(before[&addr("10.0.0.1:8000")], ids(&["a", "b"]));
        assert_eq!(after[&addr("10.0.0.1:8000")], ids(&["a", "c"]));
    }

    #[test]
    fn dual_stack_pod_is_one_node() {
        let slices = [
            slice(
                "v4",
                &[("http", 8000)],
                vec![endpoint("10.0.0.1", "p1", false)],
            ),
            slice(
                "v6",
                &[("http", 8000)],
                vec![endpoint("fd00::1", "p1", false)],
            ),
        ];
        let pods = [pod("p1", &["c1"])];

        assert_eq!(
            desired_endpoints(&slices, &pods, None),
            HashMap::from([(addr("10.0.0.1:8000"), ids(&["c1"]))])
        );
    }
}
