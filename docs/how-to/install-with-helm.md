# Install kausa with Helm

Install the released chart, which runs the published image, first in dry-run and then pointed at
a model.

Status as of 2026-10-29: the v0.1.0 chart from the GitHub release was installed in dry-run in kind
(Kubernetes v1.37) with the published image, on 2026-10-28. The chart in the repository was
checked with a server-side dry run against Kubernetes v1.36, installed in dry-run on a Raspberry
Pi 4 node, and installed with an endpoint by the kind test (`tests/e2e/run.sh`, passed on
2026-10-27). The v0.3.0 chart (2026-10-29) adds the required `rbac.scope` and the namespaced
scope; both scopes passed the kind test on 2026-10-28 ([0013. Offer a
namespaced install mode: one watch pair and one Role per listed namespace, and nothing
cluster-wide](../adr/0013-namespaced-install-mode.md)). It also adds the explanation history
(`history.enabled`, off by default), which the kind test installed
in both scopes on 2026-10-28 ([Keep the history of explanations](#keep-the-history-of-explanations)),
and notifications (`notifications.enabled`, off by default; [Send notifications to Slack, Telegram
or a webhook](send-notifications.md)). The chart in the repository adds, for v0.4 (not released as
of 2026-10-29), a watch on Deployments for stalled rollouts (`rollouts`, on by default), which the
kind test installed in both scopes on 2026-10-29 ([Upgrade to the Deployment
watch](#upgrade-to-the-deployment-watch)).

## Before you start

- A cluster where you may create a namespace and, for `rbac.scope=cluster`, a ClusterRole and a
  ClusterRoleBinding (for `rbac.scope=namespaced`, a Role and a RoleBinding in each listed
  namespace instead: [Watch only some namespaces](#watch-only-some-namespaces)); with the history
  on, also a CustomResourceDefinition and a ClusterRole, unless an administrator installs them
  ([Keep the history of explanations](#keep-the-history-of-explanations)); and `kubectl` and
  Helm (CI uses 4.3.1; the chart was also rendered and installed with 3.16) pointed at it: check
  `kubectl config current-context`.
- Nodes on linux/amd64 or linux/arm64 that can pull from `ghcr.io` (or your own build, below).
- A model endpoint: a hosted one with its API key, or one in the cluster (for example Ollama).

## Steps

1. **Pick the release and, optionally, verify it.** The chart of each release is an asset of
   its [GitHub release](https://github.com/fedeam/kausa/releases) and uses the image
   `ghcr.io/fedeam/kausa` of the same version. The release notes have the commands to verify the
   image's signature and provenance and the chart's checksum with cosign and the GitHub CLI.

   ```sh
   CHART=https://github.com/fedeam/kausa/releases/download/v0.3.0/kausa-0.3.0.tgz
   ```

2. **Create the namespace and, for a hosted endpoint, the API key's Secret.** kausa never reads it
   through the API; the kubelet mounts it as a file:

   ```sh
   kubectl create namespace kausa
   kubectl label namespace kausa pod-security.kubernetes.io/enforce=restricted
   kubectl -n kausa create secret generic kausa-api-key --from-file=api-key=./api-key.txt
   ```

3. **See what kausa would send, first:**

   ```sh
   helm install kausa "$CHART" -n kausa --set rbac.scope=cluster --set dryRun=true
   kubectl -n kausa logs deploy/kausa -f
   ```

   `rbac.scope=cluster` lets kausa read Pods, Events and (when it explains) container logs in
   every namespace. The chart requires the value, on every install and upgrade; the v0.2.0 chart
   and earlier ignore it. To grant less, see [Watch only some namespaces](#watch-only-some-namespaces).

   Each `request` line is the redacted body that would leave the cluster. In dry-run the chart
   grants `pods/log` but no Event writes.

4. **Point it at the model.** `--reuse-values` keeps the values of step 3, `rbac.scope` included:

   ```sh
   helm upgrade kausa "$CHART" -n kausa --reuse-values --set dryRun=false \
     --set model.endpoint=https://api.openai.com/v1 --set model.name=<model> \
     --set model.apiKey.existingSecret=kausa-api-key
   ```

   For an endpoint in the cluster over plain HTTP, add `--set model.allowHttp=true` and leave the
   Secret out if it needs no key. Every value: [Helm chart](../reference/helm-chart.md).

5. **Optionally, limit its traffic** with `--set networkPolicy.enabled=true`, the API server's
   addresses in `networkPolicy.apiServer.cidrs` (from
   `kubectl get endpointslices -n default -l kubernetes.io/service-name=kubernetes`) and the
   endpoint's peers in `networkPolicy.endpoint.to`.

## Run your own build

To run a build of your own (a fork, a commit after the release), build the image on a machine of
your nodes' architecture (the build is native: an arm64 machine builds the arm64 image), push it,
and install the chart of the same commit with your image:

```sh
docker build --target runtime -t <registry>/kausa:<tag> .
docker push <registry>/kausa:<tag>
helm install kausa charts/kausa -n kausa --set rbac.scope=cluster \
  --set image.repository=<registry>/kausa --set image.tag=<tag> --set dryRun=true
```

## Upgrade from a chart without rbac.scope

The charts of v0.2.0 and v0.1.0 have no `rbac.scope`, so the first upgrade to a chart that requires
it sets it once, `--reuse-values` included: `--reuse-values` reuses the values of the earlier
release, which have no `rbac` map, and the chart refuses to render without it (checked in kind on
2026-10-28 with Helm 3.16.1, from the v0.1.0 chart: the upgrade fails with `rbac.scope is not set`
and the release stays as it was; the v0.2.0 chart's values have no `rbac` map either).

```sh
helm upgrade kausa <chart> -n kausa --reuse-values --set rbac.scope=cluster
```

`rbac.scope=cluster` keeps the ClusterRole, its rules and the whole-cluster watch. After that,
`--reuse-values` carries the scope on. Without `--reuse-values`, pass the scope every time, best
from a values file: `helm upgrade --reset-values`, or an upgrade with `--set` and neither
`--reuse-values` nor a values file, renders the chart's defaults, which have no scope, and fails.

## Upgrade to the Deployment watch

From v0.4, kausa also watches Deployments, to explain a rollout past its progress deadline
(`D014`), and its role gains `list` and `watch` on `deployments` in both scopes ([0018. Explain a
Deployment past its progress deadline (`D014`) from a Deployment watch that is on by default, and
add nothing for StatefulSets or DaemonSets](../adr/0018-stuck-rollouts.md)).

- **Upgrade the chart and the image together.** A v0.4 image with the v0.3 chart's role, RBAC made
  outside the chart, or the read-only proxy of [Try kausa against a cluster with a local
  model](try-with-a-local-model.md) without the Deployment paths, gets `403` on the Deployment
  watch: kausa explains everything else but is never Ready, `helm upgrade --wait` times out, and
  `deployment watch error` lines whose `error` says `deployments.apps is forbidden` appear at most
  every 30 seconds. The chart's own upgrade needs nothing more: a missing `rollouts` value, as
  `--reuse-values` gives from a v0.3 release, means on.
- **RBAC made outside the chart** adds this rule to the ClusterRole, or to the Role of each
  watched namespace:

  ```yaml
  - apiGroups: ["apps"]
    resources: ["deployments"]
    verbs: ["list", "watch"]
  ```

- **To keep v0.3's permissions**, turn the watch off: `--set rollouts=false` removes the rule and
  passes `--no-rollouts` (outside the chart, run `kausa run --no-rollouts`). `D014` is then off.
- **The Deployments' own deadlines decide when `D014` comes:** `spec.progressDeadlineSeconds`
  (600 seconds by default) after the last progress, plus the settle window and a check of 5
  seconds. kausa has no setting for it; lower a Deployment's deadline to hear sooner, and a
  Deployment without one is never explained by `D014`.
- **The first start explains every Deployment already past its deadline**, one window later,
  within the call budget and the queue of 16 incidents waiting for an explanation (beyond it, the
  oldest is dropped and counted in `kausa_explanations_dropped_total`); a restart does it again,
  as for Pod failures. A stall already explained by a Pod rule or `D011` of the same Deployment
  gives no `D014`.

## Watch only some namespaces

With `rbac.scope=namespaced`, kausa watches only the namespaces in `rbac.namespaces` (at most 32),
and the chart grants it a Role and a RoleBinding in each of them and nothing cluster-scoped: no
ClusterRole, no access to `kube-system`, to other namespaces or to its own namespace unless it is
listed ([Permissions](../reference/permissions.md)).

Status as of 2026-10-29: v0.3.0 is the first release with this scope. The v0.2.0 image refuses
`--namespace`, and the v0.2.0 chart ignores `rbac` and creates a ClusterRole.

1. **Create the namespaces first.** The chart creates none, and Helm cannot create a Role in a
   namespace that does not exist.

2. **Keep the scope and the list in a values file** that you pass on every install and upgrade:

   ```yaml
   # kausa-values.yaml
   rbac:
     scope: namespaced
     namespaces: [shop, payments]
   ```

   ```sh
   helm install kausa <chart> -n kausa -f kausa-values.yaml --set dryRun=true
   ```

   Later upgrades pass `-f kausa-values.yaml` again, or use `--reuse-values`. An upgrade that
   resets the values (`--reset-values`, or `--set` without either) fails with `rbac.scope is not
   set` and leaves the Roles as they were, so it never widens kausa to the whole cluster.

3. **Check** that `NOTES.txt` names the scope and the namespaces, that the first line of
   `kubectl -n kausa logs deploy/kausa` (`started`) lists them under `namespaces`, and that
   nothing cluster-scoped was created:

   ```sh
   kubectl get clusterroles,clusterrolebindings -l app.kubernetes.io/instance=kausa
   kubectl get roles,rolebindings -A -l app.kubernetes.io/instance=kausa
   ```

- **Changing the list or the scope** is a `helm upgrade`: Helm creates the Roles now listed and
  deletes the others, and kausa's Pod is replaced, so its budget and deduplication cache start
  over. `helm uninstall` removes every Role and RoleBinding.
- **Back to the whole cluster:** the list must be emptied too, and with `--reuse-values` only an
  explicit empty list does it: `--set rbac.scope=cluster --set-json 'rbac.namespaces=[]'`, or a
  values file with `scope: cluster` and `namespaces: []`. `--set rbac.namespaces=null` and `--set
  'rbac.namespaces={}'` leave a list, and the chart refuses with `rbac.namespaces is set but
  rbac.scope is cluster` (checked in kind on 2026-10-28 with Helm 3.16.1).
- **Several releases** may share a cluster only with disjoint lists, each in its own namespace,
  with its own budget and model key. Two releases watching the same namespace, or a namespaced
  release beside a cluster-scope one, would explain the same incident twice and update each
  other's Events.
- **A namespace deleted later** takes kausa's Role there with it; recreated, it gets no access
  until the next `helm upgrade`. Remove it from the list to stop the retries.

## Keep the history of explanations

An Event keeps a short version of the explanation, cut to 1024 bytes, and the API server deletes
it after an hour by default. With `history.enabled=true`, kausa also keeps each incident's whole
explanations in an Explanation object in its namespace, for `history.days` days (7 by default, 1
to 30) ([Explanation resource](../reference/explanation-resource.md)). Status as of 2026-10-29:
v0.3.0 is the first release with it.

1. **Turn it on** with an endpoint and without `dryRun`; the history is written for explained
   incidents only:

   ```sh
   helm upgrade kausa <chart> -n kausa --reuse-values --set history.enabled=true
   ```

   The chart installs the CustomResourceDefinition `explanations.kausa.fedeam.com` and a reader
   ClusterRole aggregated to `view`, adds `--history --history-days "7"` to kausa's arguments and
   `list`, `create`, `patch` and `delete` on `explanations` to its rules
   ([Permissions](../reference/permissions.md)). A release installed with an earlier chart has no
   `history` values; upgraded with `--reuse-values`, it renders with the history off.

2. **Read it** with `kubectl`, as you read Events: whoever has `view`, `edit` or `admin` in the
   namespace can, unless `history.readers.aggregate=false`:

   ```sh
   kubectl get explanations -A -o wide
   kubectl get explanation -n <namespace> kausa.<fingerprint> -o yaml
   ```

   The name is the Event's name without its last part: the Event `kausa.<fingerprint>.<hash>`
   leads to `kausa.<fingerprint>`.

- **Turning it off deletes all history.** `helm uninstall`, `history.enabled=false`, an empty
  `model.endpoint` or `dryRun=true` deletes the CustomResourceDefinition and, with it, every
  Explanation in the cluster, every release's included. This is on purpose: once kausa stops
  writing, nothing would prune text with real names. Seen in kind on 2026-10-28: `helm upgrade
  --reuse-values --set history.enabled=false`, and `--set dryRun=true`, left no rule, reader role,
  CustomResourceDefinition or Explanation.
- **A namespaced install without cluster rights** sets `history.installCRD=false`: the release
  then creates nothing cluster-scoped, and a cluster administrator installs the
  CustomResourceDefinition and the reader role from the same values:

  ```sh
  helm template kausa <chart> --kube-version 1.36.0 -f kausa-values.yaml \
    --set history.enabled=true --set history.installCRD=true \
    --show-only templates/explanation-crd.yaml --show-only templates/explanation-reader.yaml |
    kubectl apply -f -
  ```

  Their lifetime is then the administrator's: uninstalling the release does not delete them, and
  nothing prunes the Explanations while no kausa runs with `--history`. Until the
  CustomResourceDefinition exists, kausa logs `history write failed` and `history sweep failed`
  naming it, and keeps working. Granting the release's Role may also need the installer to hold
  `list`, `create`, `patch` and `delete` on `explanations` in each listed namespace (RBAC's
  escalation check; hypothesis, as in [0013. Offer a namespaced install mode: one watch pair and
  one Role per listed namespace, and nothing
  cluster-wide](../adr/0013-namespaced-install-mode.md)).
- **A second release** in the same cluster sets `history.installCRD=false`: exactly one release
  installs the CustomResourceDefinition, and Helm refuses a second one that renders it
  (`invalid ownership metadata`, seen in kind on 2026-10-28). Uninstalling the release that
  installs it, or turning its history off, deletes the other releases' history too.
- **Bound the count** in a namespace with a ResourceQuota on
  `count/explanations.kausa.fedeam.com`, if tenants there can create Explanations
  ([Security policy](../../SECURITY.md)).

## Verify

- `kubectl -n kausa get pods`: one Pod, ready once every watch has listed (three, or three per
  listed namespace; two, or two per listed namespace, with `rollouts=false`).
- When a workload has been failing for a minute:
  `kubectl get events.events.k8s.io -A --field-selector reportingController=kausa` (or `-n
  <namespace>` for each listed namespace) lists kausa's Events, and `kubectl describe deployment -n <namespace> <name>` shows the explanation.
- With the history on: `kubectl get explanations -A` lists one object per explained incident.
- `kubectl get --raw /api/v1/namespaces/kausa/services/kausa-metrics:9090/proxy/metrics` shows
  the counters.

## If something fails

- **`ErrImagePull`** → the cluster cannot pull the image → check that the nodes reach `ghcr.io`,
  or, for your own build, the registry, the tag and `imagePullSecrets`.
- **`helm install` or `helm upgrade` fails with `rbac.scope is not set`** → the scope is required
  on every install and upgrade → pass `--set rbac.scope=cluster` (as before), or `namespaced` with
  `rbac.namespaces`, or the values file you installed with; upgrading from the v0.2.0 or v0.1.0
  chart, once, even with `--reuse-values`.
- **`helm install` fails with `model.name is required`** (or another of the chart's checks) → set
  the value it names ([Helm chart](../reference/helm-chart.md)).
- **`helm install` fails with `namespaces "<name>" not found`** → a listed namespace does not
  exist → Helm keeps the release as `failed`: `helm uninstall` it, create the namespace and
  install again (seen in kind on 2026-10-28 with Helm 3.16.1; the uninstall left no Role).
- **The Pod runs but is not Ready, without restarts, and `helm install --wait` or `helm upgrade
  --wait` times out** → kausa was refused (`403`) in a listed namespace before it could list it,
  for example because its RoleBinding there was deleted → a `pod watch error` or `event watch
  error` line in `kubectl -n kausa logs deploy/kausa` names it in its `namespace` field, and its
  `error` says `forbidden` → restore the binding (`helm upgrade` with the same values recreates
  it), or remove the namespace from the list. The other namespaces keep working meanwhile. A
  binding deleted while kausa runs shows up later, when the open watch ends: after about 290
  seconds in kind on 2026-10-28.
- **The Pod runs but is not Ready, and the log has `deployment watch error` with `deployments.apps
  is forbidden`** → kausa's role does not grant `list` and `watch` on Deployments (a v0.4 image on
  the v0.3 chart's role, or RBAC made outside the chart) → upgrade the chart with the image, add
  the rule of [Upgrade to the Deployment watch](#upgrade-to-the-deployment-watch), or set
  `rollouts=false`.
- **The Pod restarts with exit code 2** → kausa refused its arguments → `kubectl -n kausa logs
  deploy/kausa --previous` says which.
- **`history write failed`** with `the CustomResourceDefinition explanations.kausa.fedeam.com is
  not installed` → the history is on but its CustomResourceDefinition is missing (for example
  `history.installCRD=false` and no administrator installed it) → install it as above; kausa
  picks it up without a restart.
- **`history write refused`** → an Explanation of that name exists without kausa's label, or
  about another rule or target → kausa leaves it alone; delete it if it is not yours.
- **`explanation skipped`** in the log → the budget, the endpoint or the validation stopped it;
  the reason is in the line and in the `ExplanationSkipped` Event.

## Uninstall

```sh
helm uninstall kausa -n kausa
kubectl delete namespace kausa
```

kausa's Events expire with the API server's event TTL (one hour by default). With the history on
and `history.installCRD=true`, `helm uninstall` also deletes the CustomResourceDefinition and every
Explanation in the cluster; with `history.installCRD=false`, the administrator who installed it
deletes it (`kubectl delete crd explanations.kausa.fedeam.com`), which deletes every Explanation.
