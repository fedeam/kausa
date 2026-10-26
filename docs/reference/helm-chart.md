# Helm chart

The values of the chart in `charts/kausa`, what each one turns into, and the objects it creates.

Status as of 2026-10-26: the chart exists and is checked by `helm lint` and renders in CI, by a
server-side dry run against Kubernetes v1.36 (k3s), by one install in dry-run on that cluster
(admitted under the restricted Pod Security Standard), and, once GitHub Actions run, by the kind
test. No image is published, so `image.repository` must point at one you built ([Install kausa
with Helm](../how-to/install-with-helm.md)). Why it is built this way: [0010. Ship a static binary
on distroless, with a Helm chart, built in CI and not published
yet](../adr/0010-packaging.md).

## Objects

| Object | Name | Notes |
|---|---|---|
| ServiceAccount | the release's full name | `serviceAccount.name` to override |
| ClusterRole and ClusterRoleBinding | the release's full name | The table in [Permissions](permissions.md); `pods/log` only when kausa explains (an endpoint or `dryRun`) and `logs` is true; `create` and `patch` on Events only with an endpoint, `events` true and no `dryRun` |
| Deployment | the release's full name | One replica, `Recreate`, the restricted Pod Security settings, a read-only root filesystem, `POD_NAME` from the downward API |
| Service | `<full name>-metrics` | Only with `metrics.enabled` |
| NetworkPolicy | the release's full name | Only with `networkPolicy.enabled` |

The full name is the release name when it contains `kausa`, and `<release>-kausa` otherwise.

## Values

| Value | Default | Becomes |
|---|---|---|
| `image.repository` | `ghcr.io/fedeam/kausa` | The image; nothing is published there yet |
| `image.tag` | the chart's `appVersion` | |
| `image.digest` | `""` | `repository@digest`, which takes precedence over the tag |
| `image.pullPolicy` | `IfNotPresent` | |
| `imagePullSecrets` | `[]` | |
| `model.endpoint` | `""` | `--endpoint`. Empty, without `dryRun`: kausa only logs incidents |
| `model.name` | `""` | `--model`; required with an endpoint |
| `model.allowHttp` | `false` | `--allow-http`; required for an `http://` endpoint |
| `model.jsonSchema` | `true` | `false` adds `--no-json-schema` |
| `model.apiKey.existingSecret` | `""` | A Secret of yours, mounted read-only at `/var/run/secrets/kausa/api-key` (mode 0440, group 65532) and passed as `--api-key-file` |
| `model.apiKey.key` | `api-key` | The key of that Secret |
| `dryRun` | `false` | `--dry-run` |
| `logs` | `true` | `false` adds `--no-logs` and removes `pods/log` |
| `events` | `true` | `false` adds `--no-events` and removes the Event writes |
| `settleSeconds` | `60` | `--settle-seconds` |
| `budget.callsPerHour` | `30` | `--max-calls-per-hour` |
| `budget.callsPerDay` | `200` | `--max-calls-per-day` |
| `budget.maxInputTokens` | `6000` | `--max-input-tokens` |
| `budget.dedupHours` | `6` | `--dedup-hours` |
| `metrics.enabled` | `true` | `--metrics-address <bindAddress>:<port>`, the liveness (`/healthz`) and readiness (`/readyz`) probes, and the Service |
| `metrics.port` | `9090` | |
| `metrics.bindAddress` | `[::]` | IPv4 and IPv6 on Linux (checked in the image on 2026-10-26, also with IPv6 turned off in the container); `0.0.0.0` for nodes booted with `ipv6.disable=1` |
| `metrics.service.type` | `ClusterIP` | |
| `metrics.service.annotations` | `{}` | For example, scrape annotations |
| `serviceAccount.name` | `""` | |
| `serviceAccount.annotations` | `{}` | |
| `resources` | requests `10m` CPU and `32Mi`, limits `200m` and `128Mi` | Room above the 2 MiB and 1m measured on a Raspberry Pi 4 in dry-run ([Memory and CPU on a Raspberry Pi 4](../research/2026-10-26-resource-use-on-a-raspberry-pi.md)) |
| `networkPolicy.enabled` | `false` | A NetworkPolicy with egress to DNS (port 53), the API server and the model endpoint only, and ingress to the metrics port only |
| `networkPolicy.apiServer.cidrs` | `[]` | The API server's addresses; required with the policy |
| `networkPolicy.apiServer.port` | `6443` | |
| `networkPolicy.endpoint.to` | `[]` | NetworkPolicy peers of the model endpoint; required with the policy and an endpoint |
| `networkPolicy.endpoint.ports` | `[]` | Their ports |
| `networkPolicy.metricsFrom` | `[]` | Peers allowed to reach the metrics port; empty allows any |
| `podAnnotations`, `podLabels`, `nodeSelector`, `tolerations`, `affinity` | empty | Passed to the Pod |

The chart refuses to render (`helm install` fails with the reason) when an endpoint has no
`model.name`, when an `http://` endpoint lacks `model.allowHttp`, and when the NetworkPolicy is
enabled without the API server's addresses or, with an endpoint, without its peers.

`helm template` without a cluster needs `--kube-version` (1.32 or later): the chart declares
`kubeVersion: ">=1.32.0-0"`, and helm's default version can be older.
