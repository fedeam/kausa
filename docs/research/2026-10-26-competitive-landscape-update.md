# Competitive landscape, update of 2026-10-26

The competitive survey of 2026-10-23 repeated three days later, as the publication checklist asks:
what changed, and what the first survey missed.

Done as the first: one researcher checked every tool in [Competitive
landscape](2026-10-23-competitive-landscape.md) and searched for new ones, and an independent
fact-checker re-read the primary sources and corrected the report. Activity from the GitHub API;
everything else from the projects' repositories.

## What changed since 2026-10-23

Nothing that touches kausa's combination. No new release of k8sgpt (still v0.4.41), its operator
(v0.2.29), HolmesGPT (0.45.0), kagent (v0.10.4), khook (v0.0.4), kwatch (v1.1.0), Robusta (0.53.0)
or kubectl-ai (v0.0.31); kubernetes-mcp-server released v0.0.69.

- **k8sgpt** pull request #1845, the one the first survey flagged as a way to close the gap, is
  still open, approved on 2026-10-20 and not merged. It lets users mute known findings by a hash
  of the finding, which skips the model call for them: a manual list of exclusions, not automatic
  deduplication of incidents or a call budget. k8sgpt's next release, prepared on 2026-10-26, adds
  Karpenter analyzers and fixes; nothing about content redaction, Events or budgets.
- **kagent** issue #3329 (2026-10-23) confirms that its chart lets the controller read every
  Secret in the namespaces it watches and asks for smaller roles; issue #2148 (Events about its
  reconciles) was closed as not planned on 2026-10-24.
- **LiteLLM**, one of the gateways the first survey named as a way to redact and budget in front
  of any model, had a compromised release on 2026-10-21 (litellm 1.107.0 on PyPI and its
  container images, withdrawn the same day; OSV `MAL-2026-2144`, LiteLLM issue #47826). Not a
  change in features, but a reminder that a gateway in front of the model is one more component
  with its own supply chain.

## What the first survey missed

Small projects, all MIT-licensed, with little or no adoption; none ships kausa's combination:

- **[incident-explainer](https://github.com/farooqhussain82/incident-explainer)** (created on
  2026-10-19, one star, two commits): the closest. A Go controller in the cluster that redacts
  content by default before calling Ollama, does not read Secrets, binds the answer to a JSON
  schema, and reports a 13 MB image in its README. It lists Pods every 15 seconds instead of
  watching, opens one incident per Pod, writes no Events, uses placeholders rather than
  deterministic pseudonyms, and has no budget, no deduplication and no untrusted-data framing.
- **[ai-kubernetes-troubleshooter](https://github.com/kmpoltorak/ai-kubernetes-troubleshooter)**:
  a service with a database and a REST API, triggered per incident through the API rather than
  watching the cluster; it redacts logs and events, gives the model no tools and validates the
  answer.
- **k8s-doctor** (a read-only agent that combines rules and a model explanation),
  **kubectl-ai-explain** (a kubectl plugin) and **OpenSRE** (an investigating agent with a web
  console): different shapes, not in-cluster explainers like kausa.
- A paper, **LogJack** (arXiv 2604.15368, 2026-04-15), studies indirect prompt injection through
  cloud logs against debugging agents: evidence for treating cluster text as untrusted.

## What it changes

- The gap is still open: no tool combines a watching, single-binary controller, one incident per
  workload, deterministic pseudonyms, untrusted-data framing, a budget with deduplication and
  Events on the failing object.
- One angle is weaker than the first survey said. "Content redaction built into the path to the
  model, on by default" exists, in small projects without adoption; it is not unique to kausa.
- The README comparison stays as it is: it compares kausa with k8sgpt, HolmesGPT, kagent and
  kwatch, and those statements still hold.
