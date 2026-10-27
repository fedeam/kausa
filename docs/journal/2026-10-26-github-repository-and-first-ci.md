# 2026-10-26 — GitHub repository and first CI runs

Create the private GitHub repository, push the first commit to both forges and check the first CI
runs. Repositories touched: kausa.

## What was done

- **The first commit**, `13ed6e7` (`chore(repo): set up repository skeleton, docs and ci`), the
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

- **First merge on GitHub**, pull request #1, squash-merged as `a20bcf4` (after the rewrite below), then synced to Gitea.
  The commit header is the pull request title followed by ` (#1)`, which confirms the hypothesis
  in [0007. Take contributions and merge pull requests on
  GitHub](../adr/0007-contributions-and-merges-on-github.md). Its author is the noreply address,
  as on every recent commit the maintainer made on GitHub's web (checked before merging); its
  author name is the name of the maintainer's GitHub profile, and its committer `GitHub <noreply@github.com>`.

## Decisions

- [0008. Send only allowlisted, redacted data to one OpenAI-compatible
  endpoint](../adr/0008-data-leaving-the-cluster.md) and [0009. Run with read-only permissions
  that never include Secrets](../adr/0009-least-privilege-rbac.md) accepted by the maintainer,
  with log collection on by default for the crash-type and Job detections only.
- The maintainer delegated the rest of the work to agents on 2026-10-26: they merge their own pull
  requests and sync `main`, and ask only about decisions. The repository stays private until the maintainer
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
  maintainer can run `kausa run` themselves (read-only), or allow kind; otherwise the kind tests of
  the third weekend are the first live run.
- **Hypotheses from the first weekend:** that memory stays flat with the number of healthy Pods
  (measured on the fourth weekend); that the kubelet and scheduler reasons in the rules are the
  ones current releases emit.

## Later the same day: the maintainer's name is FedeAM

Request: every commit and every text should name the maintainer `FedeAM` only.

### What was done

- **History rewritten** before publication, while the repository is private: the author and the
  committer of the four commits on `main` became `FedeAM <98847815+fedeam@users.noreply.github.com>`
  (`git filter-branch --env-filter`; trees and dates unchanged): `b89d67e` → `13ed6e7`, `a9e4533` →
  `a20bcf4`, `3ff74c6` → `40015aa`, `7e044fe` → `c038200`. `main` was force-pushed to both forges,
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

## Later the same day: the second implementation weekend

Request: carry on with v0.1.

### What was done

- **Redaction** (`src/redact.rs`): pseudonyms for the names kausa knows (`R002`), numbered in order
  of appearance and mapped back in the answer, and the patterns `R003` to `R013`, with a corpus of
  invented lines that must and must not be redacted ([Detection and redaction
  rules](../reference/rules.md)). Built on the `regex` crate, whose engine runs in linear time.
- **Context** (`src/context.rs`): the allowlisted fields of [Data sent to the
  provider](../reference/data-sent-to-the-provider.md), at most three Pods and ten events, and the
  failed container's last 50 log lines (32 KiB asked for, 4 KiB kept after redaction) for the
  crash-type and Job detections.
- **Prompt and validation** (`src/prompt.rs`): a fixed system message (version `1`) that says the
  data is untrusted, the context as one JSON document under `untrusted_cluster_data`, no tools,
  `response_format` with a JSON schema unless `--no-json-schema`, and an answer validated and
  cleaned (NFC; control, bidirectional, zero-width and markup characters removed; lengths cut).
- **Client** (`src/model.rs`): reqwest with rustls and ring, no redirects, no proxy from the
  environment, timeouts and a 256 KiB cap; plain HTTP only with `--allow-http`; the API key from
  `--api-key-file` or `KAUSA_API_KEY`.
- **Orchestration** (`src/explain.rs`): explanations run one at a time inside the controller's
  loop, with a queue of 16, and log `request` (dry-run), `explanation` or `explanation skipped`.
- **Configuration as flags:** the configuration file of the planned design was dropped; the flags
  are in [Command line and log format](../reference/command-line.md).
- **Review before the pull request:** a reviewer tried to get secrets and names through the
  redaction with concrete strings, a second reviewed the client, the prompt and the
  orchestration, a third the documentation against the code, and a skeptic re-checked every
  finding: 33 confirmed and fixed, 4 rejected. The main ones: known names in uppercase
  (`PAYMENTS_SERVICE_HOST`) or after a percent escape got through; hexadecimal secrets could never
  reach the entropy threshold; `redis://:password@host` and passwords with `@` or `/` got through;
  common keys (`SECRET_KEY`, `MYSQL_PWD`, `passphrase`), escaped JSON, `--password value`, XML
  elements and cookies got through; Services in another namespace (`svc.ns:port`, `http://svc.ns/`)
  got through; node and service account names were not replaced although ADR 0008 says so;
  **reqwest read `HTTP_PROXY` from the environment** although the docs said otherwise (the client
  now uses `no_proxy()`); a known name inside a secret's key could break the key apart (secrets now
  run before names); `restore` could chain and could map placeholder-shaped text from the cluster
  to real names; restored names skipped the cleaning; the log read failed on invalid UTF-8 and the
  API server's byte limit cut the end of the log; and chat-template markers (`<|im_start|>`) went
  through to the model.
- **Tests:** 70 unit tests and 8 command-line tests (7 outside Linux). A command-line test had started `kausa run
  --dry-run` against the developer's current kubeconfig context for about ten minutes, read-only,
  after `--dry-run` became a valid flag: the tests now run the binary with a nonexistent
  kubeconfig and without the in-cluster environment, so they can never reach a cluster, and
  `AGENTS.md` says so.
- **Live test**, read-only through the restricted `kubectl proxy`, now with the Pod log path, the
  same seeded failures in a namespace created and deleted for it, and Ollama 0.9.2 with
  `qwen3:0.6b` on the developer's machine: five incidents, five dry-run requests with no real
  name, address or node in them, and five valid explanations with structured output (about 330 to
  520 tokens in and 130 to 190 out each). The model capitalized a placeholder (`Pod-1`), which the
  restore missed; fixed. The answers are those of a 0.6B model; quality is the model's.
- Memory while watching the whole cluster and explaining: 22.1 MiB resident.
- [Try kausa against a cluster with a local model](../how-to/try-with-a-local-model.md), new.

### Pending

- The third weekend: call and token budget, deduplication, metrics, Events on the failing object,
  Helm chart, multi-architecture image, kind tests in CI.
- `R011`'s token formats are to be checked against each issuer's documentation.

## Later the same day: the code of the third implementation weekend

Request: carry on with v0.1. The maintainer chose, for this weekend: a Helm chart, an image on
distroless static (non-root), the image only built in CI until publication, and the kind tests on
GitHub Actions; those come in their own change.

### What was done

- **Budget** (`src/budget.rs`): model calls per hour and per day over sliding windows
  (`--max-calls-per-hour` 30, `--max-calls-per-day` 200), and a cap on a request's estimated
  input tokens (`--max-input-tokens` 6000): the log tail is halved, then Pods and events dropped,
  until it fits.
- **Deduplication:** up to 1024 explanations by fingerprint, for `--dedup-hours` (6); a repeated
  incident is explained again from the cache, with no call.
- **Events** (`src/events.rs`): `Explained` (Normal) or `ExplanationSkipped` (Warning) Events,
  `regarding` the top-level owner and `related` the Pod, named after the fingerprint and a hash
  of the note: the note of an Event is immutable, so a repeat with the same text patches only the
  series, and new text is a new Event; `--no-events`.
- **Metrics** (`src/metrics.rs`): ten counters, `/healthz` and `/readyz`, with
  `--metrics-address`, served by a minimal responder on `tokio::net`.
- **Live test**, through the `kubectl proxy`, now letting `POST` and `PATCH` on Events in the test
  namespace only, with the same seeded failures and Ollama, and `--max-calls-per-hour 3`: five
  incidents, three explanations and two skipped for the budget, five Events in the cluster with the
  expected names, owners and Pods, the metrics matching (1337 tokens in, 485 out), `/readyz` at
  200, and 22.5 MiB resident. `kubectl describe deployment` showed the `Explained` Event next to
  the Deployment's own events. The test namespace and its Events were deleted afterwards; no kausa
  Event was left anywhere else.
- **Review** of the third weekend's code before the pull request, by independent reviewers whose
  findings were checked by others: 16 confirmed, 2 rejected, all fixed. The ones that changed the
  code: the first version patched the note of a repeated Event, which the API server refuses (now
  the name carries a hash of the note, and only the series is patched); an Event with kausa's name
  but written by someone else would have been patched (now refused); a slow metrics client held
  the loop (now up to 8 connections are answered beside it, and a failed accept waits 100 ms); the
  budget and the cache used the wall clock (now the monotonic one); and `--metrics-address` is
  checked when the flags are parsed. The rest were texts that still spoke of the second weekend.
  Checked on the live cluster afterwards, in a throwaway namespace: a merge patch of the series
  alone is accepted, a patch of the note is refused (`field is immutable`), and the field selector
  on `metadata.name` finds the Event.
- **Tests:** 83 unit tests and 8 command-line tests (7 outside Linux).

### Pending

- The image, the Helm chart and the kind tests: done the same day, below.

## Later the same day: the image, the Helm chart and the kind test

The rest of the third weekend, with the choices above
([0010. Ship a static binary on distroless, with a Helm chart, built in CI and not published
yet](../adr/0010-packaging.md)).

### What was done

- **Image:** the Dockerfile's `runtime` target, a static musl release build on distroless static
  `nonroot`, after the `ci` target so that the classic builder's `--target ci` does not compile
  it. Built locally for linux/arm64: 9.6 MB uncompressed, 4.3 MB compressed, the binary
  `statically linked`, user 65532, 1 minute 28 seconds on the maintainer's machine.
- **Live test of the image**, as a container with a read-only root filesystem and no
  capabilities, against the seeded failures through the `kubectl proxy` (now also accepting the
  host name Docker Desktop gives the host), with Ollama and `--max-calls-per-hour 3`: five
  incidents, three explanations and two skipped for the budget, five Events named
  `kausa.<fingerprint>.<hash>` (`kausa.skipped.…` for the two skipped), the proxy refusing none of
  kausa's requests, and 4.4 MiB of memory in the container (`docker stats`). The test namespace
  was deleted afterwards.
- **Helm chart** in `charts/kausa`, with its reference and an install guide. Every combination of
  the values that change the arguments was rendered and passed to the binary, which accepted
  them; the binary takes a flag and its value as two arguments, not `--flag=value`, so the chart
  writes them that way. A server-side dry run of the rendered chart (with the NetworkPolicy)
  against Kubernetes v1.36 (k3s) passed, creating nothing.
- **CI:** helm 4.3.1, pinned, in the `ci` target, which lints the chart and renders it with four
  value sets; the Gitea job builds the arm64 image after the checks; on GitHub, an `image` job
  builds it natively on amd64 and arm64, and an `e2e` workflow runs `tests/e2e/run.sh` in kind
  (pinned kind v0.33.0 and `kindest/node` v1.36.4). Every pin was checked against its published
  checksum or digest.
- **The end-to-end test** did not run here: kind is not installed on the maintainer's machine,
  and installing it was left out of this session. It runs on GitHub Actions once they run for the account; until then
  the chart is checked by the lint, the renders, the parser and the server-side dry run.
- The texts that said "he" or "his" of the maintainer now say "the maintainer" or "their".
- **Review** of the change before the pull request, with each finding checked by another
  reviewer: 11 confirmed, 2 rejected, all fixed. The ones that changed behaviour: the metrics
  listened on `0.0.0.0` only, so on an IPv6 cluster the probes would have failed (now `[::]` by
  default, which accepts both families, checked in the image, and `metrics.bindAddress` to
  change it); and the kind test could not have passed on Docker Desktop, where the kind
  network's gateway is not the host (now the script uses the address the nodes give
  `host.docker.internal` there, a hypothesis until it runs). The rest were texts that still
  called the image and the chart future.

### Pending

- The first run of the kind test and of the GitHub image job (GitHub Actions, from 2026-11-01).
- The fourth weekend: memory and CPU on a Raspberry Pi 4, and tokens per incident compared with
  k8sgpt (the comparison: done the same day, below).

## Later the same day: tokens per incident compared with k8sgpt

The maintainer asked to carry on to the end. Result and method:
[Tokens per incident compared with k8sgpt](../research/2026-10-26-tokens-compared-with-k8sgpt.md).

### What was done

- k8sgpt upgraded on the maintainer's machine from 0.3.41 to 0.4.41 (Homebrew), so that the
  comparison uses its current release.
- The seeded failures and a counting proxy in front of Ollama, added to `tests/bench/`. kausa: 5
  calls, 2,183 tokens in and 765 out. k8sgpt: 12 calls, 1,301 in and 3,827 out (1,401 and 4,035
  with `--anonymize`).
- A mistake caught while writing it up: the proxy's log showed a refused request for Secrets, but
  it was the check of the proxy itself, made by hand before k8sgpt ran; k8sgpt asked only for
  ConfigMaps. The research note says so.
- The test namespace was deleted afterwards; no kausa Event was written (`--no-events`).

- **Gitea CI after the packaging change:** run 9 stayed `running` with no process left on the
  runner, and Gitea cancelled it when the next push arrived; run 10 failed with its log cut off
  while the release build compiled `kube-core`. No memory limit was hit (the cgroup counters
  showed no OOM kill). The cause: the runner stops a job after 30 minutes (`timeout: 30m` in its
  configuration), and the checks (about 20 minutes) and the release build no longer fit in one
  job. The fix splits the Gitea workflow into `ci` and `image` (after `ci`); the `image` job
  builds only the build and runtime stages, cut from the Dockerfile, because the classic builder
  would otherwise repeat the ci stage, whose cache does not match a new checkout's `.git`.

- After that split, run 11 passed: `ci` in 21 minutes and `image` in 20.

## Later the same day: memory and CPU on a Raspberry Pi 4

Loading the image into a node's containerd over SSH was not allowed to the agent, so the
maintainer ran that command. Result and method: [Memory and CPU on a Raspberry Pi
4](../research/2026-10-26-resource-use-on-a-raspberry-pi.md).

- The chart installed in dry-run on one node, under the restricted Pod Security Standard: its
  first install on a real cluster. 22 minutes, five seeded failures at minute 10: 2.12 MiB of
  working set at the end, 0.67 seconds of CPU, five incidents, no restart.
- 15 restarts of the Event watch with `etcd event received with PrevKv=nil`, recorded in the note
  as an open question.
- The release, the namespaces and the ClusterRole were removed afterwards. The image is still in
  the node's containerd (`docker.io/library/kausa:bench`), since removing it needs the same SSH
  access.

### Pending

- The first run of the kind test and of the GitHub image job (GitHub Actions).
- Why the API server ends kausa's Event watch with `PrevKv=nil` on k3s.
- Publication, when the maintainer validates the project.

