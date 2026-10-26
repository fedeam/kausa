# 0007. Take contributions and merge pull requests on GitHub

- Status: accepted
- Date: 2026-10-23

## Context

- **The maintainer's standard** keeps every repository on his private Gitea instance, the source
  of truth, and mirrors it to GitHub through a second push URL.
- **Outside contributors can only reach GitHub.** The Gitea instance is private and not reachable
  from the internet, so issues, pull requests (PRs) and reviews from anyone but the maintainer can
  only happen on GitHub.
- **One Conventional Commit per PR.** The commit rules in [`AGENTS.md`](../../AGENTS.md#commits)
  require a Conventional Commits header with a scope. GitHub's squash button turns a PR into one
  commit on `main`, and the repository can take that commit's header from the PR title and its
  body from the PR description. GitHub appends ` (#<number>)` to the title (seen on squash commits
  of other repositories; hypothesis for this one until its first merge).
- **Two architectures.** The Gitea Actions runner is linux/arm64; GitHub's hosted runners are
  linux/amd64. Both run the same `docker build --target ci .` ([0003. Run CI in one pinned
  container build, on GitHub Actions and Gitea Actions](0003-ci-in-one-pinned-container-build.md)).
- **The same flow already works** in the maintainer's earlier project kaution, decided in its ADR
  0007 on 2026-10-23 after the alternatives were weighed there: keeping Gitea first (outside
  contributors could not reach it), merging locally and pushing to both (GitHub shows the PR as
  closed, not merged, and nothing ties the pushed tree to the checked one), and a Gitea pull mirror
  of GitHub (it needs a recreated repository and a GitHub credential in Gitea).

## Decision

Contributors work on GitHub ([`fedeam/kausa`](https://github.com/fedeam/kausa)): issues, PRs and
reviews. PRs are merged on GitHub with the squash button, and the PR title, which becomes the
commit header on `main` with GitHub's ` (#<number>)` after it, must be a Conventional Commit with a
scope. Nobody pushes new commits to `main` directly, the maintainer included: his changes also go
through a PR. After each merge, the maintainer syncs `main` to Gitea, where the arm64 CI runs:

```sh
git fetch github
git merge --ff-only github/main
git push origin main
```

FedeAM decided this on 2026-10-23, as for kaution. The one exception is the first commit,
which creates `main` on both forges before any PR can exist. The commands and checks are in
[Maintain the repository](../how-to/maintain.md).

## Consequences

- **Gitea is a synced copy that runs the arm64 CI.** Its issues, pull requests and wiki are
  disabled so that nobody works there. Its `main` is protected against force pushes and deletion
  and receives only fast-forwards from GitHub's `main`. The branches of the maintainer's own PRs
  reach Gitea too, because `origin` pushes to both forges; no workflow runs for them there.
- **GitHub's CI (linux/amd64) is the PR gate once GitHub Actions run.** The `ci` check becomes a
  required status check in the ruleset on `main` at publication ([Publish the repository on
  GitHub](../how-to/publish-the-repository.md)). Until Actions run, the maintainer builds each PR
  locally before merging, in a disposable clone ([Maintain the
  repository](../how-to/maintain.md#review-and-merge-a-pull-request)).
- **Only merged outside code reaches the self-hosted runner.** Contributions never reach Gitea,
  and the Gitea workflow has no `pull_request` trigger and runs for pushes to `main`. A branch of
  the maintainer's that changes `.gitea/workflows/ci.yml` still runs there, because Gitea reads the
  workflow from the pushed commit ([0003. Run CI in one pinned container build, on GitHub Actions
  and Gitea Actions](0003-ci-in-one-pinned-container-build.md)).
- **Dependency updates come from Dependabot on GitHub.** `.github/dependabot.yml` asks for weekly
  version updates of Cargo, the Docker base image, the Rust toolchain and GitHub Actions, minor and
  patch updates grouped, and a three-day cooldown for new releases in every ecosystem. The `rust`
  image and `rust-toolchain.toml`, which must name the same release, move together in one
  multi-ecosystem group (hypothesis until its first PR). Titles start with `build(deps)`,
  `build(deps-dev)` or `ci(deps)`. The tools pinned by `ARG` in the `Dockerfile` (cargo-deny,
  cargo-audit and gitleaks) are bumped by hand.
- **The sync is a manual step.** Until the maintainer runs it, the arm64 CI and Gitea's weekly
  advisory run check an older `main`.
- **Nothing checks the PR title automatically.** The maintainer checks it, and edits it if needed,
  before merging.
