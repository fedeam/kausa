# 2026-10-23 — Repository setup

Survey the competition, decide whether and how to build kausa, and set up its repository
(community files, documentation, decisions, CI) before the first implementation weekend.
Repositories touched: kausa.

## What was done

- **Competitive survey with refutation.** Four researchers covered k8sgpt and its operator,
  HolmesGPT, kagent and the rest of the 2025–2026 field, and the candidate angles (minimal binary,
  deterministic rules before the model, redaction, local models, Events or custom resource output,
  permissions, prompt injection, cost). For each report an independent fact-checker re-read the
  primary sources and tried to refute its claims and its gap hints; a final pass resolved the
  conflicts. Result: no single feature is new, and the honest differentiator is a combination
  ([Competitive landscape](../research/2026-10-23-competitive-landscape.md)). The key claims used
  in the README (k8sgpt's chart role, the operator's 30-second reconcile interval, HolmesGPT's
  memory request and role) were checked again in the sources afterwards.
- **Decisions by the maintainer**, asked one by one after the survey: build, narrowly; the name
  `kausa`; Rust with kube-rs and `MIT OR Apache-2.0`; everything in English; a v0.1 of four
  weekends.
- **Name availability**, checked without credentials: crates.io's sparse index, npm and PyPI
  answer 404 for `kausa`, and GitHub's repository search has no repository named exactly
  `kausa`.
- **Facts the design depends on**, checked in Kubernetes' source: an `events.k8s.io/v1` Event's
  note holds up to 1024 bytes and its reason up to 128 (`pkg/apis/core/validation/events.go`,
  which compares byte lengths although its message says characters), and the API server keeps
  Events for one hour by default (`--event-ttl`).
- **Documentation** in Diátaxis, with the research folder: the [documentation front
  page](../index.md), the planned [Architecture](../explanation/architecture.md), three reference
  pages ([Detection and redaction rules](../reference/rules.md), [Data sent to the
  provider](../reference/data-sent-to-the-provider.md) and [Permissions](../reference/permissions.md)),
  and three how-tos ([Set up a development environment and run the checks](../how-to/develop.md),
  [Maintain the repository](../how-to/maintain.md) and [Publish the repository on
  GitHub](../how-to/publish-the-repository.md)).
- **Community and agent files**, following the maintainer's earlier project kaution: `README.md`
  (the problem with an example, what leaves the cluster, design principles, an honest comparison
  with k8sgpt, HolmesGPT, kagent and kwatch, the roadmap), `AGENTS.md` with a one-line
  `CLAUDE.md`, `SECURITY.md` with a threat model of its own (who controls the inputs, what is
  protected, what kausa must do and what it cannot promise), `CONTRIBUTING.md` (detection and
  redaction rules with fixtures), `CHANGELOG.md` (Keep a Changelog 1.1.0), `CODE_OF_CONDUCT.md`
  (Contributor Covenant 2.1, contact `conduct@fedeam.com`), both license texts (checked against
  apache.org and SPDX), issue forms, a pull request template, `CODEOWNERS`, `dependabot.yml` with
  a three-day cooldown and `release.yml`.
- **Command-line skeleton:** `Cargo.toml` (`kausa` `0.1.0-dev`, edition 2024, no dependencies,
  `publish = false`, `unsafe_code` forbidden), `src/main.rs` (only `--help` and `--version`; exit
  code 2 on a usage or output error), `tests/cli.rs` and `rust-toolchain.toml` (Rust 1.99.1, the
  stable release in the channel manifest, dated 2026-10-15).
- **CI**, the same as kaution's: the `Dockerfile` `ci` target on `rust:1.99.1-slim-trixie`
  pinned by digest (read from Docker Hub's registry API; the index has linux/amd64 and
  linux/arm64), cargo-deny 0.20.2, cargo-audit 0.22.2 and gitleaks 8.30.1 checked by SHA-256 for
  each architecture (the checksum files of cargo-deny and gitleaks, and the SHA-256 of
  cargo-audit's downloaded archives, all compared on 2026-10-23), the checks as an unprivileged
  user in the safe order; `.gitea/workflows/ci.yml` (plain shell, the job token in an HTTP
  header, `REPO_CLONE_URL`, no `pull_request` trigger, a weekly run); `.github/workflows/ci.yml`
  (`actions/checkout` v7.0.1 pinned by commit SHA, checked with `git ls-remote`, and
  `contents: read`); `deny.toml`.
- **Gitea**, set up through its API: a private repository, with issues, pull requests, wiki and
  projects disabled and Actions on; `main` protected against force pushes (Gitea refuses to delete
  a protected branch); the Actions variable `REPO_CLONE_URL` set to the clone URL the runner uses.
  The same settings as kaution's, read from its repository first.
- **Remotes** in the local clone: `origin` fetches from Gitea and pushes to Gitea and GitHub;
  `github` fetches from GitHub and its push URL is disabled. No credentials in any URL.
- **Review before the first commit.** Four reviewers (the maintainer's standard, security and
  privacy, consistency across documents, CI) read every file, a skeptic per dimension tried to
  refute each finding, and the confirmed ones were applied: 45 confirmed, 9 rejected. Among them:
  a leftover reference to kaution's ADR file name in the Gitea workflow; claims copied from
  kaution as if observed on kausa; Event limits counted in bytes, not characters; logs of a failed
  Job's Pod come from the terminated container, not `previous=true`; `D003` takes precedence over
  `D001`; Unicode format characters stripped from answers; the shared budget and its reset on
  restart; the configuration ConfigMap mounted as a volume; the reach of `create` and `patch` on
  Events; and link texts that now match the titles of their targets.
- **CI build** (`docker build --target ci .`) run locally for linux/arm64 before the first commit:
  every check passed and the six command-line tests passed.

## Decisions

- Scope, positioning and roadmap → [0001. Build a small, read-only explainer that writes one
  redacted explanation per incident](../adr/0001-scope-and-positioning.md) (accepted).
- Rust with kube-rs and the dual license → [0002. Write kausa in Rust with kube-rs and license it
  MIT OR Apache-2.0](../adr/0002-rust-and-dual-license.md) (accepted).
- CI → [0003. Run CI in one pinned container build, on GitHub Actions and Gitea
  Actions](../adr/0003-ci-in-one-pinned-container-build.md) (accepted, reusing kaution's).
- English only → [0004. Write everything in English](../adr/0004-english-only.md) (accepted).
- No type prefix → [0005. Name the repository kausa, without a type
  prefix](../adr/0005-name-without-prefix.md) (accepted).
- Gated publication → [0006. Make the GitHub repository public only after a
  checklist](../adr/0006-publication-checklist.md) (accepted, reusing kaution's, plus a repeated
  survey and a check of the fixtures).
- Contributions and merges on GitHub, Gitea a synced copy with the arm64 CI, Dependabot →
  [0007. Take contributions and merge pull requests on
  GitHub](../adr/0007-contributions-and-merges-on-github.md) (accepted, reusing kaution's).
- What leaves the cluster → [0008. Send only allowlisted, redacted data to one OpenAI-compatible
  endpoint](../adr/0008-data-leaving-the-cluster.md) (proposed).
- Permissions → [0009. Run with read-only permissions that never include
  Secrets](../adr/0009-least-privilege-rbac.md) (proposed).
- Rule ids: `D` for detection, `R` for redaction. No ADR: the catalogue is a proposal that may
  change until v0.1 is released.

## Pending

- **Proposed ADRs:** the maintainer approves or changes 0008 and 0009, in particular whether log
  collection is on by default for crash-type detections.
- **First CI runs** on Gitea (linux/arm64) and, once GitHub Actions run for the account, on GitHub
  (linux/amd64); confirm the name of the `ci` check run on the first GitHub run.
- **The GitHub repository:** create it, private, with squash merges only (title and message from
  the pull request), the labels, Dependabot alerts, the description and the topics, and record
  them here or in the journal of that day.
- **Hypotheses to verify on the first weekend:** a field selector on `type` for `events.k8s.io/v1`
  Events; that a failed Job can be detected from events and Pod status without watching Jobs;
  the event reasons in [Detection and redaction rules](../reference/rules.md); that memory stays
  flat with the number of healthy Pods when watches keep small records.
- **Hypotheses to verify on later weekends:** which OpenAI-compatible servers honour
  `response_format` with a JSON schema, and which need `max_completion_tokens` instead of
  `max_tokens`; whether the kind tests can run on the arm64 runner; the
  footprint targets.
- **Upstream:** two small fixes to k8sgpt found in the survey (its answer cache likely misses when
  anonymization is on, and the `--anonymize` help text is out of date), each to be reproduced
  before proposing it.
- **First merge on GitHub:** confirm that the squash commit's header is the pull request title
  followed by ` (#<number>)`.
- **Publication:** after validation, follow [Publish the repository on
  GitHub](../how-to/publish-the-repository.md).
