#!/usr/bin/env bash
# Chart check: lints and renders charts/kausa in both RBAC scopes
# (docs/adr/0013-namespaced-install-mode.md), with the explanation history on and off
# (docs/adr/0014-explanation-history.md), with notifications on and off
# (docs/adr/0015-notifications.md), with the Deployment watch of D014 on and off
# (docs/adr/0018-stuck-rollouts.md) and with one or two replicas
# (docs/adr/0019-high-availability.md), and checks the rendered objects, the rules, the
# Deployment's arguments and volumes, the NetworkPolicy, NOTES.txt, and every refusal of
# kausa.validate with its exact message.
# Needs helm, git and coreutils only, and never contacts a cluster (KUBECONFIG points nowhere).
# Run it from anywhere: tests/chart/render.sh. It prints one line per check and exits 1 if any
# failed.
#
# KAUSA_CHART_BASE names a git ref with the v0.1 chart (default v0.1.0); when the ref is in the
# clone, the ClusterRole rules are also compared with that chart's render: equal with
# rollouts=false, and with only the deployments rule added by default. KAUSA_CHART_PREVIOUS
# names a git ref with the chart before notifications (for example 13cfd4d, the history's merge;
# unset by default, because the chart's version labels differ after a release); when it is set and
# in the clone, renders with notifications off are compared with its renders byte for byte (with
# rollouts=false, its --no-rollouts arg left out). KAUSA_CHART_V03 names a git ref with the v0.3
# chart (default v0.3.0); when the ref is in the clone, NOTES.txt's scope paragraph with
# rollouts=false is compared with that chart's. KAUSA_CHART_PRE_HA names a git ref with the chart
# before replicaCount (unset by default, for the same reason as KAUSA_CHART_PREVIOUS); when it is
# set and in the clone, the one-replica renders are compared with its renders byte for byte.
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
        # The chart before notifications had no Deployment watch: compare with it off.
        grep -vx -- ' *- --no-rollouts' "$now" >"$now.v0.3" || true
        if cmp -s "$out" "$now.v0.3"; then
          ok "$scope/$name renders like $PREVIOUS"
        else
          fail "$scope/$name renders unlike $PREVIOUS: $(diff "$out" "$now" | head -20)"
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

# egress FILE: the egress section of the NetworkPolicy, as rendered.
egress() {
  docs "$1" NetworkPolicy | awk '$0 == "  egress:" { on = 1; next } on && !/^    / { on = 0 } on'
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
      check_same "$set-is-$PRE_HA" "$WORK/standby-$set-one" "$out"
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

if [ "$FAILURES" -ne 0 ]; then
  echo "FAIL: $FAILURES chart checks failed" >&2
  exit 1
fi
echo "ok: every chart check passed"
