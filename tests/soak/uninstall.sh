#!/usr/bin/env bash
# Removes the soak (docs/how-to/run-the-soak.md) from the cluster of the kube context given as the
# only argument: the release kausa-soak (with its ClusterRole and ClusterRoleBinding), then the
# namespace kausa-soak with the fake endpoint, its ConfigMap, the Secret and the breaker Jobs. It
# does not remove the seeded failures of tests/bench/seeded-failures.yaml: it prints what to delete.
#
# The context is never the current one: it must be named, it must be a context of the kubeconfig,
# and every kubectl and helm call names it. Needs kubectl and helm.
set -euo pipefail

usage() {
  echo "usage: tests/soak/uninstall.sh <kube-context>" >&2
  exit 2
}
fail() {
  echo "FAIL: $*" >&2
  exit 1
}

[ "$#" -eq 1 ] || usage
CTX=$1
case "$CTX" in '' | -*) usage ;; esac

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

h uninstall kausa-soak -n kausa-soak --wait
k delete namespace kausa-soak --wait
echo "soak: the release kausa-soak and the namespace kausa-soak are deleted"
echo
echo "If tests/bench/seeded-failures.yaml was applied for the soak, delete what its header lists,"
echo "with the cluster owner's consent: the namespaces kausa-test and kausa-test-cron and the"
echo "cluster-scoped RuntimeClass kausa-test-no-handler."
