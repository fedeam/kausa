# 0012. Release from a tag: a signed image on GHCR and the chart on the GitHub release

- Status: accepted
- Date: 2026-10-28
- Builds on: [0010. Ship a static binary on distroless, with a Helm chart, built in CI and not
  published yet](0010-packaging.md)

## Context

v0.1 is feature-complete and the repository is public, but nothing could be installed without
building the image. OpenSSF Scorecard's Signed-Releases and Packaging checks and the Best Practices
criteria on version tags all ask for real releases. FedeAM decided on 2026-10-28 to cut v0.1.0.

## Decision

- **A release starts with a tag** `vX.Y.Z` on `main`. `.github/workflows/release.yml` refuses a tag
  that does not match the version of `Cargo.toml` and the chart's `version` and `appVersion`.
- **The image** goes to `ghcr.io/fedeam/kausa:X.Y.Z`, a multi-architecture index of two native
  builds (linux/amd64 and linux/arm64), with a tag per architecture (`X.Y.Z-amd64`, `X.Y.Z-arm64`)
  for the parts. No `latest`: an install names its version.
- **Signatures without keys:** the image is signed with cosign keyless (the workflow's OIDC
  identity, recorded in Sigstore's transparency log), and GitHub attests its SLSA build provenance.
  Nothing secret is kept, so `SECURITY.md` ("No repository secrets are configured") stays true.
- **The chart** is attached to the GitHub release as `kausa-X.Y.Z.tgz`, with `SHA256SUMS` and its
  cosign signature bundle; `helm install` takes its URL. It is not pushed to an OCI registry yet:
  one package to keep public on GHCR is enough for now.
- **The notes** are the version's section of `CHANGELOG.md`, plus how to install and verify.
- **Only GitHub's own actions** run in the workflow, pinned by SHA; cosign and helm are downloaded
  and checked against their SHA-256, as the repository's Actions policy and its other workflows
  require.
- crates.io stays for later ([Publish the repository on
  GitHub](../how-to/publish-the-repository.md), step 13).

## Alternatives

- **Release by hand** from the maintainer's machine: nothing ties the artifacts to the code that CI
  checked, and a key would have to be kept.
- **GoReleaser or Docker's actions:** third-party actions, which the Actions policy keeps out.
- **The chart on GHCR as OCI:** a second package whose visibility and link to the repository would
  have to be managed; it can come later.

## Consequences

- Users install a published, signed image, and can verify it before running it (the release notes
  show the `cosign verify` and `gh attestation verify` commands).
- The first push creates the GHCR package; whether it inherits the repository's public visibility
  is checked after the first release, and set to public by hand if not.
- cosign's version and checksum are bumped by hand, like the other pinned tools.
