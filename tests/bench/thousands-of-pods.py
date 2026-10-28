"""Writes the objects of the scale measurement
(docs/research/2026-10-28-thousands-of-pods.md) to stdout: a fake node with no kubelet, and
namespaces of Deployments whose Pods are bound to it, so that the API server holds thousands of
Pods and ReplicaSets and no container runs. Apply it to a throwaway kind cluster only: Pods
bound to a node without a kubelet cannot be deleted.

Usage: python3 thousands-of-pods.py [namespaces] [deployments per namespace] [replicas]
"""
import sys

args = sys.argv[1:] + ["20", "50", "3"][len(sys.argv) - 1 :]
namespaces, deployments, replicas = (int(a) for a in args)

print("""apiVersion: v1
kind: Node
metadata:
  name: fake-1
  labels: {kubernetes.io/hostname: fake-1}
spec:
  taints: [{key: fake, value: "true", effect: NoSchedule}]""")
for n in range(namespaces):
    print(f"---\napiVersion: v1\nkind: Namespace\nmetadata: {{name: scale-{n:02d}}}")
    for d in range(deployments):
        print(f"""---
apiVersion: apps/v1
kind: Deployment
metadata: {{name: app-{d:03d}, namespace: scale-{n:02d}}}
spec:
  replicas: {replicas}
  selector: {{matchLabels: {{app: app-{d:03d}}}}}
  template:
    metadata: {{labels: {{app: app-{d:03d}}}}}
    spec:
      nodeName: fake-1
      tolerations: [{{operator: Exists}}]
      containers: [{{name: app, image: registry.k8s.io/pause:3.10}}]""")
