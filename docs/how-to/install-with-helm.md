# Install kausa with Helm

Install the released chart, which runs the published image, first in dry-run and then pointed at
a model.

Status as of 2026-11-04: the v0.1.0 chart from the GitHub release was installed in dry-run in kind
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
or a webhook](send-notifications.md)). The v0.4.0 chart (2026-10-30) adds a watch on Deployments
for stalled rollouts (`rollouts`, on by default), which the kind test installed in both scopes on
2026-10-29 ([Upgrade to the Deployment watch](#upgrade-to-the-deployment-watch)), and an optional
standby replica (`replicaCount: 2`, off by default), whose renders are checked in both scopes and
whose kind test runs on GitHub Actions ([Run a standby replica](#run-a-standby-replica)). The
chart of v0.5.0 adds `helm test`, which checks the endpoint, the key
and the model with one synthetic request (step 6); its kind test runs on GitHub Actions, and the
v0.4.1 chart has no test ([0020. Check the model endpoint on demand with `kausa check-endpoint` and
`helm test`, and hold calls behind a breaker while it fails](../adr/0020-check-the-endpoint.md)).
From v0.6.0 each release also publishes the chart at `oci://ghcr.io/fedeam/charts/kausa`, signed
and with provenance, with a CycloneDX SBOM for each image ([0023. Publish the chart as a signed
OCI artifact at `oci://ghcr.io/fedeam/charts/kausa`, list it on Artifact Hub, and attest a
CycloneDX SBOM of an auditable binary to each image](../adr/0023-oci-chart-and-sbom.md)). v0.6.0,
released on 2026-11-04, is the first; the v0.5.2 chart and earlier are only on their GitHub
releases. On 2026-11-04 the v0.6.0 chart was pulled from GHCR without a login, matched the signed
checksums, and installed in kind in both scopes and with two replicas; a release installed from the
v0.5.2 chart URL was upgraded to it with `--reuse-values`.

## Before you start

- A cluster where you may create a namespace and, for `rbac.scope=cluster`, a ClusterRole and a
  ClusterRoleBinding (for `rbac.scope=namespaced`, a Role and a RoleBinding in each listed
  namespace instead: [Watch only some namespaces](#watch-only-some-namespaces)); with the history
  on, also a CustomResourceDefinition and a ClusterRole, unless an administrator installs them
  ([Keep the history of explanations](#keep-the-history-of-explanations)); and `kubectl` and
  Helm (CI uses 4.3.1; the chart was also rendered and installed with 3.16; OCI charts need Helm
  3.8 or later) pointed at it: check `kubectl config current-context`.
- Nodes on linux/amd64 or linux/arm64 that can pull from `ghcr.io` (or your own build, below).
- A model endpoint: a hosted one with its API key, or one in the cluster (for example Ollama).

## Steps

1. **Pick the release and, optionally, verify it.** From v0.6.0 the chart of each release is
   published at `oci://ghcr.io/fedeam/charts/kausa` and uses the image `ghcr.io/fedeam/kausa` of
   the same version. The commands below name the chart as `"$CHART" --version "$VERSION"`: without
   `--version`, Helm takes the newest chart on GHCR, and with it a newer image. The release notes
   have the commands to verify the image, the chart and their SBOMs and provenance ([Verify a
   release](#verify-a-release)).

   ```sh
   CHART=oci://ghcr.io/fedeam/charts/kausa
   VERSION=0.6.1
   ```

   The chart of each release is also an asset of its [GitHub
   release](https://github.com/fedeam/kausa/releases). To use it, set
   `CHART=https://github.com/fedeam/kausa/releases/download/v$VERSION/kausa-$VERSION.tgz`; Helm
   3.16.1 and 4.3.1 accept and ignore `--version` with a chart URL (checked on 2026-11-03), so the
   commands below work unchanged, but the URL alone picks the version. The v0.5.2 chart and
   earlier are only there: for them, this is the way to install.

2. **Create the namespace and, for a hosted endpoint, the API key's Secret.** kausa never reads it
   through the API; the kubelet mounts it as a file:

   ```sh
   kubectl create namespace kausa
   kubectl label namespace kausa pod-security.kubernetes.io/enforce=restricted
   kubectl -n kausa create secret generic kausa-api-key --from-file=api-key=./api-key.txt
   ```

3. **See what kausa would send, first:**

   ```sh
   helm install kausa "$CHART" --version "$VERSION" -n kausa --set rbac.scope=cluster --set dryRun=true
   kubectl -n kausa logs deploy/kausa -f
   ```

   `rbac.scope=cluster` lets kausa read Pods, Events and (when it explains) container logs in
   every namespace. The chart requires the value, on every install and upgrade; the v0.2.0 chart
   and earlier ignore it. To grant less, see [Watch only some namespaces](#watch-only-some-namespaces).

   Each `request` line is the redacted body that would leave the cluster. In dry-run the chart
   grants `pods/log` but no Event writes.

4. **Point it at the model.** `--reuse-values` keeps the values of step 3, `rbac.scope` included:

   ```sh
   helm upgrade kausa "$CHART" --version "$VERSION" -n kausa --reuse-values --set dryRun=false \
     --set model.endpoint=https://api.openai.com/v1 --set model.name=<model> \
     --set model.apiKey.existingSecret=kausa-api-key
   ```

   For an endpoint in the cluster over plain HTTP, add `--set model.allowHttp=true` and leave the
   Secret out if it needs no key. Every value: [Helm chart](../reference/helm-chart.md).

5. **Optionally, limit its traffic** with `--set networkPolicy.enabled=true`, the API server's
   addresses in `networkPolicy.apiServer.cidrs` (from
   `kubectl get endpointslices -n default -l kubernetes.io/service-name=kubernetes`) and the
   endpoint's peers in `networkPolicy.endpoint.to`.

6. **Check the endpoint, the key and the model** (with a chart that has the test, from v0.5.0):

   ```sh
   helm test kausa -n kausa --logs
   ```

   It runs `kausa check-endpoint` in a Pod of its own: one request with synthetic data, billed by
   the provider and outside kausa's budget, and one `endpoint check` line. `"result":"ok"` and
   exit `0` mean an answer came back and passed kausa's validation; otherwise `helm test` fails,
   and the line's `cause` and `hint` say what to fix ([If something fails](#if-something-fails)).
   With `model.timeoutSeconds` above 240, add `--timeout 15m` (anything longer than the value
   plus 60 seconds): `helm test` waits 5 minutes by default. The finished Pod stays, with its log,
   until the next `helm test`; delete it with `kubectl -n kausa delete pod kausa-endpoint-check`.
   Install and upgrade never create it; `--set endpointCheck.enabled=false` leaves it out of the
   chart.

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

## Upgrade from a chart URL to the OCI chart

A release installed from a chart URL of a GitHub release (v0.5.2 and earlier, or any later one)
moves to the chart on GHCR with an upgrade to v0.6.0 or a later version; `--reuse-values` keeps
its values, `rbac.scope` included:

```sh
helm upgrade kausa oci://ghcr.io/fedeam/charts/kausa --version <version> -n kausa --reuse-values
```

Rehearsed on 2026-11-03 in kind with Helm 4.3.1, from the v0.5.1 chart URL to the same chart
pushed to a throwaway local registry: `helm get values` kept `rbac.scope` and `dryRun`, and `helm
history` showed revision 2. As of 2026-11-04 not yet done against GHCR, where the first OCI chart
is v0.6.0, released on 2026-11-04.

## Upgrade from a chart without rbac.scope

The charts of v0.2.0 and v0.1.0 have no `rbac.scope`, so the first upgrade to a chart that requires
it sets it once, `--reuse-values` included: `--reuse-values` reuses the values of the earlier
release, which have no `rbac` map, and the chart refuses to render without it (checked in kind on
2026-10-28 with Helm 3.16.1, from the v0.1.0 chart: the upgrade fails with `rbac.scope is not set`
and the release stays as it was; the v0.2.0 chart's values have no `rbac` map either).

```sh
helm upgrade kausa "$CHART" --version "$VERSION" -n kausa --reuse-values --set rbac.scope=cluster
```

`rbac.scope=cluster` keeps the ClusterRole, its rules and the whole-cluster watch. After that,
`--reuse-values` carries the scope on. Without `--reuse-values`, pass the scope every time, best
from a values file: `helm upgrade --reset-values`, or an upgrade with `--set` and neither
`--reuse-values` nor a values file, renders the chart's defaults, which have no scope, and fails.

## Upgrade to the Deployment watch

From v0.4.0, kausa also watches Deployments, to explain a rollout past its progress deadline
(`D014`), and its role gains `list` and `watch` on `deployments` in both scopes ([0018. Explain a
Deployment past its progress deadline (`D014`) from a Deployment watch that is on by default, and
add nothing for StatefulSets or DaemonSets](../adr/0018-stuck-rollouts.md)).

- **Upgrade the chart and the image together.** A v0.4.0 image with the v0.3 chart's role, RBAC made
  outside the chart, or the read-only proxy of [Try kausa against a cluster with a local
  model](try-with-a-local-model.md) without the Deployment paths, gets `403` on the Deployment
  watch: kausa explains everything else but is never Ready, `helm upgrade --wait` times out, and
  `deployment watch error` lines whose `error` says `deployments.apps is forbidden` appear: the
  first at once, then one every 5 minutes at most, with the repeats in between in `folded`. The
  chart's own upgrade needs nothing more: a missing `rollouts` value, as `--reuse-values` gives
  from a v0.3 release, means on.
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
  paced by the call budget: 3 at once, then one every `3600/budget.callsPerHour` seconds (120 at
  the default), while the others wait in the queue of 16 incidents (beyond it, the oldest is
  dropped and counted in `kausa_explanations_dropped_total`, or, while the queue waits for the
  budget, skipped with an `ExplanationSkipped` Event); a restart does it again, as for Pod
  failures. A stall already explained by a Pod rule or `D011` of the same Deployment
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
   helm install kausa "$CHART" --version "$VERSION" -n kausa -f kausa-values.yaml --set dryRun=true
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
  deletes the others, and kausa's Pod is replaced, so its deduplication cache starts over and its
  budget starts with 3 calls, then refills at the hourly rate. `helm uninstall` removes every Role and RoleBinding.
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
   helm upgrade kausa "$CHART" --version "$VERSION" -n kausa --reuse-values --set history.enabled=true
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
  helm template kausa "$CHART" --version "$VERSION" --kube-version 1.36.0 -f kausa-values.yaml \
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

## Run a standby replica

With `replicaCount: 2`, a second Pod waits as a standby and takes over when the one that acts
stops: within 40 seconds when its node dies or it freezes, and within about 5 seconds when
it is deleted or drained, then the new leader lists and its incidents settle ([Architecture](../explanation/architecture.md#failover)).
Without it, a lost node leaves kausa down until its Pod is replaced, about five minutes by default
(hypothesis: the default `tolerationSeconds` of 300). Status as of 2026-10-30: v0.4.0 is the
first release with it, and its takeovers were measured in kind on 2026-10-29 ([Standby and
failover](../research/2026-10-29-standby-and-failover.md); [0019. Offer an optional standby
replica that takes over through a chart-created Lease, elected by kausa's own
code](../adr/0019-high-availability.md)).

1. **Turn it on**, in either scope:

   ```sh
   helm upgrade kausa "$CHART" --version "$VERSION" -n kausa --reuse-values --set replicaCount=2 --wait
   ```

   The chart runs two Pods with `--leader-lease kausa/kausa-leader` and adds, in the release
   namespace, the Lease `kausa-leader`, a Role and RoleBinding of that name with `get` and `patch`
   on that one Lease ([Permissions](../reference/permissions.md#in-kausas-own-namespace-with-two-replicas)),
   a PodDisruptionBudget, and, while `affinity` is empty, a preferred anti-affinity that spreads
   the Pods over nodes when it can ([Helm chart](../reference/helm-chart.md#a-standby-replica)).
   `strategy` stays `Recreate`, so this upgrade, like every later one, stops both old Pods before
   the new ones start. `--wait` waits for both Pods to be Ready (checked with Helm 3.16.1).
   `replicaCount` takes `1` or `2` only; scale only through it, never with `kubectl scale`.

2. **See which one acts:**

   ```sh
   kubectl -n kausa get lease kausa-leader -o jsonpath='{.spec.holderIdentity}'
   ```

   It prints the name of the Pod that holds the Lease. Its log has a `leading` line; the other's
   has `standby`, and nothing else after `started`: no incident, no explanation. Both are Ready;
   `/readyz` answers `ready` on the leader and `standby` on the other. Use the holder's log:

   ```sh
   kubectl -n kausa logs "$(kubectl -n kausa get lease kausa-leader -o jsonpath='{.spec.holderIdentity}')"
   ```

3. **Scrape each Pod, not the Service.** The Service `kausa-metrics` selects both Pods, so a scrape
   through it reaches either one at random. Each Pod has its own counters, its own budget and its
   own `kausa_leader` gauge (`1` on the leader, `0` on the standby): scrape them as Pods, with a
   PodMonitor if you run the Prometheus Operator,

   ```yaml
   apiVersion: monitoring.coreos.com/v1
   kind: PodMonitor
   metadata:
     name: kausa
     namespace: kausa
   spec:
     selector:
       matchLabels:
         app.kubernetes.io/name: kausa
         app.kubernetes.io/instance: kausa
     podMetricsEndpoints:
       - port: metrics
   ```

   or with the scrape annotations your Prometheus reads on Pods, through `podAnnotations` (for
   example `prometheus.io/scrape: "true"` and `prometheus.io/port: "9090"`, if your scrape
   configuration uses them). With `networkPolicy.metricsFrom`, allow your Prometheus to reach both
   Pods.

4. **Alert** on nobody acting, or two, and on a Lease that changes hands too often, with
   [`KausaNoLeader`](../reference/alerts.md#kausanoleader) and
   [`KausaLeaderFlapping`](../reference/alerts.md#kausaleaderflapping) of
   [Alerts](../reference/alerts.md). Both add up a release's Pods by their `namespace` and `job`
   labels; adjust that to your scrape configuration. The first fires when the Lease is held by
   someone else, has been deleted, or cannot be renewed (an API server outage, a deleted
   `kausa-leader` RoleBinding): kausa then explains nothing, on purpose, rather than act without
   the Lease. The second shows a Lease that flips, by failures or by someone writing it.

What to expect:

- **Each replica keeps its own call budget,** so while both run they can spend up to twice
  `budget.callsPerHour` and `budget.callsPerDay`; set the provider's own spending cap accordingly.
  A takeover brings at most 3 calls at once, then the hourly rate, so a Lease that flips does not
  refill the budget.
- **A failover may explain an incident again.** The new leader does not have the old one's
  deduplication cache, so an incident that still fails is explained once more: one more call,
  Event (or series update), history entry and notification.
- **Whoever can write Leases in kausa's namespace can stop it** in both replicas (the default
  `edit` and `admin` roles can, as a review found in kind; [Security policy](../../SECURITY.md)). Keep kausa in a
  namespace of its own, administered by the people who administer kausa.
- **Going back to one replica** (`--set replicaCount=1`) removes the Lease, its Role and
  RoleBinding and the PodDisruptionBudget, and the remaining Pod runs without `--leader-lease`
  (Helm deletes what a release no longer renders; not checked in kind for these objects).

## Verify a release

Each release's notes, from v0.6.0, give these commands with the release's digests filled in: run
them from there. Below, `<version>` is the release without its `v`, and `<index>`, `<amd64>`,
`<arm64>` and `<chart-digest>` are the `sha256:…` digests the notes list for the image, the image
of each architecture and the chart. Verify by digest: a tag on GHCR can be moved. With cosign v3 or
later, and no login:

```sh
ID=https://github.com/fedeam/kausa/.github/workflows/release.yml@refs/tags/v<version>
ISSUER=https://token.actions.githubusercontent.com
cosign verify ghcr.io/fedeam/kausa@<index> --certificate-identity "$ID" --certificate-oidc-issuer "$ISSUER" > /dev/null
cosign verify-attestation --type https://slsa.dev/provenance/v1 ghcr.io/fedeam/kausa@<index> --certificate-identity "$ID" --certificate-oidc-issuer "$ISSUER" > /dev/null
cosign verify-attestation --type https://cyclonedx.org/bom ghcr.io/fedeam/kausa@<amd64> --certificate-identity "$ID" --certificate-oidc-issuer "$ISSUER" > /dev/null
cosign verify-attestation --type https://cyclonedx.org/bom ghcr.io/fedeam/kausa@<arm64> --certificate-identity "$ID" --certificate-oidc-issuer "$ISSUER" > /dev/null
cosign verify ghcr.io/fedeam/charts/kausa@<chart-digest> --certificate-identity "$ID" --certificate-oidc-issuer "$ISSUER" > /dev/null
cosign verify-attestation --type https://slsa.dev/provenance/v1 ghcr.io/fedeam/charts/kausa@<chart-digest> --certificate-identity "$ID" --certificate-oidc-issuer "$ISSUER" > /dev/null
```

To install exactly the chart you checked, compare it with the release's signed checksums (on
macOS, `shasum -a 256` in place of `sha256sum`):

```sh
curl -fsSLO https://github.com/fedeam/kausa/releases/download/v<version>/SHA256SUMS
curl -fsSLO https://github.com/fedeam/kausa/releases/download/v<version>/SHA256SUMS.sigstore.json
cosign verify-blob SHA256SUMS --bundle SHA256SUMS.sigstore.json --certificate-identity "$ID" --certificate-oidc-issuer "$ISSUER"
helm pull oci://ghcr.io/fedeam/charts/kausa --version <version>
sha256sum --ignore-missing -c SHA256SUMS
helm install kausa kausa-<version>.tgz -n kausa --create-namespace --set rbac.scope=cluster --set dryRun=true
```

The notes also verify the same attestations with `gh attestation verify`, which needs `gh auth
login`. The v0.5.2 release and earlier have no chart on GHCR and no SBOM: their notes verify the
image's signature and provenance and the chart's checksum. Why: [0023. Publish the chart as a signed OCI artifact at
`oci://ghcr.io/fedeam/charts/kausa`, list it on Artifact Hub, and attest a CycloneDX SBOM of an
auditable binary to each image](../adr/0023-oci-chart-and-sbom.md). v0.6.0, released on
2026-11-04, is the first release whose notes give them; every command of its notes passed on
2026-11-04 (the `cosign` block and the checksum block with no login, the `gh` block logged in).

## Verify

- `kubectl -n kausa get pods`: one Pod, ready once every watch has listed (three, or three per
  listed namespace; two, or two per listed namespace, with `rollouts=false`). With
  `replicaCount=2`, two Pods, both Ready, and the Lease `kausa-leader` names one of them.
- When a workload has been failing for a minute:
  `kubectl get events.events.k8s.io -A --field-selector reportingController=kausa` (or `-n
  <namespace>` for each listed namespace) lists kausa's Events, and `kubectl describe deployment -n <namespace> <name>` shows the explanation.
- With the history on: `kubectl get explanations -A` lists one object per explained incident.
- `helm test kausa -n kausa --logs` prints `"result":"ok"` (step 6).
- `kubectl get --raw /api/v1/namespaces/kausa/services/kausa-metrics:9090/proxy/metrics` shows
  the counters and the gauges; `kausa_endpoint_state{state="closed"} 1` while kausa calls its
  endpoint.
- To hear when kausa itself needs attention, load the alerting rules
  [`deploy/prometheus/kausa-alerts.yaml`](../../deploy/prometheus/kausa-alerts.yaml), which
  [Alerts](../reference/alerts.md) explains alert by alert. Among them:
  `KausaDailyCallBudgetSpent` fires when the acting replica has spent its daily call budget,
  `KausaExplanationsBehind` when incidents have waited for 45 minutes without the queue ever
  emptying, and `KausaModelEndpointFailing` when kausa has stopped calling its endpoint for 20
  minutes because calls keep failing (the breaker of [Command line and log
  format](../reference/command-line.md#the-breaker)); a passing `helm test` does not resolve the
  last one, since the test is another process.

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
  is forbidden`** → kausa's role does not grant `list` and `watch` on Deployments (a v0.4.0 image on
  the v0.3 chart's role, or RBAC made outside the chart) → upgrade the chart with the image, add
  the rule of [Upgrade to the Deployment watch](#upgrade-to-the-deployment-watch), or set
  `rollouts=false`.
- **With `replicaCount=2`, both Pods log `standby` and `sum(kausa_leader)` is 0** → nobody holds
  the Lease → a `lease error` line names the call and, on a `403`, has the hint `kausa needs get
  and patch on this Lease`: the `kausa-leader` Role or RoleBinding was deleted or changed; on a
  `404`, `the chart creates this Lease: run helm upgrade`: the Lease was deleted. `helm upgrade`
  with the same values puts all three back. Without a `lease error`, someone else holds the Lease:
  `kubectl -n kausa get lease kausa-leader -o yaml` shows who, and whoever can write Leases in the
  namespace can take it.
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
- **`endpoint unavailable`** in the log, `kausa_endpoint_state{state="open"}` at 1, or incidents
  skipped as `endpoint_unavailable` → kausa stopped calling a failing endpoint and probes it once
  per hold, up to every 30 minutes; queued incidents wait → run `helm test kausa -n kausa --logs`
  and act on its `cause`, below. A fixed key needs a restart of kausa (`kubectl -n kausa rollout
  restart deploy/kausa`): it reads the key once, at start. Credit added at the provider needs
  nothing: the next probe closes the breaker and the waiting incidents are explained.

`helm test` fails with one `endpoint check` line; by its `cause` ([Causes](../reference/command-line.md#causes)):

- **`key`** (`401`, `403`) → the endpoint refused the key → check the Secret of
  `model.apiKey.existingSecret` and its `model.apiKey.key`, and the key's access to the model.
- **`not_found`** (`404`, `405`, `410`) → a wrong model name, a model no longer offered, or a
  wrong URL → check `model.name`, and that `model.endpoint` is the base URL (often ending in
  `/v1`; kausa appends `/chat/completions`).
- **`quota`** (`402`, or `429` with `insufficient_quota`) → the account has no credit left → add
  credit, or use another key.
- **`redirect`** (`3xx`) → the URL redirects, and kausa follows no redirect → put the final URL in
  `model.endpoint`.
- **`not_an_answer`** → a `2xx` that is not a chat completion (not JSON, or no `choices`) → the URL
  serves something else: put the API's base URL in `model.endpoint`.
- **`no_content`** → the model answered without text; `reason` says whether it refused, a filter
  removed the text or it ran out of output tokens (kausa asks for 800) → try another model, or one
  that does not reason at length.
- **`invalid_answer`** → the model answered, but not with kausa's JSON → try a larger model, or
  `model.jsonSchema=false` if the server ignores `response_format`.
- **`timeout`** → no answer within `model.timeoutSeconds` → the model may be slow on this hardware
  or overloaded: raise the value (and `helm test --timeout` above 240), or try later.
- **`connection`** → kausa could not connect: DNS, TLS or a refused connection → check
  `model.endpoint`, the certificate, and with the NetworkPolicy, `networkPolicy.endpoint.to` and
  `.ports`.
- **`rate_limited`** (`429`) or **`server_error`** (`5xx`) → the provider is busy or failing →
  try later; kausa waits by itself (`retry_after_seconds` shows the provider's `Retry-After`).
- **`rejected`** (another `4xx`) → the endpoint refused the request → read `detail`; if the server
  does not support structured output, set `model.jsonSchema=false`.
- **`too_large`** (`413`) → the model refuses a request of this size: unexpected for the check's
  small request; for incidents, lower `budget.maxInputTokens` or use a model with a larger
  context.
- **`helm test` itself times out** → the Pod did not finish within 5 minutes (an image that cannot
  be pulled, or `model.timeoutSeconds` above 240) → `kubectl -n kausa describe pod
  kausa-endpoint-check`, and pass a longer `--timeout`.

## Uninstall

```sh
helm uninstall kausa -n kausa
kubectl delete namespace kausa
```

kausa's Events expire with the API server's event TTL (one hour by default). With the history on
and `history.installCRD=true`, `helm uninstall` also deletes the CustomResourceDefinition and every
Explanation in the cluster; with `history.installCRD=false`, the administrator who installed it
deletes it (`kubectl delete crd explanations.kausa.fedeam.com`), which deletes every Explanation.
With `replicaCount=2`, `helm uninstall` also deletes the Lease, its Role and RoleBinding and the
PodDisruptionBudget. `helm uninstall` leaves the finished `helm test` Pod (seen in kind on
2026-11-02); deleting the namespace deletes it, or, to keep the namespace, `kubectl -n kausa
delete pod kausa-endpoint-check`.
