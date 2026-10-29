# Helm chart

The values of the chart in `charts/kausa`, what each one turns into, and the objects it creates.

Status as of 2026-10-28: the chart exists and is checked by `helm lint` and renders in CI, by a
server-side dry run against Kubernetes v1.36 (k3s), by one install in dry-run on that cluster
(admitted under the restricted Pod Security Standard), and by the kind test, which installs it
with an endpoint in each `rbac.scope` (both passed on 2026-10-28 on the maintainer's machine).
Since v0.1.0 (2026-10-28) each release attaches the packaged chart to its GitHub release and
publishes the image it uses by default, and the v0.1.0 chart was installed in dry-run in kind with
that image ([Install kausa with Helm](../how-to/install-with-helm.md)). `rbac.scope` and
`rbac.namespaces` are in the chart of the repository, not in a release yet: the v0.2.0 and v0.1.0
charts have neither and always create the ClusterRole. Why it is built this way: [0010. Ship a static binary
on distroless, with a Helm chart, built in CI and not published
yet](../adr/0010-packaging.md), [0012. Release from a tag: a signed image on GHCR and the
chart on the GitHub release](../adr/0012-releases.md) and [0013. Offer a namespaced install mode:
one watch pair and one Role per listed namespace, and nothing
cluster-wide](../adr/0013-namespaced-install-mode.md).

## Objects

| Object | Name | Notes |
|---|---|---|
| ServiceAccount | the release's full name | `serviceAccount.name` to override |
| ClusterRole and ClusterRoleBinding | the release's full name | Only with `rbac.scope=cluster`. The table in [Permissions](permissions.md); `pods/log` only when kausa explains (an endpoint or `dryRun`) and `logs` is true; `create` and `patch` on Events only with an endpoint, `events` true and no `dryRun` |
| Role and RoleBinding, in each namespace of `rbac.namespaces` | the release's full name | Only with `rbac.scope=namespaced`. The same rules, with the same conditions; the RoleBinding's subject is the ServiceAccount in the release namespace |
| Deployment | the release's full name | One replica, `Recreate`, the restricted Pod Security settings, a read-only root filesystem, `POD_NAME` from the downward API |
| Service | `<full name>-metrics` | Only with `metrics.enabled` |
| NetworkPolicy | the release's full name | Only with `networkPolicy.enabled` |

The full name is the release name when it contains `kausa`, and `<release>-kausa` otherwise.
Every object is in the release namespace, except the Roles and RoleBindings, which are in the
listed namespaces. The chart creates no Namespace: each listed one must exist before `helm
install` (a chart that owned them would delete their workloads on `helm uninstall`).

The RBAC objects come from `templates/rbac.yaml`, and their rules from the `kausa.rules` helper in
`templates/_helpers.tpl`, which the ClusterRole and every Role render: a permission added later
goes there only, so that both scopes grant the same.

## Values

| Value | Default | Becomes |
|---|---|---|
| `image.repository` | `ghcr.io/fedeam/kausa` | The image, published by each release for linux/amd64 and linux/arm64 |
| `image.tag` | the chart's `appVersion` | |
| `image.digest` | `""` | `repository@digest`, which takes precedence over the tag |
| `image.pullPolicy` | `IfNotPresent` | |
| `imagePullSecrets` | `[]` | |
| `rbac.scope` | none: required | `cluster`: the ClusterRole and ClusterRoleBinding, and no `--namespace`, so kausa watches every namespace. `namespaced`: a Role and a RoleBinding in each namespace of `rbac.namespaces`, nothing cluster-scoped, and one `--namespace` per entry. Set it on every install and upgrade, in a values file or with `--reuse-values` |
| `rbac.namespaces` | `[]` | With `namespaced`: the namespaces kausa watches and is granted, 1 to 32 distinct namespace names, in the order of the `--namespace` arguments. Each must exist before `helm install`. Must be empty with `cluster` |
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

The chart refuses to render (`helm install`, `helm upgrade` and `helm template` fail with the
reason, and nothing is applied) in these cases, checked in this order, with these messages:

| When | Message |
|---|---|
| `rbac.scope` is empty or missing (the defaults, `--set rbac=null`, `helm upgrade --reset-values`, or a release of the v0.2.0 or v0.1.0 chart upgraded with `--reuse-values`) | `rbac.scope is not set: set rbac.scope=cluster to let kausa read every namespace, or rbac.scope=namespaced with rbac.namespaces` |
| `rbac.scope` is another value | `rbac.scope must be cluster or namespaced, not '<value>'` |
| `namespaced` with an empty list | `rbac.scope=namespaced needs rbac.namespaces: the namespaces kausa may read` |
| `cluster` with a list | `rbac.namespaces is set but rbac.scope is cluster, which grants every namespace: set rbac.scope=namespaced` |
| A name that is not a namespace name (a DNS-1123 label: lowercase letters, digits and `-`, starting and ending with a letter or digit, 1 to 63 characters), the first in list order | `rbac.namespaces: '<name>' is not a namespace name (lowercase letters, digits and '-', at most 63 characters)` |
| A name listed twice, the first repeat | `rbac.namespaces: '<name>' is listed twice` |
| More than 32 names | `rbac.namespaces lists <n> namespaces; kausa watches at most 32 (use rbac.scope=cluster instead)` |
| An endpoint without `model.name` | `model.name is required with model.endpoint` |
| An `http://` endpoint without `model.allowHttp` | `model.endpoint is plain http://: set model.allowHttp=true (only for an endpoint in the cluster or on the same private network)` |
| The NetworkPolicy without the API server's addresses | `networkPolicy.apiServer.cidrs is required with networkPolicy.enabled: without it kausa cannot reach the API server` |
| The NetworkPolicy with an endpoint (outside `dryRun`) and without its peers | `networkPolicy.endpoint.to is required with networkPolicy.enabled and model.endpoint: without it kausa cannot reach the model` |

The checks are in `kausa.validate` (`templates/_helpers.tpl`). `rbac` is read with defaults, so a
missing or null map reaches the first message and never a nil-pointer error.

`NOTES.txt`, printed after an install or upgrade, names the scope: in cluster scope its first
lines say that kausa can read Pods and Events (and container logs, when it explains with `logs`
on) in every namespace; in namespaced scope it lists the namespaces and, when kausa writes Events, prints one `kubectl get
events.events.k8s.io -n <namespace> --field-selector reportingController=kausa` per namespace
instead of one with `-A`.
Both remind that `rbac.scope` must be set on every upgrade.

To lint and render the chart without a cluster:

```sh
helm lint --strict charts/kausa --set rbac.scope=cluster
helm template kausa charts/kausa --kube-version 1.36.0 --set rbac.scope=cluster
helm template kausa charts/kausa --kube-version 1.36.0 \
  --set rbac.scope=namespaced --set 'rbac.namespaces={shop,web}'
```

The Dockerfile's `ci` target renders both scopes and every `rbac` refusal, and
`tests/chart/render.sh` runs a longer set of the same checks (helm only, no cluster), including a
comparison of the cluster-scope rules with the v0.1.0 chart's.

`helm template` without a cluster needs `--kube-version` (1.32 or later): the chart declares
`kubeVersion: ">=1.32.0-0"`, and helm's default version can be older.
