# Helm chart

The values of the chart in `charts/kausa`, what each one turns into, and the objects it creates.

Status as of 2026-10-29: the chart exists and is checked by `helm lint` and renders in CI, by a
server-side dry run against Kubernetes v1.36 (k3s), by one install in dry-run on that cluster
(admitted under the restricted Pod Security Standard), and by the kind test, which installs it
with an endpoint in each `rbac.scope` (both passed on 2026-10-28 on the maintainer's machine).
Since v0.1.0 (2026-10-28) each release attaches the packaged chart to its GitHub release and
publishes the image it uses by default, and the v0.1.0 chart was installed in dry-run in kind with
that image ([Install kausa with Helm](../how-to/install-with-helm.md)). `rbac.scope` and
`rbac.namespaces` are in the chart since v0.3.0 (2026-10-29): the v0.2.0 and v0.1.0
charts have neither and always create the ClusterRole. The `history` values, the Explanation
CustomResourceDefinition and its reader role came with v0.3.0 too, and the kind test installs
them in both scopes. So did the `notifications` values, which the kind test installs in the
`json` format in both scopes. The `rollouts` value, the Deployment watch for stalled rollouts
(`D014`), was added for v0.4 on 2026-10-29 and is checked by the renders and the kind test in both
scopes ([0018. Explain a Deployment past its progress deadline (`D014`) from a Deployment watch
that is on by default, and add nothing for StatefulSets or
DaemonSets](../adr/0018-stuck-rollouts.md)). Why it is built this way: [0010. Ship a static binary
on distroless, with a Helm chart, built in CI and not published
yet](../adr/0010-packaging.md), [0012. Release from a tag: a signed image on GHCR and the
chart on the GitHub release](../adr/0012-releases.md) and [0013. Offer a namespaced install mode:
one watch pair and one Role per listed namespace, and nothing
cluster-wide](../adr/0013-namespaced-install-mode.md) and [0014. Keep each incident's full
explanations in a namespaced Explanation resource, off by default](../adr/0014-explanation-history.md) and [0015. Post each new explanation to one optional webhook, as JSON, Slack or
Telegram](../adr/0015-notifications.md).

## Objects

| Object | Name | Notes |
|---|---|---|
| ServiceAccount | the release's full name | `serviceAccount.name` to override |
| ClusterRole and ClusterRoleBinding | the release's full name | Only with `rbac.scope=cluster`. The table in [Permissions](permissions.md); `pods/log` only when kausa explains (an endpoint or `dryRun`) and `logs` is true; `create` and `patch` on Events only with an endpoint, `events` true and no `dryRun`; `list` and `watch` on `deployments` only while `rollouts` is on |
| Role and RoleBinding, in each namespace of `rbac.namespaces` | the release's full name | Only with `rbac.scope=namespaced`. The same rules, with the same conditions; the RoleBinding's subject is the ServiceAccount in the release namespace |
| Deployment | the release's full name | One replica, `Recreate`, the restricted Pod Security settings, a read-only root filesystem, `POD_NAME` from the downward API |
| Service | `<full name>-metrics` | Only with `metrics.enabled` |
| NetworkPolicy | the release's full name | Only with `networkPolicy.enabled` |
| CustomResourceDefinition | `explanations.kausa.fedeam.com` | Only while the release writes history (`kausa.writesHistory`) and `history.installCRD` is true. Cluster-scoped, in both `rbac.scope`s. No `helm.sh/resource-policy: keep` ([Explanation resource](explanation-resource.md)) |
| ClusterRole | `<full name>-explanation-reader` | Only with the CustomResourceDefinition and `history.readers.aggregate`: `get`, `list` and `watch` on `explanations`, aggregated to `view` ([Permissions](permissions.md)) |

The full name is the release name when it contains `kausa`, and `<release>-kausa` otherwise.
Every object is in the release namespace, except the Roles and RoleBindings, which are in the
listed namespaces, and the CustomResourceDefinition and the reader role, which are
cluster-scoped. The chart creates no Namespace: each listed one must exist before `helm
install` (a chart that owned them would delete their workloads on `helm uninstall`).

The RBAC objects come from `templates/rbac.yaml`, and their rules from the `kausa.rules` helper in
`templates/_helpers.tpl`, which the ClusterRole and every Role render: a permission added later
goes there only, so that both scopes grant the same. The history's rule (`list`, `create`,
`patch` and `delete` on `explanations` in `kausa.fedeam.com`) is appended last, only while the
release writes history. The Deployment watch's rule (`list` and `watch` on `deployments` in
`apps`) comes right after the `replicasets` rule, only while `rollouts` is on; with `rollouts:
false` the rules are v0.3.0's, byte for byte.

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
| `rollouts` | `true` | Watch Deployments and explain rollouts past their progress deadline (`D014`). `false` adds `--no-rollouts` and removes `list` and `watch` on `deployments` from the ClusterRole or the Roles. Only the value `false` turns it off: a missing value (the values of a v0.3 release reused with `--reuse-values`) or `null` means on. Independent of the endpoint and `dryRun`. No refusal |
| `history.enabled` | `false` | With an endpoint and without `dryRun`: `--history --history-days <days>`, the `explanations` rule, and (with `installCRD`) the CustomResourceDefinition and the reader role. Turning it off, removing the endpoint or setting `dryRun` deletes them, and every Explanation in the cluster |
| `history.days` | `7` | `--history-days`: Explanations older than this many days are deleted, 1 to 30 |
| `history.installCRD` | `true` | Whether this release installs the CustomResourceDefinition and the reader role. `false` when another release or a cluster administrator installs them |
| `history.readers.aggregate` | `true` | The reader role, which lets `view`, `edit` and `admin` read Explanations; `false` leaves it out |
| `notifications.enabled` | `false` | Post each new explanation to one webhook ([Notifications](#notifications)): `--notify-url-file /var/run/secrets/kausa-notify/url --notify-format <format>`, and the Secret volume. Needs `model.endpoint`, and not with `dryRun` |
| `notifications.format` | `json` | `--notify-format`: `json`, `slack` or `telegram` |
| `notifications.existingSecret` | `""` | Required when enabled: a Secret of yours in the release namespace that holds the webhook URL, mounted read-only at `/var/run/secrets/kausa-notify/` (mode 0440, group 65532) |
| `notifications.urlKey` | `url` | The key of that Secret that holds the whole URL, mounted as the file `url` |
| `notifications.tokenKey` | `""` | `json` only, optional: the key that holds a bearer token, mounted as the file `token` and passed as `--notify-token-file` |
| `notifications.telegramChatId` | `""` | Required with `telegram`, refused with the others: `--notify-telegram-chat-id`, an integer or `@` and a channel's username |
| `notifications.allowHttp` | `false` | `--notify-allow-http`, for an `http://` webhook in the cluster or on the same private network |
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
| `networkPolicy.enabled` | `false` | A NetworkPolicy with egress to DNS (port 53), the API server, the model endpoint and, with notifications, the webhook only, and ingress to the metrics port only |
| `networkPolicy.apiServer.cidrs` | `[]` | The API server's addresses; required with the policy |
| `networkPolicy.apiServer.port` | `6443` | |
| `networkPolicy.endpoint.to` | `[]` | NetworkPolicy peers of the model endpoint; required with the policy and an endpoint |
| `networkPolicy.endpoint.ports` | `[]` | Their ports |
| `networkPolicy.notifications.to` | `[]` | NetworkPolicy peers of the webhook; required with the policy and notifications |
| `networkPolicy.notifications.ports` | TCP `443` | Their ports; TCP 443 when missing or empty, never every port |
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
| `history.days` is not a whole number from 1 to 30, whether the history is on or off (for example `0`, `31`, `abc` or `7.5`) | `history.days must be a whole number of days from 1 to 30, not '<value>'` |
| Notifications without `existingSecret` | `notifications.existingSecret is required with notifications.enabled: the Secret that holds the webhook URL` |
| Notifications with another format | `notifications.format must be json, slack or telegram, not '<value>'` |
| `telegram` without a chat id | `notifications.telegramChatId is required with notifications.format=telegram` |
| A chat id with `json` or `slack` | `notifications.telegramChatId is used only with notifications.format=telegram` |
| `tokenKey` with `slack` or `telegram` | `notifications.tokenKey is used only with notifications.format=json` |
| Notifications without `model.endpoint` | `notifications.enabled needs model.endpoint` |
| Notifications with `dryRun` | `notifications.enabled is not used with dryRun: no explanation is written` |
| Notifications and the NetworkPolicy without the webhook's peers | `networkPolicy.notifications.to is required with networkPolicy.enabled and notifications.enabled: without it kausa cannot reach the webhook` |

The checks are in `kausa.validate` (`templates/_helpers.tpl`). `rbac` is read with defaults, so a
missing or null map reaches the first message and never a nil-pointer error.

The notification checks run only with `notifications.enabled`.

## Notifications

With `notifications.enabled`, the helper `kausa.notifies` is `true`, and the chart renders:

- **The arguments**, after the history's and before `--metrics-address`, in this order:
  `--notify-url-file /var/run/secrets/kausa-notify/url`, `--notify-format "<format>"`, then
  `--notify-telegram-chat-id "<id>"` with `telegram`, `--notify-token-file
  /var/run/secrets/kausa-notify/token` with a `tokenKey`, and `--notify-allow-http` with
  `allowHttp`. The URL itself is never in the Pod spec.
- **The volume `notify`**: the Secret `existingSecret`, `defaultMode: 0440`, with the item
  `urlKey` as `url` and, with a `tokenKey`, that item as `token`, mounted read-only at
  `/var/run/secrets/kausa-notify` (readable through the Pod's `fsGroup`, 65532). It is never
  optional: a missing Secret or key leaves the Pod in `ContainerCreating` with a `FailedMount`
  event (`secret "<name>" not found`, or `references non-existent secret key: <key>`), not a
  kausa that starts without notifications; once the Secret is fixed, the same Pod starts with no
  restart (seen in kind on 2026-10-29). It shares the `volumes` and `volumeMounts` lists with the
  API key's volume.
- **A fourth egress rule** in the NetworkPolicy, after the model's, commented `# The notification
  webhook.`: `to` from `networkPolicy.notifications.to`, and always `ports`, from
  `networkPolicy.notifications.ports`, or TCP 443 when they are missing or empty. Unlike
  `networkPolicy.endpoint.ports`, where an empty list leaves the rule without `ports` (every port
  to those peers), an empty list here falls back to 443. A NetworkPolicy cannot select a
  hostname: a hosted receiver needs an `ipBlock`, a CNI with FQDN policies or an egress proxy.
- **A paragraph in `NOTES.txt`** that names the format, the Secret and its `urlKey` (and
  `tokenKey`), and how to find failures: `kausa_notifications_total{outcome="failed"}` and the
  `notification failed` log line, with a `kubectl logs` command.

No RBAC rule changes, in either scope: `kausa.rules` is the same with notifications on or off,
and the ClusterRole and every Role render the same.

`notifications` and `networkPolicy.notifications` are read nil-safely (`default dict`): a release
whose stored values have neither (one installed with an earlier chart and upgraded with
`--reuse-values`) renders with notifications off, with no nil-pointer error, and `--reuse-values
--set notifications.enabled=true ...` turns them on. An unquoted `telegramChatId` in a values file
(`-1001234567890`) reaches the template as a floating-point number; the chart prints it as a whole
number, `"-1001234567890"`, not in exponent notation. Quoting it is clearer.

## The history

Three helpers in `templates/_helpers.tpl` decide what the history renders:

| Helper | Gives `true` when | Renders |
|---|---|---|
| `kausa.writesHistory` | `history.enabled`, `model.endpoint` set, and `dryRun` false | `--history --history-days "<days>"` in the Deployment's arguments (after the `--no-events` position), the `explanations` rule in `kausa.rules`, and the history lines of `NOTES.txt` |
| `kausa.historyDays` | (gives the days as a string) | The value of `--history-days` and of the `history.days` check |
| `kausa.installsCRD` | `kausa.writesHistory` and `history.installCRD` | `templates/explanation-crd.yaml`, and with `history.readers.aggregate`, `templates/explanation-reader.yaml` |

`history` is read nil-safely: a release whose stored values have no `history` map (one installed
with an earlier chart and upgraded with `--reuse-values`, which renders with the old chart's
values) renders as `enabled: false`, `days: 7`, `installCRD: true` and `readers.aggregate: true`,
with no nil-pointer error. `days`, `installCRD` and `readers.aggregate` default only when the key
is missing, so an explicit `false` stays false. The NetworkPolicy does not change: the history
goes to the API server only.

**Life cycle.** The CustomResourceDefinition is a template, not in `crds/`, so Helm upgrades it
with the release, and it carries no `helm.sh/resource-policy: keep`. It exists exactly while the
release's kausa writes, and so sweeps, the history. **`helm uninstall`, `history.enabled=false`,
an empty `model.endpoint` or `dryRun=true` deletes the CustomResourceDefinition and every
Explanation in the cluster**; the next explanation after turning the history back on starts it
again. In kind on 2026-10-28, `helm upgrade --reuse-values --set history.enabled=false` and `--set
dryRun=true` removed the rule, the CRD, the reader and every Explanation.

**Two releases in one cluster** (namespaced scope, with disjoint `rbac.namespaces`): exactly one
sets `history.installCRD=true`, the others `false`. Helm refuses to install a second release that
renders the same CustomResourceDefinition (`invalid ownership metadata`, naming the release that
owns it; seen in kind on 2026-10-28). Uninstalling the release that owns it, or turning its
history off, deletes every release's history. Each kausa sweeps only its own namespaces.

**`history.installCRD=false`**, for an installer without the right to create a
CustomResourceDefinition or a ClusterRole: the release renders the flags and the rule but neither
object, and a cluster administrator applies both from the same values:

```sh
helm template kausa charts/kausa --kube-version 1.36.0 -f values.yaml \
  --set history.installCRD=true \
  --show-only templates/explanation-crd.yaml --show-only templates/explanation-reader.yaml |
  kubectl apply -f -
```

That CustomResourceDefinition's lifetime is then the administrator's: uninstalling the release does
not delete it, and nothing prunes its Explanations while no kausa runs with `--history`. Until it
exists, kausa logs `history write failed` and `history sweep failed` naming it, and keeps working.
A namespaced install with `installCRD=false` creates nothing cluster-scoped.

`NOTES.txt`, printed after an install or upgrade, names the scope: in cluster scope its first
lines say that kausa can read Pods, Events and, with `rollouts` on, Deployments (and container
logs, when it explains with `logs` on) in every namespace; in namespaced scope it lists the
namespaces, names the same kinds, and, when kausa writes Events, prints one `kubectl get
events.events.k8s.io -n <namespace> --field-selector reportingController=kausa` per namespace
instead of one with `-A`.
Both remind that `rbac.scope` must be set on every upgrade. When the release writes history,
`NOTES.txt` adds `kubectl get explanations -A` (or one `kubectl get explanations -n <namespace>`
per listed namespace) and `kubectl get explanation -n <namespace> <name> -o yaml`, and says that
uninstalling or turning the history off deletes the CustomResourceDefinition and every
Explanation; with `history.installCRD=false`, it names `explanations.kausa.fedeam.com` and says
that a cluster administrator installs it.

To lint and render the chart without a cluster:

```sh
helm lint --strict charts/kausa --set rbac.scope=cluster
helm template kausa charts/kausa --kube-version 1.36.0 --set rbac.scope=cluster
helm template kausa charts/kausa --kube-version 1.36.0 \
  --set rbac.scope=namespaced --set 'rbac.namespaces={shop,web}'
```

The Dockerfile's `ci` target renders both scopes, the history toggles, each notification format
with and without the NetworkPolicy, values without a `notifications` map, the `rollouts` value
(the `deployments` rule right after `replicasets` by default, none and one `--no-rollouts` with
`false`, `null` rendered as the default), and every `rbac`, `history.days` and notification
refusal, and `tests/chart/render.sh` runs a longer set of the same checks (helm only, no cluster),
including a comparison of the cluster-scope rules with the v0.1.0 chart's: equal with `rollouts:
false`, and with only the `deployments` rule more by default.

`helm template` without a cluster needs `--kube-version` (1.32 or later): the chart declares
`kubeVersion: ">=1.32.0-0"`, and helm's default version can be older.
