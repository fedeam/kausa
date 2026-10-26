# Try kausa against a cluster with a local model

Run `kausa run` from your machine against a cluster you can read, with a model served by Ollama on
the same machine, so that nothing leaves it; and, if you want, through a `kubectl proxy` that only
lets kausa's own reads through.

Status as of 2026-10-26: what the second implementation weekend can do. kausa logs each incident,
the redacted request and the model's explanation; it writes no Event yet. Tested on 2026-10-26
with Ollama 0.9.2 and the model `qwen3:0.6b` against Kubernetes v1.36.5 (k3s).

## Before you start

- A cluster you may read, and `kubectl` working against it. kausa reads Pods, Warning events, the
  metadata of ReplicaSets and Jobs, and the logs of failed containers
  ([Permissions](../reference/permissions.md)); it writes nothing yet.
- [Ollama](https://ollama.com) installed, and a model pulled (`ollama pull qwen3:0.6b` is about
  500 MB; a larger model explains better).
- kausa built from source ([Set up a development environment and run the
  checks](develop.md)): `cargo build --locked --release`.

## Steps

1. **Start the model server** and check its OpenAI-compatible endpoint, which lists the pulled
   models:

   ```sh
   OLLAMA_HOST=127.0.0.1:11434 ollama serve &
   curl -s http://127.0.0.1:11434/v1/models
   ```

2. **Optionally, put a read-only proxy between kausa and the cluster.** `kubectl proxy` uses your
   own credentials, so kausa gets none; the filters let through only the requests kausa makes and
   refuse every write. Replace `<context>` with the context of the cluster you mean:

   ```sh
   kubectl --context=<context> proxy --address=127.0.0.1 --port=18001 \
     --reject-methods='^(POST|PUT|PATCH|DELETE)$' \
     --accept-paths='^/version$|^/api/v1/pods$|^/apis/events\.k8s\.io/v1/events$|^/apis/apps/v1/namespaces/[^/]+/replicasets/[^/]+$|^/apis/batch/v1/namespaces/[^/]+/jobs/[^/]+$|^/api/v1/namespaces/[^/]+/pods/[^/]+/log$' &
   ```

   Then a kubeconfig with no credentials that points at it:

   ```sh
   cat > kubeconfig-proxy <<'EOF'
   apiVersion: v1
   kind: Config
   clusters: [{name: proxy, cluster: {server: "http://127.0.0.1:18001"}}]
   users: [{name: none, user: {}}]
   contexts: [{name: proxy, context: {cluster: proxy, user: none}}]
   current-context: proxy
   EOF
   export KUBECONFIG=$PWD/kubeconfig-proxy
   ```

   Without the proxy, kausa uses the current context of your kubeconfig: check
   `kubectl config current-context` first.

3. **See what would leave**, without sending anything:

   ```sh
   ./target/release/kausa run --dry-run
   ```

   Each `request` line holds the exact body kausa would send: placeholders instead of names,
   `<redacted:…>` markers instead of addresses and secrets, and the counts per redaction rule.

4. **Ask the local model:**

   ```sh
   ./target/release/kausa run --endpoint http://127.0.0.1:11434/v1 --model qwen3:0.6b --allow-http
   ```

   `--allow-http` is needed because the endpoint is plain HTTP on your machine. Each incident gets
   an `explanation` line with the real names mapped back, and the tokens the endpoint reported.

5. **Stop** kausa with Ctrl-C, then the proxy and the model server.

## Verify

- `started` has the server version; each failing workload gives one `incident` line once it has
  failed for a minute (a failed Job at once), then one `request` (dry-run) or `explanation`.
- With the proxy, run it with `-v=4` and look for `Filter rejecting`: kausa's requests should never
  be among them.

## If something fails

- **`cannot configure the Kubernetes client`** → no kubeconfig at `KUBECONFIG` or
  `~/.kube/config` → point `KUBECONFIG` at one.
- **`the endpoint uses plain HTTP`** → add `--allow-http`, only for an endpoint on your machine,
  in the cluster or in the same private network.
- **`explanation skipped` with `invalid answer`** → the model did not answer the expected JSON.
  Try `--no-json-schema` if the server does not support structured output, or a larger model.
- **`log read failed`** → the container's log is gone (the Pod was deleted, or it never ran);
  the explanation is built without it.
- **No incident at all** → nothing has been failing for a minute: kausa explains current
  failures only, not old events.
