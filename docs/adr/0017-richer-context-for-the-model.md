# 0017. Send the model resource requests, probe handlers, image pull settings and an unschedulable Pod's scheduling keys, reduced and redacted

- Status: proposed
- Date: 2026-10-29
- Builds on: [0008. Send only allowlisted, redacted data to one OpenAI-compatible
  endpoint](0008-data-leaving-the-cluster.md) (amends its allowlist: a probe's path and node-label
  and taint keys, never sent until now, are sent redacted) and [0016. Send each Pod's status reason
  and message to the model, redacted and bounded](0016-send-the-pod-status-reason-and-message.md)
- Keeps: [0009. Run with read-only permissions that never include
  Secrets](0009-least-privilege-rbac.md) and [0013. Offer a namespaced install mode: one watch pair
  and one Role per listed namespace, and nothing cluster-wide](0013-namespaced-install-mode.md)
  (no new permission, nothing cluster-scoped)
- Related: [0014. Keep each incident's full explanations in a namespaced Explanation resource, off
  by default](0014-explanation-history.md) (`promptVersion` becomes `"2"`), and the other v0.4
  ADRs, `0018-stuck-rollouts.md` and `0019-high-availability.md`

## Context

Read in the code on 2026-10-29 (`src/record.rs`, `src/context.rs`), every request carries each
container's image (`R002` for private registries), `memory_limit` and `cpu_limit` as the API
returns them, and each probe's kind and four timings, "never its command, path or headers"
(`ProbeRecord`, tested by `the_record_drops_environment_and_command_lines`); for `D005`, the
scheduler's redacted message. Missing: requests (the scheduler places by them; `D003` and `D009`
turn on request against limit), how a probe checks, what an unschedulable Pod asks of the nodes,
and for `D002` its pull credentials. All of it is in the Pod spec kausa already watches.

Seen on kind, Kubernetes 1.36.4 (the e2e node image), on 2026-10-29, with invented workloads:

1. Limits alone are stored with equal requests; quantities come back canonical (`0.5` as `500m`);
   probes get `scheme: HTTP` and `successThreshold: 1`.
2. Every Pod gets two tolerations, `node.kubernetes.io/not-ready` and `…/unreachable`.
3. The scheduler's message names no key, value, taint or node: `1 node(s) didn't match Pod's node
   affinity/selector`, `had untolerated taint(s)`, `didn't match pod affinity rules`.
4. The kubelet's `Unhealthy` note names no path for a `404`; for a connection error it quotes the
   URL with the Pod IP, path, query and fragment, sent today redacted by patterns only.
5. A named probe port resolves against `ports[].name`. A failing liveness probe ends in
   `CrashLoopBackOff`, so `D001` and `D006` both match.
6. The API server refuses a node selector value or a toleration key that breaks label syntax.

## Decision

### Which rules get which fields

| Field | Rules |
|---|---|
| `pods[].containers[].memory_request`, `cpu_request` | every rule |
| `pods[].containers[].probes[].handler`, `port`, `success_threshold` | every rule |
| `pods[].containers[].probes[].path` | `D001`, `D006` |
| `pods[].containers[].image_pull_policy`, `pods[].image_pull_secrets` | `D002` |
| `scheduling` (top level, once, from the first Pod that has one; the sample comes first) | `D005` |

Small structured fields go with every rule, like the limits and probe timings since 0008; the path,
the one new free text in a container, with the two rules a probe explains (fact 5). A gated field
is absent for the other rules.

### Requests and quantities

`ContainerRecord` gains `memory_request` and `cpu_request` (`resources.requests`), init containers
included. Every quantity, the existing limits too, is kept only if it is at most 32 bytes and
matches the Kubernetes quantity grammar, otherwise `null`: after fact 1 it should never fire, and
it makes `R001` stricter. Not sent: `ephemeral-storage`, extended resources, Pod-level
`spec.resources` and `status.qosClass`.

### The probe

`ProbeRecord` gains:

- `handler`: `exec`, `http`, `https` (scheme `HTTPS`), `tcp` or `grpc`; `null` if none.
- `port`: a number from 1 to 65535; a named port resolved to the same container's `containerPort`;
  `null` for `exec`, an unresolved name or a number out of range. A port's name is never sent.
- `success_threshold`.
- `path`: `httpGet.path` up to its first `?` or `#`; `null` when empty or, once the query is gone,
  longer than 128 bytes. Never cut, so no cut can shorten a token below `R012`'s threshold or drop
  the suffix `R013` needs. Passed through `Redactor::text` (`R002` to `R013`).
- Never recorded or sent: an `exec` command, `httpHeaders`, `httpGet.host`, `tcpSocket.host`, the
  query and fragment, `grpc.service`, `terminationGracePeriodSeconds`.

### Image pull settings, `D002`

`image_pull_policy` per container (`Always`, `IfNotPresent` or `Never`, else `null`), and
`image_pull_secrets` per Pod: how many entries `spec.imagePullSecrets` has, capped at 16. Never a
Secret's name; counting references reads no Secret (invariant 1).

### Scheduling constraints, `D005`

`PodRecord` gains `scheduling: Option<SchedulingRecord>`, set only while the Pod's `PodScheduled`
condition sets `unschedulable` (`Unschedulable` or `SchedulerError`), so healthy Pods keep none.

| Field | Source (required terms only) | Kept |
|---|---|---|
| `node_selector` | `nodeSelector`, key order | `{key, value}` |
| `node_affinity` | `nodeAffinity` terms, `matchExpressions` then `matchFields` | `{term, key, operator, values, value_count}` |
| `pod_affinity`, `pod_anti_affinity` | `podAffinity`, `podAntiAffinity` terms | `{topology_key, namespaces_set}` (`namespaces` or a `namespaceSelector` is set) |
| `tolerations` | `tolerations`, spec order | `{key, operator, effect}` |
| `omitted` | | entries dropped by the caps |

- **Values are never sent**, but for `kubernetes.io/os` (`linux`, `windows`) and
  `kubernetes.io/arch` (`amd64`, `arm64`, `arm`, `386`, `ppc64le`, `s390x`, `riscv64`), from those
  closed lists only; everything else (`Gt`/`Lt` operands, zones, pools, a `kubernetes.io/hostname`
  or `metadata.name` node name) is counted. A toleration's `value` and `tolerationSeconds`, Pod
  affinity label selectors, `matchLabelKeys`, namespace names and selectors never are. An affinity
  requirement's closed values are kept as a set of fixed size (a bitset over the nine values: at
  most 2 for `os`, 7 for `arch`, each once), and `value_count` (a `u16`, saturating) counts every
  value listed, duplicates included. `NodeSelectorRequirement.values` is an unbounded list in
  k8s-openapi 0.28.0, and the API server does not appear to cap or deduplicate it (hypothesis), so a
  `Vec` of every closed value would let a tenant's pending Pod hold megabytes in a record kausa
  keeps for every Pod (invariant 6).
- **Keys:** longer than 128 bytes, the entry is dropped and counted; otherwise checked against the
  qualified-name syntax, else sent as `"invalid"` (fact 6 covers selector values and toleration
  keys; for the others, kausa does not rely on the API); then `Redactor::text`. So
  `topology.kubernetes.io/zone`, `example.com/pool` and `disktype` stay, and
  `eks.amazonaws.com/nodegroup` becomes `host-1/nodegroup` (`R013`).
- **Operators and effects** map to closed sets at record time (`In`, `NotIn`, `Exists`,
  `DoesNotExist`, `Gt`, `Lt`; `Exists`, `Equal`, absent as `Equal`; `NoSchedule`,
  `PreferNoSchedule`, `NoExecute`, empty as `null`); anything else is `"other"`.
- **Caps:** 8 selector entries, 8 affinity expressions across terms, 4 Pod affinity and 4
  anti-affinity terms, 8 tolerations, each entry of fixed size but for one key. Preferred terms,
  topology spread constraints, priority and `schedulerName` are not sent.

### Fitting, prompt, everything else

- `fit()` gains a step after "one Pod and three events": drop `scheduling` and every probe `path`
  (sent as `null`), then skip with `context_too_large` as before.
- The system message's list of facts names requests and limits, probe settings and, for a Pod that
  cannot be scheduled, its constraints by key, "their values are not sent, so never guess them".
  `PROMPT_VERSION` becomes `"2"` (an Explanation's `promptVersion` then tells which context the
  model saw; at most 16 characters, no schema change).
- Unchanged: permissions in both `rbac.scope`s, flags, chart values, the fingerprint (it reads none
  of these fields), the Event, the Explanation's schema, the notification's fields, `restore` and
  `restore_only` (no new name kind), `D002`'s image and `D005`'s message.

### Token cost

By kausa's estimate (a quarter of the body's bytes; the context is a JSON string in the user
message, so its quotes count twice): requests about 14 tokens per container, the probe shape about
15 and a path about 5 (at most 36), `scheduling` about 90 typical and 1,540 at worst. Three Pods
with one container and two HTTP probes grow by about 130 tokens (160 with paths, 220 for `D005`),
a third of the 437 measured on 2026-10-26 ([Tokens per incident compared with
k8sgpt](../research/2026-10-26-tokens-compared-with-k8sgpt.md)); the worst fits the default 6,000.

## Alternatives considered

- **Send values through `Redactor::text`:** a pending Pod's `kubernetes.io/hostname` value is a
  node name `R002` does not know, and 0008 keeps such names out.
- **New `R002` kinds `label-N`, `value-N`,** to line up with the scheduler's message: it names no
  key or value (fact 3). They would add `PLACEHOLDER_LITERAL` entries and over-replacement (a value
  `node` or `zone` rewritten inside `node(s)` or `topology.kubernetes.io/zone`).
- **Pseudonymize every key outside a closed list:** the same new kind. **Read Nodes:** a
  cluster-scoped grant, against 0009 and impossible with `rbac.scope=namespaced`.
- **Never send the path:** a `404` stays unexplained, while a connection error already sends the
  URL (fact 4). **Every rule:** explains nothing else. **`D006` only:** misses liveness kills (5).
- **Cut long paths and keys:** a cut can defeat `R012` and `R013`. **Gate every field by rule:**
  a branch per field, and requests missing from `D001` and `D009`.
- **A byte cap on `scheduling`, `qos_class`, `host_set`, ports as `"named"`:** caps and `fit()`
  bound it; QoS derives from what is sent; another host is rare; a resolved number says more.
- **Extended resources, `ephemeral-storage`, Pod-level resources, topology spread constraints:**
  later ADRs if needed. **A flag:** one more combination. **`PROMPT_VERSION` `"1"`:** an
  Explanation could not tell which context the model saw.

## Consequences

- Explanations of `D001`, `D002`, `D003`, `D005` and `D006` can name the request against the limit,
  the probe and what it checks, the selector key or toleration that keeps a Pod off every node, and
  a missing pull secret. More precise is a hypothesis until `tests/bench/` is explained again.
- A tight `--max-input-tokens` loses log bytes earlier on `D001`, `D003` and `D008` (Changelog).
- **Invariants:** 3, the allowlist grows through this ADR; 4, `R001` lets through two values it held
  back (the probe path without its query, node-label and taint keys), which this ADR decides, and
  gets stricter on quantities; 1, 2, 8 and 11 hold. No invariant's text changes. 0008 gets an
  `Update` paragraph on acceptance.
- **Known gaps.** A path or key is written by the workload's author, like a volume name (0016). A
  customer name in a path, in a key without a prefix, under a public host's or registry's domain
  (`example.com`, `github.com`, `gcr.io`), under a suffix `R013` does not list (`karpenter.sh`) or
  under a subdomain of `kubernetes.io` leaves. Cloud keys (`cloud.google.com/…`) become
  `host-N/…`, never mapped back. Fact 4, true today already: a probe's query in a connection-error
  note leaves unless a pattern matches; a stricter redaction rule is a separate change.
- **Records grow** by two quantities and a pull policy per container, a port and a path of at most
  128 bytes per probe, and at most about 6 KiB per unschedulable Pod whatever its spec holds (32
  keys of at most 128 bytes and fixed-size entries; about 57 MiB for 10,000 such Pods at that
  worst); hypothesis: within the margin of [Thousands of
  Pods](../research/2026-10-28-thousands-of-pods.md). Nothing new is read, so under
  `0019-high-availability.md` it all happens in the leader.
- **`0018-stuck-rollouts.md`:** its `D014` sends no Pods, so none of these fields; a rollout whose
  new Pods fail readiness is also a `D006`, which carries them, one template per Pod sent.
- **Docs in the same change:** [Data sent to the
  provider](../reference/data-sent-to-the-provider.md) (rows, "never a probe's command, headers,
  host, gRPC service, or its path's query and fragment", the values that never leave, fact 4's
  gap); [Detection and redaction
  rules](../reference/rules.md) (free text adds paths and keys, `R001`'s quantity check, `D005` and
  `D006` notes, Limits); [SECURITY.md](../../SECURITY.md) (inputs and gaps); [Data sent in
  notifications](../reference/data-sent-in-notifications.md) (an explanation can repeat a path or a
  key); [Architecture](../explanation/architecture.md); the [Changelog](../../CHANGELOG.md).
  Permissions, Command line and log format and the chart do not change.
- **Tests in the same change:** `src/record.rs` (each field, cap and closed set; scheduling only for
  an unschedulable Pod; a requirement with 100,000 duplicated `amd64` values keeps one value and
  saturates `value_count`; `the_record_drops_environment_and_command_lines` asserts that no query,
  header, host, command, gRPC service, pull secret name, selector, toleration or affinity value
  reaches the record); `src/context.rs` (the rule table, names in paths and keys replaced, `R012`
  and `R013` on keys, the same incident gives the same request); `src/explain.rs` (the worst `D005`
  fits the default cap; `fit()` drops `scheduling` and paths before skipping); `src/prompt.rs`;
  `tests/e2e` (a probe path with a fake query on `crasher`, a new unschedulable Deployment with
  invented selector and toleration values: `requests.jsonl` holds the path and the keys, never the
  query, the values or the Deployment's name; one more notification; the `kubectl auth can-i` lists
  unchanged in both scopes). No new fuzz target: nothing new slices untrusted text, and keys and
  paths go through `Redactor::text`, already fuzzed.
- **Before acceptance:** `tests/bench/thousands-of-pods.py` and the token benchmark again, with
  probes and unschedulable Pods at the worst scheduling record, in a dated research note.
  **Hypotheses left:** the API validates `matchExpressions` keys and `topologyKey` like label keys;
  the API does not cap an affinity requirement's values (kausa does not rely on it either way); the
  ServiceAccount admission plugin copies a service account's pull secrets into the Pod; preferred
  terms never block scheduling; the `os` and `arch` lists are complete; other Kubernetes versions
  word facts 3 and 4 alike.
