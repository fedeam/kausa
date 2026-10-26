# Data sent to the provider

Exactly what a kausa request to the language model endpoint contains, and what it never
contains.

Status as of 2026-10-23: **planned** for v0.1, nothing implemented. Why it works this way: [0008.
Send only allowlisted, redacted data to one OpenAI-compatible
endpoint](../adr/0008-data-leaving-the-cluster.md). Run kausa with `--dry-run` (planned) to see
the requests it would send.

## Where it goes

One endpoint, the base URL in kausa's configuration, with `POST <base URL>/chat/completions`.
Nothing else: no telemetry, no update checks, no fallback provider. With a model served inside the
cluster or the private network, nothing leaves it.

## What one request contains

After the allowlist, pseudonyms and pattern redaction of [Detection and redaction
rules](rules.md):

| Field | Example after redaction | Notes |
|---|---|---|
| Detection | `D001` crash loop | The rule id and its name |
| Workload kind | `Deployment` | The top-level owner's kind, and the kinds in the chain |
| Names | `namespace-1`, `deployment-1`, `pod-1`, `container-1` | Pseudonyms (`R002`); the real names never leave |
| Image | `docker.io/library/nginx:1.27` or `registry-1/image-1:tag` | Public registries kept, others pseudonymized |
| Container state | waiting `CrashLoopBackOff`; last termination `Error`, exit code `1` | Reasons and exit codes as Kubernetes reports them |
| Restart count and Pod phase | `7`, `Running` | |
| Probe and resource settings | probe type and timing, memory limit `256Mi` | Numbers only, for `D003` and `D006` |
| Warning events | reason, redacted message, count, first and last seen | Only the events of this incident |
| Log tail | the last lines of the failed container, redacted | Only `D001`, `D003` and `D008`: the previous instance (`previous=true`) for `D001` and for `D003` when the container restarted, the terminated container itself (`previous=false`) for `D008` and any Pod that did not restart; capped by lines and bytes; can be turned off |
| Number of Pods affected | `3 of 3` | |

The system message is fixed in the kausa binary and versioned with it. Every value above sits in
one JSON document marked as untrusted data, and the request asks for a JSON answer with a fixed
schema. The provider's own `usage` numbers (tokens in and out) are recorded in the metrics.

## What a request never contains

- Secrets and ConfigMaps: kausa cannot read them ([Permissions](permissions.md)).
- Labels, annotations, environment variables, command lines and arguments, volume definitions,
  UIDs, resource versions and managed fields.
- Real names of namespaces, workloads, Pods, containers, nodes and service accounts, and private
  registry hosts.
- Anything a redaction rule matched: IP addresses, email addresses, credentials in URLs,
  authorization values, JSON Web Tokens, PEM blocks, values of secret-named keys, known token
  formats, high-entropy strings and hostnames.
- The cluster's name, its API server address and kausa's configuration.

## What stays in the cluster

- The full explanation, with real names mapped back, in kausa's own log.
- A short version, up to the 1024-byte limit of an Event note, in an Event about the failing
  workload.
- The pseudonym mapping, the fingerprints and the deduplication cache, in kausa's memory only;
  nothing is written to disk.
