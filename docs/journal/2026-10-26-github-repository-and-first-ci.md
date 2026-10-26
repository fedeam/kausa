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

- **First merge on GitHub**, pull request #1, squash-merged as `a9e4533`, then synced to Gitea.
  The commit header is the pull request title followed by ` (#1)`, which confirms the hypothesis
  in [0007. Take contributions and merge pull requests on
  GitHub](../adr/0007-contributions-and-merges-on-github.md). Its author is the noreply address,
  as on every recent commit the maintainer made on GitHub's web (checked before merging), with
  the name of his GitHub profile, `FedeAM`, rather than `FedeAM`; the committer is
  `GitHub <noreply@github.com>`.

## Decisions

- [0008. Send only allowlisted, redacted data to one OpenAI-compatible
  endpoint](../adr/0008-data-leaving-the-cluster.md) and [0009. Run with read-only permissions
  that never include Secrets](../adr/0009-least-privilege-rbac.md) accepted by the maintainer,
  with log collection on by default for the crash-type and Job detections only.
- The maintainer delegated the rest of the work to agents on 2026-10-26: they merge their own pull
  requests and sync `main`, and ask only about decisions. The repository stays private until he
  validates it.

## Pending

- **GitHub Actions** from 2026-11-01, or right after publication: confirm the first `ci` run on
  linux/amd64.
- **Dependabot's first run** and its titles, and the `rust` group (hypotheses in
  `.github/dependabot.yml`).
- **Author name of squash merges:** GitHub writes the profile name, `FedeAM`. If the public
  history should read `FedeAM`, change the name on the GitHub profile before more merges.
- The rest of the list in [2026-10-23 — Repository setup](2026-10-23-repository-setup.md#pending)
  stands, except the creation of the GitHub repository, done here.
