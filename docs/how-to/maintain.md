# Maintain the repository

The maintainer's recipes: triage issues, review and merge pull requests on GitHub, open their own
pull requests, sync `main` to Gitea, check the arm64 CI run there, bump the tools pinned in the CI
image and, later, cut a release. Why contributions and merges happen on GitHub, and why nobody
pushes new commits to `main`: [0007. Take contributions and merge pull requests on
GitHub](../adr/0007-contributions-and-merges-on-github.md).

Status as of 2026-10-27: the GitHub repository is public since 2026-10-27; GitHub Actions run since then
(the pull requests before that have no CI result on GitHub); Dependabot's first pull request was merged on 2026-10-27; nothing has been released.

## Before you start

- `gh auth status` shows the `fedeam` account, with admin rights on `fedeam/kausa`.
- The local clone has two remotes. `origin` fetches from Gitea and pushes to Gitea and GitHub, in
  that order; `github` fetches from GitHub and its push URL is disabled. `git remote -v` shows:

  ```text
  github  git@github.com:fedeam/kausa.git (fetch)
  github  no-push-use-origin (push)
  origin  <Gitea clone URL> (fetch)
  origin  <Gitea clone URL> (push)
  origin  git@github.com:fedeam/kausa.git (push)
  ```

- `git config user.name` and `git config user.email`, run in the clone, print `FedeAM`
  and `98847815+fedeam@users.noreply.github.com` (GitHub's noreply address), so that no personal
  address is ever in a commit. Set them in the clone's own configuration before the first commit:
  commit metadata cannot be changed later without rewriting the history. The environment variables
  `GIT_AUTHOR_NAME`, `GIT_AUTHOR_EMAIL`, `GIT_COMMITTER_NAME` and `GIT_COMMITTER_EMAIL` take
  precedence over that configuration, so none of them may be set in the shell that commits; check
  each new commit with `git log -1 --format='%an <%ae> %cn <%ce>'`.

## Triage issues

The issue forms add `needs-triage` to every new issue. Triage removes it once the issue is
understood and labelled.

| Label | Use |
|---|---|
| `needs-triage` | Not reviewed yet. Release notes skip pull requests that carry it |
| `rule` | A new or changed detection or redaction rule (the Rule request form adds it) |
| `bug` | kausa fails, explains something wrong, misses a failure or costs more than it should (the Bug report form adds it) |
| `enhancement` | Any other feature |
| `security` | Hardening and security fixes, redaction gaps included. Vulnerabilities themselves never go in an issue |
| `dependencies` | Dependency updates (Dependabot adds it) |
| `documentation` | Documentation only |
| `ci` | CI, the CI image and the workflows |

GitHub's default labels (`good first issue`, `help wanted`, `question`, `duplicate`, `invalid`,
`wontfix` and the others) keep their usual meaning. The same labels sort pull requests into the
release notes (`.github/release.yml`).

1. List what is waiting:

   ```sh
   gh issue list --repo fedeam/kausa --label needs-triage
   ```

2. Read each issue. If the input is missing or not minimal, ask for a sanitized, minimal one. A
   rule request is judged by the criteria in [Contributing to kausa](../../CONTRIBUTING.md#propose-a-rule).
   Issues about kausa often quote cluster output: look for real logs, events, names and addresses
   before anything else (step 3).
3. If an issue discloses a vulnerability or contains a secret, an internal hostname or personal
   data, delete it (an edit keeps the old text in the issue's history) and continue privately, as
   the [Security policy](../../SECURITY.md) says. A leaked secret also has to be rotated at its
   source, whoever owns it.

   ```sh
   gh issue delete <number> --repo fedeam/kausa
   ```

   A pull request cannot be deleted that way: `gh pr` has no delete command, and its
   `refs/pull/<number>/head` stays in the repository. If a pull request contains a secret, an
   internal hostname or personal data, rotate the secret at its source, close the pull request,
   and ask GitHub Support to remove it and its refs: GitHub's documentation on removing sensitive
   data sends the cleanup of affected pull requests to Support (checked on 2026-10-23). If it is
   still there at publication, the history scan in [Publish the repository on
   GitHub](publish-the-repository.md) finds it and stops the publication.

4. Label it and remove `needs-triage`:

   ```sh
   gh issue edit <number> --repo fedeam/kausa --add-label rule --remove-label needs-triage
   ```

## Review and merge a pull request

1. **Read the change** before anything runs it. Look hardest at what runs in CI: the workflows,
   the `Dockerfile`, `.dockerignore`, build scripts, tests and new dependencies (each one needs a
   reason and must pass `deny.toml`). Then what kausa would do differently in a cluster: a new
   field in a request, a new permission or a changed redaction rule needs an ADR and an update to
   the reference pages and `SECURITY.md`. No secrets, internal hostnames or personal data.

   ```sh
   gh pr view <number> --repo fedeam/kausa
   gh pr diff <number> --repo fedeam/kausa
   ```

2. **Check the title.** GitHub appends ` (#<number>)` to it, and the result becomes the commit
   header on `main`. The title is a Conventional Commit with a scope, `type(scope): subject`,
   imperative, lowercase start, no trailing period, up to 64 characters so that the header stays
   within 72 ([`AGENTS.md`](../../AGENTS.md#commits)). The suffix is seen on squash commits of
   other repositories but not mentioned in GitHub's documentation of the setting: confirm it on
   the first merge and record it in the journal. Fix the title if needed:

   ```sh
   gh pr edit <number> --repo fedeam/kausa --title "fix(rules): <subject>"
   ```

3. **Label it** with one of `rule`, `bug`, `enhancement`, `security`, `documentation` or `ci`.
   The label decides the pull request's category in the release notes (`.github/release.yml`);
   without one it lands under "Other changes". The issue forms label issues, not pull requests,
   and only Dependabot labels its own:

   ```sh
   gh pr edit <number> --repo fedeam/kausa --add-label <label>
   ```

4. **Check CI.** The `ci`, `ci-arm64`, `image (ubuntu-24.04)`, `image (ubuntu-24.04-arm)` and
   `kind` checks must be green (the ruleset on `main`
   requires them):

   ```sh
   gh pr checks <number> --repo fedeam/kausa
   ```

   For an outside contributor, GitHub may wait for approval before running the workflow (step 7
   of [Publish the repository on GitHub](publish-the-repository.md) makes it wait for every
   outside contributor): approve the run only after step 1.

   To build a pull request locally as well, do it only after step 1: the build runs the pull
   request's code (build scripts, procedural macros and tests) with network access, from your
   machine. So:

   - Build it in a disposable clone of GitHub, never in your own clone. The build context
     includes `.git`, and your clone's `.git/config` names the Gitea remote; a changed
     `.dockerignore` would also let in your ignored files (`.claude/`, `.env*`). A fresh clone
     knows only github.com and has no ignored files.
   - Prefer a network with no route to your own clusters and private services.

   ```sh
   D=$(mktemp -d)
   git clone git@github.com:fedeam/kausa.git "$D/src"
   git -C "$D/src" fetch origin "pull/<number>/head"
   git -C "$D/src" switch --detach FETCH_HEAD
   (cd "$D/src" && docker build --target ci .)
   rm -rf "$D"
   ```

5. **Merge with the squash button** on the pull request page. The dialog shows the commit that
   will land on `main`: the title comes from the pull request title, with ` (#<number>)`
   appended, and the message from its description. Before confirming, remove the template's
   checklist and comments from the message, keep the why, and check that it carries no AI
   attribution trailer. GitHub deletes the branch after the merge when it lives in this
   repository (Dependabot's and the maintainer's do); a fork's branch stays in the fork.
6. **Sync Gitea** right away, as [Sync main to Gitea](#sync-main-to-gitea) describes.

## Open a pull request of your own

The maintainer's changes go through a pull request too: nobody pushes new commits to `main`.
Branch from GitHub's `main`, which may be ahead of the local one:

```sh
git fetch github
git switch --no-track -c <branch> github/main
# edit and commit
git push origin <branch>
gh pr create --repo fedeam/kausa --base main --head <branch> \
  --title "type(scope): subject" --body "<why>"
```

`git push origin <branch>` pushes the branch to Gitea and GitHub. Gitea runs no workflow for it
unless the branch changes `.gitea/workflows/ci.yml`; review such a change before pushing it. Then
review and merge the pull request as in [Review and merge a pull
request](#review-and-merge-a-pull-request), and sync. GitHub deletes its copy of the branch on
merge; after the sync, delete Gitea's and the local one:

```sh
git push origin --delete <branch>
git branch -D <branch>
```

The first command deletes the branch on Gitea, and its GitHub part fails with "remote ref does not
exist", which is expected: GitHub deleted it already.

## Sync main to Gitea

After each merge on GitHub, fast-forward the local `main` to GitHub's and push it through
`origin`:

```sh
git switch main
git fetch github
git merge --ff-only github/main
git push origin main
```

`origin` pushes to Gitea first, then to GitHub, where `main` is already at that commit: git
sends GitHub no update (`Everything up-to-date`), so the ruleset on `main` has nothing to check.
The push to Gitea starts the CI run for that commit.

Verify that both forges point at the same commit:

```sh
git fetch origin
git rev-parse origin/main github/main   # two identical SHAs
```

## Check the arm64 run on Gitea

1. Open the Gitea repository's **Actions** tab and find the `CI` run of the commit you synced (its
   title is the commit header: the pull request title and its number).
2. Green: nothing to do. The run has two jobs: `ci` (the checks) and then `image` (the arm64
   image).
3. Red because a download failed (a connection reset or a timeout while the image fetches the
   pinned tools): run it again. If downloads from GitHub keep failing, check the MTU of Docker's
   bridge on the runner against the pod network's first, the cause on 2026-10-23. Either use
   **Re-run all jobs** on the run's page, or start a manual run of the `CI` workflow on `main` from
   the Actions tab: the workflow accepts manual runs (`workflow_dispatch`), which check the latest
   commit of `main`.
4. A job stopped with no error, its log cut off in the middle of a step: the runner stops a job
   after 30 minutes. If `ci` or `image` grew past that, split it again or make it faster; the
   limit belongs to the runner's configuration, not to this repository.
5. Red for a real reason (something that fails only on linux/arm64): `main` already has the
   change, so open an issue on GitHub and fix it in a new pull request.

## Review Dependabot pull requests

Dependabot opens its pull requests on GitHub every week (`.github/dependabot.yml`), with the
`dependencies` label and titles such as `build(deps): bump …`, `build(deps-dev): bump …` (a
dev-dependency) or `ci(deps): bump …`. Review and merge them like any other pull request, then
sync.

- **The commit message.** The squash dialog proposes the pull request body as the commit body,
  and Dependabot's carries release notes, a commit list and its commands. Before confirming,
  replace it with one line that says why, for example `Keep <dependency> current; see its release
  notes.`
- **The `rust` group.** The `rust` image in the `Dockerfile` and `rust-toolchain.toml` must name
  the same release, or the CI build fails, so a multi-ecosystem group moves both in one pull
  request. The `rust-version` in `Cargo.toml` (the minimum supported Rust version, shown by the
  README badge) does not move with the toolchain. Hypotheses to check on the first such pull
  request: that both updates arrive together, the title it gets, that the image keeps the
  `-slim-trixie` suffix and that its digest moves with the tag.
- **Editing a Dependabot branch.** If you push a commit to it (to add a missing half of the
  `rust` group, for example), Dependabot stops rebasing that pull request, as GitHub's
  documentation says; when it falls behind `main`, update it with the update-branch button.
- **New releases wait three days** in every ecosystem before Dependabot proposes them
  (`cooldown`).

## Bump the pinned tools

Dependabot does not see cargo-deny, cargo-audit, gitleaks and helm: the `Dockerfile` pins each
one by `ARG`, with a version and a SHA-256 checksum per architecture. Nor does it see kind, its
node image and helm in `.github/workflows/e2e.yml`, or the distroless digest of the `runtime`
target (its docker updates are only for the `rust` image); those are covered under
[Bump the packaging pins](#bump-the-packaging-pins). Nothing reports their new releases,
so check them from time to time, for example with each Rust update.

1. Find the latest versions:

   ```sh
   gh release view --repo EmbarkStudios/cargo-deny --json tagName --jq .tagName
   gh api 'repos/rustsec/rustsec/releases?per_page=100' \
     --jq '.[].tag_name | select(startswith("cargo-audit/"))' | head -1
   gh release view --repo gitleaks/gitleaks --json tagName --jq .tagName
   ```

   `rustsec/rustsec` publishes releases of several crates, so its latest release is often not
   cargo-audit's.

2. Take both checksums of each new version from the official release, as the comment above each
   tool in the `Dockerfile` says (replace `<version>`):

   ```sh
   # cargo-deny: the .sha256 file next to each archive
   for triple in x86_64-unknown-linux-musl aarch64-unknown-linux-musl; do
     gh release download <version> --repo EmbarkStudios/cargo-deny \
       --pattern "cargo-deny-<version>-$triple.tar.gz.sha256" --output -
     echo "  $triple"
   done

   # cargo-audit: no checksum file; the SHA-256 digests GitHub reports for the release assets
   gh api 'repos/rustsec/rustsec/releases/tags/cargo-audit%2Fv<version>' \
     --jq '.assets[] | "\(.digest)  \(.name)"' \
     | grep -E 'x86_64-unknown-linux-musl|aarch64-unknown-linux-gnu'

   # gitleaks: the checksums file of the release
   gh release download v<version> --repo gitleaks/gitleaks \
     --pattern 'gitleaks_*_checksums.txt' --output - | grep -E 'linux_(x64|arm64)\.tar\.gz$'
   ```

3. In the `Dockerfile`, change the tool's `*_VERSION` and both `*_SHA256_AMD64` and
   `*_SHA256_ARM64` values. Then update the versions that name the tool elsewhere:
   [Set up a development environment and run the checks](develop.md) (the versions and the
   `cargo install` line) and the `cargo-deny` version in the comment at the top of `deny.toml`.
4. Build the CI image for both architectures, or let the pull request's CI do it (GitHub's
   runners are linux/amd64, Gitea's linux/arm64). On an arm64 Mac the second build is emulated
   and slow:

   ```sh
   docker build --target ci .
   docker build --platform linux/amd64 --target ci .
   ```

5. Open a pull request ([Open a pull request of your own](#open-a-pull-request-of-your-own)), for
   example `build(deps): bump gitleaks to <version>`, merge it and sync.

## Bump the packaging pins

The same idea for the image and the end-to-end test. From time to time, and when distroless or
kind publish a security fix:

1. **distroless:** the digest of the multi-architecture index of the tag in the `runtime` target:

   ```sh
   docker buildx imagetools inspect gcr.io/distroless/static-debian13:nonroot --format '{{json .Manifest.Digest}}'
   ```

   Replace the digest after `nonroot@` in the `Dockerfile`.
2. **helm:** the version and both checksums in the `Dockerfile` (the `.sha256sum` file next to
   each archive on `get.helm.sh`), and the same version and the amd64 checksum in
   `.github/workflows/e2e.yml`:

   ```sh
   for arch in amd64 arm64; do curl -fsSL "https://get.helm.sh/helm-v<version>-linux-$arch.tar.gz.sha256sum"; done
   ```

3. **kind and its node image:** the version and the checksum of `kind-linux-amd64` (the
   `.sha256sum` asset of the release), and the `kindest/node` image with the digest the release
   notes list for that kind version:

   ```sh
   gh release download v<version> --repo kubernetes-sigs/kind --pattern 'kind-linux-amd64.sha256sum' --output -
   gh release view v<version> --repo kubernetes-sigs/kind --json body --jq .body | grep 'kindest/node:'
   ```

4. Build both targets (`docker build --target ci .` and `docker build --target runtime .`) and
   open a pull request, for example `build(deps): bump distroless to <date>`.

## Cut a release

Not scheduled yet; nothing here has been run. The image and the Helm chart are built in CI but
not published ([0010. Ship a static binary on distroless, with a Helm chart, built in CI and not
published yet](../adr/0010-packaging.md)), there is no release tooling yet, the crate is not
published (`publish = false` in `Cargo.toml`), and where the multi-architecture image and the
chart are published, and how they are signed, will be decided before the first release. The steps below are a placeholder:

1. In `CHANGELOG.md`, move the entries under `Unreleased` to a section for the new version with
   its date, and update the links at the bottom.
2. Set the new `version` in `Cargo.toml`. `Cargo.lock` records it too, so run `cargo build` once
   without `--locked` (which would refuse to change the lock file) and check that the lock file
   changed only there.
3. Merge both in one pull request of your own and sync Gitea.
4. Tag the merged commit on `main`, push the tag through `origin` (it reaches both forges), and
   create the GitHub release with the notes GitHub generates from the labels
   (`.github/release.yml`).

## If something fails

- **`git merge --ff-only github/main` refuses** → the local `main` has commits that are not on
  GitHub, which the rule against direct pushes should prevent → do not force anything. List them
  with `git log github/main..main`, move them to a branch for a pull request, and ask before
  resetting the local `main`.
- **The push to Gitea is rejected** → Gitea's `main` has commits that GitHub's does not → find
  out where they came from (`git fetch origin` and `git log github/main..origin/main`). Never
  force push: `main` is protected on Gitea.
- **During a sync, the push to Gitea works but GitHub's part is rejected** → another pull request
  was merged on GitHub after your `git fetch github`, so Gitea's `main` is now at a commit that
  GitHub's `main` already contains → run the sync again, which fast-forwards both. A push that
  carries new commits never gets here: they go through a pull request.
- **A Dependabot pull request for `rust` fails CI** → it moved only one of the `rust` image and
  `rust-toolchain.toml`, or the image for the new release is not published yet → add the missing
  half on the pull request's branch (see "Editing a Dependabot branch"), or close it and wait for
  the next run.
- **A tool bump fails at `sha256sum -c`** → a checksum does not match the downloaded archive →
  copy it again from the release, for the right architecture and asset name.
