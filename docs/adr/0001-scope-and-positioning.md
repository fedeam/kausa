# 0001. Build a small, read-only explainer that writes one redacted explanation per incident

- Status: accepted
- Date: 2026-10-23

## Context

When a workload fails in Kubernetes, the facts are scattered: Warning events on the Pod, the
container state and exit code in its status, the last log lines of the previous container, and
the owner chain (ReplicaSet, Deployment, Job) that tells which application it is. Putting them
together and knowing what each failure usually means (`CrashLoopBackOff` after an
`OOMKilled`, an image pull that fails on authentication, a probe that never passes) takes
experience. Large language models (LLMs) are good at turning those facts into a plain
explanation and a next step.

The maintainer, FedeAM, had a competitive survey done with independent fact-checking
before deciding anything ([Competitive landscape](../research/2026-10-23-competitive-landscape.md),
2026-10-23). Its findings, in short:

- The space is crowded and alive. **k8sgpt** and its operator (Go, Apache-2.0, CNCF Sandbox),
  **HolmesGPT** (Python, Apache-2.0, CNCF Sandbox) and **kagent** (CNCF Sandbox) all explain
  Kubernetes problems with LLMs, and all support local models through Ollama or an
  OpenAI-compatible endpoint.
- No single feature of the planned tool is new. What none of them ships together is: a small
  single-container controller that **watches** instead of polling; deterministic correlation of
  one workload's failures into **one incident and one prompt**; **content redaction on by
  default** before anything leaves the cluster; a prompt that treats cluster text as untrusted
  data, with schema-bound output and no tools; a **call and token budget** with deduplication of
  repeated incidents; the explanation written as a **Kubernetes Event on the failing object**, so
  `kubectl describe` shows it; and permissions that never include Secrets or ConfigMaps.
- Built naively, the project would be "k8sgpt-operator in Rust" and would lose to an active CNCF
  project. Expected adoption is low; the value as a tool the maintainer uses on their own clusters,
  and as a public example of a careful controller, is high.

## Alternatives considered

- **Do not build; contribute to k8sgpt instead.** Some pieces would fit upstream (content
  redaction, a call budget, Events on the failing object). Its architecture would not change
  cheaply: a separate server called over gRPC, a reconcile loop that polls every 30 seconds, and a
  workload role that reads Secrets cluster-wide. The footprint and least-privilege story needs a
  new codebase. Small fixes found during the survey can still go upstream, separately.
- **An agentic investigator** (the HolmesGPT approach): the model calls tools in a loop. More
  capable on hard problems, but each investigation costs many calls, the model decides what to
  read, and every tool is a path for prompt injection. Rejected for a controller that runs
  unattended on every failure.
- **A dashboard or chat interface:** outside what the maintainer wants; Events and logs reach
  every existing tool already.

## Decision

kausa is a small, read-only Kubernetes controller that watches Warning events and pod status,
groups the failures of one workload into one incident with deterministic rules, sends a redacted,
allowlisted context to one OpenAI-compatible endpoint within a call and token budget, and writes
the explanation as a Kubernetes Event on the failing object. FedeAM decided this on
2026-10-23, after the survey, as a narrow build with that positioning.

In scope for v0.1:

- Detection of the common workload failures and their grouping by owner, listed in [Detection
  and redaction rules](../reference/rules.md).
- Redaction on by default, as proposed in [0008. Send only allowlisted, redacted data to one
  OpenAI-compatible endpoint](0008-data-leaving-the-cluster.md).
- Permissions as proposed in [0009. Run with read-only permissions that never include
  Secrets](0009-least-privilege-rbac.md).
- A call and token budget, deduplication by incident fingerprint, and Prometheus metrics.
- A dry-run mode that builds and logs the redacted request and sends nothing.
- A multi-architecture (linux/amd64 and linux/arm64), minimal, non-root image with declared
  resource limits, a Helm chart, and end-to-end tests in a kind cluster.
- A published memory and CPU measurement on a Raspberry Pi 4, and a token comparison against
  k8sgpt on the same seeded failures.

Out of scope:

- Changing anything in the cluster other than writing Events: no remediation, no patches, no
  restarts, no exec.
- Tools or function calling for the model, and multi-step agent loops.
- A user interface, chat integrations or notification sinks.
- Reading Secrets, ConfigMaps or metrics backends.
- A hosted service, telemetry or any network call other than the Kubernetes API and the configured
  LLM endpoint.

Roadmap, as planned on 2026-10-23 (the weekends give the order of work, not dates):

| Step | Content |
|---|---|
| Weekend 1 | Watches, detection rules, settle window, owner correlation, incident fingerprint |
| Weekend 2 | Redaction with a test corpus, the untrusted-data prompt, the OpenAI-compatible client, dry-run |
| Weekend 3 | Budget, deduplication, metrics, Events on the failing object, Helm chart, multi-arch image, kind tests in CI |
| Weekend 4 | Memory and CPU measured on a Raspberry Pi 4; token comparison against k8sgpt on seeded failures |
| v0.2 | Longer explanations and history in a custom resource; a namespaced install mode if v0.1 ships only the cluster-wide one |

## Consequences

- Positioning is a combination, not a feature: each piece has to be done well, and the README
  compares kausa with the other tools honestly, with dates and sources.
- Events are short-lived (the API server keeps them one hour by default, `--event-ttl`) and their
  note is limited to 1024 bytes (`NoteLengthLimit` in Kubernetes'
  `pkg/apis/core/validation/events.go`, which compares the byte length; checked on 2026-10-23).
  The explanation must be terse;
  longer text waits for the v0.2 custom resource, and kausa's own log carries the full text.
- Without tools, the model only sees what kausa sends. Some failures (a wrong value inside a
  ConfigMap, a NetworkPolicy that blocks traffic) cannot be explained from that context; the
  explanation says what to check instead.
- The incumbents keep moving: if k8sgpt adds content redaction or Events on the failing object,
  the combination is worth less. The survey is dated and is repeated before publication.
- The programming language and the license, the language of the repository and its name are
  decided in [0002. Write kausa in Rust with kube-rs and license it MIT OR
  Apache-2.0](0002-rust-and-dual-license.md), [0004. Write everything in
  English](0004-english-only.md) and [0005. Name the repository kausa, without a type
  prefix](0005-name-without-prefix.md).
