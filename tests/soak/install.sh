#!/usr/bin/env bash
# Installs the soak (docs/how-to/run-the-soak.md) in the cluster of the kube context given as the
# only argument: the namespace kausa-soak under the restricted Pod Security Standard, the fake
# endpoint of tests/soak/endpoint.yaml with its ConfigMap, the notification Secret, and the release
# kausa-soak of the chart and image pinned in tests/e2e/upgrade-from.txt (the chart downloaded from
# its GitHub release and checked against the pinned SHA-256) with tests/soak/values.yaml. Then waits
# for two Ready kausa Pods and a holder of the Lease kausa-soak-leader, and prints the next
# commands. Refuses when kausa-soak already exists. Writes nothing outside kausa-soak but kausa's
# ClusterRole and ClusterRoleBinding kausa-soak.
#
# The context is never the current one: it must be named, it must be a context of the kubeconfig,
# and every kubectl and helm call names it. Needs kubectl, helm, curl and python3, and network
# access to github.com; the cluster pulls kausa's image from ghcr.io and python's from Docker Hub.
set -euo pipefail

usage() {
  echo "usage: tests/soak/install.sh <kube-context>" >&2
  exit 2
}
fail() {
  echo "FAIL: $*" >&2
  exit 1
}

[ "$#" -eq 1 ] || usage
CTX=$1
case "$CTX" in '' | -*) usage ;; esac

cd "$(dirname "$0")/../.."

# The context must be a whole name in the kubeconfig: a local read, no cluster call.
if ! contexts=$(kubectl config get-contexts -o name); then
  fail "cannot read the contexts of the kubeconfig"
fi
known=no
while IFS= read -r name; do
  if [ "$name" = "$CTX" ]; then known=yes; fi
done <<<"$contexts"
[ "$known" = yes ] || fail "no context '$CTX' in the kubeconfig"

k() { kubectl --context "$CTX" "$@"; }
h() { helm --kube-context "$CTX" "$@"; }

NS=kausa-soak
RELEASE=kausa-soak
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

# The release, as tests/e2e/upgrade.sh reads it: never sourced, each value checked.
pin() { sed -n "s/^$1=//p" tests/e2e/upgrade-from.txt; }
FROM_VERSION=$(pin KAUSA_UPGRADE_FROM_VERSION)
FROM_CHART_SHA256=$(pin KAUSA_UPGRADE_FROM_CHART_SHA256)
FROM_IMAGE_DIGEST=$(pin KAUSA_UPGRADE_FROM_IMAGE_DIGEST)
[[ $FROM_VERSION =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || fail "no version in tests/e2e/upgrade-from.txt"
[[ $FROM_CHART_SHA256 =~ ^[0-9a-f]{64}$ ]] || fail "no chart SHA-256 in tests/e2e/upgrade-from.txt"
[[ $FROM_IMAGE_DIGEST =~ ^sha256:[0-9a-f]{64}$ ]] ||
  fail "no image digest in tests/e2e/upgrade-from.txt"

# The released chart, checked against the pinned SHA-256 before Helm reads it.
curl --proto '=https' --tlsv1.2 -fsSL --retry 5 -o "$WORK/from.tgz" \
  "https://github.com/fedeam/kausa/releases/download/v$FROM_VERSION/kausa-$FROM_VERSION.tgz"
got=$(python3 -I -c 'import hashlib, sys; print(hashlib.sha256(open(sys.argv[1], "rb").read()).hexdigest())' "$WORK/from.tgz")
[ "$got" = "$FROM_CHART_SHA256" ] || fail "the v$FROM_VERSION chart is not the pinned one"

if [ -n "$(k get namespace "$NS" --ignore-not-found -o name)" ]; then
  fail "the namespace kausa-soak exists in this context: run tests/soak/uninstall.sh first"
fi
echo "soak: installing kausa v$FROM_VERSION as $RELEASE in $NS"

k create namespace "$NS"
k label namespace "$NS" pod-security.kubernetes.io/enforce=restricted
k -n "$NS" create configmap kausa-soak-endpoint --from-file=fake_endpoint.py=tests/e2e/fake_endpoint.py
k -n "$NS" apply -f tests/soak/endpoint.yaml
k -n "$NS" rollout status deploy/kausa-soak-endpoint --timeout 300s
# The webhook's URL, from a file, as tests/e2e/run.sh creates its Secret.
printf '%s' "http://kausa-soak-endpoint.$NS.svc:8080/hook/soak" >"$WORK/notify-url"
k -n "$NS" create secret generic kausa-soak-notify --from-file=url="$WORK/notify-url"

h install "$RELEASE" "$WORK/from.tgz" -n "$NS" -f tests/soak/values.yaml --set image.digest="$FROM_IMAGE_DIGEST" --wait --timeout 300s

# "yes" when exactly two kausa Pods of the release are not being deleted and both are Ready.
pods_ready() {
  k -n "$NS" get pods -l "app.kubernetes.io/name=kausa,app.kubernetes.io/instance=$RELEASE" -o json |
    python3 -I -c '
import json, sys
pods = [p for p in json.load(sys.stdin)["items"] if not p["metadata"].get("deletionTimestamp")]
ready = [p for p in pods if any(c.get("type") == "Ready" and c.get("status") == "True"
                                for c in (p.get("status") or {}).get("conditions") or [])]
print("yes" if len(pods) == 2 and len(ready) == 2 else "no")
'
}
holder=""
for _ in $(seq 1 60); do
  if [ "$(pods_ready)" = yes ]; then
    holder=$(k -n "$NS" get lease "$RELEASE-leader" --ignore-not-found -o jsonpath='{.spec.holderIdentity}')
    if [ -n "$holder" ]; then break; fi
  fi
  sleep 5
done
[ -n "$holder" ] || fail "no two Ready kausa Pods with one holding the Lease $RELEASE-leader after 300 s"

echo "soak: two Ready kausa Pods; the Lease $RELEASE-leader has a holder"
echo
echo "Next (docs/how-to/run-the-soak.md), with the cluster owner's consent for the seeded failures:"
printf '  %s\n' \
  "kubectl --context $CTX apply -f tests/bench/seeded-failures.yaml" \
  "python3 -I tests/soak/sample.py --context $CTX --out <a directory outside the repository>" \
  "tests/soak/exercise.sh $CTX   # once a day, on days 2 to 6" \
  "python3 -I tests/soak/summarize.py <that directory>" \
  "tests/soak/uninstall.sh $CTX"
