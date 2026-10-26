# 0006. Make the GitHub repository public only after a checklist

- Status: accepted
- Date: 2026-10-23

## Context

- The GitHub repository `fedeam/kausa` is private until the maintainer decides that the project is
  validated, then public. The copy on his private Gitea instance, which runs the arm64 CI, stays
  private ([0007. Take contributions and merge pull requests on
  GitHub](0007-contributions-and-merges-on-github.md)).
- Making a repository public exposes its whole history, commit metadata included, and every ref
  GitHub holds, not only the current tree. kausa is developed and tested next to the maintainer's
  own clusters, whose hostnames, addresses and usernames must never be published.
- kausa handles cluster data that its users consider sensitive, so its own repository has to be
  an example: no real logs, events or names in fixtures, ever.
- The maintainer's standard requires the history to be reviewed and the `repository: public`
  badge to be committed before the visibility changes.
- GitHub's private vulnerability reporting, the channel that the [Security
  policy](../../SECURITY.md) points to, is enabled per repository. Rulesets on `main` need a public
  repository or GitHub Pro (GitHub's answer for kaution on 2026-10-23).

This repeats the checklist of the maintainer's earlier project kaution (its ADRs 0006 and 0007,
2026-10-23), in its final form.

## Decision

The GitHub repository becomes public only after every step of [Publish the repository on
GitHub](../how-to/publish-the-repository.md) passes, in order. FedeAM decided this on
2026-10-23, reusing kaution's checklist.

- **The history is checked as GitHub holds it**, on a mirror clone of the GitHub repository:
  `gitleaks` for secrets, a private patterns file, kept outside the repository, for internal names
  and personal data, and a search for private addresses. Commit and tag identities are reviewed
  too.
- **The competitive survey is repeated** and the README comparison updated, because it is dated
  and the other projects move fast.
- **The badge and status commit comes before the visibility change**, as the standard requires.
- **Security features are turned on at publication:** private vulnerability reporting, secret
  scanning with push protection, and Dependabot security updates. Dependabot alerts are on from
  the start.
- **`main` is protected on both forges.** On Gitea, against force pushes and deletion from the
  creation of the repository. On GitHub, a ruleset at publication: a pull request, the `ci` check,
  a linear history, no force push and no deletion, with no bypass.

## Consequences

- Publication is a deliberate, recorded step, with a documented rollback; a failed step stops it.
- Vulnerability reports go through GitHub private vulnerability reporting, which exists only from
  publication on ([Security policy](../../SECURITY.md)).
- Until publication, nothing enforces the rule against direct pushes to `main` on GitHub; it is
  written down in [`AGENTS.md`](../../AGENTS.md) and followed.
