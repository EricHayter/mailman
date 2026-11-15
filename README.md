# mailman
A router for vLLM

## Local deployment (CPU, kind)

Runs two CPU vLLM pods (a tiny model) and mailman in a local
[kind](https://kind.sigs.k8s.io/) cluster. mailman finds the vLLM pods through
the `vllm` Service's EndpointSlices.

Needs Docker, [kind](https://kind.sigs.k8s.io/docs/user/quick-start/#installation),
kubectl, `envsubst` (gettext) and `jq`. vLLM takes about 4 CPUs and 8 GiB of
memory; set `replicas: 1` in `deploy/vllm-cpu.yaml` on a smaller machine.

```
scripts/kind-up.sh
```

This creates the cluster (named `mailman`, override with `KIND_CLUSTER`) if it
doesn't exist, deploys vLLM, then builds mailman's image, loads it into the
cluster and deploys it. The first run downloads the vLLM image and model, so it
takes a few minutes. Rerun it after changing mailman's code to rebuild and
redeploy.

Send a request through mailman:

```
kubectl --context kind-mailman port-forward svc/mailman 3000:3000
scripts/chat.sh "hello"
```

Useful while it runs:

```
kubectl --context kind-mailman logs -f deploy/mailman
kubectl --context kind-mailman get endpointslices -l kubernetes.io/service-name=vllm
```

Remove everything with `kind delete cluster --name mailman`.

### Configuration

mailman is configured through environment variables, set in
`deploy/mailman.yaml`:

| Variable | |
|---|---|
| `MAILMAN_K8S_SERVICE` | Service whose pods to route to; without it, mailman sends to `127.0.0.1:8000` |
| `MAILMAN_K8S_POD_LABELS` | label selector for those pods, e.g. `app=vllm` (required with the above) |
| `MAILMAN_K8S_NAMESPACE` | defaults to mailman's own namespace |
| `MAILMAN_K8S_PORT_NAME` | only needed if the Service has several ports |
| `RUST_LOG` | log filter, e.g. `info` or `mailman=debug` |
| `LOG_FORMAT` | `json` for JSON logs |
