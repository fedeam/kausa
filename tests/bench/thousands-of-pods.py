"""Writes the objects of the scale measurement
(docs/research/2026-10-28-thousands-of-pods.md) to stdout: a fake node with no kubelet, and
namespaces of Deployments whose Pods are bound to it, so that the API server holds thousands of
Pods and ReplicaSets and no container runs. Apply it to a throwaway kind cluster only: Pods
bound to a node without a kubelet cannot be deleted.

Two options, added on 2026-10-29 for the measurement of
docs/research/2026-10-29-richer-context-measured.md (0017's larger records):

- --probes gives each container requests and limits and an HTTP liveness, readiness and startup
  probe whose paths are each as long as kausa keeps (128 bytes).
- --unschedulable writes, in namespaces named unsched-NN instead of scale-NN, Deployments whose
  Pods are bound to no node and that the scheduler cannot place, with the scheduling record at
  its caps (src/record.rs): 8 node selector keys, 8 required node affinity requirements, 4
  required Pod affinity and 4 anti-affinity terms and 8 tolerations, each key 128 bytes long. No
  node has the labels, so the Pods stay Pending. They can be deleted, with their namespaces.

Usage: python3 thousands-of-pods.py [--probes] [--unschedulable]
       [namespaces] [deployments per namespace] [replicas]
"""
import sys

flags = {a for a in sys.argv[1:] if a.startswith("--")}
unknown = flags - {"--probes", "--unschedulable"}
if unknown:
    sys.exit(f"unknown option: {sorted(unknown)[0]}")
positional = [a for a in sys.argv[1:] if not a.startswith("--")]
args = positional + ["20", "50", "3"][len(positional) :]
namespaces, deployments, replicas = (int(a) for a in args)
probes, unschedulable = "--probes" in flags, "--unschedulable" in flags


def key(kind, index):
    """A qualified name of exactly 128 bytes, MAX_KEY_BYTES: a 64-byte DNS subdomain prefix, a
    slash and a 63-byte name. Invented, and on no node."""
    prefix = f"bench-{kind}-{index}".ljust(56, "y") + ".example"
    name = f"{kind}-{index}-".ljust(63, "x")
    return f"{prefix}/{name}"


def path(kind):
    """An HTTP probe path of exactly 128 bytes, MAX_PROBE_PATH_BYTES."""
    return f"/bench-{kind}-probe-".ljust(128, "p")


def probe(kind):
    return f"{kind}Probe: {{httpGet: {{path: {path(kind)}, port: 8080}}}}"


def container():
    fields = ["name: app", "image: registry.k8s.io/pause:3.10"]
    if probes:
        fields += [
            "resources: {requests: {cpu: 5m, memory: 8Mi}, limits: {cpu: 50m, memory: 32Mi}}",
        ] + [probe(kind) for kind in ("liveness", "readiness", "startup")]
    return "{" + ", ".join(fields) + "}"


def scheduling():
    """The Pod spec's scheduling fields at the caps of src/record.rs."""
    selector = ", ".join(f'"{key("selector", i)}": "v"' for i in range(8))
    requirements = ", ".join(
        f'{{key: "{key("affinity", i)}", operator: In, values: ["v"]}}' for i in range(8)
    )
    no_pod = "labelSelector: {matchLabels: {app: bench-no-such-pod}}"
    affinity = ", ".join(f'{{{no_pod}, topologyKey: "{key("topology", i)}"}}' for i in range(4))
    anti = ", ".join(f'{{{no_pod}, topologyKey: "{key("anti", i)}"}}' for i in range(4))
    tolerations = ", ".join(
        f'{{key: "{key("toleration", i)}", operator: Exists, effect: NoSchedule}}'
        for i in range(8)
    )
    return f"""      nodeSelector: {{{selector}}}
      affinity:
        nodeAffinity:
          requiredDuringSchedulingIgnoredDuringExecution:
            nodeSelectorTerms: [{{matchExpressions: [{requirements}]}}]
        podAffinity:
          requiredDuringSchedulingIgnoredDuringExecution: [{affinity}]
        podAntiAffinity:
          requiredDuringSchedulingIgnoredDuringExecution: [{anti}]
      tolerations: [{tolerations}]"""


if unschedulable:
    placement, prefix = scheduling(), "unsched"
else:
    placement, prefix = "      nodeName: fake-1\n      tolerations: [{operator: Exists}]", "scale"
    print("""apiVersion: v1
kind: Node
metadata:
  name: fake-1
  labels: {kubernetes.io/hostname: fake-1}
spec:
  taints: [{key: fake, value: "true", effect: NoSchedule}]""")
for n in range(namespaces):
    print(f"---\napiVersion: v1\nkind: Namespace\nmetadata: {{name: {prefix}-{n:02d}}}")
    for d in range(deployments):
        print(f"""---
apiVersion: apps/v1
kind: Deployment
metadata: {{name: app-{d:03d}, namespace: {prefix}-{n:02d}}}
spec:
  replicas: {replicas}
  selector: {{matchLabels: {{app: app-{d:03d}}}}}
  template:
    metadata: {{labels: {{app: app-{d:03d}}}}}
    spec:
{placement}
      containers: [{container()}]""")
