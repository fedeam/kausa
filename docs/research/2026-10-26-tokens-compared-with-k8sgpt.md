# Tokens per incident compared with k8sgpt

kausa and k8sgpt explained the same five seeded failures with the same local model, and a proxy in
front of the model counted the calls and the tokens. The roadmap's fourth weekend asks for this
comparison ([0001. Build a small, read-only explainer that writes one redacted explanation per
incident](../adr/0001-scope-and-positioning.md)).

Measured on 2026-10-26. One run per tool, with one small model: read the numbers as an order of
magnitude, not a benchmark.

## Result

| | kausa | k8sgpt | k8sgpt `--anonymize` |
|---|---|---|---|
| Model calls | 5 | 12 | 12 |
| Input tokens | 2,183 | 1,301 | 1,401 |
| Output tokens | 765 | 3,827 | 4,035 |
| Total tokens | 2,948 | 5,128 | 5,436 |
| Total per failing workload (5) | 590 | 1,026 | 1,087 |

- **Fewer calls.** kausa groups the failing Pods of one workload into one incident, so it made one
  call per workload. k8sgpt made one per failing object: the four Deployments, the Job, and each
  failing Pod (two crash-looping replicas and the Job's two Pods).
- **More input per call.** A kausa request averaged 437 input tokens against k8sgpt's 108: kausa
  sends the container states, the incident's Warning events and, for crash loops, out-of-memory
  kills and failed Jobs, the failed container's last log lines. k8sgpt sends its analyzer's error
  text. Neither number says which explanation is better.
- **Much less output.** kausa asks for a JSON answer with a fixed schema; k8sgpt asks for free
  text. With this model, every k8sgpt explanation began with the model's reasoning
  (`<think> Okay, let's see. …`), which k8sgpt kept in its output; kausa's answers had none.
  Whether the structured output is what stops the reasoning here is a hypothesis.
- **What the tools read.** k8sgpt's default analyzers asked for the namespace's ConfigMaps (once
  in each run); the proxy in front of the API server refused them, and k8sgpt reported
  `Forbidden (get configmaps)` and went on. It did not ask for Secrets. kausa asked only for what [Permissions](../reference/permissions.md)
  lists, and its proxy refused nothing.

## Setup

- kausa 0.1.0-dev at commit `0f807de`, release build, run on the maintainer's machine with
  `--endpoint`, `--no-events` and the default budget, until it had explained the five incidents.
- k8sgpt 0.4.41 (Homebrew), `k8sgpt analyze --explain --backend ollama --namespace <ns>
  --no-cache --output json`, with its default analyzers, once without and once with
  `--anonymize`.
- Ollama on the same machine (Apple silicon, macOS), the model `qwen3:0.6b`, one request at a
  time (`OLLAMA_NUM_PARALLEL=1`).
- A Kubernetes v1.36 (k3s) cluster, reached through `kubectl proxy`: for kausa, the read-only
  filter of [Try kausa against a cluster with a local
  model](../how-to/try-with-a-local-model.md); for k8sgpt, every `GET` except Secrets,
  ConfigMaps, `exec` and `attach`.
- The failures: [`tests/bench/seeded-failures.yaml`](../../tests/bench/seeded-failures.yaml), a
  crash loop (two replicas), an image that cannot be pulled, a Pod that cannot be scheduled, an
  out-of-memory kill and a failed Job, all in one namespace, with nothing else failing in it.
- The counts: [`tests/bench/token_proxy.py`](../../tests/bench/token_proxy.py), between each tool
  and Ollama, records the token counts Ollama reports for each call (`usage` on the
  OpenAI-compatible API kausa uses, `prompt_eval_count` and `eval_count` on the native API k8sgpt
  uses). Ollama reuses the cached prefix of the previous prompt and then counts only the new
  tokens, so before each call the proxy sends a one-token request with an unrelated prompt; that
  this clears the reuse completely is a hypothesis, and a few tokens of the chat template may still
  be counted short.

## Limits

- One model, and a very small one. A larger model writes different amounts, and a hosted one
  counts tokens with its own tokenizer.
- One run each: the output varies from run to run.
- k8sgpt is a command run once; kausa is a controller that waits for failures to settle and then
  explains each incident once (and again from its cache, with no call, if it repeats). A
  `k8sgpt analyze` run every few minutes pays its calls again each time unless its cache is on;
  this comparison does not measure that.
- Memory is not compared: k8sgpt here is a short-lived command, and kausa a long-running process
  (its resident memory on macOS was 21 MiB at the end of the run; the measurement that counts is
  on a Raspberry Pi 4).

## Reproduce it

On a test cluster, with Ollama serving `qwen3:0.6b` on its default port:

```sh
kubectl apply -f tests/bench/seeded-failures.yaml
python3 tests/bench/token_proxy.py 11500 tokens-kausa.jsonl qwen3:0.6b &
cargo run --locked --release -- run --endpoint http://127.0.0.1:11500/v1 --model qwen3:0.6b \
  --allow-http --no-events
# Once kausa has logged five explanations: stop it and the proxy, and start a new proxy.
python3 tests/bench/token_proxy.py 11500 tokens-k8sgpt.jsonl qwen3:0.6b &
k8sgpt auth add --backend ollama --model qwen3:0.6b --baseurl http://127.0.0.1:11500
k8sgpt analyze --explain --backend ollama --namespace kausa-test --no-cache --output json
kubectl delete namespace kausa-test
```

Each line of the proxy's log is one model call with its token counts. kausa runs against the
cluster of the kubeconfig's current context and sees every namespace: check that nothing else is
failing, or compare only the `kausa-test` incidents of its log.
