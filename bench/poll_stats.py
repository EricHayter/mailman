"""Reads mailman's /stats during a benchmark.

  poll_stats.py wait URL NODES      waits until mailman has scraped NODES nodes
  poll_stats.py poll URL OUT.jsonl  appends {"time": ..., "stats": ...} every
                                    5s until killed
"""

import json
import sys
import time
import urllib.request

INTERVAL_SECS = 5


def fetch_stats(url):
    with urllib.request.urlopen(f"{url}/stats", timeout=5) as response:
        return json.load(response)


def wait_for_nodes(url, nodes):
    while True:
        try:
            stats = fetch_stats(url)
            scraped = sum(node["vllm"] is not None for node in stats["nodes"])
            if scraped >= nodes:
                return
            print(f"mailman has scraped {scraped}/{nodes} nodes", flush=True)
        except OSError as err:
            print(f"mailman not reachable yet: {err}", flush=True)
        time.sleep(INTERVAL_SECS)


def poll(url, out_path):
    with open(out_path, "a") as out:
        while True:
            try:
                stats = fetch_stats(url)
                out.write(json.dumps({"time": time.time(), "stats": stats}) + "\n")
                out.flush()
            except OSError as err:
                print(f"couldn't read /stats: {err}", file=sys.stderr, flush=True)
            time.sleep(INTERVAL_SECS)


if __name__ == "__main__":
    mode, url, arg = sys.argv[1:]
    if mode == "wait":
        wait_for_nodes(url, int(arg))
    else:
        poll(url, arg)
