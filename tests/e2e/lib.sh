# shellcheck shell=bash disable=SC2034
# The helpers of the end-to-end tests in kind: sourced from the repository root by
# tests/e2e/run.sh and tests/e2e/upgrade.sh, never run. It defines fail, the constants, k, h,
# cleanup, start_endpoint (the fake model endpoint on the host, reachable from the cluster) and the
# helpers that read the cluster, kausa's log and its metrics. The functions read the globals when
# they are called, so a script may set CLUSTER, CONTEXT, SCOPE or REPLICAS after sourcing it. The
# scripts that source it use the constants and globals it sets, which shellcheck cannot see here
# (SC2034).

fail() { echo "FAIL: $*" >&2; exit 1; }

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
# With two replicas (docs/adr/0019-high-availability.md): the Lease the chart creates and its Role
# and RoleBinding, all three named so, in NAMESPACE; the PodDisruptionBudget is named RELEASE.
LEASE=$RELEASE-leader
LEASES=leases.coordination.k8s.io
# src/leader.rs: the hint of a lease error answered 403.
HINT_403="kausa needs get and patch on this Lease (docs/reference/permissions.md)"
# The fake endpoint's fail file (tests/e2e/fake_endpoint.py): while it exists, every model request
# is answered with the status on its first line.
FAIL_FILE=$WORK/endpoint-fail
# The requests the helm test Pod sent to the fake endpoint: outside kausa's budget, they are no
# model call of kausa's (docs/adr/0020-check-the-endpoint.md).
CHECK_REQUESTS=0
k() { kubectl --context "$CONTEXT" "$@"; }
h() { helm --kube-context "$CONTEXT" "$@"; }

cleanup() {
  status=$?
  [ -n "${LOGS_PID:-}" ] && kill "$LOGS_PID" 2>/dev/null || true
  if [ "$status" -ne 0 ]; then
    # Every kausa Pod's log: with two replicas, the leader's and the standby's.
    for p in $(k -n "$NAMESPACE" get pods -l app.kubernetes.io/name=kausa -o name 2>/dev/null ||
      true); do
      echo "--- kausa's log: $p" >&2
      k -n "$NAMESPACE" logs "$p" --tail=200 >&2 || true
    done
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

start_endpoint() {
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
# The time log holds one receive time per line of the request log, so both always count the same.
python3 tests/e2e/fake_endpoint.py --port "$PORT" --log "$WORK/requests.jsonl" \
  --time-log "$WORK/request-times.txt" --hook-log "$WORK/hooks.jsonl" --fail-file "$FAIL_FILE" &
ENDPOINT_PID=$!
for _ in $(seq 1 20); do
  curl -s -o /dev/null -X POST "http://127.0.0.1:$PORT/v1/chat/completions" && break
  kill -0 "$ENDPOINT_PID" 2>/dev/null || fail "the fake endpoint did not start"
  sleep 0.5
done
curl -s -o /dev/null -X POST "http://127.0.0.1:$PORT/v1/chat/completions" ||
  fail "the fake endpoint does not answer on port $PORT"
: > "$WORK/requests.jsonl"
: > "$WORK/request-times.txt"
: > "$WORK/hooks.jsonl"
# The notifications' webhook: a fake Telegram-shaped bot token in the URL's path, and a fake bearer
# token, both built here so that no file holds a token-shaped string. Telegram's path is checked
# only with the telegram format: this one is for json.
FAKE_BOT="1234567890:E2eFake$(printf '%028d' "$PORT")"
FAKE_BEARER="e2e-fake-bearer-$PORT-not-a-real-one"
HOOK_PATH="/hook/bot$FAKE_BOT/sendMessage"
printf '%s' "http://$gateway:$PORT$HOOK_PATH" > "$WORK/notify-url"
printf '%s' "$FAKE_BEARER" > "$WORK/notify-token"
}

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
# kausa's log, into a file first, so that a failed read is a failure and not an empty log: of Pod
# $1 (pod/<name>) if given, else of the one Pod, or with two replicas of the Lease holder's.
kausa_log() {
  from=${1:-}
  if [ -z "$from" ]; then
    if [ "$REPLICAS" = 1 ]; then from=deploy/kausa; else from=pod/$(holder); fi
  fi
  k -n "$NAMESPACE" logs "$from" > "$WORK/kausa.log"
}
# The Pod that holds kausa's Lease, as its holderIdentity names it (empty while nobody does). It
# only picks the Pod to ask, and no failure message prints it.
holder() {
  k -n "$NAMESPACE" get lease "$LEASE" -o jsonpath='{.spec.holderIdentity}'
}
# kausa's Pods that are not being deleted, one name per line.
kausa_pods() {
  k -n "$NAMESPACE" get pods -l app.kubernetes.io/name=kausa -o json | python3 -c '
import json, sys
for pod in json.load(sys.stdin)["items"]:
    if not pod["metadata"].get("deletionTimestamp"):
        print(pod["metadata"]["name"])
'
}
# "yes" when exactly $1 kausa Pods are not being deleted and every one of them is Ready.
pods_ready() {
  k -n "$NAMESPACE" get pods -l app.kubernetes.io/name=kausa -o json | python3 -c '
import json, sys
want = int(sys.argv[1])
pods = [p for p in json.load(sys.stdin)["items"] if not p["metadata"].get("deletionTimestamp")]
ready = [p for p in pods if any(c.get("type") == "Ready" and c.get("status") == "True"
                                for c in (p.get("status") or {}).get("conditions") or [])]
print("yes" if len(pods) == want and len(ready) == want else "no")
' "$1"
}
# GET $2 (metrics or readyz) of kausa in Pod $1 (its name), through the API server's pod proxy:
# with two replicas the Service proxy balances across both Ready Pods.
pod_get() {
  k get --raw "/api/v1/namespaces/$NAMESPACE/pods/$1:9090/proxy/$2"
}
# The value of series $2, as /metrics prints it before the value, in Pod $1 (empty if absent).
pod_metric() {
  pod_get "$1" metrics | awk -v series="$2" '$1 == series { print $2 }'
}
# kausa's metrics: through the Service with one replica, from the Lease holder's Pod with two.
kausa_metrics() {
  if [ "$REPLICAS" = 1 ]; then
    k get --raw "/api/v1/namespaces/$NAMESPACE/services/kausa-metrics:9090/proxy/metrics"
  else
    pod_get "$(holder)" metrics
  fi
}
# The model calls kausa counted, over every Pod: a standby makes none.
model_calls() {
  if [ "$REPLICAS" = 1 ]; then
    kausa_metrics
  else
    for p in $(kausa_pods); do pod_get "$p" metrics; done
  fi | awk '$1 ~ /^kausa_model_calls_total([{]|$)/ { sum += $2 } END { print sum + 0 }'
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
# What the rules of an RBAC object ($@, as kubectl get takes it) say about Deployments
# (docs/adr/0018-stuck-rollouts.md): "yes" when the one rule about them is list and watch and comes
# right after the replicasets rule, "no" when there is none.
deployments_rule() {
  k get "$@" -o json | python3 -c '
import json, sys
rules = json.load(sys.stdin).get("rules") or []
want = {"apiGroups": ["apps"], "resources": ["deployments"], "verbs": ["list", "watch"]}
replicasets = {"apiGroups": ["apps"], "resources": ["replicasets"], "verbs": ["get"]}
about = [r for r in rules if any(x.startswith("deployments") for x in r.get("resources") or [])]
if not about:
    print("no")
elif about == [want] and replicasets in rules and rules.index(want) == rules.index(replicasets) + 1:
    print("yes")
else:
    print("other rules: " + json.dumps(about))
'
}
# The arguments of kausa's container in Pod $1 (pod/<name>), one per line.
kausa_args() {
  k -n "$NAMESPACE" get "$1" -o json | python3 -c '
import json, sys
for container in json.load(sys.stdin)["spec"]["containers"]:
    if container["name"] == "kausa":
        print("\n".join(container.get("args") or []))
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
# Restarts kausa and waits until the rollout is done (the new Pod is Ready); sets pod (pod/<name>),
# with two replicas to the new Pod that holds the Lease.
restart_kausa() {
  old=$(k -n "$NAMESPACE" get pods -l app.kubernetes.io/name=kausa -o name)
  k -n "$NAMESPACE" rollout restart deploy/kausa
  k -n "$NAMESPACE" rollout status deploy/kausa --timeout 120s
  if [ "$REPLICAS" = 2 ]; then
    wait_pair 60 "no two Ready kausa Pods with one of them holding the Lease after the restart"
    pod=pod/$leader
    if printf '%s\n' "$old" | grep -qxF -- "$pod"; then
      fail "a kausa Pod from before the restart holds the Lease"
    fi
    return 0
  fi
  pod=$(k -n "$NAMESPACE" get pods -l app.kubernetes.io/name=kausa -o name |
    grep -vxF -- "$old" | head -n 1 || true)
  [ -n "$pod" ] || fail "no new kausa Pod after the restart"
}
# Checks the Explanation of each Explained Event about $1 (kind) $2 (name) in TARGET, with the
# events, the Explanations and kausa's log in WORK: named kausa.<fingerprint> after the Event, its
# spec {$3, $4 $1 $2} cross-checked with the incident kausa logged under that fingerprint, its
# newest entry the fake endpoint's answer with the namespace mapped back and the injected step
# redacted, starting the Event's note, and every field kausa writes read back (none pruned). Prints
# the Explanations' names.
check_explanations() {
  python3 -c '
import json, re, sys
sys.path.insert(0, "tests/e2e")
from fake_endpoint import ANSWER, REDACTED_STEP
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
answer = {"summary": summary, "cause": ANSWER["cause"], "confidence": ANSWER["confidence"],
          "model": "fake"}
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
    # The first step as sent, the second redacted (docs/adr/0022-redact-the-answer.md); the
    # message names no value, in case one reached the entry.
    steps = first["steps"]
    if (not isinstance(steps, list) or len(steps) != 2 or steps[0] != ANSWER["steps"][0]
            or not isinstance(steps[1], str) or not re.fullmatch(REDACTED_STEP, steps[1])):
        fail("its newest entry does not have the answer'"'"'s steps, the second one redacted")
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
# None of the values the fake endpoint injected in its answer reached a sink
# (docs/adr/0022-redact-the-answer.md): kausa's Event notes in TARGET, the entries of the
# Explanations in TARGET, the bodies the webhook received and kausa's explanation log lines, from
# events.json, explanations.json, hooks.jsonl and kausa.log in WORK. Each sink must hold something
# to look at. The messages name the sink, never the value.
no_injected_values() {
  python3 -c '
import json, sys
sys.path.insert(0, "tests/e2e")
from fake_endpoint import INJECTED
work = sys.argv[1]

def fail(message):
    sys.exit("FAIL: " + message)

notes = [e.get("note") or "" for e in json.load(open(work + "/events.json"))["items"]
         if e.get("reportingController") == "kausa"]
entries = [json.dumps(entry) for item in json.load(open(work + "/explanations.json"))["items"]
           for entry in (item.get("status") or {}).get("entries") or []]
hooks = [line for line in open(work + "/hooks.jsonl") if line.strip()]
lines = []
for line in open(work + "/kausa.log"):
    try:
        entry = json.loads(line)
    except ValueError:
        continue
    if isinstance(entry, dict) and entry.get("msg") == "explanation":
        lines.append(line)
for sink, texts in (("an Event note", notes), ("an Explanation entry", entries),
                    ("a notification", hooks), ("an explanation log line", lines)):
    if not texts:
        fail("no %s to look for the injected values in" % sink)
    if any(value in text for value in INJECTED for text in texts):
        fail("an injected value reached " + sink)
print("the injected values: in none of %d Event notes, %d Explanation entries, %d notifications "
      "and %d explanation log lines" % (len(notes), len(entries), len(hooks), len(lines)))
' "$WORK"
}
# The Role and the RoleBinding named after the release exist in namespace $1.
has_roles() {
  k -n "$1" get "role/$RELEASE" "rolebinding/$RELEASE" -o name >/dev/null ||
    fail "no Role and RoleBinding $RELEASE in $1"
}

# The value of field $1 of the one started line in kausa's log in WORK, as JSON.
started_field() {
  python3 -c '
import json, sys
started = []
for line in open(sys.argv[2] + "/kausa.log"):
    try:
        entry = json.loads(line)
    except ValueError:
        continue
    if isinstance(entry, dict) and entry.get("msg") == "started":
        started.append(entry)
if len(started) != 1:
    sys.exit("FAIL: %d started lines in the log" % len(started))
print(json.dumps(started[0].get(sys.argv[1], "<missing>")))
' "$1" "$WORK"
}
# The number of lines of a kausa log on stdin whose msg is one of $1 (separated by "|") and, with
# $2 and $3, whose field $2 is $3.
log_count() {
  python3 -c '
import json, sys
msgs = sys.argv[1].split("|")
field = sys.argv[2:4] if len(sys.argv) > 3 else None
count = 0
for line in sys.stdin:
    try:
        entry = json.loads(line)
    except ValueError:
        continue
    if (isinstance(entry, dict) and entry.get("msg") in msgs
            and (field is None or entry.get(field[0]) == field[1])):
        count += 1
print(count)
' "$@"
}
# The number of kausa's Events in TARGET, about $2 (kind) $3 (name) if given, that another Pod than
# $1 reported (reportingInstance is the Pod's name).
others_reported() {
  k -n "$TARGET" get events.events.k8s.io -o json | python3 -c '
import json, sys
me, about = sys.argv[1], sys.argv[2:4]
count = 0
for event in json.load(sys.stdin)["items"]:
    regarding = event.get("regarding") or {}
    if event.get("reportingController") != "kausa":
        continue
    if about and [regarding.get("kind"), regarding.get("name")] != about:
        continue
    count += event.get("reportingInstance") != me
print(count)
' "$@"
}
# Seeds one more failure in TARGET: a Job named $1 whose only Pod fails, as failing-job's (D008).
seed_failure() {
  k apply -f - >/dev/null <<EOF
apiVersion: batch/v1
kind: Job
metadata:
  name: $1
  namespace: $TARGET
spec:
  backoffLimit: 0
  template:
    spec:
      restartPolicy: Never
      containers:
        - name: task
          image: kausa:e2e
          imagePullPolicy: Never
          args: ["--no-such-flag"]
EOF
}
# Waits for an Explained Event about $1 (kind) $2 (name) in TARGET.
wait_explained() {
  for _ in $(seq 1 60); do
    [ -n "$(explained "$1" "$2")" ] && return 0
    sleep 3
  done
  fail "no Explained Event about $1 $2"
}
# Waits up to $1 seconds until two kausa Pods are Ready and the Lease names one of them; sets
# leader and standby to their names. $2 is the failure message.
wait_pair() {
  leader="" standby=""
  deadline=$((SECONDS + $1))
  while :; do
    pods=$(kausa_pods)
    current=$(holder)
    if [ "$(pods_ready 2)" = yes ] && [ -n "$current" ] &&
      printf '%s\n' "$pods" | grep -qxF -- "$current"; then
      leader=$current
      standby=$(printf '%s\n' "$pods" | grep -vxF -- "$current")
      return 0
    fi
    [ "$SECONDS" -lt "$deadline" ] || fail "$2"
    sleep 2
  done
}
# Waits at most $2 seconds from now for the Lease to name Pod $1; sets took to the whole seconds it
# took. Fails (returns 1) otherwise.
wait_holder() {
  since=$SECONDS
  while :; do
    if [ "$(holder)" = "$1" ]; then
      took=$((SECONDS - since))
      return 0
    fi
    [ $((SECONDS - since)) -lt "$2" ] || return 1
    sleep 0.5
  done
}
# Waits up to $2 seconds for a holder other than Pod $1, sets it in new_holder.
wait_other_holder() {
  since=$SECONDS
  while :; do
    new_holder=$(holder)
    if [ -n "$new_holder" ] && [ "$new_holder" != "$1" ]; then
      took=$((SECONDS - since))
      return 0
    fi
    [ $((SECONDS - since)) -lt "$2" ] || return 1
    sleep 0.5
  done
}
# The sum of kausa_leader over kausa's Pods; "missing" when there is no Pod or a Pod's metrics
# have no kausa_leader (a failed read must not pass for 0).
leader_sum() {
  sum=0 seen=""
  for p in $(kausa_pods); do
    value=$(pod_metric "$p" kausa_leader)
    [ -n "$value" ] || { echo missing; return 0; }
    sum=$((sum + value)) seen=yes
  done
  if [ -n "$seen" ]; then echo "$sum"; else echo missing; fi
}
# The sum of kausa_leadership_changes_total over both values of to, in Pod $1; "missing" when
# its metrics have neither series.
changes() {
  pod_get "$1" metrics | awk '$1 ~ /^kausa_leadership_changes_total[{]/ { sum += $2; n++ }
    END { if (n == 2) print sum; else print "missing" }'
}
# The Lease's leaseTransitions, 0 when absent.
lease_transitions() {
  value=$(k -n "$NAMESPACE" get lease "$LEASE" -o jsonpath='{.spec.leaseTransitions}')
  echo "${value:-0}"
}
# The restart count of kausa's container in Pod $1 (its name).
restart_count() {
  k -n "$NAMESPACE" get pod "$1" -o jsonpath='{.status.containerStatuses[0].restartCount}'
}
# The PIDs, on the kind node, of the processes whose environment holds POD_NAME=$1: kausa in Pod
# $1, which the chart gives its own name through the downward API.
node_pids() {
  docker exec "$CLUSTER-control-plane" sh -c '
for dir in /proc/[0-9]*; do
  if tr "\0" "\n" < "$dir/environ" 2>/dev/null | grep -qxF "POD_NAME=$1"; then
    echo "${dir#/proc/}"
  fi
done' sh "$1"
}
# Sends signal $1 (STOP or CONT) to process $2 on the kind node, with the node shell's kill.
node_signal() {
  docker exec "$CLUSTER-control-plane" sh -c 'kill -"$1" "$2"' sh "$1" "$2"
}
# A Lease named $1 in NAMESPACE, as a manifest.
lease_manifest() {
  cat <<EOF
apiVersion: coordination.k8s.io/v1
kind: Lease
metadata:
  name: $1
  namespace: $NAMESPACE
spec: {}
EOF
}
# kausa's Events and the Explanations in TARGET, each with its resourceVersion: any write by kausa
# (a create, a series patch, a history write or a sweep) changes it.
writes_snapshot() {
  row='{.metadata.name} {.metadata.resourceVersion}{"\n"}'
  k -n "$TARGET" get events.events.k8s.io \
    -o jsonpath="{range .items[?(@.reportingController==\"kausa\")]}$row{end}"
  k -n "$TARGET" get "$EXPLANATIONS" -o jsonpath="{range .items[*]}$row{end}"
}
# The number of lines in file $1.
lines_of() { wc -l < "$1" | tr -d ' '; }
# Checks the endpoint check (docs/adr/0020-check-the-endpoint.md) whose helm test output is in
# WORK/helm-test.txt, for the release's model $2, from the fake endpoint's request log as it was
# $3 lines long before it: with $1 = ok, result ok; with $1 = not_found, a 404 for a model the
# endpoint does not have. Its one endpoint check line has exactly the documented fields, and the
# endpoint received exactly one request with the synthetic body of the golden file (model.jsonSchema
# stays true), the model set to $2. The messages name fields and counts, never a request.
check_endpoint_line() {
  python3 -I - "$@" "$WORK" "http://$gateway:$PORT" <<'EOF'
import json, sys

mode, model, before, work, endpoint = sys.argv[1:6]
GOLDEN = "tests/golden/check-endpoint-request-json-schema.json"
FIELDS = ["answer_valid", "api_key", "authenticated", "cause", "class", "connected", "detail",
          "endpoint", "hint", "json_schema", "latency_ms", "level", "model", "model_found", "msg",
          "prompt_version", "reason", "request_accepted", "result", "retry_after_seconds",
          "status", "timeout_seconds", "tokens_in", "tokens_out", "ts"]


def fail(message):
    sys.exit("FAIL: helm test with model %s: %s" % (model, message))


found = []
for text in open(work + "/helm-test.txt", encoding="utf-8", errors="replace"):
    try:
        entry = json.loads(text)
    except ValueError:
        continue
    if isinstance(entry, dict) and entry.get("msg") == "endpoint check":
        found.append(entry)
if len(found) != 1:
    fail("%d endpoint check lines in its output" % len(found))
line = found[0]
if sorted(line) != FIELDS:
    fail("the endpoint check line has the fields %s" % sorted(line))
want = {"endpoint": endpoint, "model": model, "json_schema": True, "timeout_seconds": 120,
        "api_key": False, "connected": True, "retry_after_seconds": None}
if mode == "ok":
    want.update({"level": "info", "result": "ok", "cause": None, "class": None, "status": 200,
                 "authenticated": True, "model_found": True, "request_accepted": True,
                 "answer_valid": True, "reason": None, "hint": None, "detail": None,
                 "tokens_in": 100, "tokens_out": 20})
else:
    want.update({"level": "error", "result": "failed", "cause": "not_found",
                 "class": "configuration", "status": 404, "authenticated": None,
                 "model_found": False, "request_accepted": None, "answer_valid": None,
                 "reason": "the endpoint answered 404 Not Found", "tokens_in": None,
                 "tokens_out": None})
    if not isinstance(line["hint"], str) or not line["hint"]:
        fail("the endpoint check line has no hint")
for key, value in want.items():
    if json.dumps(line[key]) != json.dumps(value):
        fail("%s is %s, not %s" % (key, json.dumps(line[key]), json.dumps(value)))
latency = line["latency_ms"]
if not isinstance(latency, int) or isinstance(latency, bool) or latency < 0:
    fail("latency_ms is not a whole number")
if not isinstance(line["prompt_version"], str) or not line["prompt_version"]:
    fail("prompt_version is not a string")

golden = json.load(open(GOLDEN, encoding="utf-8"))
golden["model"] = model
new = open(work + "/requests.jsonl", "rb").read().splitlines()[int(before):]
same = 0
for text in new:
    try:
        same += json.loads(text) == golden
    except ValueError:
        pass
if same != 1:
    fail("%d of the %d requests the endpoint received during it are the synthetic one of %s"
         % (same, len(new), GOLDEN))
print("helm test with model %s: result %s, cause %s, %d ms; the synthetic request, once"
      % (model, line["result"], json.dumps(line["cause"]), latency))
EOF
}
# Runs helm test --logs on the release, whose model is $2, and checks it with check_endpoint_line
# $1 $2: it must exit 0 with $1 = ok and fail with $1 = not_found. Its one request is counted in
# CHECK_REQUESTS. On a failure, helm's output is printed: it holds no token.
endpoint_check() {
  before=$(lines_of "$WORK/requests.jsonl")
  if h test "$RELEASE" -n "$NAMESPACE" --logs --timeout 180s > "$WORK/helm-test.txt" 2>&1; then
    check_status=0
  else
    check_status=$?
  fi
  CHECK_REQUESTS=$((CHECK_REQUESTS + 1))
  if [ "$1" = ok ] && [ "$check_status" != 0 ]; then
    cat "$WORK/helm-test.txt" >&2
    fail "helm test exited $check_status with model $2"
  fi
  if [ "$1" != ok ] && [ "$check_status" = 0 ]; then
    cat "$WORK/helm-test.txt" >&2
    fail "helm test passed with model $2"
  fi
  check_endpoint_line "$1" "$2" "$before" || { cat "$WORK/helm-test.txt" >&2; exit 1; }
}
# The value of series $1 in the metrics in file $2, empty when absent.
metric_of() { awk -v series="$1" '$1 == series { print $2 }' "$2"; }
# "yes" when the metrics in file $1 have exactly one kausa_endpoint_state series at 1, state $2,
# and the other two at 0.
endpoint_state_is() {
  awk -v want="$2" '
    $1 ~ /^kausa_endpoint_state[{]state="[a-z_]+"[}]$/ {
      state = $1
      sub(/^kausa_endpoint_state[{]state="/, "", state)
      sub(/"[}]$/, "", state)
      if (!(state in seen)) n++
      seen[state] = $2
    }
    END {
      ok = ("closed" in seen) && ("half_open" in seen) && ("open" in seen) && n == 3
      for (state in seen) ok = ok && seen[state] == (state == want ? 1 : 0)
      print (ok ? "yes" : "no")
    }' "$1"
}
