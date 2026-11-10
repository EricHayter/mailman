#!/usr/bin/env bash
# Sends a chat completion request and prints the response body. The HTTP
# status goes to stderr, so the body can be piped to `jq`.
#
# Usage: scripts/chat.sh [-s] [-a addr] [-m model] <message>
#   -s        stream the response
#   -a addr   host:port to send to (overrides CHAT_ADDR)
#   -m model  model name (overrides CHAT_MODEL)
#
# Settings are read from .env in the repo root (see .env.example). Variables
# already set in your shell take precedence over .env.
#   CHAT_ADDR        host:port, e.g. 127.0.0.1:3000 for mailman
#   CHAT_MODEL       model name (default: the model of the COMPOSE_FILE
#                    deployment)
#   CHAT_MAX_TOKENS  max tokens to generate (default 64; CPU vLLM is slow)
#   CHAT_PATH        request path (default /chat/completions; use
#                    /v1/chat/completions to send straight to a vLLM node)
set -euo pipefail

usage() {
    # Print the comment block at the top of this file.
    awk 'NR > 1 && /^#/ { sub(/^# ?/, ""); print; next } NR > 1 { exit }' "$0" >&2
    exit 1
}

# Read KEY=VALUE lines without executing the file, skipping keys the shell
# already has.
env_file="$(dirname "$0")/../.env"
if [[ -f "$env_file" ]]; then
    while IFS='=' read -r key value || [[ -n "$key" ]]; do
        [[ -z "$key" || "$key" == \#* ]] && continue
        value="${value%\"}"
        value="${value#\"}"
        [[ -z "${!key+set}" ]] && export "$key=$value"
    done <"$env_file"
fi

stream=false
addr="${CHAT_ADDR:-}"
model="${CHAT_MODEL:-}"
while getopts "sa:m:" opt; do
    case "$opt" in
    s) stream=true ;;
    a) addr="$OPTARG" ;;
    m) model="$OPTARG" ;;
    *) usage ;;
    esac
done
shift $((OPTIND - 1))
[[ $# -eq 1 ]] || usage
message=$1

# Default to the model the deployment runs, so the two can't drift apart.
if [[ -z "$model" ]]; then
    model=$(cd "$(dirname "$0")/.." &&
        docker compose config --format json 2>/dev/null |
        jq -r '.services.vllm.command | index("--model") as $i | .[$i + 1] // empty') || true
fi

if [[ -z "$addr" || -z "$model" ]]; then
    echo "error: no address or model; pass -a and -m, or set COMPOSE_FILE in .env (cp .env.example .env)" >&2
    exit 1
fi

body=$(jq -n \
    --arg model "$model" \
    --arg message "$message" \
    --argjson stream "$stream" \
    --argjson max_tokens "${CHAT_MAX_TOKENS:-64}" \
    '{model: $model, messages: [{role: "user", content: $message}], stream: $stream, max_tokens: $max_tokens}')

curl --silent --show-error --no-buffer \
    --write-out '%{stderr}\nHTTP %{http_code}\n' \
    --header 'Content-Type: application/json' \
    --data "$body" \
    "http://${addr}${CHAT_PATH:-/chat/completions}"
