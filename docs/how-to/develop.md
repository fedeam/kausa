# Set up a development environment and run the checks

Install the toolchain, build kausa and run the same checks as CI before proposing a change.
Why CI is built this way, and where it runs for pull requests and for `main`: [0003. Run CI in
one pinned container build, on GitHub Actions and Gitea
Actions](../adr/0003-ci-in-one-pinned-container-build.md).

Status as of 2026-10-27: the four implementation weekends of v0.1 are done. The unit tests cover the rules, grouping, fingerprints, owner correlation, redaction,
the context, the prompt, the validation of answers, the budget, the Events, the metrics and the
HTTP client (against a local test server) without a cluster. The end-to-end test in kind
(`tests/e2e/run.sh`) passed on the maintainer's machine (Docker Desktop, arm64) on 2026-10-27; on
GitHub Actions it waits until they start jobs again (the account's included minutes for private
repositories are used up until the 1st; public repositories do not use them).

## Before you start

- **Rust** through [rustup](https://rustup.rs). `rust-toolchain.toml` pins the Rust release, with
  clippy and rustfmt, the same release as the CI image; rustup installs it the first time you run
  `cargo` in the checkout. This page does not repeat the release, so that Dependabot's update of
  `rust-toolchain.toml` leaves nothing behind.
- **cargo-deny** and **cargo-audit**, for the dependency policy and the RustSec advisories.
- **gitleaks**, for the secret scan.
- **Docker** (BuildKit or the classic builder), for the CI target and the image.
- **helm**, for the chart's lint (the CI image pins 4.3.1). For the end-to-end test only:
  **kind** (the version and node image CI pins are in `.github/workflows/e2e.yml`), and
  **kubectl** and **python3** (CI uses the ones preinstalled on GitHub's `ubuntu-24.04` runner).

Versions on the maintainer's machine, verified on 2026-10-23: cargo-deny 0.20.2, cargo-audit
0.22.2, gitleaks 8.30.1 and Docker 29.9.2 on macOS (arm64), the same tool versions as the CI image;
the maintainer's Rust comes from Homebrew, one patch release behind `rust-toolchain.toml`, so the CI build is
the check that uses the pinned compiler. Install cargo tools with `--locked` so they build with the
dependency versions their authors tested:

```sh
cargo install --locked cargo-deny@0.20.2 cargo-audit@0.22.2
```

## Steps

1. Clone the repository from GitHub. To propose a change, fork it on GitHub first and clone your
   fork instead:

   ```sh
   git clone https://github.com/fedeam/kausa.git
   cd kausa
   ```

2. Build:

   ```sh
   cargo build --locked
   ```

3. Run the checks, the same set and order as CI, as listed in
   [Verification (same as CI)](../../AGENTS.md#verification-same-as-ci):

   ```sh
   gitleaks git --redact .
   cargo fmt --all -- --check
   cargo deny --locked check
   cargo audit
   helm lint --strict charts/kausa
   cargo clippy --locked --all-targets --all-features -- -D warnings
   cargo test --locked --all-features
   ```

4. Run the CI build, which repeats the checks inside the pinned CI image on Linux:

   ```sh
   docker build --target ci .
   ```

   On an arm64 Mac this builds for linux/arm64, the same platform as the Gitea Actions runner
   that checks `main`. GitHub's hosted runners, which check pull requests, are linux/amd64; to
   reproduce them, add `--platform linux/amd64` (emulated, and much slower).

5. Build the controller image, for your machine's architecture:

   ```sh
   docker build --target runtime -t kausa:dev .
   docker run --rm kausa:dev --version
   ```

6. Optionally, run the end-to-end test. It creates a kind cluster named `kausa-e2e` (and deletes
   it at the end), installs the chart with the image, runs a fake model endpoint on your machine,
   and checks the Events, what the endpoint received, the ClusterRole and the metrics:

   ```sh
   KUBECONFIG=$(mktemp) KIND_NODE_IMAGE=<the node image pinned in .github/workflows/e2e.yml> \
     tests/e2e/run.sh
   ```

   It ends with `e2e: ok`. The fake endpoint runs on your machine. On Linux the cluster reaches it
   through the kind network's gateway; on Docker Desktop, whose gateway belongs to Docker's VM,
   the script uses the address the nodes give `host.docker.internal` instead. The Docker Desktop
   path passed on 2026-10-27; the Linux one waits for GitHub's runners. kind writes its context to
   the kubeconfig and makes it the current one: to leave yours as it is, run the test with
   `KUBECONFIG` pointing at a file of its own.

## Verify

Every command in steps 2 to 5 exits with code `0`. `gitleaks` ends with `no leaks found`.

## If something fails

- **`cargo fmt` reports a diff** → the code is not formatted → run `cargo fmt --all` and commit
  the result.
- **`cargo clippy` fails on a warning** → CI treats warnings as errors → fix the code. An
  `#[allow(...)]` needs a comment that says why.
- **`cargo deny --locked check` rejects a license, a source or a banned crate** → a new or
  updated dependency does not fit the policy in `deny.toml` → explain the dependency in the pull
  request before touching the policy.
- **`cargo audit` or `cargo deny check advisories` cannot fetch the advisory database** → both
  download the RustSec advisory database, and `cargo audit` also refreshes the crates.io index;
  that is the only network access of the checks themselves until there are dependencies, which
  cargo downloads (the image build also downloads the base image, Debian packages and the pinned
  tools) → work offline with the cached copy (`cargo audit --no-fetch`,
  `cargo deny --offline check`), and run the full check before pushing.
- **`gitleaks` finds something** → do not commit it. If it is a real secret, remove it and rotate
  it at its source, because it may already be in the history. If it is a false positive in a
  fixture, replace the value with an obviously fake one (`example.com`, `192.0.2.0/24`, AWS
  account `123456789012`) instead of allowlisting it.
- **`docker build --target ci .` passes locally but fails in CI** → check the platform (amd64 on
  GitHub, arm64 on Gitea) and that nothing in the build relies on files outside the build
  context.
