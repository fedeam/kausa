#!/usr/bin/env bash
# Chart check: lints and renders charts/kausa in both RBAC scopes
# (docs/adr/0013-namespaced-install-mode.md) and with the explanation history on and off
# (docs/adr/0014-explanation-history.md), and checks the rendered objects, the rules, the
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
rule_owners() {
  printf '%s\n' '-apiGroups:["apps"]' 'resources:["replicasets"]' 'verbs:["get"]' \
    '-apiGroups:["batch"]' 'resources:["jobs"]' 'verbs:["get"]'
}
# The history rule (docs/adr/0014-explanation-history.md), last.
rule_history() {
  printf '%s\n' '-apiGroups:["kausa.fedeam.com"]' 'resources:["explanations"]' \
    'verbs:["list","create","patch","delete"]'
}
# expected_rules LOGS WRITES [HISTORY]: the rules for "yes"/"no" pods/log, Event writes and the
# history.
expected_rules() {
  rule_pods
  if [ "$1" = yes ]; then rule_logs; fi
  rule_events_read
  if [ "$2" = yes ]; then rule_events_write; fi
  rule_owners
  if [ "${3-no}" = yes ]; then rule_history; fi
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
# HISTORY=yes expects the history rule last and --history in the args; EXTRA lists the other
# cluster-scoped objects expected (Kind/name, space-separated), none by default.
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

if [ "$FAILURES" -ne 0 ]; then
  echo "FAIL: $FAILURES chart checks failed" >&2
  exit 1
fi
echo "ok: every chart check passed"
