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

## Later the same day: publication checks and the first implementation weekend

Request: carry on with everything, without asking but for decisions; the repository stays private.

### What was done

- **Publication checklist, steps 1 to 3, as a dry run** ([Publish the repository on
  GitHub](../how-to/publish-the-repository.md)), on a mirror clone of the GitHub repository with
  every ref (`main`, `refs/pull/1/head`, `refs/pull/2/head`): `gitleaks` found no leaks; the
  private patterns file (kept outside the repository), the search for `.local`, `.lan`,
  `.internal` and `home.arpa` names and the search for private addresses found nothing; the only
  identities are the noreply address and `GitHub <noreply@github.com>`. The mirror clone was
  deleted afterwards. Steps 5 onwards wait for the maintainer's validation.
- **The competitive survey, repeated:** [Competitive landscape, update of
  2026-10-26](../research/2026-10-26-competitive-landscape-update.md). The gap is still open;
  content redaction on by default exists in a small project the first survey missed.
- **First implementation weekend**, in this pull request:
  - `src/record.rs`: Pods and Warning events reduced to small records at once; the record drops
    environment values, command lines and probe paths (a test checks it).
  - `src/detect.rs`: the detection rules `D001` to `D008`, with `D003` before `D001`.
  - `src/owner.rs`: owner correlation through ReplicaSets and Jobs only, with metadata reads.
  - `src/incident.rs`: signals, grouping by namespace, top-level owner and detection, and the
    settle window; `src/fingerprint.rs`: normalization and an FNV-1a fingerprint.
  - `src/run.rs`: the watches (Pods; `events.k8s.io/v1` Events with the field selector
    `type=Warning,reportingController!=kausa`), relists, a five-second tick and the incident log
    lines; `src/main.rs`: `kausa run [--settle-seconds <N>]`.
  - Dependencies: `kube` 4.2.0 (client, runtime, rustls with ring), `k8s-openapi` 0.28.0
    (Kubernetes 1.32 API), `tokio`, `futures` and `serde_json`; 196 crates in all. `deny.toml`
    now allows ISC, BSD-3-Clause, Zlib, Unicode-3.0 and CDLA-Permissive-2.0, each with the crates
    that need it.
  - A review before the pull request (a reviewer for the Rust code and one for code against
    documentation, a skeptic re-checking each finding): 19 findings confirmed and fixed. Among
    them: a crash loop was logged again on every back-off cycle (states now get a 330-second
    grace period before they are forgotten); single events of different Pods added up to a false
    `D006` (events now settle one by one); `D005` could never settle (it now reads the Pod's
    `PodScheduled` condition); the fingerprint changed with the back-off duration and between
    `ErrImagePull` and `ImagePullBackOff`; a hung owner lookup could stop the loop (5-second
    timeout) and a Ctrl-C could be lost; a failed lookup was never retried; the event map kept
    every Warning event; the client honoured kube-rs's debug variables; log write errors were
    ignored; and documentation that did not match the code.
  - 32 unit tests and 7 command-line tests; the CI build passed locally for linux/arm64.
  - Documentation: [Command line and log format](../reference/command-line.md), new; the status
    of [Architecture](../explanation/architecture.md), [Detection and redaction
    rules](../reference/rules.md), the README, `AGENTS.md` and the front page.
- **Facts checked in Kubernetes' source** (2026-10-26): the Job controller emits a Warning event
  with reason `BackoffLimitExceeded` or `DeadlineExceeded` on the Job it marks failed, so `D008`
  needs no watch on Jobs; `events.k8s.io/v1` accepts field selectors on `type` and
  `reportingController`.

### Pending

- **A run against a real cluster.** The pure core is tested without one, but `kausa run` has not
  watched a live cluster yet: in this session, extracting a single-context kubeconfig of the
  maintainer's cluster and downloading kind were both refused by the agent's permission rules. The
  maintainer can run `kausa run` himself (read-only), or allow kind; otherwise the kind tests of
  the third weekend are the first live run.
- **Hypotheses from the first weekend:** that memory stays flat with the number of healthy Pods
  (measured on the fourth weekend); that the kubelet and scheduler reasons in the rules are the
  ones current releases emit.
