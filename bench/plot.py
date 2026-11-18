# /// script
# requires-python = ">=3.11"
# dependencies = ["pandas", "seaborn", "matplotlib"]
# ///
"""Plots bench/run-k8s.sh's results, comparing routers.

Reads bench/results/<router>/run-<n>/ (AIPerf's per-request records and
mailman's /stats snapshots) and writes PNGs and summary.csv to
bench/results/plots/. Bars show the mean over runs, with whiskers spanning the
fastest to slowest run.

Usage: uv run bench/plot.py [results dir]
"""

import json
import sys
from pathlib import Path

import matplotlib.pyplot as plt
import pandas as pd
import seaborn as sns

# Fixed per router, so a router keeps its color whichever runs are present.
ROUTER_COLORS = {"round_robin": "#2a78d6", "cache_aware": "#eb6834"}
ROUTER_LABELS = {"round_robin": "Round robin", "cache_aware": "Cache-aware"}
TEXT = "#0b0b0b"
MUTED = "#52514e"
GRID = "#e4e3df"


def load_requests(run_dir: Path) -> pd.DataFrame:
    rows = []
    with open(run_dir / "aiperf" / "profile_export.jsonl") as f:
        for line in f:
            record = json.loads(line)
            metrics = record["metrics"]
            if "time_to_first_token" not in metrics:
                continue  # failed request
            rows.append(
                {
                    "end_ns": record["metadata"]["request_end_ns"],
                    "ttft_ms": metrics["time_to_first_token"]["value"],
                    "latency_ms": metrics["request_latency"]["value"],
                }
            )
    df = pd.DataFrame(rows)
    df["elapsed_s"] = (df["end_ns"] - df["end_ns"].min()) / 1e9
    return df


def load_stats(run_dir: Path) -> list[dict]:
    with open(run_dir / "stats.jsonl") as f:
        return [json.loads(line) for line in f]


def node_names(snapshots: list[dict]) -> dict[str, str]:
    addrs = sorted({node["addr"] for snap in snapshots for node in snap["stats"]["nodes"]})
    return {addr: f"Node {i + 1}" for i, addr in enumerate(addrs)}


def per_node_totals(snapshots: list[dict], names: dict[str, str]) -> pd.DataFrame:
    """Requests mailman sent each node, and each node's prefix cache hit rate,
    from the change between the first and last snapshots. vLLM's counters
    span every run, so only the change belongs to this one."""
    first = {n["addr"]: n for n in snapshots[0]["stats"]["nodes"]}
    rows = []
    for node in snapshots[-1]["stats"]["nodes"]:
        start = first.get(node["addr"])
        if not (start and start["vllm"] and node["vllm"]):
            continue
        queried = _counter_delta(node, start, "prefix_cache_queried_tokens")
        hit = _counter_delta(node, start, "prefix_cache_hit_tokens")
        rows.append(
            {
                "node": names[node["addr"]],
                "requests_sent": node["requests_sent"],
                "prefix_hit_tokens": hit,
                "prefix_query_tokens": queried,
            }
        )
    if not rows:
        print("warning: no node has vLLM stats in both the first and last snapshot;"
              " this run has no cache numbers", file=sys.stderr)
    columns = ["node", "requests_sent", "prefix_hit_tokens", "prefix_query_tokens"]
    return pd.DataFrame(rows, columns=columns)


def _counter_delta(end: dict, start: dict, name: str) -> float:
    return end["vllm"]["lifetime"][name] - start["vllm"]["lifetime"][name]


def per_node_series(snapshots: list[dict], names: dict[str, str]) -> pd.DataFrame:
    t0 = snapshots[0]["time"]
    rows = []
    for snap in snapshots:
        for node in snap["stats"]["nodes"]:
            if not node["vllm"]:
                continue
            rows.append(
                {
                    "elapsed_s": snap["time"] - t0,
                    "node": names[node["addr"]],
                    "requests_waiting": node["vllm"]["requests_waiting"],
                    "kv_cache_usage": node["vllm"]["kv_cache_usage"],
                }
            )
    return pd.DataFrame(rows)


def style():
    sns.set_theme(
        style="whitegrid",
        rc={
            "axes.edgecolor": GRID,
            "grid.color": GRID,
            "text.color": TEXT,
            "axes.labelcolor": MUTED,
            "xtick.color": MUTED,
            "ytick.color": MUTED,
            "axes.titleweight": "bold",
            "lines.linewidth": 2,
            "figure.dpi": 150,
        },
    )


def FULL_RANGE(values):
    """Error bars from the lowest to the highest run."""
    return values.min(), values.max()


def write_summary(summary: pd.DataFrame, metrics: list[str], out: Path):
    """Each router's mean, and its spread between runs as a % of the mean;
    then cache-aware's change from round robin. A change much bigger than
    the spread is real; one about the same size is noise."""
    grouped = summary.groupby("router")[metrics]
    mean = grouped.mean()
    spread = (grouped.max() - grouped.min()) / mean * 100
    table = pd.concat({"mean": mean, "spread %": spread}).round(3)
    rr, ca = ROUTER_LABELS["round_robin"], ROUTER_LABELS["cache_aware"]
    if rr in mean.index and ca in mean.index:
        change = ((mean.loc[ca] - mean.loc[rr]) / mean.loc[rr] * 100).round(1)
        table.loc[("change %", f"{ca} vs {rr}"), :] = change
    table.to_csv(out / "summary.csv")
    print(table.to_string())


def save(fig, out: Path, name: str):
    fig.tight_layout()
    fig.savefig(out / name, bbox_inches="tight")
    plt.close(fig)
    print(f"wrote {out / name}")


def main():
    results = Path(sys.argv[1] if len(sys.argv) > 1 else Path(__file__).parent / "results")
    runs = {r: sorted((results / r).glob("run-*/aiperf")) for r in ROUTER_COLORS}
    routers = [r for r in ROUTER_COLORS if runs[r]]
    if not routers:
        sys.exit(f"no results in {results}; run bench/run-k8s.sh first")
    out = results / "plots"
    out.mkdir(exist_ok=True)
    style()
    palette = {ROUTER_LABELS[r]: ROUTER_COLORS[r] for r in routers}

    requests, nodes, series, summary = [], [], [], []
    for router, run_dir in ((r, aiperf_dir.parent) for r in routers for aiperf_dir in runs[r]):
        label = ROUTER_LABELS[router]
        reqs = load_requests(run_dir)
        snapshots = load_stats(run_dir)
        names = node_names(snapshots)
        totals = per_node_totals(snapshots, names)
        aiperf = json.loads((run_dir / "aiperf" / "profile_export_aiperf.json").read_text())

        requests.append(reqs.assign(router=label))
        nodes.append(totals.assign(router=label))
        series.append(per_node_series(snapshots, names).assign(router=label))
        queried = totals["prefix_query_tokens"].sum()
        summary.append(
            {
                "router": label,
                "run": run_dir.name,
                "Throughput (req/s)": aiperf["request_throughput"]["avg"],
                "TTFT p50 (ms)": aiperf["time_to_first_token"]["p50"],
                "TTFT p99 (ms)": aiperf["time_to_first_token"]["p99"],
                "Latency p50 (ms)": aiperf["request_latency"]["p50"],
                "Prefix cache hit rate": totals["prefix_hit_tokens"].sum() / queried
                if queried
                else float("nan"),
            }
        )
    requests = pd.concat(requests)
    nodes = pd.concat(nodes)
    series = pd.concat(series)
    summary = pd.DataFrame(summary)
    metrics = [c for c in summary.columns if c not in ("router", "run")]
    write_summary(summary, metrics, out)

    # Headline numbers, one panel each since their units differ.
    order = [ROUTER_LABELS[r] for r in routers]
    fig, axes = plt.subplots(1, len(metrics), figsize=(3.2 * len(metrics), 3.4))
    for ax, column in zip(axes, metrics):
        sns.barplot(summary, x="router", y=column, hue="router", order=order, palette=palette,
                    saturation=1, width=0.6, errorbar=FULL_RANGE, capsize=0.15,
                    err_kws={"color": MUTED, "linewidth": 1.5}, ax=ax)
        sns.stripplot(summary, x="router", y=column, order=order, color=TEXT, size=4,
                      jitter=0.08, ax=ax)
        for i, value in enumerate(summary.groupby("router")[column].mean().reindex(order)):
            text = f"{value:.0%}" if "rate" in column else f"{value:,.1f}"
            ax.annotate(text, (i, 0), ha="center", va="bottom", xytext=(0, 4),
                        textcoords="offset points", color="white", fontsize=9,
                        fontweight="bold")
        ax.set_title(column, fontsize=10)
        ax.set_xlabel("")
        ax.set_ylabel("")
        ax.tick_params(axis="x", labelsize=8)
    fig.suptitle("Router comparison", fontweight="bold")
    save(fig, out, "summary.png")

    # TTFT distribution: where the cache hits show up.
    fig, ax = plt.subplots(figsize=(7, 4))
    sns.ecdfplot(requests, x="ttft_ms", hue="router", palette=palette, ax=ax)
    ax.set_xscale("log")
    ax.set_xlabel("Time to first token (ms, log scale)")
    ax.set_ylabel("Fraction of requests")
    ax.set_title("Time to first token")
    ax.get_legend().set_title(None)
    save(fig, out, "ttft_ecdf.png")

    # Load and cache hits per node.
    fig, axes = plt.subplots(1, 2, figsize=(10, 3.8))
    sns.barplot(nodes, x="node", y="requests_sent", hue="router", palette=palette, saturation=1,
                errorbar=FULL_RANGE, ax=axes[0])
    axes[0].set_title("Requests sent to each node")
    axes[0].set_ylabel("Requests")
    nodes["hit_rate"] = nodes["prefix_hit_tokens"] / nodes["prefix_query_tokens"]
    sns.barplot(nodes, x="node", y="hit_rate", hue="router", palette=palette, saturation=1,
                errorbar=FULL_RANGE, ax=axes[1])
    axes[1].set_title("Prefix cache hit rate per node")
    axes[1].set_ylabel("Hit rate")
    axes[1].yaxis.set_major_formatter(lambda v, _: f"{v:.0%}")
    for ax in axes:
        ax.set_xlabel("")
        ax.get_legend().remove()
    handles, labels = axes[0].get_legend_handles_labels()
    fig.legend(handles, labels, loc="lower center", ncol=len(labels), frameon=False,
               bbox_to_anchor=(0.5, -0.06))
    save(fig, out, "per_node.png")

    # Queueing and KV cache pressure over time, one panel per node.
    for column, title, name in [
        ("requests_waiting", "Requests waiting", "waiting_over_time.png"),
        ("kv_cache_usage", "KV cache usage", "kv_usage_over_time.png"),
    ]:
        grid = sns.relplot(series, x="elapsed_s", y=column, hue="router", col="node",
                           col_wrap=2, kind="line", palette=palette, height=2.6, aspect=1.5,
                           errorbar=FULL_RANGE)
        grid.set_axis_labels("Seconds into run", title)
        grid.set_titles("{col_name}")
        grid.legend.set_title(None)
        if column == "kv_cache_usage":
            for ax in grid.axes:
                ax.yaxis.set_major_formatter(lambda v, _: f"{v:.0%}")
        grid.figure.suptitle(f"{title} per node", fontweight="bold", y=1.02)
        save(grid.figure, out, name)


if __name__ == "__main__":
    main()
