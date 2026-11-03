#!/usr/bin/env bash
# Chart check: lints and renders charts/kausa in both RBAC scopes
# (docs/adr/0013-namespaced-install-mode.md), with the explanation history on and off
# (docs/adr/0014-explanation-history.md), with notifications on and off
# (docs/adr/0015-notifications.md), with the Deployment watch of D014 on and off
# (docs/adr/0018-stuck-rollouts.md) and with one or two replicas
# (docs/adr/0019-high-availability.md), and checks the rendered objects, the rules, the
# Deployment's arguments and volumes, the NetworkPolicy, NOTES.txt, the budget's flags and comments
# (docs/adr/0021-refill-the-call-budget.md), and every refusal of kausa.validate with its exact
# message. It also checks, with helm alone, that the detections of v0.5's coverage gaps need no new
# permission, and the seeded failure the kind test adds for them (tests/e2e/failures.yaml and
# tests/e2e/run.sh), and the endpoint check's helm test Pod, its NetworkPolicy and
# model.timeoutSeconds (docs/adr/0020-check-the-endpoint.md). For the upgrade test and the soak
# (v0.6, measured reliability), it renders the pinned release's values upgraded to this chart as
# helm upgrade --reuse-values does and the soak's values, and checks the soak endpoint's manifest,
# the e2e workflow's jobs, and the e2e and soak scripts, the soak's against stub kubectl, helm, curl
# and sleep, and the pages that document them.
# Needs helm, git, coreutils and python3 (shellcheck when installed), and never contacts a cluster
# (KUBECONFIG points nowhere) or the network.
# Run it from anywhere: tests/chart/render.sh. It prints one line per check and exits 1 if any
# failed.
#
# KAUSA_CHART_BASE names a git ref with the v0.1 chart (default v0.1.0); when the ref is in the
# clone, the ClusterRole rules are also compared with that chart's render: equal with
# rollouts=false, and with only the deployments rule added by default. KAUSA_CHART_PREVIOUS
# names a git ref with the chart before notifications (for example 13cfd4d, the history's merge;
# unset by default, because the chart's version labels differ after a release); when it is set and
# in the clone, renders with notifications off are compared with its renders byte for byte (with
# rollouts=false, its --no-rollouts arg left out, and without the endpoint check's documents and
# --model-timeout arg). KAUSA_CHART_V03 names a git ref with the v0.3 chart (default v0.3.0);
# when the ref is in the clone, NOTES.txt's scope paragraph with rollouts=false is compared with
# that chart's. KAUSA_CHART_PRE_HA names a git ref with the chart before replicaCount (unset by
# default, for the same reason as KAUSA_CHART_PREVIOUS); when it is set and in the clone, the
# one-replica renders are compared with its renders byte for byte (without the endpoint check's
# documents and --model-timeout arg).
# HELM names the binary.
# No pipefail: grep -q ends a pipe early, and its writer's SIGPIPE is not a failure.
set -eu

ROOT=$(cd "$(dirname "$0")/../.." && pwd)
CHART="$ROOT/charts/kausa"
HELM=${HELM:-helm}
BASE=${KAUSA_CHART_BASE:-v0.1.0}
PREVIOUS=${KAUSA_CHART_PREVIOUS:-}
KUBE_VERSION=1.36.0
RELEASE=kausa
NAMESPACE=kausa-system
# The helm test Pod of the endpoint check and its NetworkPolicy (docs/adr/0020-check-the-endpoint.md).
CHECK_POD=kausa-endpoint-check
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
export KUBECONFIG=/nonexistent/kausa-chart-tests/kubeconfig
FAILURES=0

fail() {
  echo "FAIL: $*" >&2
  FAILURES=$((FAILURES + 1))
}
ok() { echo "ok: $*"; }
# passed SINCE NAME: prints ok when no check failed since FAILURES was SINCE.
passed() { if [ "$FAILURES" = "$1" ]; then ok "$2"; fi; }

# render OUT RELEASE CHART ARGS...: helm template into OUT, its stderr into OUT.err.
render() {
  local out=$1 release=$2 chart=$3
  shift 3
  "$HELM" template "$release" "$chart" --kube-version "$KUBE_VERSION" --namespace "$NAMESPACE" \
    "$@" >"$out" 2>"$out.err"
}

# docs FILE KIND [NAMESPACE] [NAME]: the documents of that kind (and metadata.namespace, and
# metadata.name; an empty NAMESPACE matches any), each followed by a line '%%'.
docs() {
  awk -v kind="$2" -v ns="${3-}" -v nm="${4-}" '
    function flush() {
      if (k && (ns == "" || n) && (nm == "" || m)) { printf "%s", buf; print "%%" }
      buf = ""; k = 0; n = 0; m = 0; meta = 0
    }
    /^---/ { flush(); next }
    { buf = buf $0 "\n" }
    /^[^ #]/ { meta = ($0 == "metadata:") }
    $0 == "kind: " kind { k = 1 }
    meta && ns != "" && ($0 == "  namespace: " ns || $0 == "  namespace: \"" ns "\"" ||
                         $0 == "  namespace: '\''" ns "'\''") { n = 1 }
    meta && nm != "" && ($0 == "  name: " nm || $0 == "  name: \"" nm "\"") { m = 1 }
    END { flush() }
  ' "$1"
}

# cluster_scoped FILE: Kind/name of every document without a metadata.namespace (every namespaced
# object of the chart sets one), sorted, one per line.
cluster_scoped() {
  awk '
    function flush() {
      if (k != "" && !n) print k "/" name
      k = ""; n = 0; name = ""; meta = 0
    }
    /^---/ { flush(); next }
    /^kind: / { k = substr($0, 7) }
    /^[^ #]/ { meta = ($0 == "metadata:") }
    meta && /^  namespace: / { n = 1 }
    meta && /^  name: / { name = substr($0, 9); gsub(/"/, "", name) }
    END { flush() }
  ' "$1" | sort
}

# expected_cluster_scoped OBJECTS...: the same format, for the objects given.
expected_cluster_scoped() {
  if [ "$#" -gt 0 ]; then printf '%s\n' "$@" | sort; fi
}

count_docs() { docs "$@" | grep -c '^%%$' || true; }

# without_endpoint_check: the render on stdin without what docs/adr/0020-check-the-endpoint.md
# added: the documents named $CHECK_POD (the helm test Pod and its NetworkPolicy), each with its
# '---' line, and the Deployment's --model-timeout arg with its value. For the byte-for-byte
# comparisons with charts from before it, as --no-rollouts is left out for those before 0018.
without_endpoint_check() {
  awk -v nm="$CHECK_POD" '
    function flush() { if (!drop) printf "%s", buf; buf = ""; drop = 0; meta = 0 }
    /^---/ { flush() }
    skip { skip = 0; next }
    /^ *- --model-timeout$/ { skip = 1; next }
    { buf = buf $0 "\n" }
    /^[^ #]/ { meta = ($0 == "metadata:") }
    meta && ($0 == "  name: " nm || $0 == "  name: \"" nm "\"") { drop = 1 }
    END { flush() }'
}

# section KEY: the lines of a top-level key of the documents on stdin, spaces removed.
section() {
  awk -v key="$1" '$0 == key ":" { on = 1; next } on && /^[^ ]/ { on = 0 } on' | tr -d ' '
}

# args FILE: the items of the Deployment's container args, without their indentation.
args() {
  docs "$1" Deployment | awk '
    !on && /^ *args:$/ { on = 1; match($0, /^ */); ind = RLENGTH; next }
    on {
      if ($0 ~ /^ *$/) next
      match($0, /^ */)
      if (RLENGTH <= ind) exit
      s = substr($0, RLENGTH + 1)
      if (s !~ /^#/) print s
    }'
}

# The rule rows of v0.1, without spaces (charts/kausa/templates/clusterrole.yaml at v0.1.0).
rule_pods() { printf '%s\n' '-apiGroups:[""]' 'resources:["pods"]' 'verbs:["get","list","watch"]'; }
rule_logs() { printf '%s\n' '-apiGroups:[""]' 'resources:["pods/log"]' 'verbs:["get"]'; }
rule_events_read() {
  printf '%s\n' '-apiGroups:["events.k8s.io"]' 'resources:["events"]' 'verbs:["list","watch"]'
}
rule_events_write() {
  printf '%s\n' '-apiGroups:["events.k8s.io"]' 'resources:["events"]' 'verbs:["create","patch"]'
}
rule_replicasets() { printf '%s\n' '-apiGroups:["apps"]' 'resources:["replicasets"]' 'verbs:["get"]'; }
rule_jobs() { printf '%s\n' '-apiGroups:["batch"]' 'resources:["jobs"]' 'verbs:["get"]'; }
# The Deployment watch of D014 (docs/adr/0018-stuck-rollouts.md), right after replicasets.
rule_deployments() {
  printf '%s\n' '-apiGroups:["apps"]' 'resources:["deployments"]' 'verbs:["list","watch"]'
}
# The history rule (docs/adr/0014-explanation-history.md), last.
rule_history() {
  printf '%s\n' '-apiGroups:["kausa.fedeam.com"]' 'resources:["explanations"]' \
    'verbs:["list","create","patch","delete"]'
}
# expected_rules LOGS WRITES [HISTORY]: the rules for "yes"/"no" pods/log, Event writes and the
# history; with the deployments rule unless ROLLOUTS=no.
expected_rules() {
  rule_pods
  if [ "$1" = yes ]; then rule_logs; fi
  rule_events_read
  if [ "$2" = yes ]; then rule_events_write; fi
  rule_replicasets
  if [ "${ROLLOUTS:-yes}" != no ]; then rule_deployments; fi
  rule_jobs
  if [ "${3-no}" = yes ]; then rule_history; fi
}

# check_rollouts_arg NAME FILE: with ROLLOUTS=no, exactly one --no-rollouts arg; otherwise none.
check_rollouts_arg() {
  local count
  count=$(args "$2" | grep -cx -- '- --no-rollouts' || true)
  if [ "${ROLLOUTS:-yes}" = no ]; then
    [ "$count" = 1 ] || fail "$1: $count --no-rollouts args, not exactly one"
  else
    [ "$count" = 0 ] || fail "$1: passes --no-rollouts"
  fi
}

# NOTES.txt is not part of helm template's output: a copy of the chart renders it as a string.
# notes_chart CHART: adds that template to a copy of a chart.
notes_chart() {
  {
    echo '{{- define "kausa.notesCheck" }}'
    cat "$1/templates/NOTES.txt"
    echo '{{- end }}'
    echo 'notes: {{ include "kausa.notesCheck" . | toJson }}'
  } >"$1/templates/zz-notes-check.yaml"
}
NOTES_CHART="$WORK/notes/kausa"
mkdir -p "$WORK/notes"
cp -R "$CHART" "$NOTES_CHART"
notes_chart "$NOTES_CHART"

# notes OUT ARGS...: the rendered NOTES.txt, as text; NOTES_FROM names another such copy.
notes() {
  local out=$1
  shift
  render "$out.raw" "$RELEASE" "${NOTES_FROM:-$NOTES_CHART}" \
    --show-only templates/zz-notes-check.yaml "$@" || {
    fail "NOTES.txt does not render with $*: $(cat "$out.raw.err")"
    : >"$out"
    return 0
  }
  awk '/^notes: / {
         s = substr($0, 9); s = substr(s, 1, length(s) - 1)
         gsub(/\\n/, "\n", s); gsub(/\\"/, "\"", s)
         gsub(/\\u003c/, "<", s); gsub(/\\u003e/, ">", s); gsub(/\\u0026/, "\\&", s)
         print s
       }' "$out.raw" >"$out"
}

ENDPOINT=(--set model.endpoint=https://api.example.com/v1 --set model.name=m
  --set model.apiKey.existingSecret=k)
DRY_NO_LOGS=(--set dryRun=true --set logs=false --set metrics.enabled=false)
NO_EVENTS=(--set model.endpoint=http://m.example.svc:11434/v1 --set model.name=m
  --set model.allowHttp=true --set events=false --set networkPolicy.enabled=true
  --set 'networkPolicy.apiServer.cidrs[0]=192.0.2.1/32'
  --set 'networkPolicy.endpoint.to[0].ipBlock.cidr=192.0.2.2/32')
DRY=(--set dryRun=true)
LONG63=$(printf '%063d' 0 | tr 0 a)
LONG64="${LONG63}a"
NAMES32=$(seq -f 'team-%g' 1 32 | paste -sd, -)

# --- Lint ---------------------------------------------------------------------------------------

if "$HELM" lint --strict "$CHART" --set rbac.scope=cluster >"$WORK/lint" 2>&1; then
  ok "helm lint --strict with rbac.scope=cluster"
else
  fail "helm lint --strict with rbac.scope=cluster: $(cat "$WORK/lint")"
fi
if "$HELM" lint --strict "$CHART" --set rbac.scope=namespaced --set 'rbac.namespaces={e2e-a}' \
  >"$WORK/lint" 2>&1; then
  ok "helm lint --strict with rbac.scope=namespaced"
else
  fail "helm lint --strict with rbac.scope=namespaced: $(cat "$WORK/lint")"
fi
# A bare helm lint is not checked: lint renders in lint mode, where fail is logged as [INFO] and
# does not fail the lint (Helm 3.16.1, checked on 2026-10-28). helm template refuses (below).

# --- Cluster scope ------------------------------------------------------------------------------

# check_cluster NAME LOGS WRITES ARGS...: a cluster-scope render with v0.1's objects and rules.
# HISTORY=yes expects the history rule last and --history in the args; EXTRA lists the other
# cluster-scoped objects expected (Kind/name, space-separated), none by default. ROLLOUTS=no expects
# no deployments rule and one --no-rollouts arg.
check_cluster() {
  local name=$1 logs=$2 writes=$3
  shift 3
  local out="$WORK/cluster-$name" since=$FAILURES history=${HISTORY:-no}
  # shellcheck disable=SC2206 # split on spaces on purpose
  local extra=(${EXTRA-})
  if ! render "$out" "$RELEASE" "$CHART" --set rbac.scope=cluster "$@"; then
    fail "cluster/$name does not render: $(cat "$out.err")"
    return 0
  fi
  [ "$(count_docs "$out" ClusterRole "" kausa)" = 1 ] ||
    fail "cluster/$name: not exactly one ClusterRole named kausa"
  [ "$(count_docs "$out" ClusterRoleBinding)" = 1 ] ||
    fail "cluster/$name: not exactly one ClusterRoleBinding"
  [ "$(count_docs "$out" Role)" = 0 ] || fail "cluster/$name: renders a Role"
  [ "$(count_docs "$out" RoleBinding)" = 0 ] || fail "cluster/$name: renders a RoleBinding"
  [ "$(count_docs "$out" Namespace)" = 0 ] || fail "cluster/$name: renders a Namespace"
  cluster_scoped "$out" >"$out.scoped"
  expected_cluster_scoped ClusterRole/kausa ClusterRoleBinding/kausa \
    ${extra[@]+"${extra[@]}"} >"$out.scoped-expected"
  diff -u "$out.scoped-expected" "$out.scoped" >"$out.scoped-diff" ||
    fail "cluster/$name: the cluster-scoped objects: $(cat "$out.scoped-diff")"
  docs "$out" ClusterRole "" kausa | section rules >"$out.rules"
  expected_rules "$logs" "$writes" "$history" >"$out.expected"
  diff -u "$out.expected" "$out.rules" >"$out.diff" ||
    fail "cluster/$name: the ClusterRole rules are not the expected ones: $(cat "$out.diff")"
  if [ "$history" = yes ]; then
    args "$out" | grep -qx -- '- --history' || fail "cluster/$name: no --history arg"
  else
    args "$out" | grep -q -- '^- --history' && fail "cluster/$name: passes a --history arg"
  fi
  docs "$out" ClusterRoleBinding | section roleRef >"$out.ref"
  for line in 'apiGroup:rbac.authorization.k8s.io' 'kind:ClusterRole' 'name:kausa'; do
    grep -qx -- "$line" "$out.ref" || fail "cluster/$name: the roleRef lacks $line"
  done
  docs "$out" ClusterRoleBinding | section subjects >"$out.subjects"
  for line in '-kind:ServiceAccount' 'name:kausa' "namespace:$NAMESPACE"; do
    grep -qx -- "$line" "$out.subjects" || fail "cluster/$name: the subject lacks $line"
  done
  args "$out" | grep -qx -- '- --namespace' && fail "cluster/$name: passes --namespace"
  [ "$(args "$out" | head -n 1)" = '- run' ] || fail "cluster/$name: the first arg is not run"
  check_rollouts_arg "cluster/$name" "$out"
  passed "$since" "cluster/$name"
}

check_cluster default no no
check_cluster endpoint yes yes "${ENDPOINT[@]}"
check_cluster dry-run-no-logs no no "${DRY_NO_LOGS[@]}"
check_cluster dry-run yes no "${DRY[@]}"
check_cluster no-events yes no "${NO_EVENTS[@]}"

# The same value sets with the Deployment watch off (docs/adr/0018-stuck-rollouts.md): no
# deployments rule and one --no-rollouts arg, whatever else is on.
ROLLOUTS_OFF=(--set rollouts=false)
ROLLOUTS=no check_cluster default-no-rollouts no no "${ROLLOUTS_OFF[@]}"
ROLLOUTS=no check_cluster endpoint-no-rollouts yes yes "${ENDPOINT[@]}" "${ROLLOUTS_OFF[@]}"
ROLLOUTS=no check_cluster dry-run-no-logs-no-rollouts no no "${DRY_NO_LOGS[@]}" \
  "${ROLLOUTS_OFF[@]}"
ROLLOUTS=no check_cluster dry-run-no-rollouts yes no "${DRY[@]}" "${ROLLOUTS_OFF[@]}"
ROLLOUTS=no check_cluster no-events-no-rollouts yes no "${NO_EVENTS[@]}" "${ROLLOUTS_OFF[@]}"

# The same rules as the v0.1 chart's render with the same values, when that chart is in the clone:
# exactly, with rollouts=false; and with only the three rows of the deployments rule added right
# after the replicasets rule, by default.
if git -C "$ROOT" rev-parse --verify --quiet "$BASE^{commit}" >/dev/null 2>&1; then
  mkdir -p "$WORK/base"
  git -C "$ROOT" archive "$BASE" charts/kausa | tar -x -C "$WORK/base"
  for name in default endpoint dry-run-no-logs dry-run no-events; do
    case $name in
      default) values=() ;;
      endpoint) values=("${ENDPOINT[@]}") ;;
      dry-run-no-logs) values=("${DRY_NO_LOGS[@]}") ;;
      dry-run) values=("${DRY[@]}") ;;
      no-events) values=("${NO_EVENTS[@]}") ;;
    esac
    out="$WORK/base-$name"
    if render "$out" "$RELEASE" "$WORK/base/charts/kausa" ${values[@]+"${values[@]}"}; then
      docs "$out" ClusterRole | section rules >"$out.rules"
      if [ -f "$WORK/cluster-$name-no-rollouts.rules" ] &&
        diff -u "$out.rules" "$WORK/cluster-$name-no-rollouts.rules" >"$out.diff"; then
        ok "cluster/$name with rollouts=false has the rules of $BASE"
      else
        fail "cluster/$name with rollouts=false: the rules differ from $BASE's:" \
          "$(cat "$out.diff" 2>/dev/null)"
      fi
      rule_deployments >"$out.deployments-rows"
      awk '
        NR == FNR { rows = rows $0 "\n"; next }
        { print }
        after && /^verbs:/ { printf "%s", rows; after = 0 }
        $0 == "resources:[\"replicasets\"]" { after = 1 }
      ' "$out.deployments-rows" "$out.rules" >"$out.rollouts-expected"
      [ "$(grep -cxF 'resources:["deployments"]' "$out.rollouts-expected" || true)" = 1 ] ||
        fail "$BASE's $name rules: the deployments rows were not added exactly once"
      if [ -f "$WORK/cluster-$name.rules" ] &&
        diff -u "$out.rollouts-expected" "$WORK/cluster-$name.rules" >"$out.rollouts-diff"; then
        ok "cluster/$name has the rules of $BASE and the deployments rule after replicasets"
      else
        fail "cluster/$name: the rules are not $BASE's with the deployments rule after" \
          "replicasets: $(cat "$out.rollouts-diff" 2>/dev/null)"
      fi
    else
      fail "the $BASE chart does not render with $name: $(cat "$out.err")"
    fi
  done
else
  echo "skipped: $BASE is not in this clone; the rules are checked against v0.1's rows above"
fi

# --- Namespaced scope ---------------------------------------------------------------------------

# check_namespaced NAME LOGS WRITES LIST [ARGS...]: a namespaced render for the comma-separated
# LIST: nothing cluster-scoped, one Role and one RoleBinding per namespace, the rules of the
# cluster scope with the same values, and one --namespace per entry right after run.
# FULLNAME and SUBJECT (the names of the objects and the service account) default to kausa;
# HISTORY, EXTRA and ROLLOUTS as in check_cluster.
check_namespaced() {
  local name=$1 logs=$2 writes=$3 list=$4
  shift 4
  local out="$WORK/namespaced-$name" fullname=${FULLNAME:-kausa} subject=${SUBJECT:-kausa}
  local release=${RELEASE_NAME:-$RELEASE} since=$FAILURES history=${HISTORY:-no}
  # shellcheck disable=SC2206 # split on spaces on purpose
  local extra=(${EXTRA-})
  local IFS=,
  # shellcheck disable=SC2206 # split on commas on purpose
  local namespaces=($list) namespaces_checked
  unset IFS
  if ! render "$out" "$release" "$CHART" --set rbac.scope=namespaced \
    --set "rbac.namespaces={$list}" "$@"; then
    fail "namespaced/$name does not render: $(cat "$out.err")"
    return 0
  fi
  # Nothing cluster-scoped but the EXTRA objects (the history's CRD and reader role).
  cluster_scoped "$out" >"$out.scoped"
  expected_cluster_scoped ${extra[@]+"${extra[@]}"} >"$out.scoped-expected"
  diff -u "$out.scoped-expected" "$out.scoped" >"$out.scoped-diff" ||
    fail "namespaced/$name: the cluster-scoped objects: $(cat "$out.scoped-diff")"
  [ "$(count_docs "$out" ClusterRoleBinding)" = 0 ] ||
    fail "namespaced/$name: renders a ClusterRoleBinding"
  [ "$(count_docs "$out" Namespace)" = 0 ] || fail "namespaced/$name: renders a Namespace"
  local roles bindings ns
  roles=$(count_docs "$out" Role)
  bindings=$(count_docs "$out" RoleBinding)
  if [ "$roles" != "${#namespaces[@]}" ] || [ "$bindings" != "${#namespaces[@]}" ]; then
    fail "namespaced/$name: $roles Roles and $bindings RoleBindings for ${#namespaces[@]}" \
      "namespaces, not one each per namespace"
    namespaces_checked=()
  else
    namespaces_checked=("${namespaces[@]}")
  fi
  expected_rules "$logs" "$writes" "$history" >"$out.expected"
  for ns in ${namespaces_checked[@]+"${namespaces_checked[@]}"}; do
    if [ "$(count_docs "$out" Role "$ns")" != 1 ] ||
      [ "$(count_docs "$out" RoleBinding "$ns")" != 1 ]; then
      fail "namespaced/$name: not exactly one Role and one RoleBinding in $ns"
      continue
    fi
    for kind in Role RoleBinding; do
      docs "$out" "$kind" "$ns" | grep -qx "  name: $fullname" ||
        fail "namespaced/$name: the $kind in $ns is not named $fullname"
      docs "$out" "$kind" "$ns" | grep -qx "    app.kubernetes.io/instance: $release" ||
        fail "namespaced/$name: the $kind in $ns lacks the instance label"
      docs "$out" "$kind" "$ns" | grep -qx '    app.kubernetes.io/name: kausa' ||
        fail "namespaced/$name: the $kind in $ns lacks the name label"
    done
    docs "$out" Role "$ns" | section rules >"$out.$ns.rules"
    diff -u "$out.expected" "$out.$ns.rules" >"$out.$ns.diff" ||
      fail "namespaced/$name: the rules of the Role in $ns: $(cat "$out.$ns.diff")"
    docs "$out" RoleBinding "$ns" | section roleRef >"$out.$ns.ref"
    for line in 'apiGroup:rbac.authorization.k8s.io' 'kind:Role' "name:$fullname"; do
      grep -qx -- "$line" "$out.$ns.ref" ||
        fail "namespaced/$name: the roleRef in $ns lacks $line"
    done
    docs "$out" RoleBinding "$ns" | section subjects >"$out.$ns.subjects"
    [ "$(grep -c '^-' "$out.$ns.subjects")" = 1 ] ||
      fail "namespaced/$name: the RoleBinding in $ns has not exactly one subject"
    for line in '-kind:ServiceAccount' "name:$subject" "namespace:$NAMESPACE"; do
      grep -qx -- "$line" "$out.$ns.subjects" ||
        fail "namespaced/$name: the subject in $ns lacks $line"
    done
  done
  {
    echo '- run'
    for ns in "${namespaces[@]}"; do
      echo '- --namespace'
      echo "- \"$ns\""
    done
  } >"$out.args-expected"
  args "$out" | head -n $((1 + 2 * ${#namespaces[@]})) >"$out.args"
  diff -u "$out.args-expected" "$out.args" >"$out.args-diff" ||
    fail "namespaced/$name: the --namespace args after run: $(cat "$out.args-diff")"
  [ "$(args "$out" | grep -cx -- '- --namespace')" = "${#namespaces[@]}" ] ||
    fail "namespaced/$name: not one --namespace per entry"
  if [ "$history" = yes ]; then
    args "$out" | grep -qx -- '- --history' || fail "namespaced/$name: no --history arg"
  else
    args "$out" | grep -q -- '^- --history' && fail "namespaced/$name: passes a --history arg"
  fi
  check_rollouts_arg "namespaced/$name" "$out"
  passed "$since" "namespaced/$name"
}

check_namespaced endpoint yes yes e2e-a,e2e-b "${ENDPOINT[@]}"
check_namespaced dry-run-no-logs no no e2e-a,e2e-b "${DRY_NO_LOGS[@]}"
check_namespaced dry-run yes no e2e-a,e2e-b "${DRY[@]}"
check_namespaced no-events yes no e2e-a "${NO_EVENTS[@]}"
check_namespaced default no no e2e-a
# The listed order is kept, and the release namespace may be listed.
check_namespaced order no no "zeta,alpha,$NAMESPACE"
check_namespaced max no no "$NAMES32"
check_namespaced long-name no no "$LONG63,0,a-1"
# --set reads 123 as a number: it is still a namespace name, passed as a string.
check_namespaced number no no 123
RELEASE_NAME=tenant FULLNAME=tenant-kausa SUBJECT=tenant-kausa \
  check_namespaced release-name no no e2e-a
FULLNAME=kausa SUBJECT=reader check_namespaced service-account no no e2e-a \
  --set serviceAccount.name=reader
# Comparing with the cluster scope: the Roles get exactly the ClusterRole's rules.
for name in endpoint dry-run-no-logs dry-run no-events default; do
  for rules in "$WORK/namespaced-$name".*.rules; do
    [ -f "$rules" ] || continue
    diff -q "$WORK/cluster-$name.rules" "$rules" >/dev/null 2>&1 ||
      fail "namespaced/$name: a Role's rules differ from the ClusterRole's ($rules)"
  done
done

# With the Deployment watch off, the Roles lose the deployments rule as the ClusterRole does.
ROLLOUTS=no check_namespaced default-no-rollouts no no e2e-a "${ROLLOUTS_OFF[@]}"
ROLLOUTS=no check_namespaced endpoint-no-rollouts yes yes e2e-a,e2e-b "${ENDPOINT[@]}" \
  "${ROLLOUTS_OFF[@]}"
ROLLOUTS=no check_namespaced dry-run-no-logs-no-rollouts no no e2e-a,e2e-b "${DRY_NO_LOGS[@]}" \
  "${ROLLOUTS_OFF[@]}"
for name in default-no-rollouts endpoint-no-rollouts dry-run-no-logs-no-rollouts; do
  for rules in "$WORK/namespaced-$name".*.rules; do
    [ -f "$rules" ] || continue
    diff -q "$WORK/cluster-$name.rules" "$rules" >/dev/null 2>&1 ||
      fail "namespaced/$name: a Role's rules differ from the ClusterRole's ($rules)"
  done
done

# A namespace that is a number must stay a string in metadata.namespace.
for kind in Role RoleBinding; do
  docs "$WORK/namespaced-number" "$kind" 123 | grep -Eqx "  namespace: (\"123\"|'123')" ||
    fail "namespaced/number: the $kind's metadata.namespace is not the string \"123\""
done

# --- Stuck rollouts (docs/adr/0018-stuck-rollouts.md) -------------------------------------------

# One value drives the grant and the watch: rollouts=false removes the rule and passes
# --no-rollouts (checked above); any other value, the key missing included, is on and renders like
# the default. There is no refusal.
check_cluster rollouts-true no no --set rollouts=true
check_cluster rollouts-null no no --set rollouts=null
check_cluster rollouts-string no no --set rollouts=no
ROLLOUTS=no check_cluster rollouts-string-false no no --set-string rollouts=false
check_namespaced rollouts-true no no e2e-a --set rollouts=true
check_namespaced rollouts-null no no e2e-a --set rollouts=null
ROLLOUTS=no check_namespaced rollouts-string-false no no e2e-a --set-string rollouts=false
for pair in cluster-rollouts-true:cluster-default cluster-rollouts-null:cluster-default \
  cluster-rollouts-string:cluster-default namespaced-rollouts-true:namespaced-default \
  namespaced-rollouts-null:namespaced-default; do
  out="$WORK/${pair%%:*}" compare="$WORK/${pair#*:}"
  if [ -f "$out" ] && [ -f "$compare" ] && cmp -s "$compare" "$out"; then
    ok "${pair%%:*} renders like ${pair#*:}"
  else
    fail "${pair%%:*} does not render like ${pair#*:}:" \
      "$(diff "$compare" "$out" 2>&1 | head -20)"
  fi
done

# The --no-rollouts arg comes right after where --no-logs goes.
if [ -f "$WORK/cluster-dry-run-no-logs-no-rollouts" ]; then
  args "$WORK/cluster-dry-run-no-logs-no-rollouts" | grep -A1 -x -- '- --no-logs' | tail -n 1 |
    grep -qx -- '- --no-rollouts' ||
    fail "cluster/dry-run-no-logs-no-rollouts: --no-rollouts does not follow --no-logs"
fi

# A release made before the Deployment watch (helm upgrade --reuse-values from v0.3) renders with
# values that have no rollouts key: a copy of the chart without it renders like the chart's
# defaults, in both scopes, so the watch and its rule come on.
NOROLLOUTS_CHART="$WORK/norollouts/kausa"
mkdir -p "$WORK/norollouts"
cp -R "$CHART" "$NOROLLOUTS_CHART"
grep -v '^rollouts:' "$CHART/values.yaml" >"$NOROLLOUTS_CHART/values.yaml"
grep -q '^rollouts:' "$CHART/values.yaml" || fail "values.yaml has no top-level rollouts key"
for pair in cluster-default:cluster cluster-endpoint:cluster,endpoint \
  namespaced-default:namespaced namespaced-endpoint:namespaced,endpoint; do
  name=${pair%%:*} sets=${pair#*:}
  set_values=()
  case ",$sets," in
    *,cluster,*) set_values+=(--set rbac.scope=cluster) ;;
    *,namespaced,*) set_values+=(--set rbac.scope=namespaced) ;;
  esac
  case "$name" in
    namespaced-default) set_values+=(--set 'rbac.namespaces={e2e-a}') ;;
    namespaced-endpoint) set_values+=(--set 'rbac.namespaces={e2e-a,e2e-b}') ;;
  esac
  case ",$sets," in *,endpoint,*) set_values+=("${ENDPOINT[@]}") ;; esac
  out="$WORK/norollouts-$name"
  if ! render "$out" "$RELEASE" "$NOROLLOUTS_CHART" "${set_values[@]}"; then
    fail "no rollouts key, $name: does not render: $(cat "$out.err")"
  elif [ ! -f "$WORK/$name" ]; then
    fail "no rollouts key, $name: no render of the chart to compare with"
  elif cmp -s "$WORK/$name" "$out"; then
    ok "no rollouts key, $name: renders like the chart"
  else
    fail "no rollouts key, $name: renders unlike the chart: $(diff "$WORK/$name" "$out" | head -20)"
  fi
done

# helm lint --strict with the Deployment watch off.
if "$HELM" lint --strict "$CHART" --set rbac.scope=cluster "${ROLLOUTS_OFF[@]}" \
  >"$WORK/lint-rollouts" 2>&1; then
  ok "helm lint --strict with rollouts=false"
else
  fail "helm lint --strict with rollouts=false: $(cat "$WORK/lint-rollouts")"
fi
if "$HELM" lint --strict "$CHART" --set rbac.scope=namespaced --set 'rbac.namespaces={e2e-a}' \
  "${ROLLOUTS_OFF[@]}" >"$WORK/lint-rollouts" 2>&1; then
  ok "helm lint --strict with rollouts=false, namespaced"
else
  fail "helm lint --strict with rollouts=false, namespaced: $(cat "$WORK/lint-rollouts")"
fi

# --- Explanation history (docs/adr/0014-explanation-history.md) ---------------------------------

HISTORY_ON=(--set history.enabled=true)
CRD_NAME=explanations.kausa.fedeam.com
HISTORY_OBJECTS="ClusterRole/kausa-explanation-reader CustomResourceDefinition/$CRD_NAME"

# columns: the printer columns of the CRD documents on stdin, one per line as
# NAME|type|jsonPath|priority, with the name in capitals (kubectl prints it so) and no quotes.
columns() {
  awk '
    function flush() {
      if (any) print toupper(f["name"]) "|" f["type"] "|" f["jsonPath"] "|" f["priority"]
      split("", f); any = 0
    }
    !on && /^ *additionalPrinterColumns:$/ { on = 1; match($0, /^ */); base = RLENGTH; next }
    on {
      if ($0 ~ /^ *$/) next
      match($0, /^ */); ind = RLENGTH; s = substr($0, ind + 1)
      if (ind < base || (ind == base && s !~ /^- /)) exit
      if (s ~ /^- /) { flush(); s = substr(s, 3) }
      key = s; sub(/:.*/, "", key)
      value = s; sub(/^[^:]*: */, "", value); gsub(/^["'\'']|["'\'']$/, "", value)
      f[key] = value; any = 1
    }
    END { flush() }
  '
}

expected_columns() {
  printf '%s\n' \
    'KIND|string|.spec.target.kind|' \
    'TARGET|string|.spec.target.name|' \
    'RULE|string|.spec.rule|' \
    'COUNT|integer|.status.occurrences|' \
    'LAST|date|.status.lastTime|' \
    'AGE|date|.metadata.creationTimestamp|' \
    'CONFIDENCE|string|.status.entries[0].confidence|1' \
    'SUMMARY|string|.status.entries[0].summary|1'
  # If the kind test (T0) shows that a printer column refuses the [0] path, the last two go.
}

# check_crd NAME FILE [RELEASE]: the CustomResourceDefinition of the history, rendered from
# charts/kausa/files/explanation-schema.json (fromJson | toYaml).
check_crd() {
  local name=$1 out=$2 release=${3:-$RELEASE} since=$FAILURES crd
  crd="$out.crd"
  docs "$out" CustomResourceDefinition >"$crd"
  if [ "$(grep -c '^%%$' "$crd" || true)" != 1 ]; then
    fail "$name: not exactly one CustomResourceDefinition"
    return 0
  fi
  grep -qx "  name: $CRD_NAME" "$crd" || fail "$name: the CRD is not named $CRD_NAME"
  grep -q '^apiVersion: apiextensions.k8s.io/v1$' "$crd" || fail "$name: the CRD's apiVersion"
  for line in 'group: kausa.fedeam.com' 'scope: Namespaced' 'kind: Explanation' \
    'listKind: ExplanationList' 'plural: explanations' 'singular: explanation' \
    'name: v1alpha1' 'served: true' 'storage: true'; do
    grep -Eq "^ *(- )?$line\$" "$crd" || fail "$name: the CRD lacks '$line'"
  done
  [ "$(grep -cE '^ *served: ' "$crd" || true)" = 1 ] || fail "$name: not exactly one version"
  for key in shortNames categories subresources; do
    grep -Eq "^ *$key:" "$crd" && fail "$name: the CRD has $key"
  done
  if grep -Eq '^ *strategy: ' "$crd"; then
    grep -Eq '^ *strategy: None$' "$crd" || fail "$name: the CRD's conversion is not None"
  fi
  grep -q 'helm.sh/resource-policy' "$crd" && fail "$name: the CRD has a resource policy"
  for line in "    app.kubernetes.io/instance: $release" '    app.kubernetes.io/name: kausa' \
    '    app.kubernetes.io/managed-by: Helm'; do
    grep -qx "$line" "$crd" || fail "$name: the CRD lacks the label '${line#    }'"
  done
  grep -q '^    helm.sh/chart: kausa-' "$crd" || fail "$name: the CRD lacks the chart label"
  columns <"$crd" >"$crd.columns"
  expected_columns >"$crd.columns-expected"
  diff -u "$crd.columns-expected" "$crd.columns" >"$crd.columns-diff" ||
    fail "$name: the printer columns: $(cat "$crd.columns-diff")"
  # The schema: spec required and immutable, every limit a plain integer.
  grep -Eq "^ *(- )?message: [\"']?spec is immutable[\"']?\$" "$crd" ||
    fail "$name: the CRD lacks the message 'spec is immutable'"
  grep -Eq "^ *(- )?rule: [\"']?self == oldSelf[\"']?\$" "$crd" ||
    fail "$name: the CRD lacks the rule 'self == oldSelf'"
  grep -Eq "^ *pattern: [\"']?\\^D\\[0-9\\][{]3[}]\\\$[\"']?\$" "$crd" ||
    fail "$name: the CRD lacks the pattern of spec.rule"
  grep -Eqx ' *maxLength: 300' "$crd" || fail "$name: no 'maxLength: 300' as an integer"
  grep -E '^ *(maxLength|maxItems|minimum): ' "$crd" | grep -vE ': [0-9]+$' >"$crd.numbers" || true
  [ -s "$crd.numbers" ] && fail "$name: limits that are not plain integers: $(cat "$crd.numbers")"
  [ "$(grep -E '^ *maxLength: ' "$crd" | sed 's/.*: //' | sort -n | paste -sd' ' -)" = \
    '4 16 20 20 20 63 253 253 253 300 300 600' ] ||
    fail "$name: the maxLength values are not the contract's:" \
      "$(grep -E '^ *maxLength: ' "$crd" | sed 's/.*: //' | sort -n | paste -sd' ' -)"
  [ "$(grep -E '^ *maxItems: ' "$crd" | sed 's/.*: //' | paste -sd' ' -)" = '5 5' ] ||
    fail "$name: the maxItems values are not 5 and 5"
  [ "$(grep -cE '^ *format: date-time$' "$crd" || true)" = 3 ] ||
    fail "$name: not three date-time fields"
  [ "$(grep -cE '^ *description:' "$crd" || true)" -ge 19 ] ||
    fail "$name: fewer descriptions than the 19 properties"
  grep -Eq '^ +metadata:' "$crd" && fail "$name: the schema lists metadata"
  passed "$since" "$name: the CustomResourceDefinition"
}

rule_reader() {
  printf '%s\n' '-apiGroups:["kausa.fedeam.com"]' 'resources:["explanations"]' \
    'verbs:["get","list","watch"]'
}

# check_reader NAME FILE [FULLNAME]: the reader ClusterRole, aggregated to view only.
check_reader() {
  local name=$1 out=$2 fullname=${3:-kausa} since=$FAILURES reader
  reader="$fullname-explanation-reader"
  if [ "$(count_docs "$out" ClusterRole "" "$reader")" != 1 ]; then
    fail "$name: not exactly one ClusterRole $reader"
    return 0
  fi
  docs "$out" ClusterRole "" "$reader" >"$out.reader"
  grep -Eqx "    rbac.authorization.k8s.io/aggregate-to-view: (\"true\"|'true')" "$out.reader" ||
    fail "$name: the reader is not aggregated to view"
  grep -Eq 'aggregate-to-(edit|admin)' "$out.reader" &&
    fail "$name: the reader is aggregated to edit or admin"
  grep -qx '    app.kubernetes.io/name: kausa' "$out.reader" ||
    fail "$name: the reader lacks the name label"
  section rules <"$out.reader" >"$out.reader-rules"
  rule_reader >"$out.reader-expected"
  diff -u "$out.reader-expected" "$out.reader-rules" >"$out.reader-diff" ||
    fail "$name: the reader's rules: $(cat "$out.reader-diff")"
  passed "$since" "$name: the reader ClusterRole"
}

# check_history_args NAME FILE BASE DAYS: FILE's args are BASE's (the same values without the
# history) with --history --history-days "DAYS" where --no-events would go, before
# --metrics-address.
check_history_args() {
  local name=$1 out=$2 base=$3 days=$4
  [ -f "$out" ] && [ -f "$base" ] || return 0
  args "$base" | awk -v days="$days" '
    !done && $0 == "- --metrics-address" { insert() }
    { print }
    END { if (!done) insert() }
    function insert() {
      print "- --history"; print "- --history-days"; print "- \"" days "\""; done = 1
    }
  ' >"$out.args-expected"
  args "$out" >"$out.args"
  if diff -u "$out.args-expected" "$out.args" >"$out.args-diff"; then
    ok "$name: --history --history-days \"$days\" in place"
  else
    fail "$name: the history args: $(cat "$out.args-diff")"
  fi
}

# History on, in cluster scope: the CRD, the reader, the rule last and the two flags.
HISTORY=yes EXTRA="$HISTORY_OBJECTS" check_cluster history yes yes "${ENDPOINT[@]}" \
  "${HISTORY_ON[@]}"
check_crd cluster/history "$WORK/cluster-history"
check_reader cluster/history "$WORK/cluster-history"
check_history_args cluster/history "$WORK/cluster-history" "$WORK/cluster-endpoint" 7
for days in 1 30; do
  HISTORY=yes EXTRA="$HISTORY_OBJECTS" check_cluster "history-days-$days" yes yes \
    "${ENDPOINT[@]}" "${HISTORY_ON[@]}" --set "history.days=$days"
  check_history_args "cluster/history-days-$days" "$WORK/cluster-history-days-$days" \
    "$WORK/cluster-endpoint" "$days"
done
# A value file may quote the number.
HISTORY=yes EXTRA="$HISTORY_OBJECTS" check_cluster history-days-string yes yes \
  "${ENDPOINT[@]}" "${HISTORY_ON[@]}" --set-string history.days=12
check_history_args cluster/history-days-string "$WORK/cluster-history-days-string" \
  "$WORK/cluster-endpoint" 12
# Without the CRD: the rule and the flags only, and no reader either way.
HISTORY=yes check_cluster history-no-crd yes yes "${ENDPOINT[@]}" "${HISTORY_ON[@]}" \
  --set history.installCRD=false --set history.readers.aggregate=false
check_history_args cluster/history-no-crd "$WORK/cluster-history-no-crd" \
  "$WORK/cluster-endpoint" 7
HISTORY=yes check_cluster history-no-crd-aggregate yes yes "${ENDPOINT[@]}" "${HISTORY_ON[@]}" \
  --set history.installCRD=false
# The CRD without the reader.
HISTORY=yes EXTRA="CustomResourceDefinition/$CRD_NAME" check_cluster history-no-reader yes yes \
  "${ENDPOINT[@]}" "${HISTORY_ON[@]}" --set history.readers.aggregate=false
check_crd cluster/history-no-reader "$WORK/cluster-history-no-reader"
# No history without an endpoint or in dry-run: no CRD, reader, rule or flag.
check_cluster history-dry-run yes no "${ENDPOINT[@]}" "${HISTORY_ON[@]}" "${DRY[@]}"
check_cluster history-dry-run-only yes no "${HISTORY_ON[@]}" "${DRY[@]}"
check_cluster history-no-endpoint no no "${HISTORY_ON[@]}"
check_cluster history-off yes yes "${ENDPOINT[@]}" --set history.enabled=false \
  --set history.installCRD=true --set history.days=30
# events=false: the history only, without the Event writes.
HISTORY=yes EXTRA="$HISTORY_OBJECTS" check_cluster history-no-events yes no "${NO_EVENTS[@]}" \
  "${HISTORY_ON[@]}"
check_crd cluster/history-no-events "$WORK/cluster-history-no-events"
check_history_args cluster/history-no-events "$WORK/cluster-history-no-events" \
  "$WORK/cluster-no-events" 7

# Another release: the reader takes its full name; the CRD's name is the same for every release.
out="$WORK/cluster-history-tenant"
if render "$out" tenant "$CHART" --set rbac.scope=cluster "${ENDPOINT[@]}" "${HISTORY_ON[@]}"; then
  since=$FAILURES
  [ "$(count_docs "$out" ClusterRole "" tenant-kausa)" = 1 ] ||
    fail "cluster/history-tenant: no ClusterRole tenant-kausa"
  check_reader cluster/history-tenant "$out" tenant-kausa
  check_crd cluster/history-tenant "$out" tenant
  passed "$since" "cluster/history-tenant"
else
  fail "cluster/history-tenant does not render: $(cat "$out.err")"
fi

# Namespaced scope: the rule in every Role; the CRD and the reader are the only cluster-scoped
# objects, and with installCRD=false there is nothing cluster-scoped.
HISTORY=yes EXTRA="$HISTORY_OBJECTS" check_namespaced history yes yes e2e-a,e2e-b \
  "${ENDPOINT[@]}" "${HISTORY_ON[@]}"
check_crd namespaced/history "$WORK/namespaced-history"
check_reader namespaced/history "$WORK/namespaced-history"
check_history_args namespaced/history "$WORK/namespaced-history" "$WORK/namespaced-endpoint" 7
HISTORY=yes check_namespaced history-no-crd yes yes e2e-a,e2e-b "${ENDPOINT[@]}" \
  "${HISTORY_ON[@]}" --set history.installCRD=false
HISTORY=yes EXTRA="$HISTORY_OBJECTS" check_namespaced history-no-events yes no e2e-a \
  "${NO_EVENTS[@]}" "${HISTORY_ON[@]}"
check_namespaced history-dry-run yes no e2e-a,e2e-b "${ENDPOINT[@]}" "${HISTORY_ON[@]}" \
  "${DRY[@]}"
for pair in history:history history-no-crd:history-no-crd history-no-events:history-no-events \
  history-dry-run:history-dry-run; do
  cluster=${pair%%:*} namespaced=${pair#*:}
  for rules in "$WORK/namespaced-$namespaced".*.rules; do
    [ -f "$rules" ] || continue
    diff -q "$WORK/cluster-$cluster.rules" "$rules" >/dev/null 2>&1 ||
      fail "namespaced/$namespaced: a Role's rules differ from the ClusterRole's ($rules)"
  done
done

# The defaults render nothing of the history (checked by every check_cluster and
# check_namespaced above without HISTORY and EXTRA), and history=null renders like the defaults.
for values in default endpoint; do
  case $values in
    default) set_values=() ;;
    endpoint) set_values=("${ENDPOINT[@]}") ;;
  esac
  out="$WORK/history-null-$values"
  if render "$out" "$RELEASE" "$CHART" --set rbac.scope=cluster \
    ${set_values[@]+"${set_values[@]}"} --set history=null; then
    if cmp -s "$WORK/cluster-$values" "$out"; then
      ok "history=null renders like $values"
    else
      fail "history=null does not render like $values:" \
        "$(diff "$WORK/cluster-$values" "$out" | head -20)"
    fi
  else
    fail "history=null does not render with $values: $(cat "$out.err")"
  fi
done

# A release made before the history (helm upgrade --reuse-values from v0.2) renders with values
# that have no history map: a copy of the chart without it renders like the chart's defaults, and
# a history map given in parts takes the defaults of the missing keys (7 days, the CRD, the
# reader), never a nil-pointer error.
NOMAP_CHART="$WORK/nomap/kausa"
mkdir -p "$WORK/nomap"
cp -R "$CHART" "$NOMAP_CHART"
awk '/^history:/ { skip = 1; next } skip && /^[^ ]/ { skip = 0 } !skip' "$CHART/values.yaml" \
  >"$NOMAP_CHART/values.yaml"
grep -q '^history:' "$NOMAP_CHART/values.yaml" && fail "the chart copy still has a history map"
for pair in default: endpoint:endpoint history:endpoint,history \
  history-no-crd-aggregate:endpoint,history,no-crd \
  history-no-reader:endpoint,history,no-reader history-days-30:endpoint,history,days-30; do
  name=${pair%%:*} sets=${pair#*:}
  set_values=()
  case ",$sets," in *,endpoint,*) set_values+=("${ENDPOINT[@]}") ;; esac
  case ",$sets," in *,history,*) set_values+=("${HISTORY_ON[@]}") ;; esac
  case ",$sets," in *,no-crd,*) set_values+=(--set history.installCRD=false) ;; esac
  case ",$sets," in *,no-reader,*) set_values+=(--set history.readers.aggregate=false) ;; esac
  case ",$sets," in *,days-30,*) set_values+=(--set history.days=30) ;; esac
  out="$WORK/nomap-$name"
  if ! render "$out" "$RELEASE" "$NOMAP_CHART" --set rbac.scope=cluster \
    ${set_values[@]+"${set_values[@]}"}; then
    if grep -qi 'nil pointer' "$out.err"; then
      fail "no history map, $name: a nil-pointer error: $(cat "$out.err")"
    else
      fail "no history map, $name: does not render: $(cat "$out.err")"
    fi
  elif [ ! -f "$WORK/cluster-$name" ]; then
    fail "no history map, $name: no render of the chart to compare with"
  elif cmp -s "$WORK/cluster-$name" "$out"; then
    ok "no history map, $name: renders like the chart"
  else
    fail "no history map, $name: renders unlike the chart:" \
      "$(diff "$WORK/cluster-$name" "$out" | head -20)"
  fi
done

# helm lint --strict with the history on.
if "$HELM" lint --strict "$CHART" --set rbac.scope=cluster "${ENDPOINT[@]}" "${HISTORY_ON[@]}" \
  >"$WORK/lint-history" 2>&1; then
  ok "helm lint --strict with the history on"
else
  fail "helm lint --strict with the history on: $(cat "$WORK/lint-history")"
fi
if "$HELM" lint --strict "$CHART" --set rbac.scope=namespaced --set 'rbac.namespaces={e2e-a}' \
  "${ENDPOINT[@]}" "${HISTORY_ON[@]}" >"$WORK/lint-history" 2>&1; then
  ok "helm lint --strict with the history on, namespaced"
else
  fail "helm lint --strict with the history on, namespaced: $(cat "$WORK/lint-history")"
fi

# --- NOTES.txt ----------------------------------------------------------------------------------

notes "$WORK/notes-cluster" --set rbac.scope=cluster "${ENDPOINT[@]}"
if [ -s "$WORK/notes-cluster" ]; then
  since=$FAILURES
  head -n 6 "$WORK/notes-cluster" >"$WORK/notes-cluster.head"
  for words in 'every namespace' 'rbac.scope=cluster' 'Pods' 'Events' 'log'; do
    grep -qiF "$words" "$WORK/notes-cluster.head" ||
      fail "NOTES.txt in cluster scope: its first lines do not mention '$words'" \
        "(kausa reads Pods, Events and logs in every namespace): $(cat "$WORK/notes-cluster.head")"
  done
  passed "$since" "NOTES.txt in cluster scope"
fi
notes "$WORK/notes-namespaced" --set rbac.scope=namespaced --set 'rbac.namespaces={e2e-a,e2e-b}' \
  "${ENDPOINT[@]}"
if [ -s "$WORK/notes-namespaced" ]; then
  since=$FAILURES
  grep -qF 'namespaced' "$WORK/notes-namespaced" ||
    fail "NOTES.txt in namespaced scope does not name the scope"
  for ns in e2e-a e2e-b; do
    grep -qF "$ns" "$WORK/notes-namespaced" ||
      fail "NOTES.txt in namespaced scope does not name $ns"
    grep -qF "kubectl get events.events.k8s.io -n $ns --field-selector reportingController=kausa" \
      "$WORK/notes-namespaced" || fail "NOTES.txt in namespaced scope lacks the -n $ns command"
  done
  grep -qF -- '-A --field-selector' "$WORK/notes-namespaced" &&
    fail "NOTES.txt in namespaced scope still prints the -A command"
  passed "$since" "NOTES.txt in namespaced scope"
fi
for scope in cluster namespaced; do
  file="$WORK/notes-$scope"
  [ -s "$file" ] || continue
  since=$FAILURES
  for words in 'rbac.scope' 'upgrade' '--reuse-values'; do
    grep -qiF -- "$words" "$file" ||
      fail "NOTES.txt in $scope scope does not say that rbac.scope is set on every upgrade" \
        "(values file or --reuse-values): no '$words'"
  done
  passed "$since" "NOTES.txt in $scope scope says to set rbac.scope on every upgrade"
done

# NOTES.txt with the history: how to read it, per scope, and that turning it off deletes it.
# collapsed FILE: the text on one line, so that a sentence may wrap.
collapsed() { tr '\n' ' ' <"$1" | tr -s ' '; }
notes "$WORK/notes-history-cluster" --set rbac.scope=cluster "${ENDPOINT[@]}" "${HISTORY_ON[@]}"
notes "$WORK/notes-history-namespaced" --set rbac.scope=namespaced \
  --set 'rbac.namespaces={e2e-a,e2e-b}' "${ENDPOINT[@]}" "${HISTORY_ON[@]}"
notes "$WORK/notes-history-no-crd" --set rbac.scope=cluster "${ENDPOINT[@]}" "${HISTORY_ON[@]}" \
  --set history.installCRD=false
for scope in cluster namespaced no-crd; do
  file="$WORK/notes-history-$scope"
  [ -s "$file" ] || { fail "NOTES.txt with the history ($scope) is empty"; continue; }
  since=$FAILURES
  grep -Eq '^ *kubectl get explanation -n <namespace> <name> -o yaml *$' "$file" ||
    fail "NOTES.txt with the history ($scope) lacks" \
      "'kubectl get explanation -n <namespace> <name> -o yaml'"
  collapsed "$file" | grep -qi 'uninstalling' ||
    fail "NOTES.txt with the history ($scope) does not say what uninstalling does"
  collapsed "$file" | grep -qF 'deletes the CustomResourceDefinition and every Explanation' ||
    fail "NOTES.txt with the history ($scope) does not say that it" \
      "'deletes the CustomResourceDefinition and every Explanation'"
  passed "$since" "NOTES.txt with the history ($scope)"
done
if [ -s "$WORK/notes-history-cluster" ]; then
  grep -Eq '^ *kubectl get explanations -A *$' "$WORK/notes-history-cluster" ||
    fail "NOTES.txt with the history in cluster scope lacks 'kubectl get explanations -A'"
fi
if [ -s "$WORK/notes-history-namespaced" ]; then
  for ns in e2e-a e2e-b; do
    grep -Eq "^ *kubectl get explanations -n $ns *\$" "$WORK/notes-history-namespaced" ||
      fail "NOTES.txt with the history in namespaced scope lacks 'kubectl get explanations -n $ns'"
  done
  grep -q 'kubectl get explanations -A' "$WORK/notes-history-namespaced" &&
    fail "NOTES.txt with the history in namespaced scope prints the -A command"
fi
if [ -s "$WORK/notes-history-no-crd" ]; then
  collapsed "$WORK/notes-history-no-crd" | grep -qF "$CRD_NAME" ||
    fail "NOTES.txt with history.installCRD=false does not name $CRD_NAME"
  collapsed "$WORK/notes-history-no-crd" | grep -qi 'cluster administrator' ||
    fail "NOTES.txt with history.installCRD=false does not say a cluster administrator installs it"
fi
# Without the history (the defaults, or dryRun), nothing about Explanations.
notes "$WORK/notes-history-dry-run" --set rbac.scope=cluster "${ENDPOINT[@]}" "${HISTORY_ON[@]}" \
  "${DRY[@]}"
for file in "$WORK/notes-cluster" "$WORK/notes-namespaced" "$WORK/notes-history-dry-run"; do
  [ -s "$file" ] || continue
  grep -qE 'kubectl get explanation|CustomResourceDefinition' "$file" &&
    fail "NOTES.txt without the history tells how to read Explanations ($file)"
done

# NOTES.txt names the Deployments kausa reads only while the Deployment watch is on; with
# rollouts=false it keeps v0.3's sentence (docs/adr/0018-stuck-rollouts.md).
NS_AB=(--set rbac.scope=namespaced --set 'rbac.namespaces={e2e-a,e2e-b}')
notes "$WORK/notes-cluster-no-logs" --set rbac.scope=cluster
notes "$WORK/notes-namespaced-no-logs" "${NS_AB[@]}"
notes "$WORK/notes-cluster-no-rollouts" --set rbac.scope=cluster "${ENDPOINT[@]}" \
  "${ROLLOUTS_OFF[@]}"
notes "$WORK/notes-cluster-no-logs-no-rollouts" --set rbac.scope=cluster "${ROLLOUTS_OFF[@]}"
notes "$WORK/notes-namespaced-no-rollouts" "${NS_AB[@]}" "${ENDPOINT[@]}" "${ROLLOUTS_OFF[@]}"
notes "$WORK/notes-namespaced-no-logs-no-rollouts" "${NS_AB[@]}" "${ROLLOUTS_OFF[@]}"
for pair in \
  "cluster|kausa can read Pods, Events and Deployments in every namespace of the cluster, and container logs too." \
  "cluster-no-logs|kausa can read Pods, Events and Deployments in every namespace of the cluster." \
  "namespaced|can read Pods, Events, Deployments and container logs in them and nowhere else:" \
  "namespaced-no-logs|can read Pods, Events and Deployments in them and nowhere else:" \
  "cluster-no-rollouts|kausa can read Pods and Events in every namespace of the cluster, and container logs too." \
  "cluster-no-logs-no-rollouts|kausa can read Pods and Events in every namespace of the cluster." \
  "namespaced-no-rollouts|can read Pods, Events and container logs in them and nowhere else:" \
  "namespaced-no-logs-no-rollouts|can read Pods and Events in them and nowhere else:"; do
  name=${pair%%|*} sentence=${pair#*|}
  file="$WORK/notes-$name"
  [ -s "$file" ] || { fail "NOTES.txt ($name) is empty"; continue; }
  since=$FAILURES
  collapsed "$file" | grep -qF -- "$sentence" ||
    fail "NOTES.txt ($name) lacks '$sentence': $(collapsed "$file" | head -c 300)"
  case $name in
    *-no-rollouts)
      grep -qF 'Deployments' "$file" && fail "NOTES.txt ($name) names Deployments with rollouts=false"
      ;;
    *) grep -qF 'Deployments' "$file" || fail "NOTES.txt ($name) does not name Deployments" ;;
  esac
  passed "$since" "NOTES.txt ($name) on what kausa reads"
done

# With rollouts=false, the scope paragraph (up to "kausa is running in") is v0.3's own render with
# the same values, when that chart is in the clone (KAUSA_CHART_V03, default v0.3.0).
V03=${KAUSA_CHART_V03:-v0.3.0}
if git -C "$ROOT" rev-parse --verify --quiet "$V03^{commit}" >/dev/null 2>&1; then
  mkdir -p "$WORK/v03"
  git -C "$ROOT" archive "$V03" charts/kausa | tar -x -C "$WORK/v03"
  notes_chart "$WORK/v03/charts/kausa"
  scope_paragraph() { awk '/^kausa is running in / { exit } { print }' "$1"; }
  for name in cluster-no-rollouts cluster-no-logs-no-rollouts namespaced-no-rollouts \
    namespaced-no-logs-no-rollouts; do
    case $name in
      cluster-*) set_values=(--set rbac.scope=cluster) ;;
      *) set_values=("${NS_AB[@]}") ;;
    esac
    case $name in *-no-logs-*) ;; *) set_values+=("${ENDPOINT[@]}") ;; esac
    NOTES_FROM="$WORK/v03/charts/kausa" notes "$WORK/notes-v03-$name" "${set_values[@]}"
    if [ -s "$WORK/notes-v03-$name" ] && [ -s "$WORK/notes-$name" ] &&
      [ "$(scope_paragraph "$WORK/notes-v03-$name")" = \
        "$(scope_paragraph "$WORK/notes-$name")" ]; then
      ok "NOTES.txt ($name) has the scope paragraph of $V03"
    else
      fail "NOTES.txt ($name): the scope paragraph differs from $V03's:" \
        "$(diff <(scope_paragraph "$WORK/notes-v03-$name") <(scope_paragraph "$WORK/notes-$name"))"
    fi
  done
else
  echo "skipped: $V03 is not in this clone; NOTES.txt is checked against v0.3's sentences above"
fi

# --- Refusals -----------------------------------------------------------------------------------

# refuse NAME MESSAGE ARGS...: helm template fails, and its error contains MESSAGE exactly.
refuse() {
  local name=$1 message=$2
  shift 2
  local out="$WORK/refuse-$name"
  if render "$out" "$RELEASE" "$CHART" "$@"; then
    fail "refusal/$name renders; expected: $message"
  elif grep -qi 'nil pointer' "$out.err"; then
    fail "refusal/$name fails with a nil-pointer error: $(cat "$out.err")"
  elif ! grep -qF -- "$message" "$out.err"; then
    fail "refusal/$name: expected '$message', got: $(cat "$out.err")"
  else
    ok "refusal/$name"
  fi
}

NOT_SET='rbac.scope is not set: set rbac.scope=cluster to let kausa read every namespace, or rbac.scope=namespaced with rbac.namespaces'
NEEDS_LIST='rbac.scope=namespaced needs rbac.namespaces: the namespaces kausa may read'
CLUSTER_LIST='rbac.namespaces is set but rbac.scope is cluster, which grants every namespace: set rbac.scope=namespaced'
not_a_name() {
  echo "rbac.namespaces: '$1' is not a namespace name (lowercase letters, digits and '-', at most 63 characters)"
}

refuse defaults "$NOT_SET"
refuse rbac-null "$NOT_SET" --set rbac=null
refuse scope-null "$NOT_SET" --set rbac.scope=null
refuse scope-empty "$NOT_SET" --set rbac.scope=
refuse scope-unset-with-list "$NOT_SET" --set 'rbac.namespaces={a}'
refuse scope-all "rbac.scope must be cluster or namespaced, not 'all'" --set rbac.scope=all
refuse scope-capital "rbac.scope must be cluster or namespaced, not 'Cluster'" \
  --set rbac.scope=Cluster
refuse namespaced-no-list "$NEEDS_LIST" --set rbac.scope=namespaced
refuse namespaced-null-list "$NEEDS_LIST" --set rbac.scope=namespaced --set rbac.namespaces=null
refuse namespaced-empty-list "$NEEDS_LIST" --set rbac.scope=namespaced \
  --set-json 'rbac.namespaces=[]'
# --set 'rbac.namespaces={}' is a list of one empty name, not an empty list.
refuse empty-braces "$(not_a_name '')" --set rbac.scope=namespaced --set 'rbac.namespaces={}'
refuse cluster-with-list "$CLUSTER_LIST" --set rbac.scope=cluster --set 'rbac.namespaces={a}'
refuse capital "$(not_a_name Shop)" --set rbac.scope=namespaced --set 'rbac.namespaces={Shop}'
refuse too-long "$(not_a_name "$LONG64")" --set rbac.scope=namespaced \
  --set "rbac.namespaces={$LONG64}"
refuse leading-dash "$(not_a_name -a)" --set rbac.scope=namespaced --set 'rbac.namespaces={-a}'
refuse trailing-dash "$(not_a_name a-)" --set rbac.scope=namespaced --set 'rbac.namespaces={a-}'
refuse underscore "$(not_a_name a_b)" --set rbac.scope=namespaced --set 'rbac.namespaces={a_b}'
refuse dot "$(not_a_name a.b)" --set rbac.scope=namespaced --set 'rbac.namespaces={a.b}'
refuse empty-name "$(not_a_name '')" --set rbac.scope=namespaced --set 'rbac.namespaces[0]='
refuse twice "rbac.namespaces: 'a' is listed twice" --set rbac.scope=namespaced \
  --set 'rbac.namespaces={a,a}'
refuse twice-later "rbac.namespaces: 'b' is listed twice" --set rbac.scope=namespaced \
  --set 'rbac.namespaces={b,c,b}'
refuse too-many "rbac.namespaces lists 33 namespaces; kausa watches at most 32 (use rbac.scope=cluster instead)" \
  --set rbac.scope=namespaced --set "rbac.namespaces={$NAMES32,team-33}"
# The order of the checks: the scope, then the list against the scope, the names, the repeats and
# the count, and the checks of v0.1 last.
refuse order-scope-first "$NOT_SET" --set model.endpoint=https://api.example.com/v1
refuse order-value-before-list "rbac.scope must be cluster or namespaced, not 'all'" \
  --set rbac.scope=all --set 'rbac.namespaces={Shop}'
refuse order-cluster-list-before-names "$CLUSTER_LIST" --set rbac.scope=cluster \
  --set 'rbac.namespaces={Shop}'
refuse order-names-before-repeats "$(not_a_name Shop)" --set rbac.scope=namespaced \
  --set 'rbac.namespaces={Shop,a,a}'
refuse order-names-before-count "$(not_a_name Shop)" --set rbac.scope=namespaced \
  --set "rbac.namespaces={$NAMES32,Shop}"
refuse order-repeats-before-count "rbac.namespaces: 'team-1' is listed twice" \
  --set rbac.scope=namespaced --set "rbac.namespaces={$NAMES32,team-1}"
refuse v0.1-checks-stay "model.name is required with model.endpoint" --set rbac.scope=cluster \
  --set model.endpoint=https://api.example.com/v1
# rollouts adds no refusal and changes none (docs/adr/0018-stuck-rollouts.md).
refuse rollouts-off-scope-first "$NOT_SET" --set rollouts=false
refuse rollouts-off-namespaced-no-list "$NEEDS_LIST" --set rbac.scope=namespaced \
  --set rollouts=false
refuse rollouts-off-v0.1-checks-stay "model.name is required with model.endpoint" \
  --set rbac.scope=cluster --set rollouts=false --set model.endpoint=https://api.example.com/v1
# history.days, the last check, with the history on and off (helm lint does not fail on a
# template's fail; helm template does).
not_days() { echo "history.days must be a whole number of days from 1 to 30, not '$1'"; }
for days in 0 31 abc 7.5 -1 100; do
  refuse "days-$days-on" "$(not_days "$days")" --set rbac.scope=cluster "${ENDPOINT[@]}" \
    "${HISTORY_ON[@]}" --set "history.days=$days"
  refuse "days-$days-off" "$(not_days "$days")" --set rbac.scope=cluster \
    --set "history.days=$days"
done
refuse days-namespaced "$(not_days 31)" --set rbac.scope=namespaced \
  --set 'rbac.namespaces={e2e-a}' "${HISTORY_ON[@]}" --set history.days=31
refuse days-string "$(not_days 7d)" --set rbac.scope=cluster --set-string history.days=7d
refuse order-days-after-rbac "$NOT_SET" --set history.days=0
refuse order-days-last "model.name is required with model.endpoint" --set rbac.scope=cluster \
  --set model.endpoint=https://api.example.com/v1 --set history.days=0

# --- Notifications (docs/adr/0015-notifications.md) ---------------------------------------------

NOTIFY_SECRET=team-webhook
NOTIFY_ON=(--set notifications.enabled=true --set "notifications.existingSecret=$NOTIFY_SECRET")
CHAT_ID=-1001234567890
NP_ON=(--set networkPolicy.enabled=true --set 'networkPolicy.apiServer.cidrs[0]=192.0.2.1/32'
  --set 'networkPolicy.endpoint.to[0].ipBlock.cidr=192.0.2.2/32')
NP_NOTIFY_TO=(--set 'networkPolicy.notifications.to[0].ipBlock.cidr=198.51.100.0/24')
ENDPOINT_NO_KEY=(--set model.endpoint=https://api.example.com/v1 --set model.name=m)
NOTIFY_DIR=/var/run/secrets/kausa-notify

# notify_args FORMAT [CHAT] [TOKEN] [HTTP]: the args the chart passes for notifications, in
# order: the chat id CHAT when given, the token file when TOKEN is yes, --notify-allow-http when
# HTTP is yes.
notify_args() {
  printf '%s\n' '- --notify-url-file' "- $NOTIFY_DIR/url" '- --notify-format' "- \"$1\""
  if [ -n "${2-}" ]; then printf '%s\n' '- --notify-telegram-chat-id' "- \"$2\""; fi
  if [ "${3-}" = yes ]; then printf '%s\n' '- --notify-token-file' "- $NOTIFY_DIR/token"; fi
  if [ "${4-}" = yes ]; then echo '- --notify-allow-http'; fi
}

# list_item FILE KEY NAME: the item named NAME of the Deployment's list KEY (volumes or
# volumeMounts), one row per line, with spaces and quotes removed.
list_item() {
  docs "$1" Deployment | awk -v key="$2" -v name="$3" '
    !on && $0 ~ "^ *" key ":$" { on = 1; match($0, /^ */); ind = RLENGTH; next }
    on {
      if ($0 ~ /^ *$/) next
      match($0, /^ */)
      if (RLENGTH <= ind) exit
      s = substr($0, RLENGTH + 1)
      if (RLENGTH == ind + 2 && s ~ /^- /) {
        item = s; sub(/^- name: */, "", item); gsub(/["'\'']/, "", item)
        cur = (s ~ /^- name:/ && item == name)
      }
      if (cur) { gsub(/[ "'\'']/, "", s); print s }
    }'
}

# rules_of FILE: the rules of kausa's ClusterRole and of every Role, in order, spaces removed.
rules_of() {
  docs "$1" ClusterRole "" "${2:-kausa}" | section rules
  docs "$1" Role | section rules
}

# with_inserted BASE LINES: BASE's args with the lines of the file LINES inserted before
# --metrics-address, or at the end without it.
with_inserted() {
  args "$1" | awk -v lines="$2" '
    function insert() { while ((getline l < lines) > 0) print l; done = 1 }
    !done && $0 == "- --metrics-address" { insert() }
    { print }
    END { if (!done) insert() }
  '
}

# check_notify NAME OUT BASE URLKEY [TOKENKEY]: OUT renders like BASE (the same values without
# notifications) with the notify args of $OUT.notify-args inserted before --metrics-address, the
# notify volume of the Secret NOTIFY_SECRET with the items URLKEY (and TOKENKEY), its read-only
# mount, the same objects and the same rules.
check_notify() {
  local name=$1 out=$2 base=$3 url_key=$4 token_key=${5-} since=$FAILURES
  if [ ! -f "$out" ] || [ ! -f "$base" ]; then
    fail "$name: no render to check (or no base render to compare with)"
    return 0
  fi
  with_inserted "$base" "$out.notify-args" >"$out.args-expected"
  args "$out" >"$out.args"
  diff -u "$out.args-expected" "$out.args" >"$out.args-diff" ||
    fail "$name: the notify args: $(cat "$out.args-diff")"
  [ "$(docs "$out" Deployment | grep -cE '^ *volumes:$' || true)" = 1 ] ||
    fail "$name: not exactly one volumes list in the Deployment"
  [ "$(docs "$out" Deployment | grep -cE '^ *volumeMounts:$' || true)" = 1 ] ||
    fail "$name: not exactly one volumeMounts list in the Deployment"
  list_item "$out" volumes notify | sed 's/^defaultMode:288$/defaultMode:0440/' >"$out.volume"
  {
    printf '%s\n' '-name:notify' 'secret:' "secretName:$NOTIFY_SECRET" 'defaultMode:0440' 'items:'
    printf '%s\n' "-key:$url_key" 'path:url'
    if [ -n "$token_key" ]; then printf '%s\n' "-key:$token_key" 'path:token'; fi
  } >"$out.volume-expected"
  diff -u <(sort "$out.volume-expected") <(sort "$out.volume") >"$out.volume-diff" ||
    fail "$name: the notify volume: $(cat "$out.volume-diff")"
  awk 'on { print } $0 == "items:" { on = 1 }' "$out.volume" >"$out.items"
  awk 'on { print } $0 == "items:" { on = 1 }' "$out.volume-expected" >"$out.items-expected"
  diff -u "$out.items-expected" "$out.items" >"$out.items-diff" ||
    fail "$name: the notify volume's items, in order: $(cat "$out.items-diff")"
  grep -q '^optional:' "$out.volume" && fail "$name: the notify volume is optional"
  list_item "$out" volumeMounts notify | sort >"$out.mount"
  printf '%s\n' '-name:notify' "mountPath:$NOTIFY_DIR" 'readOnly:true' | sort >"$out.mount-expected"
  diff -u "$out.mount-expected" "$out.mount" >"$out.mount-diff" ||
    fail "$name: the notify mount: $(cat "$out.mount-diff")"
  # The API key's volume and mount stay as they are, in the same lists.
  for key in volumes volumeMounts; do
    list_item "$base" "$key" api-key >"$out.base-$key-api-key"
    list_item "$out" "$key" api-key >"$out.$key-api-key"
    cmp -s "$out.base-$key-api-key" "$out.$key-api-key" ||
      fail "$name: the api-key item of $key changed"
  done
  grep '^kind: ' "$base" | sort >"$out.kinds-expected"
  grep '^kind: ' "$out" | sort >"$out.kinds"
  cmp -s "$out.kinds-expected" "$out.kinds" ||
    fail "$name: other objects than with notifications off: $(diff "$out.kinds-expected" "$out.kinds")"
  rules_of "$base" >"$out.all-rules-expected"
  rules_of "$out" >"$out.all-rules"
  diff -u "$out.all-rules-expected" "$out.all-rules" >"$out.all-rules-diff" ||
    fail "$name: the ClusterRole or Role rules differ from notifications off:" \
      "$(cat "$out.all-rules-diff")"
  passed "$since" "$name: the notify args, volume, mount and unchanged rules"
}

# Bases with notifications off that the checks below compare with (the others are above).
check_cluster endpoint-no-key yes yes "${ENDPOINT_NO_KEY[@]}"
check_cluster endpoint-no-metrics yes yes "${ENDPOINT[@]}" --set metrics.enabled=false

# notify_case NAME BASE ARGS-FORMAT... -- VALUES...: a cluster-scope render of the notify values
# on top of the base's (check_cluster with the rules of BASE), then check_notify. The words
# before -- are notify_args' arguments; URLKEY and TOKENKEY name the items (url and none by
# default), HISTORY, EXTRA, LOGS and WRITES pass through to check_cluster.
notify_case() {
  local name=$1 base=$2
  shift 2
  local spec=()
  while [ "$#" -gt 0 ] && [ "$1" != -- ]; do
    spec+=("$1")
    shift
  done
  shift
  HISTORY=${HISTORY:-no} EXTRA=${EXTRA-} check_cluster "$name" "${LOGS:-yes}" "${WRITES:-yes}" "$@"
  notify_args "${spec[@]}" >"$WORK/cluster-$name.notify-args"
  check_notify "cluster/$name" "$WORK/cluster-$name" "$WORK/cluster-$base" "${URLKEY:-url}" \
    "${TOKENKEY-}"
}

notify_case notify-json endpoint json -- "${ENDPOINT[@]}" "${NOTIFY_ON[@]}"
notify_case notify-json-explicit endpoint json -- "${ENDPOINT[@]}" "${NOTIFY_ON[@]}" \
  --set notifications.format=json
TOKENKEY=token notify_case notify-json-token endpoint json "" yes -- "${ENDPOINT[@]}" \
  "${NOTIFY_ON[@]}" --set notifications.tokenKey=token
TOKENKEY=bearer notify_case notify-json-token-key endpoint json "" yes -- "${ENDPOINT[@]}" \
  "${NOTIFY_ON[@]}" --set notifications.tokenKey=bearer
notify_case notify-json-http endpoint json "" no yes -- "${ENDPOINT[@]}" "${NOTIFY_ON[@]}" \
  --set notifications.allowHttp=true
URLKEY=hook-url notify_case notify-url-key endpoint json -- "${ENDPOINT[@]}" "${NOTIFY_ON[@]}" \
  --set notifications.urlKey=hook-url
notify_case notify-slack endpoint slack -- "${ENDPOINT[@]}" "${NOTIFY_ON[@]}" \
  --set notifications.format=slack
notify_case notify-slack-http endpoint slack "" no yes -- "${ENDPOINT[@]}" "${NOTIFY_ON[@]}" \
  --set notifications.format=slack --set notifications.allowHttp=true
notify_case notify-telegram endpoint telegram "$CHAT_ID" -- "${ENDPOINT[@]}" "${NOTIFY_ON[@]}" \
  --set notifications.format=telegram --set-string "notifications.telegramChatId=$CHAT_ID"
# --set reads the chat id as an integer, a values file as a float64: both render the whole number.
notify_case notify-telegram-int endpoint telegram "$CHAT_ID" -- "${ENDPOINT[@]}" \
  "${NOTIFY_ON[@]}" --set notifications.format=telegram --set "notifications.telegramChatId=$CHAT_ID"
cat >"$WORK/telegram-values.yaml" <<VALUES
notifications:
  enabled: true
  format: telegram
  existingSecret: $NOTIFY_SECRET
  telegramChatId: $CHAT_ID
VALUES
notify_case notify-telegram-float endpoint telegram "$CHAT_ID" -- "${ENDPOINT[@]}" \
  -f "$WORK/telegram-values.yaml"
[ -f "$WORK/cluster-notify-telegram-float" ] && args "$WORK/cluster-notify-telegram-float" |
  grep -q 'e+' && fail "cluster/notify-telegram-float: the chat id renders in scientific notation"
notify_case notify-telegram-channel endpoint telegram @valid_name -- "${ENDPOINT[@]}" \
  "${NOTIFY_ON[@]}" --set notifications.format=telegram \
  --set notifications.telegramChatId=@valid_name
notify_case notify-telegram-http endpoint telegram "$CHAT_ID" no yes -- "${ENDPOINT[@]}" \
  "${NOTIFY_ON[@]}" --set notifications.format=telegram \
  --set-string "notifications.telegramChatId=$CHAT_ID" --set notifications.allowHttp=true
# Without the API key, the notify volume and mount are the only ones.
notify_case notify-no-api-key endpoint-no-key json -- "${ENDPOINT_NO_KEY[@]}" "${NOTIFY_ON[@]}"
if [ -f "$WORK/cluster-notify-no-api-key" ]; then
  [ -z "$(list_item "$WORK/cluster-notify-no-api-key" volumes api-key)" ] ||
    fail "cluster/notify-no-api-key: an api-key volume without model.apiKey.existingSecret"
fi
# Without metrics, the notify args come last.
notify_case notify-no-metrics endpoint-no-metrics json -- "${ENDPOINT[@]}" "${NOTIFY_ON[@]}" \
  --set metrics.enabled=false
# With the history: after the history args.
HISTORY=yes EXTRA="$HISTORY_OBJECTS" notify_case notify-history history json -- \
  "${ENDPOINT[@]}" "${HISTORY_ON[@]}" "${NOTIFY_ON[@]}"
# events=false (and its NetworkPolicy, which then needs the webhook's peers).
WRITES=no notify_case notify-no-events no-events json -- "${NO_EVENTS[@]}" "${NOTIFY_ON[@]}" \
  "${NP_NOTIFY_TO[@]}"

# The history args come before the notify args.
if [ -f "$WORK/cluster-notify-history.args" ]; then
  awk '$0 == "- --history" { h = NR } $0 == "- --notify-url-file" { n = NR }
       END { exit !(h && n && h < n) }' "$WORK/cluster-notify-history.args" ||
    fail "cluster/notify-history: the notify args do not follow the history args"
fi

# Namespaced scope: the same args, volume and mount, and every Role unchanged.
# notify_namespaced NAME ARGS-FORMAT... -- VALUES...: like notify_case, on namespaced/endpoint.
notify_namespaced() {
  local name=$1
  shift
  local spec=()
  while [ "$#" -gt 0 ] && [ "$1" != -- ]; do
    spec+=("$1")
    shift
  done
  shift
  check_namespaced "$name" yes yes e2e-a,e2e-b "$@"
  notify_args "${spec[@]}" >"$WORK/namespaced-$name.notify-args"
  check_notify "namespaced/$name" "$WORK/namespaced-$name" "$WORK/namespaced-endpoint" \
    "${URLKEY:-url}" "${TOKENKEY-}"
}
notify_namespaced notify-json json -- "${ENDPOINT[@]}" "${NOTIFY_ON[@]}"
TOKENKEY=token notify_namespaced notify-json-token json "" yes -- "${ENDPOINT[@]}" \
  "${NOTIFY_ON[@]}" --set notifications.tokenKey=token
notify_namespaced notify-slack slack -- "${ENDPOINT[@]}" "${NOTIFY_ON[@]}" \
  --set notifications.format=slack
notify_namespaced notify-telegram telegram "$CHAT_ID" -- "${ENDPOINT[@]}" "${NOTIFY_ON[@]}" \
  --set notifications.format=telegram --set-string "notifications.telegramChatId=$CHAT_ID"

# With notifications off, nothing of them renders: no --notify arg, no notify volume, no fourth
# egress rule. Checked on every render of the sections above.
since=$FAILURES
for file in "$WORK"/cluster-* "$WORK"/namespaced-*; do
  base_name=$(basename "$file")
  case $base_name in *.* | *notify*) continue ;; esac
  args "$file" | grep -q -- '^- --notify' && fail "$base_name: a --notify arg with notifications off"
  grep -q 'kausa-notify' "$file" && fail "$base_name: the notify volume with notifications off"
  grep -q 'notification webhook' "$file" &&
    fail "$base_name: the webhook's egress rule with notifications off"
done
passed "$since" "notifications off: no --notify arg, notify volume or webhook rule"

# Values of notifications that are set while it is off change nothing, and neither does
# notifications=null.
for pair in \
  "endpoint|--set notifications.enabled=false --set notifications.format=xml --set notifications.telegramChatId=1 --set notifications.tokenKey=t --set notifications.allowHttp=true --set notifications.existingSecret=" \
  "endpoint|--set notifications.format=telegram --set notifications.existingSecret=$NOTIFY_SECRET" \
  "endpoint|--set notifications=null" \
  "default|--set notifications=null" \
  "no-events|--set notifications.enabled=false ${NP_NOTIFY_TO[*]}" \
  "no-events|--set networkPolicy.notifications=null" \
  "dry-run|--set notifications.enabled=false --set notifications.format=xml"; do
  values=${pair%%|*} extra_sets=${pair#*|}
  case $values in
    default) set_values=() ;;
    endpoint) set_values=("${ENDPOINT[@]}") ;;
    no-events) set_values=("${NO_EVENTS[@]}") ;;
    dry-run) set_values=("${DRY[@]}") ;;
  esac
  set -f
  # shellcheck disable=SC2206 # split on spaces on purpose, without globbing the [0]
  extra=($extra_sets)
  set +f
  out="$WORK/off-$values-$(printf '%s' "$extra_sets" | cksum | cut -d' ' -f1)"
  if render "$out" "$RELEASE" "$CHART" --set rbac.scope=cluster ${set_values[@]+"${set_values[@]}"} \
    "${extra[@]}"; then
    if cmp -s "$WORK/cluster-$values" "$out"; then
      ok "notifications off ($extra_sets) renders like $values"
    else
      fail "notifications off ($extra_sets) does not render like $values:" \
        "$(diff "$WORK/cluster-$values" "$out" | head -20)"
    fi
  else
    fail "notifications off ($extra_sets) does not render with $values: $(cat "$out.err")"
  fi
done

# Byte for byte like the chart before notifications, when that chart is in the clone.
if [ -n "$PREVIOUS" ] && git -C "$ROOT" rev-parse --verify --quiet "$PREVIOUS^{commit}" >/dev/null 2>&1; then
  mkdir -p "$WORK/previous"
  git -C "$ROOT" archive "$PREVIOUS" charts/kausa | tar -x -C "$WORK/previous"
  for name in default endpoint dry-run-no-logs dry-run no-events history history-no-events; do
    case $name in
      default) values=() ;;
      endpoint) values=("${ENDPOINT[@]}") ;;
      dry-run-no-logs) values=("${DRY_NO_LOGS[@]}") ;;
      dry-run) values=("${DRY[@]}") ;;
      no-events) values=("${NO_EVENTS[@]}") ;;
      history) values=("${ENDPOINT[@]}" "${HISTORY_ON[@]}") ;;
      history-no-events) values=("${NO_EVENTS[@]}" "${HISTORY_ON[@]}") ;;
    esac
    for scope in cluster namespaced; do
      [ "$scope" = namespaced ] && [ "$name" != endpoint ] && [ "$name" != default ] && continue
      if [ "$scope" = cluster ]; then
        scope_args=(--set rbac.scope=cluster)
      else
        scope_args=(--set rbac.scope=namespaced --set 'rbac.namespaces={e2e-a,e2e-b}')
      fi
      out="$WORK/previous-$scope-$name"
      now="$WORK/now-$scope-$name"
      if render "$out" "$RELEASE" "$WORK/previous/charts/kausa" "${scope_args[@]}" \
        ${values[@]+"${values[@]}"} && render "$now" "$RELEASE" "$CHART" "${scope_args[@]}" \
        ${values[@]+"${values[@]}"} "${ROLLOUTS_OFF[@]}"; then
        # The chart before notifications had no Deployment watch: compare with it off; nor the
        # endpoint check, whose documents and --model-timeout arg are left out.
        { grep -vx -- ' *- --no-rollouts' "$now" || true; } | without_endpoint_check >"$now.v0.3"
        if cmp -s "$out" "$now.v0.3"; then
          ok "$scope/$name renders like $PREVIOUS"
        else
          fail "$scope/$name renders unlike $PREVIOUS: $(diff "$out" "$now.v0.3" | head -20)"
        fi
      else
        fail "$scope/$name does not render here or in $PREVIOUS: $(cat "$out.err" "$now.err")"
      fi
    done
  done
else
  echo "skipped: KAUSA_CHART_PREVIOUS is unset or not in this clone; renders with notifications off are checked above"
fi

# --- The webhook's egress rule ---

# egress FILE: the egress section of kausa's NetworkPolicy, as rendered (the endpoint check's
# policy, docs/adr/0020-check-the-endpoint.md, is another document).
egress() {
  docs "$1" NetworkPolicy "$NAMESPACE" kausa | awk '$0 == "  egress:" { on = 1; next } on && !/^    / { on = 0 } on'
}
# egress_rule FILE N: the Nth egress rule, spaces and quotes removed.
egress_rule() {
  egress "$1" | awk -v n="$2" '
    /^    - / { r++; if (r > n) exit }
    /^    #/ { if (r >= n) exit; next }
    r == n { s = $0; gsub(/[ "'\'']/, "", s); if (s != "") print s }'
}
# egress_comment FILE N: the comment line before the Nth egress rule.
egress_comment() {
  egress "$1" | awk -v n="$2" '
    /^    #/ { c = $0 }
    /^    - / { r++; if (r == n) { sub(/^ +/, "", c); print c; exit }; c = "" }'
}
egress_count() { egress "$1" | grep -c '^    - ' || true; }

# check_egress NAME FILE BASE EXPECTED: FILE's NetworkPolicy keeps BASE's three egress rules and
# adds a fourth, '# The notification webhook.', whose rows (spaces removed) are in EXPECTED.
check_egress() {
  local name=$1 out=$2 base=$3 expected=$4 since=$FAILURES n
  [ -f "$out" ] || return 0
  [ "$(egress_count "$out")" = 4 ] || fail "$name: $(egress_count "$out") egress rules, not 4"
  for n in 1 2 3; do
    cmp -s <(egress_rule "$base" "$n") <(egress_rule "$out" "$n") ||
      fail "$name: egress rule $n changed: $(diff <(egress_rule "$base" "$n") <(egress_rule "$out" "$n"))"
  done
  [ "$(egress_comment "$out" 4)" = '# The notification webhook.' ] ||
    fail "$name: the fourth egress rule is not commented '# The notification webhook.'" \
      "($(egress_comment "$out" 4))"
  egress_rule "$out" 4 >"$out.rule4"
  diff -u "$expected" "$out.rule4" >"$out.rule4-diff" ||
    fail "$name: the webhook's egress rule: $(cat "$out.rule4-diff")"
  passed "$since" "$name: the webhook's egress rule"
}

printf '%s\n' '-to:' '-ipBlock:' 'cidr:198.51.100.0/24' 'ports:' '-port:443' 'protocol:TCP' \
  >"$WORK/rule4-443"
printf '%s\n' '-to:' '-ipBlock:' 'cidr:198.51.100.0/24' 'ports:' '-port:8443' 'protocol:TCP' \
  '-port:443' 'protocol:TCP' >"$WORK/rule4-explicit"
printf '%s\n' '-to:' '-namespaceSelector:' 'matchLabels:' 'kubernetes.io/metadata.name:hooks' \
  'podSelector:' 'matchLabels:' 'app:receiver' 'ports:' '-port:8080' 'protocol:TCP' \
  >"$WORK/rule4-in-cluster"

# np_render NAME ARGS...: a cluster-scope render into $WORK/np-NAME.
np_render() {
  local name=$1
  shift
  render "$WORK/np-$name" "$RELEASE" "$CHART" --set rbac.scope=cluster "$@" ||
    fail "np/$name does not render: $(cat "$WORK/np-$name.err")"
}

np_render base "${ENDPOINT[@]}" "${NP_ON[@]}"
if [ -f "$WORK/np-base" ]; then
  [ "$(egress_count "$WORK/np-base")" = 3 ] || fail "np/base: not three egress rules"
  # The webhook's peers alone add nothing while notifications are off.
  np_render base-with-to "${ENDPOINT[@]}" "${NP_ON[@]}" "${NP_NOTIFY_TO[@]}"
  cmp -s "$WORK/np-base" "$WORK/np-base-with-to" ||
    fail "np/base-with-to: networkPolicy.notifications.to changes a render with notifications off"
fi
np_render default-ports "${ENDPOINT[@]}" "${NP_ON[@]}" "${NOTIFY_ON[@]}" "${NP_NOTIFY_TO[@]}"
check_egress np/default-ports "$WORK/np-default-ports" "$WORK/np-base" "$WORK/rule4-443"
np_render explicit-ports "${ENDPOINT[@]}" "${NP_ON[@]}" "${NOTIFY_ON[@]}" "${NP_NOTIFY_TO[@]}" \
  --set-json 'networkPolicy.notifications.ports=[{"port":8443,"protocol":"TCP"},{"port":443,"protocol":"TCP"}]'
check_egress np/explicit-ports "$WORK/np-explicit-ports" "$WORK/np-base" "$WORK/rule4-explicit"
np_render empty-ports "${ENDPOINT[@]}" "${NP_ON[@]}" "${NOTIFY_ON[@]}" "${NP_NOTIFY_TO[@]}" \
  --set-json 'networkPolicy.notifications.ports=[]'
check_egress np/empty-ports "$WORK/np-empty-ports" "$WORK/np-base" "$WORK/rule4-443"
np_render null-ports "${ENDPOINT[@]}" "${NP_ON[@]}" "${NOTIFY_ON[@]}" "${NP_NOTIFY_TO[@]}" \
  --set networkPolicy.notifications.ports=null
check_egress np/null-ports "$WORK/np-null-ports" "$WORK/np-base" "$WORK/rule4-443"
np_render in-cluster "${ENDPOINT[@]}" "${NP_ON[@]}" "${NOTIFY_ON[@]}" \
  --set notifications.allowHttp=true \
  --set-json 'networkPolicy.notifications.to=[{"namespaceSelector":{"matchLabels":{"kubernetes.io/metadata.name":"hooks"}},"podSelector":{"matchLabels":{"app":"receiver"}}}]' \
  --set-json 'networkPolicy.notifications.ports=[{"port":8080,"protocol":"TCP"}]'
check_egress np/in-cluster "$WORK/np-in-cluster" "$WORK/np-base" "$WORK/rule4-in-cluster"
np_render telegram "${ENDPOINT[@]}" "${NP_ON[@]}" "${NOTIFY_ON[@]}" "${NP_NOTIFY_TO[@]}" \
  --set notifications.format=telegram --set-string "notifications.telegramChatId=$CHAT_ID"
check_egress np/telegram "$WORK/np-telegram" "$WORK/np-base" "$WORK/rule4-443"
# Notifications on without the NetworkPolicy: no NetworkPolicy at all.
if [ -f "$WORK/cluster-notify-json" ]; then
  [ "$(count_docs "$WORK/cluster-notify-json" NetworkPolicy)" = 0 ] ||
    fail "cluster/notify-json: a NetworkPolicy with networkPolicy.enabled=false"
fi
# Namespaced scope: the same rule.
render "$WORK/np-namespaced-base" "$RELEASE" "$CHART" --set rbac.scope=namespaced \
  --set 'rbac.namespaces={e2e-a}' "${ENDPOINT[@]}" "${NP_ON[@]}" ||
  fail "np/namespaced-base does not render: $(cat "$WORK/np-namespaced-base.err")"
render "$WORK/np-namespaced" "$RELEASE" "$CHART" --set rbac.scope=namespaced \
  --set 'rbac.namespaces={e2e-a}' "${ENDPOINT[@]}" "${NP_ON[@]}" "${NOTIFY_ON[@]}" \
  "${NP_NOTIFY_TO[@]}" || fail "np/namespaced does not render: $(cat "$WORK/np-namespaced.err")"
check_egress np/namespaced "$WORK/np-namespaced" "$WORK/np-namespaced-base" "$WORK/rule4-443"

# --- Values without the notifications maps (helm upgrade --reuse-values from an earlier release)

NONOTIFY_CHART="$WORK/nonotify/kausa"
mkdir -p "$WORK/nonotify"
cp -R "$CHART" "$NONOTIFY_CHART"
awk '
  { match($0, /^ */); ind = RLENGTH; blank = ($0 ~ /^ *$/) }
  skip && !blank && ind <= skip_ind { skip = 0 }
  /^[^ #]/ { top = $0 }
  !skip && $0 ~ /^notifications:/ { skip = 1; skip_ind = 0; next }
  !skip && top ~ /^networkPolicy:/ && $0 ~ /^  notifications:/ { skip = 1; skip_ind = 2; next }
  !skip
' "$CHART/values.yaml" >"$NONOTIFY_CHART/values.yaml"
grep -q '^notifications:' "$NONOTIFY_CHART/values.yaml" &&
  fail "the chart copy still has a notifications map"
awk '/^networkPolicy:/ { on = 1; next } on && /^[^ ]/ { on = 0 } on && /^  notifications:/ { f = 1 }
     END { exit !f }' "$NONOTIFY_CHART/values.yaml" &&
  fail "the chart copy still has networkPolicy.notifications"
# Notifications given in full by --set: the same render as the chart's.
NOTIFY_FULL=("${NOTIFY_ON[@]}" --set notifications.format=json --set notifications.urlKey=url
  --set notifications.allowHttp=false)
for pair in default: endpoint:endpoint no-events:no-events history:endpoint,history \
  notify-json:endpoint,notify np-default-ports:endpoint,np,notify,to; do
  name=${pair%%:*} sets=${pair#*:}
  set_values=()
  case ",$sets," in *,endpoint,*) set_values+=("${ENDPOINT[@]}") ;; esac
  case ",$sets," in *,no-events,*) set_values+=("${NO_EVENTS[@]}") ;; esac
  case ",$sets," in *,history,*) set_values+=("${HISTORY_ON[@]}") ;; esac
  case ",$sets," in *,np,*) set_values+=("${NP_ON[@]}") ;; esac
  case ",$sets," in *,notify,*) set_values+=("${NOTIFY_FULL[@]}") ;; esac
  case ",$sets," in *,to,*) set_values+=("${NP_NOTIFY_TO[@]}") ;; esac
  out="$WORK/nonotify-$name"
  case $name in
    np-*) compare="$WORK/$name" ;;
    *) compare="$WORK/cluster-$name" ;;
  esac
  if [ "$name" = notify-json ]; then
    # The chart's render with the same values set explicitly.
    compare="$WORK/notify-full"
    render "$compare" "$RELEASE" "$CHART" --set rbac.scope=cluster "${set_values[@]}" ||
      fail "notify-full does not render: $(cat "$compare.err")"
  fi
  if ! render "$out" "$RELEASE" "$NONOTIFY_CHART" --set rbac.scope=cluster \
    ${set_values[@]+"${set_values[@]}"}; then
    if grep -qi 'nil pointer' "$out.err"; then
      fail "no notifications map, $name: a nil-pointer error: $(cat "$out.err")"
    else
      fail "no notifications map, $name: does not render: $(cat "$out.err")"
    fi
  elif [ ! -f "$compare" ]; then
    fail "no notifications map, $name: no render of the chart to compare with"
  elif cmp -s "$compare" "$out"; then
    ok "no notifications map, $name: renders like the chart"
  else
    fail "no notifications map, $name: renders unlike the chart:" \
      "$(diff "$compare" "$out" | head -20)"
  fi
done

# --- helm lint and NOTES.txt with notifications ---

for format in json slack telegram; do
  format_args=(--set "notifications.format=$format")
  [ "$format" = telegram ] && format_args+=(--set-string "notifications.telegramChatId=$CHAT_ID")
  for scope in cluster namespaced; do
    if [ "$scope" = cluster ]; then
      scope_args=(--set rbac.scope=cluster)
    else
      scope_args=(--set rbac.scope=namespaced --set 'rbac.namespaces={e2e-a}')
    fi
    if "$HELM" lint --strict "$CHART" "${scope_args[@]}" "${ENDPOINT[@]}" "${NOTIFY_ON[@]}" \
      "${format_args[@]}" >"$WORK/lint-notify" 2>&1; then
      ok "helm lint --strict with notifications in $format, $scope"
    else
      fail "helm lint --strict with notifications in $format, $scope: $(cat "$WORK/lint-notify")"
    fi
    file="$WORK/notes-notify-$format-$scope"
    notes "$file" "${scope_args[@]}" "${ENDPOINT[@]}" "${NOTIFY_ON[@]}" "${format_args[@]}" \
      --set notifications.urlKey=hook-url
    [ -s "$file" ] || { fail "NOTES.txt with notifications ($format, $scope) is empty"; continue; }
    since=$FAILURES
    for words in "$format" "$NOTIFY_SECRET" hook-url 'kausa_notifications_total{outcome="failed"}' \
      'notification failed' "kubectl logs -n $NAMESPACE deploy/kausa"; do
      collapsed "$file" | grep -qF -- "$words" ||
        fail "NOTES.txt with notifications ($format, $scope) does not name '$words'"
    done
    passed "$since" "NOTES.txt with notifications ($format, $scope)"
  done
done
for file in "$WORK/notes-cluster" "$WORK/notes-namespaced" "$WORK/notes-history-cluster"; do
  [ -s "$file" ] || continue
  grep -qE 'kausa_notifications_total|notification failed' "$file" &&
    fail "NOTES.txt without notifications tells how to find their failures ($file)"
done

# --- Refusals of notifications, after every earlier check ---

NOTIFY_NO_SECRET='notifications.existingSecret is required with notifications.enabled: the Secret that holds the webhook URL'
notify_bad_format() { echo "notifications.format must be json, slack or telegram, not '$1'"; }
NOTIFY_NEEDS_CHAT='notifications.telegramChatId is required with notifications.format=telegram'
NOTIFY_CHAT_ONLY='notifications.telegramChatId is used only with notifications.format=telegram'
NOTIFY_TOKEN_ONLY='notifications.tokenKey is used only with notifications.format=json'
NOTIFY_NEEDS_ENDPOINT='notifications.enabled needs model.endpoint'
NOTIFY_NOT_DRY='notifications.enabled is not used with dryRun: no explanation is written'
NOTIFY_NEEDS_TO='networkPolicy.notifications.to is required with networkPolicy.enabled and notifications.enabled: without it kausa cannot reach the webhook'
CL=(--set rbac.scope=cluster)
NS=(--set rbac.scope=namespaced --set 'rbac.namespaces={e2e-a}')
TELEGRAM=(--set notifications.format=telegram)

refuse notify-no-secret "$NOTIFY_NO_SECRET" "${CL[@]}" "${ENDPOINT[@]}" \
  --set notifications.enabled=true
refuse notify-empty-secret "$NOTIFY_NO_SECRET" "${CL[@]}" "${ENDPOINT[@]}" \
  --set notifications.enabled=true --set notifications.existingSecret=
refuse notify-no-secret-namespaced "$NOTIFY_NO_SECRET" "${NS[@]}" "${ENDPOINT[@]}" \
  --set notifications.enabled=true
for format in xml Slack JSON telegram2; do
  refuse "notify-format-$format" "$(notify_bad_format "$format")" "${CL[@]}" "${ENDPOINT[@]}" \
    "${NOTIFY_ON[@]}" --set "notifications.format=$format"
done
refuse notify-telegram-no-chat "$NOTIFY_NEEDS_CHAT" "${CL[@]}" "${ENDPOINT[@]}" \
  "${NOTIFY_ON[@]}" "${TELEGRAM[@]}"
refuse notify-telegram-empty-chat "$NOTIFY_NEEDS_CHAT" "${CL[@]}" "${ENDPOINT[@]}" \
  "${NOTIFY_ON[@]}" "${TELEGRAM[@]}" --set notifications.telegramChatId=
refuse notify-chat-json "$NOTIFY_CHAT_ONLY" "${CL[@]}" "${ENDPOINT[@]}" "${NOTIFY_ON[@]}" \
  --set-string "notifications.telegramChatId=$CHAT_ID"
refuse notify-chat-slack "$NOTIFY_CHAT_ONLY" "${CL[@]}" "${ENDPOINT[@]}" "${NOTIFY_ON[@]}" \
  --set notifications.format=slack --set notifications.telegramChatId=@valid_name
refuse notify-token-slack "$NOTIFY_TOKEN_ONLY" "${CL[@]}" "${ENDPOINT[@]}" "${NOTIFY_ON[@]}" \
  --set notifications.format=slack --set notifications.tokenKey=token
refuse notify-token-telegram "$NOTIFY_TOKEN_ONLY" "${CL[@]}" "${ENDPOINT[@]}" "${NOTIFY_ON[@]}" \
  "${TELEGRAM[@]}" --set-string "notifications.telegramChatId=$CHAT_ID" \
  --set notifications.tokenKey=token
refuse notify-no-endpoint "$NOTIFY_NEEDS_ENDPOINT" "${CL[@]}" "${NOTIFY_ON[@]}"
refuse notify-no-endpoint-namespaced "$NOTIFY_NEEDS_ENDPOINT" "${NS[@]}" "${NOTIFY_ON[@]}"
refuse notify-dry-run "$NOTIFY_NOT_DRY" "${CL[@]}" "${ENDPOINT[@]}" "${NOTIFY_ON[@]}" "${DRY[@]}"
refuse notify-np-no-to "$NOTIFY_NEEDS_TO" "${CL[@]}" "${ENDPOINT[@]}" "${NOTIFY_ON[@]}" \
  "${NP_ON[@]}"
refuse notify-np-empty-to "$NOTIFY_NEEDS_TO" "${CL[@]}" "${ENDPOINT[@]}" "${NOTIFY_ON[@]}" \
  "${NP_ON[@]}" --set-json 'networkPolicy.notifications.to=[]'
refuse notify-np-null "$NOTIFY_NEEDS_TO" "${CL[@]}" "${ENDPOINT[@]}" "${NOTIFY_ON[@]}" \
  "${NP_ON[@]}" --set networkPolicy.notifications=null
refuse notify-np-namespaced "$NOTIFY_NEEDS_TO" "${NS[@]}" "${ENDPOINT[@]}" "${NOTIFY_ON[@]}" \
  "${NP_ON[@]}"
# The order: the earlier checks first, then the eight in the order of the contract.
refuse notify-order-rbac-first "$NOT_SET" "${ENDPOINT[@]}" --set notifications.enabled=true
refuse notify-order-v0.1-first "model.name is required with model.endpoint" "${CL[@]}" \
  --set model.endpoint=https://api.example.com/v1 --set notifications.enabled=true
refuse notify-order-endpoint-to-first \
  "networkPolicy.endpoint.to is required with networkPolicy.enabled and model.endpoint: without it kausa cannot reach the model" \
  "${CL[@]}" "${ENDPOINT[@]}" "${NOTIFY_ON[@]}" --set networkPolicy.enabled=true \
  --set 'networkPolicy.apiServer.cidrs[0]=192.0.2.1/32'
refuse notify-order-days-first "$(not_days 0)" "${CL[@]}" "${ENDPOINT[@]}" \
  --set notifications.enabled=true --set history.days=0
refuse notify-order-secret-before-format "$NOTIFY_NO_SECRET" "${CL[@]}" "${ENDPOINT[@]}" \
  --set notifications.enabled=true --set notifications.format=xml
refuse notify-order-format-before-chat "$(notify_bad_format xml)" "${CL[@]}" "${ENDPOINT[@]}" \
  "${NOTIFY_ON[@]}" --set notifications.format=xml --set notifications.telegramChatId=1
refuse notify-order-format-before-token "$(notify_bad_format xml)" "${CL[@]}" "${ENDPOINT[@]}" \
  "${NOTIFY_ON[@]}" --set notifications.format=xml --set notifications.tokenKey=token
refuse notify-order-chat-required-before-token "$NOTIFY_NEEDS_CHAT" "${CL[@]}" \
  "${ENDPOINT[@]}" "${NOTIFY_ON[@]}" "${TELEGRAM[@]}" --set notifications.tokenKey=token
refuse notify-order-chat-before-token "$NOTIFY_CHAT_ONLY" "${CL[@]}" "${ENDPOINT[@]}" \
  "${NOTIFY_ON[@]}" --set notifications.format=slack --set notifications.telegramChatId=1 \
  --set notifications.tokenKey=token
refuse notify-order-token-before-endpoint "$NOTIFY_TOKEN_ONLY" "${CL[@]}" "${NOTIFY_ON[@]}" \
  --set notifications.format=slack --set notifications.tokenKey=token
refuse notify-order-endpoint-before-dry-run "$NOTIFY_NEEDS_ENDPOINT" "${CL[@]}" \
  "${NOTIFY_ON[@]}" "${DRY[@]}"
refuse notify-order-dry-run-before-np "$NOTIFY_NOT_DRY" "${CL[@]}" "${ENDPOINT[@]}" \
  "${NOTIFY_ON[@]}" "${DRY[@]}" "${NP_ON[@]}"
# Values without the notifications maps reach the same refusals, never a nil-pointer error.
# refuse_in CHART NAME MESSAGE ARGS...: refuse, with another chart.
refuse_in() {
  local saved=$CHART
  CHART=$1
  shift
  refuse "$@"
  CHART=$saved
}
refuse_in "$NONOTIFY_CHART" notify-nomap-no-secret "$NOTIFY_NO_SECRET" "${CL[@]}" \
  "${ENDPOINT[@]}" --set notifications.enabled=true
refuse_in "$NONOTIFY_CHART" notify-nomap-np "$NOTIFY_NEEDS_TO" "${CL[@]}" "${ENDPOINT[@]}" \
  "${NOTIFY_FULL[@]}" "${NP_ON[@]}"

# --- Standby replica (docs/adr/0019-high-availability.md) ---------------------------------------

# replicaCount is 1 (the default) or 2. With 1, null or no key (a release's stored v0.3 values
# under helm upgrade --reuse-values), the chart renders byte for byte as before. With 2, and in
# both scopes alike: replicas 2 with Recreate, the --leader-lease pair right before
# --settle-seconds, a preferred pod anti-affinity while affinity is empty, and, outside kausa.rules,
# a Lease, a Role and a RoleBinding <fullname>-leader in the release namespace (get and patch on
# that one Lease) and a PodDisruptionBudget <fullname>. Everything else renders unchanged. Any
# other value is refused.
# Written against Helm 3.16.1. CI pins Helm 4.3.1 (HELM_VERSION in the Dockerfile and in
# .github/workflows/e2e.yml): how hasKey sees --set replicaCount=null and toString a float64 such
# as 2.0 there is checked by the ci target's renders and the e2e test, or here with HELM pointing
# at that binary. The line below says which Helm ran these checks.
HELM_SHORT=$("$HELM" version --short 2>/dev/null) || HELM_SHORT='(unknown version)'
echo "note: the replicaCount checks run with Helm $HELM_SHORT"

HA=(--set replicaCount=2)
HA_CL=(--set rbac.scope=cluster)
HA_NS=(--set rbac.scope=namespaced --set 'rbac.namespaces={e2e-a,e2e-b}')
HA_NS_OWN=(--set rbac.scope=namespaced --set "rbac.namespaces={e2e-a,$NAMESPACE}")
HA_NP=(--set networkPolicy.enabled=true --set 'networkPolicy.apiServer.cidrs[0]=192.0.2.1/32'
  --set 'networkPolicy.endpoint.to[0].ipBlock.cidr=192.0.2.2/32')
NODE_AFFINITY='affinity={"nodeAffinity":{"requiredDuringSchedulingIgnoredDuringExecution":'
NODE_AFFINITY+='{"nodeSelectorTerms":[{"matchExpressions":[{"key":"kubernetes.io/arch",'
NODE_AFFINITY+='"operator":"In","values":["arm64"]}]}]}}}'
REQUIRED_ANTI_AFFINITY='affinity={"podAntiAffinity":'
REQUIRED_ANTI_AFFINITY+='{"requiredDuringSchedulingIgnoredDuringExecution":'
REQUIRED_ANTI_AFFINITY+='[{"topologyKey":"kubernetes.io/hostname","labelSelector":{"matchLabels":'
REQUIRED_ANTI_AFFINITY+='{"app.kubernetes.io/name":"kausa"}}}]}}'

grep -qx 'replicaCount: 1' "$CHART/values.yaml" ||
  fail "values.yaml has no top-level 'replicaCount: 1'"
# A copy of the chart without the key: a release made before it.
NOREPLICAS_CHART="$WORK/noreplicas/kausa"
mkdir -p "$WORK/noreplicas"
cp -R "$CHART" "$NOREPLICAS_CHART"
grep -v '^replicaCount:' "$CHART/values.yaml" >"$NOREPLICAS_CHART/values.yaml"

# The rule of the Role <fullname>-leader, without spaces.
rule_leader() {
  printf '%s\n' '-apiGroups:["coordination.k8s.io"]' 'resources:["leases"]' \
    "resourceNames:[\"$1\"]" 'verbs:["get","patch"]'
}

# affinity_of FILE: the Deployment's pod affinity block, spaces removed.
affinity_of() {
  docs "$1" Deployment | awk '
    $0 == "      affinity:" { on = 1; next }
    on && !/^       / { on = 0 }
    on' | tr -d ' '
}

# kinds FILE [EXTRA...]: "Kind count" per kind of FILE's documents, plus the EXTRA kinds.
kinds() {
  local file=$1
  shift
  { grep -h '^kind: ' "$file" | cut -c7-; if [ "$#" -gt 0 ]; then printf '%s\n' "$@"; fi; } |
    sort | uniq -c | awk '{ print $2, $1 }'
}

# without_standby FILE LEASE_ARG STRIP_AFFINITY: the Deployment of a replicaCount=2 render with
# replicas back to 1, the --leader-lease pair removed and, with STRIP_AFFINITY=yes, the affinity
# block removed: what is left must be the replicaCount=1 Deployment.
without_standby() {
  docs "$1" Deployment | awk -v strip="$3" '
    $0 == "  replicas: 2" { print "  replicas: 1"; next }
    skip { skip = 0; next }
    /^ *- --leader-lease$/ { skip = 1; next }
    strip == "yes" && $0 == "      affinity:" { aff = 1; next }
    aff && /^       / { next }
    { aff = 0; print }'
}

# check_single NAME FILE: a render with one replica holds nothing of the standby.
check_single() {
  local name=$1 out=$2
  [ "$(count_docs "$out" Lease)" = 0 ] || fail "standby/$name: renders a Lease"
  [ "$(count_docs "$out" PodDisruptionBudget)" = 0 ] ||
    fail "standby/$name: renders a PodDisruptionBudget"
  grep -q -- '-leader$' "$out" && fail "standby/$name: renders an object named *-leader"
  args "$out" | grep -q -- 'leader-lease' && fail "standby/$name: passes --leader-lease"
  docs "$out" Deployment | grep -qx '  replicas: 1' || fail "standby/$name: replicas is not 1"
  docs "$out" Deployment | grep -qx '    type: Recreate' || fail "standby/$name: not Recreate"
  grep -q 'podAntiAffinity' "$out" && fail "standby/$name: renders a pod anti-affinity"
  return 0
}

# check_ha NAME FILE BASE: FILE renders replicaCount=2 with the values that rendered BASE with one
# replica. FULLNAME and SUBJECT (the objects' and the service account's names) default to kausa;
# AFFINITY=user expects the given affinity instead of the default one.
check_ha() {
  local name=$1 out=$2 base=$3 fullname=${FULLNAME:-kausa} subject=${SUBJECT:-kausa}
  local lease="${FULLNAME:-kausa}-leader" since=$FAILURES strip=yes
  [ "${AFFINITY:-default}" = user ] && strip=no
  docs "$out" Deployment | grep -qx '  replicas: 2' || fail "standby/$name: replicas is not 2"
  docs "$out" Deployment | grep -qx '    type: Recreate' ||
    fail "standby/$name: the strategy is not Recreate"
  # The args: exactly the pair, right before --settle-seconds.
  [ "$(args "$out" | grep -cx -- '- --leader-lease')" = 1 ] ||
    fail "standby/$name: not exactly one --leader-lease arg"
  args "$out" | grep -A2 -x -- '- --leader-lease' >"$out.pair" || true
  printf '%s\n' '- --leader-lease' "- \"$NAMESPACE/$lease\"" '- --settle-seconds' \
    >"$out.pair-expected"
  diff -u "$out.pair-expected" "$out.pair" >"$out.pair-diff" ||
    fail "standby/$name: the --leader-lease pair: $(cat "$out.pair-diff")"
  # The Deployment is the one-replica Deployment but for replicas, the pair and the affinity.
  docs "$base" Deployment >"$out.deployment-expected"
  without_standby "$out" "$NAMESPACE/$lease" "$strip" >"$out.deployment"
  diff -u "$out.deployment-expected" "$out.deployment" >"$out.deployment-diff" ||
    fail "standby/$name: the Deployment changes more than replicas, the arg pair and the" \
      "affinity: $(head -40 "$out.deployment-diff")"
  if [ "$strip" = yes ]; then
    affinity_of "$out" >"$out.affinity"
    for line in 'podAntiAffinity:' 'preferredDuringSchedulingIgnoredDuringExecution:' \
      '-weight:100' 'podAffinityTerm:' 'topologyKey:kubernetes.io/hostname' 'labelSelector:' \
      'matchLabels:' 'app.kubernetes.io/name:kausa' \
      "app.kubernetes.io/instance:${RELEASE_NAME:-$RELEASE}"; do
      grep -qx -- "$line" "$out.affinity" || fail "standby/$name: the anti-affinity lacks $line"
    done
    [ "$(wc -l <"$out.affinity" | tr -d ' ')" = 9 ] ||
      fail "standby/$name: the default affinity is not just the preferred anti-affinity:" \
        "$(cat "$out.affinity")"
  fi
  # The Lease, with no spec: Helm owns the object, kausa its spec.
  [ "$(count_docs "$out" Lease)" = 1 ] || fail "standby/$name: not exactly one Lease"
  if [ "$(count_docs "$out" Lease "$NAMESPACE" "$lease")" = 1 ]; then
    docs "$out" Lease "$NAMESPACE" "$lease" >"$out.lease"
    for line in 'apiVersion: coordination.k8s.io/v1' 'spec: {}' \
      "    app.kubernetes.io/instance: ${RELEASE_NAME:-$RELEASE}" \
      '    app.kubernetes.io/name: kausa'; do
      grep -qx -- "$line" "$out.lease" || fail "standby/$name: the Lease lacks '$line'"
    done
  else
    fail "standby/$name: no Lease $lease in $NAMESPACE"
  fi
  # The Role: one rule, get and patch on that one Lease.
  [ "$(count_docs "$out" Role "" "$lease")" = 1 ] ||
    fail "standby/$name: not exactly one Role named $lease"
  if [ "$(count_docs "$out" Role "$NAMESPACE" "$lease")" = 1 ]; then
    docs "$out" Role "$NAMESPACE" "$lease" | section rules >"$out.leader-rules"
    rule_leader "$lease" >"$out.leader-rules-expected"
    diff -u "$out.leader-rules-expected" "$out.leader-rules" >"$out.leader-rules-diff" ||
      fail "standby/$name: the rules of the Role $lease: $(cat "$out.leader-rules-diff")"
  else
    fail "standby/$name: no Role $lease in $NAMESPACE"
  fi
  [ "$(count_docs "$out" RoleBinding "" "$lease")" = 1 ] ||
    fail "standby/$name: not exactly one RoleBinding named $lease"
  if [ "$(count_docs "$out" RoleBinding "$NAMESPACE" "$lease")" = 1 ]; then
    docs "$out" RoleBinding "$NAMESPACE" "$lease" | section roleRef >"$out.leader-ref"
    printf '%s\n' 'apiGroup:rbac.authorization.k8s.io' 'kind:Role' "name:$lease" |
      sort >"$out.leader-ref-expected"
    sort "$out.leader-ref" | diff -u "$out.leader-ref-expected" - >"$out.leader-ref-diff" ||
      fail "standby/$name: the roleRef of $lease: $(cat "$out.leader-ref-diff")"
    docs "$out" RoleBinding "$NAMESPACE" "$lease" | section subjects >"$out.leader-subjects"
    [ "$(grep -c '^-' "$out.leader-subjects")" = 1 ] ||
      fail "standby/$name: the RoleBinding $lease has not exactly one subject"
    for line in '-kind:ServiceAccount' "name:$subject" "namespace:$NAMESPACE"; do
      grep -qx -- "$line" "$out.leader-subjects" ||
        fail "standby/$name: the subject of $lease lacks $line"
    done
  else
    fail "standby/$name: no RoleBinding $lease in $NAMESPACE"
  fi
  # Nothing else names the group of Leases: no other grant on them.
  [ "$(grep -v '^ *#' "$out" | grep -c 'coordination.k8s.io')" = 2 ] ||
    fail "standby/$name: coordination.k8s.io appears other than in the Lease and its rule"
  # The PodDisruptionBudget.
  if [ "$(count_docs "$out" PodDisruptionBudget "$NAMESPACE" "$fullname")" = 1 ] &&
    [ "$(count_docs "$out" PodDisruptionBudget)" = 1 ]; then
    docs "$out" PodDisruptionBudget >"$out.pdb"
    grep -qx 'apiVersion: policy/v1' "$out.pdb" ||
      fail "standby/$name: the PodDisruptionBudget is not policy/v1"
    section spec <"$out.pdb" >"$out.pdb-spec"
    printf '%s\n' 'maxUnavailable:1' 'unhealthyPodEvictionPolicy:AlwaysAllow' 'selector:' \
      'matchLabels:' 'app.kubernetes.io/name:kausa' \
      "app.kubernetes.io/instance:${RELEASE_NAME:-$RELEASE}" | sort >"$out.pdb-expected"
    sort "$out.pdb-spec" | diff -u "$out.pdb-expected" - >"$out.pdb-diff" ||
      fail "standby/$name: the PodDisruptionBudget's spec: $(cat "$out.pdb-diff")"
  else
    fail "standby/$name: not exactly one PodDisruptionBudget, $fullname in $NAMESPACE"
  fi
  # Nothing else is added or changed: the same kinds plus the four objects, nothing new that is
  # cluster-scoped, and the same ClusterRole, Roles (kausa.rules), bindings, ServiceAccount,
  # Service and NetworkPolicy.
  kinds "$base" Lease Role RoleBinding PodDisruptionBudget >"$out.kinds-expected"
  kinds "$out" >"$out.kinds"
  diff -u "$out.kinds-expected" "$out.kinds" >"$out.kinds-diff" ||
    fail "standby/$name: the kinds rendered: $(cat "$out.kinds-diff")"
  cluster_scoped "$base" >"$out.scoped-expected"
  cluster_scoped "$out" >"$out.scoped"
  diff -u "$out.scoped-expected" "$out.scoped" >"$out.scoped-diff" ||
    fail "standby/$name: the cluster-scoped objects: $(cat "$out.scoped-diff")"
  for kind in ServiceAccount Service NetworkPolicy ClusterRole ClusterRoleBinding; do
    cmp -s <(docs "$base" "$kind") <(docs "$out" "$kind") ||
      fail "standby/$name: the ${kind}s differ from the one-replica render"
  done
  for kind in Role RoleBinding; do
    cmp -s <(docs "$base" "$kind" "" "$fullname") <(docs "$out" "$kind" "" "$fullname") ||
      fail "standby/$name: the ${kind}s $fullname (kausa.rules) differ from the one-replica render"
  done
  passed "$since" "standby/$name"
}

# check_same NAME FILE COMPARE: FILE renders byte for byte like COMPARE.
check_same() {
  if [ -f "$2" ] && [ -f "$3" ] && cmp -s "$3" "$2"; then
    ok "standby/$1"
  else
    fail "standby/$1: does not render byte for byte the same: $(diff "$3" "$2" 2>&1 | head -20)"
  fi
}

VALUES_TWO="$WORK/values-replicas-2.yaml"
VALUES_ONE="$WORK/values-replicas-1.yaml"
echo 'replicaCount: 2' >"$VALUES_TWO"
echo 'replicaCount: 1' >"$VALUES_ONE"
for set in cluster cluster-endpoint namespaced namespaced-endpoint namespaced-own; do
  case $set in
    cluster*) scope=("${HA_CL[@]}") ;;
    namespaced-own) scope=("${HA_NS_OWN[@]}") ;;
    namespaced*) scope=("${HA_NS[@]}") ;;
  esac
  case $set in
    *-endpoint) values=("${ENDPOINT[@]}" "${HA_NP[@]}") ;;
    *) values=() ;;
  esac
  out="$WORK/standby-$set"
  if ! render "$out-one" "$RELEASE" "$CHART" "${scope[@]}" ${values[@]+"${values[@]}"}; then
    fail "standby/$set does not render with one replica: $(cat "$out-one.err")"
    continue
  fi
  check_single "$set" "$out-one"
  # One replica, however it is given, renders byte for byte the same.
  render "$out-null" "$RELEASE" "$CHART" "${scope[@]}" ${values[@]+"${values[@]}"} \
    --set replicaCount=null || fail "standby/$set-null does not render: $(cat "$out-null.err")"
  check_same "$set-null" "$out-null" "$out-one"
  render "$out-1" "$RELEASE" "$CHART" "${scope[@]}" ${values[@]+"${values[@]}"} \
    --set replicaCount=1 || fail "standby/$set-1 does not render: $(cat "$out-1.err")"
  check_same "$set-1" "$out-1" "$out-one"
  render "$out-file-1" "$RELEASE" "$CHART" "${scope[@]}" ${values[@]+"${values[@]}"} \
    -f "$VALUES_ONE" || fail "standby/$set-file-1 does not render: $(cat "$out-file-1.err")"
  check_same "$set-file-1" "$out-file-1" "$out-one"
  render "$out-nokey" "$RELEASE" "$NOREPLICAS_CHART" "${scope[@]}" ${values[@]+"${values[@]}"} ||
    fail "standby/$set-nokey does not render: $(cat "$out-nokey.err")"
  check_same "$set-nokey" "$out-nokey" "$out-one"
  # Two replicas.
  if render "$out-two" "$RELEASE" "$CHART" "${scope[@]}" ${values[@]+"${values[@]}"} \
    "${HA[@]}"; then
    check_ha "$set" "$out-two" "$out-one"
  else
    fail "standby/$set does not render with two replicas: $(cat "$out-two.err")"
    continue
  fi
  # 2 from a values file (a float64), as a string and as 2.0 renders the same.
  render "$out-file-2" "$RELEASE" "$CHART" "${scope[@]}" ${values[@]+"${values[@]}"} \
    -f "$VALUES_TWO" || fail "standby/$set-file-2 does not render: $(cat "$out-file-2.err")"
  check_same "$set-file-2" "$out-file-2" "$out-two"
  render "$out-string-2" "$RELEASE" "$CHART" "${scope[@]}" ${values[@]+"${values[@]}"} \
    --set-string replicaCount=2 ||
    fail "standby/$set-string-2 does not render: $(cat "$out-string-2.err")"
  check_same "$set-string-2" "$out-string-2" "$out-two"
  render "$out-float-2" "$RELEASE" "$CHART" "${scope[@]}" ${values[@]+"${values[@]}"} \
    --set-json replicaCount=2.0 ||
    fail "standby/$set-float-2 does not render: $(cat "$out-float-2.err")"
  check_same "$set-float-2" "$out-float-2" "$out-two"
done
# The names follow the release and the service account.
for set in cluster namespaced; do
  case $set in
    cluster) scope=("${HA_CL[@]}") ;;
    *) scope=("${HA_NS[@]}") ;;
  esac
  out="$WORK/standby-$set-tenant"
  if render "$out-one" tenant "$CHART" "${scope[@]}" &&
    render "$out-two" tenant "$CHART" "${scope[@]}" "${HA[@]}"; then
    RELEASE_NAME=tenant FULLNAME=tenant-kausa SUBJECT=tenant-kausa \
      check_ha "$set-tenant" "$out-two" "$out-one"
  else
    fail "standby/$set-tenant does not render: $(cat "$out-one.err" "$out-two.err")"
  fi
  out="$WORK/standby-$set-reader"
  if render "$out-one" "$RELEASE" "$CHART" "${scope[@]}" --set serviceAccount.name=reader &&
    render "$out-two" "$RELEASE" "$CHART" "${scope[@]}" --set serviceAccount.name=reader \
      "${HA[@]}"; then
    SUBJECT=reader check_ha "$set-reader" "$out-two" "$out-one"
  else
    fail "standby/$set-reader does not render: $(cat "$out-one.err" "$out-two.err")"
  fi
  # A user affinity replaces the default one, a required anti-affinity included.
  for affinity in node required; do
    case $affinity in
      node) given=$NODE_AFFINITY ;;
      *) given=$REQUIRED_ANTI_AFFINITY ;;
    esac
    out="$WORK/standby-$set-affinity-$affinity"
    if render "$out-one" "$RELEASE" "$CHART" "${scope[@]}" --set-json "$given" &&
      render "$out-two" "$RELEASE" "$CHART" "${scope[@]}" --set-json "$given" "${HA[@]}"; then
      AFFINITY=user check_ha "$set-affinity-$affinity" "$out-two" "$out-one"
      grep -q 'preferredDuringSchedulingIgnoredDuringExecution' "$out-two" &&
        fail "standby/$set-affinity-$affinity: the default anti-affinity is added to the user's"
      [ -s "$out-one" ] && [ -n "$(affinity_of "$out-one")" ] ||
        fail "standby/$set-affinity-$affinity: the one-replica render lost the user's affinity"
    else
      fail "standby/$set-affinity-$affinity does not render: $(cat "$out-one.err" "$out-two.err")"
    fi
  done
done

# The names follow the release namespace: nothing of the standby names kausa-system when the
# release is installed elsewhere.
saved_namespace=$NAMESPACE
NAMESPACE=shop-ops
for set in cluster namespaced; do
  case $set in
    cluster) scope=("${HA_CL[@]}") ;;
    *) scope=("${HA_NS[@]}") ;;
  esac
  out="$WORK/standby-$set-$NAMESPACE"
  if render "$out-one" "$RELEASE" "$CHART" "${scope[@]}" &&
    render "$out-two" "$RELEASE" "$CHART" "${scope[@]}" "${HA[@]}"; then
    check_ha "$set-in-$NAMESPACE" "$out-two" "$out-one"
    grep -q "$saved_namespace" "$out-two" &&
      fail "standby/$set-in-$NAMESPACE: names $saved_namespace, not the release namespace"
  else
    fail "standby/$set-in-$NAMESPACE does not render: $(cat "$out-one.err" "$out-two.err")"
  fi
done
NAMESPACE=$saved_namespace

# The Lease, its Role and RoleBinding and the PodDisruptionBudget are outside kausa.rules: byte for
# byte the same in both scopes, whatever rbac.namespaces lists and with the endpoint's
# NetworkPolicy on.
standby_objects() {
  docs "$1" Lease
  docs "$1" Role "" "$2-leader"
  docs "$1" RoleBinding "" "$2-leader"
  docs "$1" PodDisruptionBudget
}
for prefix in standby standby-tenant; do
  case $prefix in
    standby)
      suffix='' fullname=kausa
      sets="cluster-endpoint namespaced namespaced-endpoint namespaced-own"
      ;;
    *) suffix=-tenant fullname=tenant-kausa sets=namespaced ;;
  esac
  standby_objects "$WORK/standby-cluster$suffix-two" "$fullname" >"$WORK/$prefix-objects-cluster"
  if [ "$(grep -c '^%%$' "$WORK/$prefix-objects-cluster")" != 4 ]; then
    fail "$prefix/objects: the cluster render has not the four standby objects"
    continue
  fi
  for set in $sets; do
    standby_objects "$WORK/standby-$set$suffix-two" "$fullname" >"$WORK/$prefix-objects-$set"
    if cmp -s "$WORK/$prefix-objects-cluster" "$WORK/$prefix-objects-$set"; then
      ok "$prefix/$set renders the standby objects of cluster scope"
    else
      fail "$prefix/$set: the standby objects differ from cluster scope:" \
        "$(diff "$WORK/$prefix-objects-cluster" "$WORK/$prefix-objects-$set" | head -20)"
    fi
  done
done

# A whole number from a JSON or YAML float is that number (toString of a float64): 1.0 renders
# like the default, and 2.0 like 2 (checked above).
for set in cluster namespaced; do
  case $set in
    cluster) scope=("${HA_CL[@]}") ;;
    *) scope=("${HA_NS[@]}") ;;
  esac
  out="$WORK/standby-$set-float-1"
  render "$out" "$RELEASE" "$CHART" "${scope[@]}" --set-json replicaCount=1.0 ||
    fail "standby/$set-float-1 does not render: $(cat "$out.err")"
  check_same "$set-float-1" "$out" "$WORK/standby-$set-one"
done

# The default renders are those of the chart before this value, byte for byte, when
# KAUSA_CHART_PRE_HA names that chart's git ref (unset by default: after a release the chart's
# version labels differ).
PRE_HA=${KAUSA_CHART_PRE_HA:-}
if [ -n "$PRE_HA" ] &&
  git -C "$ROOT" rev-parse --verify --quiet "$PRE_HA^{commit}" >/dev/null 2>&1; then
  mkdir -p "$WORK/pre-ha"
  git -C "$ROOT" archive "$PRE_HA" charts/kausa | tar -x -C "$WORK/pre-ha"
  for set in cluster cluster-endpoint namespaced namespaced-endpoint namespaced-own; do
    case $set in
      cluster*) scope=("${HA_CL[@]}") ;;
      namespaced-own) scope=("${HA_NS_OWN[@]}") ;;
      namespaced*) scope=("${HA_NS[@]}") ;;
    esac
    case $set in
      *-endpoint) values=("${ENDPOINT[@]}" "${HA_NP[@]}") ;;
      *) values=() ;;
    esac
    out="$WORK/pre-ha-$set"
    if render "$out" "$RELEASE" "$WORK/pre-ha/charts/kausa" "${scope[@]}" \
      ${values[@]+"${values[@]}"}; then
      # Without the endpoint check, which that chart did not have.
      without_endpoint_check <"$WORK/standby-$set-one" >"$out.now"
      check_same "$set-is-$PRE_HA" "$out.now" "$out"
    else
      fail "the $PRE_HA chart does not render $set: $(cat "$out.err")"
    fi
  done
else
  echo "skipped: KAUSA_CHART_PRE_HA is unset or not in this clone; the default renders are" \
    "checked against replicaCount=1, null and a chart without the key above"
fi

# NOTES.txt names the Lease, how to see its holder, the budget per replica and how to scale, only
# with two replicas; with one it is unchanged.
for set in cluster namespaced; do
  case $set in
    cluster) scope=("${HA_CL[@]}") ;;
    *) scope=("${HA_NS[@]}") ;;
  esac
  notes "$WORK/notes-standby-$set-one" "${scope[@]}" "${ENDPOINT[@]}"
  notes "$WORK/notes-standby-$set-1" "${scope[@]}" "${ENDPOINT[@]}" --set replicaCount=1
  notes "$WORK/notes-standby-$set-null" "${scope[@]}" "${ENDPOINT[@]}" --set replicaCount=null
  notes "$WORK/notes-standby-$set-two" "${scope[@]}" "${ENDPOINT[@]}" "${HA[@]}"
  check_same "notes-$set-1" "$WORK/notes-standby-$set-1" "$WORK/notes-standby-$set-one"
  check_same "notes-$set-null" "$WORK/notes-standby-$set-null" "$WORK/notes-standby-$set-one"
  file="$WORK/notes-standby-$set-one"
  if [ -s "$file" ]; then
    grep -qiwE 'lease|leases|standby|replicaCount' "$file" &&
      fail "NOTES.txt with one replica ($set) speaks of a Lease, a standby or replicaCount"
  fi
  file="$WORK/notes-standby-$set-two"
  [ -s "$file" ] || { fail "NOTES.txt with two replicas ($set) is empty"; continue; }
  since=$FAILURES
  collapsed "$file" | grep -qF "$NAMESPACE/kausa-leader" ||
    fail "NOTES.txt with two replicas ($set) does not name the Lease $NAMESPACE/kausa-leader"
  grep -qF "kubectl -n $NAMESPACE get lease kausa-leader -o jsonpath='{.spec.holderIdentity}'" \
    "$file" || fail "NOTES.txt with two replicas ($set) lacks the command that shows the holder"
  collapsed "$file" | grep -qi 'budget' ||
    fail "NOTES.txt with two replicas ($set) does not say each replica has its own budget"
  collapsed "$file" | grep -qF 'replicaCount' ||
    fail "NOTES.txt with two replicas ($set) does not say to scale only through replicaCount"
  # The rest of NOTES.txt is the one-replica text.
  grep -vxFf "$WORK/notes-standby-$set-two" "$WORK/notes-standby-$set-one" >"$file.lost" || true
  [ -s "$file.lost" ] &&
    fail "NOTES.txt with two replicas ($set) drops lines of the one-replica text:" \
      "$(cat "$file.lost")"
  passed "$since" "NOTES.txt with two replicas ($set)"
done

# helm lint --strict with two replicas, in both scopes.
if "$HELM" lint --strict "$CHART" "${HA_CL[@]}" "${HA[@]}" >"$WORK/lint-standby" 2>&1; then
  ok "helm lint --strict with replicaCount=2"
else
  fail "helm lint --strict with replicaCount=2: $(cat "$WORK/lint-standby")"
fi
if "$HELM" lint --strict "$CHART" "${HA_NS[@]}" "${HA[@]}" >"$WORK/lint-standby" 2>&1; then
  ok "helm lint --strict with replicaCount=2, namespaced"
else
  fail "helm lint --strict with replicaCount=2, namespaced: $(cat "$WORK/lint-standby")"
fi

# Refusals (helm template: helm lint does not fail on a template's fail).
not_replicas() {
  echo "replicaCount must be 1 or 2, not '$1': a second replica is a standby that takes over through a Lease (docs/adr/0019-high-availability.md)"
}
for value in 0 3 two -1 1.5 10; do
  refuse "replicas-$value" "$(not_replicas "$value")" "${HA_CL[@]}" --set "replicaCount=$value"
done
refuse replicas-empty "$(not_replicas '')" "${HA_CL[@]}" --set replicaCount=
refuse replicas-string-3 "$(not_replicas 3)" "${HA_CL[@]}" --set-string replicaCount=3
refuse replicas-namespaced "$(not_replicas 3)" "${HA_NS[@]}" --set replicaCount=3
refuse replicas-endpoint "$(not_replicas 0)" "${HA_CL[@]}" "${ENDPOINT[@]}" \
  --set replicaCount=0
# The same in namespaced scope, the release namespace listed or not.
for value in 0 3 two -1 1.5; do
  refuse "replicas-namespaced-$value" "$(not_replicas "$value")" "${HA_NS[@]}" \
    --set "replicaCount=$value"
  refuse "replicas-namespaced-own-$value" "$(not_replicas "$value")" "${HA_NS_OWN[@]}" \
    --set "replicaCount=$value"
done
# --set keeps 1.5 a string; a values file and --set-json give a float64 or a bool, refused too.
VALUES_FLOAT="$WORK/values-replicas-float.yaml"
echo 'replicaCount: 1.5' >"$VALUES_FLOAT"
refuse replicas-file-1.5 "$(not_replicas 1.5)" "${HA_CL[@]}" -f "$VALUES_FLOAT"
refuse replicas-file-1.5-namespaced "$(not_replicas 1.5)" "${HA_NS[@]}" -f "$VALUES_FLOAT"
refuse replicas-json-2.5 "$(not_replicas 2.5)" "${HA_CL[@]}" --set-json replicaCount=2.5
refuse replicas-json-true "$(not_replicas true)" "${HA_CL[@]}" --set-json replicaCount=true

# --- The coverage gaps of v0.5 (no ADR) ------------------------------------------------

# D011 about a CronJob and D008's new reasons need no new permission: a CronJob in an event's
# `regarding` is its own top-level owner, and the only batch hop is Job -> CronJob, a `get` of the
# Job. No rule names cronjobs, in either scope, with one replica or two, and every batch rule is
# `get` on jobs alone.
# coverage_rules NAME ARGS...: renders with ARGS and checks the rules of the ClusterRole and Roles.
coverage_rules() {
  local name=$1 out="$WORK/coverage-$1" since=$FAILURES
  shift
  if ! render "$out" "$RELEASE" "$CHART" "$@"; then
    fail "coverage/$name does not render: $(cat "$out.err")"
    return 0
  fi
  rules_of "$out" >"$out.rules"
  if grep -qi cronjob "$out.rules"; then
    fail "coverage/$name: a rule names cronjobs: $(cat "$out.rules")"
  fi
  if ! grep -qx 'resources:\["jobs"\]' "$out.rules"; then
    fail "coverage/$name: no rule on jobs, the Job -> CronJob hop"
  fi
  awk '/^-apiGroups:/ { group = $0; next }
       group == "-apiGroups:[\"batch\"]" && /^(resources|verbs):/ { print }' "$out.rules" |
    sort -u >"$out.batch"
  printf '%s\n' 'resources:["jobs"]' 'verbs:["get"]' >"$out.batch-expected"
  if ! diff -u "$out.batch-expected" "$out.batch" >"$out.batch-diff"; then
    fail "coverage/$name: the batch rules are not get on jobs: $(cat "$out.batch-diff")"
  fi
  passed "$since" "coverage/$name: no rule names cronjobs, batch is get on jobs"
}
coverage_rules cluster "${HA_CL[@]}"
coverage_rules namespaced "${HA_NS[@]}"
coverage_rules cluster-standby "${HA_CL[@]}" --set replicaCount=2
coverage_rules namespaced-standby "${HA_NS[@]}" --set replicaCount=2
coverage_rules cluster-endpoint-history "${HA_CL[@]}" "${ENDPOINT[@]}" "${HISTORY_ON[@]}"

# The kind test seeds a Job failed by its podFailurePolicy (D008, reason PodFailurePolicy), with
# backoffLimit 6 so that the reason cannot be BackoffLimitExceeded, and no CronJob or ResourceQuota:
# a count/jobs.batch quota in e2e-shop would refuse the other Jobs. The manifest is read with helm
# (fromYaml in a throwaway chart), so its layout is free; tests/e2e/run.sh is only read.
E2E="$ROOT/tests/e2e"
SEEDED="$WORK/e2e-seeded"
mkdir -p "$SEEDED/templates"
printf '%s\n' 'apiVersion: v2' 'name: e2e-seeded' 'version: 0.0.0' >"$SEEDED/Chart.yaml"
cp "$E2E/failures.yaml" "$SEEDED/failures.yaml"
cat >"$SEEDED/templates/seeded.yaml" <<'TEMPLATE'
{{- range $doc := regexSplit "(?m)^---[ \t]*$" (.Files.Get "failures.yaml") -1 }}
{{- $o := fromYaml $doc }}
{{- if $o.kind }}
# kind {{ $o.kind }} {{ dig "metadata" "namespace" "" $o }}/{{ dig "metadata" "name" "" $o }}
{{- end }}
{{- if and (eq (toString $o.kind) "Job") (eq (dig "metadata" "name" "" $o) "policy-job") }}
{{- $spec := $o.spec }}
{{- $pod := $spec.template.spec }}
{{- $containers := list }}
{{- range $pod.containers }}
{{- $containers = append $containers (dict "name" .name "image" .image "imagePullPolicy" .imagePullPolicy "args" .args) }}
{{- end }}
# policy-job {{ dict "namespace" $o.metadata.namespace "backoffLimit" $spec.backoffLimit "podFailurePolicy" $spec.podFailurePolicy "restartPolicy" $pod.restartPolicy "containers" $containers | toJson }}
{{- end }}
{{- end }}
TEMPLATE
since=$FAILURES
if ! render "$SEEDED/out" e2e-seeded "$SEEDED"; then
  fail "e2e/failures.yaml cannot be read: $(cat "$SEEDED/out.err")"
else
  grep '^# kind ' "$SEEDED/out" | sed 's/^# kind //' >"$SEEDED/kinds"
  [ "$(grep -cx 'Job e2e-shop/policy-job' "$SEEDED/kinds" || true)" = 1 ] ||
    fail "e2e/failures.yaml: not exactly one Job policy-job in e2e-shop"
  if grep -Eq '^(CronJob|ResourceQuota) ' "$SEEDED/kinds"; then
    fail "e2e/failures.yaml: seeds a CronJob or a ResourceQuota: $(cat "$SEEDED/kinds")"
  fi
  # The seeded objects before this change stay.
  for object in 'Namespace /e2e-shop' 'Deployment e2e-shop/crasher' 'Job e2e-shop/failing-job' \
    'Deployment e2e-shop/stranded' 'Deployment e2e-shop/stalled' 'Deployment e2e-shop/refused'; do
    grep -qxF "$object" "$SEEDED/kinds" || fail "e2e/failures.yaml: $object is gone"
  done
  POLICY_JOB='{"backoffLimit":6,"containers":[{"args":["--no-such-flag"],"image":"kausa:e2e","imagePullPolicy":"Never","name":"e2e-policy-task"}],"namespace":"e2e-shop","podFailurePolicy":{"rules":[{"action":"FailJob","onExitCodes":{"containerName":"e2e-policy-task","operator":"In","values":[2]}}]},"restartPolicy":"Never"}'
  got=$(sed -n 's/^# policy-job //p' "$SEEDED/out")
  [ "$got" = "$POLICY_JOB" ] ||
    fail "e2e/failures.yaml: policy-job is not the contract's Job: expected $POLICY_JOB, got ${got:-nothing}"
  grep -q '^# - policy-job: ' "$E2E/failures.yaml" ||
    fail "e2e/failures.yaml: no '# - policy-job: ' bullet in the header comment"
fi
passed "$since" "e2e/failures.yaml: policy-job fails by its podFailurePolicy, no CronJob or quota"

# tests/e2e/run.sh: the Job is explained as D008, its incident's sample.reason is PodFailurePolicy,
# its request carries the note without its real names, its names are in the leak loop, it is
# notified once, and the notifications sent count it (one per failing workload, now six).
since=$FAILURES
RUN_SH="$E2E/run.sh"
bash -n "$RUN_SH" || fail "e2e/run.sh: bash -n fails"
grep -Eq '^[[:space:]]*explanations_of Job policy-job D008 batch/v1[[:space:]]*$' "$RUN_SH" ||
  fail "e2e/run.sh: no 'explanations_of Job policy-job D008 batch/v1'"
# shellcheck disable=SC2016 # the words are run.sh's own text, not to be expanded
for words in PodFailurePolicy 'matching FailJob rule at index 0' \
  '("Job", "policy-job", "D008", "batch/v1")' '{outcome=\"sent\"}" && $2 >= 6'; do
  grep -qF -- "$words" "$RUN_SH" || fail "e2e/run.sh: no '$words'"
done
# shellcheck disable=SC2016 # the same
leak_loop=$(grep -E '^for name in "\$TARGET" "\$OTHER" ' "$RUN_SH" || true)
[ -n "$leak_loop" ] || fail "e2e/run.sh: no real-name leak loop"
for name in crasher failing-job stranded stalled refused policy-job e2e-policy-task; do
  case " $leak_loop " in
    *" $name "* | *" $name;"*) ;;
    *) fail "e2e/run.sh: $name is not in the real-name leak loop" ;;
  esac
done
passed "$since" "e2e/run.sh: policy-job is explained, redacted, leak-checked and notified"

# --- Refill budget (docs/adr/0021-refill-the-call-budget.md) ------------------------------------

# kausa refills the hourly budget at a steady rate from a start burst of 3. The chart passes the
# same two flags as before, with no new value, no new default and no refusal: only the comments of
# values.yaml and the two-replica paragraph of NOTES.txt change.

# budget_args FILE: the values of --max-calls-per-hour and --max-calls-per-day, in that order,
# one per line, as rendered (quoted).
budget_args() {
  args "$1" | awk '
    want { print substr($0, 3); want = 0 }
    $0 == "- --max-calls-per-hour" || $0 == "- --max-calls-per-day" { want = 1 }'
}

# check_budget_args NAME HOUR DAY ARGS...: one --max-calls-per-hour "HOUR" and one
# --max-calls-per-day "DAY", in that order.
check_budget_args() {
  local name=$1 hour=$2 day=$3
  shift 3
  local out="$WORK/budget-$name"
  if ! render "$out" "$RELEASE" "$CHART" "$@"; then
    fail "budget/$name does not render: $(cat "$out.err")"
    return 0
  fi
  local got
  got=$(budget_args "$out" | paste -sd' ' -)
  if [ "$got" = "\"$hour\" \"$day\"" ] &&
    [ "$(args "$out" | grep -cxF -- '- --max-calls-per-hour')" = 1 ] &&
    [ "$(args "$out" | grep -cxF -- '- --max-calls-per-day')" = 1 ]; then
    ok "budget/$name"
  else
    fail "budget/$name: expected --max-calls-per-hour \"$hour\" --max-calls-per-day \"$day\"," \
      "got: $got"
  fi
}

for set in cluster namespaced; do
  case $set in
    cluster) scope=(--set rbac.scope=cluster) ;;
    *) scope=(--set rbac.scope=namespaced --set 'rbac.namespaces={e2e-a}') ;;
  esac
  check_budget_args "$set-defaults" 30 200 "${scope[@]}" "${ENDPOINT[@]}"
  # The e2e values: one call a second for the existing runs, one every 10 s for the budget case.
  check_budget_args "$set-3600" 3600 200 "${scope[@]}" "${ENDPOINT[@]}" \
    --set budget.callsPerHour=3600
  check_budget_args "$set-360" 360 200 "${scope[@]}" "${ENDPOINT[@]}" \
    --set budget.callsPerHour=360
  check_budget_args "$set-two-replicas" 30 200 "${scope[@]}" "${ENDPOINT[@]}" --set replicaCount=2
done
check_budget_args zero 0 0 --set rbac.scope=cluster "${ENDPOINT[@]}" \
  --set budget.callsPerHour=0 --set budget.callsPerDay=0
check_budget_args largest 1000000 1000000 --set rbac.scope=cluster "${ENDPOINT[@]}" \
  --set budget.callsPerHour=1000000 --set budget.callsPerDay=1000000
# A dry run passes them too, as before; detection only (no endpoint, no dry run) passes neither.
check_budget_args dry-run 360 200 --set rbac.scope=cluster "${DRY[@]}" \
  --set budget.callsPerHour=360
out="$WORK/budget-detect-only"
if render "$out" "$RELEASE" "$CHART" --set rbac.scope=cluster --set budget.callsPerHour=360; then
  if args "$out" | grep -qE -- '^- --max-calls-per-(hour|day)$'; then
    fail "budget/detect-only: detection only passes a budget flag"
  else
    ok "budget/detect-only"
  fi
else
  fail "budget/detect-only does not render: $(cat "$out.err")"
fi

# values.yaml: the budget block and replicaCount keep their keys and defaults; their comments
# describe the refill, the start burst after a takeover and the advice for the daily cap.
VALUES="$CHART/values.yaml"
budget_values=$(awk '
  $0 == "budget:" { on = 1; print; next }
  on && /^[^ #]/ { on = 0 }
  on && /^ *#/ { next }
  on && NF { print }' "$VALUES")
expected_budget_values=$(printf '%s\n' 'budget:' '  callsPerHour: 30' '  callsPerDay: 200' \
  '  maxInputTokens: 6000' '  dedupHours: 6')
if [ "$budget_values" = "$expected_budget_values" ]; then
  ok "values.yaml: the budget block keeps its keys and defaults"
else
  fail "values.yaml: the budget block changed: $budget_values"
fi
if grep -qx 'replicaCount: 1' "$VALUES"; then
  ok "values.yaml: replicaCount stays 1"
else
  fail "values.yaml: replicaCount is no longer 1"
fi

# comments_of KEY: the comment lines right above a top-level KEY and inside its block, without
# the '#', joined by spaces.
comments_of() {
  awk -v key="$1" '
    /^#/ { above = above " " substr($0, 2); next }
    $0 == key ":" || index($0, key ": ") == 1 { print above; on = 1; above = ""; next }
    on && /^ +#/ { sub(/^ +#/, ""); print; next }
    on && /^[^ ]/ { on = 0 }
    { above = "" }' "$VALUES" | tr '\n' ' ' | tr -s ' '
}
since=$FAILURES
budget_comments=$(comments_of budget)
for words in 'start burst of 3' '3600/callsPerHour' 'callsPerHour <= callsPerDay / 24'; do
  printf '%s' "$budget_comments" | grep -qF -- "$words" ||
    fail "values.yaml: the budget comments do not say '$words': $budget_comments"
done
replica_comments=$(comments_of replicaCount)
for words in 'own call budget' 'at most 3 calls' 'twice'; do
  printf '%s' "$replica_comments" | grep -qF -- "$words" ||
    fail "values.yaml: the replicaCount comments do not say '$words': $replica_comments"
done
passed "$since" "values.yaml: the comments describe the refilling budget"

# NOTES.txt with two replicas: a takeover brings at most 3 calls, and the word budget stays.
for set in cluster namespaced; do
  file="$WORK/notes-standby-$set-two"
  [ -s "$file" ] || { fail "NOTES.txt with two replicas ($set) is empty"; continue; }
  since=$FAILURES
  collapsed "$file" | grep -qi 'budget' ||
    fail "NOTES.txt with two replicas ($set) no longer mentions the budget"
  collapsed "$file" | grep -qF 'at most 3' ||
    fail "NOTES.txt with two replicas ($set) does not say a takeover brings at most 3 calls"
  passed "$since" "NOTES.txt with two replicas ($set) describes the refilling budget"
done

# The budget values add no refusal, and the earlier checks still come first.
refuse budget-scope-first "$NOT_SET" --set budget.callsPerHour=360
refuse budget-v0.1-checks-stay "model.name is required with model.endpoint" \
  --set rbac.scope=cluster --set model.endpoint=https://api.example.com/v1 \
  --set budget.callsPerHour=360
refuse budget-replicas-stay "$(not_replicas 3)" --set rbac.scope=cluster \
  --set budget.callsPerHour=360 --set replicaCount=3

# --- Endpoint check (docs/adr/0020-check-the-endpoint.md) ---------------------------------------

# Acceptance checks written before the chart change: model.timeoutSeconds (--model-timeout in the
# Deployment), endpointCheck.enabled, the helm test Pod <fullname>-endpoint-check and, with the
# NetworkPolicy on, its own ordinary policy of the same name; NOTES.txt; and the two refusals.
# Blocks are compared row by row with indentation and quotes removed; lists are split into items
# by the indentation of their first row.

EP_NP=(--set networkPolicy.enabled=true --set 'networkPolicy.apiServer.cidrs[0]=192.0.2.1/32'
  --set 'networkPolicy.endpoint.to[0].ipBlock.cidr=192.0.2.2/32'
  --set 'networkPolicy.endpoint.ports[0].port=443'
  --set 'networkPolicy.endpoint.ports[0].protocol=TCP')
EP_NP_NOTIFY=(--set 'networkPolicy.notifications.to[0].ipBlock.cidr=198.51.100.0/24')
bad_model_timeout() {
  echo "model.timeoutSeconds must be a whole number of seconds from 10 to 600, not '$1'"
}
bad_endpoint_check() { echo "endpointCheck.enabled must be true or false, not '$1'"; }
not_an_endpoint_check_map() {
  echo "endpointCheck must be a map with enabled: true or false, not '$1'"
}

# ep_render NAME ARGS...: renders with rbac.scope=cluster into $WORK/ep-NAME (empty on failure).
ep_render() {
  local name=$1
  shift
  if ! render "$WORK/ep-$name" "$RELEASE" "${EP_CHART:-$CHART}" --set rbac.scope=cluster "$@"; then
    fail "endpoint check, $name: does not render: $(cat "$WORK/ep-$name.err")"
    : >"$WORK/ep-$name"
  fi
}

# doc_of FILE KIND [NAME]: one document of the render, without the '%%' separator.
doc_of() { docs "$1" "$2" "$NAMESPACE" "${3-}" | sed '/^%%$/d'; }

# blocks KEY: every block on stdin that starts with a line 'KEY:' at any indentation, as rows
# without the key's indentation, quotes and comments, each block followed by a line '%%'.
blocks() {
  awk -v key="$1" '
    function done() { if (on) print "%%"; on = 0 }
    {
      if (on) {
        if ($0 ~ /^ *$/) next
        match($0, /^ */)
        if (RLENGTH > ind) {
          s = substr($0, ind + 1)
          gsub(/"/, "", s)
          if (s !~ /^ *#/) print s
          next
        }
        done()
      }
      if ($0 ~ "^ *" key ":$") { on = 1; match($0, /^ */); ind = RLENGTH }
    }
    END { done() }'
}

# first_block KEY: the rows of the first such block, without the separator.
first_block() { blocks "$1" | awk '$0 == "%%" { exit } { print }'; }

# trimmed: the rows on stdin without their indentation.
trimmed() { sed 's/^ *//'; }

# joined: each block on stdin on one line, rows trimmed and joined by ' | ', sorted.
joined() {
  awk '$0 == "%%" { print row; row = ""; next }
       { sub(/^ */, ""); row = row (row == "" ? "" : " | ") $0 }' | sort
}

# meta_map KEY: the rows of metadata.KEY (labels or annotations) of the document on stdin, quotes
# removed, sorted.
meta_map() {
  awk -v key="$1" '
    /^metadata:$/ { m = 1; next }
    /^[^ ]/ { m = 0 }
    m && $0 == "  " key ":" { on = 1; next }
    m && on && /^    / { s = substr($0, 5); gsub(/"/, "", s); print s; next }
    { on = 0 }' | sort
}

# items: the items of the list block on stdin (rows from first_block), one item per line, its rows
# trimmed and joined by ' | '. An item starts at a row '- ' indented as the first row.
items() {
  awk 'NR == 1 { match($0, /^ */); ind = RLENGTH }
       { match($0, /^ */); s = substr($0, RLENGTH + 1) }
       RLENGTH == ind && s ~ /^- / { if (row != "") print row; row = s; next }
       { row = row " | " s }
       END { if (row != "") print row }'
}

# pod_args FILE: the Pod's container args, one per line, without '- ' and quotes.
pod_args() { doc_of "$1" Pod "$CHECK_POD" | first_block args | trimmed | sed 's/^- //'; }

# deployment_args FILE: the same for the Deployment.
deployment_args() { args "$1" | sed 's/^- //' | tr -d '"'; }

# Where the Pod and its policy render: with model.endpoint, without dryRun and with
# endpointCheck.enabled (true, or missing); the policy also needs networkPolicy.enabled.
since=$FAILURES
for case in \
  "defaults|0|0|1|" \
  "endpoint|1|0|1|endpoint" \
  "endpoint-no-key|1|0|1|endpoint-no-key" \
  "dry|0|0|1|dry" \
  "endpoint-dry|0|0|1|endpoint,dry" \
  "endpoint-off|0|0|1|endpoint,off" \
  "endpoint-off-string|0|0|1|endpoint,off-string" \
  "endpoint-on-string|1|0|1|endpoint,on-string" \
  "endpoint-no-map|1|0|1|endpoint,no-map" \
  "endpoint-np|1|1|1|endpoint,np" \
  "endpoint-np-off|0|0|1|endpoint,np,off" \
  "dry-np|0|0|1|dry,np-api" \
  "endpoint-np-notify|1|1|1|endpoint,np,notify"; do
  IFS='|' read -r name pods policies deployments sets <<<"$case"
  set_values=()
  case ",$sets," in *,endpoint,*) set_values+=("${ENDPOINT[@]}") ;; esac
  case ",$sets," in *,endpoint-no-key,*) set_values+=("${ENDPOINT_NO_KEY[@]}") ;; esac
  case ",$sets," in *,dry,*) set_values+=(--set dryRun=true) ;; esac
  case ",$sets," in *,off,*) set_values+=(--set endpointCheck.enabled=false) ;; esac
  case ",$sets," in *,off-string,*) set_values+=(--set-string endpointCheck.enabled=false) ;; esac
  case ",$sets," in *,on-string,*) set_values+=(--set-string endpointCheck.enabled=true) ;; esac
  case ",$sets," in *,no-map,*) set_values+=(--set endpointCheck=null) ;; esac
  case ",$sets," in *,np,*) set_values+=("${EP_NP[@]}") ;; esac
  case ",$sets," in
    *,np-api,*) set_values+=(--set networkPolicy.enabled=true
      --set 'networkPolicy.apiServer.cidrs[0]=192.0.2.1/32') ;;
  esac
  case ",$sets," in *,notify,*) set_values+=("${NOTIFY_ON[@]}" "${EP_NP_NOTIFY[@]}") ;; esac
  ep_render "$name" ${set_values[@]+"${set_values[@]}"}
  file="$WORK/ep-$name"
  got_pods=$(count_docs "$file" Pod "$NAMESPACE" "$CHECK_POD")
  [ "$got_pods" = "$pods" ] || fail "endpoint check, $name: $got_pods test Pods, not $pods"
  [ "$(count_docs "$file" Pod)" = "$pods" ] ||
    fail "endpoint check, $name: another Pod renders: $(count_docs "$file" Pod)"
  got_policies=$(count_docs "$file" NetworkPolicy "$NAMESPACE" "$CHECK_POD")
  [ "$got_policies" = "$policies" ] ||
    fail "endpoint check, $name: $got_policies test NetworkPolicies, not $policies"
  [ "$(count_docs "$file" Deployment)" = "$deployments" ] ||
    fail "endpoint check, $name: not one Deployment"
  case ",$sets," in
    *,np,* | *,np-api,*)
      [ "$(count_docs "$file" NetworkPolicy "$NAMESPACE" kausa)" = 1 ] ||
        fail "endpoint check, $name: kausa's own NetworkPolicy is gone"
      ;;
  esac
done
# In the namespaced scope too.
render "$WORK/ep-namespaced" "$RELEASE" "$CHART" --set rbac.scope=namespaced \
  --set 'rbac.namespaces={e2e-a}' "${ENDPOINT[@]}" ||
  fail "endpoint check, namespaced: does not render: $(cat "$WORK/ep-namespaced.err")"
[ "$(count_docs "$WORK/ep-namespaced" Pod "$NAMESPACE" "$CHECK_POD")" = 1 ] ||
  fail "endpoint check, namespaced: no test Pod"
passed "$since" "endpoint check: the Pod and its policy render only with an endpoint, outside dryRun, with endpointCheck.enabled"

# The Pod: a test hook kept until the next helm test, with no token and no ServiceAccount of
# kausa's, the namespace's default, and a deadline of the timeout plus 60 s.
since=$FAILURES
pod=$(doc_of "$WORK/ep-endpoint" Pod "$CHECK_POD")
expected_annotations=$(printf '%s\n' 'helm.sh/hook-delete-policy: before-hook-creation' \
  'helm.sh/hook: test' | sort)
got_annotations=$(printf '%s\n' "$pod" | meta_map annotations)
[ "$got_annotations" = "$expected_annotations" ] ||
  fail "endpoint check: the Pod's annotations are not the two hook annotations: $got_annotations"
printf '%s\n' "$pod" | grep -q 'hook-succeeded' &&
  fail "endpoint check: the Pod is deleted when it succeeds (helm test --logs loses its line)"
for row in 'restartPolicy: Never' 'activeDeadlineSeconds: 180' \
  'automountServiceAccountToken: false' 'enableServiceLinks: false'; do
  printf '%s\n' "$pod" | grep -qx "  $row" || fail "endpoint check: the Pod has no '$row'"
done
printf '%s\n' "$pod" | grep -qE '^ *(serviceAccountName|serviceAccount):' &&
  fail "endpoint check: the Pod names a ServiceAccount"
printf '%s\n' "$pod" | grep -qx "  namespace: $NAMESPACE" ||
  fail "endpoint check: the Pod is not in the release namespace"
passed "$since" "endpoint check: the Pod is a test hook with no token, no ServiceAccount and a deadline"

# Its labels: instance, component, version, managed-by and chart, never app.kubernetes.io/name.
since=$FAILURES
deployment_labels=$(doc_of "$WORK/ep-endpoint" Deployment | meta_map labels)
version_label=$(printf '%s\n' "$deployment_labels" | grep '^app.kubernetes.io/version: ' || true)
chart_label=$(printf '%s\n' "$deployment_labels" | grep '^helm.sh/chart: ' || true)
expected_labels=$(printf '%s\n' 'app.kubernetes.io/component: endpoint-check' \
  'app.kubernetes.io/instance: kausa' 'app.kubernetes.io/managed-by: Helm' "$version_label" \
  "$chart_label" | sort)
pod_labels=$(printf '%s\n' "$pod" | meta_map labels)
[ "$pod_labels" = "$expected_labels" ] ||
  fail "endpoint check: the Pod's labels are not the five expected: $pod_labels"
passed "$since" "endpoint check: the Pod's labels carry no app.kubernetes.io/name"

# One container, kausa's image, pull policy, resources and security contexts, and nothing else of
# kausa's Pod: no env, ports, probes, pod labels or annotations.
since=$FAILURES
deployment=$(doc_of "$WORK/ep-endpoint" Deployment)
containers=$(printf '%s\n' "$pod" | first_block containers)
[ "$(printf '%s\n' "$containers" | items | grep -c .)" = 1 ] ||
  fail "endpoint check: the Pod has not exactly one container: $(printf '%s\n' "$containers" | items)"
containers=$(printf '%s\n' "$containers" | trimmed)
printf '%s\n' "$containers" | grep -qx -- '- name: endpoint-check' ||
  fail "endpoint check: the container is not named endpoint-check"
for key in image imagePullPolicy; do
  want=$(printf '%s\n' "$deployment" | grep -m1 -E "^ *$key: " | sed 's/^ *//' | tr -d '"')
  got=$(printf '%s\n' "$containers" | grep -m1 -E "^$key: " || true)
  [ -n "$want" ] && [ "$got" = "$want" ] || fail "endpoint check: $key is '$got', not '$want'"
done
[ "$(printf '%s\n' "$pod" | first_block resources | trimmed)" = \
  "$(printf '%s\n' "$deployment" | first_block resources | trimmed)" ] ||
  fail "endpoint check: the Pod's resources differ from the Deployment's"
[ "$(printf '%s\n' "$pod" | blocks securityContext | joined)" = \
  "$(printf '%s\n' "$deployment" | blocks securityContext | joined)" ] ||
  fail "endpoint check: the Pod's security contexts differ from the Deployment's:" \
    "$(printf '%s\n' "$pod" | blocks securityContext | joined)"
for key in env envFrom ports livenessProbe readinessProbe startupProbe command; do
  printf '%s\n' "$pod" | grep -qE "^ *$key:" && fail "endpoint check: the Pod has $key"
done
ep_render pod-extras "${ENDPOINT[@]}" --set podLabels.team=blue \
  --set podAnnotations.example\\.com/note=kept --set 'imagePullSecrets[0].name=regcred'
extras=$(doc_of "$WORK/ep-pod-extras" Pod "$CHECK_POD")
printf '%s\n' "$extras" | grep -qE 'team: blue|example.com/note' &&
  fail "endpoint check: the Pod takes podLabels or podAnnotations"
[ "$(printf '%s\n' "$extras" | first_block imagePullSecrets | trimmed)" = '- name: regcred' ] ||
  fail "endpoint check: the Pod does not take imagePullSecrets"
passed "$since" "endpoint check: one container with kausa's image, resources and security contexts, nothing else"

# Scheduling from the values: nodeSelector, tolerations and affinity as the Deployment has them,
# never the standby's own anti-affinity.
since=$FAILURES
ep_render scheduling "${ENDPOINT[@]}" --set nodeSelector.disktype=ssd \
  --set 'tolerations[0].key=dedicated' --set 'tolerations[0].operator=Exists' \
  --set-json 'affinity={"nodeAffinity":{"requiredDuringSchedulingIgnoredDuringExecution":{"nodeSelectorTerms":[{"matchExpressions":[{"key":"kubernetes.io/arch","operator":"In","values":["arm64"]}]}]}}}'
for key in nodeSelector tolerations affinity; do
  got=$(doc_of "$WORK/ep-scheduling" Pod "$CHECK_POD" | first_block "$key" | trimmed)
  want=$(doc_of "$WORK/ep-scheduling" Deployment | first_block "$key" | trimmed)
  [ -n "$got" ] && [ "$got" = "$want" ] || fail "endpoint check: the Pod's $key: '$got'"
done
ep_render standby "${ENDPOINT[@]}" --set replicaCount=2
doc_of "$WORK/ep-standby" Pod "$CHECK_POD" | grep -qE '^ *(affinity|podAntiAffinity):' &&
  fail "endpoint check: the Pod takes the standby's anti-affinity"
passed "$since" "endpoint check: the Pod schedules by the values only"

# The args, for each combination of allowHttp, jsonSchema and the key's Secret.
since=$FAILURES
expect_pod_args() {
  local name=$1
  shift
  local got
  got=$(pod_args "$WORK/ep-$name")
  [ "$got" = "$(printf '%s\n' "$@")" ] ||
    fail "endpoint check, $name: the Pod's args are: $(printf '%s' "$got" | tr '\n' ' ')"
}
CHECK_ARGS=(check-endpoint --endpoint https://api.example.com/v1 --model m --model-timeout 120)
KEY_ARGS=(--api-key-file /var/run/secrets/kausa/api-key)
expect_pod_args endpoint "${CHECK_ARGS[@]}" "${KEY_ARGS[@]}"
expect_pod_args endpoint-no-key "${CHECK_ARGS[@]}"
ep_render no-schema "${ENDPOINT[@]}" --set model.jsonSchema=false
expect_pod_args no-schema "${CHECK_ARGS[@]}" --no-json-schema "${KEY_ARGS[@]}"
ep_render http --set model.endpoint=http://ollama.example.svc:11434/v1 --set model.name=m \
  --set model.allowHttp=true
expect_pod_args http check-endpoint --endpoint http://ollama.example.svc:11434/v1 --model m \
  --model-timeout 120 --allow-http
ep_render http-no-schema-key --set model.endpoint=http://ollama.example.svc:11434/v1 \
  --set model.name=m --set model.allowHttp=true --set model.jsonSchema=false \
  --set model.apiKey.existingSecret=k --set model.timeoutSeconds=300
expect_pod_args http-no-schema-key check-endpoint --endpoint http://ollama.example.svc:11434/v1 \
  --model m --model-timeout 300 --allow-http --no-json-schema "${KEY_ARGS[@]}"
doc_of "$WORK/ep-http-no-schema-key" Pod "$CHECK_POD" | grep -qx '  activeDeadlineSeconds: 360' ||
  fail "endpoint check: activeDeadlineSeconds is not the timeout plus 60 with 300"
# Every combination of the three, in the order kausa run takes them: --allow-http, then
# --no-json-schema, then --api-key-file.
for http in no yes; do
  for schema in yes no; do
    for key in yes no; do
      name="combo-http-$http-schema-$schema-key-$key"
      if [ "$http" = yes ]; then
        url=http://ollama.example.svc:11434/v1
        set_values=(--set "model.endpoint=$url" --set model.allowHttp=true)
      else
        url=https://api.example.com/v1
        set_values=(--set "model.endpoint=$url")
      fi
      set_values+=(--set model.name=m)
      want_args=(check-endpoint --endpoint "$url" --model m --model-timeout 120)
      [ "$http" = yes ] && want_args+=(--allow-http)
      if [ "$schema" = no ]; then
        set_values+=(--set model.jsonSchema=false)
        want_args+=(--no-json-schema)
      fi
      if [ "$key" = yes ]; then
        set_values+=(--set model.apiKey.existingSecret=k)
        want_args+=("${KEY_ARGS[@]}")
      fi
      ep_render "$name" "${set_values[@]}"
      expect_pod_args "$name" "${want_args[@]}"
    done
  done
done
for seconds in 10 600; do
  ep_render "timeout-$seconds" "${ENDPOINT[@]}" --set "model.timeoutSeconds=$seconds"
  doc_of "$WORK/ep-timeout-$seconds" Pod "$CHECK_POD" |
    grep -qx "  activeDeadlineSeconds: $((seconds + 60))" ||
    fail "endpoint check: activeDeadlineSeconds is not $((seconds + 60)) with $seconds"
done
passed "$since" "endpoint check: the Pod's args and deadline follow the model values"

# One volume, the API key's, as the Deployment mounts it; none without the Secret; never the
# notification Secret, even with notifications on.
since=$FAILURES
for name in endpoint endpoint-np-notify; do
  file="$WORK/ep-$name"
  got=$(doc_of "$file" Pod "$CHECK_POD" | first_block volumes | items)
  want=$(doc_of "$WORK/ep-endpoint" Deployment | first_block volumes | items)
  [ -n "$got" ] && [ "$got" = "$want" ] ||
    fail "endpoint check, $name: the Pod's volumes are not the API key's alone: $got"
  [ "$(printf '%s\n' "$got" | grep -c .)" = 1 ] ||
    fail "endpoint check, $name: not exactly one volume"
  got=$(doc_of "$file" Pod "$CHECK_POD" | first_block volumeMounts | items)
  want=$(doc_of "$WORK/ep-endpoint" Deployment | first_block volumeMounts | items)
  [ -n "$got" ] && [ "$got" = "$want" ] ||
    fail "endpoint check, $name: the Pod's volume mounts are not the API key's alone: $got"
  doc_of "$file" Pod "$CHECK_POD" | grep -qE "kausa-notify|$NOTIFY_SECRET" &&
    fail "endpoint check, $name: the Pod mounts the notification Secret"
done
doc_of "$WORK/ep-endpoint-np-notify" Deployment | grep -q "$NOTIFY_SECRET" ||
  fail "endpoint check: the notification render has no notification Secret to leave out"
doc_of "$WORK/ep-endpoint-no-key" Pod "$CHECK_POD" | grep -qE '^ *(volumes|volumeMounts):' &&
  fail "endpoint check: the Pod has a volume without model.apiKey.existingSecret"
passed "$since" "endpoint check: the Pod mounts the API key's Secret only, and only with it"

# No selector built on kausa.selectorLabels matches the Pod: the metrics Service, the
# PodDisruptionBudget and the anti-affinity of two replicas, and kausa's NetworkPolicy.
since=$FAILURES
# matches: whether every row of the selector block on stdin is one of the rows in POD_LABELS.
matches() {
  local row
  while IFS= read -r row; do
    [ -z "$row" ] && continue
    printf '%s\n' "$POD_LABELS" | grep -qxF -- "$row" || return 1
  done
  return 0
}
ep_render selectors "${ENDPOINT[@]}" "${EP_NP[@]}" --set replicaCount=2
file="$WORK/ep-selectors"
POD_LABELS=$(doc_of "$file" Pod "$CHECK_POD" | meta_map labels)
[ -n "$POD_LABELS" ] || fail "endpoint check: the selector render has no test Pod"
selectors=0
for source in "Service|kausa-metrics|selector" "PodDisruptionBudget|kausa|matchLabels" \
  "Deployment||matchLabels" "NetworkPolicy|kausa|matchLabels"; do
  IFS='|' read -r kind name key <<<"$source"
  while IFS= read -r selector; do
    [ -z "$selector" ] && continue
    selectors=$((selectors + 1))
    if printf '%s\n' "$selector" | tr '|' '\n' | sed 's/^ *//; s/ *$//' | matches; then
      fail "endpoint check: the $kind's $key selects the test Pod: $selector"
    fi
  done <<<"$(doc_of "$file" "$kind" "$name" | blocks "$key" | joined)"
done
# The Service, the PodDisruptionBudget, the Deployment's selector and its anti-affinity, and
# kausa's NetworkPolicy.
[ "$selectors" -ge 5 ] || fail "endpoint check: only $selectors selectors were found"
# The test policy selects the test Pod, and not kausa's own Pods.
test_selector=$(doc_of "$file" NetworkPolicy "$CHECK_POD" | first_block matchLabels | trimmed)
if [ -z "$test_selector" ] || ! printf '%s\n' "$test_selector" | matches; then
  fail "endpoint check: the test NetworkPolicy does not select the test Pod: $test_selector"
fi
POD_LABELS=$(doc_of "$file" Deployment | blocks labels | trimmed |
  awk '$0 == "%%" { n++; next } n == 1')
[ -n "$test_selector" ] && printf '%s\n' "$test_selector" | matches &&
  fail "endpoint check: the test NetworkPolicy selects kausa's Pods"
passed "$since" "endpoint check: no selector of kausa's matches the test Pod, and its policy selects it alone"

# The test NetworkPolicy: an ordinary object (not a hook), no ingress, egress to DNS and the
# endpoint only, each rule as kausa's own policy renders it.
since=$FAILURES
for name in endpoint-np endpoint-np-notify; do
  file="$WORK/ep-$name"
  policy=$(doc_of "$file" NetworkPolicy "$CHECK_POD")
  main=$(doc_of "$file" NetworkPolicy kausa)
  printf '%s\n' "$policy" | grep -q 'helm.sh/hook' && fail "endpoint check, $name: the policy is a hook"
  types=$(printf '%s\n' "$policy" |
    awk '/^  policyTypes:/ { on = 1; print; next } on && /^  [^ ]/ { on = 0 } on' | tr -d ' []-')
  case "$types" in *Ingress*Egress* | *Egress*Ingress*) ;;
    *) fail "endpoint check, $name: policyTypes is not Ingress and Egress: $types" ;; esac
  [ -z "$(printf '%s\n' "$policy" | first_block ingress)" ] ||
    fail "endpoint check, $name: the test policy allows ingress"
  printf '%s\n' "$policy" | grep -qE '^ *(- )?from:' && fail "endpoint check, $name: a from: peer"
  rules=$(printf '%s\n' "$policy" | first_block egress | items)
  main_rules=$(printf '%s\n' "$main" | first_block egress | items)
  [ "$(printf '%s\n' "$rules" | grep -c .)" = 2 ] ||
    fail "endpoint check, $name: not exactly two egress rules: $rules"
  dns=$(printf '%s\n' "$main_rules" | grep -F 'port: 53' || true)
  endpoint_rule=$(printf '%s\n' "$main_rules" | grep -F '192.0.2.2/32' || true)
  if [ -z "$dns" ] || ! printf '%s\n' "$rules" | grep -qxF -- "$dns"; then
    fail "endpoint check, $name: the DNS rule differs from kausa's: $rules"
  fi
  if [ -z "$endpoint_rule" ] || ! printf '%s\n' "$rules" | grep -qxF -- "$endpoint_rule"; then
    fail "endpoint check, $name: the endpoint rule differs from kausa's: $rules"
  fi
  printf '%s\n' "$policy" | grep -qE '192\.0\.2\.1/32|port: 6443|198\.51\.100\.0/24' &&
    fail "endpoint check, $name: the test policy reaches the API server or the webhook"
done
passed "$since" "endpoint check: the test NetworkPolicy allows DNS and the endpoint only, as kausa's"

# The endpoint rule of the test policy is kausa's byte for byte, as rendered (comment, indentation
# and quotes included), for every shape of networkPolicy.endpoint: an ipBlock with one port, an
# ipBlock with an exception and a port range, and an in-cluster peer with no ports (every port).
since=$FAILURES
# endpoint_rule: the raw lines of the egress rule after '# The model endpoint.' in the policy on
# stdin, the comment included.
endpoint_rule() {
  awk '$0 == "    # The model endpoint." { on = 1; print; next }
       on == 1 && /^    - / { on = 2; print; next }
       on == 2 && /^      / { print; next }
       on { exit }'
}
ep_render np-range "${ENDPOINT[@]}" --set networkPolicy.enabled=true \
  --set 'networkPolicy.apiServer.cidrs[0]=192.0.2.1/32' \
  --set-json 'networkPolicy.endpoint={"to":[{"ipBlock":{"cidr":"203.0.113.0/24","except":["203.0.113.8/29"]}}],"ports":[{"port":443,"protocol":"TCP"},{"port":8000,"endPort":8100,"protocol":"TCP"}]}'
ep_render np-in-cluster --set model.endpoint=http://ollama.example.svc:11434/v1 --set model.name=m \
  --set model.allowHttp=true --set networkPolicy.enabled=true \
  --set 'networkPolicy.apiServer.cidrs[0]=192.0.2.1/32' \
  --set-json 'networkPolicy.endpoint={"to":[{"namespaceSelector":{"matchLabels":{"kubernetes.io/metadata.name":"ollama"}},"podSelector":{"matchLabels":{"app":"ollama"}}}],"ports":[]}'
render "$WORK/ep-np-namespaced" "$RELEASE" "$CHART" --set rbac.scope=namespaced \
  --set 'rbac.namespaces={e2e-a}' "${ENDPOINT[@]}" "${EP_NP[@]}" ||
  fail "endpoint check, np-namespaced: does not render: $(cat "$WORK/ep-np-namespaced.err")"
for case in "endpoint-np|192.0.2.2/32" "endpoint-np-notify|192.0.2.2/32" \
  "np-range|endPort: 8100" "np-in-cluster|app: ollama" "np-namespaced|192.0.2.2/32"; do
  IFS='|' read -r name marker <<<"$case"
  file="$WORK/ep-$name"
  doc_of "$file" NetworkPolicy kausa | endpoint_rule >"$file.main-rule"
  doc_of "$file" NetworkPolicy "$CHECK_POD" | endpoint_rule >"$file.check-rule"
  grep -qF -- "$marker" "$file.main-rule" ||
    fail "endpoint check, $name: kausa's policy has no endpoint rule with '$marker'"
  cmp -s "$file.main-rule" "$file.check-rule" ||
    fail "endpoint check, $name: the endpoint rules differ: $(diff "$file.main-rule" "$file.check-rule")"
done
grep -qE '^ *ports:' "$WORK/ep-np-in-cluster.check-rule" &&
  fail "endpoint check, np-in-cluster: the endpoint rule has ports with networkPolicy.endpoint.ports empty"
passed "$since" "endpoint check: the test policy's endpoint rule is kausa's byte for byte"

# The Deployment passes --model-timeout with the model flags, after --no-json-schema and before
# --api-key-file; not without an endpoint or dryRun.
since=$FAILURES
# after_arg FILE ARG: the arg right after ARG in the Deployment's args.
after_arg() { deployment_args "$1" | awk -v a="$2" 'found { print; exit } $0 == a { found = 1 }'; }
before_arg() { deployment_args "$1" | awk -v a="$2" '$0 == a { print prev; exit } { prev = $0 }'; }
for case in "endpoint|120|--api-key-file" "endpoint-no-key|120|--max-calls-per-hour" \
  "no-schema|120|--api-key-file" "http-no-schema-key|300|--api-key-file" \
  "timeout-10|10|--api-key-file" "timeout-600|600|--api-key-file" "dry|120|--max-calls-per-hour"; do
  IFS='|' read -r name seconds next <<<"$case"
  file="$WORK/ep-$name"
  [ "$(deployment_args "$file" | grep -cx -- --model-timeout)" = 1 ] ||
    fail "endpoint check, $name: not one --model-timeout in the Deployment"
  [ "$(after_arg "$file" --model-timeout)" = "$seconds" ] ||
    fail "endpoint check, $name: --model-timeout is '$(after_arg "$file" --model-timeout)', not $seconds"
  [ "$(after_arg "$file" "$seconds")" = "$next" ] ||
    fail "endpoint check, $name: --model-timeout comes before '$(after_arg "$file" "$seconds")', not $next"
done
for name in no-schema http-no-schema-key; do
  [ "$(before_arg "$WORK/ep-$name" --model-timeout)" = --no-json-schema ] ||
    fail "endpoint check, $name: --model-timeout does not follow --no-json-schema"
done
deployment_args "$WORK/ep-defaults" | grep -qx -- --model-timeout &&
  fail "endpoint check: --model-timeout without an endpoint or dryRun"
ep_render timeout-no-endpoint --set model.timeoutSeconds=300
deployment_args "$WORK/ep-timeout-no-endpoint" | grep -qx -- --model-timeout &&
  fail "endpoint check: --model-timeout without an endpoint or dryRun, with the value set"
passed "$since" "endpoint check: the Deployment passes --model-timeout with the model flags"

# Accepted forms of the two values: a number from a values file (a float), a string, null (the
# default), and true or false as strings.
since=$FAILURES
ep_render timeout-json "${ENDPOINT[@]}" --set-json model.timeoutSeconds=120.0
ep_render timeout-string "${ENDPOINT[@]}" --set-string model.timeoutSeconds=45
ep_render timeout-null "${ENDPOINT[@]}" --set model.timeoutSeconds=null
for case in timeout-json:120 timeout-string:45 timeout-null:120; do
  name=${case%%:*} seconds=${case#*:}
  [ "$(after_arg "$WORK/ep-$name" --model-timeout)" = "$seconds" ] ||
    fail "endpoint check, $name: --model-timeout is not $seconds"
done
passed "$since" "endpoint check: model.timeoutSeconds is read as a whole number, 120 when missing"

# A release made before 0020 (helm upgrade --reuse-values from v0.4.1) has neither value: it
# renders --model-timeout 120 and the test Pod.
OLD_VALUES_REF=v0.4.1
if git -C "$ROOT" cat-file -e "$OLD_VALUES_REF:charts/kausa/values.yaml" 2>/dev/null; then
  since=$FAILURES
  EP_OLD_CHART="$WORK/ep-old/kausa"
  mkdir -p "$WORK/ep-old"
  cp -R "$CHART" "$EP_OLD_CHART"
  git -C "$ROOT" show "$OLD_VALUES_REF:charts/kausa/values.yaml" >"$EP_OLD_CHART/values.yaml"
  grep -qE '^endpointCheck:|timeoutSeconds:' "$EP_OLD_CHART/values.yaml" &&
    fail "the values of $OLD_VALUES_REF already have the new keys"
  EP_CHART="$EP_OLD_CHART" ep_render old-values "${ENDPOINT[@]}"
  [ "$(after_arg "$WORK/ep-old-values" --model-timeout)" = 120 ] ||
    fail "endpoint check: the values of $OLD_VALUES_REF do not render --model-timeout 120"
  [ "$(count_docs "$WORK/ep-old-values" Pod "$NAMESPACE" "$CHECK_POD")" = 1 ] ||
    fail "endpoint check: the values of $OLD_VALUES_REF do not render the test Pod"
  expect_pod_args old-values "${CHECK_ARGS[@]}" "${KEY_ARGS[@]}"
  doc_of "$WORK/ep-old-values" Pod "$CHECK_POD" | grep -qx '  activeDeadlineSeconds: 180' ||
    fail "endpoint check: the values of $OLD_VALUES_REF do not give activeDeadlineSeconds 180"
  grep -qi 'nil pointer' "$WORK/ep-old-values.err" &&
    fail "endpoint check: the values of $OLD_VALUES_REF give a nil-pointer error"
  passed "$since" "endpoint check: the values of $OLD_VALUES_REF render the defaults"
else
  echo "skipped: $OLD_VALUES_REF is not in this clone; its values are not rendered"
fi

# The new values in values.yaml, with their defaults.
since=$FAILURES
grep -qx '  timeoutSeconds: 120' "$VALUES" || fail "values.yaml: model.timeoutSeconds is not 120"
awk '/^endpointCheck:/ { on = 1; next } on && /^[^ ]/ { exit } on && /^  enabled: true$/ { found = 1 }
     END { exit !found }' "$VALUES" || fail "values.yaml: endpointCheck.enabled is not true"
passed "$since" "values.yaml: model.timeoutSeconds 120 and endpointCheck.enabled true"

# NOTES.txt names helm test, the call outside the budget and the cleanup, only when the test
# renders.
since=$FAILURES
notes "$WORK/ep-notes-on" --set rbac.scope=cluster "${ENDPOINT[@]}"
on_text=$(collapsed "$WORK/ep-notes-on")
for words in "helm test $RELEASE -n $NAMESPACE --logs" 'billed by the provider' \
  "outside kausa's budget" "kubectl -n $NAMESPACE delete pod $CHECK_POD" \
  'stays until the next helm test'; do
  printf '%s' "$on_text" | grep -qF -- "$words" ||
    fail "NOTES.txt with the test does not say '$words'"
done
for case in "defaults|" "dry|dry" "endpoint-dry|endpoint,dry" "endpoint-off|endpoint,off"; do
  IFS='|' read -r name sets <<<"$case"
  set_values=()
  case ",$sets," in *,endpoint,*) set_values+=("${ENDPOINT[@]}") ;; esac
  case ",$sets," in *,dry,*) set_values+=(--set dryRun=true) ;; esac
  case ",$sets," in *,off,*) set_values+=(--set endpointCheck.enabled=false) ;; esac
  notes "$WORK/ep-notes-$name" --set rbac.scope=cluster ${set_values[@]+"${set_values[@]}"}
  grep -qF 'helm test' "$WORK/ep-notes-$name" &&
    fail "NOTES.txt names helm test without the test ($name)"
  grep -qF "$CHECK_POD" "$WORK/ep-notes-$name" &&
    fail "NOTES.txt names the test Pod without the test ($name)"
done
passed "$since" "NOTES.txt names helm test only when the test renders"

# helm lint --strict with the test Pod and its policy.
if "$HELM" lint --strict "$CHART" --set rbac.scope=cluster "${ENDPOINT[@]}" "${EP_NP[@]}" \
  >"$WORK/lint-endpoint-check" 2>&1; then
  ok "helm lint --strict with the endpoint check and its NetworkPolicy"
else
  fail "helm lint --strict with the endpoint check: $(cat "$WORK/lint-endpoint-check")"
fi

# Refusals, with helm template (helm lint does not fail on fail): the timeout outside 10-600,
# endpointCheck.enabled other than true or false and an endpointCheck set but neither a map nor
# null (endpointCheck=false, meant as off, would otherwise render the test Pod), with or without an
# endpoint; the scope first.
for value in 9 601 0 -1 abc 1.5 1000; do
  refuse "timeout-$value" "$(bad_model_timeout "$value")" --set rbac.scope=cluster \
    --set "model.timeoutSeconds=$value"
done
refuse timeout-9-endpoint "$(bad_model_timeout 9)" --set rbac.scope=cluster "${ENDPOINT[@]}" \
  --set model.timeoutSeconds=9
refuse timeout-601-dry "$(bad_model_timeout 601)" --set rbac.scope=cluster --set dryRun=true \
  --set model.timeoutSeconds=601
refuse timeout-json-float "$(bad_model_timeout 1.5)" --set rbac.scope=cluster "${ENDPOINT[@]}" \
  --set-json model.timeoutSeconds=1.5
refuse timeout-empty "$(bad_model_timeout '')" --set rbac.scope=cluster \
  --set-string model.timeoutSeconds=
refuse endpoint-check-yes "$(bad_endpoint_check yes)" --set rbac.scope=cluster \
  --set endpointCheck.enabled=yes
refuse endpoint-check-yes-endpoint "$(bad_endpoint_check yes)" --set rbac.scope=cluster \
  "${ENDPOINT[@]}" --set endpointCheck.enabled=yes
refuse endpoint-check-1 "$(bad_endpoint_check 1)" --set rbac.scope=cluster \
  --set endpointCheck.enabled=1
refuse endpoint-check-capital "$(bad_endpoint_check True)" --set rbac.scope=cluster \
  --set-string endpointCheck.enabled=True
refuse endpoint-check-scalar "$(not_an_endpoint_check_map false)" --set rbac.scope=cluster \
  "${ENDPOINT[@]}" --set endpointCheck=false
refuse endpoint-check-scalar-true "$(not_an_endpoint_check_map true)" --set rbac.scope=cluster \
  --set endpointCheck=true
refuse endpoint-check-scalar-empty "$(not_an_endpoint_check_map '')" --set rbac.scope=cluster \
  --set endpointCheck=
refuse endpoint-check-list "$(not_an_endpoint_check_map '[]')" --set rbac.scope=cluster \
  --set-json 'endpointCheck=[]'
cat >"$WORK/endpoint-check-scalar.yaml" <<'VALUES'
endpointCheck: false
VALUES
refuse endpoint-check-scalar-file "$(not_an_endpoint_check_map false)" --set rbac.scope=cluster \
  "${ENDPOINT[@]}" -f "$WORK/endpoint-check-scalar.yaml"
refuse timeout-scope-first "$NOT_SET" --set model.timeoutSeconds=9
refuse endpoint-check-scope-first "$NOT_SET" --set endpointCheck.enabled=yes
refuse endpoint-check-scalar-scope-first "$NOT_SET" --set endpointCheck=false

# --- The upgrade test and the soak (v0.6, measured reliability) --------------------------------

# Theme 3 of v0.6 changes no template: it adds the upgrade test from the pinned latest release
# (tests/e2e/upgrade-from.txt, tests/e2e/upgrade.sh, the helpers it shares with run.sh in
# tests/e2e/lib.sh, and the upgrade jobs of .github/workflows/e2e.yml) and the soak scripts
# (tests/soak/, docs/how-to/run-the-soak.md). Checked here without a cluster: the release's values
# upgraded to this chart as --reuse-values does, the soak's values on the release's chart and on
# this one, the soak endpoint's manifest, the workflow's jobs and its warning step (against a stub
# gh), the scripts' syntax, and the soak scripts against stub kubectl, helm, curl and sleep that log
# their arguments: each refuses a missing, empty or option-like context before any call, names the
# context first in every call, and writes what the spec says; summarize.py's verdicts on
# synthetic samples; and the pages that document both. tests/upgrade_and_soak.rs checks the
# scripts' text under cargo test.
if ! command -v python3 >/dev/null 2>&1; then
  fail "python3 is needed for the upgrade and soak checks"
fi
RELIABILITY="$WORK/reliability"
mkdir -p "$RELIABILITY"

# The pins, read as upgrade.sh reads them.
UPGRADE_FROM="$ROOT/tests/e2e/upgrade-from.txt"
upgrade_pin() { sed -n "s/^$1=//p" "$UPGRADE_FROM" 2>/dev/null || true; }
FROM_VERSION=$(upgrade_pin KAUSA_UPGRADE_FROM_VERSION)
FROM_DIGEST=$(upgrade_pin KAUSA_UPGRADE_FROM_IMAGE_DIGEST)
since=$FAILURES
[ -f "$UPGRADE_FROM" ] || fail "tests/e2e/upgrade-from.txt is missing"
printf '%s\n' "$FROM_VERSION" | grep -Eqx '[0-9]+\.[0-9]+\.[0-9]+' ||
  fail "upgrade-from.txt: no version X.Y.Z"
printf '%s\n' "$FROM_DIGEST" | grep -Eqx 'sha256:[0-9a-f]{64}' ||
  fail "upgrade-from.txt: no image digest"
passed "$since" "upgrade-from.txt: the pinned release v$FROM_VERSION and its image digest"
# The git ref of the pinned release, whose charts/kausa its chart was packaged from.
FROM_REF=${KAUSA_CHART_UPGRADE_FROM:-v$FROM_VERSION}

# yaml_docs FILE OUT: every YAML document of FILE as one line of JSON in OUT, read with helm
# (fromYaml in a throwaway chart), so that the layout of the file is free.
YAML_DOCS="$RELIABILITY/yaml-docs"
mkdir -p "$YAML_DOCS/templates"
printf '%s\n' 'apiVersion: v2' 'name: yaml-docs' 'version: 0.0.0' >"$YAML_DOCS/Chart.yaml"
cat >"$YAML_DOCS/templates/docs.yaml" <<'TEMPLATE'
{{- range $doc := regexSplit "(?m)^---[ \t]*$" (.Files.Get "input.yaml") -1 }}
{{- $o := fromYaml $doc }}
{{- if $o }}
# doc {{ $o | toJson }}
{{- end }}
{{- end }}
TEMPLATE
yaml_docs() {
  cp "$1" "$YAML_DOCS/input.yaml"
  if "$HELM" template yaml-docs "$YAML_DOCS" >"$2.raw" 2>"$2.err"; then
    sed -n 's/^# doc //p' "$2.raw" >"$2"
  else
    return 1
  fi
}

# json_check NAME SCRIPT ARGS...: runs the Python SCRIPT (stdlib only) with ARGS; each line it
# prints is a failed check of NAME.
json_check() {
  local name=$1 script=$2 line
  shift 2
  if ! python3 -I -c "$script" "$@" >"$RELIABILITY/json-check" 2>&1; then
    fail "$name: the check itself failed: $(head -n 5 "$RELIABILITY/json-check")"
    return
  fi
  while IFS= read -r line; do
    fail "$name: $line"
  done <"$RELIABILITY/json-check"
}

# --- The upgrade, rendered

# With --reuse-values, Helm v4.3.1 renders the new chart's templates with the old release's values
# in place of the new chart's defaults (pkg/action/upgrade.go, chart.Values = oldVals) and the new
# --set values on top. So this chart, with the release's values.yaml in place of its own, rendered
# with upgrade.sh's install values and its four image values, is what the upgrade applies. A
# template that reads a value the release lacks, without a default, fails here as the upgrade
# would, while a fresh install of this chart still renders.
UPGRADE_INSTALL=(--set image.digest="$FROM_DIGEST" --set replicaCount=2
  --set model.endpoint=http://192.0.2.10:8080/v1 --set model.name=fake --set model.allowHttp=true
  --set settleSeconds=10 --set history.enabled=true --set budget.callsPerHour=3600
  --set notifications.enabled=true --set notifications.format=json
  --set notifications.existingSecret=kausa-notify --set notifications.tokenKey=token
  --set notifications.allowHttp=true)
UPGRADE_IMAGE=(--set image.repository=kausa --set image.tag=e2e --set-string image.digest=
  --set image.pullPolicy=Never)
FROM_CHART=""
if [ -n "$FROM_VERSION" ] &&
  git -C "$ROOT" rev-parse --verify --quiet "$FROM_REF^{commit}" >/dev/null 2>&1; then
  mkdir -p "$RELIABILITY/from"
  git -C "$ROOT" archive "$FROM_REF" charts/kausa | tar -x -C "$RELIABILITY/from"
  FROM_CHART="$RELIABILITY/from/charts/kausa"
  REUSED="$RELIABILITY/reused/kausa"
  mkdir -p "$RELIABILITY/reused"
  cp -R "$CHART" "$REUSED"
  cp "$FROM_CHART/values.yaml" "$REUSED/values.yaml"
  for scope in cluster namespaced; do
    since=$FAILURES
    case "$scope" in
      cluster) scope_args=(--set rbac.scope=cluster) ;;
      *) scope_args=(--set rbac.scope=namespaced --set 'rbac.namespaces={e2e-shop}') ;;
    esac
    out="$RELIABILITY/upgrade-$scope"
    if ! render "$out.before" "$RELEASE" "$FROM_CHART" "${UPGRADE_INSTALL[@]}" "${scope_args[@]}"
    then
      fail "upgrade/$scope: $FROM_REF's chart does not render upgrade.sh's install:" \
        "$(cat "$out.before.err")"
    elif ! render "$out" "$RELEASE" "$REUSED" "${UPGRADE_INSTALL[@]}" "${scope_args[@]}" \
      "${UPGRADE_IMAGE[@]}"; then
      fail "upgrade/$scope: this chart does not render from $FROM_REF's values" \
        "(helm upgrade --reuse-values): $(cat "$out.err")"
    else
      docs "$out.before" Deployment | grep -Eqx " +image: \"?ghcr\.io/fedeam/kausa@$FROM_DIGEST\"?" ||
        fail "upgrade/$scope: the release is not installed by the pinned digest"
      docs "$out" Deployment >"$out.deployment"
      grep -Eqx ' +image: "?kausa:e2e"?' "$out.deployment" ||
        fail "upgrade/$scope: the upgraded Deployment does not run kausa:e2e"
      grep -Eqx ' +imagePullPolicy: "?Never"?' "$out.deployment" ||
        fail "upgrade/$scope: the upgraded Deployment does not pull Never"
      grep -qx '  replicas: 2' "$out.deployment" ||
        fail "upgrade/$scope: the upgraded Deployment does not keep two replicas"
      args "$out" | awk -v want="$NAMESPACE/$RELEASE-leader" '
        prev == "- --leader-lease" && ($0 == "- " want || $0 == "- \"" want "\"") { ok = 1 }
        { prev = $0 } END { exit !ok }' ||
        fail "upgrade/$scope: the upgraded Pods do not run with --leader-lease $NAMESPACE/$RELEASE-leader"
      [ "$(count_docs "$out" Lease "$NAMESPACE" "$RELEASE-leader")" = 1 ] ||
        fail "upgrade/$scope: the upgrade drops the Lease"
    fi
    passed "$since" "upgrade/$scope: this chart renders from $FROM_REF's values with the four image values"
  done
else
  fail "upgrade: $FROM_REF is not in this clone (git fetch --tags), or no version is pinned"
fi

# --- The soak's values

# tests/soak/values.yaml with the pinned digest, as install.sh installs it, on the release's chart
# and on this one (a later soak of this chart reuses the file): cluster scope, two replicas, no
# Events, no history, no dry-run, the in-cluster endpoint and the notification Secret; nothing
# namespaced outside kausa-soak and nothing cluster-scoped but kausa-soak's ClusterRole and binding.
SOAK_VALUES="$ROOT/tests/soak/values.yaml"
check_soak() {
  local name=$1 chart=$2 out="$RELIABILITY/soak-$1"
  since=$FAILURES
  if ! "$HELM" template kausa-soak "$chart" --kube-version "$KUBE_VERSION" --namespace kausa-soak \
    -f "$SOAK_VALUES" --set image.digest="$FROM_DIGEST" >"$out" 2>"$out.err"; then
    fail "soak/$name: does not render: $(cat "$out.err")"
    return
  fi
  [ "$(cluster_scoped "$out")" = \
    "$(expected_cluster_scoped ClusterRole/kausa-soak ClusterRoleBinding/kausa-soak)" ] ||
    fail "soak/$name: renders cluster-scoped objects other than kausa-soak's ClusterRole and" \
      "ClusterRoleBinding: $(cluster_scoped "$out" | tr '\n' ' ')"
  for kind in Role RoleBinding; do
    [ "$(count_docs "$out" "$kind")" = "$(count_docs "$out" "$kind" kausa-soak)" ] ||
      fail "soak/$name: renders a $kind outside kausa-soak"
    [ "$(count_docs "$out" "$kind" kausa-soak kausa-soak-leader)" = 1 ] ||
      fail "soak/$name: no $kind kausa-soak-leader"
  done
  [ "$(count_docs "$out" Lease kausa-soak kausa-soak-leader)" = 1 ] ||
    fail "soak/$name: no Lease kausa-soak-leader in kausa-soak"
  if grep -q CustomResourceDefinition "$out"; then
    fail "soak/$name: renders a CustomResourceDefinition"
  fi
  args "$out" >"$out.args"
  [ "$(grep -cx -- '- --no-events' "$out.args" || true)" = 1 ] ||
    fail "soak/$name: not exactly one --no-events arg"
  for arg in --history --dry-run; do
    if grep -qx -- "- $arg" "$out.args"; then fail "soak/$name: passes $arg"; fi
  done
  awk 'prev == "- --leader-lease" &&
         ($0 == "- kausa-soak/kausa-soak-leader" || $0 == "- \"kausa-soak/kausa-soak-leader\"") {
         ok = 1 }
       { prev = $0 } END { exit !ok }' "$out.args" ||
    fail "soak/$name: no --leader-lease kausa-soak/kausa-soak-leader"
  awk 'prev == "- --endpoint" && index($0, "http://kausa-soak-endpoint.kausa-soak.svc:8080/v1") {
         ok = 1 }
       { prev = $0 } END { exit !ok }' "$out.args" ||
    fail "soak/$name: the endpoint is not the in-cluster fake endpoint"
  grep -qx -- '- --notify-url-file' "$out.args" || fail "soak/$name: notifications are off"
  docs "$out" Deployment >"$out.deployment"
  grep -Eqx " +image: \"?ghcr\.io/fedeam/kausa@$FROM_DIGEST\"?" "$out.deployment" ||
    fail "soak/$name: kausa's image is not the pinned digest"
  grep -qx '  replicas: 2' "$out.deployment" || fail "soak/$name: not two replicas"
  grep -Eqx ' +secretName: "?kausa-soak-notify"?' "$out.deployment" ||
    fail "soak/$name: does not mount the Secret kausa-soak-notify"
  # The labels sample.py selects kausa's Pods by.
  for label in 'app.kubernetes.io/name: kausa' 'app.kubernetes.io/instance: kausa-soak'; do
    grep -qx "        $label" "$out.deployment" ||
      fail "soak/$name: the Pods do not carry $label"
  done
  if "$HELM" lint --strict "$chart" -f "$SOAK_VALUES" --set image.digest="$FROM_DIGEST" \
    >"$out.lint" 2>&1; then :; else
    fail "soak/$name: helm lint --strict fails: $(cat "$out.lint")"
  fi
  passed "$since" "soak/$name: two replicas, no Events, history or dry-run, nothing outside kausa-soak"
}
if [ ! -f "$SOAK_VALUES" ]; then
  fail "tests/soak/values.yaml is missing"
else
  check_soak this-chart "$CHART"
  if [ -n "$FROM_CHART" ]; then check_soak "$FROM_REF" "$FROM_CHART"; fi
fi

# --- The soak's endpoint

# tests/soak/endpoint.yaml: a Deployment and a Service kausa-soak-endpoint in kausa-soak, under the
# restricted Pod Security Standard, running tests/e2e/fake_endpoint.py from the ConfigMap with its
# logs to /dev/null and its fail file in /tmp; labels that kausa's selector never matches.
# shellcheck disable=SC2016 # Python, not shell
ENDPOINT_CHECK='
import json, re, sys
docs = [json.loads(l) for l in open(sys.argv[1])]
problems = []
def say(m): problems.append(m)
names = sorted(tuple(str((d.get("metadata") or {}).get(k) or "") if k else str(d.get("kind")) for k in (None, "namespace", "name")) for d in docs)
want = [("Deployment", "kausa-soak", "kausa-soak-endpoint"), ("Service", "kausa-soak", "kausa-soak-endpoint")]
if names != want:
    say("the documents are %s, not a Deployment and a Service kausa-soak-endpoint in kausa-soak" % names)
mine = {"app.kubernetes.io/name": "kausa-soak-endpoint"}
for d in docs:
    if d.get("kind") == "Deployment":
        spec = d.get("spec") or {}
        pod = (spec.get("template") or {}).get("spec") or {}
        labels = ((spec.get("template") or {}).get("metadata") or {}).get("labels") or {}
        selector = (spec.get("selector") or {}).get("matchLabels") or {}
        for what, got in (("labels", labels), ("selector", selector)):
            if got.get("app.kubernetes.io/name") != "kausa-soak-endpoint" or "app.kubernetes.io/instance" in got:
                say("the Pod %s are not app.kubernetes.io/name=kausa-soak-endpoint alone" % what)
        if spec.get("replicas", 1) != 1:
            say("not one replica")
        if pod.get("automountServiceAccountToken") is not False:
            say("automountServiceAccountToken is not false")
        containers = pod.get("containers") or []
        if len(containers) != 1:
            say("not one container")
            continue
        c = containers[0]
        if c.get("name") != "endpoint":
            say("the container is not named endpoint")
        if not re.fullmatch(r"python:3\.13-slim-trixie@sha256:[0-9a-f]{64}", str(c.get("image"))):
            say("the image is not python:3.13-slim-trixie by digest")
        command = ["python3", "-I", "-B", "/app/fake_endpoint.py", "--port", "8080", "--log", "/dev/null",
                   "--hook-log", "/dev/null", "--fail-file", "/tmp/fail"]
        if (c.get("command") or []) + (c.get("args") or []) != command:
            say("the command is not the spec: %s" % json.dumps(command))
        sc = dict(pod.get("securityContext") or {})
        sc.update(c.get("securityContext") or {})
        for key, value in (("runAsNonRoot", True), ("runAsUser", 65534), ("runAsGroup", 65534),
                           ("allowPrivilegeEscalation", False), ("readOnlyRootFilesystem", True),
                           ("capabilities", {"drop": ["ALL"]}), ("seccompProfile", {"type": "RuntimeDefault"})):
            if sc.get(key) != value:
                say("securityContext.%s is not %s" % (key, json.dumps(value)))
        if c.get("resources") != {"requests": {"cpu": "10m", "memory": "32Mi"}, "limits": {"cpu": "100m", "memory": "64Mi"}}:
            say("resources are not requests 10m/32Mi and limits 100m/64Mi")
        if str(((c.get("readinessProbe") or {}).get("tcpSocket") or {}).get("port")) != "8080":
            say("no tcpSocket readiness probe on 8080")
        volumes = {v.get("name"): v for v in pod.get("volumes") or []}
        mounts = {}
        for m in c.get("volumeMounts") or []:
            v = volumes.get(m.get("name")) or {}
            if "configMap" in v:
                mounts[m.get("mountPath")] = "configMap:" + str(v["configMap"].get("name"))
            elif "emptyDir" in v:
                mounts[m.get("mountPath")] = "emptyDir"
            else:
                mounts[m.get("mountPath")] = "other"
        if mounts != {"/app": "configMap:kausa-soak-endpoint", "/tmp": "emptyDir"}:
            say("the mounts are %s, not the ConfigMap kausa-soak-endpoint at /app and an emptyDir at /tmp" % json.dumps(mounts, sort_keys=True))
    if d.get("kind") == "Service":
        spec = d.get("spec") or {}
        if spec.get("type", "ClusterIP") != "ClusterIP":
            say("the Service is not ClusterIP")
        if spec.get("selector") != mine:
            say("the Service selector is not app.kubernetes.io/name=kausa-soak-endpoint")
        if [p.get("port") for p in spec.get("ports") or []] != [8080]:
            say("the Service does not serve port 8080 alone")
for p in problems:
    print(p)
'
since=$FAILURES
if [ ! -f "$ROOT/tests/soak/endpoint.yaml" ]; then
  fail "tests/soak/endpoint.yaml is missing"
elif ! yaml_docs "$ROOT/tests/soak/endpoint.yaml" "$RELIABILITY/endpoint.json"; then
  fail "soak/endpoint.yaml cannot be read: $(cat "$RELIABILITY/endpoint.json.err")"
else
  json_check soak/endpoint.yaml "$ENDPOINT_CHECK" "$RELIABILITY/endpoint.json"
fi
passed "$since" "soak/endpoint.yaml: the fake endpoint, restricted, pinned, never selected as kausa"

# --- The workflow

# .github/workflows/e2e.yml: the five tool pins once, at the workflow level; the kind job as before;
# the upgrade job of the spec; read-only permissions, one checkout action, GH_TOKEN on the warning
# step alone.
WORKFLOW="$ROOT/.github/workflows/e2e.yml"
# block FILE START: the lines after the first line equal to START that are indented deeper than it.
block() {
  awk -v start="$2" '
    !on && $0 == start { on = 1; match($0, /^ */); ind = RLENGTH; next }
    on { if ($0 !~ /^ *$/) { match($0, /^ */); if (RLENGTH <= ind) exit } print }' "$1"
}
# job FILE NAME: the lines of job NAME.
job() { block "$1" "  $2:"; }
# step_run JOB_LINES STEP: the run block of step STEP in the job lines on stdin, unindented.
step_run() {
  awk -v step="      - name: $1" '
    $0 == step { in_step = 1; next }
    in_step && /^      - / { exit }
    in_step && $0 == "        run: |" { on = 1; next }
    on { if ($0 !~ /^ *$/ && $0 !~ /^          /) exit; sub(/^          /, ""); print }'
}
since=$FAILURES
if [ ! -f "$WORKFLOW" ]; then
  fail "e2e.yml is missing"
else
  header=$(awk '/^#/ { on = 1; sub(/^# ?/, ""); printf "%s ", $0; next } on { exit }' "$WORKFLOW")
  for words in '"upgrade (cluster)"' '"upgrade (namespaced)"' tests/e2e/upgrade.sh; do
    case "$header" in *"$words"*) ;; *) fail "e2e.yml: the header comment does not name $words" ;; esac
  done
  [ "$(block "$WORKFLOW" 'permissions:' | grep -v '^ *#')" = '  contents: read' ] ||
    fail "e2e.yml: permissions are not contents: read alone"
  top=$(grep -nE '^(permissions|env|concurrency|jobs):' "$WORKFLOW" | cut -d: -f2 | tr '\n' ' ')
  [ "$top" = "permissions env concurrency jobs " ] ||
    fail "e2e.yml: no workflow-level env right after permissions (top-level keys: $top)"
  for pin in KIND_VERSION KIND_SHA256 HELM_VERSION HELM_SHA256 KIND_NODE_IMAGE; do
    block "$WORKFLOW" 'env:' | grep -Eq "^  $pin: [^ ]+$" ||
      fail "e2e.yml: $pin is not in the workflow-level env"
    [ "$(grep -cE "^ *$pin:" "$WORKFLOW")" = 1 ] || fail "e2e.yml: $pin is set more than once"
  done
  jobs=$(block "$WORKFLOW" 'jobs:' | grep -E '^  [a-z][a-z0-9_-]*:$' | tr -d ' :' | tr '\n' ' ')
  [ "$jobs" = "kind upgrade " ] || fail "e2e.yml: the jobs are $jobs, not kind and upgrade"
  job "$WORKFLOW" kind >"$RELIABILITY/job-kind"
  job "$WORKFLOW" upgrade >"$RELIABILITY/job-upgrade"
  # shellcheck disable=SC2016 # GitHub expressions, not shell
  for line in "    name: kind (\${{ matrix.scope }}\${{ matrix.replicas == 2 && ', standby' || '' }})" \
    '    timeout-minutes: 30' '        scope: [cluster, namespaced]' '        replicas: [1]' \
    '      KAUSA_E2E_SCOPE: ${{ matrix.scope }}' '      KAUSA_E2E_REPLICAS: ${{ matrix.replicas || 1 }}' \
    '        run: tests/e2e/run.sh'; do
    grep -qxF -- "$line" "$RELIABILITY/job-kind" || fail "e2e.yml: the kind job lost '$line'"
  done
  # shellcheck disable=SC2016 # the same
  for line in '    name: upgrade (${{ matrix.scope }})' '    runs-on: ubuntu-24.04' \
    '    timeout-minutes: 20' '      fail-fast: false' '        scope: [cluster, namespaced]' \
    '      KAUSA_E2E_SCOPE: ${{ matrix.scope }}' '        run: tests/e2e/upgrade.sh' \
    '          persist-credentials: false'; do
    grep -qxF -- "$line" "$RELIABILITY/job-upgrade" || fail "e2e.yml: the upgrade job has no '$line'"
  done
  grep -q 'KAUSA_E2E_REPLICAS' "$RELIABILITY/job-upgrade" &&
    fail "e2e.yml: the upgrade job sets KAUSA_E2E_REPLICAS"
  steps=$(grep -E '^      - name: ' "$RELIABILITY/job-upgrade" | sed 's/^      - name: //' | tr '\n' '|')
  [ "$steps" = "Check out|Install kind and helm|Warn when a newer release exists|Run the upgrade test|" ] ||
    fail "e2e.yml: the upgrade job's steps are $steps"
  step_run 'Install kind and helm' <"$RELIABILITY/job-kind" >"$RELIABILITY/install-kind"
  step_run 'Install kind and helm' <"$RELIABILITY/job-upgrade" >"$RELIABILITY/install-upgrade"
  if [ ! -s "$RELIABILITY/install-kind" ] ||
    ! cmp -s "$RELIABILITY/install-kind" "$RELIABILITY/install-upgrade"; then
    fail "e2e.yml: the two 'Install kind and helm' run blocks differ"
  fi
  uses=$(grep -E '^ *(- )?uses: ' "$WORKFLOW" | sed -E 's/^ *(- )?//' | sort | uniq -c | awk '{ $1 = $1 } 1')
  case "$uses" in
    *"
"*) fail "e2e.yml: more than one action: $(printf '%s' "$uses" | tr '\n' ';')" ;;
    "2 uses: actions/checkout@"*) ;;
    *) fail "e2e.yml: not the one checkout action twice: $uses" ;;
  esac
  awk '/^      - name: Warn when a newer release exists$/ { on = 1; next } on && /^      - / { exit }
    on' "$RELIABILITY/job-upgrade" >"$RELIABILITY/step-warn"
  # shellcheck disable=SC2016 # a GitHub expression
  if [ "$(grep -c 'GH_TOKEN' "$WORKFLOW")" != 1 ] ||
    ! grep -qxF '          GH_TOKEN: ${{ github.token }}' "$RELIABILITY/step-warn"; then
    fail "e2e.yml: GH_TOKEN is not set once, on the warning step"
  fi
  grep -q 'secrets\.' "$WORKFLOW" && fail "e2e.yml: reads a secret"
fi
passed "$since" "e2e.yml: the pins at the workflow level, the kind job, the upgrade job in both scopes"

# The warning step, run as GitHub runs it (bash -e, pipefail), against a stub gh: silent while the
# pin is the latest release, one warning when a newer one exists, one when gh fails; never a
# failure.
STUB="$RELIABILITY/stub"
mkdir -p "$STUB/bin"
cat >"$STUB/bin/gh" <<'STUB'
#!/usr/bin/env bash
{ printf 'gh'; printf ' %s' "$@"; printf '\n'; } >>"$STUB_LOG"
[ -n "${STUB_GH_TAG:-}" ] || { echo 'stub gh: no release' >&2; exit 1; }
printf '%s\n' "$STUB_GH_TAG"
STUB
chmod 755 "$STUB/bin/gh"
since=$FAILURES
if [ -f "$WORKFLOW" ]; then
  step_run 'Warn when a newer release exists' <"$RELIABILITY/job-upgrade" >"$RELIABILITY/warn.sh"
  mkdir -p "$RELIABILITY/warn/tests/e2e"
  cp "$UPGRADE_FROM" "$RELIABILITY/warn/tests/e2e/" 2>/dev/null || true
  for latest in "v$FROM_VERSION" v99.0.0 ''; do
    STUB_LOG="$RELIABILITY/warn.log"
    : >"$STUB_LOG"
    if (cd "$RELIABILITY/warn" && STUB_LOG="$STUB_LOG" STUB_GH_TAG="$latest" \
      GITHUB_REPOSITORY=fedeam/kausa PATH="$STUB/bin:$PATH" \
      bash --noprofile --norc -eo pipefail "$RELIABILITY/warn.sh") >"$RELIABILITY/warn.out" \
      2>"$RELIABILITY/warn.err"
    then :; else fail "e2e.yml: the warning step fails when the latest release is '${latest:-unreadable}'"; fi
    case "$latest" in
      "v$FROM_VERSION") want='' ;;
      v99.0.0) want="::warning::tests/e2e/upgrade-from.txt names v$FROM_VERSION; the latest release is v99.0.0" ;;
      *) want='::warning::could not read the latest release' ;;
    esac
    [ "$(cat "$RELIABILITY/warn.out")" = "$want" ] ||
      fail "e2e.yml: the warning step prints '$(head -c 200 "$RELIABILITY/warn.out")' for latest '${latest:-unreadable}'"
    grep -qxF 'gh release view --repo fedeam/kausa --json tagName --jq .tagName' "$STUB_LOG" ||
      fail "e2e.yml: the warning step does not ask gh for the latest release of the repository"
  done
fi
passed "$since" "e2e.yml: the warning step warns about a newer release and never fails"

# --- The scripts

since=$FAILURES
SCRIPTS=(tests/e2e/lib.sh tests/e2e/run.sh tests/e2e/upgrade.sh tests/soak/install.sh
  tests/soak/exercise.sh tests/soak/uninstall.sh)
for script in "${SCRIPTS[@]}"; do
  if [ ! -f "$ROOT/$script" ]; then
    fail "$script is missing"
  elif ! bash -n "$ROOT/$script" 2>"$RELIABILITY/bash-n"; then
    fail "$script: bash -n fails: $(cat "$RELIABILITY/bash-n")"
  fi
done
if command -v shellcheck >/dev/null 2>&1; then
  (cd "$ROOT" && shellcheck -x "${SCRIPTS[@]}") >"$RELIABILITY/shellcheck" 2>&1 ||
    fail "shellcheck -x: $(head -n 20 "$RELIABILITY/shellcheck")"
else
  echo "skipped: shellcheck is not installed; bash -n only"
fi
for script in tests/soak/sample.py tests/soak/summarize.py; do
  if [ ! -f "$ROOT/$script" ]; then
    fail "$script is missing"
  elif ! python3 -I -c 'import sys; compile(open(sys.argv[1]).read(), sys.argv[1], "exec")' \
    "$ROOT/$script" 2>"$RELIABILITY/compile"; then
    fail "$script does not compile: $(cat "$RELIABILITY/compile")"
  fi
done
passed "$since" "the e2e and soak scripts: bash -n, shellcheck -x, and the Python compiles"

# --- The soak scripts against stubs

# A kubectl that logs its arguments and answers the reads of the soak with a fake cluster: Pods
# kausa-soak-stub-1 (the Lease holder) and kausa-soak-stub-2 on nodes stub-node-b and stub-node-a of
# three, no metrics-server, contexts other-ctx, stub-ctx and stub-ctx-2. It keeps every manifest it
# is given with -f in $STUB_LOG.manifests and the notification URL file in $STUB_LOG.url.
cat >"$STUB/bin/kubectl" <<'STUB'
#!/usr/bin/env bash
{ printf 'kubectl'; printf ' %s' "$@"; printf '\n'; } >>"$STUB_LOG"
prev="" raw="" out=""
for a in "$@"; do
  case "$prev" in
    -f | --filename)
      if [ "$a" = - ]; then cat >>"$STUB_LOG.manifests"; elif [ -f "$a" ]; then cat "$a" >>"$STUB_LOG.manifests"; fi
      printf '\n---\n' >>"$STUB_LOG.manifests" ;;
    --raw) raw=$a ;;
    -o | --output) out=$a ;;
  esac
  case "$a" in
    --from-file=url=*) cat "${a#--from-file=url=}" >"$STUB_LOG.url" 2>/dev/null ;;
    -o=* | --output=*) out=${a#*=} ;;
    -ojson) out=json ;;
    -oname) out=name ;;
  esac
  prev=$a
done
args=" $* "
has() { case "$args" in *" $1 "*) return 0 ;; esac; return 1; }
node_pod() { case "$1" in stub-node-a) echo kausa-soak-stub-2 ;; stub-node-b) echo kausa-soak-stub-1 ;; esac; }
pod_json() { # NAME NODE
  printf '{"metadata":{"name":"%s","namespace":"kausa-soak","labels":{"app.kubernetes.io/name":"kausa","app.kubernetes.io/instance":"kausa-soak"}},"spec":{"nodeName":"%s","containers":[{"name":"kausa"}]},"status":{"phase":"Running","conditions":[{"type":"Ready","status":"True"}],"containerStatuses":[{"name":"kausa","ready":true,"restartCount":0,"lastState":{}}]}}' "$1" "$2"
}
if has config; then
  if has get-contexts; then printf '%s\n' other-ctx stub-ctx stub-ctx-2; fi
  exit 0
fi
if [ -n "$raw" ]; then
  node=${raw#/api/v1/nodes/}
  node=${node%%/*}
  case "$raw" in
    /api/v1/nodes/*/proxy/stats/summary)
      if [ -n "${STUB_FAIL_SUMMARY:-}" ]; then echo 'Error from server (ServiceUnavailable)' >&2; exit 1; fi
      pod=$(node_pod "$node")
      case "$pod" in
        kausa-soak-stub-1) ws=24117248 ;;
        kausa-soak-stub-2) ws=23068672 ;;
        *) printf '{"node":{"nodeName":"%s"},"pods":[]}\n' "$node"; exit 0 ;;
      esac
      printf '{"node":{"nodeName":"%s"},"pods":[{"podRef":{"name":"%s","namespace":"kausa-soak"},"containers":[{"name":"kausa","memory":{"workingSetBytes":%s,"rssBytes":%s,"usageBytes":%s},"cpu":{"usageCoreNanoSeconds":123456789}}]}]}\n' \
        "$node" "$pod" "$ws" $((ws - 1048576)) $((ws + 2097152))
      exit 0 ;;
    /api/v1/nodes/*/proxy/metrics/cadvisor)
      pod=$(node_pod "$node")
      [ -n "$pod" ] || exit 0
      printf '# TYPE container_memory_max_usage_bytes gauge\n'
      printf 'container_memory_max_usage_bytes{container="kausa",id="/kubepods/x",image="ghcr.io/fedeam/kausa",name="x",namespace="kausa-soak",pod="%s"} 3.145728e+07 1767225600000\n' "$pod"
      printf 'container_memory_max_usage_bytes{container="other",id="/kubepods/y",image="z",name="y",namespace="kausa-soak",pod="%s"} 999 1767225600000\n' "$pod"
      printf 'container_oom_events_total{container="kausa",id="/kubepods/x",image="ghcr.io/fedeam/kausa",name="x",namespace="kausa-soak",pod="%s"} 0 1767225600000\n' "$pod"
      exit 0 ;;
    /api/v1/namespaces/kausa-soak/pods/kausa-soak-stub-1:9090/proxy/metrics)
      printf '%s\n' '# HELP kausa_leader 1 while this replica leads.' '# TYPE kausa_leader gauge' 'kausa_leader 1' \
        'kausa_endpoint_state{state="closed"} 0' 'kausa_endpoint_state{state="half_open"} 0' \
        'kausa_endpoint_state{state="open"} 1' 'kausa_model_calls_total{outcome="ok"} 3' \
        'kausa_incidents_total{detection="D001"} 4' 'process_cpu_seconds_total 1'
      exit 0 ;;
    /api/v1/namespaces/kausa-soak/pods/kausa-soak-stub-2:9090/proxy/metrics)
      printf '%s\n' 'kausa_leader 0' 'kausa_model_calls_total{outcome="ok"} 0'
      exit 0 ;;
    /apis/metrics.k8s.io/*)
      echo 'Error from server (NotFound): the server could not find the requested resource' >&2
      exit 1 ;;
    *) echo "stub kubectl: no answer for that path" >&2; exit 1 ;;
  esac
fi
if has get; then
  case "$args" in
    *" namespace "* | *" namespaces "* | *" ns "* | *" namespace/kausa-soak "* | *" ns/kausa-soak "*)
      if [ -n "${STUB_NS_EXISTS:-}" ]; then
        if [ "$out" = json ]; then echo '{"metadata":{"name":"kausa-soak"}}'; else echo namespace/kausa-soak; fi
        exit 0
      fi
      has --ignore-not-found && exit 0
      echo 'Error from server (NotFound): namespaces "kausa-soak" not found' >&2
      exit 1 ;;
  esac
  if has -A || has --all-namespaces; then
    if has events || has event || has events.events.k8s.io; then n=2; else n=3; fi
    if [ "$out" = json ]; then
      printf '{"items":['; for i in $(seq 1 "$n"); do [ "$i" = 1 ] || printf ','; printf '{"metadata":{"name":"o%s"}}' "$i"; done; printf ']}\n'
    else
      for i in $(seq 1 "$n"); do echo "object/o$i"; done
    fi
    exit 0
  fi
  if has pods || has pod || has po; then
    case "$out" in
      json) printf '{"items":[%s,%s]}\n' "$(pod_json kausa-soak-stub-1 stub-node-b)" "$(pod_json kausa-soak-stub-2 stub-node-a)" ;;
      name) printf '%s\n' pod/kausa-soak-stub-1 pod/kausa-soak-stub-2 ;;
    esac
    exit 0
  fi
  if has nodes || has node || has no; then
    case "$out" in
      json) echo '{"items":[{"metadata":{"name":"stub-node-c"}},{"metadata":{"name":"stub-node-a"}},{"metadata":{"name":"stub-node-b"}}]}' ;;
      name) printf '%s\n' node/stub-node-c node/stub-node-a node/stub-node-b ;;
      jsonpath*) echo 'stub-node-c stub-node-a stub-node-b' ;;
    esac
    exit 0
  fi
  case "$args" in
    *" lease "* | *" leases "* | *" lease/kausa-soak-leader "* | *" leases.coordination.k8s.io "*)
      case "$out" in
        json) echo '{"metadata":{"name":"kausa-soak-leader"},"spec":{"holderIdentity":"kausa-soak-stub-1","leaseTransitions":1}}' ;;
        jsonpath*) printf 'kausa-soak-stub-1' ;;
        name) echo lease.coordination.k8s.io/kausa-soak-leader ;;
      esac
      exit 0 ;;
  esac
fi
exit 0
STUB
cat >"$STUB/bin/helm" <<'STUB'
#!/usr/bin/env bash
{ printf 'helm'; printf ' %s' "$@"; printf '\n'; } >>"$STUB_LOG"
STUB
cat >"$STUB/bin/curl" <<'STUB'
#!/usr/bin/env bash
# Writes a fake chart wherever -o points.
{ printf 'curl'; printf ' %s' "$@"; printf '\n'; } >>"$STUB_LOG"
prev=""
for a in "$@"; do
  if [ "$prev" = -o ] || [ "$prev" = --output ]; then printf 'stub chart\n' >"$a"; fi
  prev=$a
done
STUB
cat >"$STUB/bin/sleep" <<'STUB'
#!/usr/bin/env bash
# Logs, sleeps a moment, and with STUB_TERM_ON_SLEEP sends TERM to its caller first.
{ printf 'sleep'; printf ' %s' "$@"; printf '\n'; } >>"$STUB_LOG"
if [ -n "${STUB_TERM_ON_SLEEP:-}" ]; then kill -TERM "$PPID"; fi
command -p sleep 0.05
STUB
chmod 755 "$STUB/bin/kubectl" "$STUB/bin/helm" "$STUB/bin/curl" "$STUB/bin/sleep"

# stub_run NAME SECONDS DIR COMMAND...: runs COMMAND in DIR with the stubs first in PATH, its log in
# $STUB_LOG, its output in $STUB_LOG.out and .err; stopped with TERM after SECONDS. Sets status.
stub_run() {
  local name=$1 seconds=$2 dir=$3 pid watchdog
  shift 3
  STUB_LOG="$RELIABILITY/stub-$name.log"
  rm -f "$STUB_LOG" "$STUB_LOG.manifests" "$STUB_LOG.url"
  : >"$STUB_LOG"
  (cd "$dir" && STUB_LOG="$STUB_LOG" PATH="$STUB/bin:$PATH" exec "$@") \
    >"$STUB_LOG.out" 2>"$STUB_LOG.err" </dev/null &
  pid=$!
  (command sleep "$seconds" && kill -TERM "$pid") >/dev/null 2>&1 &
  watchdog=$!
  if wait "$pid"; then status=0; else status=$?; fi
  kill "$watchdog" >/dev/null 2>&1 || true
  wait "$watchdog" 2>/dev/null || true
}
# calls: the kubectl and helm calls of the last stub_run.
calls() { grep -E '^(kubectl|helm) ' "$STUB_LOG" || true; }
# refused NAME USAGE COMMAND...: COMMAND exits 2, prints USAGE as a line on stderr (or, for a
# Python script, a line starting with usage:), and calls nothing.
refused() {
  local name=$1 usage=$2
  shift 2
  stub_run "$name" 20 "$ROOT" "$@"
  [ "$status" = 2 ] || fail "$name: exits $status, not 2"
  if [ "$usage" = argparse ]; then
    grep -q '^usage: ' "$STUB_LOG.err" || fail "$name: no usage line on stderr"
  else
    grep -qxF -- "$usage" "$STUB_LOG.err" || fail "$name: no line '$usage' on stderr"
  fi
  [ ! -s "$STUB_LOG" ] || fail "$name: calls $(head -n 1 "$STUB_LOG" | cut -d' ' -f1-2) before refusing"
}
# unknown NAME COMMAND...: a context that is not a whole name in the kubeconfig (stub, a prefix of
# stub-ctx) is refused after the guard's one read and before any other call.
unknown() {
  local name=$1
  shift
  stub_run "$name" 20 "$ROOT" "$@"
  [ "$status" != 0 ] || fail "$name: accepts the unknown context stub"
  [ "$(calls)" = 'kubectl config get-contexts -o name' ] ||
    fail "$name: calls more than kubectl config get-contexts -o name for an unknown context"
}
# named NAME: in the last stub_run, the first call is the guard's and every other call names
# stub-ctx first.
named() {
  local bad
  [ "$(calls | head -n 1)" = 'kubectl config get-contexts -o name' ] ||
    fail "$1: the first call is not kubectl config get-contexts -o name"
  bad=$(calls | grep -vxF 'kubectl config get-contexts -o name' |
    grep -vE '^kubectl --context stub-ctx |^helm --kube-context stub-ctx ' || true)
  [ -z "$bad" ] || fail "$1: calls without the context first: $(printf '%s\n' "$bad" | head -n 3 | tr '\n' ';')"
}
# line_of PATTERN: the number of the first log line matching the extended regular expression.
line_of() { grep -nE -- "$1" "$STUB_LOG" | head -n 1 | cut -d: -f1; }
last_line_of() { grep -nE -- "$1" "$STUB_LOG" | tail -n 1 | cut -d: -f1; }

SOAK="$ROOT/tests/soak"
if [ -f "$SOAK/install.sh" ] && [ -f "$SOAK/exercise.sh" ] && [ -f "$SOAK/uninstall.sh" ]; then
  since=$FAILURES
  for script in install uninstall; do
    usage="usage: tests/soak/$script.sh <kube-context>"
    refused "$script-none" "$usage" "tests/soak/$script.sh"
    refused "$script-empty" "$usage" "tests/soak/$script.sh" ''
    refused "$script-option" "$usage" "tests/soak/$script.sh" -x
    refused "$script-extra" "$usage" "tests/soak/$script.sh" stub-ctx extra
    unknown "$script-unknown" "tests/soak/$script.sh" stub
  done
  usage='usage: tests/soak/exercise.sh <kube-context> [minutes]'
  refused exercise-none "$usage" tests/soak/exercise.sh
  refused exercise-empty "$usage" tests/soak/exercise.sh ''
  refused exercise-option "$usage" tests/soak/exercise.sh -x
  refused exercise-extra "$usage" tests/soak/exercise.sh stub-ctx 5 extra
  for minutes in 0 61 abc 1.5 -1; do
    refused "exercise-minutes-$minutes" "$usage" tests/soak/exercise.sh stub-ctx "$minutes"
  done
  unknown exercise-unknown tests/soak/exercise.sh stub 1
  passed "$since" "soak shell scripts: a missing, empty, option-like, extra or unknown context is refused before any call"

  # install.sh, in a copy of tests/ whose pin is the fake chart's SHA-256: the namespace, the
  # ConfigMap, the endpoint, its rollout, the Secret, then the release from the pinned chart.
  since=$FAILURES
  SOAK_ROOT="$RELIABILITY/soak-root"
  mkdir -p "$SOAK_ROOT/tests"
  cp -R "$ROOT/tests/e2e" "$ROOT/tests/soak" "$ROOT/tests/bench" "$SOAK_ROOT/tests/"
  cp "$ROOT/Cargo.toml" "$SOAK_ROOT/"
  stub_sha=$(printf 'stub chart\n' |
    python3 -I -c 'import hashlib, sys; print(hashlib.sha256(sys.stdin.buffer.read()).hexdigest())')
  sed "s/^KAUSA_UPGRADE_FROM_CHART_SHA256=.*/KAUSA_UPGRADE_FROM_CHART_SHA256=$stub_sha/" \
    "$UPGRADE_FROM" >"$SOAK_ROOT/tests/e2e/upgrade-from.txt"
  stub_run install 60 "$SOAK_ROOT" tests/soak/install.sh stub-ctx
  named install
  install_line=$(line_of "^helm --kube-context stub-ctx install kausa-soak [^ ]+ -n kausa-soak -f tests/soak/values.yaml --set image.digest=$FROM_DIGEST --wait --timeout 300s$")
  previous=0
  for step in 'create (namespace|ns) kausa-soak' 'create configmap kausa-soak-endpoint .*fake_endpoint\.py' \
    'apply .*tests/soak/endpoint\.yaml' 'rollout status (deploy|deployment)/kausa-soak-endpoint' \
    'create secret generic kausa-soak-notify .*--from-file=url='; do
    at=$(line_of "^kubectl --context stub-ctx .*$step")
    if [ -z "$at" ]; then
      fail "install: no call matching '$step'"
    elif [ "$at" -le "$previous" ]; then
      fail "install: '$step' is out of order"
    else
      previous=$at
    fi
  done
  if [ -z "$install_line" ]; then
    fail "install: no helm install of kausa-soak from the downloaded chart with tests/soak/values.yaml and the pinned digest"
  elif [ "$install_line" -le "$previous" ]; then
    fail "install: helm install runs before the endpoint and the Secret are ready"
  fi
  if ! grep -qF 'pod-security.kubernetes.io/enforce=restricted' "$STUB_LOG" &&
    ! grep -qF 'pod-security.kubernetes.io/enforce: restricted' "$STUB_LOG.manifests" 2>/dev/null; then
    fail "install: kausa-soak is not labelled pod-security.kubernetes.io/enforce=restricted"
  fi
  [ "$(tr -d '\n' <"$STUB_LOG.url" 2>/dev/null)" = 'http://kausa-soak-endpoint.kausa-soak.svc:8080/hook/soak' ] ||
    fail "install: the Secret's url is not the fake endpoint's /hook/soak"
  grep -q kausa-test "$STUB_LOG" && fail "install: touches the seeded failures"
  if [ "$status" = 0 ]; then
    grep -qF tests/soak/sample.py "$STUB_LOG.out" "$STUB_LOG.err" ||
      fail "install: does not print the sampler's command"
  fi
  # The real pin and the fake chart: refused before any install.
  stub_run install-checksum 60 "$ROOT" tests/soak/install.sh stub-ctx
  [ "$status" != 0 ] || fail "install-checksum: installs a chart that is not the pinned one"
  calls | grep -q '^helm' && fail "install-checksum: calls helm with a chart that is not the pinned one"
  # An existing namespace: refused, nothing created.
  stub_run install-exists 60 "$SOAK_ROOT" env STUB_NS_EXISTS=1 tests/soak/install.sh stub-ctx
  [ "$status" != 0 ] || fail "install-exists: installs over an existing kausa-soak"
  grep -qF 'the namespace kausa-soak exists in this context: run tests/soak/uninstall.sh first' \
    "$STUB_LOG.err" || fail "install-exists: no message about the existing namespace"
  calls | grep -Eq '^helm| create | apply ' && fail "install-exists: creates or installs something"
  passed "$since" "soak install.sh: the endpoint, its Secret and the pinned release, in kausa-soak only"

  # exercise.sh: a failing Job, the 401 fail file, the wait, the file removed, also on TERM.
  since=$FAILURES
  stub_run exercise 60 "$ROOT" tests/soak/exercise.sh stub-ctx 1
  [ "$status" = 0 ] || fail "exercise: exits $status: $(head -n 3 "$STUB_LOG.err")"
  named exercise
  write=$(line_of '^kubectl .* exec .*deploy(ment)?/kausa-soak-endpoint .*-c endpoint .*printf "401\\n" > /tmp/fail$')
  remove=$(last_line_of '^kubectl .* exec .*deploy(ment)?/kausa-soak-endpoint .*-c endpoint .*rm -f /tmp/fail$')
  slept=$(last_line_of '^sleep ')
  if [ -z "$write" ] || [ -z "$remove" ] || [ -z "$slept" ]; then
    fail "exercise: no 401 written, no wait or no removal of /tmp/fail in the endpoint container"
  elif ! [ "$write" -lt "$slept" ] || ! [ "$slept" -lt "$remove" ]; then
    fail "exercise: the 401 is not written, waited on, then removed, in that order"
  fi
  grep -E '^kubectl .* exec ' "$STUB_LOG" | grep -vqE -- '(-n|--namespace)[ =]kausa-soak( |$)' &&
    fail "exercise: an exec outside kausa-soak"
  BUSYBOX=$(grep -Eo 'busybox[^ ]*@sha256:[0-9a-f]{64}' "$ROOT/tests/bench/seeded-failures.yaml" | head -n 1)
  if [ ! -s "$STUB_LOG.manifests" ] || ! yaml_docs "$STUB_LOG.manifests" "$RELIABILITY/job.json"; then
    fail "exercise: no Job manifest given to kubectl -f"
  else
    created=$(grep -E '^kubectl .* (create|apply) ' "$STUB_LOG" | head -n 1)
    # shellcheck disable=SC2016 # Python, not shell
    json_check exercise '
import json, re, sys
jobs = [d for d in map(json.loads, open(sys.argv[1])) if d.get("kind") == "Job"]
busybox, created = sys.argv[2], sys.argv[3]
if len(jobs) != 1:
    print("not one Job but %d" % len(jobs)); sys.exit()
job = jobs[0]
meta, spec = job.get("metadata") or {}, job.get("spec") or {}
pod = (spec.get("template") or {}).get("spec") or {}
if not re.fullmatch(r"soak-breaker-[0-9]{12}", str(meta.get("name"))):
    print("the Job is not named soak-breaker-<UTC yyyymmddHHMM>")
if meta.get("namespace", "") != "kausa-soak" and not re.search(r"(-n|--namespace)[ =]kausa-soak( |$)", created):
    print("the Job is not in kausa-soak")
if spec.get("backoffLimit") != 0 or spec.get("ttlSecondsAfterFinished") != 86400:
    print("backoffLimit is not 0 or ttlSecondsAfterFinished is not 86400")
if pod.get("restartPolicy") != "Never":
    print("restartPolicy is not Never")
cs = pod.get("containers") or []
if len(cs) != 1 or cs[0].get("image") != busybox:
    print("not one container with the busybox of tests/bench/seeded-failures.yaml")
elif (cs[0].get("command") or []) + (cs[0].get("args") or []) != ["sh", "-c", "exit 1"]:
    print("the command is not sh -c \"exit 1\"")
else:
    sc = dict(pod.get("securityContext") or {})
    sc.update(cs[0].get("securityContext") or {})
    for key, value in (("runAsNonRoot", True), ("allowPrivilegeEscalation", False),
                       ("capabilities", {"drop": ["ALL"]}), ("seccompProfile", {"type": "RuntimeDefault"})):
        if sc.get(key) != value:
            print("securityContext.%s is not %s" % (key, json.dumps(value)))
    if not isinstance(sc.get("runAsUser"), int) or sc.get("runAsUser") == 0:
        print("no non-root runAsUser")
' "$RELIABILITY/job.json" "$BUSYBOX" "$created"
  fi
  stub_run exercise-term 60 "$ROOT" env STUB_TERM_ON_SLEEP=1 tests/soak/exercise.sh stub-ctx 1
  write=$(line_of 'printf "401\\n" > /tmp/fail$')
  remove=$(last_line_of '^kubectl .* exec .*deploy(ment)?/kausa-soak-endpoint .*-c endpoint .*rm -f /tmp/fail$')
  if [ -z "$write" ] || [ -z "$remove" ] || [ "$remove" -le "$write" ]; then
    fail "exercise-term: /tmp/fail is not removed when the script is stopped with TERM"
  fi
  passed "$since" "soak exercise.sh: a failing Job, 401 in the fail file, removed after the wait and on TERM"

  # uninstall.sh: the release, then the namespace; a reminder, never the cleanup, of the seeded
  # failures.
  since=$FAILURES
  stub_run uninstall 60 "$ROOT" tests/soak/uninstall.sh stub-ctx
  [ "$status" = 0 ] || fail "uninstall: exits $status: $(head -n 3 "$STUB_LOG.err")"
  named uninstall
  helm_at=$(line_of '^helm --kube-context stub-ctx uninstall kausa-soak -n kausa-soak --wait$')
  ns_at=$(line_of '^kubectl --context stub-ctx delete namespace kausa-soak --wait$')
  if [ -z "$helm_at" ] || [ -z "$ns_at" ] || [ "$ns_at" -le "$helm_at" ]; then
    fail "uninstall: not helm uninstall kausa-soak, then the namespace deleted"
  fi
  grep -Eiq 'kausa-test|runtimeclass' "$STUB_LOG" && fail "uninstall: deletes the seeded failures"
  grep -Eq 'seeded-failures|kausa-test' "$STUB_LOG.out" "$STUB_LOG.err" ||
    fail "uninstall: no reminder of the seeded failures' cleanup"
  passed "$since" "soak uninstall.sh: the release and kausa-soak, and a reminder of the seeded failures"
else
  fail "tests/soak/install.sh, exercise.sh or uninstall.sh is missing"
fi

# sample.py: the usage refusals, then a few seconds against the fake cluster.
if [ -f "$SOAK/sample.py" ]; then
  since=$FAILURES
  SAMPLE=(python3 -I tests/soak/sample.py)
  refused sample-none argparse "${SAMPLE[@]}"
  refused sample-no-out argparse "${SAMPLE[@]}" --context stub-ctx
  refused sample-empty argparse "${SAMPLE[@]}" --context '' --out "$RELIABILITY/never"
  refused sample-option argparse "${SAMPLE[@]}" --context -x --out "$RELIABILITY/never"
  unknown sample-unknown "${SAMPLE[@]}" --context stub --out "$RELIABILITY/never"
  [ ! -e "$RELIABILITY/never" ] || fail "sample: writes output for a refused context"
  passed "$since" "soak sample.py: a missing, empty, option-like or unknown context is refused before any call"

  since=$FAILURES
  SAMPLES="$RELIABILITY/samples"
  stub_run sample 60 "$ROOT" "${SAMPLE[@]}" --context stub-ctx --out "$SAMPLES" --interval 1 --days 0.00005
  [ "$status" = 0 ] || fail "sample: exits $status: $(head -n 3 "$STUB_LOG.err")"
  named sample
  calls | grep -q '^helm' && fail "sample: calls helm"
  grep -rqF stub-node "$SAMPLES" "$STUB_LOG.out" "$STUB_LOG.err" && fail "sample: writes a node name"
  # shellcheck disable=SC2016 # Python, not shell
  SAMPLE_CHECK='
import glob, json, os, sys
out, mode = sys.argv[1], sys.argv[2]
fields = {"ts", "pod", "node", "holder", "ready", "restarts", "last_terminated_reason", "ws_bytes",
          "rss_bytes", "usage_bytes", "cpu_ns", "top_memory", "top_cpu", "max_usage_bytes",
          "oom_events", "kausa", "errors"}
files = sorted(glob.glob(os.path.join(out, "samples-*.jsonl")))
if not files:
    print("no samples-<date>.jsonl"); sys.exit()
lines = [json.loads(l) for f in files for l in open(f) if l.strip()]
pods = {l.get("pod") for l in lines}
if pods != {"kausa-soak-stub-1", "kausa-soak-stub-2"}:
    print("the samples are not of the two kausa Pods")
if len(lines) < 4:
    print("fewer than two samples per Pod")
ws = {"kausa-soak-stub-1": 24117248, "kausa-soak-stub-2": 23068672}
first = {}
for l in lines:
    missing = fields - set(l)
    if missing:
        print("a sample lacks %s" % sorted(missing)); break
    if not isinstance(l["node"], int) or isinstance(l["node"], bool):
        print("node is not an index"); break
    if l["holder"] is not (l["pod"] == "kausa-soak-stub-1"):
        print("holder is not true for the Lease holder alone"); break
    if l["ready"] is not True or l["restarts"] != 0:
        print("ready or restarts is not read from the Pod"); break
    if l["top_memory"] is not None or l["top_cpu"] is not None or "top" not in l["errors"]:
        print("without metrics-server, top_* are not null with top in errors"); break
    if mode == "summary-fails":
        if l["ws_bytes"] is not None or "summary" not in l["errors"]:
            print("a failed summary read is not null with summary in errors"); break
    elif l["ws_bytes"] != ws.get(l["pod"]):
        print("ws_bytes is not the kubelet summary of the Pod container kausa"); break
    first.setdefault(l["pod"], l)
    kausa = l["kausa"] or {}
    if any(not k.startswith("kausa_") for k in kausa):
        print("kausa holds a series that is not kausa_"); break
    if kausa.get("kausa_leader") != (1 if l["pod"] == "kausa-soak-stub-1" else 0):
        print("kausa does not hold kausa_leader of the Pod"); break
for pod, l in first.items():
    if l["max_usage_bytes"] != 31457280 or l["oom_events"] != 0:
        print("the first sample of %s lacks the cAdvisor max usage and OOM events of container kausa" % pod)
leader = first.get("kausa-soak-stub-1", {}).get("kausa") or {}
if leader.get("kausa_endpoint_state{state=\"open\"}") != 1:
    print("the leader sample lacks kausa_endpoint_state{state=\"open\"} 1")
cluster = sorted(glob.glob(os.path.join(out, "cluster-*.jsonl")))
counts = [json.loads(l) for f in cluster for l in open(f) if l.strip()]
if not counts:
    print("no cluster-<date>.jsonl")
elif set(counts[0]) != {"ts", "pods", "warning_events", "nodes"}:
    print("a cluster line is not ts, pods, warning_events and nodes")
elif (counts[0]["pods"], counts[0]["warning_events"], counts[0]["nodes"]) != (3, 2, 3):
    print("the cluster counts are not those of the fake cluster")
'
  json_check sample "$SAMPLE_CHECK" "$SAMPLES" ok
  stub_run sample-summary-fails 60 "$ROOT" env STUB_FAIL_SUMMARY=1 "${SAMPLE[@]}" --context stub-ctx \
    --out "$RELIABILITY/samples-failing" --interval 1 --days 0.00002
  [ "$status" = 0 ] || fail "sample-summary-fails: exits $status when the kubelet summary fails"
  json_check sample-summary-fails "$SAMPLE_CHECK" "$RELIABILITY/samples-failing" summary-fails
  passed "$since" "soak sample.py: one line per Pod per sample, node indexes, null for a failed read"
else
  fail "tests/soak/sample.py is missing"
fi

# summarize.py: its self-test, the dry run's few minutes, and the decision rule of spec B7 on
# synthetic samples of 168 hours, two Pods at 30-s intervals: flat (S1 VALID, S2 and S3 PASS);
# +1 MiB/day (S3 FAIL); +0.3 MiB/day (S3 INCONCLUSIVE, extend to 14 days); a 40% gap, a restart,
# the breaker open on 3 days only (S1 INVALID); an OOMKilled and a working set over 32 MiB (S2
# FAIL); and a window shorter than 168 hours (S1 INVALID).
if [ -f "$SOAK/summarize.py" ]; then
  since=$FAILURES
  SUMMARIZE=(python3 -I tests/soak/summarize.py)
  stub_run summarize-self-test 120 "$ROOT" "${SUMMARIZE[@]}" --self-test
  [ "$status" = 0 ] || fail "summarize --self-test exits $status: $(tail -n 3 "$STUB_LOG.err")"
  grep -qx 'self-test: ok' "$STUB_LOG.out" || fail "summarize --self-test does not print self-test: ok"
  [ ! -s "$STUB_LOG" ] || fail "summarize --self-test calls $(head -n 1 "$STUB_LOG" | cut -d' ' -f1)"
  if [ -d "$RELIABILITY/samples" ]; then
    stub_run summarize-dry 60 "$ROOT" "${SUMMARIZE[@]}" "$RELIABILITY/samples" --hours 0.15
    [ "$status" = 0 ] || fail "summarize of a short run exits $status: $(tail -n 3 "$STUB_LOG.err")"
    [ ! -s "$STUB_LOG" ] || fail "summarize calls $(head -n 1 "$STUB_LOG" | cut -d' ' -f1)"
    grep -Eq '^S1 INVALID: .*shorter than 168 hours' "$STUB_LOG.out" ||
      fail "summarize of a 0.15-hour window: no 'S1 INVALID: ... shorter than 168 hours'"
    for verdict in 'S2 (PASS|FAIL): ' 'S3 (PASS|FAIL|INCONCLUSIVE): '; do
      grep -Eq "^$verdict" "$STUB_LOG.out" || fail "summarize of a short run: no line ^$verdict"
    done
  fi
  # shellcheck disable=SC2016 # Python, not shell
  SYNTHETIC='
import json, os, sys, time
out, case = sys.argv[1], sys.argv[2]
MIB = 2 ** 20
START = 1767225600  # 2026-01-01T00:00:00Z
HOURS = 10 if case == "short" else 168
rate = {"grow-1": 1.0, "grow-0.3": 0.3}.get(case, 0.0)
breaker_days = (1, 2, 3) if case == "breaker-3-days" else (1, 2, 3, 4, 5)
files = {}
def write(name, ts, record):
    day = time.strftime("%Y-%m-%d", time.gmtime(ts))
    path = os.path.join(out, "%s-%s.jsonl" % (name, day))
    if path not in files:
        files[path] = open(path, "w")
    files[path].write(json.dumps(record, separators=(",", ":")) + "\n")
os.makedirs(out, exist_ok=True)
for i in range(HOURS * 120):
    ts = START + 30 * i
    hours = (ts - START) / 3600
    if case == "gap-40" and 40 <= hours < 40 + 0.4 * HOURS:
        continue
    day, hour = int(hours // 24), hours % 24
    open_now = day in breaker_days and 12 <= hour < 12 + 10 / 60
    if i % 20 == 0:
        write("cluster", ts, {"ts": ts, "pods": 100, "warning_events": 40, "nodes": 3})
    for n, pod in enumerate(("kausa-soak-synth-1", "kausa-soak-synth-2")):
        leader = n == 0
        ws = 20 * MIB + n * MIB + rate * MIB * hours / 24
        restarts, reason = 0, None
        if hours >= 100 and n == 1 and case in ("restart", "oom"):
            restarts, reason = 1, ("OOMKilled" if case == "oom" else "Error")
        if case == "ws-33" and n == 0 and i == 5000:
            ws = 33 * MIB
        kausa = {"kausa_leader": 1 if leader else 0}
        if leader:
            kausa.update({
                "kausa_model_calls_total{outcome=\"ok\"}": int(hours // 6),
                "kausa_model_calls_total{outcome=\"error\"}": sum(1 for d in breaker_days if d < day or (d == day and hour >= 12)),
                "kausa_incidents_total{detection=\"D001\"}": int(hours // 6),
                "kausa_explanation_queue_length": 1 if open_now else 0,
                "kausa_budget_calls_remaining{window=\"hour\"}": 30,
                "kausa_budget_calls_remaining{window=\"day\"}": 200,
                "kausa_endpoint_state{state=\"closed\"}": 0 if open_now else 1,
                "kausa_endpoint_state{state=\"half_open\"}": 0,
                "kausa_endpoint_state{state=\"open\"}": 1 if open_now else 0,
            })
        cadvisor = i % 20 == 0
        write("samples", ts, {
            "ts": ts, "pod": pod, "node": n, "holder": leader, "ready": True,
            "restarts": restarts, "last_terminated_reason": reason,
            "ws_bytes": int(ws), "rss_bytes": int(ws) - MIB, "usage_bytes": int(ws) + 2 * MIB,
            "cpu_ns": int(5e6 * 30 * i), "top_memory": None, "top_cpu": None,
            "max_usage_bytes": int(ws) + 4 * MIB if cadvisor else None,
            "oom_events": 0 if cadvisor else None, "kausa": kausa, "errors": ["top"],
        })
for f in files.values():
    f.close()
'
  # verdict CASE ARGS... -- PATTERNS...: summarize.py on the synthetic CASE prints a line matching
  # each extended regular expression.
  verdict() {
    local case=$1 dir="$RELIABILITY/synthetic-$1" pattern
    shift
    local args=()
    while [ "$1" != -- ]; do args+=("$1"); shift; done
    shift
    rm -rf "$dir"
    if ! python3 -I -c "$SYNTHETIC" "$dir" "$case" 2>"$RELIABILITY/synthetic.err"; then
      fail "summarize/$case: the synthetic samples cannot be written: $(cat "$RELIABILITY/synthetic.err")"
      return
    fi
    stub_run "summarize-$case" 120 "$ROOT" "${SUMMARIZE[@]}" "$dir" ${args[@]+"${args[@]}"}
    [ "$status" = 0 ] || fail "summarize/$case: exits $status: $(tail -n 3 "$STUB_LOG.err")"
    for pattern in "$@"; do
      grep -Eq -- "$pattern" "$STUB_LOG.out" ||
        fail "summarize/$case: no line matching '$pattern' in: $(grep -E '^S[123] ' "$STUB_LOG.out" | tr '\n' '|')"
    done
    rm -rf "$dir"
  }
  verdict flat -- '^S1 VALID: ' '^S2 PASS: ' '^S3 PASS: '
  verdict grow-1 -- '^S1 VALID: ' '^S2 PASS: ' '^S3 FAIL: '
  verdict grow-0.3 -- '^S3 INCONCLUSIVE: .*extend to 14 days\.?$'
  verdict gap-40 -- '^S1 INVALID: '
  verdict restart -- '^S1 INVALID: '
  verdict breaker-3-days -- '^S1 INVALID: '
  verdict oom -- '^S2 FAIL: '
  verdict ws-33 -- '^S2 FAIL: '
  verdict short --hours 10 -- '^S1 INVALID: .*shorter than 168 hours'
  passed "$since" "soak summarize.py: the self-test and the decision rule of spec B7 on synthetic samples"
else
  fail "tests/soak/summarize.py is missing"
fi

# --- The pages

# The five e2e checks named alike wherever the checks are listed; the pin's bump in "Cut a
# release"; the soak's how-to with its rule, dated before any measurement, linked from the front
# page and mapped in ARCHITECTURE.md.
since=$FAILURES
# joined FILE: the file's text on one line, so that a sentence wrapped across lines still matches.
joined() { tr '\n' ' ' <"$ROOT/$1" | tr -s ' '; }
for page in docs/how-to/develop.md CONTRIBUTING.md AGENTS.md ARCHITECTURE.md \
  docs/explanation/architecture.md; do
  case "$(joined "$page")" in
    *tests/e2e/upgrade.sh*) ;;
    *) fail "$page does not name tests/e2e/upgrade.sh" ;;
  esac
done
develop=$(joined docs/how-to/develop.md)
for words in 'kind (cluster)' 'kind (namespaced)' 'kind (namespaced, standby)' 'upgrade (cluster)' \
  'upgrade (namespaced)' tests/e2e/lib.sh tests/e2e/upgrade-from.txt; do
  case "$develop" in *"$words"*) ;; *) fail "develop.md does not name $words" ;; esac
done
maintain=$(joined docs/how-to/maintain.md)
for words in tests/e2e/upgrade-from.txt 'gh release download' SHA256SUMS \
  'docker buildx imagetools inspect ghcr.io/fedeam/kausa:'; do
  case "$maintain" in *"$words"*) ;; *) fail "maintain.md, Cut a release: no '$words'" ;; esac
done
# shellcheck disable=SC2016 # Markdown backquotes
case "$(joined ARCHITECTURE.md)" in
  *'| `tests/soak/` |'*) ;;
  *) fail "ARCHITECTURE.md has no tests/soak/ row" ;;
esac
grep -q 'how-to/run-the-soak\.md' "$ROOT/docs/index.md" || fail "docs/index.md does not link run-the-soak.md"
if [ ! -f "$ROOT/docs/how-to/run-the-soak.md" ]; then
  fail "docs/how-to/run-the-soak.md is missing"
else
  soak=$(joined docs/how-to/run-the-soak.md)
  head -n 1 "$ROOT/docs/how-to/run-the-soak.md" | grep -q '^# ' || fail "run-the-soak.md has no H1 first"
  for words in 'set on 2026-11-03, before any measurement' 'tests/soak/install.sh' \
    'tests/soak/exercise.sh' 'tests/soak/uninstall.sh' 'tests/soak/sample.py --context' \
    'tests/soak/summarize.py' tests/bench/seeded-failures.yaml consent '90%' '32 MiB' '64 MiB' \
    '0.1 MiB' '0.5 MiB' '14 days' '96 MiB' '128 MiB'; do
    case "$soak" in *"$words"*) ;; *) fail "run-the-soak.md does not say '$words'" ;; esac
  done
  case "$soak" in
    *'out of the repository'* | *'outside the repository'*) ;;
    *) fail "run-the-soak.md does not say the output stays out of the repository" ;;
  esac
fi
passed "$since" "the pages: the five e2e checks, the pin's bump, the soak how-to and its rule"

if [ "$FAILURES" -ne 0 ]; then
  echo "FAIL: $FAILURES chart checks failed" >&2
  exit 1
fi
echo "ok: every chart check passed"
