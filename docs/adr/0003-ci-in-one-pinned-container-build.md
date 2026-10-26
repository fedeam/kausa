# 0003. Run CI in one pinned container build, on GitHub Actions and Gitea Actions

- Status: accepted
- Date: 2026-10-23

## Context

Context reconstructed from the maintainer's earlier project kaution, whose ADRs 0003 and 0007
(2026-10-23) made the same decisions after its first CI runs; they are written here directly in
their final form.

- **Two forges.** Contributions and merges happen on GitHub ([0007. Take contributions and merge
  pull requests on GitHub](0007-contributions-and-merges-on-github.md)). The maintainer's private
  Gitea instance holds a copy of `main`, synced after each merge, and has a self-hosted Gitea
  Actions runner.
- **The runners.** GitHub's hosted runners are linux/amd64. The Gitea Actions runner (act_runner,
  label `native`) runs on Raspberry Pi 4 nodes: linux/arm64, with git and Docker's classic builder
  (no buildx, so the `Dockerfile` avoids BuildKit-only features), and a disk shared between jobs.
  kausa targets arm64 clusters, so the arm64 build matters.
- **GitHub Actions may not run.** Status as of 2026-10-23: GitHub does not start jobs for the
  account's private repositories, because October's included minutes were used up by another
  repository's macOS CI; they renew on the 1st of each month, and public repositories on
  GitHub-hosted runners do not use them. No payment is involved.
- **Everything in the repository becomes public**, workflow files included. The Gitea hostname,
  private addresses and the maintainer's usernames on his own systems must not appear in any
  file.
- **The Gitea job token.** On Gitea 1.23 the automatic token of an Actions job can read and write
  the repository in every run except pull requests from forks, and a workflow cannot narrow it
  (Gitea's source at v1.23.8, `routers/web/repo/githttp.go`, as checked for kaution).
- **Supply chain.** CI runs code from dependencies (build scripts, procedural macros, test
  tooling). Floating versions would make a green build today mean nothing tomorrow.

## Decision

Every check runs inside the `ci` target of the `Dockerfile`, built from pinned inputs, and both
forges run the same `docker build --target ci .`: GitHub Actions (linux/amd64) for every pull
request and for `main`, Gitea Actions (linux/arm64) for `main` after each sync and once a week.
FedeAM decided this on 2026-10-23, reusing kaution's setup.

- **One build definition.** Secret scan, formatting, dependency policy, advisories, lints and
  tests run in the image, so the Gitea job, the GitHub job and a developer machine run the same
  thing ([Set up a development environment and run the checks](../how-to/develop.md)).
- **Check order.** The checks that run no code from the crate or its dependencies (secret scan,
  formatting, dependency policy, advisories) run before clippy and the tests, which compile and
  run build scripts, procedural macros and tests. That code could replace the tools in the image
  for every later step, so nothing but the tests runs after clippy.
- **No `uses:` in the Gitea workflow.** The job is plain shell, git and docker on the runner: each
  Marketplace action would be code fetched from outside at run time, on a self-hosted runner.
- **No `pull_request` trigger on Gitea.** Contributors' pull requests and Dependabot's branches
  never reach Gitea, so the self-hosted runner builds outside code only after it is merged on
  GitHub and synced. The maintainer's own branches are pushed to Gitea too, and Gitea reads the
  workflow from the pushed commit: a branch of his that changes `.gitea/workflows/ci.yml` runs on
  the runner, so he reviews such a change before pushing it ([Maintain the
  repository](../how-to/maintain.md#open-a-pull-request-of-your-own)).
- **The automatic job token** fetches the commit. It goes in an HTTP header through the
  environment (`http.extraHeader`), never in argv or `.git/config`, and is removed from the
  environment before `docker build`. No repository secret is configured.
- **The clone URL is the repository variable `REPO_CLONE_URL`**, set in Gitea, not written in the
  workflow, so the public file carries no internal hostname. Gitea 1.23 rejects variable names
  that start with `CI`.
- **The runner's shared disk.** Each run works in its own temporary directory and removes it on
  exit; each run first removes the directories that killed runs left behind (older than an hour).
  `docker build --force-rm` keeps a failed step's container from staying on the classic builder.
- **On GitHub:** `actions/checkout` pinned by full commit SHA, `persist-credentials: false`, full
  history for the secret scan, and read-only permissions (`contents: read`).
- **Pinned inputs:** the base image by `@sha256:` digest, actions by full commit SHA, the Rust
  toolchain by exact version (`rust-toolchain.toml` and the image tag), the downloaded tools by
  SHA-256 for each architecture, and crates by `Cargo.lock` and `--locked`. Digests, SHAs and
  checksums come from the official source and are verified, never typed from memory. The
  exceptions, on purpose:
  - **Debian packages** (`ca-certificates`, `curl`, `git`) come from the suite of the pinned base
    image. apt verifies their signatures, but their versions float with the Debian archive.
  - **clippy and rustfmt** are installed by `rustup component add`, which checks them against the
    hashes in the Rust release manifest.
  - **cargo-audit** publishes no checksum file, so it is checked against the SHA-256 of its release
    assets (the digests GitHub reports, which the maintainer also computed from the downloaded
    archives on 2026-10-23). cargo-deny and gitleaks are checked against the checksum files their
    releases publish.
  - **The RustSec advisory database** is fetched live on every run, and `cargo audit` refreshes
    the crates.io index: advisories are only useful when current.
- **Dependency policy and advisories:** `cargo deny --locked check` and `cargo audit` on every
  build, and the weekly scheduled run on Gitea, so that a new advisory about a dependency already
  on `main` shows up without waiting for a push.
- **Secret scan:** `gitleaks` over the full history on every build.

## Consequences

- Once GitHub Actions run, the `ci` check on GitHub is the pull request gate; the arm64 build on
  Gitea checks `main` after the merge, not before it, so an arm64-only failure is fixed by a new
  pull request. Until GitHub Actions run, the maintainer builds pull requests locally, as
  [Maintain the repository](../how-to/maintain.md) describes.
- The Gitea job needs `REPO_CLONE_URL` before its first run.
- Builds on a Raspberry Pi 4 are slow. The Docker layer cache keeps the base image and the pinned
  tools; kausa and its dependencies are recompiled on every run, because the build context
  includes `.git` and `COPY . .` comes before every cargo step. A dependency-caching layer can be
  added when there are dependencies.
- The Gitea token can write to the repository during the job, and Gitea 1.23 cannot narrow it.
  Accepted: it is used only for the fetch, never written to disk, and removed before any code from
  the repository runs.
- The code of a merged change runs inside `docker build` on the self-hosted runner with Docker's
  default network. The container is not a network boundary; the barrier is that only reviewed,
  merged code reaches that runner. Running the checks without network access is a follow-up.
- The end-to-end tests in a kind cluster (third weekend) need a Docker daemon that can run kind;
  whether they run on both forges or only on GitHub is decided then.
- Pinning makes every update an explicit, reviewed pull request.
