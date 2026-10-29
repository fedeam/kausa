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
#
# Both install with history.enabled=true (docs/adr/0014-explanation-history.md) and check each
# failing workload's Explanation, its columns, what kausa and the view and edit roles may do with
# it, the CustomResourceDefinition's refusals, the sweep after a restart, and kausa's own update of
# crasher's Explanation after it (a merge patch with fieldValidation=Strict). The cluster scope
# then turns the history off through dryRun=true and history.enabled=false, and ends with a deleted
# CustomResourceDefinition, which kausa must survive.
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
# The history's resource, its CustomResourceDefinition and its reader role.
EXPLANATIONS=explanations.kausa.fedeam.com
CRD=$EXPLANATIONS
READER=$RELEASE-explanation-reader
# Whether the release writes history, which decides what may be cluster-scoped (no_cluster_rbac).
HISTORY=on
# src/history/pure.rs: the error of a list answered 404.
CRD_MISSING="the CustomResourceDefinition $CRD is not installed (chart value history.installCRD)"
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
    echo "--- Explanations" >&2
    k get "$EXPLANATIONS" -A -o wide >&2 || true
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
  --set model.allowHttp=true --set settleSeconds="$SETTLE" --set history.enabled=true)

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
# error. While the history is on, the reader role and the CustomResourceDefinition are the
# exception, and they must exist; with it off, neither may.
no_cluster_rbac() {
  found=$(k get clusterroles,clusterrolebindings -l "app.kubernetes.io/instance=$RELEASE" -o name)
  crd=$(k get crd "$CRD" --ignore-not-found -o name)
  if [ "$HISTORY" = on ]; then
    reader="clusterrole.rbac.authorization.k8s.io/$READER"
    printf '%s\n' "$found" | grep -qxF -- "$reader" || fail "no ClusterRole $READER with the history"
    found=$(printf '%s\n' "$found" | grep -vxF -- "$reader" || true)
    [ -n "$crd" ] || fail "no CustomResourceDefinition $CRD with the history"
  else
    [ -z "$crd" ] || fail "the CustomResourceDefinition $CRD remains without the history"
  fi
  [ -z "$found" ] || fail "cluster-scoped RBAC objects remain: $found"
}
# Waits until the CustomResourceDefinition is gone: the API server deletes its Explanations first.
crd_gone() {
  for _ in $(seq 1 60); do
    [ -z "$(k get crd "$CRD" --ignore-not-found -o name)" ] && return 0
    sleep 2
  done
  fail "the CustomResourceDefinition $CRD is still there"
}
# What the rules of an RBAC object ($@, as kubectl get takes it) say about Explanations: "yes" when
# the one rule about them is the history's and comes last, "no" when there is none.
history_rule() {
  k get "$@" -o json | python3 -c '
import json, sys
rules = json.load(sys.stdin).get("rules") or []
want = {"apiGroups": ["kausa.fedeam.com"], "resources": ["explanations"],
        "verbs": ["list", "create", "patch", "delete"]}
about = [r for r in rules if "kausa.fedeam.com" in (r.get("apiGroups") or [])]
if not about:
    print("no")
elif about == [want] and rules[-1] == want:
    print("yes")
else:
    print("other rules: " + json.dumps(about))
'
}
# The Explanations in namespace $1 (all of them, with -A), one namespace/name per line.
explanations() {
  if [ "$1" = -A ]; then where=(-A); else where=(-n "$1"); fi
  k get "$EXPLANATIONS" "${where[@]}" \
    -o jsonpath='{range .items[*]}{.metadata.namespace}/{.metadata.name}{"\n"}{end}'
}
# A hand-made Explanation in TARGET, as JSON: name $1, kausa's label if $2 is yes, a summary of
# $3 copies of "é" (two bytes each), and $4 as every time. Its spec is what kausa would write.
hand_made() {
  python3 -c '
import json, sys
name, labelled, length, time, namespace = sys.argv[1:6]
metadata = {"name": name, "namespace": namespace}
if labelled == "yes":
    metadata["labels"] = {"app.kubernetes.io/managed-by": "kausa"}
entry = {"summary": "é" * int(length), "cause": "e2e", "steps": [], "confidence": "low",
         "model": "e2e", "promptVersion": "1", "firstTime": time, "lastTime": time, "count": 1}
print(json.dumps({
    "apiVersion": "kausa.fedeam.com/v1alpha1", "kind": "Explanation", "metadata": metadata,
    "spec": {"rule": "D001",
             "target": {"apiVersion": "apps/v1", "kind": "Deployment", "name": "hand-made"}},
    "status": {"occurrences": 1, "lastTime": time, "entries": [entry]},
}))
' "$1" "$2" "$3" "$4" "$TARGET"
}
# The time now, as the Explanation stores it.
utc_now() { date -u +%Y-%m-%dT%H:%M:%SZ; }
# Restarts kausa and waits until the rollout is done (the new Pod is Ready); sets pod.
restart_kausa() {
  old=$(k -n "$NAMESPACE" get pods -l app.kubernetes.io/name=kausa -o name)
  k -n "$NAMESPACE" rollout restart deploy/kausa
  k -n "$NAMESPACE" rollout status deploy/kausa --timeout 120s
  pod=$(k -n "$NAMESPACE" get pods -l app.kubernetes.io/name=kausa -o name |
    grep -vxF -- "$old" | head -n 1 || true)
  [ -n "$pod" ] || fail "no new kausa Pod after the restart"
}
# Checks the Explanation of each Explained Event about $1 (kind) $2 (name) in TARGET, with the
# events, the Explanations and kausa's log in WORK: named kausa.<fingerprint> after the Event, its
# spec {$3, $4 $1 $2} cross-checked with the incident kausa logged under that fingerprint, its
# newest entry the fake endpoint's answer with the namespace mapped back, starting the Event's
# note, and every field kausa writes read back (none pruned). Prints the Explanations' names.
check_explanations() {
  python3 -c '
import json, re, sys
sys.path.insert(0, "tests/e2e")
from fake_endpoint import ANSWER
kind, name, rule, api_version, target, work = sys.argv[1:7]

def fail(message):
    sys.exit("FAIL: the Explanation of %s %s: %s" % (kind, name, message))

def number(value, least):
    return isinstance(value, int) and not isinstance(value, bool) and value >= least

TIME = re.compile(r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z")
FIELDS = ["summary", "cause", "steps", "confidence", "model", "promptVersion", "firstTime",
          "lastTime", "count"]
events = json.load(open(work + "/events.json"))["items"]
objects = {o["metadata"]["name"]: o for o in json.load(open(work + "/explanations.json"))["items"]}
incidents = {}
for line in open(work + "/kausa.log"):
    try:
        entry = json.loads(line)
    except ValueError:
        continue
    if isinstance(entry, dict) and entry.get("msg") == "incident":
        incidents[entry.get("fingerprint")] = entry
explained = [e for e in events if e.get("reportingController") == "kausa"
             and e.get("reason") == "Explained" and e["regarding"]["kind"] == kind
             and e["regarding"]["name"] == name]
if not explained:
    fail("no Explained Event")
spec = {"rule": rule, "target": {"apiVersion": api_version, "kind": kind, "name": name}}
summary = ANSWER["summary"].replace("namespace-1", target)
answer = {"summary": summary, "cause": ANSWER["cause"], "steps": ANSWER["steps"],
          "confidence": ANSWER["confidence"], "model": "fake"}
names = set()
for event in explained:
    match = re.match(r"kausa\.([0-9a-f]{16})\.", event["metadata"]["name"])
    if not match:
        fail("the Event %s is not named kausa.<fingerprint>.<hash>" % event["metadata"]["name"])
    fingerprint = match.group(1)
    incident = incidents.get(fingerprint)
    if incident is None:
        fail("no incident with fingerprint %s in the log" % fingerprint)
    if (incident.get("detection"), incident.get("namespace"), incident.get("owner")) != (
            rule, target, spec["target"]):
        fail("the log gives incident %s another detection or owner" % fingerprint)
    found = objects.get("kausa." + fingerprint)
    if found is None:
        fail("no Explanation kausa." + fingerprint)
    metadata = found["metadata"]
    if (metadata.get("labels") or {}).get("app.kubernetes.io/managed-by") != "kausa":
        fail("no label app.kubernetes.io/managed-by=kausa")
    if metadata.get("ownerReferences"):
        fail("it has ownerReferences")
    if found.get("spec") != spec:
        fail("its spec is %s" % json.dumps(found.get("spec")))
    status = found.get("status") or {}
    if sorted(status) != ["entries", "lastTime", "occurrences"]:
        fail("its status has the fields %s" % sorted(status))
    entries = status["entries"]
    if not isinstance(entries, list) or not 1 <= len(entries) <= 5:
        fail("it has %s entries" % (len(entries) if isinstance(entries, list) else "no list of"))
    first = entries[0]
    if sorted(first) != sorted(FIELDS):
        fail("its newest entry has the fields %s" % sorted(first))
    for key, value in answer.items():
        if first[key] != value:
            fail("its newest entry has %s %s, not %s" % (key, json.dumps(first[key]),
                                                         json.dumps(value)))
    if target not in first["summary"]:
        fail("the summary does not name " + target)
    if not event["note"].startswith(first["summary"] + " "):
        fail("the summary does not start the Event note")
    if not isinstance(first["promptVersion"], str) or not 1 <= len(first["promptVersion"]) <= 16:
        fail("its promptVersion is %s" % json.dumps(first["promptVersion"]))
    times = [first["firstTime"], first["lastTime"], status["lastTime"]]
    if not all(isinstance(t, str) and TIME.fullmatch(t) for t in times):
        fail("its times are %s" % json.dumps(times))
    if not first["firstTime"] <= first["lastTime"] == status["lastTime"]:
        fail("its times are out of order: %s" % json.dumps(times))
    if not number(first["count"], 1) or not number(status["occurrences"], first["count"]):
        fail("count %s, occurrences %s" % (first["count"], status["occurrences"]))
    names.add("kausa." + fingerprint)
print("\n".join(sorted(names)))
' "$1" "$2" "$3" "$4" "$TARGET" "$WORK"
}
# Runs check_explanations until it passes, for the history write that follows each Event; sets
# history_names to the names it printed.
explanations_of() {
  for _ in $(seq 1 10); do
    k -n "$TARGET" get events.events.k8s.io -o json > "$WORK/events.json"
    k -n "$TARGET" get "$EXPLANATIONS" -o json > "$WORK/explanations.json"
    kausa_log
    history_names=$(check_explanations "$@" 2>/dev/null) && return 0
    sleep 3
  done
  history_names=$(check_explanations "$@") || exit 1
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

# Each failing workload's Explanation (src/detect.rs: D001 is a crash loop, D008 a failed Job).
explanations_of Deployment crasher D001 apps/v1
deployment_history=$history_names
explanations_of Job failing-job D008 batch/v1
echo "Explanations: $deployment_history $history_names"
# In both scopes, all of kausa's Explanations are in TARGET.
outside=$(explanations -A | grep -v "^$TARGET/" || true)
[ -z "$outside" ] || fail "Explanations outside $TARGET: $outside"
if [ "$SCOPE" = namespaced ]; then
  [ -z "$(explanations "$OTHER")" ] || fail "an Explanation in $OTHER"
fi
# The columns, with the two of -o wide from the newest entry.
k -n "$TARGET" get "$EXPLANATIONS" -o wide > "$WORK/wide.txt"
header=$(head -n 1 "$WORK/wide.txt" | tr -s ' ')
[ "$header" = "NAME KIND TARGET RULE COUNT LAST AGE CONFIDENCE SUMMARY" ] ||
  fail "kubectl get $EXPLANATIONS -o wide prints the columns '$header'"
for name in $deployment_history; do
  row=$(awk -v name="$name" '$1 == name' "$WORK/wide.txt" | tr -s ' ')
  case "$row" in
    "$name Deployment crasher D001 "*" high e2e: a workload in $TARGET fails.") ;;
    *) fail "kubectl get -o wide prints for $name: '$row'" ;;
  esac
done

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
  "get replicasets.apps" "get jobs.batch"
  "list $EXPLANATIONS" "create $EXPLANATIONS" "patch $EXPLANATIONS" "delete $EXPLANATIONS")
refused=("get secrets" "list secrets" "get configmaps" "list configmaps" "delete pods"
  "create pods" "create pods --subresource=exec" "update events.events.k8s.io"
  "delete events.events.k8s.io" "list nodes" "create serviceaccounts --subresource=token"
  "impersonate users" "list replicasets.apps" "list jobs.batch"
  "get $EXPLANATIONS" "watch $EXPLANATIONS" "update $EXPLANATIONS"
  "deletecollection $EXPLANATIONS"
  "create customresourcedefinitions.apiextensions.k8s.io"
  "patch customresourcedefinitions.apiextensions.k8s.io")
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
  for ns in "$TARGET" "$DENIED"; do
    [ "$(history_rule role/"$RELEASE" -n "$ns")" = yes ] || fail "no history rule in $ns's Role"
  done
else
  [ "$(history_rule clusterrole/"$RELEASE")" = yes ] || fail "no history rule in the ClusterRole"
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
echo "$metrics" | grep -E '^kausa_history_writes_total\{outcome="created"\} [1-9]' >/dev/null ||
  fail "kausa_history_writes_total does not count the Explanations created"
restarts=$(k -n "$NAMESPACE" get pods -l app.kubernetes.io/name=kausa \
  -o jsonpath='{.items[0].status.containerStatuses[0].restartCount}')
[ "$restarts" = 0 ] || fail "kausa restarted $restarts times"
# The history adds no request: each one the endpoint received is a model call kausa counted. A
# call still in flight is logged by the endpoint before kausa counts it, so this is retried.
calls="" lines=""
for _ in $(seq 1 10); do
  calls=$(k get --raw "/api/v1/namespaces/$NAMESPACE/services/kausa-metrics:9090/proxy/metrics" |
    awk '$1 ~ /^kausa_model_calls_total([{]|$)/ { sum += $2 } END { print sum + 0 }')
  lines=$(wc -l < "$WORK/requests.jsonl" | tr -d ' ')
  [ "$calls" = "$lines" ] && break
  sleep 2
done
[ "$calls" = "$lines" ] || fail "the endpoint received $lines requests for $calls model calls"
echo "requests: $lines, as many as model calls"

# The built-in view and edit roles read Explanations through the reader role, and write none. The
# aggregation controller fills them in a moment after the install.
for role in view edit; do
  k -n "$TARGET" create serviceaccount "e2e-$role"
  k -n "$TARGET" create rolebinding "e2e-$role" --clusterrole="$role" \
    --serviceaccount="$TARGET:e2e-$role"
done
for role in view edit; do
  as="system:serviceaccount:$TARGET:e2e-$role"
  readable=""
  for _ in $(seq 1 30); do
    [ "$(k auth can-i --as="$as" list "$EXPLANATIONS" -n "$TARGET" 2>/dev/null)" = yes ] &&
      readable=yes && break
    sleep 2
  done
  [ -n "$readable" ] || fail "$role cannot list $EXPLANATIONS in $TARGET"
  listed=$(k --as="$as" -n "$TARGET" get "$EXPLANATIONS" -o name)
  for name in $deployment_history; do
    printf '%s\n' "$listed" | grep -qxF -- "explanation.kausa.fedeam.com/$name" ||
      fail "$role does not list $name"
  done
  for verb in create delete; do
    if [ "$(k auth can-i --as="$as" "$verb" "$EXPLANATIONS" -n "$TARGET" 2>/dev/null)" = yes ]; then
      fail "$role can $verb $EXPLANATIONS in $TARGET"
    fi
  done
done
echo "view and edit: list Explanations, create and delete none"

# What the CustomResourceDefinition refuses, as cluster administrator, on hand-made objects.
now=$(utc_now)
hand_made e2e-admin no 1 "$now" | k create -f - >/dev/null
# The command after $1 fails, with $1 in its output.
refused_with() {
  want=$1
  shift
  if out=$("$@" 2>&1); then fail "accepted: $*"; fi
  case "$out" in *"$want"*) ;; *) fail "refused without '$want': $out" ;; esac
}
refused_with "spec is immutable" \
  k -n "$TARGET" patch "$EXPLANATIONS" e2e-admin --type=merge -p '{"spec":{"rule":"D002"}}'
refused_with "Required value" \
  k -n "$TARGET" patch "$EXPLANATIONS" e2e-admin --type=merge -p '{"spec":null}'
hand_made e2e-no-spec no 1 "$now" | python3 -c '
import json, sys
body = json.load(sys.stdin)
del body["spec"]
print(json.dumps(body))
' > "$WORK/no-spec.json"
refused_with "Required value" k create -f "$WORK/no-spec.json"
# A merge patch with the resourceVersion read is a precondition: once the object has changed, the
# same patch answers 409.
version=$(k -n "$TARGET" get "$EXPLANATIONS" e2e-admin -o jsonpath='{.metadata.resourceVersion}')
patch="{\"metadata\":{\"resourceVersion\":\"$version\"},\"status\":{\"occurrences\":2}}"
k -n "$TARGET" patch "$EXPLANATIONS" e2e-admin --type=merge -p "$patch" >/dev/null
refused_with "(Conflict)" k -n "$TARGET" patch "$EXPLANATIONS" e2e-admin --type=merge -p "$patch"
# maxLength counts characters: 300 two-byte characters fit, 301 do not.
hand_made e2e-summary-300 no 300 "$now" | k create -f - >/dev/null
hand_made e2e-summary-301 no 301 "$now" | refused_with "summary" k create -f -
echo "the CustomResourceDefinition refuses a spec change or removal, a stale patch and 301 characters"

# The sweep at start: of three old objects, it deletes only the one that is kausa-shaped. The
# other two carry kausa's label with another name, or kausa's name without the label.
old_time=2000-01-01T00:00:00Z
hand_made kausa.0000000000000001 yes 1 "$old_time" | k create -f - >/dev/null
hand_made e2e-old-labelled yes 1 "$old_time" | k create -f - >/dev/null
hand_made kausa.0000000000000002 no 1 "$old_time" | k create -f - >/dev/null
# crasher's Explanations before the restart, for kausa's own update after it.
k -n "$TARGET" get "$EXPLANATIONS" -o json > "$WORK/before-restart.json"
restart_kausa
swept=""
for _ in $(seq 1 30); do
  kausa_log "$pod"
  python3 -c '
import json, sys
for line in sys.stdin:
    try:
        entry = json.loads(line)
    except ValueError:
        continue
    if isinstance(entry, dict) and entry.get("msg") == "history swept" and (
            entry.get("deleted_by_age") or 0) >= 1:
        sys.exit(0)
sys.exit(1)
' < "$WORK/kausa.log" &&
    [ -z "$(explanations "$TARGET" | grep -xF -- "$TARGET/kausa.0000000000000001" || true)" ] &&
    swept=yes && break
  sleep 2
done
[ -n "$swept" ] ||
  fail "no 'history swept' line deleting by age, or the old kausa-shaped Explanation remains"
for name in e2e-old-labelled kausa.0000000000000002; do
  explanations "$TARGET" | grep -qxF -- "$TARGET/$name" || fail "the sweep deleted $name"
done
metrics=$(k get --raw "/api/v1/namespaces/$NAMESPACE/services/kausa-metrics:9090/proxy/metrics")
echo "$metrics" | grep -E '^kausa_history_deleted_total\{reason="age"\} [1-9]' >/dev/null ||
  fail "kausa_history_deleted_total does not count the deletion by age"
echo "the sweep deleted the old kausa-shaped Explanation and kept the other two"

# kausa's own update: the restart emptied the deduplication cache and crasher still fails, so
# kausa explains it again and patches its existing Explanation (a list, the identity check, and a
# merge patch of status and resourceVersion with fieldValidation=Strict). One of crasher's
# Explanations has more occurrences than before the restart, and its newest entry a higher count
# or a later lastTime; the new Pod counts the update and no failed or refused write.
updated=""
for _ in $(seq 1 40); do
  metrics=$(k get --raw "/api/v1/namespaces/$NAMESPACE/services/kausa-metrics:9090/proxy/metrics")
  if echo "$metrics" | grep -E '^kausa_history_writes_total\{outcome="updated"\} [1-9]' >/dev/null
  then
    k -n "$TARGET" get "$EXPLANATIONS" -o json > "$WORK/after-restart.json"
    python3 -c '
import json, sys
names, work = sys.argv[1].split(), sys.argv[2]
def load(name):
    items = json.load(open(work + "/" + name))["items"]
    return {o["metadata"]["name"]: o.get("status") or {} for o in items}
before, after = load("before-restart.json"), load("after-restart.json")
for name in names:
    old, new = before.get(name), after.get(name)
    if not old or not new or not old.get("entries") or not new.get("entries"):
        continue
    if new["occurrences"] > old["occurrences"] and (
            new["entries"][0]["count"] > old["entries"][0]["count"]
            or new["entries"][0]["lastTime"] > old["entries"][0]["lastTime"]):
        sys.exit(0)
sys.exit(1)
' "$deployment_history" "$WORK" && updated=yes && break
  fi
  sleep 3
done
[ -n "$updated" ] || fail "kausa did not update crasher's Explanation after the restart"
for outcome in error refused; do
  series="kausa_history_writes_total\\{outcome=\"$outcome\"\\}"
  if echo "$metrics" | grep -E "^$series [1-9]" >/dev/null; then
    fail "kausa_history_writes_total counts an outcome $outcome after the restart"
  fi
done
echo "kausa updated crasher's Explanation: more occurrences, a higher count or a later lastTime"

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
  # The CustomResourceDefinition goes with the release, and every Explanation with it.
  HISTORY=off
  crd_gone
  no_cluster_rbac
  echo "helm uninstall: no Role, no reader role and no CustomResourceDefinition left"
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

  # dryRun=true turns the history off: the rule, the reader role and the CustomResourceDefinition
  # go, and with it every Explanation, kausa's and the hand-made ones.
  [ -n "$(explanations -A)" ] || fail "no Explanation before the upgrade to dryRun=true"
  h upgrade "$RELEASE" charts/kausa -n "$NAMESPACE" --wait --timeout 120s --reuse-values \
    --set dryRun=true
  HISTORY=off
  [ "$(history_rule clusterrole/"$RELEASE")" = no ] || fail "a history rule with dryRun=true"
  [ -z "$(k get clusterrole "$READER" --ignore-not-found -o name)" ] ||
    fail "the reader role remains with dryRun=true"
  crd_gone
  # Back on: a new CustomResourceDefinition, which holds none of the objects of the old one.
  h upgrade "$RELEASE" charts/kausa -n "$NAMESPACE" --wait --timeout 120s --reuse-values \
    --set dryRun=false
  HISTORY=on
  [ "$(history_rule clusterrole/"$RELEASE")" = yes ] || fail "no history rule after dryRun=false"
  k get clusterrole "$READER" -o name >/dev/null
  for name in e2e-admin e2e-summary-300 e2e-old-labelled kausa.0000000000000002; do
    if explanations "$TARGET" | grep -qxF -- "$TARGET/$name"; then
      fail "$name outlived its CustomResourceDefinition"
    fi
  done
  echo "helm upgrade --set dryRun=true: no history rule, reader role or Explanation left"

  # history.enabled=false: the rule, the reader role and the CustomResourceDefinition go.
  h upgrade "$RELEASE" charts/kausa -n "$NAMESPACE" --wait --timeout 120s --reuse-values \
    --set history.enabled=false
  HISTORY=off
  [ "$(history_rule clusterrole/"$RELEASE")" = no ] || fail "a history rule without the history"
  [ -z "$(k get clusterrole "$READER" --ignore-not-found -o name)" ] ||
    fail "the reader role remains without the history"
  crd_gone
  echo "helm upgrade --set history.enabled=false: no history rule, reader role or CRD left"

  # From cluster to namespaced scope in place, with the history on again: the ClusterRole and its
  # binding go, a Role and a RoleBinding come; the reader role and the CustomResourceDefinition are
  # the only cluster-scoped objects.
  h upgrade "$RELEASE" charts/kausa -n "$NAMESPACE" --wait --timeout 120s --reuse-values \
    --set rbac.scope=namespaced --set "rbac.namespaces={$TARGET}" --set history.enabled=true
  HISTORY=on
  no_cluster_rbac
  has_roles "$TARGET"
  [ "$(history_rule role/"$RELEASE" -n "$TARGET")" = yes ] || fail "no history rule in the Role"
  echo "helm upgrade to rbac.scope=namespaced: no cluster-scoped RBAC object, a Role in $TARGET"

  # A deleted CustomResourceDefinition: kausa starts, is Ready, does not restart, and says why it
  # cannot sweep.
  k delete crd "$CRD"
  restart_kausa
  failed_sweep=""
  for _ in $(seq 1 30); do
    kausa_log "$pod"
    log_has "history sweep failed" "$CRD_MISSING" anywhere < "$WORK/kausa.log" &&
      failed_sweep=yes && break
    sleep 2
  done
  [ -n "$failed_sweep" ] || fail "no 'history sweep failed' line naming $CRD"
  ready=$(k -n "$NAMESPACE" get "$pod" \
    -o jsonpath='{.status.conditions[?(@.type=="Ready")].status}')
  [ "$ready" = True ] || fail "kausa is not Ready ($ready) without the CustomResourceDefinition"
  restarts=$(k -n "$NAMESPACE" get "$pod" -o jsonpath='{.status.containerStatuses[0].restartCount}')
  [ "$restarts" = 0 ] || fail "kausa restarted $restarts times without the CRD"
  echo "without the CustomResourceDefinition: kausa is Ready, did not restart and logs why"
fi

echo "e2e: ok"
