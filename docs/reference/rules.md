# Detection and redaction rules

The deterministic rules kausa applies before any language model call: detection rules decide what
is an incident and how failures are grouped, and redaction rules decide what never leaves the
cluster.

Status as of 2026-10-26: the **detection rules, grouping and fingerprint are implemented** (first
implementation weekend, `src/detect.rs`, `src/incident.rs`, `src/fingerprint.rs`); container logs
are not read yet (the "Logs read" column is the plan for the second weekend, with the redaction
rules). Ids, triggers and defaults may change until v0.1 is
released; after that an id is never reused. How the rules fit in the
pipeline: [Architecture](../explanation/architecture.md). Why redaction works this way: [0008.
Send only allowlisted, redacted data to one OpenAI-compatible
endpoint](../adr/0008-data-leaving-the-cluster.md).

## Detection rules

A detection matches a signal: a **state** read from a Pod at every check (a container state, or
the Pod's scheduling condition), or a Warning **event** about a Pod or a Job. The failure must
persist through the **settle window** (60 seconds by default, `--settle-seconds`) before it becomes
an incident, so a container that restarts once and recovers is not explained:

- **A state** settles when it has been present, at the checks every 5 seconds, over at least the
  window. It is forgotten only after 330 seconds without it, longer than the kubelet's longest
  back-off (300 seconds), so the moments a crash-looping container runs between back-offs do not
  split one failure into several incidents.
- **An event** settles when the same event happens again at least a window after it was first
  seen, by kausa or in its own series (`count` above 1 and a first occurrence a window before the
  last). Single events of different Pods never add up: a rolling update whose new Pods fail one
  readiness probe each is not an incident. An event counts as a signal for 10 minutes after it last
  happened (hypothesis behind the value, to verify on a live cluster: client-go's event recorder
  updates a repeated event about every 5 minutes at most).
- **A failed Job** is final and is an incident at once.

Pods of the same top-level owner with the same detection form one incident, logged once while it
lasts and again only if it clears and comes back. The incident lists every Pod seen failing while
it was pending, so replicas whose crash loops are out of step all count.

| Id | Failure | Signal | Logs read |
|---|---|---|---|
| `D001` | Crash loop | Container waiting with reason `CrashLoopBackOff`, or terminated with any reason but `Completed` after at least one restart (between restarts the kubelet reports it as terminated for much of each back-off) | Yes, previous container |
| `D002` | Image cannot be pulled | Container waiting with `ErrImagePull`, `ImagePullBackOff` or `InvalidImageName` | No |
| `D003` | Out of memory | Container waiting in `CrashLoopBackOff` whose last termination is `OOMKilled`, or terminated with `OOMKilled` and not restarted | Yes, previous container if it restarted, otherwise the terminated one |
| `D004` | Container cannot be created | Container waiting with `CreateContainerConfigError` or `CreateContainerError` | No |
| `D005` | Pod cannot be scheduled | The Pod still `Pending`, with its `PodScheduled` condition `False` for reason `Unschedulable` | No |
| `D006` | Probe failures | Warning event `Unhealthy` for a readiness, liveness or startup probe, happening again a settle window after it was first seen | No |
| `D007` | Volume cannot be attached or mounted | Warning event `FailedMount` or `FailedAttachVolume` | No |
| `D008` | Job failed | Warning event `BackoffLimitExceeded` or `DeadlineExceeded` about the Job | Yes, the terminated container of the last failed Pod |

`D005` reads the Pod's condition rather than the scheduler's `FailedScheduling` events, which may
stop being updated for a Pod that keeps failing (hypothesis about client-go's `events.k8s.io`
broadcaster, to verify on a live cluster); the condition holds as long as the failure. `D008` needs no watch on Jobs: the Job controller emits a Warning event on the Job, with the reason
of its `Failed` condition, when it marks the Job failed (`pkg/controller/job/job_controller.go` in
Kubernetes, checked on 2026-10-26). A container recovering after one out-of-memory kill, and the
kubelet's own `BackOff` events (the container state already says it), match nothing. Hypothesis
still to verify against live clusters: that the kubelet and scheduler reasons above are the ones
current releases emit (they are the long-standing ones).

**Grouping and fingerprint.** The incident key is the namespace, the top-level owner (found by
following owner references: Pod → ReplicaSet → Deployment, Pod → StatefulSet, Pod → DaemonSet,
Pod → Job → CronJob, or the Pod itself) and the detection id. kausa follows the controller owner
reference only through kinds it may `get` (ReplicaSet, Job). When the next owner is of another
kind, including a custom resource, that owner reference is the top-level owner. It is named in the
incident key and in `regarding` from the reference alone (apiVersion, kind, name, uid), without
reading the object. A container matches at most one detection, the most specific one: `D003`
takes precedence over `D001` when the last termination is `OOMKilled`, before grouping.

The **fingerprint** hashes the detection id, the namespace, the owner's kind and name, and a text
that stays the same for the same failure: the example's message with the names of the incident's
Pods, UUIDs, Go durations (the kubelet's growing back-off: `40s`, `2m40s`, `5m0s`), hexadecimal runs
and other numbers normalized; for `D002`, whose message alternates between `ErrImagePull` and
`ImagePullBackOff`, the image reference instead. It is computed before redaction and never sent;
it appears in kausa's log and will key the deduplication cache (third weekend).

## Redaction rules

Applied in this order to every request. `R001` and `R002` work on structure; the others are
patterns over free text (event messages, status messages and log lines). Each match is replaced
by a typed placeholder, such as `<redacted:jwt>`, and counted in the metrics.

| Id | What | How |
|---|---|---|
| `R001` | Fields outside the allowlist | Never serialized: the allowlist is [Data sent to the provider](data-sent-to-the-provider.md) |
| `R002` | Known names | Namespace, owner, Pod, container, node and service account names, and image registry hosts and paths outside the public registries list, replaced by stable placeholders (`namespace-1`, `deployment-1`) wherever they appear |
| `R003` | Names in Kubernetes messages | The quoted name in messages such as `secret "…" not found`, `configmap "…" not found` and `persistentvolumeclaim "…" not found` |
| `R004` | IP addresses | IPv4 and IPv6 addresses (also in `address:port` and `[ipv6]:port` forms); the port is kept, since it is not sensitive |
| `R005` | Email addresses | `local@domain` |
| `R006` | Credentials in URLs | The user information part of a URL (`scheme://user:password@host`), and the host itself unless it is a public registry |
| `R007` | Authorization values | `Authorization:` header values and `Bearer` or `Basic` tokens |
| `R008` | JSON Web Tokens | Three base64url segments separated by dots, the first one a JSON header |
| `R009` | Private keys and certificates | PEM blocks (`-----BEGIN … -----` to `-----END … -----`) |
| `R010` | Values of secret-named keys | `key=value`, `key: value` and JSON `"key": "value"` where the key looks like a secret (`password`, `passwd`, `secret`, `token`, `api_key`, `apikey`, `access_key`, `private_key`, `credential`, `auth`), any case and separator |
| `R011` | Known token formats | Fixed prefixes published by the issuers (for example GitHub, GitLab, Slack, AWS access key ids and the common LLM providers' keys). The list, each format's source and its test vectors ship with the implementation |
| `R012` | High-entropy strings | Runs of 32 or more characters from a token alphabet with high Shannon entropy that no rule above matched; threshold tuned on the test corpus |
| `R013` | Hostnames | Fully qualified names that are not public registries or well-known public domains, replaced by numbered placeholders (`host-1`), which are never mapped back |

Rules are tested with a corpus of invented lines that must be redacted and lines that must stay
(ordinary log lines with numbers, timestamps, versions and paths), so that a change in a rule
shows its effect on both sides. Fixtures use only fake values: `example.com`, the documentation
ranges `192.0.2.0/24` and `2001:db8::/32`, and token strings that are visibly fake.

## Limits

- Redaction has false negatives: a secret in a format no rule knows, split across lines or
  encoded, still leaves. A local model is the answer for clusters where that is unacceptable
  ([Security policy](../../SECURITY.md)).
- Pseudonyms cover the names kausa knows and the message formats above; a name that appears only
  inside a log line is caught by `R013` if it looks like a hostname, and otherwise is sent.
- Detection covers common workload failures, not every problem: a misbehaving application whose
  Pods stay `Running` and `Ready` produces no incident.
