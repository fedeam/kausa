#!/usr/bin/env bash
# The soak's daily breaker window (docs/how-to/run-the-soak.md), in the cluster of the kube context
# given as the first argument: writes 401 to the fake endpoint's fail file, so that every model call
# fails and the first one opens kausa's breaker (docs/adr/0020-check-the-endpoint.md); creates a
# failing Job soak-breaker-<UTC yyyymmddHHMM> in kausa-soak, a new owner and so a new incident and a
# call; waits the given minutes (10 by default, 1 to 60); and removes the fail file. The file is
# also removed when the script stops early, on an error, an interrupt or a TERM. The Job deletes
# itself a day after it fails. busybox is the reference of tests/bench/seeded-failures.yaml.
#
# The context is never the current one: it must be named, it must be a context of the kubeconfig,
# and every kubectl call names it. Needs kubectl.
set -euo pipefail

usage() {
  echo "usage: tests/soak/exercise.sh <kube-context> [minutes]" >&2
  exit 2
}
fail() {
  echo "FAIL: $*" >&2
  exit 1
}

[ "$#" -ge 1 ] && [ "$#" -le 2 ] || usage
CTX=$1
MINUTES=${2:-10}
case "$CTX" in '' | -*) usage ;; esac
[[ $MINUTES =~ ^[1-9][0-9]?$ ]] && [ "$MINUTES" -le 60 ] || usage

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

NS=kausa-soak
BUSYBOX=$(grep -Eo 'busybox[^ ]*@sha256:[0-9a-f]{64}' tests/bench/seeded-failures.yaml | sed -n 1p)
[ -n "$BUSYBOX" ] || fail "no busybox reference in tests/bench/seeded-failures.yaml"
JOB="soak-breaker-$(date -u +%Y%m%d%H%M)"

# Removes the fail file: the endpoint answers again, and kausa's breaker closes at its next probe.
restore() {
  if k -n "$NS" exec deploy/kausa-soak-endpoint -c endpoint -- sh -c 'rm -f /tmp/fail'; then
    echo "soak: the fake endpoint answers again"
  else
    echo "FAIL: could not remove /tmp/fail in deploy/kausa-soak-endpoint: remove it by hand" >&2
  fi
}
trap restore EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

k -n "$NS" exec deploy/kausa-soak-endpoint -c endpoint -- sh -c 'printf "401\n" > /tmp/fail'
echo "soak: the fake endpoint answers 401 for $MINUTES minutes"

k -n "$NS" create -f - <<EOF
apiVersion: batch/v1
kind: Job
metadata:
  name: $JOB
  namespace: $NS
  labels:
    app.kubernetes.io/name: soak-breaker
spec:
  backoffLimit: 0
  ttlSecondsAfterFinished: 86400
  template:
    metadata:
      labels:
        app.kubernetes.io/name: soak-breaker
    spec:
      restartPolicy: Never
      automountServiceAccountToken: false
      securityContext:
        runAsNonRoot: true
        runAsUser: 65534
        runAsGroup: 65534
        seccompProfile:
          type: RuntimeDefault
      containers:
        - name: fail
          image: $BUSYBOX
          command: ["sh", "-c", "exit 1"]
          resources:
            requests: {cpu: 5m, memory: 8Mi}
            limits: {cpu: 50m, memory: 16Mi}
          securityContext:
            allowPrivilegeEscalation: false
            readOnlyRootFilesystem: true
            capabilities:
              drop: [ALL]
EOF

# In the background, so that an interrupt or a TERM is handled at once, not after the sleep.
sleep "$((MINUTES * 60))" &
wait "$!"
