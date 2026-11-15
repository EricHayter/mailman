#!/usr/bin/env bash
# Creates or updates a local kind cluster running CPU vLLM behind mailman.
#
# Creates the cluster (deploy/kind-config.yaml) if it doesn't exist and deploys
# two CPU vLLM pods running a tiny model (deploy/vllm-cpu.yaml), which take
# about 4 CPUs and 8 GiB of memory. It then builds mailman's image, loads it
# into the cluster and deploys it (deploy/mailman.yaml). mailman finds the vLLM
# pods through the vllm Service's EndpointSlices, and is reachable on
# localhost:3000 for as long as the cluster exists.
#
# The first run downloads the vLLM image and model, so it takes a few minutes.
# Rerun after changing mailman's code to rebuild and redeploy; vLLM is left
# running. Remove everything with `kind delete cluster --name mailman`.
#
# Useful while it runs:
#   kubectl --context kind-mailman logs -f deploy/mailman
#   kubectl --context kind-mailman get endpointslices -l kubernetes.io/service-name=vllm
#
# Usage: scripts/kind-up.sh
#   KIND_CLUSTER  cluster name (default: mailman)
set -euo pipefail
cd "$(dirname "$0")/.."

declare -A install=(
    [docker]="https://docs.docker.com/engine/install/"
    [kind]="https://kind.sigs.k8s.io/docs/user/quick-start/#installation"
    [kubectl]="https://kubernetes.io/docs/tasks/tools/"
    [envsubst]="https://www.gnu.org/software/gettext/ (package gettext-base on Debian/Ubuntu, gettext elsewhere)"
)
missing=false
for cmd in docker kind kubectl envsubst; do
    if ! command -v "$cmd" >/dev/null; then
        echo "error: $cmd not found. Install it: ${install[$cmd]}" >&2
        missing=true
    fi
done
"$missing" && exit 1

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

Ready. mailman is on localhost:3000:
  scripts/chat.sh "hello"

Shut everything down with:
  kind delete cluster --name $cluster
MSG
