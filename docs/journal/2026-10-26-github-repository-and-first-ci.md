# 2026-10-26 — GitHub repository and first CI runs

Create the private GitHub repository, push the first commit to both forges and check the first CI
runs. Repositories touched: kausa.

## What was done

- **The first commit**, `b89d67e` (`chore(repo): set up repository skeleton, docs and ci`), the
  only direct commit to `main` ([0007. Take contributions and merge pull requests on
  GitHub](../adr/0007-contributions-and-merges-on-github.md)), pushed to Gitea and to GitHub.
- **Commit identity.** The first attempt carried the maintainer's personal address: the shell
  that committed exported `GIT_AUTHOR_*` and `GIT_COMMITTER_*` variables, which take precedence
  over the clone's `user.name` and `user.email`. It was caught before any push and made again with
  `FedeAM <98847815+fedeam@users.noreply.github.com>` as author and committer (the noreply
  address from `gh api user`); only `b89d67e` was ever pushed. [Maintain the
  repository](../how-to/maintain.md) now says to check for those variables and to look at each new
  commit's identity.
- **GitHub repository** `fedeam/kausa`, private, with the settings of the maintainer's earlier
  project kaution, read from it first: squash merges only, with the commit title from the pull
  request title and the message from its body; branches deleted on merge; the update-branch
  button; no wiki, projects or discussions; the labels `rule`, `ci`, `dependencies`, `security`
  and `needs-triage` next to GitHub's defaults; Dependabot alerts; the description and the topics.
- **Checked on kausa itself**, replacing what [Publish the repository on
  GitHub](../how-to/publish-the-repository.md) had taken from kaution: the default workflow token
  is read-only (`default_workflow_permissions: read`); rulesets are refused while the repository is
  private ("Upgrade to GitHub Pro or make this repository public"); the check run of the `CI`
  workflow is named `ci`.
- **First CI runs:**
  - **Gitea Actions (linux/arm64), run 238: passed** in about six minutes. gitleaks scanned the
    one commit with no leaks, `cargo deny` and `cargo audit` reported nothing, and the six
    command-line tests passed. The repository variable `REPO_CLONE_URL` and the fetch with the
    job token worked on the first try.
  - **GitHub Actions (linux/amd64): not started**, as expected: "The job was not started because
    recent account payments have failed or your spending limit needs to be increased". The
    account's included minutes for October are used up; they renew on 2026-11-01.
- **An outage on the way:** the self-hosted Git server was unreachable for part of the session
  (its node refused every TCP connection while answering ping). Nothing was changed for it; the
  push waited until it was back.

## Decisions

- None new. The flow is the one in [0007. Take contributions and merge pull requests on
  GitHub](../adr/0007-contributions-and-merges-on-github.md); this change is the first pull
  request.

## Pending

- **GitHub Actions** from 2026-11-01, or right after publication: confirm the first `ci` run on
  linux/amd64.
- **Dependabot's first run** and its titles, and the `rust` group (hypotheses in
  `.github/dependabot.yml`).
- **First merge on GitHub:** confirm that the squash commit's header is the pull request title
  followed by ` (#<number>)`.
- The rest of the list in [2026-10-23 — Repository setup](2026-10-23-repository-setup.md#pending)
  stands, except the creation of the GitHub repository, done here.
