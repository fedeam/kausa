# Security policy

kausa reads cluster data that its users consider sensitive and sends part of it to a language
model, so its security rests on what it can read, what it lets out and what it refuses to do. This
page covers supported versions, how to report a vulnerability, what to expect, the threat model
and where secrets live.

## Supported versions

Status as of 2026-10-30: the latest release is v0.4.0. Until 1.0, only the latest release will
receive security fixes.

## Reporting a vulnerability

Report it privately through GitHub private vulnerability reporting:
<https://github.com/fedeam/kausa/security/advisories/new>.

Do not open a public issue, pull request or discussion for a vulnerability. Include the version
or commit, a minimal sanitized input that reproduces it (an event message, a log line or a
manifest with fake values), what kausa did and what you expected. If kausa sent a real secret to a
provider or wrote it into an Event, rotate that secret first; do not include it in the report.

These are vulnerabilities, not ordinary bugs:

- a value that a redaction rule should catch reaching a request, an Event or kausa's log in clear;
- any request to a host other than the Kubernetes API server, the configured endpoint and, with
  notifications on, the configured webhook;
- a notification that carries a field outside [Data sent in
  notifications](docs/reference/data-sent-in-notifications.md), a log line, an event message or
  any part of the request to the model as a field, or a real name other than the namespace's and
  the top-level owner's; or the webhook URL or token in a log line, an error, an Event or a metric;
- kausa reading, or asking for permission to read, Secrets or ConfigMaps, or writing anything but
  Events, with `--history` its own Explanations and, with `--leader-lease`, the `spec` (and the
  `resourceVersion` precondition) of its one Lease; or asking for any verb on Leases but `get` and
  `patch` on that one Lease in its own namespace;
- a value read from kausa's Lease (its holder, times or labels) reaching kausa's log, a request to
  the model, an Event, an Explanation, a notification or a metric, or a stored Lease that makes
  kausa crash;
- a replica that is not the holder of the Lease calling the model, writing an Event or an
  Explanation, or posting a notification after its term ended (20 s after its last good renewal,
  plus the 10 s bound on a write already started);
- kausa patching or deleting an Explanation that is not kausa-shaped (labelled
  `app.kubernetes.io/managed-by: kausa`, named `kausa.` and 16 lowercase hexadecimal digits, with
  a `spec` that parses), patching one whose `spec` is not the incident's, or writing one in a
  namespace other than the incident's;
- text read back from an Explanation reaching a request to the model, an Event, kausa's log or
  its deduplication cache, or a stored Explanation that makes kausa crash;
- input from the cluster (an event message, a log line, a name) that changes what kausa does
  beyond the text of one explanation: making it call the model more often than its budget, write
  or patch Events about other objects, or Events kausa did not create, skip its own validation, or
  crash.

## What to expect

kausa has a single maintainer working on it in spare time. The maintainer aims to acknowledge a
report within a week, to keep you informed while it is investigated, and to agree on a disclosure
date with you. There is no bug bounty and no guaranteed time to a fix.

## Threat model

Status as of 2026-10-30: v0.4.0 is released, with the Deployment watch of `D014`, the richer
context for the model and the optional standby replica. `kausa run` reads Pods,
their logs (crash loops, out-of-memory kills and failed Jobs only), Events, the metadata of
ReplicaSets and Jobs and, unless `--no-rollouts`, Deployments; it
redacts each incident's context and sends it only to the endpoint given with `--endpoint`, or
logs it with `--dry-run`; it logs incidents and explanations with real names to its own stdout.
Calls to the model are capped per hour and per day, explanations of repeated incidents are
reused, and the queue in front of the model holds 16. It writes the explanation as an Event in the
failing workload's namespace (`create` and `patch` on Events only), and serves metrics only with
`--metrics-address`. With `--history` (off by default; the chart's `history.enabled`), it also
keeps each explained incident's full explanations in an Explanation object in that namespace and
deletes old ones ([0014. Keep each incident's full explanations in a namespaced Explanation
resource, off by default](docs/adr/0014-explanation-history.md)), since v0.3.0. With
`--notify-url-file` (off by default; the chart's `notifications.enabled`; since v0.3.0), it also
posts each new explanation to one webhook, with the real names of the namespace and the top-level owner ([0015.
Post each new explanation to one optional webhook, as JSON, Slack or
Telegram](docs/adr/0015-notifications.md)). Each of these is implemented: the redaction, the prompt's validation, the
budget, the Event writes and the metrics responder have unit tests, and the permissions are
checked by the kind test (`kubectl auth can-i`, what is allowed and what must be refused) in both
`rbac.scope`s: every namespace with `cluster`, or the listed namespaces only with `namespaced`
([0013. Offer a namespaced install mode: one watch pair and one Role per listed namespace, and
nothing cluster-wide](docs/adr/0013-namespaced-install-mode.md)). Since v0.4.0, kausa also
watches Deployments, unless `--no-rollouts` (the chart's `rollouts:
false`), to explain a rollout past its progress deadline (`D014`) ([0018. Explain a Deployment past
its progress deadline (`D014`) from a Deployment watch that is on by default, and add nothing for
StatefulSets or DaemonSets](docs/adr/0018-stuck-rollouts.md)). Also since v0.4.0, the chart's
`replicaCount: 2` (off by default) runs a second replica as a standby: both run `kausa run
--leader-lease <release namespace>/<full name>-leader`, and only the one that holds that Lease
watches, explains and writes; the other reads the Lease every 5 s and takes it over if the leader
stops renewing it ([0019. Offer an optional standby replica that takes over through a
chart-created Lease, elected by kausa's own code](docs/adr/0019-high-availability.md)). Not
tested yet: a hosted provider over HTTPS (the distroless CA bundle with rustls), clusters larger
than about a hundred Pods, and long runs ([Architecture](docs/explanation/architecture.md)).

**Who controls kausa's inputs.** Anyone who can create or change a workload in a watched namespace
controls event messages, log lines, container and volume names, image references, exit codes,
probe paths and node selector, affinity and toleration keys, and through them the Pod status messages the kubelet writes (an eviction names the container or the
`emptyDir` volume). In a shared
cluster that includes tenants who are not cluster administrators. The language model endpoint
controls the answers. kausa treats both as untrusted. With `replicaCount: 2`, whoever can write
Leases in the release namespace (the default `edit` and `admin` roles there, as the reviewer of
[0019. Offer an optional standby replica that takes over through a chart-created Lease, elected by kausa's own code](docs/adr/0019-high-availability.md) found in kind, Kubernetes v1.36.4; not re-run since)
decides which replica acts, and can stop the watches in every replica: by holding kausa's Lease
under another identity, by changing its version faster than every 30 s (`LEASE_DURATION`) while
the leader is dead, so that no standby takes over, or by deleting the Lease or the `<full
name>-leader` RoleBinding. kausa reads only the holder and the `resourceVersion` of the Lease,
compares the holder with its own Pod name and echoes the version in its next patch; nothing else
in it is read, and nothing read from it is logged, sent or reused.

**What kausa protects:**

- **Data in the cluster.** Log lines, event messages, names and addresses, which may carry
  secrets, customer data and the shape of the infrastructure.
- **The provider's API key**, which costs money and may give access to other data at the provider,
  and, with notifications on, **the webhook URL and its bearer token**: either one is enough to
  post to the channel as kausa (Telegram puts the bot token in the URL).
- **The cluster itself**, against a controller that is tricked into acting.
- **The operator's trust in the explanation**, against text crafted to mislead a person.

kausa must:

- **Never read what it does not need.** Its permissions are a fixed list: read Pods, read and
  write Events, `get` on ReplicaSets and Jobs, `list` and `watch` on Deployments only while the
  Deployment watch is on (no `get`, no write, nothing on StatefulSets or DaemonSets), `get` on
  `pods/log` only while log collection is on, `list`, `create`, `patch` and `delete` on its own `explanations` only while it writes
  history, and `get` and `patch` on its one Lease, `<full name>-leader` in its own namespace,
  only with `replicaCount: 2` (the same Role in both scopes; never `create`, `update`, `list`,
  `watch` or `delete` on Leases, and no other Lease). Never any verb on CustomResourceDefinitions: the chart installs the CRD, kausa never
  does. Never Secrets, never ConfigMaps, no other write verb, no `exec`, no wildcards
  ([Permissions](docs/reference/permissions.md), [0009. Run with read-only permissions that never
  include Secrets](docs/adr/0009-least-privilege-rbac.md)), in every namespace with
  `rbac.scope=cluster`, or only in the namespaces of `rbac.namespaces` with
  `rbac.scope=namespaced`, where the chart creates nothing cluster-scoped. `rbac.scope` has no
  default, so an upgrade that resets the values fails instead of widening a namespaced install to
  the whole cluster: keep the `rbac` values in a values file passed on every upgrade, or upgrade
  with `--reuse-values`. Pod and Deployment objects arrive whole from the API
  server, inline environment values and arguments included; kausa reduces each to the small record
  in [Architecture](docs/explanation/architecture.md) at once, and never logs, formats or returns
  a raw object. A Deployment's record keeps no template, label, other annotation or condition
  message, and is kept only while the Deployment is stalled.
- **Never send anything unredacted to the model, and only to the configured places.** A request carries only the fields in
  [Data sent to the provider](docs/reference/data-sent-to-the-provider.md), with names replaced by
  pseudonyms and free text passed through the redaction rules
  ([Detection and redaction rules](docs/reference/rules.md), [0008. Send only allowlisted,
  redacted data to one OpenAI-compatible endpoint](docs/adr/0008-data-leaving-the-cluster.md)).
  Nothing unredacted reaches the model. The webhook, when notifications are on, receives only
  the fields of [Data sent in notifications](docs/reference/data-sent-in-notifications.md), with
  the real names of the namespace and the top-level owner; every other name stays a placeholder,
  and no field holds a log line, an event message or the request. The only destinations are the
  API server, the configured endpoint and the configured webhook: no telemetry, no update checks,
  no fallback provider, no proxy, no redirects to another host. HTTPS with certificate
  verification, unless plain HTTP is allowed explicitly (`--allow-http`, `--notify-allow-http`)
  for a receiver inside the cluster or the same private network.
- **Treat cluster text as data, never as instructions.** Untrusted values sit in one JSON document
  in the request, marked as untrusted; the system message says they may contain instructions to
  ignore. The model has no tools and no functions, and kausa never acts on an answer: it validates
  it against a fixed schema, caps its length, strips control and invisible formatting characters
  and markup, and writes it only as plain text in an Event and in its own log.
- **Bound its cost and its resources.** A call budget per hour and per day, a cap on input tokens
  per request and on output tokens, deduplication of repeated incidents, one request at a time
  and a bounded queue, so that a tenant who makes Pods fail on purpose cannot run up the bill or
  the memory. The budget and the deduplication cache live in memory, so they hold per process
  lifetime: a restart starts them over. With two replicas each process keeps its own, across its
  own terms as leader: a replica that steps down and leads again keeps its count and its cache,
  so a release spends at most twice `budget.callsPerHour` and `budget.callsPerDay` while both
  processes live. Watches keep small records instead of whole objects; log
  tails, requests, answers and caches are capped. Hitting a limit is counted in the metrics and
  never crashes kausa.
- **Never explain itself.** Events with kausa's own reporting controller are ignored, so its
  output cannot feed back into its input.
- **Act only within the namespace an Event is in.** A Warning Event counts only when its
  `metadata.namespace` is non-empty and equal to the namespace of its `regarding` object. Events
  about cluster-scoped objects and Events about objects in another namespace are ignored, so
  creating Events in one namespace does not make kausa group incidents, look up owners, read logs
  or write its Event in another.
- **Keep the API key to itself**, and the webhook URL and token. The key reaches the container
  through a Secret that the kubelet mounts or injects; kausa sends it only to the configured
  endpoint, in the `Authorization` header, and never logs it or writes it into an Event. The
  webhook URL and token reach it only as files mounted from a Secret, never as a flag value or an
  environment variable; kausa posts only to that URL, sends the token only to it, and never logs
  either one (the `started` line shows the host alone) or puts it in an error, an Event or a
  metric.
- **Run with the least privilege in the pod too.** A minimal image with no shell, a numeric
  non-root user, a read-only root filesystem, no capabilities, no privilege escalation, the
  `RuntimeDefault` seccomp profile and declared resource limits. The chart offers a NetworkPolicy
  that limits egress to the API server, DNS, the endpoint and, with notifications, the webhook: by Pod and namespace selector for an
  endpoint in the cluster, and by IP block and port otherwise. Standard NetworkPolicy cannot
  select a hostname, so a hosted endpoint whose addresses change needs a CNI with FQDN policies or
  an egress proxy to be limited to that host.

**What kausa cannot promise:**

- **A stolen token holds or flips its one Lease** (with `replicaCount: 2`). It can hold kausa's
  Lease, so that no replica acts, or flip it between identities to force failovers, each of which
  costs at most the duplicates below; and, since RBAC cannot narrow `patch` to `spec`, change the
  Lease's labels and annotations (checked in kind, Kubernetes v1.36.4). Nothing more: it cannot
  create, list, watch or delete Leases, touch another Lease, another namespace or anything
  cluster-scoped. A flip never resets a budget: each process keeps its own across its terms.
- **Whoever can write Leases in kausa's namespace can stop it** (with `replicaCount: 2`), in every
  replica and with no handover, by holding the Lease under another identity or by changing its
  version faster than `LEASE_DURATION` (30 s) while the leader is dead. The reviewer of
  [0019. Offer an optional standby replica that takes over through a chart-created Lease, elected by kausa's own code](docs/adr/0019-high-availability.md) found that the default `edit` and `admin`
  ClusterRoles grant every verb on `leases` in kind, Kubernetes v1.36.4 (reported, not re-run
  here). kausa fails closed, as scaling it to zero would (no explanation, no write, nothing sent),
  and it shows as `sum(kausa_leader) != 1` over both Pods and as `standby` lines in both logs: the
  alerts in [Install kausa with Helm](docs/how-to/install-with-helm.md#run-a-standby-replica).
  The same people can already scale the Deployment to zero.
- **A deleted Lease or `<full name>-leader` RoleBinding stops kausa until `helm upgrade`**
  (with `replicaCount: 2`): every replica stays a standby, fails closed and logs `lease error`
  with a `hint` that names the fix (`helm upgrade` for a missing Lease, the permission for a
  `403`). kausa never creates the Lease.
- **An API server outage of 20 s or more steps the leader down** (with `replicaCount: 2`): Lease
  calls failing for `RENEW_DEADLINE` (20 s) end its term, and it drops the records and settle
  progress that a single replica keeps across a watch error. After the outage a replica takes the
  Lease, lists again and settles its incidents again.
- **A failover may explain an incident once more per process** (with `replicaCount: 2`). The
  deduplication cache is per process, so a failover, forced or not, can make one more call, one
  more Event series patch or new Event, one more history merge and one more notification for an
  incident that still fails. One that ended in the gap is not explained.
- **A deposed leader is bounded by time, not fenced** (with `replicaCount: 2`). It starts no write
  after 20 s from its last good renewal, and each write ends within 10 s, so before a standby may
  take over (30 s). That rests on two hypotheses: the API server does not apply a request after
  the client gave up on it, and the monotonic clock of a paused VM advances. There is no fencing
  token; the worst case is the duplicates above, never a change to a workload.
- **The metrics port gains no new way to stop the watches.** The leader's controller, the
  metrics `accept` and its connections share the process at random, as with one replica, and the
  Lease keeper is polled before them only to renew or to step down, so a connection flood from
  any peer that `networkPolicy.metricsFrom` allows (any, by default) slows the controller no more
  than with one replica, and cannot keep a starved leader holding the Lease. Measuring this under
  the 200m CPU limit is an open item before release (the research note that
  [0019. Offer an optional standby replica that takes over through a chart-created Lease, elected by kausa's own code](docs/adr/0019-high-availability.md) requires).

- **Redaction has false negatives.** A secret in a format no rule knows, split across lines or
  encoded, can reach the provider. Every rule ships with a test corpus, and `--dry-run` shows the
  exact requests before a provider is enabled. Where any leak is unacceptable, use a model served
  inside the cluster or the private network: then nothing leaves it.
- **Paths and keys leave as the workload wrote them.** A probe's path (for `D001` and `D006`)
  and an unschedulable Pod's node selector, affinity and toleration keys (for `D005`) are sent
  redacted, but a customer or host name in them that no pattern and no known name matches
  reaches the provider; the query and fragment of a URL in event text or a log, such as the probe
  URL that a kubelet `Unhealthy` note quotes on a connection error, are redacted (`R014`)
  ([0017. Send the model resource requests, probe handlers,
  image pull settings and an unschedulable Pod's scheduling keys, reduced and
  redacted](docs/adr/0017-richer-context-for-the-model.md)). Their values are never sent, except
  the fixed `kubernetes.io/os` and `kubernetes.io/arch` ones.
- **A misleading explanation is still possible.** Injected text can shape what the model writes,
  for example a suggested command that does harm. kausa never runs anything, and the explanation
  is advice for a person, who has to read it as such.
- **The provider sees what is sent.** Its retention and training policies are the provider's.
  kausa documents what it sends; choosing the provider is the operator's decision.
- **The webhook's operator learns what a notification says.** With notifications on, the real
  names of the namespace and the top-level owner, the rule, the fingerprint and the explanation
  leave the cluster for the webhook's operator (Slack, Telegram or your own receiver), under its
  retention. Telegram's cloud chats, private and group, use server-client encryption, not end to
  end (that a bot cannot take part in a secret chat is a hypothesis: Telegram does not say). "Fully local" then also means notifications off or a receiver in the cluster. A
  wrong URL sends explanations to whoever owns it; a leaked one lets anyone post as kausa: rotate
  it at the provider.
- **A notification can repeat redacted log text.** No field holds a log line, but the explanation
  is the model's text, which can quote the redacted context it was given (log and event text,
  probe paths, label keys), up to the explanation's caps. kausa cannot tell a quote from a summary.
- **A notification can still mention someone or show a link.** The Slack text escapes `&`, `<`
  and `>`, sends no `link_names`, asks for `"parse": "none"` and carries an invisible word joiner
  after every `@`; the Telegram text has no markup and carries one after every `@` and `/`. Both
  turn previews off. A real Slack post on 2026-10-30 showed a URL as plain text but a plain
  `@here` and `@channel` notifying the channel, which the joiner is meant to stop (not yet seen in
  a real post); in Telegram, a real post showed the joiners keeping mentions and commands inert
  and a URL staying a link ([Data sent in
  notifications](docs/reference/data-sent-in-notifications.md#what-slack-and-telegram-do-with-it)).
  Injected text can also make a notification misleading, as with an Event.
- **The budget is shared.** The call budget and the queue are one per release, shared by every
  namespace it watches (the whole cluster, or the listed namespaces), so a tenant that makes
  many workloads fail can use up the budget and push other namespaces' incidents out of the queue:
  their failures then get an `ExplanationSkipped` Event or none. It protects the bill, not
  fairness between namespaces; separate releases with disjoint `rbac.namespaces` have separate
  budgets.
- **The history can be rewritten or deleted by a compromised kausa** (with `--history`). RBAC
  cannot narrow `patch` and `delete` on `explanations` to kausa's own objects, so a stolen kausa
  token can rewrite or delete any Explanation, or add `finalizers` that stall its deletion (and,
  as a hypothesis not checked, the deletion of the CRD on uninstall), in the namespaces it covers,
  and nothing more: no other
  resource, and no verb on CustomResourceDefinitions. kausa itself patches only an object that
  carries its label and the incident's `spec`, sends only `status` and `metadata.resourceVersion`,
  and deletes only kausa-shaped objects, each delete with a `resourceVersion` precondition.
- **The label and the name are a convention, not proof of authorship.** Whoever can create
  Explanations in a covered namespace (a cluster administrator, or anyone granted it) can forge
  kausa-shaped objects. kausa then appends to or deletes them like its own, but never writes
  their text back: a stored entry is untrusted, and one that is malformed, future-dated, oversized
  or changed by kausa's cleaning is dropped, and nothing read back is logged or sent.
- **A forged Explanation is bounded by etcd, not by the schema.** The schema bounds `spec` and
  `status` but not `metadata`, so an object created with kausa's label can carry labels,
  annotations or finalizers up to etcd's object limit, about 1.5 MiB. The sweep lists 2 objects
  per page, which kube buffers and parses whole, so a page of such objects stays under about
  30 MiB, within the chart's 192Mi memory limit (measured on 2026-10-28), and it drops the
  `metadata` it does not read before it reduces each object. The 55 KB of the [Explanation
  resource](docs/reference/explanation-resource.md) holds for the objects kausa writes only.
- **The history's object cap is shared.** `MAX_OBJECTS` (500) counts every kausa-shaped object in
  the namespaces one kausa covers, so a tenant that makes many workloads fail, or that creates
  kausa-shaped objects, can push other namespaces' older history out. It protects etcd, not
  fairness. A ResourceQuota on `count/explanations.kausa.fedeam.com` bounds the count in a
  namespace.
- **The history keeps text with real names readable for days.** An Explanation holds the whole
  explanation, with real names mapped back, and the `--model` value, readable for up to
  `--history-days` (7 by default, at most 30) by whoever reads Events there: the chart aggregates
  `get`, `list` and `watch` on Explanations to `view` unless `history.readers.aggregate` is false.
  Nothing prunes it while kausa is not running, or runs without `--history`; in the chart the CRD,
  and with it every Explanation, exists only while kausa writes history. Backups of etcd or of the
  cluster's objects copy the Explanations, with their real names (hypothesis: it depends on the
  backup tool).
- **A stolen token reads whole Deployments** (with the Deployment watch on, the default). In the
  namespaces kausa covers, its token can list and watch Deployments: their Pod templates with
  inline environment values and arguments, their annotations (`last-applied-configuration`
  included, which repeats the whole applied object), and Deployments scaled to zero that no Pod
  shows. Still no Secret or ConfigMap. `rollouts: false` removes the grant.
- **Stalls can be provoked or forged.** Whoever can edit a Deployment can make it pass its
  deadline in seconds (`progressDeadlineSeconds: 1`), and whoever can write `deployments/status`
  can forge the condition. Each costs one call within the shared budget, as a crash loop does. No
  Deployment text reaches the model, an Event, the webhook or the log: the request carries only
  integers, the revision is a parsed integer and the condition's reason is only compared.
- **Stalls can be delayed.** `D014` yields to an explained incident of the same Deployment and
  waits while one is pending but not settled. Whoever can create Warning Events about the
  Deployment's Pods (or its ReplicaSets) in its namespace, or make one Pod's probe fail, can delay
  it for as long as they keep producing fresh, distinct events that never settle; it comes 10
  minutes after the last one. They cannot silence it for good: only an explained incident does,
  and the same writer can already forge explanations with events.
- **A list page is held whole.** Records are small, but each watch decodes list pages of up to 500
  whole objects, and no Pod quota bounds Deployments scaled to zero (hypothesis: an object is
  capped at about 1.5 MiB, annotations at 256 KiB). This holds for the Pod watch already; smaller
  pages for all three watches are a follow-up.
- **The hard cost limit is the provider's.** kausa's budget starts over when kausa restarts, and a
  kausa that restarts often can spend more than its daily cap; with `replicaCount: 2`, each
  replica has its own, so a release can spend up to twice its caps while both run. Operators should set a spending cap
  or rate limit on the provider key.
- **Events are visible to everyone who can read Events** in that namespace. An explanation only
  repeats names already visible on the object, and never a value that redaction removed. It
  summarizes the incident's redacted event messages and log lines, so readers of Events in that
  namespace learn about log content even without `pods/log`.
- **kausa's own log holds the full explanations**, with real names, from every watched namespace.
  Whoever can read kausa's Pod logs, and any log shipper that exports them, sees them: restrict
  access to kausa's namespace accordingly.
- **Event writes cannot be limited to kausa's own Events.** `create` and `patch` on Events
  cannot be narrowed by RBAC to kausa's own, since RBAC has no filter on the reporting controller,
  so a compromised kausa could create or rewrite any Event in the namespaces it covers: every
  namespace with `rbac.scope=cluster`, the listed namespaces only with `rbac.scope=namespaced`. kausa names
  its Events `kausa.<fingerprint>.<hash of the note>` (or `kausa.skipped.…`), patches one only
  when it already exists with that name and with reporting controller `kausa`, refuses the patch
  otherwise, and patches only its series (count and last time); it never changes another
  Event's note.
- **Tenants of a listed namespace can change kausa's access there** (`rbac.scope=namespaced`).
  kausa's Role and RoleBinding live in that namespace, so whoever administers it can:
  - **delete the RoleBinding.** kausa's watches there are refused (`403`); it drops what they had
    seen and keeps working in the other namespaces. After its next restart it never becomes ready,
    so `helm upgrade --wait` times out and `--atomic` rolls back, security fixes included. The
    way out is to remove the namespace from `rbac.namespaces`, or to restore the binding, then
    upgrade; the `pod watch error`, `event watch error` and `deployment watch error` lines name
    the namespace;
  - **edit the Role**, which gives a stolen kausa token more in that namespace; compare it with
    `helm get manifest`. An upgrade with the same values put an edited Role back in kind on
    2026-10-28 (Helm 3.16.1);
  - **bind more roles to kausa's service account**, as was already possible in cluster scope.

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
- Pull requests are built and tested on GitHub's hosted runners; an outside contributor's runs
  wait for the maintainer's approval. The maintainer's self-hosted runner builds and tests `main`, its dependencies
  included, with network access, and only after the maintainer has reviewed and merged a change
  on GitHub and copied `main` to the private Git server that the runner serves; code from a
  contributor's pull request never reaches it before that.
- If the project ever needs a secret (for example, to sign releases or to push the image), it
  lives in the maintainer's secret manager (Infisical) and reaches CI as a repository secret. Never
  in git.
- The repository is scanned with `gitleaks`, and fixtures and test corpora use only fake values,
  never real cluster output.

## Exposure

Nothing is deployed by this repository: kausa runs in the clusters of whoever installs it, and it
serves only its own metrics and health endpoints, inside the cluster. The GitHub repository is
public; the copy that runs the arm64 CI stays private.
