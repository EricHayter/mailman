#!/usr/bin/env bash
# One AIPerf run against the router at ROUTER_URL, recording mailman's /stats
# alongside. Runs
# in the bench pod (bench/pod.yaml), configured by environment variables.
# Leaves results in /results and touches /results/DONE, which bench/run-k8s.sh
# waits for.
set -euo pipefail

mkdir -p /results
echo "$ROUTER" >/results/router

python3 /bench/poll_stats.py wait "$MAILMAN_URL" "$VLLM_NODES"
python3 /bench/poll_stats.py poll "$MAILMAN_URL" /results/stats.jsonl &
poller=$!

# Every request starts with one of PREFIXES shared prefixes, picked at
# random, so a router that keeps each prefix on one node gets cache hits.
# ignore_eos makes every response exactly OUTPUT_TOKENS long.
# --server-metrics also scrapes each vLLM pod directly.
# shellcheck disable=SC2086  # VLLM_METRICS_URLS is a list
aiperf profile \
    --model "$MODEL" \
    --tokenizer "$MODEL" \
    --url "$ROUTER_URL" \
    --endpoint-type chat \
    --endpoint "$ENDPOINT" \
    --streaming \
    --num-prefix-prompts "$PREFIXES" \
    --prefix-prompt-length "$PREFIX_TOKENS" \
    --isl "$USER_TOKENS" \
    --osl "$OUTPUT_TOKENS" \
    --extra-inputs ignore_eos:true \
    --concurrency "$CONCURRENCY" \
    --request-count "$REQUESTS" \
    --random-seed 42 \
    --slice-duration 10 \
    --server-metrics $VLLM_METRICS_URLS \
    --ui-type simple \
    --artifact-dir /results/aiperf

# One final reading after the last request.
sleep 6
kill "$poller"
touch /results/DONE
