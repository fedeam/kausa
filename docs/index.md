# kausa — a tiny Kubernetes controller that explains failing workloads with an LLM

kausa is a Kubernetes controller for people who run small clusters and want to know why a workload
fails: it watches Warning events and Pod status, groups the failures of one workload into one
incident, sends a redacted context to one OpenAI-compatible model endpoint within a call and token
budget, and writes the explanation as an Event on the failing object. It never reads Secrets or
ConfigMaps, never changes a workload, and writes nothing but its own Events (none with
`--no-events`).

Status as of 2026-10-28: **in development** (pre-alpha). The four implementation weekends of v0.1
are done: `kausa run` watches Pods and Warning events, groups failures by
workload, redacts their context, explains them with an OpenAI-compatible model within a call
budget (or logs the request with `--dry-run`) and writes each explanation as an Event on the
failing workload. The image and the Helm chart exist, the kind test passes, and the first
release, [v0.1.0](https://github.com/fedeam/kausa/releases/tag/v0.1.0), came out on 2026-10-28
([2026-10-28 — First release](journal/2026-10-28-first-release.md)). The
repository is public on GitHub, with a private copy on the maintainer's Gitea instance, and
contributions and merges happen on GitHub ([0007. Take contributions and merge pull requests on
GitHub](adr/0007-contributions-and-merges-on-github.md)). The CI on Gitea passes since 2026-10-26
([2026-10-26 — GitHub repository and first CI
runs](journal/2026-10-26-github-repository-and-first-ci.md)). GitHub Actions run since the repository became
public, on 2026-10-27: the checks, both images and the kind test passed on their first run.

## Key facts

- **What it is:** a Rust controller on kube-rs, one static binary for linux/amd64 and
  linux/arm64, in a distroless non-root image published at `ghcr.io/fedeam/kausa` with a Helm
  chart in `charts/kausa`, attached to each GitHub release.
- **URL:** none: kausa runs in its users' clusters.
- **Repository:** [`fedeam/kausa`](https://github.com/fedeam/kausa) on GitHub, public since
  2026-10-27. A private copy on the maintainer's Gitea instance, synced by
  the maintainer after each merge, runs the arm64 CI; its issues and pull requests are disabled.
- **Contributions:** on GitHub: issues, pull requests and reviews, the maintainer's own changes
  included. Pull requests are squash-merged, and the pull request title, a Conventional Commit
  with a scope, becomes the commit header on `main`, with GitHub's ` (#<number>)` after it
  ([Contributing to kausa](../CONTRIBUTING.md)).
- **Maintainer:** FedeAM (GitHub: `fedeam`).
- **License:** `MIT OR Apache-2.0` ([`LICENSE-MIT`](../LICENSE-MIT),
  [`LICENSE-APACHE`](../LICENSE-APACHE)).
- **CI:** `docker build --target ci .` on two forges, then the image (`--target runtime`).
  GitHub Actions (linux/amd64, plus an arm64 image build and the kind test) checks every pull
  request; Gitea Actions (linux/arm64) checks `main` after each
  sync and once a week.
- **Dependency updates:** Dependabot on GitHub, weekly, for Cargo, GitHub Actions, and the
  `rust` image together with `rust-toolchain.toml`; the tools pinned in the `Dockerfile` and the
  e2e workflow, and the distroless digest, by hand ([Maintain the repository](how-to/maintain.md)).
- **Changelog:** [`CHANGELOG.md`](../CHANGELOG.md), in the Keep a Changelog format.
- **Deploy:** none from this repository; users install kausa in their clusters.
- **Secrets:** none needed to build or test. To run against a hosted provider, its API key, in a
  Kubernetes Secret of the user's that the kubelet passes to the container. CI uses the automatic
  job token.
- **Language:** everything in the repository is in English.

## How to run it

To build kausa, run it against the cluster of the kubeconfig's current context (read-only: it
logs incidents), and run the same checks as CI:

```sh
cargo build --locked
cargo run --locked -- run
docker build --target ci .
```

Flags, exit codes and log lines: [Command line and log format](reference/command-line.md).

The full list of checks and the tools they need: [Set up a development environment and run the
checks](how-to/develop.md).

## Start here

- [0001. Build a small, read-only explainer that writes one redacted explanation per
  incident](adr/0001-scope-and-positioning.md): why kausa exists, what is in and out of scope,
  and the roadmap.
- [Competitive landscape](research/2026-10-23-competitive-landscape.md): k8sgpt, HolmesGPT,
  kagent and the others, and the gap that is left; [Tokens per incident compared with
  k8sgpt](research/2026-10-26-tokens-compared-with-k8sgpt.md): the same five failures, the same
  model, calls and tokens counted; [Memory and CPU on a Raspberry Pi
  4](research/2026-10-26-resource-use-on-a-raspberry-pi.md); [Thousands of
  Pods](research/2026-10-28-thousands-of-pods.md).
- [ARCHITECTURE.md](../ARCHITECTURE.md): the map of the code, its invariants and the path of each
  kind of change.
- [Architecture](explanation/architecture.md): the planned pipeline, the model call, cost
  controls, the Event output and the safety model.
- [Data sent to the provider](reference/data-sent-to-the-provider.md) and
  [Permissions](reference/permissions.md): what leaves the cluster and what kausa may read.
- [Try kausa against a cluster with a local model](how-to/try-with-a-local-model.md): a
  read-only run with Ollama.
- [Add a detection or redaction rule](how-to/add-a-rule.md): the files, tests and docs to change.
- [Install kausa with Helm](how-to/install-with-helm.md) and the [Helm
  chart](reference/helm-chart.md) values; [0010. Ship a static binary on distroless, with a Helm
  chart, built in CI and not published yet](adr/0010-packaging.md); [0011. Scan the code with
  CodeQL's default setup](adr/0011-code-scanning-with-codeql.md); [0012. Release from a tag: a
  signed image on GHCR and the chart on the GitHub release](adr/0012-releases.md). Accepted for
  v0.2: [0013. Offer a namespaced install mode: one watch pair and one Role per listed namespace,
  and nothing cluster-wide](adr/0013-namespaced-install-mode.md), [0014. Keep each incident's full
  explanations in a namespaced Explanation resource, off by
  default](adr/0014-explanation-history.md) and [0015. Post each new explanation to one optional
  webhook, as JSON, Slack or Telegram](adr/0015-notifications.md).
  [0016. Send each Pod's status reason and message to the model, redacted and
  bounded](adr/0016-send-the-pod-status-reason-and-message.md) comes with the v0.2 detection rules.
- [Maintain the repository](how-to/maintain.md): triage, merges on GitHub, the sync to Gitea,
  the pinned tools.
- [2026-10-23 — Repository setup](journal/2026-10-23-repository-setup.md): what was set up first
  and what is pending; [2026-10-26 — GitHub repository and first CI
  runs](journal/2026-10-26-github-repository-and-first-ci.md): the first push and CI runs;
  [2026-10-27 — First run of the kind test](journal/2026-10-27-first-kind-run.md);
  [2026-10-27 — Publication](journal/2026-10-27-publication.md);
  [2026-10-28 — First release](journal/2026-10-28-first-release.md).
