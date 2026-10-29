# CI image for kausa: every check runs here, so the Gitea job, the GitHub job and a developer
# machine run the same thing. Build it from a git checkout with full history (the secret scan
# reads .git):
#
#     docker build --target ci .
#
# Nothing from this image is pushed or shipped.
#
# The runtime target is the controller image: a static binary on distroless, for the architecture
# of the build (docs/adr/0010-packaging.md):
#
#     docker build --target runtime -t kausa .
#
# It comes after the ci target on purpose: the classic builder builds every stage up to the target,
# so `--target ci` never compiles the release binary, and `--target runtime` runs the checks first.
# The Gitea job, which has only the classic builder, cuts the file from the build stage's FROM line
# on, so the build and runtime stages must stay last and must not use the ci stage.
#
# It works with BuildKit and with the classic builder (a runner without buildx falls back to the
# classic one): no RUN --mount, no heredocs, no ADD --checksum, and TARGETARCH, which only
# BuildKit sets, falls back to the architecture of the build container.

# Same release as rust-toolchain.toml: Dependabot moves both in one pull request (the "rust" group
# in .github/dependabot.yml; see docs/how-to/maintain.md). The slim variant halves the image on the
# runner's shared disk (about 0.8 GB instead of 1.6 GB for linux/arm64).
FROM rust:1.99.1-slim-trixie@sha256:1a939748a7cbab172de91a014e3804a97af1394a8c844faa67fce9401c4e40fb AS ci

ARG TARGETARCH

# curl downloads the pinned tools below, retrying: connections from the Gitea runner's build
# network were reset on 2026-10-23 (fixed on the runner since), and the checksum still decides.
# git is needed by cargo-deny 0.20 (it fetches the advisory database with the git command) and by
# gitleaks (it reads the history with git log).
# apt checks the signatures of the Debian packages.
RUN set -eu; \
    apt-get update; \
    DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends ca-certificates curl git; \
    rm -rf /var/lib/apt/lists/*

# The image ships rustup's minimal profile; clippy and rustfmt come from the same release.
RUN rustup component add clippy rustfmt

# Pinned tools, each checked against a SHA-256 before it is installed. Dependabot does not update
# these ARGs: bump the version and both checksums by hand, from the new release
# (docs/how-to/maintain.md); the build fails until they match.

# cargo-deny: dependency policy (deny.toml). Checksums: the .sha256 file published next to each
# archive in the release, https://github.com/EmbarkStudios/cargo-deny/releases
ARG CARGO_DENY_VERSION=0.20.2
ARG CARGO_DENY_SHA256_AMD64=9f12ed4c49936e09b48bf862b595cde2fe64fcbd9d74dfacac6131ca824c8d5f
ARG CARGO_DENY_SHA256_ARM64=995c82be0defc7a025cae49a2aa2644ce8245c9a3318fc4103907c6a285e8c7d
RUN set -eu; \
    arch="${TARGETARCH:-$(dpkg --print-architecture)}"; \
    case "$arch" in \
      amd64) triple=x86_64-unknown-linux-musl; sum="$CARGO_DENY_SHA256_AMD64" ;; \
      arm64) triple=aarch64-unknown-linux-musl; sum="$CARGO_DENY_SHA256_ARM64" ;; \
      *) echo "no pinned cargo-deny for architecture '$arch'" >&2; exit 1 ;; \
    esac; \
    name="cargo-deny-$CARGO_DENY_VERSION-$triple"; \
    curl --proto '=https' --tlsv1.2 -fsSL --connect-timeout 20 --retry 5 --retry-all-errors -o /tmp/cargo-deny.tar.gz \
      "https://github.com/EmbarkStudios/cargo-deny/releases/download/$CARGO_DENY_VERSION/$name.tar.gz"; \
    echo "$sum  /tmp/cargo-deny.tar.gz" | sha256sum -c -; \
    tar -xzf /tmp/cargo-deny.tar.gz -C /tmp "$name/cargo-deny"; \
    install -m 0755 "/tmp/$name/cargo-deny" /usr/local/bin/cargo-deny; \
    rm -rf /tmp/cargo-deny.tar.gz "/tmp/$name"; \
    cargo-deny --version

# cargo-audit: RustSec advisories. Its releases (https://github.com/rustsec/rustsec/releases)
# publish no checksum file; these are the SHA-256 digests GitHub reports for the release assets:
#   gh api repos/rustsec/rustsec/releases/tags/cargo-audit%2Fv<version> --jq '.assets[].digest'
# There is no musl build for arm64, so arm64 uses the glibc one.
ARG CARGO_AUDIT_VERSION=0.22.2
ARG CARGO_AUDIT_SHA256_AMD64=7fb9497f8594b389e5fce5ef9b92db08432996895b2e0c5a0167a69ed445c428
ARG CARGO_AUDIT_SHA256_ARM64=c6603814ddaa45e51263dafd31c0ac98808f688d26f7395804f9670b0fd599dd
RUN set -eu; \
    arch="${TARGETARCH:-$(dpkg --print-architecture)}"; \
    case "$arch" in \
      amd64) triple=x86_64-unknown-linux-musl; sum="$CARGO_AUDIT_SHA256_AMD64" ;; \
      arm64) triple=aarch64-unknown-linux-gnu; sum="$CARGO_AUDIT_SHA256_ARM64" ;; \
      *) echo "no pinned cargo-audit for architecture '$arch'" >&2; exit 1 ;; \
    esac; \
    name="cargo-audit-$triple-v$CARGO_AUDIT_VERSION"; \
    curl --proto '=https' --tlsv1.2 -fsSL --connect-timeout 20 --retry 5 --retry-all-errors -o /tmp/cargo-audit.tgz \
      "https://github.com/rustsec/rustsec/releases/download/cargo-audit%2Fv$CARGO_AUDIT_VERSION/$name.tgz"; \
    echo "$sum  /tmp/cargo-audit.tgz" | sha256sum -c -; \
    tar -xzf /tmp/cargo-audit.tgz -C /tmp "$name/cargo-audit"; \
    install -m 0755 "/tmp/$name/cargo-audit" /usr/local/bin/cargo-audit; \
    rm -rf /tmp/cargo-audit.tgz "/tmp/$name"; \
    cargo-audit --version

# gitleaks: secret scan of the full git history. Checksums: gitleaks_<version>_checksums.txt in
# the release, https://github.com/gitleaks/gitleaks/releases
ARG GITLEAKS_VERSION=8.30.1
ARG GITLEAKS_SHA256_AMD64=551f6fc83ea457d62a0d98237cbad105af8d557003051f41f3e7ca7b3f2470eb
ARG GITLEAKS_SHA256_ARM64=e4a487ee7ccd7d3a7f7ec08657610aa3606637dab924210b3aee62570fb4b080
RUN set -eu; \
    arch="${TARGETARCH:-$(dpkg --print-architecture)}"; \
    case "$arch" in \
      amd64) asset=x64; sum="$GITLEAKS_SHA256_AMD64" ;; \
      arm64) asset=arm64; sum="$GITLEAKS_SHA256_ARM64" ;; \
      *) echo "no pinned gitleaks for architecture '$arch'" >&2; exit 1 ;; \
    esac; \
    curl --proto '=https' --tlsv1.2 -fsSL --connect-timeout 20 --retry 5 --retry-all-errors -o /tmp/gitleaks.tar.gz \
      "https://github.com/gitleaks/gitleaks/releases/download/v$GITLEAKS_VERSION/gitleaks_${GITLEAKS_VERSION}_linux_$asset.tar.gz"; \
    echo "$sum  /tmp/gitleaks.tar.gz" | sha256sum -c -; \
    tar -xzf /tmp/gitleaks.tar.gz -C /tmp gitleaks; \
    install -m 0755 /tmp/gitleaks /usr/local/bin/gitleaks; \
    rm -f /tmp/gitleaks.tar.gz /tmp/gitleaks; \
    gitleaks version

# helm: lints and renders the chart in charts/. Checksums: the .sha256sum file published next to
# each archive, https://github.com/helm/helm/releases (the archives are on get.helm.sh).
ARG HELM_VERSION=4.3.1
ARG HELM_SHA256_AMD64=0403a88ae20913d11b10a2467b50087721fea7581736da6b9d3992f5357fe54b
ARG HELM_SHA256_ARM64=8cf8f66ecbc252ac8f7f06cca68aba88508ddfde3a6a34feb1678ad1694b4f88
RUN set -eu; \
    arch="${TARGETARCH:-$(dpkg --print-architecture)}"; \
    case "$arch" in \
      amd64) sum="$HELM_SHA256_AMD64" ;; \
      arm64) sum="$HELM_SHA256_ARM64" ;; \
      *) echo "no pinned helm for architecture '$arch'" >&2; exit 1 ;; \
    esac; \
    curl --proto '=https' --tlsv1.2 -fsSL --connect-timeout 20 --retry 5 --retry-all-errors -o /tmp/helm.tar.gz \
      "https://get.helm.sh/helm-v$HELM_VERSION-linux-$arch.tar.gz"; \
    echo "$sum  /tmp/helm.tar.gz" | sha256sum -c -; \
    tar -xzf /tmp/helm.tar.gz -C /tmp "linux-$arch/helm"; \
    install -m 0755 "/tmp/linux-$arch/helm" /usr/local/bin/helm; \
    rm -rf /tmp/helm.tar.gz "/tmp/linux-$arch"; \
    helm version --short

# The checks run as an unprivileged user.
RUN groupadd --gid 10001 ci \
 && useradd --uid 10001 --gid 10001 --create-home --shell /usr/sbin/nologin ci \
 && install -d -o 10001 -g 10001 /home/ci/kausa
WORKDIR /home/ci/kausa
USER 10001:10001
# A toolchain that does not match rust-toolchain.toml fails the build instead of being downloaded.
ENV RUSTUP_AUTO_INSTALL=0

COPY --chown=10001:10001 . .

# The commands of "Verification (same as CI)" in AGENTS.md, in the same order; gitleaks adds only
# output flags (--no-banner --no-color --verbose). Each RUN is one check, so a failure names it.
#
# The order is a security control. clippy and the tests compile and run code from the crate and
# its dependencies (build scripts, procedural macros, tests), and each RUN keeps what that code
# writes. The base image leaves $CARGO_HOME (whose bin/ comes first in PATH) and $RUSTUP_HOME
# writable for every user, and cargo and the advisory checks write to $CARGO_HOME; the sources and
# target/ belong to the ci user. So that code could replace the tools of every later step, and the
# checks that run no crate code (secret scan, formatting, dependency policy, advisories, the
# chart) come first; nothing but the tests runs after clippy. Making the toolchain read-only would
# not protect the tests (that code can rewrite the sources and target/ they are built from) and
# would copy the whole toolchain into a new layer.
#
# The advisory checks download the RustSec database and cargo audit refreshes the crates.io
# index; cargo downloads crates once there are dependencies. gitleaks scans every commit reachable
# in .git; a shallow clone limits it to the commits it has.
RUN gitleaks git --no-banner --no-color --redact --verbose .
RUN cargo fmt --all -- --check
RUN cargo deny --locked check
RUN cargo audit
# The chart: lint, then render it with the values that change which templates and rules appear
# (docs/reference/helm-chart.md). Rendering runs no code from the chart's values. rbac.scope has no
# default (docs/adr/0013-namespaced-install-mode.md), so every lint and render sets it.
RUN helm lint --strict charts/kausa --set rbac.scope=cluster
# Cluster scope: v0.1's value sets. The default render keeps the ClusterRole and its binding and
# passes no --namespace.
RUN set -eu; \
    for values in \
      "--set rbac.scope=cluster" \
      "--set rbac.scope=cluster --set model.endpoint=https://api.example.com/v1 --set model.name=m --set model.apiKey.existingSecret=k" \
      "--set rbac.scope=cluster --set dryRun=true --set logs=false --set metrics.enabled=false" \
      "--set rbac.scope=cluster --set model.endpoint=http://m.example.svc:11434/v1 --set model.name=m --set model.allowHttp=true --set events=false --set networkPolicy.enabled=true --set networkPolicy.apiServer.cidrs[0]=192.0.2.1/32 --set networkPolicy.endpoint.to[0].ipBlock.cidr=192.0.2.2/32"; \
    do \
      helm template kausa charts/kausa --kube-version 1.36.0 $values > /dev/null; \
    done; \
    out=$(helm template kausa charts/kausa --kube-version 1.36.0 --set rbac.scope=cluster); \
    for kind in ClusterRole ClusterRoleBinding; do \
      [ "$(printf '%s\n' "$out" | grep -cx "kind: $kind")" -eq 1 ] \
        || { echo "rbac.scope=cluster: not exactly one $kind" >&2; exit 1; }; \
    done; \
    if printf '%s\n' "$out" | grep -qx ' *- --namespace'; then \
      echo "rbac.scope=cluster: passes --namespace" >&2; exit 1; \
    fi
# Namespaced scope, two namespaces: nothing cluster-scoped, one Role and one RoleBinding in each,
# and one --namespace per namespace.
RUN set -eu; \
    for values in \
      "--set model.endpoint=https://api.example.com/v1 --set model.name=m --set model.apiKey.existingSecret=k" \
      "--set dryRun=true --set logs=false"; \
    do \
      out=$(helm template kausa charts/kausa --kube-version 1.36.0 --set rbac.scope=namespaced --set 'rbac.namespaces={e2e-a,e2e-b}' $values); \
      if printf '%s\n' "$out" | grep -q '^kind: Cluster'; then \
        echo "rbac.scope=namespaced $values: renders a cluster-scoped kind" >&2; exit 1; \
      fi; \
      for kind in Role RoleBinding; do \
        [ "$(printf '%s\n' "$out" | grep -cx "kind: $kind")" -eq 2 ] \
          || { echo "rbac.scope=namespaced $values: not exactly 2 ${kind}s" >&2; exit 1; }; \
      done; \
      [ "$(printf '%s\n' "$out" | grep -cx ' *- --namespace')" -eq 2 ] \
        || { echo "rbac.scope=namespaced $values: not exactly 2 --namespace args" >&2; exit 1; }; \
    done
# The explanation history (docs/adr/0014-explanation-history.md), off by default. With
# history.enabled, an endpoint and no dryRun, kausa's ClusterRole or each Role gets the
# explanations rule last, the args get --history --history-days "<days>" before --metrics-address,
# and the chart installs the CustomResourceDefinition explanations.kausa.fedeam.com
# (history.installCRD) and a reader ClusterRole aggregated to view (history.readers.aggregate).
# kausa's ClusterRole is selected by name, since the reader is a second one. A chart whose
# values.yaml has no history map (helm upgrade --reuse-values from v0.2) renders like the chart.
# tests/chart/render.sh checks the same renders in more detail, and NOTES.txt.
RUN set -eu; \
    endpoint="--set model.endpoint=https://api.example.com/v1 --set model.name=m --set model.apiKey.existingSecret=k"; \
    no_events="--set model.endpoint=http://m.example.svc:11434/v1 --set model.name=m --set model.allowHttp=true --set events=false"; \
    on="--set history.enabled=true"; \
    cluster="--set rbac.scope=cluster"; \
    namespaced="--set rbac.scope=namespaced --set rbac.namespaces={e2e-a,e2e-b}"; \
    crd=CustomResourceDefinition/explanations.kausa.fedeam.com; \
    reader=ClusterRole/kausa-explanation-reader; \
    fail() { echo "history: $*" >&2; exit 1; }; \
    render_in() { chart=$1; shift; helm template kausa "$chart" --kube-version 1.36.0 --namespace kausa-system "$@" || fail "$chart does not render with $*"; }; \
    render() { render_in charts/kausa "$@"; }; \
    has() { printf '%s\n' "$1" | grep -qx -- "$2"; }; \
    rules() { printf '%s\n' "$1" | awk -v kind="$2" -v name="$3" 'function flush() { if (k && n) printf "%s", r; k = 0; n = 0; on = 0; r = "" }; /^---/ { flush(); next }; $0 == "kind: " kind { k = 1 }; $0 == "  name: " name { n = 1 }; /^[^ ]/ { on = ($0 == "rules:"); next }; on && NF { r = r $0 "\n" }; END { flush() }'; }; \
    scoped() { printf '%s\n' "$1" | awk 'function flush() { if (k != "" && !n) print k "/" m; k = ""; n = 0; m = "" }; /^---/ { flush(); next }; /^kind: / { k = substr($0, 7) }; /^[^ #]/ { meta = ($0 == "metadata:") }; meta && /^  namespace: / { n = 1 }; meta && /^  name: / { m = substr($0, 9) }; END { flush() }' | LC_ALL=C sort | paste -sd' ' -; }; \
    args() { printf '%s\n' "$1" | sed -n '/^ *args:$/,/^ *env:$/p' | sed 's/^ *//'; }; \
    plus_history() { awk -v days="$1" '$0 == "- --metrics-address" { print "- --history"; print "- --history-days"; print "- \"" days "\"" }; { print }'; }; \
    history_rule=$(printf '%s\n' '  - apiGroups: ["kausa.fedeam.com"]' '    resources: ["explanations"]' '    verbs: ["list", "create", "patch", "delete"]'); \
    nothing() { ! printf '%s\n' "$2" | grep -qE 'kausa\.fedeam\.com|CustomResourceDefinition|--history' || fail "$1: renders part of the history"; }; \
    on_in_cluster() { \
      name=$1; objects=$2; days=$3; base=$4; extra=$5; \
      off=$(render $cluster $base); \
      out=$(render $cluster $base $on $extra); \
      [ "$(scoped "$out")" = "$objects" ] || fail "$name: the cluster-scoped objects are '$(scoped "$out")', not '$objects'"; \
      [ "$(rules "$out" ClusterRole kausa)" = "$(rules "$off" ClusterRole kausa; echo "$history_rule")" ] || fail "$name: kausa's rules are not those without the history plus the explanations rule last"; \
      [ "$(args "$out")" = "$(args "$off" | plus_history "$days")" ] || fail "$name: not --history --history-days \"$days\" before --metrics-address"; \
    }; \
    for scope in "$cluster" "$namespaced"; do \
      for values in "" "$endpoint" "--set dryRun=true"; do \
        out=$(render $scope $values); nothing "the defaults with $scope $values" "$out"; \
      done; \
    done; \
    on_in_cluster "history on" "ClusterRole/kausa $reader ClusterRoleBinding/kausa $crd" 7 "$endpoint" ""; \
    printf '%s\n' "$out" | grep -q 'helm.sh/resource-policy' && fail "history on: the CRD has a resource policy"; \
    has "$out" ' *maxLength: 300' || fail "history on: no 'maxLength: 300' as an integer"; \
    has "$out" ' *- message: spec is immutable' || fail "history on: no CEL message 'spec is immutable'"; \
    [ "$(printf '%s\n' "$out" | sed -n 's/^ *jsonPath: //p' | paste -sd' ' -)" = ".spec.target.kind .spec.target.name .spec.rule .status.occurrences .status.lastTime .metadata.creationTimestamp .status.entries[0].confidence .status.entries[0].summary" ] || fail "history on: the printer columns"; \
    has "$out" '    rbac\.authorization\.k8s\.io/aggregate-to-view: "true"' || fail "history on: the reader is not aggregated to view"; \
    printf '%s\n' "$out" | grep -qE 'aggregate-to-(edit|admin)' && fail "history on: the reader is aggregated to edit or admin"; \
    [ "$(rules "$out" ClusterRole kausa-explanation-reader)" = "$(printf '%s\n' '  - apiGroups: ["kausa.fedeam.com"]' '    resources: ["explanations"]' '    verbs: ["get", "list", "watch"]')" ] || fail "history on: the reader's rules are not get, list and watch on explanations"; \
    on_in_cluster "history.days=30" "ClusterRole/kausa $reader ClusterRoleBinding/kausa $crd" 30 "$endpoint" "--set history.days=30"; \
    on_in_cluster "history.installCRD=false" "ClusterRole/kausa ClusterRoleBinding/kausa" 7 "$endpoint" "--set history.installCRD=false --set history.readers.aggregate=false"; \
    on_in_cluster "history.readers.aggregate=false" "ClusterRole/kausa ClusterRoleBinding/kausa $crd" 7 "$endpoint" "--set history.readers.aggregate=false"; \
    on_in_cluster "history with events=false" "ClusterRole/kausa $reader ClusterRoleBinding/kausa $crd" 7 "$no_events" ""; \
    has "$out" '    verbs: \["create", "patch"\]' && fail "history with events=false: kausa may write Events"; \
    out=$(render $cluster $endpoint $on --set dryRun=true); nothing "history with dryRun" "$out"; \
    out=$(render $cluster $on); nothing "history without an endpoint" "$out"; \
    clusterrules=$(rules "$(render $cluster $endpoint $on)" ClusterRole kausa); \
    out=$(render $namespaced $endpoint $on); \
    [ "$(scoped "$out")" = "$reader $crd" ] || fail "namespaced history: the cluster-scoped objects are '$(scoped "$out")'"; \
    [ "$(rules "$out" Role kausa)" = "$(printf '%s\n%s\n' "$clusterrules" "$clusterrules")" ] || fail "namespaced history: the Roles' rules are not the ClusterRole's"; \
    [ "$(args "$out")" = "$(args "$(render $namespaced $endpoint)" | plus_history 7)" ] || fail "namespaced history: not --history --history-days \"7\" before --metrics-address"; \
    out=$(render $namespaced $endpoint $on --set history.installCRD=false); \
    [ -z "$(scoped "$out")" ] || fail "namespaced history with installCRD=false: renders cluster-scoped objects"; \
    has "$out" '    verbs: \["list", "create", "patch", "delete"\]' || fail "namespaced history with installCRD=false: no explanations rule"; \
    cp -R charts/kausa /tmp/nomap; \
    awk '/^history:/ { skip = 1; next }; skip && /^[^ ]/ { skip = 0 }; !skip' charts/kausa/values.yaml > /tmp/nomap/values.yaml; \
    grep -q '^history:' /tmp/nomap/values.yaml && fail "the chart copy still has a history map"; \
    for values in "" "$endpoint" "$endpoint $on"; do \
      a=$(render $cluster $values); b=$(render_in /tmp/nomap $cluster $values); \
      [ "$a" = "$b" ] || fail "a chart without the history map renders unlike the chart with $values"; \
    done; \
    rm -rf /tmp/nomap; \
    for scope in "$cluster" "$namespaced"; do \
      helm lint --strict charts/kausa $scope $endpoint $on > /tmp/lint 2>&1 || { cat /tmp/lint >&2; fail "helm lint --strict with $scope fails with the history on"; }; \
    done; \
    rm -f /tmp/lint
# The refusals of kausa.validate: helm template fails with the exact message (helm lint does not
# fail on a template's fail). The default values and rbac=null (a v0.1 release's stored values have
# no rbac map) must reach the rbac.scope message, never a nil-pointer error. history.days, checked
# last, is refused with the history on and off.
RUN set -eu; \
    long=$(printf '%064d' 0 | tr 0 a); \
    names33=$(seq -f 'n%g' 1 33 | paste -sd, -); \
    not_set='rbac.scope is not set: set rbac.scope=cluster to let kausa read every namespace, or rbac.scope=namespaced with rbac.namespaces'; \
    not_a_name="is not a namespace name (lowercase letters, digits and '-', at most 63 characters)"; \
    refuse() { \
      message=$1; shift; \
      if helm template kausa charts/kausa --kube-version 1.36.0 "$@" > /dev/null 2> /tmp/refusal; then \
        echo "renders with $*; expected: $message" >&2; exit 1; \
      fi; \
      if grep -qi 'nil pointer' /tmp/refusal || ! grep -qF -- "$message" /tmp/refusal; then \
        echo "with $*: expected '$message', got:" >&2; cat /tmp/refusal >&2; exit 1; \
      fi; \
    }; \
    refuse "$not_set"; \
    refuse "$not_set" --set rbac=null; \
    refuse "rbac.scope must be cluster or namespaced, not 'all'" --set rbac.scope=all; \
    refuse "rbac.scope=namespaced needs rbac.namespaces: the namespaces kausa may read" --set rbac.scope=namespaced; \
    refuse "rbac.namespaces is set but rbac.scope is cluster, which grants every namespace: set rbac.scope=namespaced" --set rbac.scope=cluster --set 'rbac.namespaces={a}'; \
    refuse "rbac.namespaces: 'Shop' $not_a_name" --set rbac.scope=namespaced --set 'rbac.namespaces={Shop}'; \
    refuse "rbac.namespaces: '$long' $not_a_name" --set rbac.scope=namespaced --set "rbac.namespaces={$long}"; \
    refuse "rbac.namespaces: '-a' $not_a_name" --set rbac.scope=namespaced --set 'rbac.namespaces={-a}'; \
    refuse "rbac.namespaces: 'a' is listed twice" --set rbac.scope=namespaced --set 'rbac.namespaces={a,a}'; \
    refuse "rbac.namespaces lists 33 namespaces; kausa watches at most 32 (use rbac.scope=cluster instead)" --set rbac.scope=namespaced --set "rbac.namespaces={$names33}"; \
    for days in 0 31 abc 7.5; do \
      refuse "history.days must be a whole number of days from 1 to 30, not '$days'" --set rbac.scope=cluster --set "history.days=$days"; \
      refuse "history.days must be a whole number of days from 1 to 30, not '$days'" --set rbac.scope=cluster --set model.endpoint=https://api.example.com/v1 --set model.name=m --set model.apiKey.existingSecret=k --set history.enabled=true --set "history.days=$days"; \
    done; \
    rm -f /tmp/refusal
RUN cargo clippy --locked --all-targets --all-features -- -D warnings
RUN cargo test --locked --all-features

# The release binary, linked statically against musl, so the runtime image needs no libc. Built
# for the architecture of the build: a multi-architecture image is two native builds
# (the image job of .github/workflows/ci.yml, and the Gitea job for arm64), so no cross compiler is needed.
FROM rust:1.99.1-slim-trixie@sha256:1a939748a7cbab172de91a014e3804a97af1394a8c844faa67fce9401c4e40fb AS build

ARG TARGETARCH

# musl-gcc compiles ring's C and assembly for the musl target.
RUN set -eu; \
    apt-get update; \
    DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends musl-tools; \
    rm -rf /var/lib/apt/lists/*

RUN set -eu; \
    arch="${TARGETARCH:-$(dpkg --print-architecture)}"; \
    case "$arch" in \
      amd64) triple=x86_64-unknown-linux-musl ;; \
      arm64) triple=aarch64-unknown-linux-musl ;; \
      *) echo "no musl target for architecture '$arch'" >&2; exit 1 ;; \
    esac; \
    rustup target add "$triple"; \
    echo "$triple" > /etc/kausa-triple

ENV RUSTUP_AUTO_INSTALL=0 \
    CC_x86_64_unknown_linux_musl=musl-gcc \
    CC_aarch64_unknown_linux_musl=musl-gcc
WORKDIR /src
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY src ./src
RUN set -eu; \
    triple=$(cat /etc/kausa-triple); \
    cargo build --locked --release --target "$triple"; \
    install -m 0755 "target/$triple/release/kausa" /kausa

# The controller image. distroless static: CA certificates, /etc/passwd with the nonroot user,
# time zone data, and nothing else (no shell, no package manager). The digest is the
# multi-architecture index, so both architectures use the same pin.
FROM gcr.io/distroless/static-debian13:nonroot@sha256:18f4cc26c9ffa7f20a359298be7deadac5c607c8e03a02b8119f1a5fc2f82c99 AS runtime

LABEL org.opencontainers.image.title="kausa" \
      org.opencontainers.image.description="A tiny Kubernetes controller that explains failing workloads with an LLM" \
      org.opencontainers.image.source="https://github.com/fedeam/kausa" \
      org.opencontainers.image.licenses="MIT OR Apache-2.0"

COPY --from=build /kausa /kausa
# The nonroot user of distroless, by number, so that runAsNonRoot can check it.
USER 65532:65532
ENTRYPOINT ["/kausa"]
CMD ["run"]
