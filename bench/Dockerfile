# AIPerf and the benchmark scripts' runtime, for bench/pod.yaml, plus vLLM's
# router to compare mailman against (bench/vllm-router.yaml). Built once and
# loaded into the cluster (scripts/k3s-up.sh), so runs don't install them.
FROM python:3.13-slim
RUN pip install --no-cache-dir --root-user-action=ignore aiperf==0.13.0 vllm-router==0.1.17
