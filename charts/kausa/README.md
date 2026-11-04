# kausa

A tiny Kubernetes controller that explains failing workloads with an LLM: one redacted, budgeted
explanation per incident, written as an Event on the object itself. It sends only allowlisted,
redacted context to one OpenAI-compatible endpoint you choose. Status: pre-alpha.

`rbac.scope` has no default: set it on every install and upgrade, to `cluster`, or to `namespaced`
with `rbac.namespaces`.

## Install, in dry-run first

```sh
helm install kausa oci://ghcr.io/fedeam/charts/kausa --version <version> \
  -n kausa --create-namespace --set rbac.scope=cluster --set dryRun=true
kubectl -n kausa logs deploy/kausa -f
```

In dry-run kausa logs the redacted request of each incident and sends nothing. To point it at a
model, and for every value: [Install kausa with Helm](https://github.com/fedeam/kausa/blob/main/docs/how-to/install-with-helm.md)
and [Helm chart](https://github.com/fedeam/kausa/blob/main/docs/reference/helm-chart.md).

## Verify

Releases are signed with cosign keyless by the release workflow; there is no key to fetch. The
chart and the image carry SLSA build provenance, and each image a CycloneDX SBOM:

- certificate identity: `https://github.com/fedeam/kausa/.github/workflows/release.yml@refs/tags/v<version>`
- issuer: `https://token.actions.githubusercontent.com`

The [release notes](https://github.com/fedeam/kausa/releases) give every command, by digest.
Security policy: [SECURITY.md](https://github.com/fedeam/kausa/blob/main/SECURITY.md).
