#!/usr/bin/env bash
# End-to-end test: kausa installed with its Helm chart in a kind cluster, against a fake model
# endpoint, with two seeded failures. It checks the Events, what left the cluster, the RBAC
# objects and what they grant, and the metrics. Needs docker, kind, kubectl, helm and python3;
# KIND_NODE_IMAGE names the node image (pinned in .github/workflows/e2e.yml). It creates the
# cluster kausa-e2e and deletes it at the end. See docs/how-to/develop.md.
#
# KAUSA_E2E_SCOPE picks the chart's rbac.scope (docs/adr/0013-namespaced-install-mode.md):
# - cluster (when unset): a ClusterRole; then an Event whose regarding object is in another
#   namespace, which kausa must ignore, and a helm upgrade to the namespaced scope;
# - namespaced: Roles in e2e-shop and e2e-denied only. kausa explains e2e-shop and not the same
#   failures in e2e-other, is granted nothing outside the list, keeps its Roles when an upgrade
#   without rbac.scope fails, stays not Ready (without crashing) when one namespace refuses it,
#   and leaves no Role behind after helm uninstall.
set -euo pipefail

fail() { echo "FAIL: $*" >&2; exit 1; }

SCOPE=${KAUSA_E2E_SCOPE:-cluster}
case "$SCOPE" in
  cluster | namespaced) ;;
  *) fail "KAUSA_E2E_SCOPE must be cluster or namespaced" ;;
esac

CLUSTER=kausa-e2e
NAMESPACE=kausa
RELEASE=kausa
TARGET=e2e-shop
# Namespaced scope: the failures are seeded in OTHER too, which is not listed; DENIED is listed
# and later loses its RoleBinding.
OTHER=e2e-other
DENIED=e2e-denied
# Cluster scope: the namespace an Event in TARGET claims to be about.
ELSEWHERE=e2e-elsewhere
SETTLE=10
# run.rs's TICK, in seconds: a settled incident is logged on the next tick.
TICK=5
WORK=$(mktemp -d)
CONTEXT="kind-$CLUSTER"
k() { kubectl --context "$CONTEXT" "$@"; }
h() { helm --kube-context "$CONTEXT" "$@"; }

cleanup() {
  status=$?
  if [ "$status" -ne 0 ]; then
    echo "--- kausa's log" >&2
    k -n "$NAMESPACE" logs deploy/kausa --tail=200 >&2 || true
    echo "--- Events in $TARGET" >&2
    k -n "$TARGET" get events.events.k8s.io -o wide >&2 || true
    if [ "$SCOPE" = namespaced ]; then
      echo "--- Events in $OTHER" >&2
      k -n "$OTHER" get events.events.k8s.io -o wide >&2 || true
    fi
  fi
  [ -n "${ENDPOINT_PID:-}" ] && kill "$ENDPOINT_PID" 2>/dev/null || true
  kind delete cluster --name "$CLUSTER" >/dev/null 2>&1 || true
  rm -rf "$WORK"
  exit "$status"
}
trap cleanup EXIT

: "${KIND_NODE_IMAGE:?set KIND_NODE_IMAGE to a pinned kindest/node image}"
echo "e2e: rbac.scope=$SCOPE"
kind create cluster --name "$CLUSTER" --image "$KIND_NODE_IMAGE" --wait 120s
docker build --target runtime -t kausa:e2e .
kind load docker-image kausa:e2e --name "$CLUSTER"

# The fake endpoint runs on the host. On Linux the cluster reaches it through the gateway of the
# kind network; on Docker Desktop that gateway belongs to Docker's VM, so the address the nodes
# give host.docker.internal is used instead (passed on Docker Desktop on 2026-10-27).
if [ "$(docker info --format '{{.OperatingSystem}}')" = "Docker Desktop" ]; then
  gateway=$(docker exec "$CLUSTER-control-plane" getent ahostsv4 host.docker.internal |
    awk 'NR == 1 { print $1 }')
else
  gateway=$(docker network inspect kind -f '{{range .IPAM.Config}}{{.Gateway}} {{end}}' |
    tr ' ' '\n' | grep -E '^[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+$' | head -n 1)
fi
[ -n "$gateway" ] || fail "no IPv4 address of the host for the cluster"
# A free port, and the endpoint answering on it before anything depends on it: a port already
# in use would otherwise leave another program to answer kausa.
PORT=$(python3 -c 'import socket; s = socket.socket(); s.bind(("", 0)); print(s.getsockname()[1])')
python3 tests/e2e/fake_endpoint.py --port "$PORT" --log "$WORK/requests.jsonl" &
ENDPOINT_PID=$!
for _ in $(seq 1 20); do
  curl -s -o /dev/null -X POST "http://127.0.0.1:$PORT/v1/chat/completions" && break
  kill -0 "$ENDPOINT_PID" 2>/dev/null || fail "the fake endpoint did not start"
  sleep 0.5
done
curl -s -o /dev/null -X POST "http://127.0.0.1:$PORT/v1/chat/completions" ||
  fail "the fake endpoint does not answer on port $PORT"
: > "$WORK/requests.jsonl"

# The install's values other than rbac, reused by the upgrade that must fail.
values=(--set image.repository=kausa --set image.tag=e2e --set image.pullPolicy=Never
  --set model.endpoint="http://$gateway:$PORT/v1" --set model.name=fake
  --set model.allowHttp=true --set settleSeconds="$SETTLE")

# The restricted Pod Security Standard: the chart's Pod must be admitted under it.
k create namespace "$NAMESPACE"
k label namespace "$NAMESPACE" pod-security.kubernetes.io/enforce=restricted
if [ "$SCOPE" = namespaced ]; then
  # The chart creates no Namespace: each listed one must exist before helm install.
  for ns in "$TARGET" "$OTHER" "$DENIED"; do
    k create namespace "$ns" --save-config
  done
  h install "$RELEASE" charts/kausa -n "$NAMESPACE" --wait --timeout 120s "${values[@]}" \
    --set rbac.scope=namespaced --set "rbac.namespaces={$TARGET,$DENIED}"
else
  h install "$RELEASE" charts/kausa -n "$NAMESPACE" --wait --timeout 120s "${values[@]}" \
    --set rbac.scope=cluster
fi

k apply -f tests/e2e/failures.yaml
if [ "$SCOPE" = namespaced ]; then
  # The same failures where kausa is granted nothing.
  sed "s/$TARGET/$OTHER/g" tests/e2e/failures.yaml | k apply -f -
fi

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
# The number of Events kausa wrote in namespace $1 (all of them, with -A), about an object named
# $2 if given.
kausa_events() {
  if [ "$1" = -A ]; then where=(-A); else where=(-n "$1"); fi
  k get events.events.k8s.io "${where[@]}" -o json | python3 -c '
import json, sys
name = sys.argv[1]
items = json.load(sys.stdin)["items"]
print(sum(1 for e in items if e.get("reportingController") == "kausa"
          and (not name or (e.get("regarding") or {}).get("name") == name)))
' "${2:-}"
}
# Whether kausa's log, on stdin, has a line whose msg is one of $1 (separated by "|") and whose
# namespace field is $2; with $3 = anywhere, a line that names $2 in any field.
log_has() {
  python3 -c '
import json, sys
msgs, namespace = sys.argv[1].split("|"), sys.argv[2]
anywhere = len(sys.argv) > 3 and sys.argv[3] == "anywhere"
for line in sys.stdin:
    try:
        entry = json.loads(line)
    except ValueError:
        continue
    if not isinstance(entry, dict) or entry.get("msg") not in msgs:
        continue
    if entry.get("namespace") == namespace or (anywhere and namespace in json.dumps(entry)):
        sys.exit(0)
sys.exit(1)
' "$@"
}
# kausa's log, into a file first, so that a failed read is a failure and not an empty log.
kausa_log() {
  k -n "$NAMESPACE" logs "${1:-deploy/kausa}" > "$WORK/kausa.log"
}
# Nothing cluster-scoped with the release's label: a label selector, so an empty result is no
# error.
no_cluster_rbac() {
  found=$(k get clusterroles,clusterrolebindings -l "app.kubernetes.io/instance=$RELEASE" -o name)
  [ -z "$found" ] || fail "cluster-scoped RBAC objects remain: $found"
}
# The Role and the RoleBinding named after the release exist in namespace $1.
has_roles() {
  k -n "$1" get "role/$RELEASE" "rolebinding/$RELEASE" -o name >/dev/null ||
    fail "no Role and RoleBinding $RELEASE in $1"
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

if [ "$SCOPE" = namespaced ]; then
  # The failures in OTHER were seeded with TARGET's: one more settle window and tick, and kausa
  # still has neither explained nor logged them.
  sleep $((SETTLE + TICK + 5))
  [ "$(kausa_events "$OTHER")" = 0 ] || fail "kausa wrote an Event in $OTHER"
  kausa_log
  if log_has incident "$OTHER" < "$WORK/kausa.log"; then
    fail "kausa logged an incident in $OTHER"
  fi
  echo "$OTHER: no Event and no incident"
fi

# What left the cluster: requests were sent, and none holds a real name.
[ -s "$WORK/requests.jsonl" ] || fail "the fake endpoint received no request"
for name in "$TARGET" "$OTHER" crasher failing-job; do
  if grep -q -- "$name" "$WORK/requests.jsonl"; then fail "a request contains '$name'"; fi
done

# The grant is the table in docs/reference/permissions.md.
sa="system:serviceaccount:$NAMESPACE:kausa"
allowed() { [ "$(k auth can-i --as="$sa" "$@" 2>/dev/null)" = yes ]; }
granted=("list pods" "watch pods" "get pods --subresource=log" "list events.events.k8s.io"
  "watch events.events.k8s.io" "create events.events.k8s.io" "patch events.events.k8s.io"
  "get replicasets.apps" "get jobs.batch")
refused=("get secrets" "list secrets" "get configmaps" "list configmaps" "delete pods"
  "create pods" "create pods --subresource=exec" "update events.events.k8s.io"
  "delete events.events.k8s.io" "list nodes" "create serviceaccounts --subresource=token"
  "impersonate users" "list replicasets.apps" "list jobs.batch")
if [ "$SCOPE" = namespaced ]; then
  for check in "${granted[@]}"; do
    for where in "-n $TARGET" "-n $DENIED"; do
      # shellcheck disable=SC2086
      allowed $check $where || fail "kausa cannot $check $where"
    done
    for where in "-n $OTHER" "-n $NAMESPACE" -A; do
      # shellcheck disable=SC2086
      if allowed $check $where; then fail "kausa can $check $where"; fi
    done
  done
  for check in "${refused[@]}"; do
    # shellcheck disable=SC2086
    if allowed $check -n "$TARGET"; then fail "kausa can $check -n $TARGET"; fi
  done
  no_cluster_rbac
else
  for check in "${granted[@]}"; do
    # shellcheck disable=SC2086
    allowed $check -A || fail "kausa cannot $check"
  done
  for check in "${refused[@]}"; do
    # shellcheck disable=SC2086
    if allowed $check -A; then fail "kausa can $check"; fi
  done
fi

# The metrics, through the API server's service proxy, which reaches only a Ready Pod.
metrics=$(k get --raw "/api/v1/namespaces/$NAMESPACE/services/kausa-metrics:9090/proxy/metrics")
echo "$metrics" | grep -E '^kausa_events_written_total\{reason="Explained"\} [1-9]' >/dev/null ||
  fail "kausa_events_written_total does not count the Events"
restarts=$(k -n "$NAMESPACE" get pods -l app.kubernetes.io/name=kausa \
  -o jsonpath='{.items[0].status.containerStatuses[0].restartCount}')
[ "$restarts" = 0 ] || fail "kausa restarted $restarts times"

if [ "$SCOPE" = namespaced ]; then
  # An upgrade that resets the values drops rbac.scope: it fails before it changes anything, so
  # the Roles stay and nothing cluster-scoped appears.
  if out=$(h upgrade "$RELEASE" charts/kausa -n "$NAMESPACE" --reset-values "${values[@]}" 2>&1)
  then
    fail "helm upgrade --reset-values without rbac.scope succeeded"
  fi
  case "$out" in
    *"rbac.scope is not set"*) ;;
    *) fail "helm upgrade --reset-values failed for another reason: $out" ;;
  esac
  has_roles "$TARGET"
  has_roles "$DENIED"
  no_cluster_rbac
  echo "helm upgrade --reset-values: refused, the Roles stay"

  # DENIED refuses kausa from now on. A restart makes kausa list it again, where an open watch
  # could otherwise run until its timeout; Recreate stops the old Pod first.
  old=$(k -n "$NAMESPACE" get pods -l app.kubernetes.io/name=kausa -o name)
  k -n "$DENIED" delete "rolebinding/$RELEASE"
  k -n "$NAMESPACE" rollout restart deploy/kausa
  pod=""
  for _ in $(seq 1 60); do
    pod=$(k -n "$NAMESPACE" get pods -l app.kubernetes.io/name=kausa \
      --field-selector=status.phase=Running -o name | grep -vxF -- "$old" | head -n 1 || true)
    [ -n "$pod" ] && break
    sleep 2
  done
  [ -n "$pod" ] || fail "no new kausa Pod is Running after the restart"
  # The watch errors come at once; an incident in TARGET after a settle window and a tick. The
  # wait also gives the readiness probe (every 10 seconds) several tries.
  started=$SECONDS
  denied_logged="" target_logged=""
  for _ in $(seq 1 60); do
    kausa_log "$pod"
    log_has "pod watch error|event watch error" "$DENIED" < "$WORK/kausa.log" && denied_logged=yes
    log_has incident "$TARGET" < "$WORK/kausa.log" && target_logged=yes
    [ -n "$denied_logged" ] && [ -n "$target_logged" ] && [ $((SECONDS - started)) -ge 30 ] && break
    sleep 2
  done
  [ -n "$denied_logged" ] || fail "no watch error with namespace $DENIED in the new Pod's log"
  [ -n "$target_logged" ] || fail "no incident with namespace $TARGET in the new Pod's log"
  # Read from the Pod: a Pod that is not Ready is no endpoint of the metrics Service.
  ready=$(k -n "$NAMESPACE" get "$pod" \
    -o jsonpath='{.status.conditions[?(@.type=="Ready")].status}')
  [ "$ready" = False ] || fail "the new kausa Pod is Ready ($ready) though $DENIED refuses it"
  restarts=$(k -n "$NAMESPACE" get "$pod" -o jsonpath='{.status.containerStatuses[0].restartCount}')
  [ "$restarts" = 0 ] || fail "the new kausa Pod restarted $restarts times"
  echo "$DENIED refused: kausa is not Ready, did not restart and still explains $TARGET"

  h uninstall "$RELEASE" -n "$NAMESPACE"
  left=""
  for _ in $(seq 1 15); do
    left=$(k get roles,rolebindings -A -l "app.kubernetes.io/instance=$RELEASE" -o name)
    [ -z "$left" ] && break
    sleep 2
  done
  [ -z "$left" ] || fail "helm uninstall left: $left"
  no_cluster_rbac
else
  # An Event in TARGET about a Job in ELSEWHERE, an empty namespace: kausa must not take TARGET's
  # word for another namespace (docs/adr/0013-namespaced-install-mode.md).
  k create namespace "$ELSEWHERE"
  # eventTime is a MicroTime: microseconds, in UTC.
  now=$(python3 -c 'import datetime as d
print(d.datetime.now(d.timezone.utc).strftime("%Y-%m-%dT%H:%M:%S.%fZ"))')
  crossed_event() {
    cat <<EOF
apiVersion: events.k8s.io/v1
kind: Event
metadata:
  name: crossed-namespace
  namespace: $TARGET
type: Warning
reason: BackoffLimitExceeded
note: Job has reached the specified backoff limit
action: Failing
eventTime: "$now"
reportingController: e2e-test
reportingInstance: e2e-test-1
regarding:
  apiVersion: batch/v1
  kind: Job
  name: elsewhere-job
  namespace: $1
EOF
  }
  # The same Event about TARGET passes a server-side dry run, so a refusal below is about the
  # namespace and not about the manifest.
  crossed_event "$TARGET" | k create --dry-run=server -f - >/dev/null
  if out=$(crossed_event "$ELSEWHERE" | k create -f - 2>&1); then
    sleep $((SETTLE + TICK + 5))
    kausa_log
    if log_has incident "$ELSEWHERE" anywhere < "$WORK/kausa.log"; then
      fail "kausa logged an incident naming $ELSEWHERE"
    fi
    [ "$(kausa_events "$ELSEWHERE")" = 0 ] || fail "kausa wrote an Event in $ELSEWHERE"
    [ "$(kausa_events -A elsewhere-job)" = 0 ] || fail "kausa wrote an Event about elsewhere-job"
    echo "the API server accepted the cross-namespace Event; kausa ignored it"
  else
    echo "$out"
    echo "the API server refused the cross-namespace Event"
  fi

  # From cluster to namespaced scope in place: the ClusterRole and its binding go, a Role and a
  # RoleBinding come.
  h upgrade "$RELEASE" charts/kausa -n "$NAMESPACE" --wait --timeout 120s --reuse-values \
    --set rbac.scope=namespaced --set "rbac.namespaces={$TARGET}"
  no_cluster_rbac
  has_roles "$TARGET"
  echo "helm upgrade to rbac.scope=namespaced: no cluster-scoped RBAC object, a Role in $TARGET"
fi

echo "e2e: ok"
