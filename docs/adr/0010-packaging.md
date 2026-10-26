# 0010. Ship a static binary on distroless, with a Helm chart, built in CI and not published yet

- Status: accepted
- Date: 2026-10-26

## Context

kausa has to run in its users' clusters, small ones included (the target is a Raspberry Pi 4), so
the image must be small, start fast and carry nothing an attacker could use. The Architecture
([Packaging](../explanation/architecture.md#packaging)) left two choices open for the third
implementation weekend: the base image (distroless static or scratch with CA certificates) and how
the image is built and installed. The competitive survey found the Rust controllers of the field at
around 10 MB compressed ([Competitive landscape](../research/2026-10-23-competitive-landscape.md)).
The project is not validated yet, and nothing is to be published before it is
([0006. Make the GitHub repository public only after a checklist](0006-publication-checklist.md)).

FedeAM chose on 2026-10-26: a Helm chart; distroless static, non-root; the image built in CI
only, not published; the end-to-end tests in kind on GitHub Actions.

## Alternatives

- **scratch with CA certificates copied in.** The smallest possible image, but kausa would need
  its own `/etc/passwd` entry for the non-root user and a CA bundle to keep up to date by hand.
  distroless static adds about 1 MB compressed and brings both, maintained and rebuilt upstream.
- **A glibc binary on distroless base or cc.** No musl, but a larger image with a libc to patch.
  kausa uses ring and rustls (no OpenSSL), so a static musl build has no C library to link but
  musl itself.
- **Cross-compiling both architectures on one runner,** with a musl cross toolchain or
  cargo-zigbuild. One more pinned tool and a C cross compiler for ring; a native build on each
  architecture needs neither, and GitHub's arm64 runners and the Gitea runner are native.
- **QEMU emulation in buildx.** Simple, but a release build under emulation takes many times
  longer, and the classic builder of the Gitea runner has no buildx.
- **Kustomize or plain manifests instead of Helm.** Fewer moving parts, but the permissions
  depend on the settings (no `pods/log` without log collection, no Event writes without an
  endpoint), and Helm is how most small clusters install third-party controllers.
- **Publishing the image now** (GHCR). Not until the project is validated and public.

## Decision

- **Image:** the Dockerfile's `runtime` target. A release build (LTO, one codegen unit, stripped,
  `panic = "abort"`) for the musl target of the build's architecture, on
  `gcr.io/distroless/static-debian13:nonroot`, pinned by the digest of its multi-architecture
  index, running as user and group 65532. The `ci` target comes first in the Dockerfile, so the
  classic builder's `--target ci` never compiles the release binary.
- **Architectures:** linux/amd64 and linux/arm64, each built natively: GitHub's `ubuntu-24.04` and
  `ubuntu-24.04-arm` runners, and the Gitea runner for arm64. Nothing is tagged for a registry or
  pushed; a multi-architecture manifest is made at release time, when publishing is decided.
- **Chart:** `charts/kausa`: a ServiceAccount, a ClusterRole that is the table in
  [Permissions](../reference/permissions.md) with `pods/log` and the Event writes only when the
  settings use them, a Deployment with one replica and `Recreate`, the restricted Pod Security
  settings, the API key as a file from the user's Secret, `POD_NAME` from the downward API, a
  metrics Service and an optional NetworkPolicy. Reference: [Helm chart](../reference/helm-chart.md).
- **Checks:** the `ci` target lints the chart with a pinned helm and renders it with the value
  sets that change it. The kind test (`tests/e2e/run.sh`, GitHub Actions) installs the chart under
  the restricted Pod Security Standard, with a fake model endpoint and two seeded failures, and
  checks the Events, that no real name reached the endpoint, the ClusterRole (with
  `kubectl auth can-i`) and the metrics.

## Consequences

- The image measured 4.3 MB compressed (9.6 MB uncompressed) for linux/arm64 on 2026-10-26, and
  the container used 4.4 MiB of memory in a live test on Docker Desktop (not yet measured on a
  Raspberry Pi 4: fourth weekend).
- No image exists in any registry: users build their own until a release
  ([Install kausa with Helm](../how-to/install-with-helm.md)).
- The distroless digest is bumped by hand, like the tools in the Dockerfile
  ([Maintain the repository](../how-to/maintain.md)): Dependabot's docker updates are grouped
  with the rust image.
- The Gitea CI builds the release binary on the Pi after the checks, which makes its run longer.
- The kind test does not run until GitHub Actions run for the account; until then the chart is
  checked by the lint, the renders, and a server-side dry run against a live cluster.
