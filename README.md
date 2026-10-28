# kausa

![repository: public](https://img.shields.io/badge/repository-public-bc4c00) ![internet: no](https://img.shields.io/badge/internet-no-0969da) ![status: in development](https://img.shields.io/badge/status-in%20development-9a6700)

![license: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-0969da) ![rust: 1.99+](https://img.shields.io/badge/rust-1.99%2B-0969da) [![CI](https://github.com/fedeam/kausa/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/fedeam/kausa/actions/workflows/ci.yml) [![OpenSSF Scorecard](https://api.scorecard.dev/projects/github.com/fedeam/kausa/badge)](https://scorecard.dev/viewer/?uri=github.com/fedeam/kausa) [![OpenSSF Best Practices](https://www.bestpractices.dev/projects/16024/badge)](https://www.bestpractices.dev/projects/16024)

A tiny Kubernetes controller that explains failing workloads with an LLM: one redacted, budgeted explanation per incident, written as an Event on the object itself. It reads the cluster and writes nothing but its own Events.

> **Status: pre-alpha.** `kausa run` detects and groups failures, redacts their context, asks an
> OpenAI-compatible model to explain them within a call budget, and writes the explanation as an
> Event on the failing workload. The image and the Helm chart are in the repository; no release,
> image or chart is published yet.

## The problem

A Deployment's three replicas are crashing. The facts are spread over several places:

```text
$ kubectl get pods -n shop
NAME                   READY   STATUS             RESTARTS      AGE
api-7d9c8b6f4d-2xkqp   0/1     CrashLoopBackOff   6 (40s ago)   7m
api-7d9c8b6f4d-8hn2w   0/1     CrashLoopBackOff   6 (35s ago)   7m
api-7d9c8b6f4d-wq7lc   0/1     CrashLoopBackOff   6 (38s ago)   7m

$ kubectl logs -n shop api-7d9c8b6f4d-2xkqp --previous
Error: connect ECONNREFUSED 192.0.2.10:5432
    at TCPConnectWrapper.afterConnect [as oncomplete] (node:net:1555:16)
```

Someone has to connect the crash loop, the exit code, the log line and the owner, and know what
they usually mean.

## What kausa does

kausa watches Warning events and Pod status. When a failure persists, it groups the failing Pods of
one workload into one incident, takes the relevant status, events and last log lines, redacts
them, and asks one OpenAI-compatible model endpoint for an explanation, within a call and token
budget. It writes the answer as an Event on the Deployment:

```text
$ kubectl describe deployment -n shop api
...
Events:
  Type    Reason     Age   From   Message
  ----    ------     ----  ----   -------
  Normal  Explained  20s   kausa  3 of 3 pods crash on start: the app cannot reach its
                                  PostgreSQL server (connection refused on port 5432).
                                  Likely cause: the database is down or the address in the
                                  app's configuration is wrong. Check: 1. the database
                                  Service has ready endpoints 2. the host and port the app
                                  uses (confidence: medium)
```

Output of this shape was seen on a live cluster on 2026-10-26; the wording is the model's. What
the model saw was `deployment-1` in `namespace-1` and `<redacted:ip>:5432`; kausa maps the names
back before writing the Event.

## Try it

In a throwaway [kind](https://kind.sigs.k8s.io) cluster, with Docker, kind, kubectl and Helm, from
a clone of this repository. In dry-run kausa logs the redacted request of each incident and sends
nothing:

```sh
kind create cluster --name kausa-try
docker build --target runtime -t kausa:try .
kind load docker-image kausa:try --name kausa-try
helm install kausa charts/kausa -n kausa --create-namespace --set dryRun=true \
  --set image.repository=kausa --set image.tag=try --set image.pullPolicy=Never
kubectl create deployment crasher --image=kausa:try -- /kausa --no-such-flag
kubectl -n kausa logs deploy/kausa -f    # an incident and its request after about a minute
kind delete cluster --name kausa-try
```

To have it explain, point it at a model: [Install kausa with
Helm](docs/how-to/install-with-helm.md).

## What leaves the cluster

Only to the one endpoint you configure, and only after three deterministic steps:

1. **An allowlist of fields:** detection, kinds, container states and exit codes, restart counts,
   Warning events of the incident and, for crash loops, out-of-memory kills and failed Jobs, a
   capped log tail. Never labels,
   annotations, environment variables, command lines or anything from a Secret or a ConfigMap,
   which kausa cannot read.
2. **Pseudonyms for names:** namespaces, workloads, Pods, containers, nodes and private registries
   become `namespace-1`, `deployment-1` and so on, numbered deterministically.
3. **Pattern redaction** of free text: IP addresses, email addresses, credentials in URLs, bearer
   tokens, JWTs, private keys, values of secret-named keys, known token formats, high-entropy
   strings and hostnames.

Point the endpoint at a model served in your cluster (Ollama, vLLM or any OpenAI-compatible
server) and nothing leaves it. `--dry-run` shows the exact requests without sending them. The
full list: [Data sent to the provider](docs/reference/data-sent-to-the-provider.md).

## Design principles

- **Read-only, except for its Events.** It reads Pods, Events, and the metadata of ReplicaSets
  and Jobs to follow owners; `pods/log` only for crash loops, out-of-memory kills and failed
  Jobs, and that can be turned off. It never reads Secrets or ConfigMaps, and the only thing it
  writes is Events ([Permissions](docs/reference/permissions.md)).
- **Deterministic before the model.** Detection, grouping, fingerprints and redaction are rules,
  tested like code. One incident is one request.
- **Cluster text is untrusted.** Logs and event messages are data inside the prompt. The model
  gets no tools, its answer must match a schema, and kausa never acts on it.
- **Budgeted.** Caps on calls per hour and per day and on tokens per request; repeated incidents
  reuse the stored explanation.
- **Small.** One static binary for linux/amd64 and linux/arm64, a distroless non-root image of
  about 4.5 MB compressed (4.3 MB for arm64, 4.6 MB for amd64), and a small record per Pod and
  per Warning event instead of a cache of whole objects: 2 MiB and 0.67 seconds of CPU in 22
  minutes on a Raspberry Pi 4 with 96 Pods, in dry-run, and 31 MiB with 10,000 Pods, ready 1.6
  seconds after start (once each: [Memory and CPU on a Raspberry Pi
  4](docs/research/2026-10-26-resource-use-on-a-raspberry-pi.md), [Thousands of
  Pods](docs/research/2026-10-28-thousands-of-pods.md)).
- **No UI.** Events, logs and Prometheus metrics, which every existing tool already reads.

## How it relates to other tools

Checked on 2026-10-23 against each project's repository, charts and documentation; the details and
sources are in [Competitive landscape](docs/research/2026-10-23-competitive-landscape.md).

- **[k8sgpt](https://github.com/k8sgpt-ai/k8sgpt)** and its
  **[operator](https://github.com/k8sgpt-ai/k8sgpt-operator)** (CNCF Sandbox) run deterministic
  analyzers and explain their results with a model, from many providers including local ones. The
  operator reconciles on a 30-second interval and stores results in a custom resource; its
  anonymization masks object names but not log or event text, and the roles it installs read
  Secrets. kausa watches instead of polling, groups a workload's failures into one request,
  redacts the content as well as the names, and never reads Secrets. k8sgpt does much more: many
  analyzers, an MCP server, integrations and a large community.
- **[HolmesGPT](https://github.com/HolmesGPT/holmesgpt)** (CNCF Sandbox) is an agentic
  investigator: the model calls tools (Kubernetes, Prometheus, logs and more) in a loop and can
  dig into hard problems. kausa makes one tool-less call per incident, which costs less, is
  easier to bound and gives injected text fewer ways in, but sees only what it sends.
- **[kagent](https://github.com/kagent-dev/kagent)** (CNCF Sandbox) is a framework for building AI
  agents that run in Kubernetes. kausa is a single-purpose controller, not a framework.
- **[kwatch](https://github.com/abahmed/kwatch)** watches for failures, correlates them and sends
  notifications, without a model, under the Elastic License 2.0. kausa adds the explanation and is
  `MIT OR Apache-2.0`.

## Status

Pre-alpha. v0.1 is feature-complete and not released: `kausa run` watches Pods and Warning
events, groups failures by workload, builds a redacted context with the failed container's log,
asks the model within a call budget (or logs the request with `--dry-run`), reuses explanations
of repeated incidents, writes each explanation as an Event on the failing workload, and serves
Prometheus metrics with `--metrics-address`; its log lines are JSON ([Command line and log
format](docs/reference/command-line.md)). The image and the Helm chart are built and checked in
CI, and the end-to-end test in kind passes; nothing is published yet.

What was measured, once each and on small setups: on five seeded failures with the same small
local model, kausa used 5 model calls and 2,948 tokens where k8sgpt used 12 calls and 5,128
([Tokens per incident compared with
k8sgpt](docs/research/2026-10-26-tokens-compared-with-k8sgpt.md)); on a Raspberry Pi 4 it used
about 2 MiB of memory ([Memory and CPU on a Raspberry Pi
4](docs/research/2026-10-26-resource-use-on-a-raspberry-pi.md)). Explanations were tested with
Ollama and small local models; a hosted provider over HTTPS has not been tried yet. What has
changed so far is in the [Changelog](CHANGELOG.md).

## Build from source

With Rust installed through [rustup](https://rustup.rs), which picks up the toolchain pinned in
`rust-toolchain.toml`:

```sh
cargo build --locked
cargo run --locked -- --help
# Watch the cluster of the kubeconfig's current context and log incidents (read-only):
cargo run --locked -- run
# Also log the redacted request each incident would send:
cargo run --locked -- run --dry-run
# Ask a model served by Ollama on this machine:
cargo run --locked -- run --endpoint http://127.0.0.1:11434/v1 --model qwen3:0.6b --allow-http
```

`kausa run` uses the current context of your kubeconfig and needs read access to Pods, their
logs, Events, ReplicaSets and Jobs, and, with `--endpoint` and without `--no-events`, permission
to create and patch Events; it has no flag to pick a context, so check
`kubectl config current-context` first, or follow [Try kausa against a cluster with a local
model](docs/how-to/try-with-a-local-model.md) to put a read-only proxy in front of it.

To run it in a cluster, build the image (`docker build --target runtime -t <registry>/kausa:<tag>
.`), push it, and install the chart in `charts/kausa` with `--set image.tag=<tag>`: [Install
kausa with Helm](docs/how-to/install-with-helm.md).

## Roadmap

- **v0.1** (feature-complete, not released): the watches, eight detection rules, the settle
  window, owner correlation and fingerprints; redaction with a test corpus, the untrusted-data
  prompt, the OpenAI-compatible client and dry-run; the budget, deduplication, metrics and
  Events; the image, the Helm chart and the kind test; a measurement on a Raspberry Pi 4 and a
  token comparison with k8sgpt.
- **Before the first release:** CI on GitHub's runners, a test against a hosted provider, and a
  published image and chart.
- **v0.2:** longer explanations and history in a custom resource, and a namespaced install mode
  (v0.1 installs cluster-wide only).

## Documentation

- [Documentation front page](docs/index.md)
- [Architecture](docs/explanation/architecture.md) and the [map of the code](ARCHITECTURE.md)
- [Detection and redaction rules](docs/reference/rules.md)
- [Data sent to the provider](docs/reference/data-sent-to-the-provider.md)
- [Permissions](docs/reference/permissions.md)
- [Command line and log format](docs/reference/command-line.md)
- [Helm chart](docs/reference/helm-chart.md)
- [Install kausa with Helm](docs/how-to/install-with-helm.md)
- [Try kausa against a cluster with a local model](docs/how-to/try-with-a-local-model.md)
- [Set up a development environment and run the checks](docs/how-to/develop.md)
- [Add a detection or redaction rule](docs/how-to/add-a-rule.md)
- [Competitive landscape](docs/research/2026-10-23-competitive-landscape.md),
  [tokens compared with k8sgpt](docs/research/2026-10-26-tokens-compared-with-k8sgpt.md) and
  [memory and CPU on a Raspberry Pi 4](docs/research/2026-10-26-resource-use-on-a-raspberry-pi.md)
- [Security policy](SECURITY.md)
- [Changelog](CHANGELOG.md)
- [Instructions for coding agents](AGENTS.md)

## Contributing

Issues, pull requests and reviews happen on GitHub, at <https://github.com/fedeam/kausa>. Pull
requests are squash-merged there, so the pull request title, with GitHub's ` (#<number>)` after
it, becomes the commit header on `main` and follows Conventional Commits with a scope
(`type(scope): subject`). See [Contributing to kausa](CONTRIBUTING.md); notable changes go in the
[Changelog](CHANGELOG.md). Everyone taking part follows the [Contributor Covenant Code of
Conduct](CODE_OF_CONDUCT.md).

## Maintainer

kausa is made by [FedeAM](https://fedeam.com), who writes about it and other projects at
[fedeam.com/proyectos](https://fedeam.com/proyectos/) (in Spanish).

## License

Licensed under either of

 * Apache License, Version 2.0
   ([LICENSE-APACHE](LICENSE-APACHE) or <http://www.apache.org/licenses/LICENSE-2.0>)
 * MIT license
   ([LICENSE-MIT](LICENSE-MIT) or <http://opensource.org/licenses/MIT>)

at your option. The same two texts are in [`LICENSES/`](LICENSES/), the layout of the REUSE
specification, for tools that look for it.

### Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in the work by you, as defined in the Apache-2.0 license, shall be
dual licensed as above, without any additional terms or conditions.
