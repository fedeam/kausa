# 2026-10-26 — GitHub repository and first CI runs

Create the private GitHub repository, push the first commit to both forges and check the first CI
runs. Repositories touched: kausa.

## What was done

- **The first commit**, `61981a4` (`chore(repo): set up repository skeleton, docs and ci`), the
  only direct commit to `main` ([0007. Take contributions and merge pull requests on
  GitHub](../adr/0007-contributions-and-merges-on-github.md)), pushed to Gitea and to GitHub.
- **Commit identity.** The first attempt carried the maintainer's personal address: the shell
  that committed exported `GIT_AUTHOR_*` and `GIT_COMMITTER_*` variables, which take precedence
  over the clone's `user.name` and `user.email`. It was caught before any push and made again with
  the noreply address from `gh api user` as author and committer; only that commit was ever
  pushed. [Maintain the
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

- **First merge on GitHub**, pull request #1, squash-merged as `ad05308` (after the rewrite below), then synced to Gitea.
  The commit header is the pull request title followed by ` (#1)`, which confirms the hypothesis
  in [0007. Take contributions and merge pull requests on
  GitHub](../adr/0007-contributions-and-merges-on-github.md). Its author is the noreply address,
  as on every recent commit the maintainer made on GitHub's web (checked before merging); its
  author name is the name of his GitHub profile, and its committer `GitHub <noreply@github.com>`.

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
- **Author name of squash merges:** GitHub writes the name of the maintainer's GitHub profile.
  The profile name has to read `FedeAM` before the next merge, or the next squash commit carries
  another name.
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

## Later the same day: the maintainer's name is FedeAM

Request: every commit and every text should name the maintainer `FedeAM` only.

### What was done

- **History rewritten** before publication, while the repository is private: the author and the
  committer of the four commits on `main` became `FedeAM <98847815+fedeam@users.noreply.github.com>`
  (`git filter-branch --env-filter`; trees and dates unchanged): `b89d67e` → `61981a4`, `a9e4533` →
  `ad05308`, `3ff74c6` → `5878cc8`, `7e044fe` → `1672e6f`. `main` was force-pushed to both forges,
  with its protection on Gitea lifted for that push only and restored after it. This one commit,
  which renames the maintainer in the texts, went to `main` with the rewrite, outside a pull
  request: the exception that [0007. Take contributions and merge pull requests on
  GitHub](../adr/0007-contributions-and-merges-on-github.md) makes for the first commit, taken once
  more by the maintainer's decision.
- **Texts:** `FedeAM` replaces the maintainer's full name in `LICENSE-MIT`, `AGENTS.md` (including
  the rule on the only identity allowed), the ADRs, the front page and the how-tos.

### Pending

- **GitHub keeps the old commits** of pull requests #1 to #3 under `refs/pull/<number>/head`, with
  the previous author names; the maintainer chose not to recreate the repository. Step 3 of
  [Publish the repository on GitHub](../how-to/publish-the-repository.md) will list them.
- **The GitHub profile name** of the maintainer has to read `FedeAM` before the next squash merge,
  which takes its author name from it.

## Later the same day: the first run against a live cluster

Request: test `kausa run` on a real cluster, then carry on.

### What was done

- **How it ran, without copying credentials:** `kubectl proxy` on `127.0.0.1` in front of the
  maintainer's cluster (Kubernetes v1.36.5, k3s, three Raspberry Pi 4 nodes), with
  `--reject-methods='^(POST|PUT|PATCH|DELETE)$'` and `--accept-paths` limited to `/version`, the
  Pod and `events.k8s.io` watches and the metadata reads of ReplicaSets and Jobs; kausa used a
  kubeconfig with no credentials that pointed at the proxy. The proxy refused a read of Secrets
  and a write of an Event when probed, and refused nothing that kausa asked for: the permissions in
  [Permissions](../reference/permissions.md) are enough for what is implemented.
- **Footprint:** 18.8 to 19.8 MiB of resident memory and no measurable CPU while watching the
  whole cluster (the release binary on macOS, 11.2 MB unstripped). A first measurement, not the
  one on a Raspberry Pi planned for the fourth weekend.
- **Seeded failures** in a namespace created for the test and deleted afterwards (`kausa-test`): a
  Deployment that crash-loops (two replicas), an image tag that does not exist, a Pod that asks for
  512 GiB, a container killed for memory and a Job with `backoffLimit: 1`. kausa logged one
  incident for each, with the right owner: `D008` at once, then `D001`, `D002`, `D003` and `D005`
  65 seconds after it started (a settle window and one check).
- **Two defects found and fixed in this pull request:**
  - On Kubernetes 1.36 the kubelet reports a crash-looping container as terminated (reason
    `Error`) for much of each back-off and as waiting in `CrashLoopBackOff` only part of the time,
    so `D001` took about four and a half minutes to settle. A failed container that has already
    restarted now counts as the same crash loop.
  - An incident listed only the Pods failing at the check where it settled, so two replicas whose
    crash loops were out of step showed as `1 of 2`. It now lists every Pod seen failing while it
    was pending.
- **A real incident:** `D006` on the Deployment of Infisical in the maintainer's cluster, whose
  Pods kept failing their readiness probe during a rollout (one Pod not ready for 50 minutes). A
  true positive, left to the maintainer.
- **The GitHub profile name** of the maintainer reads `FedeAM` since this afternoon, so squash
  merges carry that name.

### Pending

- The run on a Raspberry Pi with an image built for linux/arm64 (fourth weekend).
