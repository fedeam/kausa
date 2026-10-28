# Install kausa with Helm

Install the released chart, which runs the published image, first in dry-run and then pointed at
a model.

Status as of 2026-10-28: the v0.1.0 chart from the GitHub release was installed in dry-run in kind
(Kubernetes v1.37) with the published image, on 2026-10-28. The chart in the repository was
checked with a server-side dry run against Kubernetes v1.36, installed in dry-run on a Raspberry
Pi 4 node, and installed with an endpoint by the kind test (`tests/e2e/run.sh`, passed on
2026-10-27).

## Before you start

- A cluster where you may create a namespace and a ClusterRole, and `kubectl` and Helm (CI uses
  4.3.1; the chart was also rendered with 3.16) pointed at it: check
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
   helm install kausa "$CHART" -n kausa --set dryRun=true
   kubectl -n kausa logs deploy/kausa -f
   ```

   Each `request` line is the redacted body that would leave the cluster. In dry-run the chart
   grants `pods/log` but no Event writes.

4. **Point it at the model:**

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
helm install kausa charts/kausa -n kausa \
  --set image.repository=<registry>/kausa --set image.tag=<tag> --set dryRun=true
```

## Verify

- `kubectl -n kausa get pods`: one Pod, ready once both watches have listed.
- When a workload has been failing for a minute:
  `kubectl get events.events.k8s.io -A --field-selector reportingController=kausa` lists kausa's
  Events, and `kubectl describe deployment -n <namespace> <name>` shows the explanation.
- `kubectl get --raw /api/v1/namespaces/kausa/services/kausa-metrics:9090/proxy/metrics` shows
  the counters.

## If something fails

- **`ErrImagePull`** → the cluster cannot pull the image → check that the nodes reach `ghcr.io`,
  or, for your own build, the registry, the tag and `imagePullSecrets`.
- **`helm install` fails with `model.name is required`** (or another of the chart's checks) → set
  the value it names ([Helm chart](../reference/helm-chart.md)).
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
