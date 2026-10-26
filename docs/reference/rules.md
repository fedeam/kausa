# Detection and redaction rules

The deterministic rules kausa applies before any language model call: detection rules decide what
is an incident and how failures are grouped, and redaction rules decide what never leaves the
cluster.

Status as of 2026-10-23: **planned** for v0.1, nothing implemented. Ids, triggers and defaults may
change until v0.1 is released; after that an id is never reused. How the rules fit in the
pipeline: [Architecture](../explanation/architecture.md). Why redaction works this way: [0008.
Send only allowlisted, redacted data to one OpenAI-compatible
endpoint](../adr/0008-data-leaving-the-cluster.md).

## Detection rules

A detection matches a signal on a Pod (a container state in its status or a Warning event about
it), and the failure must persist through the **settle window** (60 seconds by default) before it
becomes an incident, so a container that restarts once and recovers is not explained. Pods of the
same top-level owner with the same detection form one incident.

| Id | Failure | Signal | Logs read |
|---|---|---|---|
| `D001` | Crash loop | Container waiting with reason `CrashLoopBackOff` | Yes, previous container |
| `D002` | Image cannot be pulled | Container waiting with `ErrImagePull`, `ImagePullBackOff` or `InvalidImageName` | No |
| `D003` | Out of memory | Last termination with reason `OOMKilled` | Yes, previous container if it restarted, otherwise the terminated one |
| `D004` | Container cannot be created | Container waiting with `CreateContainerConfigError` or `CreateContainerError` | No |
| `D005` | Pod cannot be scheduled | Warning event `FailedScheduling` and the Pod still `Pending` | No |
| `D006` | Probe failures | Warning event `Unhealthy` for a readiness, liveness or startup probe, repeated within the settle window | No |
| `D007` | Volume cannot be attached or mounted | Warning event `FailedMount` or `FailedAttachVolume` | No |
| `D008` | Job failed | Job condition `Failed` with reason `BackoffLimitExceeded` or `DeadlineExceeded` | Yes, the terminated container of the last failed Pod |

Hypotheses to verify on the first weekend: that the event reasons above are the ones current
kubelet and scheduler releases emit (they are the long-standing ones), and that `D008` can be
detected from the Job's Warning events and Pod status alone, without watching Jobs (if not, it
needs `list` and `watch` on `jobs`, an amendment to [Permissions](permissions.md)).

**Grouping and fingerprint.** The incident key is the namespace, the top-level owner (found by
following owner references: Pod → ReplicaSet → Deployment, Pod → StatefulSet, Pod → DaemonSet,
Pod → Job → CronJob, or the Pod itself) and the detection id. kausa follows the controller owner
reference only through kinds it may `get` (ReplicaSet, Job). When the next owner is of another
kind, including a custom resource, that owner reference is the top-level owner. It is named in the
incident key and in `regarding` from the reference alone (apiVersion, kind, name, uid), without
reading the object. A container matches at most one detection, the most specific one: `D003`
takes precedence over `D001` when the last termination is `OOMKilled`, before grouping. The
fingerprint adds the normalized
message: digits, hexadecimal runs, UUIDs and generated Pod name suffixes removed. It is computed in
memory before redaction, is never sent, and keys the deduplication cache.

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
