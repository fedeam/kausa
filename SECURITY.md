# Security policy

kausa reads cluster data that its users consider sensitive and sends part of it to a language
model, so its security rests on what it can read, what it lets out and what it refuses to do. This
page covers supported versions, how to report a vulnerability, what to expect, the threat model
and where secrets live.

## Supported versions

No version has been released yet. Until 1.0, only the latest release will receive security
fixes.

## Reporting a vulnerability

Report it privately through GitHub private vulnerability reporting:
<https://github.com/fedeam/kausa/security/advisories/new>. It will be enabled when the repository
goes public; until then the repository is private.

Do not open a public issue, pull request or discussion for a vulnerability. Include the version
or commit, a minimal sanitized input that reproduces it (an event message, a log line or a
manifest with fake values), what kausa did and what you expected. If kausa sent a real secret to a
provider or wrote it into an Event, rotate that secret first; do not include it in the report.

These are vulnerabilities, not ordinary bugs:

- a value that a redaction rule should catch reaching a request, an Event or kausa's log in clear;
- any request to a host other than the Kubernetes API server and the configured endpoint;
- kausa reading, or asking for permission to read, Secrets or ConfigMaps, or writing anything but
  Events;
- input from the cluster (an event message, a log line, a name) that changes what kausa does
  beyond the text of one explanation: making it call the model more often than its budget, write
  or patch Events about other objects, or Events kausa did not create, skip its own validation, or
  crash.

## What to expect

kausa has a single maintainer working on it in spare time. The maintainer aims to acknowledge a
report within a week, to keep you informed while it is investigated, and to agree on a disclosure
date with you. There is no bug bounty and no guaranteed time to a fix.

## Threat model

Status as of 2026-10-26: two of four implementation weekends are done. `kausa run` reads Pods,
their logs (crash loops, out-of-memory kills and failed Jobs only), Events and the metadata of ReplicaSets and Jobs; it
redacts each incident's context and sends it only to the endpoint given with `--endpoint`, or
logs it with `--dry-run`; it logs incidents and explanations with real names to its own stdout.
It has no call budget or deduplication yet (third weekend): each incident is explained once while
it lasts, and the queue in front of the model holds 16. It writes no Events yet. The rest are the properties v0.1 is
designed to have ([Architecture](docs/explanation/architecture.md)); each one will be backed by
tests before the release.

**Who controls kausa's inputs.** Anyone who can create or change a workload in a watched namespace
controls event messages, log lines, container names, image references and exit codes. In a shared
cluster that includes tenants who are not cluster administrators. The language model endpoint
controls the answers. kausa treats both as untrusted.

**What kausa protects:**

- **Data in the cluster.** Log lines, event messages, names and addresses, which may carry
  secrets, customer data and the shape of the infrastructure.
- **The provider's API key**, which costs money and may give access to other data at the provider.
- **The cluster itself**, against a controller that is tricked into acting.
- **The operator's trust in the explanation**, against text crafted to mislead a person.

kausa must:

- **Never read what it does not need.** Its permissions are a fixed list: read Pods, read and
  write Events, `get` on ReplicaSets and Jobs, and `get` on `pods/log` only while log collection
  is on. Never Secrets, never ConfigMaps, no other write verb, no `exec`, no wildcards
  ([Permissions](docs/reference/permissions.md), [0009. Run with read-only permissions that never
  include Secrets](docs/adr/0009-least-privilege-rbac.md)). Pod objects arrive whole from the API
  server, inline environment values and arguments included; kausa reduces each to the small record
  in [Architecture](docs/explanation/architecture.md) at once, and never logs, formats or returns
  a raw object.
- **Never send anything unredacted, and only to one place.** A request carries only the fields in
  [Data sent to the provider](docs/reference/data-sent-to-the-provider.md), with names replaced by
  pseudonyms and free text passed through the redaction rules
  ([Detection and redaction rules](docs/reference/rules.md), [0008. Send only allowlisted,
  redacted data to one OpenAI-compatible endpoint](docs/adr/0008-data-leaving-the-cluster.md)).
  The only destinations are the API server
  and the configured endpoint: no telemetry, no update checks, no fallback provider, no redirects
  to another host. HTTPS with certificate verification, unless plain HTTP is allowed explicitly
  for an endpoint inside the cluster or the same private network.
- **Treat cluster text as data, never as instructions.** Untrusted values sit in one JSON document
  in the request, marked as untrusted; the system message says they may contain instructions to
  ignore. The model has no tools and no functions, and kausa never acts on an answer: it validates
  it against a fixed schema, caps its length, strips control and invisible formatting characters
  and markup, and writes it only as plain text in an Event and in its own log.
- **Bound its cost and its resources.** A call budget per hour and per day, a cap on input tokens
  per request and on output tokens, deduplication of repeated incidents, one request at a time
  and a bounded queue, so that a tenant who makes Pods fail on purpose cannot run up the bill or
  the memory. The budget and the deduplication cache live in memory, so they hold per process
  lifetime: a restart starts them over. Watches keep small records instead of whole objects; log
  tails, requests, answers and caches are capped. Hitting a limit is counted in the metrics and
  never crashes kausa.
- **Never explain itself.** Events with kausa's own reporting controller are ignored, so its
  output cannot feed back into its input.
- **Keep the API key to itself.** The key reaches the container through a Secret that the kubelet
  mounts or injects; kausa sends it only to the configured endpoint, in the `Authorization`
  header, and never logs it or writes it into an Event.
- **Run with the least privilege in the pod too.** A minimal image with no shell, a numeric
  non-root user, a read-only root filesystem, no capabilities, no privilege escalation, the
  `RuntimeDefault` seccomp profile and declared resource limits. The chart offers a NetworkPolicy
  that limits egress to the API server, DNS and the endpoint: by Pod and namespace selector for an
  endpoint in the cluster, and by IP block and port otherwise. Standard NetworkPolicy cannot
  select a hostname, so a hosted endpoint whose addresses change needs a CNI with FQDN policies or
  an egress proxy to be limited to that host.

**What kausa cannot promise:**

- **Redaction has false negatives.** A secret in a format no rule knows, split across lines or
  encoded, can reach the provider. Every rule ships with a test corpus, and `--dry-run` shows the
  exact requests before a provider is enabled. Where any leak is unacceptable, use a model served
  inside the cluster or the private network: then nothing leaves it.
- **A misleading explanation is still possible.** Injected text can shape what the model writes,
  for example a suggested command that does harm. kausa never runs anything, and the explanation
  is advice for a person, who has to read it as such.
- **The provider sees what is sent.** Its retention and training policies are the provider's.
  kausa documents what it sends; choosing the provider is the operator's decision.
- **The budget is shared.** The call budget and the queue are cluster-wide, so a tenant that makes
  many workloads fail can use up the budget and push other namespaces' incidents out of the queue:
  their failures then get an `ExplanationSkipped` Event or none. It protects the bill, not
  fairness between namespaces.
- **The hard cost limit is the provider's.** kausa's budget starts over when kausa restarts, and a
  kausa that restarts often can spend more than its daily cap. Operators should set a spending cap
  or rate limit on the provider key.
- **Events are visible to everyone who can read Events** in that namespace. An explanation only
  repeats names already visible on the object, and never a value that redaction removed. It
  summarizes the incident's redacted event messages and log lines, so readers of Events in that
  namespace learn about log content even without `pods/log`.
- **kausa's own log holds the full explanations**, with real names, from every watched namespace.
  Whoever can read kausa's Pod logs, and any log shipper that exports them, sees them: restrict
  access to kausa's namespace accordingly.
- **Event writes cannot be limited to kausa's own Events.** `create` and `patch` on Events
  cluster-wide cannot be narrowed by RBAC, which has no filter on the reporting controller, so a
  compromised kausa could create or rewrite any Event in the namespaces it covers. kausa patches
  only Events it created, with its reporting controller, and the tests check it.

A missing detection or a weak explanation is a bug: open an issue. Anything in the list under
[Reporting a vulnerability](#reporting-a-vulnerability) is a security issue: report it privately.

## Secrets

- kausa needs no secrets to build or test. To run against a hosted provider it needs that
  provider's API key, which the operator stores in a Kubernetes Secret of their own and the chart
  passes to the container through the kubelet; a local model may need none.
- CI uses only the token that GitHub Actions or Gitea Actions gives each job automatically. No
  repository secrets are configured. On GitHub the job token is read-only (`contents: read`). On
  Gitea Actions the automatic token cannot be narrowed and has write access to this repository;
  the workflow uses it only to fetch the commit and removes it from the environment before the
  build.
- Pull requests are built and tested on GitHub's hosted runners, once GitHub Actions run for the
  repository. The maintainer's self-hosted runner builds and tests `main`, its dependencies
  included, with network access, and only after the maintainer has reviewed and merged a change
  on GitHub and copied `main` to the private Git server that the runner serves; code from a
  contributor's pull request never reaches it before that.
- Until GitHub Actions run, the maintainer builds a pull request locally with
  `docker build --target ci .`, only after reading it, in a disposable clone of the GitHub
  repository; that build has network access, like the runner's. A pull request that changes the
  `Dockerfile`, `.dockerignore`, `rust-toolchain.toml`, a build script (`build.rs` or the `build`
  key in `Cargo.toml`) or a workflow is not built locally: it waits for GitHub Actions.
- If the project ever needs a secret (for example, to sign releases or to push the image), it
  lives in the maintainer's secret manager (Infisical) and reaches CI as a repository secret. Never
  in git.
- The repository is scanned with `gitleaks`, and fixtures and test corpora use only fake values,
  never real cluster output.

## Exposure

Nothing is deployed by this repository: kausa runs in the clusters of whoever installs it, and it
serves only its own metrics and health endpoints, inside the cluster. The repository is private
until the project is validated, then public.
