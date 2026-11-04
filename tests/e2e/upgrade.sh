#!/usr/bin/env bash
# The upgrade test: the latest release (tests/e2e/upgrade-from.txt), installed from its GitHub
# release with its signed image from GHCR, upgraded with helm upgrade --reuse-values to this
# checkout's chart and image, in a kind cluster, with two replicas (docs/adr/0019-high-availability.md)
# in the rbac.scope of KAUSA_E2E_SCOPE (cluster when unset, or namespaced). Before the upgrade, the
# released build explains a seeded failure; after it, the rollout is complete, both new Pods are
# Ready and run kausa:e2e, one holds the Lease and the other is a standby, the user values are the
# old ones plus the image, the new leader starts with the old leader's settings, logs and explains a
# failure seeded after the upgrade and notifies the webhook of it, and helm test passes. Needs
# docker, kind, kubectl, helm, curl and python3, and network access to github.com and ghcr.io.
# Creates the kind cluster kausa-upgrade and deletes it at the end.
# See docs/how-to/develop.md.
#
# The released chart is downloaded and checked before the cluster exists, so that a wrong pin fails
# in seconds. The upgrade runs once and is never retried: after a failed upgrade, the next
# --reuse-values upgrade reuses the values of the last deployed revision, not of the failed one.
# The released image is pulled by the node from GHCR, never side-loaded; kind loads only kausa:e2e.
# The replica count is not read from the environment: the test always runs two replicas.
set -euo pipefail

. tests/e2e/lib.sh

# Set before the trap, so that cleanup never deletes a run.sh cluster.
REPLICAS=2
CLUSTER=kausa-upgrade
CONTEXT="kind-$CLUSTER"

SCOPE=${KAUSA_E2E_SCOPE:-cluster}
case "$SCOPE" in
  cluster | namespaced) ;;
  *) fail "KAUSA_E2E_SCOPE must be cluster or namespaced" ;;
esac

trap cleanup EXIT

# The release to upgrade from: read, never sourced, and each value checked.
pin() { sed -n "s/^$1=//p" tests/e2e/upgrade-from.txt; }
FROM_VERSION=$(pin KAUSA_UPGRADE_FROM_VERSION)
FROM_CHART_SHA256=$(pin KAUSA_UPGRADE_FROM_CHART_SHA256)
FROM_IMAGE_DIGEST=$(pin KAUSA_UPGRADE_FROM_IMAGE_DIGEST)
[[ $FROM_VERSION =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || fail "no version in tests/e2e/upgrade-from.txt"
[[ $FROM_CHART_SHA256 =~ ^[0-9a-f]{64}$ ]] || fail "no chart SHA-256 in tests/e2e/upgrade-from.txt"
[[ $FROM_IMAGE_DIGEST =~ ^sha256:[0-9a-f]{64}$ ]] ||
  fail "no image digest in tests/e2e/upgrade-from.txt"
FROM_IMAGE="ghcr.io/fedeam/kausa@$FROM_IMAGE_DIGEST"
# This checkout's version, as release.yml reads it.
TO_VERSION=$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -n 1)

# The released chart, checked against the pinned SHA-256 before Helm reads it.
curl --proto '=https' --tlsv1.2 -fsSL --retry 5 -o "$WORK/from.tgz" \
  "https://github.com/fedeam/kausa/releases/download/v$FROM_VERSION/kausa-$FROM_VERSION.tgz"
got=$(python3 -I -c 'import hashlib, sys; print(hashlib.sha256(open(sys.argv[1], "rb").read()).hexdigest())' "$WORK/from.tgz")
[ "$got" = "$FROM_CHART_SHA256" ] || fail "the v$FROM_VERSION chart is not the pinned one"

: "${KIND_NODE_IMAGE:?set KIND_NODE_IMAGE to a pinned kindest/node image}"
echo "e2e upgrade: v$FROM_VERSION to this checkout ($TO_VERSION), rbac.scope=$SCOPE, two replicas"
kind create cluster --name "$CLUSTER" --image "$KIND_NODE_IMAGE" --wait 120s
docker build --target runtime -t kausa:e2e .
kind load docker-image kausa:e2e --name "$CLUSTER"

start_endpoint

# The image of kausa's container in each of kausa's Pods that are not being deleted, sorted, once
# each.
pod_images() {
  for p in $(kausa_pods); do
    k -n "$NAMESPACE" get pod "$p" -o jsonpath='{.spec.containers[?(@.name=="kausa")].image}'
    echo
  done | sort -u
}

# The settings of the one started line in kausa's log in WORK that the user values decide, as one
# JSON object: what the new chart makes of the values, which helm get values cannot show.
started_settings() {
  python3 -I -c '
import json, sys
FIELDS = ("settle_seconds", "namespaces", "rollouts", "notifications", "leader_lease",
          "model_timeout_seconds")
started = []
for line in open(sys.argv[1]):
    try:
        entry = json.loads(line)
    except ValueError:
        continue
    if isinstance(entry, dict) and entry.get("msg") == "started":
        started.append(entry)
if len(started) != 1:
    sys.exit("FAIL: %d started lines in the log" % len(started))
print(json.dumps({field: started[0].get(field, "<missing>") for field in FIELDS}, sort_keys=True))
' "$WORK/kausa.log"
}

# 1. The namespaces and the Secret, as run.sh makes them. TARGET holds the seeded failures in both
# scopes.
k create namespace "$NAMESPACE"
k label namespace "$NAMESPACE" pod-security.kubernetes.io/enforce=restricted
# The Secret the chart mounts for the notifications, from files: no token in a command line.
k -n "$NAMESPACE" create secret generic kausa-notify --from-file=url="$WORK/notify-url" \
  --from-file=token="$WORK/notify-token" >/dev/null
k create namespace "$TARGET" --save-config

# 2. The release, installed from its chart with only values every chart since v0.5.0 has. The image
# stays at the chart's default repository, pinned by digest, which wins over the tag.
from_values=(--set image.digest="$FROM_IMAGE_DIGEST" --set replicaCount=2
  --set model.endpoint="http://$gateway:$PORT/v1" --set model.name=fake
  --set model.allowHttp=true --set settleSeconds="$SETTLE" --set history.enabled=true
  --set budget.callsPerHour=3600
  --set notifications.enabled=true --set notifications.format=json
  --set notifications.existingSecret=kausa-notify --set notifications.tokenKey=token
  --set notifications.allowHttp=true)
if [ "$SCOPE" = namespaced ]; then
  from_values+=(--set rbac.scope=namespaced --set "rbac.namespaces={$TARGET}")
else
  from_values+=(--set rbac.scope=cluster)
fi
h install "$RELEASE" "$WORK/from.tgz" -n "$NAMESPACE" --wait --timeout 180s "${from_values[@]}"

# 3. Before the upgrade: the release works as released.
wait_pair 90 "no two Ready kausa Pods with one holding the Lease after installing v$FROM_VERSION"
[ "$(pod_images)" = "$FROM_IMAGE" ] ||
  fail "a kausa Pod does not run the pinned v$FROM_VERSION image"
for p in "$leader" "$standby"; do
  kausa_log "pod/$p"
  v=$(started_field version)
  [ "$v" = "\"$FROM_VERSION\"" ] || fail "a started line has version $v before the upgrade"
done
# The leader's settings, which the new leader must start with.
kausa_log "pod/$leader"
started_settings > "$WORK/started-before.json"
seed_failure upgrade-before
wait_explained Job upgrade-before
old_pods=$(kausa_pods)
transitions_before=$(lease_transitions)
h get values "$RELEASE" -n "$NAMESPACE" -o json > "$WORK/values-before.json"
echo "v$FROM_VERSION: two Ready Pods on the pinned image, one holds the Lease;" \
  "Job upgrade-before explained"

# 4. The upgrade, once and never retried. The four image values are the only --sets: they stand in
# for a user's upgrade, in which image.tag stays empty and the new chart's appVersion picks the
# new image.
h upgrade "$RELEASE" charts/kausa -n "$NAMESPACE" --reuse-values --wait --timeout 180s \
  --set image.repository=kausa --set image.tag=e2e --set-string image.digest= \
  --set image.pullPolicy=Never

# 5. After the upgrade: the rollout and readiness. With TO_VERSION equal to FROM_VERSION (a pull
# request that does not bump the version), the image tells the new Pods from the old ones.
k -n "$NAMESPACE" rollout status deploy/kausa --timeout 120s
k -n "$NAMESPACE" get deploy/kausa -o json | python3 -I -c '
import json, sys
deployment = json.load(sys.stdin)
status = deployment.get("status") or {}
ok = status.get("observedGeneration") == deployment["metadata"]["generation"] and all(
    status.get(key) == 2
    for key in ("replicas", "updatedReplicas", "readyReplicas", "availableReplicas"))
sys.exit(0 if ok else 1)
' && [ "$(pods_ready 2)" = yes ] ||
  fail "the Deployment does not report two updated, ready and available replicas after the upgrade"
new_pods=$(kausa_pods)
for p in $new_pods; do
  if grep -qxF -- "$p" <<<"$old_pods"; then
    fail "a kausa Pod from before the upgrade is still there"
  fi
done
[ "$(pod_images)" = kausa:e2e ] || fail "a kausa Pod does not run kausa:e2e after the upgrade"
for p in $new_pods; do
  [ "$(restart_count "$p")" = 0 ] || fail "a kausa Pod restarted after the upgrade"
done
h status "$RELEASE" -n "$NAMESPACE" -o json | python3 -I -c '
import json, sys
release = json.load(sys.stdin)
sys.exit(0 if (release.get("info") or {}).get("status") == "deployed"
         and release.get("version") == 2 else 1)
' || fail "the release is not deployed at revision 2"
echo "upgraded: the rollout is complete, two new Ready Pods run kausa:e2e with no restart," \
  "revision 2 deployed"

# 6. After the upgrade: the user values are the old ones plus the four image values. Only the
# differing top-level keys are printed, never a value.
h get values "$RELEASE" -n "$NAMESPACE" -o json > "$WORK/values-after.json"
diff_values=$(python3 -I -c '
import json, sys
before = json.load(open(sys.argv[1]))
after = json.load(open(sys.argv[2]))
if after.get("image") != {"repository": "kausa", "tag": "e2e", "digest": "", "pullPolicy": "Never"}:
    print("image")
    sys.exit(0)
before.pop("image", None)
after.pop("image", None)
changed = (key for key in set(before) | set(after) if before.get(key) != after.get(key))
print(" ".join(sorted(changed)))
' "$WORK/values-before.json" "$WORK/values-after.json")
[ "$diff_values" != image ] ||
  fail "the image values after the upgrade are not the four the upgrade set"
[ -z "$diff_values" ] || fail "the upgrade changed the user values: $diff_values"
echo "values: the old user values plus the four image values"

# 7. After the upgrade: the Lease split, checked as run.sh checks it after its install, plus this
# checkout's version in each started line and one more Lease transition. The new leader starts with
# the settings the old leader started with, and its arguments carry the budget and the history of
# the user values: helm get values (step 6) shows what Helm stored, not what the new chart renders
# from it. Only the names of differing fields are printed: notifications.host is an address.
wait_pair 60 "no two Ready kausa Pods with one holding the Lease after the upgrade"
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
  v=$(started_field version)
  [ "$v" = "\"$TO_VERSION\"" ] || fail "a started line has version $v after the upgrade"
  leading=$((leading + $(log_count leading < "$WORK/kausa.log")))
done
[ "$leading" = 1 ] || fail "$leading leading lines across both Pods' logs"
[ "$(log_count leading < "$WORK/kausa.log")" = 0 ] || fail "the standby logged leading"
[ "$(log_count standby < "$WORK/kausa.log")" -ge 1 ] || fail "the standby logged no standby line"
[ "$(lease_transitions)" -gt "$transitions_before" ] ||
  fail "the Lease's leaseTransitions did not increase in the upgrade"
kausa_log "pod/$leader"
started_settings > "$WORK/started-after.json"
changed=$(python3 -I -c '
import json, sys
before = json.load(open(sys.argv[1]))
after = json.load(open(sys.argv[2]))
changed = (key for key in set(before) | set(after) if before.get(key) != after.get(key))
print(" ".join(sorted(changed)))
' "$WORK/started-before.json" "$WORK/started-after.json")
[ -z "$changed" ] || fail "the new leader started with other settings than the old one: $changed"
args=$(kausa_args "pod/$leader")
printf '%s\n' "$args" | awk '
  prev == "--max-calls-per-hour" && $0 == "3600" { ok = 1 } { prev = $0 } END { exit !ok }' ||
  fail "the new leader's --max-calls-per-hour is not followed by 3600"
[ "$(printf '%s\n' "$args" | grep -cxF -- --history)" = 1 ] ||
  fail "the new leader does not run with exactly one --history"
echo "after the upgrade: one holds the Lease; kausa_leader 1 and 0, /readyz ready and standby," \
  "one leading line, version $TO_VERSION, leaseTransitions up, the old leader's settings"

# 8. An incident after the upgrade: the new leader logs and explains it, nobody else writes about
# it, and the webhook receives it. The receiver answered its first POST 503, before the upgrade, so
# this one is answered 200. Only a count is read from the hook log, never a body.
seed_failure upgrade-after
found=""
for _ in $(seq 1 40); do
  kausa_log "pod/$leader"
  if log_has incident upgrade-after anywhere < "$WORK/kausa.log"; then
    found=yes
    break
  fi
  sleep 3
done
[ -n "$found" ] || fail "the new leader logged no incident about Job upgrade-after"
wait_explained Job upgrade-after
[ "$(others_reported "$leader" Job upgrade-after)" = 0 ] ||
  fail "another Pod than the leader wrote about Job upgrade-after"
kausa_log "pod/$standby"
[ "$(log_count incident < "$WORK/kausa.log")" = 0 ] || fail "the standby logged an incident"
# The notifications the receiver answered 200 about Job upgrade-after in TARGET.
hooks_about() {
  python3 -I -c '
import json, sys
count = 0
for line in open(sys.argv[1]):
    try:
        hook = json.loads(line)
        about = json.loads(hook["body"])["object"]
    except (ValueError, KeyError, TypeError):
        continue
    if hook.get("status") == 200 and isinstance(about, dict) and (
            about.get("kind"), about.get("namespace"), about.get("name")) == (
            "Job", sys.argv[2], "upgrade-after"):
        count += 1
print(count)
' "$WORK/hooks.jsonl" "$TARGET"
}
notified=0
for _ in $(seq 1 30); do
  notified=$(hooks_about)
  [ "$notified" -ge 1 ] && break
  sleep 2
done
[ "$notified" = 1 ] ||
  fail "the webhook received $notified notifications about Job upgrade-after, not 1"
echo "Job upgrade-after: logged and explained by the new leader only, and notified once"

# 9. The endpoint check, rendered by this chart from the reused values.
endpoint_check ok fake

echo "e2e upgrade: v$FROM_VERSION to $TO_VERSION, rbac.scope=$SCOPE: ok"
