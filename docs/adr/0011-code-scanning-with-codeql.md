# 0011. Scan the code with CodeQL's default setup

- Status: accepted
- Date: 2026-10-27

## Context

OpenSSF Scorecard's SAST check, which the publication checklist runs (step 11 of [Publish the
repository on GitHub](../how-to/publish-the-repository.md)), recognises CodeQL and SonarCloud
only: clippy, which CI runs with `-D warnings`, does not count. The checklist asked for a decision
between CodeQL and accepting a score of 0. GitHub's CodeQL default setup, offered on the
repository on 2026-10-27, detected Rust, GitHub Actions workflows and Python (the test helpers in
`tests/`), and needs no workflow file in the repository.

## Alternatives

- **Accept 0.** Nothing to maintain, but no second analyzer, and a check that stays red.
- **A CodeQL workflow in the repository.** Full control over queries and triggers, but one more
  workflow and more pinned actions to keep up to date.
- **SonarCloud.** An outside service with its own account and token, against the repository's
  rule of no secrets ([Security policy](../../SECURITY.md)).

## Decision

CodeQL's default setup, with the default query suite, turned on on 2026-10-27 right after the
repository became public. It runs on GitHub's own actions, which the repository's Actions policy
allows, scans pull requests and `main`, and reports to code scanning, next to Scorecard's results.

## Consequences

- Code scanning alerts appear in the repository's Security tab; the maintainer triages them like
  issues.
- The setup lives in the repository's settings, not in a file: check it with
  `gh api repos/fedeam/kausa/code-scanning/default-setup`.
- Whether CodeQL's Rust analysis finds anything clippy does not is unknown yet; if it only adds
  noise, revisit this decision.
