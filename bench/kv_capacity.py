"""Prints how many tokens one vLLM node's KV cache holds, from its /metrics.

Runs where the node's port 8000 is reachable as localhost, e.g. piped into
`kubectl exec -i <pod> -- python3 -`.
"""

import re
import urllib.request

text = urllib.request.urlopen("http://localhost:8000/metrics", timeout=10).read().decode()
for line in text.splitlines():
    if line.startswith("vllm:cache_config_info{"):
        labels = dict(re.findall(r'(\w+)="([^"]*)"', line))
        print(int(labels["num_gpu_blocks"]) * int(labels["block_size"]))
        break
else:
    raise SystemExit("no vllm:cache_config_info in /metrics")
