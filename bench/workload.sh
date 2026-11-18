# Sourced by bench/run-k8s.sh. Sizes the benchmark
# workload from the vLLM nodes' KV cache, keeping any value already set in the
# environment.
#
# The shared prefixes add up to half the fleet's total KV cache. That's more
# than one node holds (with 3+ nodes), so round robin, which sends every prefix
# to every node, keeps evicting; cache-aware routing spreads the prefixes so
# each node holds about half its cache's worth.

# Usage: size_workload KV_TOKENS_PER_NODE NODES
size_workload() {
    local kv_tokens=$1 nodes=$2
    export PREFIX_TOKENS=${PREFIX_TOKENS:-2000}
    export PREFIXES=${PREFIXES:-$((nodes * kv_tokens / 2 / PREFIX_TOKENS))}
    # Each prefix is used ~16 times, so only 1 in 16 has to miss.
    export REQUESTS=${REQUESTS:-$((PREFIXES * 16))}
    export CONCURRENCY=${CONCURRENCY:-$((nodes * 8))}
    export USER_TOKENS=${USER_TOKENS:-100}
    export OUTPUT_TOKENS=${OUTPUT_TOKENS:-32}
    echo "KV cache: $kv_tokens tokens per node x $nodes nodes"
    echo "workload: $PREFIXES prefixes x $PREFIX_TOKENS tokens = $((PREFIXES * PREFIX_TOKENS)) prefix tokens," \
        "$REQUESTS requests at concurrency $CONCURRENCY"
}
