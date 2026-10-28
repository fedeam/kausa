# 2026-10-28 — First release

## Request

Cut v0.1.0: publish the image and the chart, signed, as [0012. Release from a tag: a signed image
on GHCR and the chart on the GitHub release](../adr/0012-releases.md) describes, and check what
was published.

## What was done

- Pull request #31 set the version to 0.1.0 in `Cargo.toml` and the chart, and moved the
  changelog's entries under `[0.1.0] - 2026-10-28`. After its merge the maintainer tagged
  `v0.1.0` on `main` (an annotated tag) and pushed it.
- **The Release workflow passed on its first run:** both images built natively (amd64 and arm64),
  the multi-architecture index `ghcr.io/fedeam/kausa:0.1.0`
  (`sha256:4fc353048777a7cd7cd8015b0a98f6b8460f81498d8c08ec4cc65aabe4098073`), signed with
  cosign keyless and with SLSA build provenance; the GitHub release with the changelog's notes,
  `kausa-0.1.0.tgz`, `SHA256SUMS` and its cosign bundle.
- **Checked from outside, without credentials:** the image's manifest can be pulled anonymously
  and lists linux/amd64 and linux/arm64; `cosign verify` (v3.1.4) accepts the signature for the
  identity of `release.yml` at `refs/tags/v0.1.0`; `gh attestation verify` finds one SLSA v1
  provenance from that workflow; `cosign verify-blob` accepts the checksums' bundle and
  `sha256sum -c SHA256SUMS` matches the chart.
- **Installed from the release:** `helm install` of the chart's URL in a throwaway kind cluster
  (Kubernetes v1.37), in dry-run: the Pod pulled the image by the index's digest, became ready,
  and logged `started` with version 0.1.0. The cluster was deleted, and the maintainer's
  kubeconfig was left unchanged (checked by its checksum before and after).
- The README, [Install kausa with Helm](../how-to/install-with-helm.md), the [Helm
  chart](../reference/helm-chart.md) reference and the front page now install the released
  chart; building your own image is the alternative.

## Notes

- The package's page on GitHub answered 404 before the tag: the package did not exist until the
  first push to GHCR.
- OpenSSF Scorecard's checks `Signed-Releases` and `Packaging` had no release or packaging run to
  look at; both count from the next scan.

## Later the same day: provenance as a release asset

- OpenSSF Scorecard scored 7.6. Its scan ran before the release, so Signed-Releases and Packaging
  had nothing to look at yet. In Scorecard v5.6.0, Packaging needs a successful run of a workflow
  that pushes an image, which `release.yml` now has. Signed-Releases gives 8 to a release with a
  signature asset (`.sigstore.json` counts) and 10 to one with a provenance asset, which it
  recognises by the `.intoto.jsonl` extension only.
- `release.yml` now attests the chart and `SHA256SUMS` too, and attaches the bundle as
  `kausa-X.Y.Z.intoto.jsonl`.
- For v0.1.0, the image's provenance, made by the release run, was downloaded with
  `gh attestation download`, checked offline with `gh attestation verify --bundle`, and attached
  to the release as `kausa-0.1.0-image.intoto.jsonl`. The release's other assets are unchanged.
- **What Scorecard can't get from one maintainer:** Code-Review and Contributors need other
  people. Branch-Protection stays at 8: to go higher it needs two required approvals, which one
  maintainer can't give. Maintained counts from 90 days after the repository was created. SAST and
  CI-Tests rise as the commits made before CodeQL and CI leave the window they look at.
