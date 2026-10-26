# kausa — a tiny, read-only Kubernetes controller that explains failing workloads with an LLM

kausa is a Kubernetes controller for people who run small clusters and want to know why a workload
fails: it watches Warning events and Pod status, groups the failures of one workload into one
incident, sends a redacted context to one OpenAI-compatible model endpoint within a call and token
budget, and writes the explanation as an Event on the failing object. It never reads Secrets or
ConfigMaps and never changes a workload.

Status as of 2026-10-26: **in development** (pre-alpha). Only a command-line skeleton
(`kausa --help`, `kausa --version`) exists; nothing is watched, explained or released yet. The
private repositories exist on GitHub and on the maintainer's Gitea instance, and contributions and
merges happen on GitHub ([0007. Take contributions and merge pull requests on
GitHub](adr/0007-contributions-and-merges-on-github.md)). The CI on Gitea passes since 2026-10-26
([2026-10-26 — GitHub repository and first CI
runs](journal/2026-10-26-github-repository-and-first-ci.md)). Pending: GitHub Actions, which do
not start jobs while the repository is private (the account's included minutes for October are
used up; public repositories do not use them); the first Dependabot run; the four implementation
weekends of v0.1; and publication of the GitHub repository after validation.

## Key facts

- **What it is:** a Rust controller on kube-rs, one static binary for linux/amd64 and
  linux/arm64, planned to ship as a minimal non-root image with a Helm chart.
- **URL:** none: kausa runs in its users' clusters.
- **Repository:** [`fedeam/kausa`](https://github.com/fedeam/kausa) on GitHub, private until the
  project is validated, then public. A private copy on the maintainer's Gitea instance, synced by
  the maintainer after each merge, runs the arm64 CI; its issues and pull requests are disabled.
- **Contributions:** on GitHub: issues, pull requests and reviews, the maintainer's own changes
  included. Pull requests are squash-merged, and the pull request title, a Conventional Commit
  with a scope, becomes the commit header on `main`, with GitHub's ` (#<number>)` after it
  ([Contributing to kausa](../CONTRIBUTING.md)).
- **Maintainer:** FedeAM (GitHub: `fedeam`).
- **License:** `MIT OR Apache-2.0` ([`LICENSE-MIT`](../LICENSE-MIT),
  [`LICENSE-APACHE`](../LICENSE-APACHE)).
- **CI:** `docker build --target ci .` on two forges. GitHub Actions (linux/amd64) checks every
  pull request once Actions run for the account; Gitea Actions (linux/arm64) checks `main` after
  each sync and once a week.
- **Dependency updates:** Dependabot on GitHub, weekly, for Cargo, GitHub Actions, and the
  `rust` image together with `rust-toolchain.toml`; the three tools pinned in the `Dockerfile` by
  hand ([Maintain the repository](how-to/maintain.md)).
- **Changelog:** [`CHANGELOG.md`](../CHANGELOG.md), in the Keep a Changelog format.
- **Deploy:** none from this repository; users install kausa in their clusters.
- **Secrets:** none needed to build or test. To run against a hosted provider, its API key, in a
  Kubernetes Secret of the user's that the kubelet passes to the container. CI uses the automatic
  job token.
- **Language:** everything in the repository is in English.

## How to run it

Only `--help` and `--version` work yet. To build and run the skeleton, and to run the same checks
as CI:

```sh
cargo build --locked
cargo run --locked -- --help
docker build --target ci .
```

The full list of checks and the tools they need: [Set up a development environment and run the
checks](how-to/develop.md).

## Start here

- [0001. Build a small, read-only explainer that writes one redacted explanation per
  incident](adr/0001-scope-and-positioning.md): why kausa exists, what is in and out of scope,
  and the roadmap.
- [Competitive landscape](research/2026-10-23-competitive-landscape.md): k8sgpt, HolmesGPT,
  kagent and the others, and the gap that is left.
- [Architecture](explanation/architecture.md): the planned pipeline, the model call, cost
  controls, the Event output and the safety model.
- [Data sent to the provider](reference/data-sent-to-the-provider.md) and
  [Permissions](reference/permissions.md): what leaves the cluster and what kausa may read.
- [Maintain the repository](how-to/maintain.md): triage, merges on GitHub, the sync to Gitea,
  the pinned tools.
- [2026-10-23 — Repository setup](journal/2026-10-23-repository-setup.md): what was set up first
  and what is pending; [2026-10-26 — GitHub repository and first CI
  runs](journal/2026-10-26-github-repository-and-first-ci.md): the first push and CI runs.
