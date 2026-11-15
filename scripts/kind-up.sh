#!/usr/bin/env bash
# Creates or updates a local kind cluster running CPU vLLM behind mailman.
# Safe to rerun: it rebuilds and reloads mailman, and leaves vLLM running.
#
# Usage: scripts/kind-up.sh
#   KIND_CLUSTER  cluster name (default: mailman)
set -euo pipefail
cd "$(dirname "$0")/.."

cluster=${KIND_CLUSTER:-mailman}
# Pin every command to the kind cluster, whatever the current context is.
kubectl() { command kubectl --context "kind-$cluster" "$@"; }

if ! kind get clusters | grep -qx "$cluster"; then
    # kind doesn't expand ${HOME} in its config.
    kind create cluster --name "$cluster" --config <(envsubst <deploy/kind-config.yaml)
fi

kubectl apply -f deploy/vllm-cpu.yaml -f deploy/vllm-service.yaml

docker build -t mailman:dev .
kind load docker-image mailman:dev --name "$cluster"
kubectl apply -f deploy/mailman.yaml
# The tag never changes, so restart to pick up the image just loaded.
kubectl rollout restart deploy/mailman

kubectl rollout status deploy/vllm --timeout=20m
kubectl rollout status deploy/mailman

cat <<MSG

Ready. Forward mailman's port, then send a request:
  kubectl --context kind-$cluster port-forward svc/mailman 3000:3000
  scripts/chat.sh "hello"
MSG
