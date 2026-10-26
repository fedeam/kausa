# 0002. Write kausa in Rust with kube-rs and license it MIT OR Apache-2.0

- Status: accepted
- Date: 2026-10-23

## Context

kausa runs inside other people's clusters, often small ones: single-node k3s, edge devices and
Raspberry Pi boards on linux/arm64. Its value rests partly on footprint (image size, memory and
CPU) and on being safe with untrusted input: event messages and log lines that anyone who can run
a Pod controls ([0001. Build a small, read-only explainer that writes one redacted explanation
per incident](0001-scope-and-positioning.md)).

What the competitive survey found on 2026-10-23
([Competitive landscape](../research/2026-10-23-competitive-landscape.md)):

- kube-rs (`kube` 4.2.0, released 2026-07-22, a CNCF Sandbox project) includes a controller
  runtime and watchers.
- Static Rust controllers built on it publish images of about 5.6 to 11 MB compressed (for
  example the kube-rs example controller at 6.6 MB, amd64 only, and linkerd's policy controller at
  10.4 MB for arm64), against 30 to 70 MB for the Go controllers surveyed and about 280 MB for the
  Python one.
- Nothing in the survey argued against Rust. Its costs: a smaller pool of Kubernetes contributors
  than Go, no reuse of k8sgpt's Go analyzers, and cross-compiling for arm64 is up to the project.

## Alternatives considered

- **Go with controller-runtime:** the ecosystem's default, more potential contributors and the
  option to borrow from k8sgpt. Images and memory are larger, and the footprint is part of the
  positioning. The maintainer kept Rust, as in their earlier project kaution.

## Decision

kausa is written in Rust, on kube-rs, and licensed `MIT OR Apache-2.0`. FedeAM took these
decisions on 2026-10-23.

- **Rust and kube-rs.** One static binary per platform, a small image, predictable memory without
  a garbage collector, and memory safety for a parser of untrusted text.
- **License `MIT OR Apache-2.0`**, with the two texts in `LICENSE-MIT` and `LICENSE-APACHE`: the
  usual choice in the Rust ecosystem, so users and downstream crates can pick either.
- **Dependencies arrive with the code that needs them**, starting with the first implementation
  weekend; the repository setup has none. A plain HTTP client is preferred over a provider SDK for
  the OpenAI-compatible API (the survey found `async-openai` at 0.42.2, pre-1.0), so the request
  kausa sends stays small and auditable.

## Consequences

- Rust adds a compile step and a toolchain to pin; CI builds in a container image pinned by digest
  ([0003. Run CI in one pinned container build, on GitHub Actions and Gitea
  Actions](0003-ci-in-one-pinned-container-build.md)).
- Every dependency has to be compatible with distributing kausa under `MIT OR Apache-2.0`; the
  dependency policy in `deny.toml` checks the licenses of the whole crate graph.
- The arm64 image needs a cross-compilation or per-architecture build, decided on the third
  weekend.
- Memory stays small only if kausa avoids caching whole objects cluster-wide: the watches keep a
  small record per Pod instead of a full reflector ([Architecture](../explanation/architecture.md)).
- Contributions are licensed inbound as the project is licensed outbound, with no CLA and no DCO
  sign-off ([Contributing to kausa](../../CONTRIBUTING.md)).
