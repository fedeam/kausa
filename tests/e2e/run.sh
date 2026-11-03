#!/usr/bin/env bash
# End-to-end test: kausa installed with its Helm chart in a kind cluster, against a fake model
# endpoint, with six seeded failures. It checks the Events, what left the cluster, the RBAC
# objects and what they grant, and the metrics. Needs docker, kind, kubectl, helm and python3;
# KIND_NODE_IMAGE names the node image (pinned in .github/workflows/e2e.yml). It creates the
# cluster kausa-e2e and deletes it at the end. See docs/how-to/develop.md.
# The helpers live in tests/e2e/lib.sh, which tests/e2e/upgrade.sh shares.
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
#
# Both also install with notifications in the json format (docs/adr/0015-notifications.md), from
# a Secret whose URL points at the fake endpoint's /hook/ path with a fake Telegram-shaped bot
# token in it, and a fake bearer token. The fake receiver answers the first notification 503 and
# every later one 200; before the first restart the test checks one v1 body for each failing
# workload, what no body may hold, the bearer token, the counters, the started line, and that
# kausa's log holds neither token. The dryRun=true upgrade turns notifications off.
#
# The fake endpoint's answer carries an injected step with a fake address, query token and host
# (docs/adr/0022-redact-the-answer.md). Both check that every Explanation and notification holds
# it redacted, that none of those values reached an Event note, an Explanation entry, a
# notification or an explanation log line, and that kausa_answer_redactions_total counts R004,
# R013 and R014 and never R002.
#
# Both watch Deployments (docs/adr/0018-stuck-rollouts.md): stalled, held by a scheduling gate, is
# explained by D014 alone, and the request for it carries only the replica counts; crasher and
# refused also pass their progress deadline, and before the first restart (after one, refused's
# FailedCreate events may be stale) they keep their D001 and D011, with no D014. In namespaced
# scope, nothing about the same Deployments in e2e-other. kausa may list and watch Deployments
# and nothing more (it is Ready though it cannot get one), the e2e-denied step also needs a
# deployment watch error, and the cluster scope's history.enabled=false upgrade also sets
# rollouts=false: no deployments rule, --no-rollouts in the args, a Ready Pod whose started line
# has rollouts false, kept by the later upgrades through --reuse-values.
#
# The runs install with budget.callsPerHour=3600 (docs/adr/0021-refill-the-call-budget.md): after a
# start burst of 3, one call a second. Before the dryRun=true upgrade, the cluster scope checks the
# budget across restarts: callsPerHour=360 and settleSeconds=5, 20 crash-looping Deployments in
# e2e-budget, and kausa's Pod deleted every 60 s for 10 minutes. Each life runs from its process's
# started line to its stopped line, and no two overlap. The fake endpoint's receive times stay
# within 3 calls per start plus one per 10 s, in total and in each life; the queue gauge rises
# above 0, the hour gauge reads at most the burst right after a start, a life logs explanations
# waiting, and every explanation dropped line says whether it was waiting for the budget.
#
# KAUSA_E2E_REPLICAS picks the chart's replicaCount (docs/adr/0019-high-availability.md):
# - 1 (when unset): replicaCount stays unset. Nothing of the standby is installed (no Lease, no
#   <release>-leader Role or RoleBinding, no PodDisruptionBudget), the Pod runs without
#   --leader-lease, nobody may get, patch or create a Lease, and the Pod reports kausa_leader 1,
#   /readyz ready and a null leader_lease in its started line.
# - 2, with KAUSA_E2E_SCOPE=namespaced only: two Pods, one holding the Lease <release>-leader. Every
#   read that depends on the leader (its log, its metrics, its restarts) goes to the holder's Pod,
#   metrics through the pod proxy: the Service balances across both Ready Pods. It skips the steps
#   that assume one Pod (the --reset-values refusal, which is scope logic the other namespaced run
#   covers, and the e2e-denied readiness block) and checks instead: one leader and one standby,
#   with their gauge, /readyz and log lines; get and patch on that one Lease and nothing else on
#   Leases, with a real server-side create refused; that only the leader writes Events and the
#   standby logs no incident; a graceful handover (lease released, takeover within 10 s,
#   leaseTransitions + 1); a frozen leader (SIGSTOP from the kind node: takeover within 45 s, then
#   SIGCONT, leadership lost, no restart and no write after the takeover); a deleted Lease
#   RoleBinding (nobody leads, nothing is written or sent, helm upgrade restores one leader); a
#   foreign label write that keeps the holder; a rollout that ends with one leader; and an uninstall
#   that leaves no Lease, Role, RoleBinding or PodDisruptionBudget.
#
# Every run checks the endpoint check (docs/adr/0020-check-the-endpoint.md): helm test --logs
# passes against the fake endpoint, its Pod admitted under the restricted Pod Security Standard,
# with one endpoint check line and one request, the synthetic body of
# tests/golden/check-endpoint-request-json-schema.json, which kausa does not count as a model call.
# After an upgrade to the model e2e-missing, which the fake endpoint answers 404, helm test fails
# with cause not_found; the next upgrade sets the model back. After the budget case, the cluster
# scope checks the breaker: with the fake endpoint answering 401 and 18 failing Jobs (D008), the
# first call opens it, no request reaches the endpoint during the 60 s hold, kausa_endpoint_state
# reads open and endpoint unavailable is logged, and each incident the full queue skips while it
# holds gets one ExplanationSkipped Event with the fixed note, as many as the endpoint_unavailable
# counter; with the 401s gone, the probe closes it (endpoint available, the gauge reads closed) and
# the queue drains.
set -euo pipefail

. tests/e2e/lib.sh

SCOPE=${KAUSA_E2E_SCOPE:-cluster}
case "$SCOPE" in
  cluster | namespaced) ;;
  *) fail "KAUSA_E2E_SCOPE must be cluster or namespaced" ;;
esac
REPLICAS=${KAUSA_E2E_REPLICAS:-1}
case "$REPLICAS" in
  1 | 2) ;;
  *) fail "KAUSA_E2E_REPLICAS must be 1 or 2" ;;
esac
# The cluster scope's later steps (its upgrades and restarts) assume one Pod.
if [ "$REPLICAS" = 2 ] && [ "$SCOPE" != namespaced ]; then
  fail "KAUSA_E2E_REPLICAS=2 runs with KAUSA_E2E_SCOPE=namespaced only"
fi

trap cleanup EXIT

: "${KIND_NODE_IMAGE:?set KIND_NODE_IMAGE to a pinned kindest/node image}"
echo "e2e: rbac.scope=$SCOPE"
kind create cluster --name "$CLUSTER" --image "$KIND_NODE_IMAGE" --wait 120s
docker build --target runtime -t kausa:e2e .
kind load docker-image kausa:e2e --name "$CLUSTER"

start_endpoint

# The install's values other than rbac, reused by the upgrade that must fail. The hourly budget
# of 3600 refills one call a second after the start burst of 3, so that incidents wait in the
# queue for at most a few seconds and the runs below see every explanation in time.
values=(--set image.repository=kausa --set image.tag=e2e --set image.pullPolicy=Never
  --set model.endpoint="http://$gateway:$PORT/v1" --set model.name=fake
  --set model.allowHttp=true --set settleSeconds="$SETTLE" --set history.enabled=true
  --set budget.callsPerHour=3600
  --set notifications.enabled=true --set notifications.format=json
  --set notifications.existingSecret=kausa-notify --set notifications.tokenKey=token
  --set notifications.allowHttp=true)
# The other runs leave replicaCount unset: the chart's default.
[ "$REPLICAS" = 1 ] || values+=(--set replicaCount=2)

# The restricted Pod Security Standard: the chart's Pod must be admitted under it.
k create namespace "$NAMESPACE"
k label namespace "$NAMESPACE" pod-security.kubernetes.io/enforce=restricted
# The Secret the chart mounts for the notifications, from files: no token in a command line.
k -n "$NAMESPACE" create secret generic kausa-notify --from-file=url="$WORK/notify-url" \
  --from-file=token="$WORK/notify-token" >/dev/null
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

# The standby replica (docs/adr/0019-high-availability.md), before anything else touches the Pods.
if [ "$REPLICAS" = 1 ]; then
  # One replica: nothing of the standby is installed, and the one Pod acts.
  found=$(k get leases,poddisruptionbudgets -A -l "app.kubernetes.io/instance=$RELEASE" -o name)
  [ -z "$found" ] || fail "with one replica, the release installed: $found"
  found=$(k -n "$NAMESPACE" get "lease/$LEASE" "role/$LEASE" "rolebinding/$LEASE" \
    --ignore-not-found -o name)
  found="$found$(k -n "$NAMESPACE" get poddisruptionbudgets -o name)"
  [ -z "$found" ] || fail "with one replica, $NAMESPACE holds: $found"
  pod=$(k -n "$NAMESPACE" get pods -l app.kubernetes.io/name=kausa -o name)
  # Read once, then searched: with pipefail, a grep -q that stops reading early can fail the pipe.
  args=$(kausa_args "$pod")
  if printf '%s\n' "$args" | grep -qxF -- --leader-lease; then
    fail "the kausa Pod runs with --leader-lease with one replica"
  fi
  metrics=$(kausa_metrics)
  printf '%s\n' "$metrics" | grep -qxF 'kausa_leader 1' ||
    fail "kausa_leader is not 1 with one replica"
  readyz=$(k get --raw "/api/v1/namespaces/$NAMESPACE/services/kausa-metrics:9090/proxy/readyz")
  [ "$readyz" = ready ] || fail "/readyz answers '$readyz' with one replica"
  kausa_log
  lease=$(started_field leader_lease)
  [ "$lease" = null ] || fail "the started line has leader_lease $lease with one replica"
  echo "one replica: no Lease, leader Role or RoleBinding, PodDisruptionBudget or --leader-lease;" \
    "kausa_leader 1, /readyz ready"
else
  # Two Ready Pods, one leader: kausa_leader 1 and /readyz ready on the holder, 0 and standby on
  # the other; both run with the --leader-lease pair and name the Lease in their started line;
  # exactly one leading line across both logs, and a standby line in the standby's.
  wait_pair 60 "no two Ready kausa Pods with one of them holding the Lease after the install"
  first_leader=$leader first_standby=$standby
  [ "$(pod_metric "$leader" kausa_leader)" = 1 ] || fail "kausa_leader is not 1 on the Lease holder"
  [ "$(pod_metric "$standby" kausa_leader)" = 0 ] || fail "kausa_leader is not 0 on the standby"
  [ "$(pod_get "$leader" readyz)" = ready ] || fail "/readyz of the Lease holder is not 'ready'"
  [ "$(pod_get "$standby" readyz)" = standby ] || fail "/readyz of the standby is not 'standby'"
  leading=0
  for p in "$leader" "$standby"; do
    args=$(kausa_args "pod/$p")
    [ "$(printf '%s\n' "$args" | grep -cxF -- --leader-lease)" = 1 ] ||
      fail "a kausa Pod does not run with exactly one --leader-lease"
    printf '%s\n' "$args" | awk -v want="$NAMESPACE/$LEASE" '
      prev == "--leader-lease" && $0 == want { ok = 1 } { prev = $0 } END { exit !ok }' ||
      fail "a kausa Pod's --leader-lease is not followed by $NAMESPACE/$LEASE"
    kausa_log "pod/$p"
    lease=$(started_field leader_lease)
    [ "$lease" = "\"$NAMESPACE/$LEASE\"" ] || fail "a started line has leader_lease $lease"
    leading=$((leading + $(log_count leading < "$WORK/kausa.log")))
  done
  [ "$leading" = 1 ] || fail "$leading leading lines across both Pods' logs"
  [ "$(log_count leading < "$WORK/kausa.log")" = 0 ] || fail "the standby logged leading"
  [ "$(log_count standby < "$WORK/kausa.log")" -ge 1 ] || fail "the standby logged no standby line"
  k -n "$NAMESPACE" get "poddisruptionbudget/$RELEASE" -o name >/dev/null
  echo "two replicas: two Ready Pods, one holds the Lease; kausa_leader 1 and 0, /readyz ready" \
    "and standby, one leading line"
fi

for _ in $(seq 1 60); do
  deployment_note=$(explained Deployment crasher)
  job_note=$(explained Job failing-job)
  policy_note=$(explained Job policy-job)
  stranded_note=$(explained Deployment stranded)
  stalled_note=$(explained Deployment stalled)
  refused_note=$(explained Deployment refused)
  [ -n "$deployment_note" ] && [ -n "$job_note" ] && [ -n "$policy_note" ] &&
    [ -n "$stranded_note" ] && [ -n "$stalled_note" ] && [ -n "$refused_note" ] && break
  sleep 5
done
[ -n "$deployment_note" ] || fail "no Explained Event about Deployment crasher"
[ -n "$job_note" ] || fail "no Explained Event about Job failing-job"
[ -n "$policy_note" ] || fail "no Explained Event about Job policy-job"
[ -n "$stranded_note" ] || fail "no Explained Event about Deployment stranded"
[ -n "$stalled_note" ] || fail "no Explained Event about Deployment stalled"
[ -n "$refused_note" ] || fail "no Explained Event about Deployment refused"
echo "Deployment crasher: $deployment_note"
echo "Job failing-job: $job_note"
echo "Job policy-job: $policy_note"
echo "Deployment stranded: $stranded_note"
echo "Deployment stalled: $stalled_note"
echo "Deployment refused: $refused_note"

# The model named namespace-1; the Event names the real namespace.
case "$deployment_note" in *"$TARGET"*) ;; *) fail "the placeholder was not mapped back" ;; esac

if [ "$SCOPE" = namespaced ]; then
  # The failures in OTHER were seeded with TARGET's: one more settle window and tick, and kausa
  # still has neither explained nor logged them. Its stalled and refused are past their deadline
  # first, so that D014 had its chance there too.
  k -n "$OTHER" wait deploy/stalled deploy/refused --timeout=180s \
    --for=jsonpath='{.status.conditions[?(@.type=="Progressing")].reason}'=ProgressDeadlineExceeded
  sleep $((SETTLE + TICK + 5))
  [ "$(kausa_events "$OTHER")" = 0 ] || fail "kausa wrote an Event in $OTHER"
  kausa_log
  if log_has incident "$OTHER" < "$WORK/kausa.log"; then
    fail "kausa logged an incident in $OTHER"
  fi
  echo "$OTHER: no Event and no incident, stalled and refused included"
fi

# Each failing workload's Explanation (src/detect.rs: D001 is a crash loop, D008 a failed Job).
explanations_of Deployment crasher D001 apps/v1
deployment_history=$history_names
explanations_of Job failing-job D008 batch/v1
echo "Explanations: $deployment_history $history_names"
# policy-job failed by its podFailurePolicy, with retries left: D008 with the reason
# PodFailurePolicy, not BackoffLimitExceeded.
explanations_of Job policy-job D008 batch/v1
echo "Explanations: $history_names (policy-job)"
# policy-job's incidents, in the log explanations_of read: D008, about the Job, with the Job
# controller's PodFailurePolicy note as the sample, and no other rule about it.
python3 -c '
import json, sys
target, work = sys.argv[1:3]

def fail(message):
    sys.exit("FAIL: the D008 incident of policy-job: " + message)

owner = {"apiVersion": "batch/v1", "kind": "Job", "name": "policy-job"}
found = []
for line in open(work + "/kausa.log"):
    try:
        entry = json.loads(line)
    except ValueError:
        continue
    if (isinstance(entry, dict) and entry.get("msg") == "incident"
            and entry.get("namespace") == target and entry.get("owner") == owner):
        found.append(entry)
if not found:
    fail("none in the log")
for entry in found:
    sample = entry.get("sample") or {}
    if entry.get("detection") != "D008":
        fail("an incident about it has detection %s" % json.dumps(entry.get("detection")))
    if sample.get("reason") != "PodFailurePolicy":
        fail("sample.reason %s" % json.dumps(sample.get("reason")))
    message = sample.get("message")
    if not isinstance(message, str) or "matching FailJob rule at index 0" not in message:
        fail("sample.message does not say matching FailJob rule at index 0")
print("policy-job: %d D008 incident, sample.reason PodFailurePolicy" % len(found))
' "$TARGET" "$WORK"
# The stalled rollouts (docs/adr/0018-stuck-rollouts.md): stalled is explained by D014, the only
# rule its Pod meets, and refused by D011, its ReplicaSet's FailedCreate.
explanations_of Deployment stalled D014 apps/v1
stalled_history=$history_names
explanations_of Deployment refused D011 apps/v1
echo "Explanations: $stalled_history (stalled) $history_names (refused)"
# stalled's incident, in the log explanations_of read: about the Deployment, with no Pod, and the
# rollout's revision as its message.
python3 -c '
import json, re, sys
target, work = sys.argv[1:3]

def fail(message):
    sys.exit("FAIL: the D014 incident of stalled: " + message)

owner = {"apiVersion": "apps/v1", "kind": "Deployment", "name": "stalled"}
found = []
for line in open(work + "/kausa.log"):
    try:
        entry = json.loads(line)
    except ValueError:
        continue
    if (isinstance(entry, dict) and entry.get("msg") == "incident"
            and entry.get("detection") == "D014" and entry.get("namespace") == target
            and entry.get("owner") == owner):
        found.append(entry)
if not found:
    fail("none in the log")
for entry in found:
    sample = entry.get("sample") or {}
    if entry.get("detection_name") != "rollout does not progress":
        fail("detection_name %s" % json.dumps(entry.get("detection_name")))
    if entry.get("pods") != []:
        fail("it names %d Pods" % len(entry.get("pods") or []))
    if sample.get("reason") != "ProgressDeadlineExceeded":
        fail("sample.reason %s" % json.dumps(sample.get("reason")))
    message = sample.get("message")
    if not isinstance(message, str) or not re.fullmatch(r"(revision [0-9]+)?", message):
        fail("sample.message %s" % json.dumps(message))
    if [sample.get(k) for k in ("pod", "container", "exit_code")] != [None, None, None]:
        fail("its sample names a Pod, a container or an exit code")
print("stalled: %d D014 incident, %s" % (len(found), json.dumps(found[0]["sample"]["message"])))
' "$TARGET" "$WORK"
rollouts=$(started_field rollouts)
[ "$rollouts" = true ] || fail "the started line has rollouts $rollouts"

# crasher and refused pass their 20 s progress deadline too, and D014 gives way to their D001 and
# D011: a settle window and two ticks after both are past it, kausa has logged no D014 about either
# and has no D014 Event or Explanation for them, and both keep their own incident. Only before the
# first restart: after one, refused's FailedCreate events may be stale (they stop being updated
# after about 11 minutes), and a D014 is then the documented behaviour.
k -n "$TARGET" wait deploy/crasher deploy/refused --timeout=180s \
  --for=jsonpath='{.status.conditions[?(@.type=="Progressing")].reason}'=ProgressDeadlineExceeded
sleep $((SETTLE + 2 * TICK + 5))
k -n "$TARGET" get events.events.k8s.io -o json > "$WORK/events.json"
k -n "$TARGET" get "$EXPLANATIONS" -o json > "$WORK/explanations.json"
kausa_log
python3 -c '
import json, re, sys
target, work = sys.argv[1:3]

def fail(message):
    sys.exit("FAIL: D014 does not give way: " + message)

incidents = {}
for line in open(work + "/kausa.log"):
    try:
        entry = json.loads(line)
    except ValueError:
        continue
    if (isinstance(entry, dict) and entry.get("msg") == "incident"
            and entry.get("namespace") == target):
        incidents[entry.get("fingerprint")] = entry
for name, kept in (("crasher", "D001"), ("refused", "D011")):
    owner = {"apiVersion": "apps/v1", "kind": "Deployment", "name": name}
    rules = sorted({e.get("detection") for e in incidents.values() if e.get("owner") == owner})
    if "D014" in rules:
        fail("kausa logged a D014 incident about %s" % name)
    if kept not in rules:
        fail("%s has no %s incident in the log, only %s" % (name, kept, rules))
    for event in json.load(open(work + "/events.json"))["items"]:
        if (event.get("reportingController") != "kausa"
                or event["regarding"].get("kind") != "Deployment"
                or event["regarding"].get("name") != name):
            continue
        # src/events.rs: kausa.<fingerprint>.<hash>, or kausa.skipped.<fingerprint>.<hash>.
        match = re.match(r"kausa\.(?:skipped\.)?([0-9a-f]{16})\.", event["metadata"]["name"])
        incident = incidents.get(match.group(1)) if match else None
        if incident is None or incident.get("detection") == "D014":
            fail("the Event %s about %s is no %s incident" % (
                event["metadata"]["name"], name, kept))
for item in json.load(open(work + "/explanations.json"))["items"]:
    spec = item.get("spec") or {}
    if spec.get("rule") == "D014" and (spec.get("target") or {}).get("name") in (
            "crasher", "refused"):
        fail("the Explanation %s" % item["metadata"]["name"])
print("crasher and refused: past their deadline, no D014; D001 and D011 kept")
' "$TARGET" "$WORK"
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
# One line, all names on it: tests/chart/render.sh reads the list from it.
for name in "$TARGET" "$OTHER" crasher failing-job policy-job e2e-policy-task stranded stalled refused
do
  if grep -q -- "$name" "$WORK/requests.jsonl"; then fail "a request contains '$name'"; fi
done
# The richer context (docs/adr/0017-richer-context-for-the-model.md): crasher's probe path without
# its query, and stranded's node selector and toleration keys without their values.
for word in /e2e-ready example.com/e2e-pool example.com/e2e-taint; do
  grep -qF -- "$word" "$WORK/requests.jsonl" || fail "no request contains '$word'"
done
for word in e2e-query-value e2e-pool-value e2e-toleration-value; do
  if grep -qF -- "$word" "$WORK/requests.jsonl"; then fail "a request contains '$word'"; fi
done
# Each rule gets only its own fields: the D001 request (crasher) has the probe's handler, port and
# path and none of D002's or D005's; the D005 request (stranded) has the scheduling keys, no pull
# secrets and no probe path. Failure messages name the field, never what the request holds.
python3 -c '
import json, sys

def fail(message):
    sys.exit("FAIL: " + message)

def keys(value):
    if isinstance(value, dict):
        for key, inner in value.items():
            yield key
            yield from keys(inner)
    elif isinstance(value, list):
        for inner in value:
            yield from keys(inner)

contexts = {}
for line in open(sys.argv[1]):
    try:
        request = json.loads(line)
    except ValueError:
        fail("requests.jsonl holds a line that is not JSON")
    for message in (request.get("messages") or []) if isinstance(request, dict) else []:
        if not isinstance(message, dict) or message.get("role") != "user":
            continue
        try:
            context = json.loads(message["content"])["untrusted_cluster_data"]
            rule = context["detection"]["id"]
        except (ValueError, KeyError, TypeError):
            continue
        contexts.setdefault(rule, []).append(context)

probe = {"kind": "readiness", "handler": "http", "port": 8080, "path": "/e2e-ready",
         "initial_delay_seconds": 3600}
if not contexts.get("D001"):
    fail("no D001 request")
for context in contexts["D001"]:
    found = set(keys(context))
    for key in ("scheduling", "image_pull_policy", "image_pull_secrets"):
        if key in found:
            fail("a D001 request has the key " + key)
    probes = [p for pod in context.get("pods") or [] for c in pod.get("containers") or []
              for p in c.get("probes") or []]
    if not any(all(p.get(k) == v for k, v in probe.items()) for p in probes):
        fail("a D001 request has no readiness probe with handler http, port 8080 and its path")

selector = {"key": "example.com/e2e-pool", "value": None}
toleration = {"key": "example.com/e2e-taint", "operator": "Equal", "effect": "NoSchedule"}
if not contexts.get("D005"):
    fail("no D005 request")
for context in contexts["D005"]:
    found = set(keys(context))
    for key in ("image_pull_secrets", "path"):
        if key in found:
            fail("a D005 request has the key " + key)
    scheduling = context.get("scheduling")
    if not isinstance(scheduling, dict):
        fail("a D005 request has no scheduling object")
    if selector not in (scheduling.get("node_selector") or []):
        fail("a D005 request lacks the node selector key example.com/e2e-pool")
    if toleration not in (scheduling.get("tolerations") or []):
        fail("a D005 request lacks the toleration example.com/e2e-taint")
print("requests: %d D001 and %d D005 with their own fields"
      % (len(contexts["D001"]), len(contexts["D005"])))
' "$WORK/requests.jsonl"
# The D008 request for policy-job carries the Job controller's PodFailurePolicy note, with the rule
# it matched and without the Job's, the Pod's or the container's name (R003). Failure messages
# name the check, never what the request holds.
python3 -c '
import json, sys

def fail(message):
    sys.exit("FAIL: the PodFailurePolicy request: " + message)

found = 0
for line in open(sys.argv[1]):
    try:
        request = json.loads(line)
    except ValueError:
        fail("requests.jsonl holds a line that is not JSON")
    for message in (request.get("messages") or []) if isinstance(request, dict) else []:
        if not isinstance(message, dict) or message.get("role") != "user":
            continue
        try:
            context = json.loads(message["content"])["untrusted_cluster_data"]
            rule = context["detection"]["id"]
        except (ValueError, KeyError, TypeError):
            continue
        if rule != "D008":
            continue
        for event in context.get("warning_events") or []:
            if not isinstance(event, dict) or event.get("reason") != "PodFailurePolicy":
                continue
            found += 1
            note = event.get("message")
            if not isinstance(note, str):
                fail("a PodFailurePolicy event has no message")
            if "matching FailJob rule at index 0" not in note:
                fail("a PodFailurePolicy message lacks the matched rule")
            for name in ("policy-job", "e2e-policy-task"):
                if name in note:
                    fail("a PodFailurePolicy message names " + name)
if not found:
    fail("no D008 request has a warning event with the reason PodFailurePolicy")
print("the PodFailurePolicy request: %d event, the rule kept, no real name" % found)
' "$WORK/requests.jsonl"
# The request for stalled's D014 carries the rollout's replica counts and nothing else of it: no
# Pod, no log tail, and no text (docs/reference/data-sent-to-the-provider.md).
python3 -c '
import json, sys

def fail(message):
    sys.exit("FAIL: the D014 request: " + message)

KEYS = ["available_replicas", "min_ready_seconds", "progress_deadline_seconds", "ready_replicas",
        "replicas", "unavailable_replicas", "updated_replicas"]
contexts = []
for line in open(sys.argv[1]):
    request = json.loads(line)
    for message in request.get("messages") or []:
        if message.get("role") != "user":
            continue
        try:
            context = json.loads(message["content"])["untrusted_cluster_data"]
        except (ValueError, KeyError, TypeError):
            continue
        if (context.get("detection") or {}).get("id") == "D014":
            contexts.append(context)
if not contexts:
    fail("the fake endpoint received none")
for context in contexts:
    rollout = context.get("rollout")
    if not isinstance(rollout, dict) or sorted(rollout) != KEYS:
        fail("rollout is not an object with the keys %s" % KEYS)
    for key, value in rollout.items():
        if value is not None and (not isinstance(value, int) or isinstance(value, bool)):
            fail("rollout.%s is not an integer or null" % key)
    if context.get("pods") != []:
        fail("pods is not empty")
    if "log_tail" in context:
        fail("it has a log_tail")
    if (context.get("workload") or {}).get("kind") != "Deployment":
        fail("workload.kind is %s" % json.dumps((context.get("workload") or {}).get("kind")))
print("the D014 request: %d, a rollout of %d counts, no Pod and no log tail" % (
    len(contexts), len(KEYS)))
' "$WORK/requests.jsonl"

# The notifications (docs/adr/0015-notifications.md). The fake receiver answered the first one 503,
# which kausa sends again 30 seconds later, and one per failing workload is due: crasher,
# failing-job, policy-job, stranded, stalled and refused.
hooks_answered() {
  python3 -c '
import json, sys
count = 0
for line in open(sys.argv[1]):
    try:
        count += json.loads(line)["status"] == 200
    except (ValueError, KeyError, TypeError):
        pass
print(count)
' "$WORK/hooks.jsonl"
}
answered=0
for _ in $(seq 1 60); do
  answered=$(hooks_answered)
  [ "$answered" -ge 6 ] && break
  sleep 2
done
[ "$answered" -ge 6 ] || fail "the webhook answered $answered notifications 200 in 120 seconds"
k -n "$TARGET" get events.events.k8s.io -o json > "$WORK/events.json"
k -n "$TARGET" get "$EXPLANATIONS" -o json > "$WORK/explanations.json"
kausa_log
# First, so that no later message can print an injected value.
no_injected_values
# What reached the webhook: the first POST got 503, every one carried the bearer token and went to
# the URL's path, and each body is a v1 notification with only the documented fields. Exactly one
# answered 200 for each failing workload, with the real namespace, the rule, and the fingerprint
# and first_seen of the incident kausa logged and explained, and the fake endpoint's answer with
# its second step redacted. No body holds the context sent to the model, a log-tail line the model
# requests carried, a placeholder other than R013's host-N (never mapped back, by design) or the
# unlisted namespace, and requests.jsonl holds no notification. The started line names the format
# and the host, and kausa's log holds neither token.
python3 -c '
import json, re, sys
sys.path.insert(0, "tests/e2e")
from fake_endpoint import ANSWER, REDACTED_STEP
target, other, work, gateway, path, bearer, bot = sys.argv[1:8]

def fail(message):
    sys.exit("FAIL: notifications: " + message)

def strings(value):
    if isinstance(value, str):
        yield value
    elif isinstance(value, list):
        for item in value:
            yield from strings(item)
    elif isinstance(value, dict):
        for key, item in value.items():
            yield key
            yield from strings(item)

SCHEMA = "kausa.fedeam.com/notification/v1"
FIELDS = ["detection", "explanation", "fingerprint", "first_seen", "kausa_version", "object",
          "schema", "type"]
PLACEHOLDER = re.compile(r"\b[a-z]+-[0-9]+\b", re.IGNORECASE)
# The placeholders of R013, the only ones a notification may hold (src/redact.rs never restores
# them).
HOST = re.compile(r"host-[0-9]+")
hooks = [json.loads(line) for line in open(work + "/hooks.jsonl")]
if not hooks or hooks[0]["status"] != 503:
    fail("the first POST to the webhook was not answered 503")
if [h["status"] for h in hooks[1:] if h["status"] != 200]:
    fail("the fake receiver answered a later POST with something other than 200")

log = open(work + "/kausa.log").read()
for name, token in (("bot", bot), ("bearer", bearer)):
    if token in log:
        fail("kausa logged the fake %s token" % name)
entries = []
for line in log.splitlines():
    try:
        entry = json.loads(line)
    except ValueError:
        continue
    if isinstance(entry, dict):
        entries.append(entry)
incidents = {e.get("fingerprint"): e for e in entries if e.get("msg") == "incident"}
started = [e for e in entries if e.get("msg") == "started"]
if len(started) != 1:
    fail("%d started lines in the log" % len(started))
if started[0].get("notifications") != {"format": "json", "host": gateway}:
    fail("the started line has notifications %s" % json.dumps(started[0].get("notifications")))
version = started[0].get("version")

# The log-tail lines the model requests carried, which no notification may hold.
tails = set()
for line in open(work + "/requests.jsonl"):
    request = json.loads(line)
    if not isinstance(request, dict) or "messages" not in request or "schema" in request:
        fail("requests.jsonl holds something other than a request to the model")
    for message in request["messages"]:
        if message.get("role") != "user":
            continue
        try:
            context = json.loads(message["content"])["untrusted_cluster_data"]
        except (ValueError, KeyError, TypeError):
            continue
        for tail_line in (context.get("log_tail") or "").splitlines():
            if len(tail_line.strip()) >= 12:
                tails.add(tail_line.strip())
if not tails:
    fail("no request to the model carried a log-tail line to look for")

events = json.load(open(work + "/events.json"))["items"]
explained = {}
for event in events:
    if event.get("reportingController") != "kausa" or event.get("reason") != "Explained":
        continue
    match = re.match(r"kausa\.([0-9a-f]{16})\.", event["metadata"]["name"])
    if match:
        key = (event["regarding"]["kind"], event["regarding"]["name"])
        explained.setdefault(key, set()).add(match.group(1))

bodies = []
for hook in hooks:
    if hook["path"] != path:
        fail("a notification went to another path than the URL'"'"'s")
    if hook["authorization"] != "Bearer " + bearer:
        fail("a notification without the bearer token in its Authorization header")
    text = hook["body"]
    try:
        body = json.loads(text)
    except ValueError:
        fail("a notification body is not JSON")
    if not isinstance(body, dict) or sorted(body) != FIELDS or body["schema"] != SCHEMA:
        fail("a notification body is not a v1 notification with only its fields: %s" % text)
    if (sorted(body["detection"]) != ["id", "name"]
            or sorted(body["object"]) != ["apiVersion", "kind", "name", "namespace"]
            or sorted(body["explanation"]) != ["cause", "confidence", "steps", "summary"]):
        fail("a notification body has other fields: %s" % text)
    every = [text] + list(strings(body))
    for word in ["untrusted_cluster_data", "log_tail", other] + sorted(tails):
        if any(word in s for s in every):
            fail("a notification holds %s" % json.dumps(word))
    for s in every:
        for placeholder in PLACEHOLDER.finditer(s):
            if not HOST.fullmatch(placeholder.group()):
                fail("a notification holds the placeholder " + placeholder.group())
    if hook["status"] == 200:
        bodies.append(body)

explanation = {"summary": ANSWER["summary"].replace("namespace-1", target),
               "cause": ANSWER["cause"], "confidence": ANSWER["confidence"]}
for body in bodies:
    if body["object"]["namespace"] != target:
        fail("a notification about namespace " + body["object"]["namespace"])
for kind, name, rule, api_version in (("Deployment", "crasher", "D001", "apps/v1"),
                                      ("Job", "failing-job", "D008", "batch/v1"),
                                      ("Job", "policy-job", "D008", "batch/v1"),
                                      ("Deployment", "stranded", "D005", "apps/v1"),
                                      ("Deployment", "stalled", "D014", "apps/v1")):
    mine = [b for b in bodies if (b["object"]["kind"], b["object"]["name"]) == (kind, name)]
    if len(mine) != 1:
        fail("%d notifications answered 200 about %s %s" % (len(mine), kind, name))
    body = mine[0]
    owner = {"apiVersion": api_version, "kind": kind, "name": name}
    if body["type"] != "explained" or body["kausa_version"] != version:
        fail("%s %s: type %s, kausa_version %s" % (kind, name, body["type"],
                                                   body["kausa_version"]))
    if body["object"] != dict(owner, namespace=target):
        fail("%s %s: object %s" % (kind, name, json.dumps(body["object"])))
    incident = incidents.get(body["fingerprint"])
    if incident is None:
        fail("%s %s: no incident with fingerprint %s" % (kind, name, body["fingerprint"]))
    if (incident.get("detection"), incident.get("namespace"), incident.get("owner")) != (
            rule, target, owner):
        fail("%s %s: the log gives its fingerprint another incident" % (kind, name))
    if body["detection"] != {"id": rule, "name": incident.get("detection_name")}:
        fail("%s %s: detection %s" % (kind, name, json.dumps(body["detection"])))
    if body["first_seen"] != incident.get("first_seen"):
        fail("%s %s: first_seen %s, not the incident'"'"'s %s" % (
            kind, name, body["first_seen"], incident.get("first_seen")))
    if body["fingerprint"] not in explained.get((kind, name), set()):
        fail("%s %s: no Explained Event with fingerprint %s" % (kind, name, body["fingerprint"]))
    if {k: v for k, v in body["explanation"].items() if k != "steps"} != explanation:
        fail("%s %s: explanation %s" % (kind, name, json.dumps(body["explanation"])))
    # The first step as sent, the second redacted (docs/adr/0022-redact-the-answer.md).
    steps = body["explanation"]["steps"]
    if (not isinstance(steps, list) or len(steps) != 2 or steps[0] != ANSWER["steps"][0]
            or not isinstance(steps[1], str) or not re.fullmatch(REDACTED_STEP, steps[1])):
        fail("%s %s: the steps are not the answer'"'"'s, the second one redacted" % (kind, name))
print("notifications: %d POSTs, the first answered 503; one v1 body each for crasher, "
      "failing-job, policy-job, stranded and stalled" % len(hooks))
' "$TARGET" "$OTHER" "$WORK" "$gateway" "$HOOK_PATH" "$FAKE_BEARER" "$FAKE_BOT"
# The counters, once kausa has handled the answers the receiver logged.
counted=""
for _ in $(seq 1 10); do
  metrics=$(kausa_metrics)
  if echo "$metrics" | awk '$1 == "kausa_notifications_total{outcome=\"sent\"}" && $2 >= 6' |
    grep -q . && echo "$metrics" | grep -E '^kausa_notifications_retried_total [1-9]' >/dev/null
  then
    counted=yes && break
  fi
  sleep 2
done
[ -n "$counted" ] || fail "kausa_notifications_total{outcome=\"sent\"} is under 6, or" \
  "kausa_notifications_retried_total under 1"
echo "notifications: at least 6 sent and 1 retried"

# The grant is the table in docs/reference/permissions.md.
sa="system:serviceaccount:$NAMESPACE:kausa"
allowed() { [ "$(k auth can-i --as="$sa" "$@" 2>/dev/null)" = yes ]; }
granted=("list pods" "watch pods" "get pods --subresource=log" "list events.events.k8s.io"
  "watch events.events.k8s.io" "create events.events.k8s.io" "patch events.events.k8s.io"
  "get replicasets.apps" "get jobs.batch" "list deployments.apps" "watch deployments.apps"
  "list $EXPLANATIONS" "create $EXPLANATIONS" "patch $EXPLANATIONS" "delete $EXPLANATIONS")
# D014 reads Deployments through a list and a watch only (docs/adr/0018-stuck-rollouts.md): no
# get, no write, no subresource, and no other workload kind. Leases (docs/adr/0019-high-
# availability.md): never create, update, delete, list or watch, nor get or patch without a name or
# on another one, in either scope and with any replicaCount; get and patch on LEASE are below.
refused=("get secrets" "list secrets" "get configmaps" "list configmaps" "delete pods"
  "create pods" "create pods --subresource=exec" "update events.events.k8s.io"
  "delete events.events.k8s.io" "list nodes" "create serviceaccounts --subresource=token"
  "impersonate users" "list replicasets.apps" "watch replicasets.apps" "list jobs.batch"
  "get deployments.apps" "update deployments.apps" "patch deployments.apps"
  "delete deployments.apps" "get deployments.apps --subresource=scale"
  "update deployments.apps --subresource=status" "list statefulsets.apps"
  "watch statefulsets.apps" "list daemonsets.apps" "watch daemonsets.apps"
  "get $EXPLANATIONS" "watch $EXPLANATIONS" "update $EXPLANATIONS"
  "deletecollection $EXPLANATIONS"
  "create customresourcedefinitions.apiextensions.k8s.io"
  "patch customresourcedefinitions.apiextensions.k8s.io"
  "create $LEASES" "create $LEASES/$LEASE" "update $LEASES/$LEASE" "delete $LEASES/$LEASE"
  "list $LEASES" "watch $LEASES" "deletecollection $LEASES" "get $LEASES" "patch $LEASES"
  "get $LEASES/e2e-other" "patch $LEASES/e2e-other")
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
    for where in "-n $TARGET" "-n $DENIED" "-n $OTHER" "-n $NAMESPACE" -A; do
      # shellcheck disable=SC2086
      if allowed $check $where; then fail "kausa can $check $where"; fi
    done
  done
  no_cluster_rbac
  for ns in "$TARGET" "$DENIED"; do
    [ "$(history_rule role/"$RELEASE" -n "$ns")" = yes ] || fail "no history rule in $ns's Role"
    [ "$(deployments_rule role/"$RELEASE" -n "$ns")" = yes ] ||
      fail "no deployments rule right after the replicasets rule in $ns's Role"
  done
  can_get_deployments=(-n "$TARGET")
else
  [ "$(history_rule clusterrole/"$RELEASE")" = yes ] || fail "no history rule in the ClusterRole"
  [ "$(deployments_rule clusterrole/"$RELEASE")" = yes ] ||
    fail "no deployments rule right after the replicasets rule in the ClusterRole"
  for check in "${granted[@]}"; do
    # shellcheck disable=SC2086
    allowed $check -A || fail "kausa cannot $check"
  done
  for check in "${refused[@]}"; do
    # shellcheck disable=SC2086
    if allowed $check -A; then fail "kausa can $check"; fi
  done
  can_get_deployments=(-A)
fi
# kausa is Ready with list and watch alone: it never gets a Deployment.
if allowed get deployments.apps "${can_get_deployments[@]}"; then
  fail "kausa can get deployments.apps"
fi
[ "$(pods_ready "$REPLICAS")" = yes ] ||
  fail "kausa is not Ready ($REPLICAS Pods) without get on deployments.apps"
echo "can-i: list and watch on deployments.apps granted, get refused, and kausa is Ready"

# kausa's own Lease (docs/adr/0019-high-availability.md): with two replicas, get and patch on that
# one Lease in its own namespace, in either scope; nowhere else, and nothing with one replica.
for check in "get $LEASES/$LEASE" "patch $LEASES/$LEASE"; do
  if [ "$REPLICAS" = 2 ]; then
    # shellcheck disable=SC2086
    allowed $check -n "$NAMESPACE" || fail "kausa cannot $check -n $NAMESPACE"
  else
    # shellcheck disable=SC2086
    if allowed $check -n "$NAMESPACE"; then fail "kausa can $check with one replica"; fi
  fi
  for where in "-n $TARGET" "-n $DENIED" "-n $OTHER" -A; do
    # shellcheck disable=SC2086
    if allowed $check $where; then fail "kausa can $check $where"; fi
  done
done
# can-i can be wrong about a create with a name (only when a rule grants create with resourceNames,
# which kausa's does not), so a real create, dry run on the server, is refused too: a new name and
# the Lease's own. The same manifest passes as cluster administrator, so the refusal is the grant's.
lease_manifest e2e-probe | k create --dry-run=server -f - >/dev/null
for name in e2e-probe "$LEASE"; do
  if out=$(lease_manifest "$name" | k --as="$sa" create --dry-run=server -f - 2>&1); then
    fail "kausa can create the Lease $name (server-side dry run)"
  fi
  case "$out" in
    *'cannot create resource "leases"'*) ;;
    *) fail "the create of the Lease $name was refused for another reason: $out" ;;
  esac
done
echo "can-i: on Leases, get and patch on $LEASE only with two replicas; a real create refused"

if [ "$REPLICAS" = 2 ]; then
  # Only the leader acts: the holder has not changed, every Event kausa wrote names it as
  # reportingInstance, and the standby logged no incident, explanation, notification or sweep, and
  # neither token. It still answers standby.
  [ "$(holder)" = "$first_leader" ] || fail "the Lease changed holder before any handover"
  [ "$(kausa_events "$TARGET")" -ge 6 ] || fail "fewer Events in $TARGET than failing workloads"
  [ "$(others_reported "$first_leader")" = 0 ] ||
    fail "an Event in $TARGET reported by another Pod than the Lease holder"
  kausa_log "pod/$first_standby"
  quiet="incident|explanation|explanation skipped|explanation dropped|explanation retried"
  quiet="$quiet|notification sent|notification failed|notification retried|notification dropped"
  quiet="$quiet|history swept|history sweep failed|event write failed|history write failed"
  [ "$(log_count "$quiet" < "$WORK/kausa.log")" = 0 ] ||
    fail "the standby logged an incident, an explanation, a notification or a write"
  for token in "$FAKE_BOT" "$FAKE_BEARER"; do
    if grep -qF -- "$token" "$WORK/kausa.log"; then fail "the standby logged a fake token"; fi
  done
  [ "$(pod_metric "$first_standby" kausa_leader)" = 0 ] ||
    fail "the standby's kausa_leader is not 0"
  echo "two replicas: only the Lease holder wrote Events; the standby logged no incident"
fi

# The metrics, through the API server's service proxy, which reaches only a Ready Pod (with two
# replicas, from the Lease holder's Pod through the pod proxy).
metrics=$(kausa_metrics)
echo "$metrics" | grep -E '^kausa_events_written_total\{reason="Explained"\} [1-9]' >/dev/null ||
  fail "kausa_events_written_total does not count the Events"
# The answer's injected step (docs/adr/0022-redact-the-answer.md): an address, a query and a host
# replaced in the answers, and R002 never counted there.
for rule in R004 R013 R014; do
  echo "$metrics" | awk -v series="kausa_answer_redactions_total{rule=\"$rule\"}" \
    '$1 == series && $2 >= 1 { n++ } END { exit !n }' ||
    fail "kausa_answer_redactions_total does not count $rule"
done
if echo "$metrics" | grep -E '^kausa_answer_redactions_total\{([^}]*,)?rule="R002"' >/dev/null; then
  fail "kausa_answer_redactions_total counts R002"
fi
echo "$metrics" | grep -E '^kausa_history_writes_total\{outcome="created"\} [1-9]' >/dev/null ||
  fail "kausa_history_writes_total does not count the Explanations created"
echo "$metrics" |
  awk '$1 == "kausa_incidents_total{detection=\"D014\"}" && $2 >= 1 { n++ } END { exit !n }' ||
  fail "kausa_incidents_total does not count stalled's D014"
restarts=$(k -n "$NAMESPACE" get pods -l app.kubernetes.io/name=kausa \
  -o jsonpath='{.items[*].status.containerStatuses[0].restartCount}')
if [ -z "$restarts" ] || [ -n "$(printf '%s' "$restarts" | tr -d ' 0')" ]; then
  fail "kausa restarted ($restarts)"
fi
# The endpoint check (docs/adr/0020-check-the-endpoint.md): helm test runs kausa check-endpoint in
# a Pod of its own, admitted in NAMESPACE under the restricted Pod Security Standard, and it
# passes against the fake endpoint with one synthetic request.
enforce=$(k get namespace "$NAMESPACE" \
  -o jsonpath='{.metadata.labels.pod-security\.kubernetes\.io/enforce}')
[ "$enforce" = restricted ] || fail "$NAMESPACE enforces the Pod Security Standard '$enforce'"
endpoint_check ok fake

# The history adds no request: each one the endpoint received is a model call kausa counted or the
# endpoint check's, which kausa does not count. A call still in flight is logged by the endpoint
# before kausa counts it, so this is retried.
calls="" lines=""
for _ in $(seq 1 10); do
  calls=$(model_calls)
  lines=$(wc -l < "$WORK/requests.jsonl" | tr -d ' ')
  [ $((calls + CHECK_REQUESTS)) = "$lines" ] && break
  sleep 2
done
[ $((calls + CHECK_REQUESTS)) = "$lines" ] ||
  fail "the endpoint received $lines requests for $calls model calls and $CHECK_REQUESTS checks"
echo "requests: $lines, as many as model calls and endpoint checks ($CHECK_REQUESTS)"

# A model the endpoint does not have: helm test fails with cause not_found. The upgrade rolls kausa
# out with that model too, which explains nothing meanwhile; the next one sets the model back, and
# every kausa Pod runs with --model fake again.
h upgrade "$RELEASE" charts/kausa -n "$NAMESPACE" --wait --timeout 180s --reuse-values \
  --set model.name=e2e-missing
endpoint_check not_found e2e-missing
h upgrade "$RELEASE" charts/kausa -n "$NAMESPACE" --wait --timeout 180s --reuse-values \
  --set model.name=fake
k -n "$NAMESPACE" rollout status deploy/kausa --timeout 120s
if [ "$REPLICAS" = 2 ]; then
  wait_pair 120 "no two Ready kausa Pods with one of them holding the Lease after model.name=fake"
fi
for p in $(kausa_pods); do
  args=$(kausa_args "pod/$p")
  printf '%s\n' "$args" | awk 'prev == "--model" && $0 == "fake" { ok = 1 } { prev = $0 }
    END { exit !ok }' || fail "a kausa Pod does not run with --model fake after the upgrade back"
done
echo "helm test with model e2e-missing: failed with cause not_found; the model is fake again"

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
# Read once, then searched: with pipefail, a grep -q that stops reading early can fail the pipe.
remaining=$(explanations "$TARGET")
for name in e2e-old-labelled kausa.0000000000000002; do
  printf '%s\n' "$remaining" | grep -qxF -- "$TARGET/$name" || fail "the sweep deleted $name"
done
metrics=$(kausa_metrics)
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
  metrics=$(kausa_metrics)
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

# With two replicas, these two steps are skipped: the --reset-values refusal is scope logic the
# other namespaced run covers, and the e2e-denied block checks the readiness and restarts of one
# Pod.
if [ "$SCOPE" = namespaced ] && [ "$REPLICAS" = 1 ]; then
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
  # The watch errors come at once, the Deployment watch's too (docs/adr/0018-stuck-rollouts.md);
  # an incident in TARGET after a settle window and a tick. The wait also gives the readiness
  # probe (every 10 seconds) several tries.
  started=$SECONDS
  denied_logged="" denied_rollouts="" target_logged=""
  for _ in $(seq 1 60); do
    kausa_log "$pod"
    log_has "pod watch error|event watch error" "$DENIED" < "$WORK/kausa.log" && denied_logged=yes
    log_has "deployment watch error" "$DENIED" < "$WORK/kausa.log" && denied_rollouts=yes
    log_has incident "$TARGET" < "$WORK/kausa.log" && target_logged=yes
    [ -n "$denied_logged" ] && [ -n "$denied_rollouts" ] && [ -n "$target_logged" ] &&
      [ $((SECONDS - started)) -ge 30 ] && break
    sleep 2
  done
  [ -n "$denied_logged" ] || fail "no watch error with namespace $DENIED in the new Pod's log"
  [ -n "$denied_rollouts" ] ||
    fail "no deployment watch error with namespace $DENIED in the new Pod's log"
  [ -n "$target_logged" ] || fail "no incident with namespace $TARGET in the new Pod's log"
  # Read from the Pod: a Pod that is not Ready is no endpoint of the metrics Service.
  ready=$(k -n "$NAMESPACE" get "$pod" \
    -o jsonpath='{.status.conditions[?(@.type=="Ready")].status}')
  [ "$ready" = False ] || fail "the new kausa Pod is Ready ($ready) though $DENIED refuses it"
  restarts=$(k -n "$NAMESPACE" get "$pod" -o jsonpath='{.status.containerStatuses[0].restartCount}')
  [ "$restarts" = 0 ] || fail "the new kausa Pod restarted $restarts times"
  echo "$DENIED refused, its Deployment watch too: kausa is not Ready, did not restart and still" \
    "explains $TARGET"
fi

if [ "$REPLICAS" = 2 ]; then
  # A graceful handover. The leader's log is streamed, since its Pod goes with it, and the Pod is
  # deleted: kausa releases the Lease on SIGTERM, once, and another Pod takes it within 10 s: the
  # standby, which reads it every 5 s, or the ReplicaSet's replacement, which can start and find it
  # free first. leaseTransitions goes up by 1 and the new leader counts one change to leader. A new
  # failure is then explained by the new leader.
  wait_pair 60 "no two Ready kausa Pods with one of them holding the Lease before the handover"
  old_leader=$leader
  transitions=$(lease_transitions)
  k -n "$NAMESPACE" logs -f "pod/$old_leader" > "$WORK/graceful.log" 2>&1 &
  LOGS_PID=$!
  for _ in $(seq 1 20); do
    [ "$(log_count started < "$WORK/graceful.log")" = 1 ] && break
    sleep 0.5
  done
  [ "$(log_count started < "$WORK/graceful.log")" = 1 ] || fail "the leader's log is not streamed"
  k -n "$NAMESPACE" delete pod "$old_leader" --wait=false >/dev/null
  wait_other_holder "$old_leader" 10 ||
    fail "no other Pod held the Lease within 10 s of the leader's graceful delete"
  next_leader=$new_holder graceful_took=$took
  for _ in $(seq 1 60); do
    kill -0 "$LOGS_PID" 2>/dev/null || break
    sleep 1
  done
  kill "$LOGS_PID" 2>/dev/null || true
  wait "$LOGS_PID" 2>/dev/null || true
  LOGS_PID=""
  [ "$(log_count "lease released" < "$WORK/graceful.log")" = 1 ] ||
    fail "the deleted leader did not log exactly one 'lease released'"
  [ "$(lease_transitions)" = $((transitions + 1)) ] ||
    fail "leaseTransitions did not go up by exactly 1 in the graceful handover"
  [ "$(pod_metric "$next_leader" 'kausa_leadership_changes_total{to="leader"}')" = 1 ] ||
    fail "the new leader does not count one change to leader"
  wait_pair 120 "no two Ready kausa Pods with one holding the Lease after the graceful handover"
  [ "$leader" = "$next_leader" ] || fail "the Lease changed holder again after the handover"
  seed_failure e2e-graceful
  wait_explained Job e2e-graceful
  [ "$(others_reported "$leader" Job e2e-graceful)" = 0 ] ||
    fail "e2e-graceful's Event was reported by another Pod than the new leader"
  echo "graceful handover: lease released, another Pod held the Lease after ${graceful_took} s," \
    "leaseTransitions + 1, and the new leader explains"

  # A frozen leader. Its process, found on the kind node by its POD_NAME, gets SIGSTOP: it neither
  # renews nor releases, and the standby takes the Lease once it has seen it unchanged for 30 s,
  # within 45 s. SIGCONT follows at once: the liveness probe (every 20 s, 3 s timeout, 3 failures)
  # restarts a frozen container no sooner than about 43 s after it stopped answering, so the
  # takeover must come first (hypothesis, checked by the restart count). Resumed, the old leader
  # finds its term over: leadership lost, not renewed in time, then standby; no restart, no Event
  # created after the takeover, and no explanation, notification or incident after it stepped down.
  frozen=$leader next_leader=$standby
  frozen_restarts=$(restart_count "$frozen")
  pids=$(node_pids "$frozen")
  [ "$(printf '%s\n' "$pids" | grep -c .)" = 1 ] ||
    fail "not exactly one process with the leader's POD_NAME on the kind node"
  node_signal STOP "$pids"
  if ! wait_holder "$next_leader" 45; then
    node_signal CONT "$pids" || true
    fail "the standby did not hold the Lease within 45 s of the leader's SIGSTOP"
  fi
  taken_at=$(date +%s)
  node_signal CONT "$pids"
  frozen_took=$took
  seed_failure e2e-frozen
  wait_explained Job e2e-frozen
  [ "$(others_reported "$next_leader" Job e2e-frozen)" = 0 ] ||
    fail "e2e-frozen's Event was reported by another Pod than the new leader"
  stepped=""
  for _ in $(seq 1 30); do
    kausa_log "pod/$frozen"
    stepped=$(python3 -c '
import json, sys
entries = []
for line in open(sys.argv[1]):
    try:
        entry = json.loads(line)
    except ValueError:
        continue
    if isinstance(entry, dict):
        entries.append(entry)
lost = [i for i, e in enumerate(entries) if e.get("msg") == "leadership lost"
        and e.get("reason") == "not renewed in time"]
if not lost:
    print("waiting")
    sys.exit()
after = entries[lost[0] + 1:]
if not any(e.get("msg") == "standby" for e in after):
    print("waiting")
    sys.exit()
writes = [e for e in after
          if e.get("msg") in ("incident", "explanation", "notification sent", "history swept")]
print("wrote" if writes else "standby")
' "$WORK/kausa.log")
    [ "$stepped" = waiting ] || break
    sleep 2
  done
  [ "$stepped" = standby ] || fail "the resumed leader logged no 'leadership lost' (not renewed" \
    "in time) then standby, or an incident, explanation, notification or sweep after it ($stepped)"
  [ "$(restart_count "$frozen")" = "$frozen_restarts" ] || fail "the frozen leader restarted"
  late=$(k -n "$TARGET" get events.events.k8s.io -o json | python3 -c '
import datetime, json, sys
who, since = sys.argv[1], int(sys.argv[2])
late = 0
for event in json.load(sys.stdin)["items"]:
    if event.get("reportingController") != "kausa" or event.get("reportingInstance") != who:
        continue
    created = event.get("eventTime")
    if created and datetime.datetime.fromisoformat(
            created.replace("Z", "+00:00")).timestamp() > since:
        late += 1
print(late)
' "$frozen" "$taken_at")
  [ "$late" = 0 ] || fail "the frozen leader created $late Events after the takeover"
  [ "$(pod_metric "$frozen" kausa_leader)" = 0 ] ||
    fail "the resumed leader's kausa_leader is not 0"
  [ "$(pod_get "$frozen" readyz)" = standby ] || fail "/readyz of the resumed leader is not standby"
  echo "frozen leader: the standby held the Lease after ${frozen_took} s; resumed, the old leader" \
    "lost it (not renewed in time), did not restart and wrote nothing after"

  # Fail closed. Without the RoleBinding kausa-leader, both replicas get 403 on the Lease: the
  # leader cannot renew and steps down within 30 s, nobody leads (the sum of kausa_leader is 0),
  # and both log a lease error with the 403 hint, rate-limited while it lasts but counted every
  # time. A new failure then gets no Event, no Explanation, no model request and no notification
  # after a settle window, two ticks and 10 s; no kausa Event or Explanation changes at all.
  # helm upgrade with the same values restores the RoleBinding, and one Pod leads within 45 s.
  wait_pair 60 "no two Ready kausa Pods with one holding the Lease before the RoleBinding goes"
  lost_before=$(k -n "$NAMESPACE" logs "pod/$leader" | log_count "leadership lost")
  k -n "$NAMESPACE" delete "rolebinding/$LEASE" >/dev/null
  since=$SECONDS
  closed=""
  while [ $((SECONDS - since)) -le 30 ]; do
    if [ "$(leader_sum)" = 0 ]; then
      lost=$(k -n "$NAMESPACE" logs "pod/$leader" | log_count "leadership lost")
      hinted=yes
      for p in "$leader" "$standby"; do
        [ "$(k -n "$NAMESPACE" logs "pod/$p" | log_count "lease error" hint "$HINT_403")" -ge 1 ] ||
          hinted=""
      done
      [ "$lost" -gt "$lost_before" ] && [ -n "$hinted" ] && closed=yes && break
    fi
    sleep 2
  done
  [ -n "$closed" ] || fail "30 s after the RoleBinding $LEASE went, the sum of kausa_leader is" \
    "not 0, the leader logged no 'leadership lost', or a Pod logged no lease error with the hint"
  writes_snapshot > "$WORK/writes-before.txt"
  requests_before=$(lines_of "$WORK/requests.jsonl")
  hooks_before=$(lines_of "$WORK/hooks.jsonl")
  seed_failure e2e-closed
  k -n "$TARGET" wait job/e2e-closed --for=condition=Failed --timeout=120s >/dev/null
  sleep $((SETTLE + 2 * TICK + 10))
  [ "$(leader_sum)" = 0 ] || fail "a kausa Pod leads without the RoleBinding $LEASE"
  [ "$(kausa_events "$TARGET" e2e-closed)" = 0 ] ||
    fail "an Event about e2e-closed, though nobody leads"
  writes_snapshot > "$WORK/writes-after.txt"
  cmp -s "$WORK/writes-before.txt" "$WORK/writes-after.txt" ||
    fail "kausa wrote an Event or an Explanation in $TARGET while nobody led"
  [ "$(lines_of "$WORK/requests.jsonl")" = "$requests_before" ] ||
    fail "the fake model received a request while nobody led"
  [ "$(lines_of "$WORK/hooks.jsonl")" = "$hooks_before" ] ||
    fail "the fake webhook received a notification while nobody led"
  for p in "$leader" "$standby"; do
    hints=$(k -n "$NAMESPACE" logs "pod/$p" | log_count "lease error" hint "$HINT_403")
    errors=$(pod_get "$p" metrics |
      awk '$1 ~ /^kausa_lease_errors_total[{]/ { sum += $2 } END { print sum + 0 }')
    [ "$hints" -le 4 ] ||
      fail "a kausa Pod logged $hints lease errors with the hint: not rate-limited"
    [ "$errors" -gt "$hints" ] ||
      fail "a kausa Pod counted $errors lease errors for $hints logged: not counted every time"
  done
  h upgrade "$RELEASE" charts/kausa -n "$NAMESPACE" --wait --timeout 120s --reuse-values
  k -n "$NAMESPACE" get "rolebinding/$LEASE" -o name >/dev/null
  since=$SECONDS
  restored=""
  while [ $((SECONDS - since)) -le 45 ]; do
    current=$(holder)
    if [ "$(leader_sum)" = 1 ] && [ -n "$current" ] &&
      [ "$(pod_metric "$current" kausa_leader)" = 1 ]; then
      restored=yes && break
    fi
    sleep 2
  done
  [ -n "$restored" ] || fail "no single leader within 45 s of the helm upgrade that restored $LEASE"
  wait_explained Job e2e-closed
  echo "fail closed: without the RoleBinding nobody led, wrote or sent anything; helm upgrade" \
    "restored one leader, which explains e2e-closed"

  # A foreign write that keeps the holder: a label on the Lease, as cluster administrator, moves
  # its resourceVersion as a Helm label change would. The leader's next renewal gets 409, reads
  # the Lease, finds itself still named and renews: the holder and both Pods' change counters stay.
  wait_pair 60 "no two Ready kausa Pods with one holding the Lease before the label write"
  changes_leader=$(changes "$leader") changes_standby=$(changes "$standby")
  [ "$changes_leader" != missing ] && [ "$changes_standby" != missing ] ||
    fail "a kausa Pod's metrics lack kausa_leadership_changes_total"
  k -n "$NAMESPACE" label lease "$LEASE" e2e=1 >/dev/null
  sleep 15
  [ "$(holder)" = "$leader" ] || fail "a label write on the Lease moved it to another holder"
  if [ "$(changes "$leader")" != "$changes_leader" ] ||
    [ "$(changes "$standby")" != "$changes_standby" ]; then
    fail "a label write on the Lease changed kausa_leadership_changes_total"
  fi
  [ "$(leader_sum)" = 1 ] || fail "not exactly one leader after a label write on the Lease"
  echo "a label write on the Lease: the same holder, no leadership change"

  # A rollout (a new settle window changes the Pod template): two new Ready Pods, one leader.
  old_pods=$(kausa_pods)
  h upgrade "$RELEASE" charts/kausa -n "$NAMESPACE" --wait --timeout 180s --reuse-values \
    --set settleSeconds=$((SETTLE + 1))
  wait_pair 120 "no two Ready kausa Pods with one of them holding the Lease after the rollout"
  for p in "$leader" "$standby"; do
    if printf '%s\n' "$old_pods" | grep -qxF -- "$p"; then
      fail "a kausa Pod from before the rollout is still running"
    fi
  done
  if [ "$(pod_metric "$leader" kausa_leader)" != 1 ] ||
    [ "$(pod_metric "$standby" kausa_leader)" != 0 ]; then
    fail "kausa_leader is not 1 on the holder and 0 on the standby after the rollout"
  fi
  echo "rollout: two new Ready Pods, one leader"
fi

if [ "$SCOPE" = namespaced ]; then
  # Nothing of the release is left, the Lease, its Role and RoleBinding and the
  # PodDisruptionBudget of two replicas included.
  h uninstall "$RELEASE" -n "$NAMESPACE"
  left=""
  for _ in $(seq 1 15); do
    left=$(k get roles,rolebindings,leases,poddisruptionbudgets -A \
      -l "app.kubernetes.io/instance=$RELEASE" -o name)
    left="$left$(k -n "$NAMESPACE" get "lease/$LEASE" "role/$LEASE" "rolebinding/$LEASE" \
      --ignore-not-found -o name)"
    left="$left$(k -n "$NAMESPACE" get poddisruptionbudgets -o name)"
    [ -z "$left" ] && break
    sleep 2
  done
  [ -z "$left" ] || fail "helm uninstall left: $left"
  # The CustomResourceDefinition goes with the release, and every Explanation with it.
  HISTORY=off
  crd_gone
  no_cluster_rbac
  echo "helm uninstall: no Role, Lease, PodDisruptionBudget, reader role or" \
    "CustomResourceDefinition left"
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

  # Budget across restarts (docs/adr/0021-refill-the-call-budget.md), with the cluster scope's one
  # replica, the history and rollouts on: 20 crash-looping Deployments, an hourly budget of 360
  # (one call every 10 s after a start burst of 3) and kausa's Pod deleted every 60 s for 10
  # minutes. A start refills no more than the burst, so the fake endpoint's receive times stay
  # within 3 calls per start plus one per 10 s; v0.4.1 sent up to 16 within seconds of a start.
  BUDGET_NS=e2e-budget
  # The chart's values below: callsPerHour 360, so I = 3600 / 360 s, and the start burst B.
  BUDGET_BURST=3
  BUDGET_INTERVAL=10
  BUDGET_LIVES=10
  BUDGET_LIFE=60
  # The wall-clock time now, in seconds with three decimals, as the fake endpoint logs it.
  now_s() { python3 -c 'import time; print("%.3f" % time.time())'; }
  # The 20 Deployments: crasher's container (tests/e2e/failures.yaml), under invented names.
  budget_deployments() {
    for i in $(seq -w 0 19); do
      cat <<EOF
---
apiVersion: apps/v1
kind: Deployment
metadata:
  name: budget-$i
  namespace: $BUDGET_NS
spec:
  replicas: 1
  selector:
    matchLabels: {app: budget-$i}
  template:
    metadata:
      labels: {app: budget-$i}
    spec:
      containers:
        - name: app
          image: kausa:e2e
          imagePullPolicy: Never
          args: ["--no-such-flag"]
EOF
    done
  }
  # Pod $1's status.startTime, in whole seconds since the epoch (empty until it has one). It only
  # paces the lives. The check counts each life from its process's started line instead: cut to
  # whole seconds, the replacement's startTime can come before the old Pod's deletion, and a call
  # of the old process after it would be counted in the new life.
  pod_started() {
    k -n "$NAMESPACE" get pod "$1" -o json | python3 -c '
import calendar, json, sys, time
start = (json.load(sys.stdin).get("status") or {}).get("startTime")
if start:
    print(calendar.timegm(time.strptime(start, "%Y-%m-%dT%H:%M:%SZ")))
'
  }
  # One scrape of Pod $2 in life $1, appended to scrapes.txt as "life time hour queue", with "-"
  # for a series the scrape lacks. A Pod not listening yet adds nothing.
  budget_scrape() {
    out=$(pod_get "$2" metrics 2>/dev/null) || return 0
    printf '%s\n' "$out" | awk -v life="$1" -v at="$(now_s)" '
      $1 == "kausa_budget_calls_remaining{window=\"hour\"}" { hour = $2 }
      $1 == "kausa_explanation_queue_length" { queue = $2 }
      END { print life, at, (hour == "" ? "-" : hour), (queue == "" ? "-" : queue) }
    ' >> "$WORK/budget/scrapes.txt"
  }
  # The assertions, on the files in $2 (lives.txt: "pod restarts" per life, in order; scrapes.txt;
  # life-N.log, kausa's log of life N) and the fake endpoint's time log $1, with $3 the end of the
  # last life and $4 and $5 the burst and the interval in seconds. A life starts at its process's
  # started line, logged before the process builds its budget, and ends at its stopped line (the
  # last life at $3). Integer milliseconds throughout, so that the bounds are exact:
  # 0. every life but the last logged stopped, before the next one logged started: the lives do not
  #    overlap, so each request comes from the life it is counted in;
  # 1. requests in [T0, end] <= floor(B*K + (end - T0) / I), T0 the first start, K the starts;
  # 2. a request belongs to the life with the latest start at or before it, and the j-th of a life,
  #    r after its start, has j <= B + floor((r + 1 s) / I) (1 s for the node's clock, which stamps
  #    the log, against the host's, which stamps the requests: on Docker Desktop, a VM's);
  # 3. kausa_explanation_queue_length was above 0 in some scrape;
  # 4. the first scrape of each life has window="hour" <= B + floor((t - start + 1 s) / I);
  # 5. some life logged explanations waiting;
  # 6. every explanation dropped line has a boolean waiting_for_budget.
  budget_check() {
    python3 -I - "$@" <<'EOF'
import bisect, calendar, glob, json, sys, time

times_path, folder, end_text, burst, interval = sys.argv[1:6]
burst, step = int(burst), int(interval) * 1000


def fail(message):
    sys.exit("FAIL: budget across restarts: " + message)


def ms(text):
    whole, _, fraction = text.strip().partition(".")
    return int(whole) * 1000 + int((fraction + "000")[:3])


def logged(text, up):
    """kausa's ts (RFC 3339 in UTC, up to nine decimals) in milliseconds, rounded up if up."""
    whole, _, fraction = text.rstrip("Z").partition(".")
    fraction = fraction.ljust(9, "0")
    seconds = calendar.timegm(time.strptime(whole, "%Y-%m-%dT%H:%M:%S"))
    return seconds * 1000 + int(fraction[:3]) + (1 if up and int(fraction[3:]) else 0)


def life_log(path):
    """The started and stopped times of the log at path (None for a line it lacks)."""
    started = stopped = None
    for line in open(path):
        try:
            entry = json.loads(line)
        except ValueError:
            continue
        if not isinstance(entry, dict):
            continue
        if entry.get("msg") == "started" and started is None:
            started = logged(entry["ts"], False)
        if entry.get("msg") == "stopped":
            stopped = logged(entry["ts"], True)
    return started, stopped


restarts = [int(line.split()[1]) for line in open(folder + "/lives.txt") if line.strip()]
if not restarts:
    fail("no life recorded")
if any(restarts):
    fail("kausa's container restarted within a life, a start this test does not count")
starts, stops = [], []
for life in range(1, len(restarts) + 1):
    started, stopped = life_log("%s/life-%d.log" % (folder, life))
    if started is None:
        fail("life %d's log has no started line" % life)
    starts.append(started)
    stops.append(stopped)
for life in range(1, len(starts)):
    if stops[life - 1] is None:
        fail("life %d's log has no stopped line: kausa did not stop on SIGTERM" % life)
    if stops[life - 1] > starts[life]:
        fail("life %d stopped %.3f s after life %d started: the lives overlap"
             % (life, (stops[life - 1] - starts[life]) / 1000, life + 1))
first, end, starts_counted = starts[0], ms(end_text), len(starts)

requests = sorted(ms(line) for line in open(times_path) if line.strip())
requests = [r for r in requests if r >= first]
total = sum(1 for r in requests if r <= end)
bound = (burst * starts_counted * step + end - first) // step
if total > bound:
    fail("%d requests in %.3f s over %d starts; at most %d may come"
         % (total, (end - first) / 1000, starts_counted, bound))

counts = [0] * starts_counted
for r in requests:
    life = bisect.bisect_right(starts, r) - 1
    counts[life] += 1
    allowed = burst + (r - starts[life] + 1000) // step
    if counts[life] > allowed:
        fail("life %d: request %d came %.3f s after its start; at most %d may"
             % (life + 1, counts[life], (r - starts[life]) / 1000, allowed))

scrapes = [line.split() for line in open(folder + "/scrapes.txt") if line.strip()]
if not any(queue != "-" and float(queue) > 0 for _, _, _, queue in scrapes):
    fail("kausa_explanation_queue_length was never above 0")
first_scrape = {}
for life, at, hour, _ in scrapes:
    if hour != "-" and int(life) not in first_scrape:
        first_scrape[int(life)] = (ms(at), float(hour))
for life in range(1, starts_counted + 1):
    if life not in first_scrape:
        fail("no scrape of life %d has the hour series" % life)
    at, hour = first_scrape[life]
    allowed = burst + (at - starts[life - 1] + 1000) // step
    if hour > allowed:
        fail("life %d: the hour series read %g %.3f s after its start; at most %d may"
             % (life, hour, (at - starts[life - 1]) / 1000, allowed))

waiting = 0
for path in sorted(glob.glob(folder + "/life-*.log")):
    for line in open(path):
        try:
            entry = json.loads(line)
        except ValueError:
            continue
        if not isinstance(entry, dict):
            continue
        if entry.get("msg") == "explanations waiting":
            waiting += 1
        if entry.get("msg") == "explanation dropped" and not isinstance(
                entry.get("waiting_for_budget"), bool):
            fail("an explanation dropped line has no boolean waiting_for_budget")
if not waiting:
    fail("no life logged explanations waiting")
print("budget across restarts: %d requests in %.0f s over %d starts, at most %d allowed"
      % (total, (end - first) / 1000, starts_counted, bound))
EOF
  }

  # The cluster scope runs with one replica only (checked at the top); the case needs it.
  [ "$REPLICAS" = 1 ] || fail "the budget case needs one replica"
  k create namespace "$BUDGET_NS"
  budget_deployments | k create -f - >/dev/null
  h upgrade "$RELEASE" charts/kausa -n "$NAMESPACE" --wait --timeout 120s --reuse-values \
    --set budget.callsPerHour=360 --set settleSeconds=5
  mkdir -p "$WORK/budget"
  : > "$WORK/budget/lives.txt"
  : > "$WORK/budget/scrapes.txt"
  life_pod=$(kausa_pods | head -n 1)
  [ -n "$life_pod" ] || fail "no kausa Pod after the upgrade to callsPerHour=360"
  life=0
  while :; do
    life=$((life + 1))
    start=$(pod_started "$life_pod")
    [ -n "$start" ] || fail "kausa's Pod of life $life has no start time"
    # Polled every 2 s until the life is BUDGET_LIFE seconds old.
    while [ "$(date +%s)" -lt $((start + BUDGET_LIFE)) ]; do
      budget_scrape "$life" "$life_pod"
      sleep 2
    done
    # The life's restarts, read before its Pod goes.
    restarts=$(k -n "$NAMESPACE" get pod "$life_pod" \
      -o jsonpath='{.status.containerStatuses[0].restartCount}')
    echo "$life_pod ${restarts:-0}" >> "$WORK/budget/lives.txt"
    if [ "$life" -ge "$BUDGET_LIVES" ]; then
      kausa_log "pod/$life_pod"
      mv "$WORK/kausa.log" "$WORK/budget/life-$life.log"
      break
    fi
    # The life's log, followed until its process ends, so that it holds the stopped line. The
    # ReplicaSet creates the next Pod as soon as this one is deleted, before this process stops:
    # the check fails if a life stopped after the next one started, since the decision's bound
    # rests on terms not overlapping.
    log_file=$WORK/budget/life-$life.log
    k -n "$NAMESPACE" logs -f "pod/$life_pod" > "$log_file" &
    follower=$!
    for _ in $(seq 1 50); do
      grep -q '"msg":"started"' "$log_file" && break
      sleep 0.2
    done
    k -n "$NAMESPACE" delete pod "$life_pod" --timeout 120s >/dev/null
    # The Pod is gone, so its container is too: the stream ends at once; 10 s at most.
    for _ in $(seq 1 50); do
      kill -0 "$follower" 2>/dev/null || break
      sleep 0.2
    done
    kill "$follower" 2>/dev/null || true
    wait "$follower" || true
    next_pod=""
    for _ in $(seq 1 60); do
      next_pod=$(kausa_pods | grep -vxF -- "$life_pod" | head -n 1 || true)
      [ -n "$next_pod" ] && [ -n "$(pod_started "$next_pod")" ] && break
      next_pod=""
      sleep 1
    done
    [ -n "$next_pod" ] || fail "no new kausa Pod after deleting the one of life $life"
    life_pod=$next_pod
  done
  budget_check "$WORK/request-times.txt" "$WORK/budget" "$(now_s)" "$BUDGET_BURST" \
    "$BUDGET_INTERVAL"
  k delete namespace "$BUDGET_NS" --timeout 120s >/dev/null
  h upgrade "$RELEASE" charts/kausa -n "$NAMESPACE" --wait --timeout 120s --reuse-values \
    --set budget.callsPerHour=3600 --set settleSeconds="$SETTLE"
  pod=pod/$(kausa_pods | head -n 1)

  # The breaker (docs/adr/0020-check-the-endpoint.md), with the hourly budget of 3600 back for the
  # drain. The fake endpoint answers 401 while the fail file exists, a configuration error, so the
  # first call opens the breaker at once, for 60 s; 18 failing Jobs (D008), each its own owner,
  # are more incidents than the queue's 16, so that the slot skips some while it holds. The file
  # goes 5 s before the hold ends, and the probe closes the breaker.
  breaker_pod=$(kausa_pods | head -n 1)
  [ -n "$breaker_pod" ] || fail "no kausa Pod for the breaker case"
  printf '401\n' > "$FAIL_FILE.new"
  mv "$FAIL_FILE.new" "$FAIL_FILE"
  for i in $(seq -w 0 17); do
    seed_failure "breaker-$i"
  done
  opened=""
  deadline=$((SECONDS + 180))
  while :; do
    kausa_log "pod/$breaker_pod"
    [ "$(log_count "endpoint unavailable" < "$WORK/kausa.log")" -ge 1 ] && opened=yes && break
    [ "$SECONDS" -lt "$deadline" ] || break
    sleep 2
  done
  [ -n "$opened" ] || fail "kausa logged no endpoint unavailable within 180 s of the 401s"
  # Nothing calls the endpoint while the breaker holds, so its last request is the opening call:
  # the hold ends no sooner than the probe_in_seconds of the endpoint unavailable line after it.
  opened_lines=$(lines_of "$WORK/requests.jsonl")
  opened_at=$(tail -n 1 "$WORK/request-times.txt")
  # The one endpoint unavailable line so far: a 401, the key, one failed call, the first hold, and
  # no field of an incident's.
  hold=$(python3 -I - "$WORK/kausa.log" <<'EOF'
import json, sys


def fail(message):
    sys.exit("FAIL: the endpoint unavailable line: " + message)


found = []
for text in open(sys.argv[1]):
    try:
        entry = json.loads(text)
    except ValueError:
        continue
    if isinstance(entry, dict) and entry.get("msg") == "endpoint unavailable":
        found.append(entry)
if len(found) != 1:
    fail("%d of them before the first hold ended" % len(found))
line = found[0]
want = {"level": "warn", "cause": "key", "class": "configuration", "status": 401,
        "reason": "the endpoint answered 401 Unauthorized", "failed_calls": 1,
        "probe_in_seconds": 60}
for key, value in want.items():
    if json.dumps(line.get(key)) != json.dumps(value):
        fail("%s is %s, not %s" % (key, json.dumps(line.get(key)), json.dumps(value)))
if not isinstance(line.get("hint"), str) or not line["hint"]:
    fail("it has no hint")
for key in ("detection", "namespace", "owner", "fingerprint"):
    if key in line:
        fail("it has the incident field " + key)
print(line["probe_in_seconds"])
EOF
  ) || exit 1
  # Seconds from now to $1 (Unix time), negative once past.
  until_time() { python3 -c 'import sys, time; print(int(float(sys.argv[1]) - time.time()))' "$1"; }
  watch_until=$(python3 -c 'import sys; print("%.3f" % (float(sys.argv[1]) + int(sys.argv[2])))' \
    "$opened_at" "$((hold - 5))")
  [ "$(until_time "$watch_until")" -ge 10 ] ||
    fail "the endpoint unavailable line was read too late to watch the hold"
  # Until 5 s before the hold ends: no request reaches the endpoint, and the gauge reads open.
  read_open=""
  while [ "$(until_time "$watch_until")" -gt 0 ]; do
    [ "$(lines_of "$WORK/requests.jsonl")" = "$opened_lines" ] ||
      fail "a model request reached the endpoint while the breaker held"
    kausa_metrics > "$WORK/metrics.txt"
    [ "$(endpoint_state_is "$WORK/metrics.txt" open)" = yes ] && read_open=yes
    sleep 2
  done
  [ -n "$read_open" ] || fail "kausa_endpoint_state never read open while the breaker held"
  echo "the breaker opened on a 401: endpoint unavailable, no request for ${hold}s, the gauge open"
  rm -f "$FAIL_FILE"

  # The probe closes the breaker and the queue drains. Then, read together: no request came within
  # the hold of the opening call, and one came after it; one endpoint available line; the gauges
  # read closed, an empty queue and a last success after the opening call; a Job breaker-NN is
  # explained; and each incident skipped as endpoint_unavailable has one ExplanationSkipped Event
  # with the fixed note, written by this Pod, as many as the counter.
  breaker_check() {
    python3 -I - "$WORK" "$breaker_pod" "$opened_lines" "$opened_at" "$hold" <<'EOF'
import json, re, sys

work, pod, opened_lines, opened_at, hold = sys.argv[1:6]
opened_lines, opened_at, hold = int(opened_lines), float(opened_at), int(hold)
REASON = "the model endpoint is unavailable; this incident waited and was not explained"
NOTE = "kausa could not explain this failure: " + REASON


def fail(message):
    sys.exit("FAIL: the breaker: " + message)


times = [float(t) for t in open(work + "/request-times.txt") if t.strip()][opened_lines:]
early = [t for t in times if t < opened_at + hold]
if early:
    fail("a model request reached the endpoint %.3f s after the opening call, within the %d s "
         "hold" % (min(early) - opened_at, hold))
if not times:
    fail("no probe reached the endpoint after the hold")

entries = []
for text in open(work + "/kausa.log"):
    try:
        entry = json.loads(text)
    except ValueError:
        continue
    if isinstance(entry, dict):
        entries.append(entry)
available = [e for e in entries if e.get("msg") == "endpoint available"]
if len(available) != 1:
    fail("%d endpoint available lines" % len(available))
seconds = available[0].get("unavailable_seconds")
if isinstance(seconds, bool) or not isinstance(seconds, (int, float)) or seconds < hold:
    fail("endpoint available has unavailable_seconds %s" % json.dumps(seconds))
calls = available[0].get("failed_calls")
if isinstance(calls, bool) or not isinstance(calls, int) or calls < 1:
    fail("endpoint available has failed_calls %s" % json.dumps(calls))
skipped = [e.get("fingerprint") for e in entries
           if e.get("msg") == "explanation skipped" and e.get("reason") == REASON]
if not skipped:
    fail("no explanation skipped line with the endpoint_unavailable reason")

metrics = {}
for text in open(work + "/metrics.txt"):
    parts = text.split()
    if len(parts) == 2 and not text.startswith("#"):
        metrics[parts[0]] = float(parts[1])
states = {s: metrics.get('kausa_endpoint_state{state="%s"}' % s)
          for s in ("closed", "half_open", "open")}
if states != {"closed": 1, "half_open": 0, "open": 0}:
    fail("kausa_endpoint_state reads %s" % json.dumps(states))
queue = metrics.get("kausa_explanation_queue_length")
if queue != 0:
    fail("kausa_explanation_queue_length is %s" % json.dumps(queue))
last = metrics.get("kausa_model_last_success_timestamp_seconds")
if last is None or last <= opened_at:
    fail("kausa_model_last_success_timestamp_seconds is %s, not after the opening call"
         % json.dumps(last))

noted, explained = [], 0
for event in json.load(open(work + "/events.json"))["items"]:
    if event.get("reportingController") != "kausa" or event.get("reportingInstance") != pod:
        continue
    regarding = event.get("regarding") or {}
    if event.get("reason") == "ExplanationSkipped" and event.get("note") == NOTE:
        match = re.match(r"kausa\.skipped\.([0-9a-f]{16})\.", event["metadata"]["name"])
        noted.append(match.group(1) if match else None)
    if (event.get("reason") == "Explained" and regarding.get("kind") == "Job"
            and (regarding.get("name") or "").startswith("breaker-")):
        explained += 1
if sorted(noted, key=str) != sorted(skipped, key=str):
    fail("%d ExplanationSkipped Events with the fixed note for %d incidents skipped as "
         "endpoint_unavailable, or not one each" % (len(noted), len(skipped)))
counter = metrics.get('kausa_explanations_skipped_total{reason="endpoint_unavailable"}')
if counter != len(noted):
    fail("the endpoint_unavailable counter is %s for %d Events" % (json.dumps(counter), len(noted)))
if not explained:
    fail("no Job breaker-NN has an Explained Event after the probe")
print("the breaker closed after %.0f s and %d failed calls; %d incidents skipped as "
      "endpoint_unavailable, one Event each; %d Jobs breaker-NN explained"
      % (seconds, calls, len(noted), explained))
EOF
  }
  drained=""
  deadline=$((SECONDS + 300))
  while :; do
    kausa_log "pod/$breaker_pod"
    k -n "$TARGET" get events.events.k8s.io -o json > "$WORK/events.json"
    kausa_metrics > "$WORK/metrics.txt"
    breaker_report=$(breaker_check 2>/dev/null) && drained=yes && break
    [ "$SECONDS" -lt "$deadline" ] || break
    sleep 3
  done
  [ -n "$drained" ] || breaker_report=$(breaker_check) || exit 1
  echo "$breaker_report"
  [ "$(restart_count "$breaker_pod")" = 0 ] || fail "kausa restarted during the breaker case"

  # dryRun=true turns the history off: the rule, the reader role and the CustomResourceDefinition
  # go, and with it every Explanation, kausa's and the hand-made ones. The chart refuses
  # notifications with dryRun, so they go too.
  [ -n "$(explanations -A)" ] || fail "no Explanation before the upgrade to dryRun=true"
  h upgrade "$RELEASE" charts/kausa -n "$NAMESPACE" --wait --timeout 120s --reuse-values \
    --set dryRun=true --set notifications.enabled=false
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

  # history.enabled=false: the rule, the reader role and the CustomResourceDefinition go. With
  # rollouts=false (docs/adr/0018-stuck-rollouts.md), the deployments rule goes too, and the new
  # Pod runs with --no-rollouts and is Ready on two watches.
  old=$(k -n "$NAMESPACE" get pods -l app.kubernetes.io/name=kausa -o name)
  h upgrade "$RELEASE" charts/kausa -n "$NAMESPACE" --wait --timeout 120s --reuse-values \
    --set history.enabled=false --set rollouts=false
  HISTORY=off
  [ "$(history_rule clusterrole/"$RELEASE")" = no ] || fail "a history rule without the history"
  [ -z "$(k get clusterrole "$READER" --ignore-not-found -o name)" ] ||
    fail "the reader role remains without the history"
  crd_gone
  echo "helm upgrade --set history.enabled=false: no history rule, reader role or CRD left"
  [ "$(deployments_rule clusterrole/"$RELEASE")" = no ] ||
    fail "a deployments rule in the ClusterRole with rollouts=false"
  for check in "list deployments.apps" "watch deployments.apps"; do
    # shellcheck disable=SC2086
    if allowed $check -A; then fail "kausa can $check with rollouts=false"; fi
  done
  k -n "$NAMESPACE" rollout status deploy/kausa --timeout 120s
  pod=$(k -n "$NAMESPACE" get pods -l app.kubernetes.io/name=kausa -o name |
    grep -vxF -- "$old" | head -n 1 || true)
  [ -n "$pod" ] || fail "no new kausa Pod after the upgrade to rollouts=false"
  # Read once, then searched: with pipefail, a grep -q that stops reading early can fail the pipe.
  args=$(kausa_args "$pod")
  printf '%s\n' "$args" | grep -qxF -- --no-rollouts ||
    fail "the kausa Pod runs without --no-rollouts with rollouts=false"
  ready=$(k -n "$NAMESPACE" get "$pod" \
    -o jsonpath='{.status.conditions[?(@.type=="Ready")].status}')
  [ "$ready" = True ] || fail "kausa is not Ready ($ready) with rollouts=false"
  kausa_log "$pod"
  rollouts=$(started_field rollouts)
  [ "$rollouts" = false ] || fail "the started line has rollouts $rollouts with rollouts=false"
  echo "helm upgrade --set rollouts=false: no deployments rule, --no-rollouts, a Ready Pod"

  # From cluster to namespaced scope in place, with the history on again: the ClusterRole and its
  # binding go, a Role and a RoleBinding come; the reader role and the CustomResourceDefinition are
  # the only cluster-scoped objects.
  h upgrade "$RELEASE" charts/kausa -n "$NAMESPACE" --wait --timeout 120s --reuse-values \
    --set rbac.scope=namespaced --set "rbac.namespaces={$TARGET}" --set history.enabled=true
  HISTORY=on
  no_cluster_rbac
  has_roles "$TARGET"
  [ "$(history_rule role/"$RELEASE" -n "$TARGET")" = yes ] || fail "no history rule in the Role"
  # rollouts=false stays through --reuse-values.
  [ "$(deployments_rule role/"$RELEASE" -n "$TARGET")" = no ] ||
    fail "a deployments rule in the Role, though rollouts=false was reused"
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
  # Still rollouts=false, from the values reused since that upgrade.
  args=$(kausa_args "$pod")
  printf '%s\n' "$args" | grep -qxF -- --no-rollouts ||
    fail "--no-rollouts did not survive the upgrades"
  rollouts=$(started_field rollouts)
  [ "$rollouts" = false ] || fail "the started line has rollouts $rollouts after the restart"
  echo "without the CustomResourceDefinition: kausa is Ready, did not restart and logs why"
fi

echo "e2e: ok"
