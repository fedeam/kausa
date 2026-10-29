# Explanation resource

The custom resource in which kausa keeps each incident's full explanations with `--history`: its
CustomResourceDefinition, its schema and limits, the objects kausa writes and deletes, and what is
never stored.

Status as of 2026-10-29: released in v0.3.0; off by default. The
kind test (`tests/e2e/run.sh`) installs it with `history.enabled=true` in both `rbac.scope`s and
reads every field back, and the hypotheses of the decision were checked in kind on 2026-10-28
([2026-10-28 — First release](../journal/2026-10-28-first-release.md), "the hypotheses of the
explanation history"). Why it works this way: [0014. Keep each incident's full explanations in a
namespaced Explanation resource, off by default](../adr/0014-explanation-history.md).

## The CustomResourceDefinition

| Field | Value |
|---|---|
| Name | `explanations.kausa.fedeam.com` |
| Group, version | `kausa.fedeam.com`, `v1alpha1` (served and storage, the only one; conversion `None`) |
| Kind, list kind | `Explanation`, `ExplanationList` |
| Plural, singular | `explanations`, `explanation`; no short names and no categories, so it stays out of `kubectl get all` |
| Scope | `Namespaced`: each object is in the failing workload's namespace, next to its Events |
| Subresources | none: `status` is written with the object, and the CEL rule below keeps `spec` fixed |

The chart installs it from `templates/explanation-crd.yaml` only while the release writes history
with `history.installCRD` ([Helm chart](helm-chart.md)); kausa never creates, changes or reads a
CustomResourceDefinition. Its `openAPIV3Schema` is `charts/kausa/files/explanation-schema.json`,
which the chart includes with `.Files.Get` and `fromJson`, and which a unit test in
`src/history/pure.rs` reads to check that its limits equal kausa's constants. The schema is
structural, so the API server prunes a field it does not list, and every field has a description,
so `kubectl explain explanation.status.entries` documents it.

## Fields and limits

| Field | Type and limit | Content |
|---|---|---|
| `spec` | object, required, immutable | The incident's identity, written at create. CEL rule `self == oldSelf`, message `spec is immutable`, for everyone, a cluster administrator included |
| `spec.rule` | string, `^D[0-9]{3}$`, at most 4, required | The detection id, for example `D001` ([Detection and redaction rules](rules.md)) |
| `spec.target.apiVersion` | string, at most 253, required | The top-level owner's `apiVersion`, as the Event's `regarding` |
| `spec.target.kind` | string, at most 63, required | Its kind |
| `spec.target.name` | string, at most 253, required | Its name. No uid: a workload recreated under the same name continues its history |
| `status.occurrences` | integer, at least 1 | How many times kausa wrote an explanation for this incident, fresh or from the deduplication cache, since the object was created; dropped entries still count |
| `status.lastTime` | date-time, at most 20 | The last of those writes |
| `status.entries` | array, at most 5, newest first | One entry per distinct text |
| `entries[].summary` | string, at most 300 | The whole summary, with the real names mapped back |
| `entries[].cause` | string, at most 600 | The whole likely cause |
| `entries[].steps` | array of at most 5 strings of at most 300 | The whole steps; the list may be empty |
| `entries[].confidence` | `low`, `medium` or `high` | |
| `entries[].model` | string, at most 253 | The `--model` value at the last write of this text, cleaned and cut to 253 characters |
| `entries[].promptVersion` | string, at most 16 | `PROMPT_VERSION` of `src/prompt.rs` at the last write |
| `entries[].firstTime`, `entries[].lastTime` | date-time, at most 20 | When this text was first and last written |
| `entries[].count` | integer, at least 1 | How many times this text was written, like its Event's series count |

All nine fields of an entry are required. The text limits are kausa's own caps on the model's
answer, counted in characters: the API server's `maxLength` counts characters too (300 `é`, 600
bytes, accepted and 301 refused in kind on 2026-10-28, although the message says `bytes`). The
answer's caps are the same as before the history: an Explanation keeps the text an Event note cuts
to 1024 bytes, not a longer answer.

## Objects

- **One object per incident fingerprint**, named `kausa.<fingerprint>` (16 lowercase hexadecimal
  digits), so the Event `kausa.<fingerprint>.<hash>` leads to its Explanation by name, and the
  same failure finds its object again after a restart.
- **Label** `app.kubernetes.io/managed-by: kausa`; no other label, no annotation and no
  `ownerReferences`: the history outlives the workload on purpose (a failed Job with a TTL is
  deleted minutes after it fails), and kausa's sweep deletes it.
- **Kausa-shaped**: an object that carries that label, whose name is `kausa.` and 16 lowercase
  hexadecimal digits, and whose `spec` parses (a rule matching the pattern, and the three target
  strings within their limits). kausa appends to and deletes only kausa-shaped objects. This is a
  convention, not proof that kausa wrote the object: whoever can create Explanations can forge one
  ([Security policy](../../SECURITY.md)).

Printer columns, so that `kubectl get explanations -n shop` reads like this (invented values):

```text
NAME                     KIND         TARGET   RULE   COUNT   LAST   AGE
kausa.5d0c2b7e9a1f4c38   Deployment   api      D001   3       4m     2d
```

`KIND` `.spec.target.kind`, `TARGET` `.spec.target.name`, `RULE` `.spec.rule`, `COUNT`
`.status.occurrences`, `LAST` `.status.lastTime`, `AGE` `.metadata.creationTimestamp`; `-o wide`
adds `CONFIDENCE` and `SUMMARY`, of `.status.entries[0]`. The whole history:

```sh
kubectl get explanation -n shop kausa.5d0c2b7e9a1f4c38 -o yaml
```

## Times

Every time kausa writes is RFC 3339 in UTC to the second, `YYYY-MM-DDTHH:MM:SSZ` (20 characters,
for example `2026-10-28T10:00:00Z`). Times in that one form compare as strings, so the pure part of
the history needs no clock and no time library: kausa passes it `now` and the age cutoff (`now`
minus `--history-days` days).

## What kausa writes

Only for an `Explained` outcome, fresh or from the deduplication cache, with `--history`, after the
Event and independent of it. Skipped explanations stay Events, log lines and counters. One
read-modify-write, all under a 10-second timeout:

1. `list` in the incident's namespace with the field selector `metadata.name=kausa.<fingerprint>`
   (kausa has no `get`). The object is reduced at once to its label, `spec`, `resourceVersion` and
   the entries that parse.
2. None: `create` it with the name, the label, the `spec` and a `status` with one entry and
   `occurrences: 1`.
3. One: refuse (`history write refused`) unless it carries the label and its `spec` equals the
   incident's rule and target. Otherwise merge and send a JSON merge patch of `status` and
   `metadata.resourceVersion` only, with `fieldValidation=Strict`. The `resourceVersion` is a
   precondition: an object changed since the read gives `409`, which is counted as a conflict and
   not retried.

**The merge:**

- An entry is one distinct text: summary, cause, steps and confidence all equal. The same text
  again adds one to its `count` (saturating), sets its `lastTime`, `model` and `promptVersion` to
  the current ones, keeps its `firstTime` and moves to the front. A new text goes in front with
  `count: 1`.
- Then entries whose `lastTime` is older than the cutoff are dropped, and the list is cut to 5.
- `occurrences` goes up by one (saturating) and `status.lastTime` becomes `now`.

**Stored entries are untrusted.** Before the merge, kausa drops an entry with a missing or wrongly
typed field, an unknown confidence, a time not in the one form or later than `now`, a text above
its limit or more than 5 steps, a count below 1 or above 4,294,967,295, or any text (summary,
cause, a step, the model, the prompt version) that `prompt::clean` would change: a hand-made
control, bidirectional-override or zero-width character, or markup, is never written back. kausa's
own text passes unchanged, because `clean` is idempotent. A repeated text keeps its first entry, at
most 5 are kept, and an unreadable `occurrences` becomes the sum of the kept counts. Nothing read
back is logged, sent to the model, put in an Event or loaded into the deduplication cache, and no
input panics: the fuzz target `history` checks it.

## Retention

| Limit | Set by | Default |
|---|---|---|
| Entries per object | constant `MAX_ENTRIES` | 5 |
| Age, by `status.lastTime` (by `metadata.creationTimestamp` when that is unreadable or later than `now`) | `--history-days`, chart value `history.days`, 1 to 30 | 7 |
| Objects across the namespaces one kausa covers | constant `MAX_OBJECTS` | 500 |

The sweep runs in the explanation slot, at start and once an hour has passed since the last one
began, ahead of the queue, for at most 30 seconds. It lists labelled objects in pages of 2 (across
the cluster, or in each `--namespace` in order), skips those that are not kausa-shaped, deletes
those past the age as each page arrives, and keeps the others in a bounded heap: when more than
`MAX_OBJECTS` newer objects have been seen, the oldest is deleted for the cap (ties go to the
smaller namespace, then the smaller name). Each delete carries the listed `resourceVersion` as a
precondition, so an object written since the list is kept until the next sweep.

Between sweeps the count can pass `MAX_OBJECTS` by the fingerprints written in that hour, at most
`--max-calls-per-hour` plus the cached ones. An object kausa writes stays under about 55 KB in
the worst case (the largest the schema allows was 55,319 bytes of JSON in kind), and 500 under
about 28 MB.

The schema bounds `spec` and `status`, not `metadata`. An object that someone else creates with
kausa's label (the label selector returns it whether or not it is kausa-shaped) can carry labels,
annotations or finalizers up to etcd's object limit, about 1.5 MiB, and kube buffers and parses a
whole page of the list before kausa sees it. Such an object, filled with thousands of small
labels, took about 12 MiB of memory once parsed (measured on 2026-10-28), so the sweep asks for 2
objects per page, which keeps a page under about 30 MiB, within the chart's 128Mi memory limit.
It drops the annotations, the other labels, the finalizers, the managed fields and the owner
references before it reduces each object. A ResourceQuota on
`count/explanations.kausa.fedeam.com` bounds how many such objects a namespace can hold.
Nothing prunes Explanations while kausa is not running or runs without `--history`; in the chart
with `history.installCRD`, the CustomResourceDefinition, and with it every Explanation, is deleted
when the release stops writing history or is uninstalled.

## What is never stored

The context sent to the model, log lines, event messages, the request, the answer before
validation, the mapping of placeholders to names, the endpoint, token counts, the Pod name, or
anything else of kausa's log that its Event does not have, except the `--model` value. Nothing new
leaves the cluster: the history goes to the API server only ([Data sent to the
provider](data-sent-to-the-provider.md)).

## Schema changes

Within `v1alpha1`, changes are additive only: new optional fields. A breaking change is a new
version without a conversion webhook, or, since the history is derived and bounded, a release that
lets Helm replace the CustomResourceDefinition and delete the old objects, under a breaking-change
heading in the [Changelog](../../CHANGELOG.md). Every change to
`charts/kausa/files/explanation-schema.json` is a line in the Changelog.
