# 2026-10-27 — Publication

## Request

The maintainer asked to publish kausa that day, before the account's GitHub Actions minutes
renewed, and to make it mandatory that every change goes through a pull request that the
maintainer approves.

## What was checked

[Publish the repository on GitHub](../how-to/publish-the-repository.md), in order:

- **Before you start:** `gh` logged in as `fedeam` with admin rights; every branch and tag
  fetched; Gitea's CI green on `main`; `CODE_OF_CONDUCT.md` with its contact address. The test
  message to that address was not sent (the agent was not allowed to send mail from the
  maintainer's account); the maintainer checks the forwarding.
- **Steps 1 to 3** on a mirror clone of the GitHub repository (every ref, pull request refs
  included): gitleaks `no leaks found`; the private patterns (with the maintainer's legal name)
  matched only the generic `svc.cluster.local` of test fixtures; no LAN address; identities
  `FedeAM`, `GitHub` and `dependabot[bot]` only; no tags; nothing in the text of pull requests,
  issues or comments.
- **Step 4:** the tree had been read as a stranger by the audit of the same day and fixed
  ([2026-10-27 — First run of the kind test](2026-10-27-first-kind-run.md)). The competitive
  survey was not repeated: its update was one day old
  ([Competitive landscape update](../research/2026-10-26-competitive-landscape-update.md)).
- **Step 5:** the badge and status pull request, merged before the visibility change.

## What was enabled

- **Step 6:** the repository is public. GitHub Actions started at once (public repositories do
  not use the account's minutes): on their first run on GitHub, `ci`, `image (ubuntu-24.04)`,
  `image (ubuntu-24.04-arm)` and `kind` all passed. The kind test thereby also passed on Linux,
  which until then had only run on Docker Desktop.
- **Step 7:** private vulnerability reporting, secret scanning and its push protection,
  Dependabot security updates; Actions limited to GitHub's own actions and the Scorecard action,
  pinned by full commit SHA; workflow runs of every outside contributor's pull request wait for
  the maintainer's approval.
- **Step 8:** the ruleset `main`: a pull request with one approving review given after the last
  push, stale approvals dismissed, squash merges only, a linear history, no force push and no
  deletion, and the four checks above required. Its only bypass actor is the admin role, for
  pull requests only. The agent's attempt to merge its own pull request with that bypass was
  refused by its own permissions: merging stays with the maintainer, as asked.
- **Step 11:** CodeQL's default setup turned on (Rust, workflows, Python; its first run passed),
  recorded in ADR 0011, which comes with the Scorecard workflow in its own pull request.
- **Step 10:** the CI badge, in this pull request.

## Pending

- The maintainer merges the Scorecard pull request; after its first run, its badge and the
  first result of each check against step 11's expectations.
- **Step 9:** the social preview image (none exists yet); the homepage stays empty.
- **Step 12:** OpenSSF Best Practices registration, at bestpractices.dev, with the maintainer's
  GitHub login.
- **Step 13:** the crates.io name, with the first real release.
- Check that `conduct@fedeam.com` still forwards.
- `fedeam/kausa-old`, the private backup of the history before the rewrite, can be deleted.
