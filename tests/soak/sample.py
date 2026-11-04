#!/usr/bin/env python3
"""The soak's sampler (docs/how-to/run-the-soak.md): reads, every --interval seconds until --days
have passed or it is stopped, kausa's two Pods of the release kausa-soak in the cluster of the kube
context --context, and appends one JSON line per Pod to <out>/samples-<UTC date>.jsonl:

- ts: the sampler's clock, Unix seconds;
- pod: the Pod's name; node: the index of its node in the sorted list of the cluster's nodes, never
  the node's name;
- holder: whether the Pod holds the Lease kausa-soak-leader;
- ready, restarts, last_terminated_reason: the Pod's status, container kausa;
- ws_bytes, rss_bytes, usage_bytes, cpu_ns: container kausa in the kubelet's summary API;
- top_memory (bytes), top_cpu (millicores): metrics-server, when the cluster has it;
- max_usage_bytes, oom_events: the kubelet's cAdvisor metrics of container kausa
  (container_memory_max_usage_bytes and container_oom_events_total), read on the first sample and
  then once every 600 seconds, null in between;
- kausa: every kausa_ series of the Pod's metrics endpoint, as {"series": value};
- errors: the reads that failed for this line, among pods, lease, summary, top, cadvisor and
  metrics. A failed read gives null, never 0.

On the first sample and then once every 600 seconds, it also appends to
<out>/cluster-<UTC date>.jsonl the number of Pods, of Warning events and of nodes of the cluster:
counts only. It only reads, through the API server, and writes no node name, no server address and
nothing about a namespace other than kausa-soak. The output holds Pod names and the cluster's
counts: keep it out of the repository.

The context is never the current one: it must be named, it must be a context of the kubeconfig,
and every call names it. Needs kubectl and python3 (stdlib only); run it with python3 -I.
"""

import argparse
import json
import math
import os
import re
import signal
import subprocess
import sys
import time

NAMESPACE = "kausa-soak"
SELECTOR = "app.kubernetes.io/name=kausa,app.kubernetes.io/instance=kausa-soak"
LEASE = "kausa-soak-leader"
CONTAINER = "kausa"
METRICS_PORT = 9090
# The cAdvisor read and the cluster counts are heavier on the kubelet and the API server.
HEAVY_SECONDS = 600
CALL_TIMEOUT_SECONDS = 60
CADVISOR_SERIES = {
    "container_memory_max_usage_bytes": "max_usage_bytes",
    "container_oom_events_total": "oom_events",
}


def kubectl(ctx, *args):
    """The standard output of a call in the context CTX with ARGS, or None when the call fails. Its
    standard error is dropped: it may name a node or the server."""
    command = ["kubectl", "--context", ctx] + list(args)
    try:
        done = subprocess.run(
            command, capture_output=True, timeout=CALL_TIMEOUT_SECONDS, check=False
        )
    except (OSError, subprocess.TimeoutExpired):
        return None
    if done.returncode != 0:
        return None
    return done.stdout.decode("utf-8", errors="replace")


def read_json(ctx, *args):
    text = kubectl(ctx, *args)
    if text is None:
        return None
    try:
        return json.loads(text)
    except ValueError:
        return None


def number(value):
    """A sample value as JSON keeps it: an int when whole, null when not finite."""
    if not math.isfinite(value):
        return None
    if value == int(value) and abs(value) < 2**53:
        return int(value)
    return value


def parse_exposition(text):
    """(series, labels, value) for each sample line of a Prometheus text exposition."""
    for line in text.splitlines():
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        brace = line.find("{")
        space = line.find(" ")
        if brace != -1 and (space == -1 or brace < space):
            end, quoted, escaped = brace + 1, False, False
            while end < len(line):
                c = line[end]
                if escaped:
                    escaped = False
                elif c == "\\":
                    escaped = True
                elif c == '"':
                    quoted = not quoted
                elif c == "}" and not quoted:
                    break
                end += 1
            series, rest = line[: end + 1], line[end + 1 :].split()
            labels = dict(re.findall(r'(\w+)="((?:[^"\\]|\\.)*)"', line[brace + 1 : end]))
        else:
            words = line.split()
            series, rest, labels = words[0], words[1:], {}
        if not rest:
            continue
        try:
            value = float(rest[0])
        except ValueError:
            continue
        yield series, labels, value


def quantity(text, cpu):
    """A Kubernetes quantity as bytes, or as millicores when CPU; None when unreadable."""
    if not isinstance(text, str):
        return None
    match = re.fullmatch(r"([0-9.]+)([a-zA-Z]*)", text)
    if not match:
        return None
    value, suffix = float(match.group(1)), match.group(2)
    if cpu:
        scale = {"": 1000, "m": 1, "u": 1e-3, "n": 1e-6}
    else:
        scale = {"": 1, "k": 1e3, "M": 1e6, "G": 1e9, "Ki": 2**10, "Mi": 2**20, "Gi": 2**30}
    if suffix not in scale:
        return None
    return number(value * scale[suffix])


def container_status(pod):
    for status in (pod.get("status") or {}).get("containerStatuses") or []:
        if status.get("name") == CONTAINER:
            return status
    return {}


def is_ready(pod):
    return any(
        c.get("type") == "Ready" and c.get("status") == "True"
        for c in (pod.get("status") or {}).get("conditions") or []
    )


class Sampler:
    def __init__(self, ctx, out):
        self.ctx = ctx
        self.out = out
        self.nodes = []
        self.last_heavy = None
        self.files = {}

    def write(self, kind, record):
        day = time.strftime("%Y-%m-%d", time.gmtime(record["ts"]))
        path = os.path.join(self.out, "%s-%s.jsonl" % (kind, day))
        handle = self.files.get(path)
        if handle is None:
            for old in list(self.files):
                if os.path.basename(old).startswith(kind + "-"):
                    self.files.pop(old).close()
            handle = self.files[path] = open(path, "a", encoding="utf-8")
        handle.write(json.dumps(record, separators=(",", ":"), sort_keys=True) + "\n")
        handle.flush()

    def close(self):
        for handle in self.files.values():
            handle.close()
        self.files = {}

    def read_nodes(self):
        text = kubectl(self.ctx, "get", "nodes", "-o", "name")
        if text is None:
            return None
        names = sorted(line.split("/", 1)[-1] for line in text.splitlines() if line.strip())
        self.nodes = names
        return len(names)

    def count(self, *args):
        text = kubectl(self.ctx, *args)
        if text is None:
            return None
        return sum(1 for line in text.splitlines() if line.strip())

    def cluster_counts(self, ts):
        nodes = self.read_nodes()
        pods = self.count("get", "pods", "-A", "-o", "name")
        warnings = self.count(
            "get", "events", "-A", "--field-selector", "type=Warning", "-o", "name"
        )
        self.write("cluster", {"ts": ts, "pods": pods, "warning_events": warnings, "nodes": nodes})

    def summary(self, node):
        """{pod name: container kausa's stats} of the node's kubelet summary, or None."""
        data = read_json(self.ctx, "get", "--raw", "/api/v1/nodes/%s/proxy/stats/summary" % node)
        if not isinstance(data, dict):
            return None
        stats = {}
        for pod in data.get("pods") or []:
            ref = pod.get("podRef") or {}
            if ref.get("namespace") != NAMESPACE:
                continue
            for container in pod.get("containers") or []:
                if container.get("name") == CONTAINER:
                    stats[ref.get("name")] = container
        return stats

    def cadvisor(self, node):
        """{pod name: {field: value}} of container kausa in the node's cAdvisor metrics, or None."""
        text = kubectl(self.ctx, "get", "--raw", "/api/v1/nodes/%s/proxy/metrics/cadvisor" % node)
        if text is None:
            return None
        found = {}
        for series, labels, value in parse_exposition(text):
            field = CADVISOR_SERIES.get(series.split("{", 1)[0])
            if field is None:
                continue
            if labels.get("namespace") != NAMESPACE or labels.get("container") != CONTAINER:
                continue
            fields = found.setdefault(labels.get("pod"), {})
            fields[field] = number(max(value, fields.get(field) or 0))
        return found

    def top(self, pod):
        data = read_json(
            self.ctx,
            "get",
            "--raw",
            "/apis/metrics.k8s.io/v1beta1/namespaces/%s/pods/%s" % (NAMESPACE, pod),
        )
        if not isinstance(data, dict):
            return None
        for container in data.get("containers") or []:
            if container.get("name") == CONTAINER:
                usage = container.get("usage") or {}
                return quantity(usage.get("memory"), False), quantity(usage.get("cpu"), True)
        return None

    def kausa_metrics(self, pod):
        text = kubectl(
            self.ctx,
            "get",
            "--raw",
            "/api/v1/namespaces/%s/pods/%s:%d/proxy/metrics" % (NAMESPACE, pod, METRICS_PORT),
        )
        if text is None:
            return None
        return {
            series: number(value)
            for series, _, value in parse_exposition(text)
            if series.startswith("kausa_")
        }

    def sample(self):
        ts = number(round(time.time(), 3))
        heavy = self.last_heavy is None or ts - self.last_heavy >= HEAVY_SECONDS
        if heavy:
            self.last_heavy = ts
            self.cluster_counts(ts)
        pods = read_json(self.ctx, "get", "pods", "-n", NAMESPACE, "-l", SELECTOR, "-o", "json")
        if not isinstance(pods, dict):
            self.write("samples", {"ts": ts, "pod": None, "errors": ["pods"]})
            return
        lease = read_json(self.ctx, "get", "lease", LEASE, "-n", NAMESPACE, "-o", "json")
        holder = (
            ((lease or {}).get("spec") or {}).get("holderIdentity")
            if isinstance(lease, dict)
            else None
        )
        items = sorted(
            pods.get("items") or [], key=lambda p: (p.get("metadata") or {}).get("name") or ""
        )
        node_of = {}
        for pod in items:
            node = (pod.get("spec") or {}).get("nodeName")
            if node:
                node_of[(pod.get("metadata") or {}).get("name")] = node
        if any(node not in self.nodes for node in node_of.values()):
            self.read_nodes()
        summaries, cadvisors = {}, {}
        for node in sorted(set(node_of.values())):
            summaries[node] = self.summary(node)
            if heavy:
                cadvisors[node] = self.cadvisor(node)
        for pod in items:
            name = (pod.get("metadata") or {}).get("name")
            node = node_of.get(name)
            status = container_status(pod)
            errors = []
            if not isinstance(lease, dict):
                errors.append("lease")
            terminated = (status.get("lastState") or {}).get("terminated") or {}
            record = {
                "ts": ts,
                "pod": name,
                "node": self.nodes.index(node) if node in self.nodes else None,
                "holder": (holder == name) if isinstance(lease, dict) else None,
                "ready": is_ready(pod),
                "restarts": status.get("restartCount"),
                "last_terminated_reason": terminated.get("reason"),
            }
            stats = summaries.get(node) if node else None
            container = (stats or {}).get(name)
            if container is None:
                errors.append("summary")
                container = {}
            memory, cpu = container.get("memory") or {}, container.get("cpu") or {}
            record["ws_bytes"] = memory.get("workingSetBytes")
            record["rss_bytes"] = memory.get("rssBytes")
            record["usage_bytes"] = memory.get("usageBytes")
            record["cpu_ns"] = cpu.get("usageCoreNanoSeconds")
            top = self.top(name)
            if top is None:
                errors.append("top")
                top = (None, None)
            record["top_memory"], record["top_cpu"] = top
            record["max_usage_bytes"] = record["oom_events"] = None
            if heavy:
                found = cadvisors.get(node) if node else None
                if found is None:
                    errors.append("cadvisor")
                else:
                    record["max_usage_bytes"] = (found.get(name) or {}).get("max_usage_bytes")
                    record["oom_events"] = (found.get(name) or {}).get("oom_events")
            record["kausa"] = self.kausa_metrics(name)
            if record["kausa"] is None:
                errors.append("metrics")
            record["errors"] = errors
            self.write("samples", record)


def known_context(ctx):
    """Whether CTX is a whole context name of the kubeconfig: a local read, no cluster call."""
    try:
        done = subprocess.run(
            ["kubectl", "config", "get-contexts", "-o", "name"],
            capture_output=True,
            timeout=CALL_TIMEOUT_SECONDS,
            check=False,
        )
    except (OSError, subprocess.TimeoutExpired):
        return False
    names = done.stdout.decode("utf-8", errors="replace").splitlines()
    return done.returncode == 0 and ctx in names


def main():
    parser = argparse.ArgumentParser(
        prog="tests/soak/sample.py",
        description="Sample kausa's two Pods of the soak (docs/how-to/run-the-soak.md).",
    )
    parser.add_argument("--context", required=True, help="the kube context of the soak's cluster")
    parser.add_argument("--out", required=True, help="the output directory, outside the repository")
    parser.add_argument(
        "--interval", type=int, default=30, help="seconds between samples (default 30)"
    )
    parser.add_argument("--days", type=float, default=7, help="days to sample (default 7)")
    args = parser.parse_args()
    if not args.context or args.context.startswith("-"):
        parser.error("--context must name a context of the kubeconfig")
    if args.interval < 1:
        parser.error("--interval must be at least 1")
    if not (args.days > 0 and math.isfinite(args.days)):
        parser.error("--days must be above 0")
    if not known_context(args.context):
        print("sample.py: no context '%s' in the kubeconfig" % args.context, file=sys.stderr)
        return 1

    def stop(signum, frame):
        raise SystemExit(0)

    signal.signal(signal.SIGTERM, stop)
    signal.signal(signal.SIGINT, stop)
    os.makedirs(args.out, exist_ok=True)
    sampler = Sampler(args.context, args.out)
    start = time.time()
    deadline = start + args.days * 86400
    print(
        "sample.py: every %d s until %s"
        % (args.interval, time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(deadline))),
        file=sys.stderr,
    )
    try:
        next_at = start
        while True:
            sampler.sample()
            next_at += args.interval
            now = time.time()
            # After a slow sample, the next one starts at once, and the schedule moves with it.
            next_at = max(next_at, now)
            if next_at >= deadline:
                break
            time.sleep(next_at - now)
    finally:
        sampler.close()
    return 0


if __name__ == "__main__":
    sys.exit(main())
