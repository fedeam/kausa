# 0008. Send only allowlisted, redacted data to one OpenAI-compatible endpoint

- Status: accepted
- Date: 2026-10-23

## Context

kausa's job is to send cluster data to a language model, and that data is sensitive: event
messages and log lines carry tokens, connection strings, IP addresses, internal hostnames and the
names of customers' namespaces and services. The competitive survey of 2026-10-23
([Competitive landscape](../research/2026-10-23-competitive-landscape.md)) found that:

- k8sgpt's `--anonymize` masks object identifiers only, and is off by default in the command-line
  tool; log lines and event text go to the provider as they are. Its masks are random per run,
  which likely defeats its own answer cache (read in the code, not tested).
- HolmesGPT and kagent do not redact what their tools return before it reaches the model.
- Content redaction exists elsewhere, but for notifications (Robusta, kwatch) or in a separate
  gateway in front of the model (agentgateway, LiteLLM).

The maintainer's requirements for kausa (2026-10-23): redact secrets, tokens, IP addresses and
names before anything is sent; document what data leaves and to which provider; and offer a fully
local option.

## Decision

kausa sends data to exactly one destination, an OpenAI-compatible Chat Completions endpoint
(`POST <base URL>/chat/completions`) configured by whoever installs it, and only after three
deterministic steps: an allowlist of fields, pseudonymous names, and pattern redaction. Proposed
while the repository was set up on 2026-10-23; FedeAM accepted it on 2026-10-26, with log
collection on by default for the crash-type and Job detections only.

1. **Allowlist.** Only the fields listed in [Data sent to the
   provider](../reference/data-sent-to-the-provider.md) are serialized into a request. Everything
   else never leaves: labels, annotations, environment variables, command lines, volumes, owner
   UIDs, and anything kausa did not read in the first place (Secrets and ConfigMaps, see [0009.
   Run with read-only permissions that never include Secrets](0009-least-privilege-rbac.md)).
2. **Pseudonymous names.** The identifiers kausa knows (namespace, owner, Pod, container, node,
   service account, and the registry host and path of images outside a short list of public
   registries) are replaced by stable placeholders per incident, such as `namespace-1` or
   `deployment-1`, wherever they appear, logs and messages included. The mapping stays in memory
   and is applied back to the explanation before kausa writes it: only these identifiers, read
   from the incident's own objects, are mapped back, and every other placeholder (`R003` to
   `R013`) stays redacted in Events and in kausa's log. Placeholders are numbered in
   order of appearance, never random, so the same incident produces the same request.
3. **Pattern redaction** of all free text (event messages, status messages, log lines): the
   redaction rules in [Detection and redaction rules](../reference/rules.md), such as IP
   addresses, credentials in URLs, bearer tokens, JWTs, private keys, values of secret-named keys,
   known token formats and long high-entropy strings. Each match becomes a typed placeholder
   (`<redacted:jwt>`), and the counts per rule are exported as metrics.

Around those steps:

- **Logs only where they explain the failure.** The last lines of the failed container (the
  previous instance if it restarted) are read only for crash-type and Job detections, capped by
  lines and bytes after redaction. They can be turned off,
  which also removes `pods/log` from the chart's permissions.
- **Untrusted data stays data.** The request is a fixed system message, versioned with kausa, and
  one user message that carries the redacted context as a JSON document, under a key that marks it
  as untrusted. The model gets no tools or functions. The answer must be JSON with a fixed schema;
  kausa validates it, caps its length, strips control and invisible formatting characters and
  writes it only as plain text.
- **Transport.** HTTPS with certificate verification. Plain HTTP only when the configuration
  allows it explicitly, for an endpoint inside the cluster or the same private network (for
  example Ollama next to kausa). No redirects to another host.
- **The API key** reaches the container through a Secret that the kubelet mounts or injects, so
  kausa never needs permission to read Secrets. It is sent only to the configured endpoint, in the
  `Authorization` header, and never logged.
- **Dry-run** builds and redacts the request, writes it to kausa's own log and sends nothing, so
  an operator can see exactly what would leave before enabling a provider.
- **Fully local** means pointing the base URL at a model served inside the cluster or the private
  network. kausa never falls back to another endpoint.

## Consequences

- Pattern redaction has false negatives: a secret in a format no rule knows still goes out. This
  is documented in the [Security policy](../../SECURITY.md), with the advice to use a local model
  for clusters where that is unacceptable, and every redaction rule ships with a test corpus.
- Names that kausa does not know (a Secret's name inside an event message, a hostname in a log
  line) are only caught by patterns. Rules for the formats of Kubernetes messages
  (`secret "<name>" not found`) cover the common cases.
- Pseudonyms make the explanation less specific to the model (it sees `deployment-1`) but keep the
  answer correct once names are mapped back; deterministic numbering keeps deduplication and any
  provider-side prompt caching effective.
- One API shape covers the providers and servers that document an OpenAI-compatible endpoint
  (Ollama, vLLM and LiteLLM among them, per their documentation); a provider is listed as
  supported only after it is tested. Structured output (`response_format` with a JSON schema) is
  not offered by every server: kausa falls back to instructions plus validation, and records which
  mode it used.
- Anything new that would leave the cluster (a field, a provider-specific API, telemetry) needs a
  new ADR and an update to [Data sent to the provider](../reference/data-sent-to-the-provider.md)
  and the [Security policy](../../SECURITY.md).

Update 2026-10-26: as implemented on the second weekend, the output mode is chosen with
`--no-json-schema`, with no automatic fallback, and is not logged; the redaction counts are in the
`request` and `explanation` log lines until the metrics arrive (third weekend); and the
configuration is a set of command-line flags, not a file ([Command line and log
format](../reference/command-line.md)).

Update 2026-10-28: for the eviction rule `D009`, each Pod in a request also carries its
`status.reason` and its `status.message`, cut at 1024 bytes in the cluster and pattern-redacted
like the other status messages (step 3), because the message is what tells a workload's own
`ephemeral-storage` or `emptyDir` limit from node pressure. It is sent for any Pod that has one, not
only for `D009`. The message names quantities and container names (pseudonymized) and, for an
`emptyDir` limit, the volume's name, which kausa does not know and does not pseudonymize. No log is
read for `D009`. [Data sent to the provider](../reference/data-sent-to-the-provider.md) lists the
fields.
