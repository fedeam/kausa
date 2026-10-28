# 0016. Send each Pod's status reason and message to the model, redacted and bounded

- Status: accepted
- Date: 2026-10-28
- Builds on: [0008. Send only allowlisted, redacted data to one OpenAI-compatible
  endpoint](0008-data-leaving-the-cluster.md)

## Context

The eviction rule `D009` matches a Pod in phase `Failed` with `status.reason` `Evicted`. The reason
alone does not say why: the kubelet evicts a Pod whose containers exceed their `ephemeral-storage`
limits, whose `emptyDir` exceeds its `sizeLimit`, or because the node is under pressure, and only
`status.message` tells them apart (seen on kind, Kubernetes 1.37, on 2026-10-28). The first is the
workload's fault and the last is not, so an explanation without the message would guess. No log
helps: the evicted container's log is gone. Until now, the request carried no Pod-level reason or
message, only the scheduler's `unschedulable` message for `D005`.

## Alternatives considered

- **Send only the reason.** Every `D009` incident would look the same to the model.
- **Parse the message into fields** (the kind of eviction, the quantities): it ties kausa to the
  kubelet's wording, which is not an API, and it is more code than one more redacted string.
- **Send the message only for `D009`.** A Pod past its `activeDeadlineSeconds` (reason
  `DeadlineExceeded`) has a message that explains it too, and one rule for every Pod is simpler.

## Decision

- Each Pod in `pods` carries `reason` and `message`: its `status.reason` and `status.message`,
  `null` when Kubernetes did not set them, for every Pod in the request, not only for `D009`.
- Both are free text that the kubelet, and any writer of `pods/status`, can set, so both pass
  through `Redactor::text` in `src/context.rs`: the names kausa knows become pseudonyms (`R002`)
  and the patterns `R003` to `R013` run on them, as on the other status messages (step 3 of 0008).
- Both are cut at 1024 bytes when the Pod is recorded (`MAX_TEXT_BYTES` in `src/record.rs`); a
  request carries at most three Pods (`MAX_PODS` in `src/context.rs`), and one that does not fit
  `--max-input-tokens` even without logs is not sent.

## Consequences

- The model can tell a workload's own limit from node pressure, and say which limit to raise.
- **Known gap:** an eviction for an `emptyDir` `sizeLimit` names the volume (`Usage of EmptyDir
  volume "data" exceeds the limit "20Mi".`). kausa does not record volume names, so it does not
  pseudonymize them, and the name is sent as written unless a pattern matches it, as `D007`
  already does for the volume names in mount messages. A volume name is chosen by whoever writes
  the workload, like a container name; it is listed in [Data sent to the
  provider](../reference/data-sent-to-the-provider.md) and the [Security policy](../../SECURITY.md).
- A node-pressure message names a container (pseudonymized) and quantities, which are kept.
- [Data sent to the provider](../reference/data-sent-to-the-provider.md) lists both fields.
