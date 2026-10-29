# 2026-10-28 — First release

## Request

Cut v0.1.0: publish the image and the chart, signed, as [0012. Release from a tag: a signed image
on GHCR and the chart on the GitHub release](../adr/0012-releases.md) describes, and check what
was published.

## What was done

- Pull request #31 set the version to 0.1.0 in `Cargo.toml` and the chart, and moved the
  changelog's entries under `[0.1.0] - 2026-10-28`. After its merge the maintainer tagged
  `v0.1.0` on `main` (an annotated tag) and pushed it.
- **The Release workflow passed on its first run:** both images built natively (amd64 and arm64),
  the multi-architecture index `ghcr.io/fedeam/kausa:0.1.0`
  (`sha256:4fc353048777a7cd7cd8015b0a98f6b8460f81498d8c08ec4cc65aabe4098073`), signed with
  cosign keyless and with SLSA build provenance; the GitHub release with the changelog's notes,
  `kausa-0.1.0.tgz`, `SHA256SUMS` and its cosign bundle.
- **Checked from outside, without credentials:** the image's manifest can be pulled anonymously
  and lists linux/amd64 and linux/arm64; `cosign verify` (v3.1.4) accepts the signature for the
  identity of `release.yml` at `refs/tags/v0.1.0`; `gh attestation verify` finds one SLSA v1
  provenance from that workflow; `cosign verify-blob` accepts the checksums' bundle and
  `sha256sum -c SHA256SUMS` matches the chart.
- **Installed from the release:** `helm install` of the chart's URL in a throwaway kind cluster
  (Kubernetes v1.37), in dry-run: the Pod pulled the image by the index's digest, became ready,
  and logged `started` with version 0.1.0. The cluster was deleted, and the maintainer's
  kubeconfig was left unchanged (checked by its checksum before and after).
- The README, [Install kausa with Helm](../how-to/install-with-helm.md), the [Helm
  chart](../reference/helm-chart.md) reference and the front page now install the released
  chart; building your own image is the alternative.

## Notes

- The package's page on GitHub answered 404 before the tag: the package did not exist until the
  first push to GHCR.
- OpenSSF Scorecard's checks `Signed-Releases` and `Packaging` had no release or packaging run to
  look at; both count from the next scan.

## Later the same day: provenance as a release asset

- OpenSSF Scorecard scored 7.6. Its scan ran before the release, so Signed-Releases and Packaging
  had nothing to look at yet. In Scorecard v5.6.0, Packaging needs a successful run of a workflow
  that pushes an image, which `release.yml` now has. Signed-Releases gives 8 to a release with a
  signature asset (`.sigstore.json` counts) and 10 to one with a provenance asset, which it
  recognises by the `.intoto.jsonl` extension only.
- `release.yml` now attests the chart and `SHA256SUMS` too, and attaches the bundle as
  `kausa-X.Y.Z.intoto.jsonl`.
- For v0.1.0, the image's provenance, made by the release run, was downloaded with
  `gh attestation download`, checked offline with `gh attestation verify --bundle`, and attached
  to the release as `kausa-0.1.0-image.intoto.jsonl`. The release's other assets are unchanged.
- **What Scorecard can't get from one maintainer:** Code-Review and Contributors need other
  people. Branch-Protection stays at 8: to go higher it needs two required approvals, which one
  maintainer can't give. Maintained counts from 90 days after the repository was created. SAST and
  CI-Tests rise as the commits made before CodeQL and CI leave the window they look at.

## Later the same day: v0.2.0

- The v0.2 design ran as teams of agents (designers, judges, a lead, QA, security and a standards
  guardian per feature). The ADRs 0013 to 0016 were accepted, and the detection changes merged in
  pull request #37.
- Designing the namespaced mode found that the API server accepts an `events.k8s.io/v1` Event
  whose `regarding` object is in another namespace (checked in kind, Kubernetes v1.37; the core
  v1 API refuses it). kausa now ignores such Events (pull request #38).
- main already carried new detection rules, so the fix goes out as v0.2.0 rather than a patch
  release, as Semantic Versioning asks; the namespaced mode, the history and the notifications
  move to v0.3.

## Later the same day: the namespaced install mode

### Request

Build the namespaced install mode that [0013. Offer a namespaced install mode: one watch pair and
one Role per listed namespace, and nothing cluster-wide](../adr/0013-namespaced-install-mode.md)
accepted, in three tracks (code, chart, docs) on the branch `feat/v0.2-namespaced`, and check it:
the measurement the ADR makes a condition of shipping, both kind runs, and the ADR's hypotheses
that a kind cluster can settle.

### What was done

- **The Event namespace check came first,** on its own branch and merged to `main` as #38:
  `EventRecord::from_event` drops an Event whose `regarding.namespace` differs from its
  `metadata.namespace`, or that has none. The namespaced mode relies on it, and cluster mode gets
  it too (the `Security` line in the changelog).
- **Code:** `kausa run --namespace <NAME>`, repeatable up to 32 (`MAX_NAMESPACES`), with its four
  usage errors, checked before any connection. `src/run.rs` builds one Pod watch and one event
  watch per namespace (or one of each on the whole cluster), tags each item with its watch, and
  merges them per kind with `futures`' `select_all`, so the loop keeps one Pod branch and one
  event branch and no new crate or task. Relists and `403` drops are per watch, readiness waits
  for every watch (2 or 2 × N), and an ended watch names its namespace. `started` gains
  `namespaces` and the watch error lines `namespace`. Unit tests in `src/run.rs` and cases in
  `tests/cli.rs`.
- **Chart:** `rbac.scope` (`cluster` or `namespaced`, required, no default) and `rbac.namespaces`;
  `templates/clusterrole.yaml` became `templates/rbac.yaml`, which renders the ClusterRole or one
  Role and RoleBinding per listed namespace from one `kausa.rules` helper; `kausa.validate`
  refuses the seven bad cases first, nil-safe; one `--namespace` per entry in the Deployment;
  `NOTES.txt` names the scope. The Dockerfile's `ci` target and the new `tests/chart/render.sh`
  render both scopes and every refusal. Every `helm install` and `helm lint` in the repository
  now sets `rbac.scope` (the README, the docs, `AGENTS.md`, the Dockerfile, `tests/e2e/run.sh` and
  the install command printed by `release.yml`).
- **The kind test is a matrix** of `KAUSA_E2E_SCOPE` (`cluster`, `namespaced`) in
  `.github/workflows/e2e.yml`, so its checks are now named `kind (cluster)` and
  `kind (namespaced)`.
- **Integration:** the chart track was merged into the code track, then `main` (ADRs 0013 to 0016,
  `D009` and `D010`, and #38) was merged in; the changelog's `Security` line, which both sides
  carried, was kept once.
- **Docs:** [Permissions](../reference/permissions.md), [Helm chart](../reference/helm-chart.md),
  [Command line and log format](../reference/command-line.md), [Install kausa with
  Helm](../how-to/install-with-helm.md), [Try kausa against a cluster with a local
  model](../how-to/try-with-a-local-model.md), [Architecture](../explanation/architecture.md),
  [Set up a development environment and run the checks](../how-to/develop.md), the [Security
  policy](../../SECURITY.md), `ARCHITECTURE.md`, the README, the changelog, and an `Update` in
  [0009. Run with read-only permissions that never include
  Secrets](../adr/0009-least-privilege-rbac.md).
- **v0.2.0 came out meanwhile.** While the docs were written, `main` released v0.2.0 (#39)
  without this mode and moved it to v0.3 in the roadmap. `main` was merged in again: this mode's
  changelog entries stay under `Unreleased`, and the docs say "planned for v0.3" and name the
  v0.2.0 chart and image as ones without `rbac.scope` and `--namespace`.

### How it was checked

- `cargo fmt`, `clippy -D warnings` and `cargo test` on the integrated branch; `helm lint
  --strict` in both scopes; `tests/chart/render.sh` (every check passed); `gitleaks` on the
  clone (no leaks); `docker build --target ci .`.
- **The shipping gate:** [Thirty-two namespaces](../research/2026-10-28-32-namespaces.md). With 32
  namespaces (64 watches) kausa used about 13 MiB in steady state and 23 MiB at its peak, against
  the 128 MiB limit, and was ready half a second after it started. The cap stays at 32.
- **Both kind runs passed on the maintainer's machine**, on the integrated branch: `cluster` (the
  v0.1 checks, the cross-namespace Event, the upgrade to `namespaced`) and `namespaced` (an
  unlisted namespace with the same failures left alone, `kubectl auth can-i` per namespace and
  with `-A`, nothing cluster-scoped, the reset upgrade refused with the Roles kept, a refusing
  namespace that keeps kausa not Ready with 0 restarts while another is explained, and an
  uninstall that leaves no Role). Each run used a kubeconfig file of its own; the maintainer's
  kubeconfig had the same checksum before and after, and every kind cluster was deleted.

### Hypotheses of 0013, settled in kind

All with Kubernetes v1.36.4 and Helm 3.16.1, on 2026-10-28:

- **The cross-namespace Event:** the API server accepted an `events.k8s.io/v1` Event in one
  namespace whose `regarding` object is in another, and kausa ignored it (no incident, no Event).
  So the check was needed.
- **`/version` needs no grant:** in namespaced scope, with nothing cluster-wide granted, kausa
  read the server version and became ready. Which default ClusterRole allows it was not checked.
- **An earlier release's stored values have no `rbac` map:** `helm upgrade --reuse-values` from the v0.1.0
  chart failed with `rbac.scope is not set`, without a new revision; with `--set
  rbac.scope=cluster` it passed, and the ClusterRole's rules were identical to v0.1.0's. Later
  `--reuse-values` upgrades carried the scope on.
- **A plain `helm upgrade --set …`** (neither `--reuse-values` nor a values file) rendered the
  defaults and failed the same way. Confirmed.
- **A missing listed namespace** fails `helm install` with `namespaces "<name>" not found`, and
  Helm keeps the release as `failed`; uninstalling it left no Role.
- **An edited Role** (a rule on ConfigMaps added) was put back by `helm upgrade` with the same
  values; so was a deleted RoleBinding, after which the watch errors stopped.
- **An open watch outlives its RoleBinding:** after the binding was deleted, the first watch
  error came 289 seconds later, which fits the watch's 290-second timeout ending it. Its `error` text
  says `forbidden` and the `403`. kausa stayed Ready (readiness never goes back) and did not
  restart.
- **Outside the chart, a namespace that does not exist** is watched as an empty one: with broad
  credentials, `kausa run --namespace <existing> --namespace <missing>` became ready with no
  watch error.
- **Not settled here:** installing without cluster-admin (whether the `admin` ClusterRole covers
  the Event writes), and whether the matrix doubles CI minutes but not wall time. Both need
  another setup or GitHub Actions.

### Found on the way

- With `--reuse-values`, going back from `namespaced` to `cluster` needs an explicit empty list:
  `--set-json 'rbac.namespaces=[]'` or a values file with `namespaces: []` worked, while `--set
  rbac.namespaces=null` and `--set 'rbac.namespaces={}'` left a list (`{}` renders as one empty
  name) and the chart refused. [Install kausa with Helm](../how-to/install-with-helm.md) says so.
- **A review found that a `403` could lose records for good.** kube-runtime 4.2.0 relists only
  after a failed list; a refused resumed watch (`WatchStartFailed`) or a `403` in an open stream
  keeps its resource version, so once a deleted RoleBinding was restored the watcher would have
  resumed without a relist, and the Pods kausa had dropped that did not change meanwhile (a
  Pending Pod for `D005`, a static waiting state for `D004`, an evicted Pod for `D009`) would not
  have come back. kausa now starts a new watcher after any `403` (`Restarting` in `src/run.rs`),
  with a backoff that the `Init` of each new watcher does not reset; ADR 0013 has an `Update
  2026-10-28` paragraph. The unit test that fed an `Init` after `WatchStartFailed`, which kube
  never sends, now drives that sequence through `Restarting` over scripted watchers.

### Follow-ups

- **For the maintainer:** if branch protection on GitHub requires a check named `kind`, replace
  it with `kind (cluster)` and `kind (namespaced)`, or no pull request can merge (hypothesis: the
  required check is named after the job). Both matrix jobs still have to pass on GitHub Actions.
  `CONTRIBUTING.md` and [Maintain the repository](../how-to/maintain.md) now name both checks,
  and the latter's "Change the required checks" has the commands; until the ruleset is changed,
  `e2e.yml` says that both checks gating pull requests is pending.
- The release notes of the next release must say that `rbac.scope` is required, as the
  changelog's `Changed` line does.

## Later the same day: the hypotheses of the explanation history

### Request

Before any code of [0014. Keep each incident's full explanations in a namespaced Explanation
resource, off by default](../adr/0014-explanation-history.md) merges, settle in a throwaway kind
cluster the hypotheses the ADR lists about the Explanation CustomResourceDefinition (CRD).

### What was done

A kind cluster with the node image pinned in `.github/workflows/e2e.yml` (Kubernetes v1.36.4,
kind v0.33.0, Helm 3.16.1), a kubeconfig file of its own (the maintainer's had the same checksum
before and after) and deleted at the end. The CRD and the reader role were rendered with `helm
template --show-only` from a scratch copy of the chart carrying the schema file and the templates
as the plan specifies them (the chart commits come next), and applied as cluster administrator.
Raw calls went through `kubectl proxy`, so that the HTTP codes are the ones kausa will see.

### Hypotheses of 0014, settled in kind

Every hypothesis was confirmed; no fallback is needed (the name keeps its dot, and `-o wide`
keeps its two columns).

- **The dot in names:** `kausa.0123456789abcdef` was created (`201`).
- **`status` kept on create** without a status subresource: `occurrences`, `lastTime` and all
  nine fields of the entry read back.
- **`maxLength` counts characters:** a summary of 300 `é` (600 bytes) was accepted and 301 were
  refused with `422`. The message still says `may not be more than 300 bytes`.
- **CEL `self == oldSelf`:** changing `spec.rule` or `spec.target.name` was refused with `422`
  and `spec: Invalid value: spec is immutable`.
- **`required` on update and on create:** a merge patch `{"spec":null}` (`spec: Required value`)
  and one setting `spec.target.name` to null were refused, as were a create without `spec` and an
  entry without `count`. A `lastTime` that is not a date-time is refused too.
- **`resourceVersion` as a precondition of a merge patch:** with the current version the patch
  answered `200`; with the version read before it, `409` (`the object has been modified`). A
  delete with a stale `preconditions.resourceVersion` answered `409`, with the current one `200`,
  and again `404`.
- **`fieldValidation=Strict`** refused an unknown field in `status` and in an entry with `422`
  (`strict decoding error: unknown field "status.bogus"`); without the parameter the field was
  pruned and the patch answered `200`.
- **`[0]` in printer columns:** the CRD was accepted, and `kubectl get explanations -o wide`
  printed `KIND TARGET RULE COUNT LAST AGE CONFIDENCE SUMMARY` with the first entry's values.
- **The `view` aggregation:** service accounts bound to `view` and to `edit` in the namespace
  could `get`, `list` and `watch` Explanations and could not `create`, `patch` or `delete` them.
- **A `fieldSelector` on `metadata.name`** listed the one object, and another name none; a
  `labelSelector` with `limit=1` returned a `continue` token.
- **Helm refuses a second release rendering the same CRD:** `invalid ownership metadata`, naming
  the owning release; uninstalling the owner deleted the CRD and its Explanation.
- **Also observed:** the largest object the schema allows (four-byte characters at every cap,
  every target string at its cap) was accepted at 55,319 bytes of JSON, about the ADR's
  55 KB; with the CRD deleted, a list answered `404`.

### Found on the way

- **A `422` from `fieldValidation=Strict` quotes the whole stored object** in its message (1,358
  characters here, the stored summary included), not only the patch. The plan logs "the kube error
  string" in `history write failed`, which would put text read back from an Explanation into a log
  line, against invariant 5. Handed to the code track: such an error must be logged without the
  API server's message.

## Later the same day: explanation history

### Request

Build the explanation history that [0014. Keep each incident's full explanations in a namespaced
Explanation resource, off by default](../adr/0014-explanation-history.md) accepted, on top of the
namespaced install mode, in a code track and a chart track on the branch `feat/v0.3-history`, and
integrate it: the docs the ADR names, the full checks and both kind runs.

### What was done

- **`prompt::clean` became idempotent first.** It was not: `clean("ab cd", 3)` gave `"ab "`, and
  cleaning that again gave `"ab"`; a cut could also leave part of a `<redacted:…>` marker. kausa
  would then have dropped its own stored entries as unclean on the next read. The cut now trims
  the end and drops a partial marker (the changelog's `Fixed` line), with unit tests and a fuzz
  assertion that `clean(clean(x)) == clean(x)`.
- **Code:** `src/history/pure.rs`, the pure part (the name, the create and patch bodies, the
  reading of a stored object as untrusted input, the identity check, the merge and the sweep's
  plan with its bounded heap), with a test that reads `charts/kausa/files/explanation-schema.json`
  and ties its limits to the constants; `src/history.rs`, the writer and the sweep over
  `Api<DynamicObject>`; `--history` and `--history-days` with their usage errors; the Event and
  the history as two independent steps of `emit` in `src/explain.rs`; the sweep in the
  explanation slot, chosen by a pure picker in `src/run.rs`; the three counters; and the fuzz
  target `history`. No new crate: `Cargo.lock` and `fuzz/Cargo.lock` did not change.
- **Chart:** the schema file, the `history` values, the helpers `kausa.writesHistory`,
  `kausa.historyDays` and `kausa.installsCRD`, the `explanations` rule last in `kausa.rules`, the
  flags, the CustomResourceDefinition and reader templates, `NOTES.txt`, the `history.days`
  refusal, and the history renders in `tests/chart/render.sh` and the `ci` target. Both kind runs
  install with `history.enabled=true`.
- **Integration:** the chart track was merged into the code track with no conflict (both carried
  the same schema file). One fix came out of the earlier kind session: a failed create or patch is
  now logged with the API server's code and reason only, because a `422` quotes the whole stored
  object in its message, and nothing read back may reach the log.
- **Docs:** `ARCHITECTURE.md` (invariant 2 as the ADR words it, the flow, the codemap, the pure
  list, schema changes), `AGENTS.md`, the [Security policy](../../SECURITY.md) (the history's
  threat model and new vulnerability cases), [Permissions](../reference/permissions.md), [Helm
  chart](../reference/helm-chart.md), [Command line and log format](../reference/command-line.md),
  the new [Explanation resource](../reference/explanation-resource.md),
  [Architecture](../explanation/architecture.md), [Install kausa with
  Helm](../how-to/install-with-helm.md), [Data sent to the
  provider](../reference/data-sent-to-the-provider.md), the README, the changelog, the front page,
  and `Update` paragraphs in [0009. Run with read-only permissions that never include
  Secrets](../adr/0009-least-privilege-rbac.md) and [0001. Build a small, read-only explainer that
  writes one redacted explanation per incident](../adr/0001-scope-and-positioning.md) (its
  out-of-scope line). 0014 itself was not edited.

### How it was checked

- `cargo fmt --check`, `clippy -D warnings` and `cargo test` (194 unit tests, 24 command-line
  tests) on the integrated branch; `cargo check` of the fuzz crate; `helm lint --strict` with the
  history off and on; `tests/chart/render.sh` (every check passed, the `history.days` refusals
  through `helm template`).
- **`docker build --target ci .` did not run to the end**: the Docker VM's disk was full (`No
  space left on device` in the `cargo deny` step, an environment problem, not a finding). Each
  `RUN` step of the `ci` target was run on the host instead, in order, with Helm 3.16.1 instead of
  the pinned one: `gitleaks` (no leaks), `cargo fmt`, `cargo deny` (advisories, bans, licenses and
  sources ok), `cargo audit`, `helm lint`, the four render and refusal steps, `clippy` and the
  tests all passed. The runtime image built in both kind runs.
- **Both kind runs passed on the maintainer's machine** with the image of this branch
  (Kubernetes v1.36.4): `cluster` (two Explanations with every field read back, the `-o wide`
  columns, `kubectl auth can-i` on what kausa may and may not do, `view` and `edit` reading and
  not writing, the CRD's refusals, the sweep after a restart deleting the old kausa-shaped object
  and keeping the other two, as many requests as model calls, `dryRun=true` and
  `history.enabled=false` removing the rule, the reader role, the CRD and the Explanations, and a
  restart without the CRD: Ready, no restart, `history sweep failed` naming it) and `namespaced`
  (Explanations only in `e2e-shop`, the same checks per namespace, and an uninstall that left no
  Role, reader role or CRD). Each run used a kubeconfig file of its own; the maintainer's
  kubeconfig had the same checksum before and after, and every kind cluster was deleted.

### Hypotheses of 0014, settled in kind

The earlier section settled each of the ADR's hypotheses by hand; no fallback was needed (the name
keeps its dot, and `-o wide` keeps its two columns). The two kind runs confirmed some of them
again with kausa itself writing: the objects named `kausa.<fingerprint>`, `status` kept on
create, and the `-o wide` columns from `entries[0]`. The others ran again in the script, with
`kubectl` as cluster administrator on hand-made objects, not through kausa: the CEL and
`required` refusals, `409` on a stale `resourceVersion` and `maxLength` in characters; and the
`view` aggregation was checked with service accounts. Those runs created Explanations but did
not exercise kausa's own update (the list, the identity check and the merge patch with
`fieldValidation=Strict`); the review below added that. Also seen: kausa tolerates a missing CRD
without restarting. Not checked, and still hypotheses: etcd's 1.5 MiB
request limit in other clusters, RBAC's escalation check for an installer with
`history.installCRD=false`, finalizers stalling the CRD's deletion, a CRD installed later being
picked up without a restart, and what backups copy.

### Found on the way

- **`prompt::clean` was not idempotent** (above), which the history's "unclean entries are
  dropped" rule would have turned into kausa discarding its own history.
- **`helm upgrade --reuse-values` renders with the old chart's values**, so a release installed
  with an earlier chart has no `history` map at all, and `default` would turn an explicit `false`
  into `true`. The chart reads `history` nil-safely and defaults its keys with `hasKey`;
  `tests/chart/render.sh` renders a chart copy without the map.
- **A `422` from `fieldValidation=Strict` quotes the stored object** in its message, fixed in the
  log line as described above.

### Follow-ups

- Run `docker build --target ci .` again once the Docker VM has disk space, and in CI.
- Build the fuzz target with `cargo +nightly fuzz build history` (no nightly toolchain was
  available; `cargo check` of the fuzz crate passed).
- GitHub Actions on the pull request: both kind checks and the fuzz run.
- The release notes of v0.3 must say that turning the history off or uninstalling deletes every
  Explanation.

## Later the same day: review fixes of the explanation history

### Request

Fix two findings of the review of `feat/v0.3-history`, each confirmed by two of three skeptics:
the sweep's memory was not bounded against forged objects, and nothing tested kausa's own update
of an Explanation.

### What was done

- **The sweep's page went from 50 objects to 2.** The schema bounds `spec` and `status`, not
  `metadata`, and the label selector returns any object with kausa's label, so whoever may create
  Explanations in a covered namespace can make objects up to etcd's limit (about 1.5 MiB) with
  labels, annotations or finalizers. kube buffers and parses a whole page. Measured with
  kube-core 4.2.0 and a counting allocator, outside the cluster: an object of 1.4 MiB of small
  labels took about 12 MiB once parsed into a `DynamicObject`, so a page of 50 would have needed
  about 585 MiB, above the chart's 128Mi limit, at every start; a page of 5 about 59 MiB, and a
  page of 2 about 23 MiB plus the 2.9 MiB body. The sweep also drops the annotations, the other
  labels, the finalizers, the managed fields and the owner references before it reduces each
  object. The ADR got an update paragraph; the reference page and `SECURITY.md` now say that the
  55 KB per object holds only for objects kausa writes.
- **kausa's own write path is tested.** Unit tests against the fake API server in
  `src/history.rs`: an empty list and a `POST` of exactly `pure::create_body`; a listed object and
  a `PATCH` with `fieldValidation=Strict`, `application/merge-patch+json` and exactly
  `pure::patch_body`; an object without the label refused with no second request; a `PATCH`
  answered `409` as a conflict; a list answered `404` naming the missing CRD; and the outcome
  counted by `write`. `tests/e2e/run.sh` now waits, after the restart that empties the
  deduplication cache, for `kausa_history_writes_total{outcome="updated"}` and for crasher's
  Explanation to have more occurrences and a higher count or later `lastTime`, with no `error`
  or `refused` write.

### How it was checked

`cargo fmt --all -- --check`, `cargo clippy --locked --all-targets --all-features -- -D
warnings` and `cargo test --locked --all-features` passed; dropping `fieldValidation=Strict` or
the metadata reduction made a new test fail. Both kind runs passed with the image of the fixed
branch (Kubernetes v1.36.4), each with a kubeconfig file of its own; the maintainer's kubeconfig
had the same checksum before and after, and every kind cluster was deleted.

### Follow-ups

- A typed list that skips unread fields while parsing would bound a page by its bytes alone;
  not done, since it needs serde's `derive` as a direct dependency (hypothesis: about the body's
  size).

## Later the same day: the sweep's failures without the API server's message

### Request

Fix a finding of the guardian's review: commit `1710aa4` left the API server's message out of a
failed create or patch, but the sweep's refused deletes and every failed list still logged kube's
whole error.

### What was done

- **One helper tells an API error.** `api_failure` in `src/history.rs` gives an answer of the API
  server as its code and reason only, and kube-rs's text for any other failure; `Written::failed`,
  `list_failure` (the write's list and the sweep's) and the sweep's delete use it. A list answered
  `404` still names the missing CRD. A delete refused with a `403`, a `5xx` or by an admission
  webhook comes back with a message that, in cluster scope, names the listed object's namespace
  and can quote the object, against invariant 5.
- **The log line is tested.** kausa's log is written straight to standard output, which the test
  harness does not capture, so a unit test runs the sweep in a child copy of the test binary,
  against the fake API server with deletes answered `403` and `500` whose message carries a
  marker, and checks that its two `history sweep failed` lines carry neither the marker nor the
  listed namespace, with `namespace` null. The `history write failed` and `history sweep failed`
  rows of [Command line and log format](../reference/command-line.md) say so.

### How it was checked

`cargo fmt --all -- --check`, `cargo clippy --locked --all-targets --all-features -- -D
warnings` and `cargo test --locked --all-features` passed; with the delete logging kube's text
again, the new test failed. No kind run: the change touches only the text of two log lines.
