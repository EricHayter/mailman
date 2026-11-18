#!/usr/bin/env bash
# Sets up a single-machine k3s cluster on a multi-GPU Linux machine (e.g. a
# Lambda Cloud 8x H100 instance) and deploys one vLLM pod per GPU behind
# mailman, for benchmarking with bench/run-k8s.sh. Run it on that machine.
#
# Needs NVIDIA drivers, the NVIDIA Container Toolkit (k3s picks up its runtime
# when k3s starts) and Docker, to build mailman's image. Installs k3s if it
# isn't there, then:
#   - the NVIDIA device plugin (deploy/nvidia-device-plugin.yaml)
#   - one Qwen3-8B vLLM pod per GPU (deploy/vllm-gpu.yaml)
#   - mailman, built here and loaded straight into k3s, so no registry is
#     needed (deploy/mailman.yaml)
#   - the benchmark's image (bench/Dockerfile), loaded the same way
# Rerun after changing mailman's code to rebuild and redeploy.
#
# mailman's Service is a NodePort (30000) with no authentication, so keep that
# port closed in the machine's firewall.
#
# Usage: scripts/k3s-up.sh
set -euo pipefail
cd "$(dirname "$0")/.."

K3S_VERSION=v1.33.1+k3s1

for cmd in nvidia-smi nvidia-container-runtime docker curl; do
    command -v "$cmd" >/dev/null || { echo "error: $cmd not found" >&2; exit 1; }
done

if ! command -v k3s >/dev/null; then
    curl -sfL https://get.k3s.io |
        INSTALL_K3S_VERSION=$K3S_VERSION sh -s - --write-kubeconfig-mode 644
fi
export KUBECONFIG=/etc/rancher/k3s/k3s.yaml
# A fresh k3s takes a moment to register its node, and until then there's
# nothing for `kubectl wait` to wait on.
until kubectl get nodes 2>/dev/null | grep -q .; do sleep 2; done
kubectl wait --for=condition=Ready node --all --timeout=5m

gpus=$(nvidia-smi -L | wc -l)
kubectl apply -f deploy/nvidia-device-plugin.yaml
kubectl rollout status daemonset/nvidia-device-plugin-daemonset -n kube-system --timeout=5m
# The plugin takes a moment to report the GPUs after it starts.
for _ in $(seq 30); do
    advertised=$(kubectl get nodes -o jsonpath='{.items[0].status.allocatable.nvidia\.com/gpu}')
    [[ $advertised == "$gpus" ]] && break
    sleep 2
done
echo "node advertises ${advertised:-0} nvidia.com/gpu, machine has $gpus"
[[ $advertised == "$gpus" ]] || { echo "error: GPUs not visible to k3s" >&2; exit 1; }

docker build -t mailman:dev .
docker build -t mailman-bench:dev bench
docker save mailman:dev mailman-bench:dev | sudo k3s ctr images import -

kubectl apply -f deploy/vllm-gpu.yaml -f deploy/vllm-service.yaml
kubectl scale deploy/vllm --replicas="$gpus"
kubectl apply -f deploy/mailman.yaml
# The tag never changes, so restart to pick up the image just imported.
kubectl rollout restart deploy/mailman

kubectl rollout status deploy/vllm --timeout=30m
kubectl rollout status deploy/mailman

cat <<MSG

Ready: $gpus vLLM pods behind mailman. Benchmark with:
  KUBECONFIG=$KUBECONFIG bench/run-k8s.sh
MSG
