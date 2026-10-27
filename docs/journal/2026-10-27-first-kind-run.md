# 2026-10-27 — First run of the kind test

## Request

Run the kind test and the GitHub image build now instead of waiting for GitHub Actions (the
account's included minutes for private repositories are used up until 2026-11-01), then clean up.

## What was done

- kind v0.33.0, the version CI pins, installed on the maintainer's machine with Homebrew. The
  test ran with `KUBECONFIG` pointing at a file of its own: kind makes its context the current one,
  and the maintainer's kubeconfig was left unchanged (checked by its checksum before and after).
- **First run: failed, and showed a bug in the script.** The fake endpoint could not start
  (`Address already in use`: something on the machine held its fixed port, 18080), the script
  went on, and another program answered kausa with `404 Not Found`. Everything before the model
  call worked: the cluster, the chart under the restricted Pod Security Standard, the `D001` and
  `D008` incidents, and the `ExplanationSkipped` Events written about the Deployment and the Job.
- **The fix:** the script takes a free port and waits until the endpoint answers before it
  installs anything, and stops if the endpoint died.
- **Second run: `e2e: ok`**, on Docker Desktop (arm64): an `Explained` Event about the Deployment
  and one about the Job, with the placeholder `namespace-1` mapped back to the real namespace; no
  real name in the requests the endpoint received; the ClusterRole as in the reference (the
  allowed list and the refused list of `kubectl auth can-i`); `kausa_events_written_total`
  counting them; no restart of kausa. The Docker Desktop path, a hypothesis until now, works.
- **The amd64 image**, built under emulation with `docker build --platform linux/amd64 --target
  runtime`: it runs (`kausa 0.1.0-dev`), as user 65532, 4.6 MB compressed. The layers came from
  the cache of an earlier build of the same instructions.

## Later the same day: an audit before publication

The maintainer asked for an audit of the repository before publishing it, and whether kausa works
well and is set up for others to contribute. Five read-only reviewers (the history and private
data, the docs read as a stranger, the GitHub settings, readiness for contributors, and whether
it works), each with a second reviewer to verify the findings.

- **No secrets, internal names, addresses or personal email** anywhere GitHub would publish:
  gitleaks found nothing in any ref, and the pull request texts are clean.
- **Two blockers, both the maintainer's legal name:** the rewrite of 2026-10-26 changed the
  commit metadata only, so the content of four old commits on `main` (the license, `AGENTS.md`,
  ADRs) still had it; and the pull request refs `refs/pull/1` to `3`, which GitHub keeps and only
  GitHub can delete, point at commits authored with it. The maintainer chose to rewrite the
  history (content and authors) and to recreate the GitHub repository.
- **Code:** a transient endpoint failure left an incident unexplained and still counted against
  the budget, an error status lost the provider's reason, and watch errors had no metric. Fixed in
  pull request #12, and checked live (an endpoint answering `503`, then one refusing connections
  with a budget of one call per hour).
- **Docs:** the README's claim that memory does not grow with the number of Pods (it grows by one
  small record each), a checklist grep that could never print nothing, status lines the
  checklist's greps missed, planning jargon, no quickstart, no step-by-step path to add a rule,
  no way documented to propose a feature, `AGENTS.md` written for the maintainer's clone only, and
  a committed `.pyc`. Fixed in the following pull request. The README's new quickstart was run as
  written in a kind cluster: an incident and its request about 70 seconds after the failing
  Deployment was created.
- **Kept as they are, on purpose:** the mentions of the maintainer's earlier project kaution (a
  name only); the journals' names of the software in the maintainer's own infrastructure.
- **For after publication**, from the audit: good-first-issue candidates (more `R011` token
  formats with their issuers' documentation, exhaustive `match` in `is_state` and `reads_logs`, a
  cap on the Pod names an incident keeps, fewer repeated watch-error lines, a test that every rule
  id appears in the rules reference, a check of pull request titles), owner lookups only for
  failing Pods (help wanted), a test against a hosted provider, and the arm64 `ci` job on
  GitHub's runners.

## Later the same day: the history rewritten and the repository recreated

- The history of `main` was rewritten with `git filter-repo`: the maintainer's legal name and the
  old profile name replaced by `FedeAM` in every file of every commit, and the ` (#N)` at the end
  of the commit headers removed, since the pull request numbers of the first repository would
  have linked to unrelated pull requests in the new one. The tree of the last commit is
  byte-for-byte the same as before; every commit SHA changed, and the ones the docs cite were
  updated to the new history in the same change as this entry.
- The first GitHub repository was renamed `fedeam/kausa-old` and stays private, as a backup with
  its thirteen pull requests; a new private `fedeam/kausa` was created with the same description
  (now without "read-only"), settings, topics, labels and Actions permissions, and the rewritten
  `main`. Pull request numbers start again at 1.
- The copy on Gitea and the maintainer's clone still had the old history at the time of writing:
  replacing Gitea's `main` needs a force push to a protected branch, which the agent was not
  allowed to do; the maintainer does it.

## Pending

- The same two checks on GitHub's runners (the Linux path of the kind test, and the arm64 runner),
  once GitHub Actions start jobs again.
- Publication, when the maintainer validates the project.
