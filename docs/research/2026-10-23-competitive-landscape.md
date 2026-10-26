# Competitive landscape

The open-source tools that explain Kubernetes failures with a language model, as surveyed on
2026-10-23, and what gap is left for kausa.

The survey was done before the decision in [0001. Build a small, read-only explainer that writes
one redacted explanation per incident](../adr/0001-scope-and-positioning.md): four researchers
covered k8sgpt, HolmesGPT, kagent and the rest of the 2025–2026 field, and the candidate angles;
an independent fact-checker re-read the primary sources of each report and tried to refute it.
Where they disagreed, the source was read again. Sizes are compressed image sizes from the
registries' manifests; activity from the GitHub API. Everything here is dated and will be
repeated before publication ([0006. Make the GitHub repository public only after a
checklist](../adr/0006-publication-checklist.md)).

## The tools

| Tool | License, language | Activity as of 2026-10-23 | Shape and footprint | Secrets in its permissions | Redaction before the model | Local models |
|---|---|---|---|---|---|---|
| [k8sgpt](https://github.com/k8sgpt-ai/k8sgpt) | Apache-2.0, Go, CNCF Sandbox | v0.4.41 on 2026-10-21 | Command-line tool and server; image 68.7 MB amd64, 63.7 MB arm64 | Chart's ClusterRole: `get`, `list`, `watch` on all resources, Secrets included | `--anonymize` masks object identifiers only, off by default in the command-line tool; log and event text is sent as is | Yes (Ollama, LocalAI, LiteLLM, custom REST) |
| [k8sgpt-operator](https://github.com/k8sgpt-ai/k8sgpt-operator) | Apache-2.0, Go | v0.2.29 on 2026-08-13 | Reconciles every 30 seconds instead of watching; operator, server and kube-rbac-proxy, about 124 MB arm64 and about 384Mi of memory requests in total | The analysis workload's role reads Secrets and `pods/log` | Anonymization on by default, identifiers only | Through LocalAI or a base URL |
| [HolmesGPT](https://github.com/HolmesGPT/holmesgpt) | Apache-2.0, Python, CNCF Sandbox | 0.45.0 on 2026-10-15 | Agentic investigator (up to 100 tool steps), server plus an alpha operator; image 282 MB arm64; 2048Mi memory request | No Secrets; reads ConfigMaps and `pods/log` cluster-wide | None found on tool output | Through LiteLLM; Ollama marked experimental |
| [kagent](https://github.com/kagent-dev/kagent) | Apache-2.0, Go and Python, CNCF Sandbox | v0.10.4 on 2026-10-21 | Framework for agents in Kubernetes, several components and a database; tools image 225 MB arm64 | Tools installed with cluster-admin by default | None found | Yes (Ollama) |
| [khook](https://github.com/kagent-dev/khook) | Apache-2.0, Go | Last release v0.0.4 on 2025-09-18 | Watches events and hands them to kagent; 35 MB arm64 | No | None found | Through kagent |
| [kwatch](https://github.com/abahmed/kwatch) | Elastic License 2.0 (not OSI-approved), Go | v1.1.0 on 2026-10-22 | Failure notifications with correlation; 42 MB arm64, armv6 and armv7 too | Reads Secrets unless turned off | Token and key redaction for notifications | No language model, by design |

Also seen: kubectl-ai (Google, a command-line assistant), kubernetes-mcp-server (an MCP server
with a read-only mode and denied resources), Robusta (MIT; deterministic triggers in front of
HolmesGPT, with log censoring for notifications), and gateways such as agentgateway and LiteLLM,
which can mask patterns and enforce token budgets in front of any model. Smaller projects
(kubetective, Apollo, k8s-pager) were either dormant, unlicensed or not in-cluster. No Rust
controller that explains failures with a model was found.

## The candidate angles

| Angle | Verdict | Evidence |
|---|---|---|
| Minimal binary for small clusters, edge and arm64 | Partly covered | No self-contained tool that calls a model is under about 60 MB or a single small container. Rust kube-rs controllers publish images of 5.6 to 11 MB. No tool publishes measured memory use |
| Deterministic rules before the model | Mostly covered | k8sgpt's analyzers are deterministic and send only failures, one prompt per result. Left: grouping a workload's Pods into one incident and one prompt |
| Redaction before anything is sent | Partly covered | k8sgpt masks identifiers, not content; HolmesGPT and kagent do not redact; content redaction exists for notifications and in gateways. Left: content redaction built into the path to the model, on by default |
| Local models | Covered | Every major tool supports Ollama or an OpenAI-compatible endpoint. Not a differentiator |
| Output as Events or custom resource status | Custom resources covered; Events on the failing object not found | k8sgpt writes `Result` resources, HolmesGPT a `HealthCheck` status; the Events they emit are about their own resources. None found writes the explanation on the failing workload (search not exhaustive) |
| Least-privilege permissions | Partly covered | HolmesGPT and khook do not read Secrets. A tighter default (no ConfigMaps, two `get`s for owner chains) is a posture, not a feature |
| Prompt injection | Gap at the prompt | k8sgpt places logs between separators with no escaping; it fixed a prompt-injection advisory in its auto-remediation (GHSA-rp7v-4384-hfrp, version 0.4.32). HolmesGPT documents indirect injection and defends on the tool side. None found frames cluster text as untrusted data with schema-bound output and no tools |
| Cost controls | Partly covered | k8sgpt caches answers (likely defeated by its random masks, read in the code, not tested); khook deduplicates for 10 minutes; gateways enforce budgets. Left: a call budget and fingerprint deduplication inside the explainer |

## The gap

No single feature is new; built naively, the project would be k8sgpt-operator rewritten in Rust,
against an active CNCF project. What no tool ships together:

- one small multi-architecture binary that watches instead of polling;
- deterministic correlation into one incident and one prompt;
- content redaction on by default, with deterministic pseudonyms;
- an untrusted-data prompt with schema-bound output and no tools;
- a call and token budget with fingerprint deduplication;
- the explanation as an Event on the failing object;
- permissions without Secrets or ConfigMaps.

Some pieces could go to k8sgpt upstream (content redaction, a budget, Events on the failing
object); its server-plus-polling architecture and broad roles would not change cheaply. Expected
adoption of a new tool is low; its value as a tool for small clusters and as an example of a
careful controller is the reason to build it.

## Risks found

- Events live one hour by default and their note holds 1024 bytes.
- Logs carry secrets, and pattern redaction has false negatives.
- A tool-less explainer can still mislead a person into running a harmful command, if injected
  text shapes its answer.
- Small local models on Raspberry Pi-class hardware are weak; "local" usually means a model on
  another machine of the same network.
- The incumbents may close the gap (k8sgpt has an open pull request for fingerprinting known
  findings).

## Not verified

- The memory use of any tool, and kausa's own target.
- That k8sgpt's cache misses at run time when anonymization is on.
- Ollama through the operator's LocalAI backend.
- That no tool writes Events on the failing object (the search was not exhaustive).
- That a single structured call is much cheaper than an agentic investigation per incident (the
  cheapest models in HolmesGPT's own benchmark already average about USD 0.02 per investigation).
