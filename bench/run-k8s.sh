#!/usr/bin/env bash
# Benchmarks each of mailman's routers with AIPerf on the Kubernetes cluster
# from scripts/k3s-up.sh (the current kubectl context), saving results to bench/results/<router>/run-<n>/. Plot
# them with bench/plot.py.
#
# The routers take turns (round robin, cache-aware, round robin, ...) for
# REPEATS rounds, so the spread between runs of the same router shows how much
# of the difference between routers is noise.
#
# Before each run it switches mailman's router (which restarts mailman,
# clearing its routing state and stats) and empties every vLLM node's prefix
# cache, so each router starts cold.
#
# The workload is sized from the nodes' KV cache by bench/workload.sh; set
# any of its variables to override.
#
# Usage: bench/run-k8s.sh
#   ROUTERS        default: "round_robin cache_aware"
#   REPEATS        runs of each router (default: 3)
#   PREFIXES, PREFIX_TOKENS, USER_TOKENS, OUTPUT_TOKENS, CONCURRENCY, REQUESTS
#                  see bench/workload.sh
#   MODEL          must match deploy/vllm-gpu.yaml (default: Qwen/Qwen3-8B)
set -euo pipefail
cd "$(dirname "$0")/.."

export MODEL=${MODEL:-Qwen/Qwen3-8B}
routers=${ROUTERS:-round_robin cache_aware}
repeats=${REPEATS:-3}

kubectl create configmap bench-scripts \
    --from-file=bench/run-aiperf.sh --from-file=bench/poll_stats.py \
    --dry-run=client -o yaml | kubectl apply -f -

vllm_pods=$(kubectl get pods -l app=vllm --field-selector=status.phase=Running -o name)
export VLLM_NODES=$(wc -w <<<"$vllm_pods")
export VLLM_METRICS_URLS=$(kubectl get pods -l app=vllm --field-selector=status.phase=Running \
    -o jsonpath='{range .items[*]}http://{.status.podIP}:8000/metrics {end}')

source bench/workload.sh
size_workload "$(kubectl exec -i "$(head -n1 <<<"$vllm_pods")" -- python3 - <bench/kv_capacity.py)" "$VLLM_NODES"

for router in $routers; do
    rm -rf "bench/results/$router"
done

for run in $(seq "$repeats"); do
for router in $routers; do
    echo "=== $router, run $run of $repeats"
    kubectl set env deploy/mailman MAILMAN_SELECTOR="$router"
    # Restart even if the router didn't change, to clear mailman's state.
    kubectl rollout restart deploy/mailman
    kubectl rollout status deploy/mailman --timeout=5m
    # rollout status returns once the new pod is ready, but the old one, still
    # on the previous router, can take traffic through the Service until it
    # has gone.
    until [[ $(kubectl get pods -l app=mailman --no-headers | wc -l) == 1 ]]; do
        sleep 2
    done

    for pod in $vllm_pods; do
        kubectl exec "$pod" -- python3 -c "
import urllib.request
urllib.request.urlopen(urllib.request.Request('http://localhost:8000/reset_prefix_cache', method='POST'))
"
    done
    echo "reset prefix caches on $VLLM_NODES nodes"

    kubectl delete pod bench --ignore-not-found --wait
    ROUTER=$router envsubst <bench/pod.yaml | kubectl apply -f -
    kubectl wait --for=condition=Ready pod/bench --timeout=10m
    kubectl logs -f bench &
    logs=$!
    until kubectl exec bench -- test -f /results/DONE 2>/dev/null; do
        if [[ $(kubectl get pod bench -o jsonpath='{.status.phase}') == Failed ]]; then
            echo "error: benchmark failed" >&2
            exit 1
        fi
        sleep 15
    done
    kill "$logs" 2>/dev/null || true

    mkdir -p "bench/results/$router"
    kubectl cp bench:/results "bench/results/$router/run-$run"
    kubectl delete pod bench --wait
    echo "saved bench/results/$router/run-$run"
done
done
