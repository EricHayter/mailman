//! Per-node stats: how many requests mailman sent each node, and what each
//! node's vLLM reports on `/metrics`, scraped on an interval. The node list
//! follows discovery events, like the selector's.

mod prometheus;

use std::{
    collections::{HashMap, VecDeque},
    net::SocketAddr,
    sync::Mutex,
    time::{Duration, Instant},
};

use futures_util::future::join_all;
use reqwest::Client;
use serde::Serialize;
use tokio::time::MissedTickBehavior;
use tracing::{debug, info, warn};

use crate::{discovery::DiscoveryEvent, telemetry::error_chain};

#[derive(Debug, Clone, Copy)]
pub struct StatsConfig {
    pub scrape_interval: Duration,
    pub scrape_timeout: Duration,
    /// Recent rates and hit rates cover about this much scrape history.
    pub rate_window: Duration,
}

impl Default for StatsConfig {
    fn default() -> Self {
        Self {
            scrape_interval: Duration::from_secs(5),
            scrape_timeout: Duration::from_secs(2),
            rate_window: Duration::from_secs(60),
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct VllmGauges {
    requests_running: f64,
    requests_waiting: f64,
    kv_cache_usage: f64,
}

impl VllmGauges {
    /// Fails if one is missing, which means the node isn't vLLM or its metric
    /// names changed.
    fn from_samples(samples: &HashMap<String, f64>) -> Result<Self, String> {
        let required = |name| lookup(samples, name).ok_or_else(|| format!("no {name} in /metrics"));
        Ok(Self {
            requests_running: required("vllm:num_requests_running")?,
            requests_waiting: required("vllm:num_requests_waiting")?,
            kv_cache_usage: required("vllm:kv_cache_usage_perc")?,
        })
    }
}

/// Defines `VllmCounters`, one f64 per counter, with the arithmetic that
/// turns two scrapes into rates.
macro_rules! vllm_counters {
    ($($field:ident => $metric:literal),* $(,)?) => {
        #[derive(Debug, Clone, Copy, Default, PartialEq)]
        struct VllmCounters {
            $($field: f64),*
        }

        impl VllmCounters {
            /// Missing counters read as 0, since vLLM may not create a
            /// labelled counter until it first counts something.
            fn from_samples(samples: &HashMap<String, f64>) -> Self {
                Self { $($field: lookup(samples, $metric).unwrap_or(0.0)),* }
            }

            fn plus(self, other: Self) -> Self {
                Self { $($field: self.$field + other.$field),* }
            }

            fn minus(self, other: Self) -> Self {
                Self { $($field: self.$field - other.$field),* }
            }

            fn scaled(self, factor: f64) -> Self {
                Self { $($field: self.$field * factor),* }
            }

            fn any_negative(&self) -> bool {
                [$(self.$field),*].into_iter().any(|value| value < 0.0)
            }
        }
    };
}

vllm_counters! {
    requests_succeeded => "vllm:request_success",
    prompt_tokens => "vllm:prompt_tokens",
    generation_tokens => "vllm:generation_tokens",
    preemptions => "vllm:num_preemptions",
    // In tokens, not requests.
    prefix_cache_queries => "vllm:prefix_cache_queries",
    prefix_cache_hits => "vllm:prefix_cache_hits",
    ttft_seconds_sum => "vllm:time_to_first_token_seconds_sum",
    ttft_count => "vllm:time_to_first_token_seconds_count",
    e2e_latency_seconds_sum => "vllm:e2e_request_latency_seconds_sum",
    e2e_latency_count => "vllm:e2e_request_latency_seconds_count",
    inter_token_latency_seconds_sum => "vllm:inter_token_latency_seconds_sum",
    inter_token_latency_count => "vllm:inter_token_latency_seconds_count",
}

impl VllmCounters {
    fn prefix_cache_hit_rate(&self) -> Option<f64> {
        ratio(self.prefix_cache_hits, self.prefix_cache_queries)
    }

    fn mean_ttft_secs(&self) -> Option<f64> {
        ratio(self.ttft_seconds_sum, self.ttft_count)
    }

    fn mean_e2e_latency_secs(&self) -> Option<f64> {
        ratio(self.e2e_latency_seconds_sum, self.e2e_latency_count)
    }

    fn mean_inter_token_latency_secs(&self) -> Option<f64> {
        ratio(
            self.inter_token_latency_seconds_sum,
            self.inter_token_latency_count,
        )
    }
}

/// vLLM's docs name counters without the `_total` that Prometheus clients add
/// on the wire, so accept either.
fn lookup(samples: &HashMap<String, f64>, name: &str) -> Option<f64> {
    samples
        .get(name)
        .or_else(|| samples.get(&format!("{name}_total")))
        .copied()
}

fn ratio(numerator: f64, denominator: f64) -> Option<f64> {
    (denominator > 0.0).then(|| numerator / denominator)
}

#[derive(Debug, Clone, Copy)]
struct Scrape {
    at: Instant,
    gauges: VllmGauges,
    counters: VllmCounters,
}

#[derive(Default)]
struct NodeRecord {
    /// Attempts, so a request retried on another node counts on both.
    requests_sent: u64,
    /// Attempts that errored, returned a 5xx, or broke off mid-stream.
    requests_failed: u64,
    /// Oldest first. The oldest is the baseline for recent rates.
    scrapes: VecDeque<Scrape>,
    last_scrape_error: Option<String>,
}

impl NodeRecord {
    fn push_scrape(&mut self, scrape: Scrape, rate_window: Duration) {
        // A counter going down means vLLM restarted without discovery seeing
        // a new node (e.g. a fixed backend), so older scrapes can't be
        // compared with this one.
        if self
            .scrapes
            .back()
            .is_some_and(|last| scrape.counters.minus(last.counters).any_negative())
        {
            self.scrapes.clear();
        }
        self.scrapes.push_back(scrape);
        // Keep the newest scrape that's at least `rate_window` old.
        while self.scrapes.len() > 2 && scrape.at.duration_since(self.scrapes[1].at) >= rate_window
        {
            self.scrapes.pop_front();
        }
    }

    /// Counters per second between the oldest and newest scrapes, and the
    /// seconds between them.
    fn recent_rates(&self) -> Option<(VllmCounters, f64)> {
        let (oldest, newest) = (self.scrapes.front()?, self.scrapes.back()?);
        let secs = newest.at.duration_since(oldest.at).as_secs_f64();
        if secs <= 0.0 {
            return None;
        }
        let rates = newest.counters.minus(oldest.counters).scaled(1.0 / secs);
        Some((rates, secs))
    }
}

pub struct StatsCollector {
    client: Client,
    config: StatsConfig,
    nodes: Mutex<HashMap<SocketAddr, NodeRecord>>,
}

impl StatsCollector {
    pub fn new(client: Client, config: StatsConfig) -> Self {
        Self {
            client,
            config,
            nodes: Mutex::default(),
        }
    }

    /// Tracks nodes from their first `Responsive` until `Removed`. While a
    /// node is unresponsive, its failing scrapes show it.
    pub fn handle_discovery_event(&self, event: DiscoveryEvent) {
        let mut nodes = self.nodes.lock().unwrap();
        match event {
            DiscoveryEvent::Responsive(addr) => {
                nodes.entry(addr).or_default();
            }
            DiscoveryEvent::Unresponsive(_) => {}
            DiscoveryEvent::Removed(addr) => {
                nodes.remove(&addr);
            }
        }
    }

    pub fn record_request_sent(&self, addr: SocketAddr) {
        if let Some(record) = self.nodes.lock().unwrap().get_mut(&addr) {
            record.requests_sent += 1;
        }
    }

    pub fn record_request_failed(&self, addr: SocketAddr) {
        if let Some(record) = self.nodes.lock().unwrap().get_mut(&addr) {
            record.requests_failed += 1;
        }
    }

    /// Scrapes every known node each `scrape_interval`, forever.
    pub async fn run_scraper(&self) {
        let mut interval = tokio::time::interval(self.config.scrape_interval);
        interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
        info!(interval = ?self.config.scrape_interval, "scraping vLLM metrics");
        loop {
            interval.tick().await;
            self.scrape_all().await;
        }
    }

    async fn scrape_all(&self) {
        let addrs: Vec<SocketAddr> = self.nodes.lock().unwrap().keys().copied().collect();
        let results = join_all(
            addrs
                .iter()
                .map(|&addr| scrape_node(&self.client, addr, self.config.scrape_timeout)),
        )
        .await;

        // Logged after releasing the lock, so requests don't wait on log I/O.
        let mut newly_failing = Vec::new();
        let mut nodes = self.nodes.lock().unwrap();
        for (addr, result) in addrs.into_iter().zip(results) {
            let Some(record) = nodes.get_mut(&addr) else {
                continue; // removed while scraping
            };
            match result {
                Ok(scrape) => {
                    record.push_scrape(scrape, self.config.rate_window);
                    record.last_scrape_error = None;
                }
                Err(error) => {
                    if record.last_scrape_error.is_none() {
                        newly_failing.push((addr, error.clone()));
                    } else {
                        debug!(%addr, %error, "metrics scrape failed");
                    }
                    record.last_scrape_error = Some(error);
                }
            }
        }
        drop(nodes);

        for (addr, error) in newly_failing {
            warn!(%addr, %error, "metrics scrape failed");
        }
    }

    pub fn report(&self) -> StatsReport {
        let now = Instant::now();
        let nodes = self.nodes.lock().unwrap();

        let mut reports: Vec<NodeReport> = nodes
            .iter()
            .map(|(&addr, record)| {
                let latest = record.scrapes.back();
                NodeReport {
                    addr,
                    requests_sent: record.requests_sent,
                    requests_failed: record.requests_failed,
                    scraped_secs_ago: latest
                        .map(|scrape| now.duration_since(scrape.at).as_secs_f64()),
                    last_scrape_error: record.last_scrape_error.clone(),
                    vllm: latest.map(|scrape| {
                        VllmReport::new(scrape.gauges, scrape.counters, record.recent_rates())
                    }),
                }
            })
            .collect();
        reports.sort_by_key(|report| report.addr);

        let cluster = ClusterReport {
            nodes: nodes.len(),
            requests_sent: nodes.values().map(|record| record.requests_sent).sum(),
            requests_failed: nodes.values().map(|record| record.requests_failed).sum(),
            vllm: cluster_vllm(nodes.values()),
        };
        StatsReport {
            cluster,
            nodes: reports,
        }
    }
}

/// Sums every scraped node, except KV cache usage, which is the mean.
fn cluster_vllm<'a>(records: impl Iterator<Item = &'a NodeRecord>) -> Option<VllmReport> {
    let mut scraped = 0;
    let mut gauges = VllmGauges::default();
    let mut counters = VllmCounters::default();
    let mut rates: Option<(VllmCounters, f64)> = None;
    for record in records {
        let Some(latest) = record.scrapes.back() else {
            continue;
        };
        scraped += 1;
        gauges.requests_running += latest.gauges.requests_running;
        gauges.requests_waiting += latest.gauges.requests_waiting;
        gauges.kv_cache_usage += latest.gauges.kv_cache_usage;
        counters = counters.plus(latest.counters);
        if let Some((node_rates, secs)) = record.recent_rates() {
            rates = Some(match rates {
                Some((sum, max_secs)) => (sum.plus(node_rates), max_secs.max(secs)),
                None => (node_rates, secs),
            });
        }
    }
    if scraped == 0 {
        return None;
    }
    gauges.kv_cache_usage /= scraped as f64;
    Some(VllmReport::new(gauges, counters, rates))
}

async fn scrape_node(
    client: &Client,
    addr: SocketAddr,
    timeout: Duration,
) -> Result<Scrape, String> {
    let response = client
        .get(format!("http://{addr}/metrics"))
        .timeout(timeout)
        .send()
        .await
        .map_err(|err| error_chain(&err))?;
    if !response.status().is_success() {
        return Err(format!("status {}", response.status()));
    }
    let text = response.text().await.map_err(|err| error_chain(&err))?;
    let samples = prometheus::sum_by_name(&text);
    Ok(Scrape {
        at: Instant::now(),
        gauges: VllmGauges::from_samples(&samples)?,
        counters: VllmCounters::from_samples(&samples),
    })
}

#[derive(Debug, Serialize)]
pub struct StatsReport {
    pub cluster: ClusterReport,
    pub nodes: Vec<NodeReport>,
}

#[derive(Debug, Serialize)]
pub struct ClusterReport {
    pub nodes: usize,
    pub requests_sent: u64,
    pub requests_failed: u64,
    /// None until a node has been scraped.
    pub vllm: Option<VllmReport>,
}

#[derive(Debug, Serialize)]
pub struct NodeReport {
    pub addr: SocketAddr,
    pub requests_sent: u64,
    pub requests_failed: u64,
    pub scraped_secs_ago: Option<f64>,
    pub last_scrape_error: Option<String>,
    /// From the latest successful scrape, so possibly stale if scrapes fail.
    pub vllm: Option<VllmReport>,
}

#[derive(Debug, Serialize)]
pub struct VllmReport {
    pub requests_running: f64,
    pub requests_waiting: f64,
    /// Fraction of KV cache in use, 0 to 1.
    pub kv_cache_usage: f64,
    /// Since vLLM started.
    pub lifetime: LifetimeReport,
    /// None until there are two scrapes to compare.
    pub recent: Option<RecentReport>,
}

impl VllmReport {
    fn new(gauges: VllmGauges, counters: VllmCounters, rates: Option<(VllmCounters, f64)>) -> Self {
        Self {
            requests_running: gauges.requests_running,
            requests_waiting: gauges.requests_waiting,
            kv_cache_usage: gauges.kv_cache_usage,
            lifetime: LifetimeReport {
                requests_succeeded: counters.requests_succeeded,
                prompt_tokens: counters.prompt_tokens,
                generation_tokens: counters.generation_tokens,
                preemptions: counters.preemptions,
                prefix_cache_queried_tokens: counters.prefix_cache_queries,
                prefix_cache_hit_tokens: counters.prefix_cache_hits,
                prefix_cache_hit_rate: counters.prefix_cache_hit_rate(),
                mean_ttft_secs: counters.mean_ttft_secs(),
                mean_e2e_latency_secs: counters.mean_e2e_latency_secs(),
                mean_inter_token_latency_secs: counters.mean_inter_token_latency_secs(),
            },
            recent: rates.map(|(rates, window_secs)| RecentReport {
                window_secs,
                requests_per_sec: rates.requests_succeeded,
                prompt_tokens_per_sec: rates.prompt_tokens,
                generation_tokens_per_sec: rates.generation_tokens,
                preemptions_per_sec: rates.preemptions,
                // Ratios of rates are ratios of the counts in the window.
                prefix_cache_hit_rate: rates.prefix_cache_hit_rate(),
                mean_ttft_secs: rates.mean_ttft_secs(),
                mean_e2e_latency_secs: rates.mean_e2e_latency_secs(),
                mean_inter_token_latency_secs: rates.mean_inter_token_latency_secs(),
            }),
        }
    }
}

/// Means and hit rates are None when there's nothing to divide by.
#[derive(Debug, Serialize)]
pub struct LifetimeReport {
    pub requests_succeeded: f64,
    pub prompt_tokens: f64,
    pub generation_tokens: f64,
    pub preemptions: f64,
    pub prefix_cache_queried_tokens: f64,
    pub prefix_cache_hit_tokens: f64,
    /// Fraction of prompt tokens served from the prefix cache.
    pub prefix_cache_hit_rate: Option<f64>,
    pub mean_ttft_secs: Option<f64>,
    pub mean_e2e_latency_secs: Option<f64>,
    pub mean_inter_token_latency_secs: Option<f64>,
}

#[derive(Debug, Serialize)]
pub struct RecentReport {
    /// For the cluster, the longest of the nodes' windows.
    pub window_secs: f64,
    pub requests_per_sec: f64,
    pub prompt_tokens_per_sec: f64,
    pub generation_tokens_per_sec: f64,
    pub preemptions_per_sec: f64,
    pub prefix_cache_hit_rate: Option<f64>,
    pub mean_ttft_secs: Option<f64>,
    pub mean_e2e_latency_secs: Option<f64>,
    pub mean_inter_token_latency_secs: Option<f64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scrape(at: Instant, hits: f64, queries: f64) -> Scrape {
        Scrape {
            at,
            gauges: VllmGauges::default(),
            counters: VllmCounters {
                prefix_cache_hits: hits,
                prefix_cache_queries: queries,
                ..Default::default()
            },
        }
    }

    #[test]
    fn reads_counters_with_or_without_total_suffix() {
        let samples = HashMap::from([
            ("vllm:prompt_tokens_total".to_string(), 7.0),
            ("vllm:generation_tokens".to_string(), 3.0),
        ]);
        let counters = VllmCounters::from_samples(&samples);
        assert_eq!(counters.prompt_tokens, 7.0);
        assert_eq!(counters.generation_tokens, 3.0);
    }

    #[test]
    fn missing_gauge_is_an_error() {
        assert!(VllmGauges::from_samples(&HashMap::new()).is_err());
    }

    #[test]
    fn recent_hit_rate_covers_only_the_window() {
        let start = Instant::now();
        let mut record = NodeRecord::default();
        let window = Duration::from_secs(10);
        record.push_scrape(scrape(start, 0.0, 100.0), window);
        record.push_scrape(scrape(start + Duration::from_secs(10), 0.0, 200.0), window);
        record.push_scrape(scrape(start + Duration::from_secs(20), 90.0, 300.0), window);

        // The first scrape aged out, so the window is the last 10s, where 90
        // of 100 queried tokens hit.
        assert_eq!(record.scrapes.len(), 2);
        let (rates, secs) = record.recent_rates().unwrap();
        assert_eq!(secs, 10.0);
        assert_eq!(rates.prefix_cache_hit_rate(), Some(0.9));
        assert_eq!(rates.prefix_cache_queries, 10.0);
    }

    #[test]
    fn counter_reset_restarts_the_window() {
        let start = Instant::now();
        let mut record = NodeRecord::default();
        let window = Duration::from_secs(60);
        record.push_scrape(scrape(start, 50.0, 100.0), window);
        record.push_scrape(scrape(start + Duration::from_secs(5), 0.0, 10.0), window);
        assert_eq!(record.scrapes.len(), 1);
        assert!(record.recent_rates().is_none());
    }

    #[test]
    fn tracks_nodes_from_discovery() {
        let stats = StatsCollector::new(Client::new(), StatsConfig::default());
        let addr = SocketAddr::from(([127, 0, 0, 1], 1));

        // Unknown nodes aren't counted.
        stats.record_request_sent(addr);
        stats.handle_discovery_event(DiscoveryEvent::Responsive(addr));
        stats.record_request_sent(addr);
        stats.record_request_sent(addr);
        stats.record_request_failed(addr);

        let report = stats.report();
        assert_eq!(report.cluster.requests_sent, 2);
        assert_eq!(report.nodes[0].requests_failed, 1);
        assert!(report.cluster.vllm.is_none());

        stats.handle_discovery_event(DiscoveryEvent::Removed(addr));
        assert!(stats.report().nodes.is_empty());
    }
}
