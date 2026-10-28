#!/usr/bin/env bash
# Chart check: lints and renders charts/kausa in both RBAC scopes
# (docs/adr/0013-namespaced-install-mode.md) and checks the rendered objects, the rules, the
# Deployment's arguments, NOTES.txt, and every refusal of kausa.validate with its exact message.
# Needs helm, git and coreutils only, and never contacts a cluster (KUBECONFIG points nowhere).
# Run it from anywhere: tests/chart/render.sh. It prints one line per check and exits 1 if any
# failed.
#
# KAUSA_CHART_BASE names a git ref with the v0.1 chart (default v0.1.0); when the ref is in the
# clone, the ClusterRole rules are also compared with that chart's render. HELM names the binary.
# No pipefail: grep -q ends a pipe early, and its writer's SIGPIPE is not a failure.
set -eu

ROOT=$(cd "$(dirname "$0")/../.." && pwd)
CHART="$ROOT/charts/kausa"
HELM=${HELM:-helm}
BASE=${KAUSA_CHART_BASE:-v0.1.0}
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

# docs FILE KIND [NAMESPACE]: the documents of that kind (and metadata.namespace), each followed
# by a line '%%'.
docs() {
  awk -v kind="$2" -v ns="${3-}" '
    function flush() {
      if (k && (ns == "" || n)) { printf "%s", buf; print "%%" }
      buf = ""; k = 0; n = 0; meta = 0
    }
    /^---/ { flush(); next }
    { buf = buf $0 "\n" }
    /^[^ #]/ { meta = ($0 == "metadata:") }
    $0 == "kind: " kind { k = 1 }
    meta && ns != "" && ($0 == "  namespace: " ns || $0 == "  namespace: \"" ns "\"" ||
                         $0 == "  namespace: '\''" ns "'\''") { n = 1 }
    END { flush() }
  ' "$1"
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
rule_owners() {
  printf '%s\n' '-apiGroups:["apps"]' 'resources:["replicasets"]' 'verbs:["get"]' \
    '-apiGroups:["batch"]' 'resources:["jobs"]' 'verbs:["get"]'
}
# expected_rules LOGS WRITES: the rules for "yes"/"no" pods/log and Event writes.
expected_rules() {
  rule_pods
  if [ "$1" = yes ]; then rule_logs; fi
  rule_events_read
  if [ "$2" = yes ]; then rule_events_write; fi
  rule_owners
}

# NOTES.txt is not part of helm template's output: a copy of the chart renders it as a string.
NOTES_CHART="$WORK/notes/kausa"
mkdir -p "$WORK/notes"
cp -R "$CHART" "$NOTES_CHART"
{
  echo '{{- define "kausa.notesCheck" }}'
  cat "$CHART/templates/NOTES.txt"
  echo '{{- end }}'
  echo 'notes: {{ include "kausa.notesCheck" . | toJson }}'
} >"$NOTES_CHART/templates/zz-notes-check.yaml"

# notes OUT ARGS...: the rendered NOTES.txt, as text.
notes() {
  local out=$1
  shift
  render "$out.raw" "$RELEASE" "$NOTES_CHART" --show-only templates/zz-notes-check.yaml "$@" || {
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
check_cluster() {
  local name=$1 logs=$2 writes=$3
  shift 3
  local out="$WORK/cluster-$name" since=$FAILURES
  if ! render "$out" "$RELEASE" "$CHART" --set rbac.scope=cluster "$@"; then
    fail "cluster/$name does not render: $(cat "$out.err")"
    return 0
  fi
  [ "$(count_docs "$out" ClusterRole)" = 1 ] || fail "cluster/$name: not exactly one ClusterRole"
  [ "$(count_docs "$out" ClusterRoleBinding)" = 1 ] ||
    fail "cluster/$name: not exactly one ClusterRoleBinding"
  [ "$(count_docs "$out" Role)" = 0 ] || fail "cluster/$name: renders a Role"
  [ "$(count_docs "$out" RoleBinding)" = 0 ] || fail "cluster/$name: renders a RoleBinding"
  [ "$(count_docs "$out" Namespace)" = 0 ] || fail "cluster/$name: renders a Namespace"
  docs "$out" ClusterRole | section rules >"$out.rules"
  expected_rules "$logs" "$writes" >"$out.expected"
  diff -u "$out.expected" "$out.rules" >"$out.diff" ||
    fail "cluster/$name: the ClusterRole rules are not v0.1's: $(cat "$out.diff")"
  docs "$out" ClusterRole | grep -qx '  name: kausa' || fail "cluster/$name: ClusterRole name"
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
  passed "$since" "cluster/$name"
}

check_cluster default no no
check_cluster endpoint yes yes "${ENDPOINT[@]}"
check_cluster dry-run-no-logs no no "${DRY_NO_LOGS[@]}"
check_cluster dry-run yes no "${DRY[@]}"
check_cluster no-events yes no "${NO_EVENTS[@]}"

# The same rules as the v0.1 chart's render with the same values, when that chart is in the clone.
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
      if [ -f "$WORK/cluster-$name.rules" ] &&
        diff -u "$out.rules" "$WORK/cluster-$name.rules" >"$out.diff"; then
        ok "cluster/$name has the rules of $BASE"
      else
        fail "cluster/$name: the rules differ from $BASE's: $(cat "$out.diff" 2>/dev/null)"
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
# FULLNAME and SUBJECT (the names of the objects and the service account) default to kausa.
check_namespaced() {
  local name=$1 logs=$2 writes=$3 list=$4
  shift 4
  local out="$WORK/namespaced-$name" fullname=${FULLNAME:-kausa} subject=${SUBJECT:-kausa}
  local release=${RELEASE_NAME:-$RELEASE} since=$FAILURES
  local IFS=,
  # shellcheck disable=SC2206 # split on commas on purpose
  local namespaces=($list) namespaces_checked
  unset IFS
  if ! render "$out" "$release" "$CHART" --set rbac.scope=namespaced \
    --set "rbac.namespaces={$list}" "$@"; then
    fail "namespaced/$name does not render: $(cat "$out.err")"
    return 0
  fi
  grep -q '^kind: Cluster' "$out" && fail "namespaced/$name: renders a cluster-scoped RBAC kind"
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
  expected_rules "$logs" "$writes" >"$out.expected"
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

# A namespace that is a number must stay a string in metadata.namespace.
for kind in Role RoleBinding; do
  docs "$WORK/namespaced-number" "$kind" 123 | grep -Eqx "  namespace: (\"123\"|'123')" ||
    fail "namespaced/number: the $kind's metadata.namespace is not the string \"123\""
done

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

if [ "$FAILURES" -ne 0 ]; then
  echo "FAIL: $FAILURES chart checks failed" >&2
  exit 1
fi
echo "ok: every chart check passed"
