# Install kausa with Helm

Install the released chart, which runs the published image, first in dry-run and then pointed at
a model.

Status as of 2026-10-28: the v0.1.0 chart from the GitHub release was installed in dry-run in kind
(Kubernetes v1.37) with the published image, on 2026-10-28. The chart in the repository was
checked with a server-side dry run against Kubernetes v1.36, installed in dry-run on a Raspberry
Pi 4 node, and installed with an endpoint by the kind test (`tests/e2e/run.sh`, passed on
2026-10-27). The chart in the repository adds the required `rbac.scope` and the namespaced scope,
which no release has yet; both scopes passed the kind test on 2026-10-28 ([0013. Offer a
namespaced install mode: one watch pair and one Role per listed namespace, and nothing
cluster-wide](../adr/0013-namespaced-install-mode.md)).

## Before you start

- A cluster where you may create a namespace and, for `rbac.scope=cluster`, a ClusterRole and a
  ClusterRoleBinding (for `rbac.scope=namespaced`, a Role and a RoleBinding in each listed
  namespace instead: [Watch only some namespaces](#watch-only-some-namespaces)), and `kubectl` and
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
   CHART=https://github.com/fedeam/kausa/releases/download/v0.1.0/kausa-0.1.0.tgz
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
   every namespace. The chart requires the value, on every install and upgrade; the v0.1.0 chart
   ignores it. To grant less, see [Watch only some namespaces](#watch-only-some-namespaces).

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

## Upgrade from v0.1

The v0.1.0 chart had no `rbac.scope`, so the first upgrade to a chart that requires it sets it
once, `--reuse-values` included: `--reuse-values` reuses the values of v0.1's release, which have
no `rbac` map, and the chart refuses to render without it (checked in kind on 2026-10-28 with Helm
3.16.1: the upgrade fails with `rbac.scope is not set` and the release stays as it was).

```sh
helm upgrade kausa <chart> -n kausa --reuse-values --set rbac.scope=cluster
```

`rbac.scope=cluster` keeps v0.1's ClusterRole, its rules and the whole-cluster watch. After that,
`--reuse-values` carries the scope on. Without `--reuse-values`, pass the scope every time, best
from a values file: `helm upgrade --reset-values`, or an upgrade with `--set` and neither
`--reuse-values` nor a values file, renders the chart's defaults, which have no scope, and fails.

## Watch only some namespaces

With `rbac.scope=namespaced`, kausa watches only the namespaces in `rbac.namespaces` (at most 32),
and the chart grants it a Role and a RoleBinding in each of them and nothing cluster-scoped: no
ClusterRole, no access to `kube-system`, to other namespaces or to its own namespace unless it is
listed ([Permissions](../reference/permissions.md)).

Status as of 2026-10-28, no release has this scope yet: it needs the chart and the image of the
same commit ([Run your own build](#run-your-own-build)). The v0.1.0 image refuses `--namespace`,
and the v0.1.0 chart ignores `rbac` and creates a ClusterRole.

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

## Verify

- `kubectl -n kausa get pods`: one Pod, ready once every watch has listed (two, or two per listed
  namespace).
- When a workload has been failing for a minute:
  `kubectl get events.events.k8s.io -A --field-selector reportingController=kausa` (or `-n
  <namespace>` for each listed namespace) lists kausa's Events, and `kubectl describe deployment -n <namespace> <name>` shows the explanation.
- `kubectl get --raw /api/v1/namespaces/kausa/services/kausa-metrics:9090/proxy/metrics` shows
  the counters.

## If something fails

- **`ErrImagePull`** → the cluster cannot pull the image → check that the nodes reach `ghcr.io`,
  or, for your own build, the registry, the tag and `imagePullSecrets`.
- **`helm install` or `helm upgrade` fails with `rbac.scope is not set`** → the scope is required
  on every install and upgrade → pass `--set rbac.scope=cluster` (as v0.1), or `namespaced` with
  `rbac.namespaces`, or the values file you installed with; upgrading from v0.1, once, even with
  `--reuse-values`.
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
- **The Pod restarts with exit code 2** → kausa refused its arguments → `kubectl -n kausa logs
  deploy/kausa --previous` says which.
- **`explanation skipped`** in the log → the budget, the endpoint or the validation stopped it;
  the reason is in the line and in the `ExplanationSkipped` Event.

## Uninstall

```sh
helm uninstall kausa -n kausa
kubectl delete namespace kausa
```

kausa's Events expire with the API server's event TTL (one hour by default).
