# mailman

<p align="center"><img src="docs/mailman.png" alt="mailman" width="220"></p>

A router for vLLM

## Quick start (local, CPU)

Install [Docker](https://docs.docker.com/engine/install/),
[kind](https://kind.sigs.k8s.io/docs/user/quick-start/),
[kubectl](https://kubernetes.io/docs/tasks/tools/), `envsubst` and `jq`, then:

```
scripts/kind-up.sh    # cluster + vLLM + mailman, on localhost:3000
cp .env.example .env
scripts/chat.sh "hello"
```

Tear down with `kind delete cluster --name mailman`.

## Configuration

mailman is configured through environment variables, set in
`deploy/mailman.yaml`:

| Variable | |
|---|---|
| `MAILMAN_K8S_SERVICE` | Service whose pods to route to |
| `MAILMAN_BACKENDS` | without `MAILMAN_K8S_SERVICE`: comma-separated `host:port` list to route to, resolved at startup (default `127.0.0.1:8000`) |
| `MAILMAN_K8S_POD_LABELS` | label selector for those pods, e.g. `app=vllm` (required with the above) |
| `MAILMAN_K8S_NAMESPACE` | defaults to mailman's own namespace |
| `MAILMAN_K8S_PORT_NAME` | only needed if the Service has several ports |
| `MAILMAN_SELECTOR` | `cache_aware` (default) or `round_robin` |
| `RUST_LOG` | log filter, e.g. `info` or `mailman=debug` |
| `LOG_FORMAT` | `json` for JSON logs |

## Stats

`GET /stats` returns JSON for each node and for the whole cluster:

- Requests mailman sent to each node, and how many of them failed.
- What vLLM reports on `/metrics`, scraped every 5s:
  - running and waiting requests, and KV cache usage
  - prefix cache hit rate
  - token and request throughput
  - preemptions
  - mean TTFT, end-to-end and inter-token latency

Each vLLM figure comes both since vLLM started (`lifetime`) and over the last minute (`recent`).

```
curl -s localhost:3000/stats | jq .cluster
```

## Benchmarking on GPUs

This compares mailman's routers on a multi-GPU Linux machine, for example a
Lambda Cloud 8x H100 instance. The machine needs NVIDIA drivers, the NVIDIA
Container Toolkit and Docker. On that machine, run:

```
scripts/k3s-up.sh                       # k3s, one Qwen3-8B vLLM per GPU, mailman
KUBECONFIG=/etc/rancher/k3s/k3s.yaml bench/run-k8s.sh
```

`bench/run-k8s.sh` runs [AIPerf](https://github.com/ai-dynamo/aiperf) once
per router, alternating, three times each:

- Every request starts with one of a set of shared prefixes. The prefixes
  together fill half the cluster's total KV cache (`bench/workload.sh`), which
  is more than one node can hold.
- Before each run it empties every vLLM prefix cache and restarts mailman, so
  every run starts cold.
- Each run's results go to `bench/results/<router>/run-<n>/`: AIPerf's
  per-request records, and mailman's `/stats` every 5 seconds.

Copy `bench/results` back, then plot it:

```
uv run bench/plot.py
```

## Troubleshooting

**`permission denied while trying to connect to the Docker daemon socket`**

Your user isn't in the `docker` group. Add it, and apply it to the current
shell (other terminals need a new login):

```
sudo usermod -aG docker $USER && newgrp docker
```

**Pods crash, and their logs show `too many open files`**

The inotify limits are too low for a multi-node kind cluster
([details](https://kind.sigs.k8s.io/docs/user/known-issues/#pod-errors-due-to-too-many-open-files)):

```
sudo sysctl fs.inotify.max_user_watches=524288 fs.inotify.max_user_instances=512
```

**`kind-up.sh` hangs at `Waiting for deployment "vllm" rollout to finish`**

Check why the pods aren't starting:

```
kubectl --context kind-mailman describe pods -l app=vllm | grep -A3 Events
```

If it shows `0/3 nodes are available: ... Insufficient cpu` (or `memory`), the
machine can't fit two vLLM pods, which need about 4 CPUs and 8 GiB together.
Set `replicas: 1` in `deploy/vllm-cpu.yaml` and rerun `scripts/kind-up.sh`.
