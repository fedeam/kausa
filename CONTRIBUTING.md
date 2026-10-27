# Contributing to kausa

Thanks for your interest in kausa. The project is pre-alpha: `kausa run` detects and groups
failures and explains them, and there is no published image or chart yet (you can build the
image and install the chart from the repository: [Install kausa with
Helm](docs/how-to/install-with-helm.md)). Issues and ideas are welcome; for code, open an issue first
so we agree on the approach before you spend time on it.

## Ground rules

- Read [the documentation front page](docs/index.md) and the decisions in
  [`docs/adr/`](docs/adr/). A new feature needs an accepted ADR before the code.
- Everything is written in English: code, comments, docs, commits, issues and pull requests.
- Everyone taking part follows the [Contributor Covenant Code of Conduct](CODE_OF_CONDUCT.md).
- Security vulnerabilities go through the [Security policy](SECURITY.md), never a public issue.
  That includes a value kausa should have redacted and did not.
- Never paste real cluster output (logs, events, names, addresses) in an issue, a pull request or
  a fixture: reduce it to an invented example first.

## Build and test

You need Rust through [rustup](https://rustup.rs), which installs the toolchain pinned in
`rust-toolchain.toml`. For the full check you also need `cargo-deny`, `cargo-audit`, `gitleaks`,
`helm` and Docker; for the end-to-end test, kind ([Set up a development environment and run the
checks](docs/how-to/develop.md)). The tests need no cluster and no credentials.

```sh
cargo build --locked
cargo test --locked
```

Before opening a pull request, run the same checks as CI, listed in
[Verification (same as CI)](AGENTS.md#verification-same-as-ci). The shortest path is
`docker build --target ci .`, which runs them in the CI image.

## Propose a rule

kausa has two kinds of rules, both deterministic and both run before any model call
([Detection and redaction rules](docs/reference/rules.md)): **detection** rules decide what is an
incident and how failures are grouped, and **redaction** rules decide what never leaves the
cluster. Open a **Rule request** issue first. A rule is accepted with all of these:

- **An id**: `D` for detection, `R` for redaction, short and stable. Once released, an id is never
  reused for something else.
- **Evidence**: for detection, the upstream Kubernetes documentation or source that shows how the
  failure appears in events or Pod status; for redaction, the published format of the value (the
  issuer's documentation of a token prefix, for example).
- **Fixtures**: for detection, the Pod status and events that trigger the rule and the closest
  ones that must not (a single restart during a rollout, for example); for redaction, lines that
  must be redacted and lines that look alike and must stay (version strings, hashes in paths,
  timestamps).
- **Expected output**: the incident, or the redacted text, for each fixture, committed with the
  rule as unit tests next to the code. A change to an expected output is reviewed like code.

Which files to change, step by step: [Add a detection or redaction
rule](docs/how-to/add-a-rule.md).

A redaction rule may only ever make kausa send less. A change that lets a value through that a
rule caught before needs an ADR. Fixtures contain no real data, not even sanitized by hand:
invent them, with `example.com`, the documentation ranges `192.0.2.0/24` and `2001:db8::/32`, the
AWS documentation account `123456789012` and token strings built to match a format, never ones
that were issued.

## Propose a feature

A feature that is not a rule (a new output, a new flag, a new mode, support for something kausa
does not read today) starts with a **Feature, docs or question** issue. If the maintainer thinks
it fits the scope ([0001. Build a small, read-only explainer that writes one redacted explanation
per incident](docs/adr/0001-scope-and-positioning.md)), the next step is an ADR: a pull request
that adds `docs/adr/NNNN-slug.md` with `Status: proposed`, in the format of the existing ones
(Context, Alternatives, Decision, Consequences). The code comes once the ADR is accepted, in a
pull request of its own or together with it.

Model providers are not a code extension point: kausa speaks one OpenAI-compatible API
([0008. Send only allowlisted, redacted data to one OpenAI-compatible
endpoint](docs/adr/0008-data-leaving-the-cluster.md)). Support for a provider means checking that
its OpenAI-compatible endpoint works with kausa and saying so in an issue or the docs, not a
native client.

## How decisions are made

kausa has one maintainer, FedeAM, who reviews and merges pull requests and accepts ADRs. Decisions
are recorded in [`docs/adr/`](docs/adr/), so that the reason for each one outlives the
conversation that led to it. A contributor who reviews and lands changes over time can be asked
to become a reviewer; that, and anything else about how the project is run, will be written here
when it happens.

## Issues and pull requests

Everything happens on GitHub, at <https://github.com/fedeam/kausa>: issues, pull requests and
reviews. Open an issue with one of its forms: **Bug report**, **Rule request**, or **Feature, docs
or question**, which is also the one to open before a pull request that is neither a rule nor a
bug fix. Fork the repository, push a branch to your fork and open a pull request against `main`.
Why it works this way: [0007. Take contributions and merge pull requests on
GitHub](docs/adr/0007-contributions-and-merges-on-github.md).

- **Squash merges.** The maintainer merges with GitHub's squash button. Your pull request becomes
  one commit on `main`: its header is the pull request title, followed by GitHub's
  ` (#<number>)`, and its body the pull request description. The commits in your branch are
  squashed, so their messages do not reach `main`.
- **The pull request title** follows
  [Conventional Commits 1.0.0](https://www.conventionalcommits.org/en/v1.0.0/) with a required
  scope: `type(scope): subject`, for example `feat(rules): flag deleted persistent volume claims`.
  Types: `feat`, `fix`, `perf`, `refactor`, `style`, `test`, `docs`, `build`, `ci`, `chore`,
  `revert`. Subject in the imperative, lowercase start, no trailing period, title up to 64
  characters, so that the header with the number stays within 72. The maintainer may edit the
  title before merging.
- **The description** says why the change is needed: it becomes the body of the commit.
- **One logical change per pull request.**
- **CI.** For every pull request, the `CI` workflow on GitHub Actions runs
  `docker build --target ci .` on linux/amd64, and its `ci` check has to pass before merging.
  Status as of 2026-10-23: GitHub Actions do not start jobs for this repository yet, so run the
  checks yourself ([Build and test](#build-and-test)). The maintainer also builds your pull
  request before merging, after reading it and in a clean clone ([Maintain the
  repository](docs/how-to/maintain.md#review-and-merge-a-pull-request)); a pull request that
  changes the `Dockerfile`, `.dockerignore`, `rust-toolchain.toml`, a build script (`build.rs` or
  the `build` key in `Cargo.toml`) or a workflow waits for GitHub Actions instead.
- **After the merge** the maintainer copies `main` to a private Git server where the same build
  runs on linux/arm64. If it fails there, the maintainer opens an issue on GitHub that links your
  pull request, and the fix is a new pull request.
- **Changelog.** If users will notice your change, add a line under `Unreleased` in the
  [Changelog](CHANGELOG.md).
- Do not add AI attribution trailers or footers. You are responsible for what you submit.
- Do not bypass hooks with `--no-verify`.

## License of contributions

There is no CLA and no DCO sign-off. Contributions are licensed inbound as the project is
licensed outbound: unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in the work by you, as defined in the Apache-2.0 license, shall be dual
licensed under the [MIT license](LICENSE-MIT) and the [Apache License, Version 2.0](LICENSE-APACHE),
without any additional terms or conditions.
