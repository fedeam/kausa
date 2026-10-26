#!/usr/bin/env bash
# End-to-end test: kausa installed with its Helm chart in a kind cluster, against a fake model
# endpoint, with two seeded failures. It checks the Events, what left the cluster, the ClusterRole
# and the metrics. Needs docker, kind, kubectl, helm and python3; KIND_NODE_IMAGE names the node
# image (pinned in .github/workflows/e2e.yml). It creates the cluster kausa-e2e and deletes it at
# the end. See docs/how-to/develop.md.
set -euo pipefail

CLUSTER=kausa-e2e
NAMESPACE=kausa
TARGET=e2e-shop
PORT=18080
WORK=$(mktemp -d)
CONTEXT="kind-$CLUSTER"
k() { kubectl --context "$CONTEXT" "$@"; }
fail() { echo "FAIL: $*" >&2; exit 1; }

cleanup() {
  status=$?
  if [ "$status" -ne 0 ]; then
    echo "--- kausa's log" >&2
    k -n "$NAMESPACE" logs deploy/kausa --tail=200 >&2 || true
    echo "--- Events in $TARGET" >&2
    k -n "$TARGET" get events.events.k8s.io -o wide >&2 || true
  fi
  [ -n "${ENDPOINT_PID:-}" ] && kill "$ENDPOINT_PID" 2>/dev/null || true
  kind delete cluster --name "$CLUSTER" >/dev/null 2>&1 || true
  rm -rf "$WORK"
  exit "$status"
}
trap cleanup EXIT

: "${KIND_NODE_IMAGE:?set KIND_NODE_IMAGE to a pinned kindest/node image}"
kind create cluster --name "$CLUSTER" --image "$KIND_NODE_IMAGE" --wait 120s
docker build --target runtime -t kausa:e2e .
kind load docker-image kausa:e2e --name "$CLUSTER"

# The fake endpoint runs on the host. On Linux the cluster reaches it through the gateway of the
# kind network; on Docker Desktop that gateway belongs to Docker's VM, so the address the nodes
# give host.docker.internal is used instead (a hypothesis: only the Linux path has run).
if [ "$(docker info --format '{{.OperatingSystem}}')" = "Docker Desktop" ]; then
  gateway=$(docker exec "$CLUSTER-control-plane" getent ahostsv4 host.docker.internal |
    awk 'NR == 1 { print $1 }')
else
  gateway=$(docker network inspect kind -f '{{range .IPAM.Config}}{{.Gateway}} {{end}}' |
    tr ' ' '\n' | grep -E '^[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+$' | head -n 1)
fi
[ -n "$gateway" ] || fail "no IPv4 address of the host for the cluster"
python3 tests/e2e/fake_endpoint.py --port "$PORT" --log "$WORK/requests.jsonl" &
ENDPOINT_PID=$!

# The restricted Pod Security Standard: the chart's Pod must be admitted under it.
k create namespace "$NAMESPACE"
k label namespace "$NAMESPACE" pod-security.kubernetes.io/enforce=restricted
helm --kube-context "$CONTEXT" install kausa charts/kausa -n "$NAMESPACE" --wait --timeout 120s \
  --set image.repository=kausa --set image.tag=e2e --set image.pullPolicy=Never \
  --set model.endpoint="http://$gateway:$PORT/v1" --set model.name=fake \
  --set model.allowHttp=true --set settleSeconds=10

k apply -f tests/e2e/failures.yaml

# Wait for an Explained Event about each failing workload.
explained() {
  k -n "$TARGET" get events.events.k8s.io -o json | python3 -c '
import json, sys
kind, name = sys.argv[1], sys.argv[2]
items = json.load(sys.stdin)["items"]
ok = [e for e in items if e.get("reportingController") == "kausa" and e.get("reason") == "Explained"
      and e["regarding"]["kind"] == kind and e["regarding"]["name"] == name]
if ok:
    print(ok[0]["note"])
' "$1" "$2"
}
for _ in $(seq 1 60); do
  deployment_note=$(explained Deployment crasher)
  job_note=$(explained Job failing-job)
  [ -n "$deployment_note" ] && [ -n "$job_note" ] && break
  sleep 5
done
[ -n "$deployment_note" ] || fail "no Explained Event about Deployment crasher"
[ -n "$job_note" ] || fail "no Explained Event about Job failing-job"
echo "Deployment crasher: $deployment_note"
echo "Job failing-job: $job_note"

# The model named namespace-1; the Event names the real namespace.
case "$deployment_note" in *"$TARGET"*) ;; *) fail "the placeholder was not mapped back" ;; esac

# What left the cluster: requests were sent, and none holds a real name.
[ -s "$WORK/requests.jsonl" ] || fail "the fake endpoint received no request"
for name in "$TARGET" crasher failing-job; do
  if grep -q -- "$name" "$WORK/requests.jsonl"; then fail "a request contains '$name'"; fi
done

# The ClusterRole is the table in docs/reference/permissions.md.
sa="system:serviceaccount:$NAMESPACE:kausa"
allowed() { [ "$(k auth can-i --as="$sa" "$@" 2>/dev/null)" = yes ]; }
for check in "list pods" "watch pods" "get pods --subresource=log" "list events.events.k8s.io" \
  "watch events.events.k8s.io" "create events.events.k8s.io" "patch events.events.k8s.io" \
  "get replicasets.apps" "get jobs.batch"; do
  # shellcheck disable=SC2086
  allowed $check -A || fail "kausa cannot $check"
done
for check in "get secrets" "list secrets" "get configmaps" "list configmaps" "delete pods" \
  "create pods" "create pods --subresource=exec" "update events.events.k8s.io" "delete events.events.k8s.io" \
  "list nodes" "create serviceaccounts --subresource=token" "impersonate users" "list replicasets.apps" \
  "list jobs.batch"; do
  # shellcheck disable=SC2086
  if allowed $check -A; then fail "kausa can $check"; fi
done

# The metrics, through the API server's service proxy.
metrics=$(k get --raw "/api/v1/namespaces/$NAMESPACE/services/kausa-metrics:9090/proxy/metrics")
echo "$metrics" | grep -E '^kausa_events_written_total\{reason="Explained"\} [1-9]' >/dev/null ||
  fail "kausa_events_written_total does not count the Events"
restarts=$(k -n "$NAMESPACE" get pods -l app.kubernetes.io/name=kausa \
  -o jsonpath='{.items[0].status.containerStatuses[0].restartCount}')
[ "$restarts" = 0 ] || fail "kausa restarted $restarts times"

echo "e2e: ok"
