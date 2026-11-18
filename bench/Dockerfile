# AIPerf and the benchmark scripts' runtime, for bench/pod.yaml. Built once
# and loaded into the cluster (scripts/k3s-up.sh), so runs don't install it.
FROM python:3.13-slim
RUN pip install --no-cache-dir --root-user-action=ignore aiperf==0.13.0
