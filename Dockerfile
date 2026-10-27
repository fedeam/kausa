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
# (docs/reference/helm-chart.md). Rendering runs no code from the chart's values.
RUN helm lint --strict charts/kausa
RUN set -eu; \
    for values in \
      "" \
      "--set model.endpoint=https://api.example.com/v1 --set model.name=m --set model.apiKey.existingSecret=k" \
      "--set dryRun=true --set logs=false --set metrics.enabled=false" \
      "--set model.endpoint=http://m.example.svc:11434/v1 --set model.name=m --set model.allowHttp=true --set events=false --set networkPolicy.enabled=true --set networkPolicy.apiServer.cidrs[0]=192.0.2.1/32 --set networkPolicy.endpoint.to[0].ipBlock.cidr=192.0.2.2/32"; \
    do \
      helm template kausa charts/kausa --kube-version 1.36.0 $values > /dev/null; \
    done
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
