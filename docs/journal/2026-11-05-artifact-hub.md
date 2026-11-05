# 2026-11-05 — Artifact Hub

## Request

List the chart on Artifact Hub, as [Maintain the repository](../how-to/maintain.md) ("List the
chart on Artifact Hub") and [0023](../adr/0023-oci-chart-and-sbom.md) describe.

## What was done

- The maintainer registered the repository once in Artifact Hub's control panel, signed in with
  the `fedeam` GitHub account: kind Helm charts, name `kausa`, URL
  `oci://ghcr.io/fedeam/charts/kausa`, no credentials, neither disabled nor with its security
  scanning disabled.
- `charts/artifacthub-repo.yml` holds the repository's ID, public by design, and no owners. The
  `artifacthub` job of the next release pushes it to `ghcr.io/fedeam/charts/kausa:artifacthub.io`;
  until then the job skips, as it did for v0.6.0.

## Next steps

- After the next release: check that Artifact Hub processed the repository with no errors, that
  it shows the publisher as verified and the chart as signed, and record it here or in that day's
  journal.
