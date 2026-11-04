# 0023. Publish the chart as a signed OCI artifact at `oci://ghcr.io/fedeam/charts/kausa`, list it on Artifact Hub, and attest a CycloneDX SBOM of an auditable binary to each image

- Status: accepted
- Date: 2026-11-03
- Amends: [0012](0012-releases.md) (the chart also goes to GHCR: `0012:28-29` and the alternative at
  `:42-43`), [0010](0010-packaging.md) (the build stage runs `cargo auditable`) and
  [0003](0003-ci-in-one-pinned-container-build.md) (one more pinned tool and two Debian packages, in
  the build stage); each stays `accepted` and gets an `Update` paragraph when this ADR is accepted
- Keeps: [0001](0001-scope-and-positioning.md), [0008](0008-data-leaving-the-cluster.md),
  [0009](0009-least-privilege-rbac.md), [0013](0013-namespaced-install-mode.md) and
  [0019](0019-high-availability.md): nothing changes at run time. Related: the v0.6 plan's theme 3
  (its upgrade test installs the previous release from the GitHub release `.tgz`, which stays)

## Context

Status as of 2026-11-03 (v0.5.1). `release.yml` has two jobs. `image` checks the tag against
`Cargo.toml`, `version` and `appVersion` (`:48-66`), pushes one image per architecture and keeps
`docker image inspect`'s `RepoDigests` as `digest-<arch>.txt` (`:68-86`). `publish` holds
`contents: write`, `packages: write`, `id-token: write` and `attestations: write` for every step
(`:92-96`): it installs cosign v3.1.4 and helm v4.3.1 by SHA-256 (`:97-129`), creates, signs and
attests the index (`:131-156`), packages the chart, signs `SHA256SUMS` (`:158-167`), writes the
notes (`:168-197`), attests the `.tgz` and `SHA256SUMS` (`:199-214`) and creates the release
(`:216-225`). Users install from the release URL (`README.md:65-66`,
`docs/how-to/install-with-helm.md:41-48`). The chart has no maintainers, annotations or README
(`charts/kausa/Chart.yaml:1-13`). The binary is stripped (`Cargo.toml:46-50`, `Dockerfile:557-560`),
so nothing in the image tells a scanner which crates it holds. Rules: only GitHub's own actions,
pinned by SHA (`publish-the-repository.md:206-217`); tools pinned by SHA-256 from the official
source (`AGENTS.md:146-148`); "No repository secrets are configured" (`SECURITY.md:441-442`); three
days of cooldown (`dependabot.yml:7-10`); the Gitea job builds from the build stage's `FROM` on, with
the classic builder (`.gitea/workflows/ci.yml:137-141`, `Dockerfile:14-21`).

Checked on 2026-11-03, in source, documentation or by running it:

- **Helm v4.3.1 source:** `helm push` pushes to `<path>/<chart name>:<version>`
  (`pkg/pusher/ocipusher.go:88`) with the `.tgz` as the one layer (`pkg/registry/client.go:676`) and
  prints `Digest:` (`:754`); `org.opencontainers.image.created` is the file's modification time
  (`ocipusher.go:93-94`); `helm package` writes each file's modification time into the tar
  (`pkg/chart/v2/util/save.go:244-253`). **Rehearsed with Helm 3.16.1** on a local `registry:2`: one
  file pushed twice gave one digest; the layer digest was the file's SHA-256; repackaging the same
  directory gave other bytes; `docker buildx imagetools inspect --raw` returned the chart manifest.
- **v0.5.1 as published:** `0.5.1-amd64` is one Docker v2 manifest, not an index, signed through an
  OCI referrer. `gh attestation verify` (gh 2.103.3) with no login stops at "gh auth login".
- **actions/attest v4.2.2** (`1e69f48a…`) gives a CycloneDX SBOM the predicate type
  `https://cyclonedx.org/bom` (`src/sbom.ts:93`), caps it at 16 MB, and has `create-storage-record`
  (default `true`; its README limits storage records to organization-owned repositories).
  `actions/attest-sbom` is deprecated in its favour. `attest-build-provenance` v4.2.2
  (`release.yml:152`) wraps actions/attest v4.2.1 and has the same input.
- **Artifact Hub documentation:** an OCI repository is `oci://registry/namespace/chart-name`, its
  versions semver tags; the metadata file goes to tag `artifacthub.io` with `oras push` and two
  Artifact Hub media types; `repositoryID` alone gives Verified publisher, and `owners` (emails)
  only serves ownership claims; images come from an annotation or a default dry-run; the license
  "must be a valid SPDX identifier"; `signKey` is a PGP key. **Its source:** `validateURL` fetches
  nothing for an OCI Helm URL (`internal/repo/manager.go:822-847`); an OCI repository is reprocessed
  only when its versions change (`:579-585`); `HasCosignSignature` finds tag and referrer
  signatures, bundles included (`internal/oci/oci.go:154-224`); `helm.sh/images` from 2026-09-16.
- **oras v1.3.6** uses image-spec v1.0 when `--config` comes without `--artifact-type`
  (`cmd/oras/root/push.go:138-143`). **syft v1.55.0** writes CycloneDX 1.2 to 1.7 (default 1.7);
  its checksums file is signed keyless by its `release.yaml` on `main` (`install.sh:605`); v1.56.0
  (2026-11-02) is inside the cooldown. **Pins** equal GitHub's asset digests: syft `bb652495…`,
  oras `66fb0ad7…`, cargo-auditable v0.7.7 musl `7676443…`/`c324af5…`.

## Decision

### Names

| What | Reference | GHCR package |
|---|---|---|
| Image (unchanged) | `ghcr.io/fedeam/kausa:X.Y.Z`, `X.Y.Z-amd64`, `X.Y.Z-arm64` | `kausa` |
| Chart | `oci://ghcr.io/fedeam/charts/kausa`, tag `X.Y.Z` | `charts/kausa` |
| Artifact Hub metadata | `ghcr.io/fedeam/charts/kausa:artifacthub.io` | `charts/kausa` |

The chart keeps its name (a rename changes every release's full name and labels), so its tag is
`X.Y.Z` and it cannot share the image's package. No `latest`.

### The binary

The build stage installs `curl` and `xz-utils` beside `musl-tools` (`ca-certificates` is already in
the pinned rust image), downloads cargo-auditable v0.7.7 for its triple, checks it with
`sha256sum -c` against `ARG CARGO_AUDITABLE_SHA256_AMD64`/`_ARM64`, and runs `cargo auditable build
--locked --release --target "$triple"`. `readelf -S --wide … | grep -q '\.dep-v0'` then fails the
build if the crate list is missing. The `ci` target is unchanged: it never builds the shipped
binary, and `cargo audit` reads `Cargo.lock`. Every runtime build shares the stage: both
pull-request `image` jobs (`ci.yml:55-75`), Gitea and the release, so each pull request proves it.
The section holds crate names and versions that `Cargo.lock` already publishes; kausa never reads it.

### The release workflow: five jobs, each with only what it needs

| Job | Needs | Permissions | Runs |
|---|---|---|---|
| `image` | | `contents: read`, `packages: write` (unchanged) | docker; the check also refuses an `artifacthub.io/images` entry other than `ghcr.io/fedeam/kausa:<version>` |
| `sbom` (new) | `image` | `contents: read`; no login or OIDC, but the run's artifact store | syft v1.55.0 |
| `publish` | `image`, `sbom` | `contents: read`, `packages: write`, `id-token: write`, `attestations: write` | cosign, helm, docker, GitHub's actions |
| `release` (new) | `publish` | `contents: write` only | the runner's `gh`; nothing downloaded |
| `artifacthub` (new) | `release` | `contents: read`, `packages: write` | oras v1.3.6, if `charts/artifacthub-repo.yml` exists |

- **`sbom`** refuses a `digest-<arch>.txt` that is not a single-platform manifest, runs `syft scan
  registry:<ref> --platform linux/<arch> -o cyclonedx-json@1.6=…` (`SYFT_CHECK_FOR_APP_UPDATE=false`),
  requires `specVersion` 1.6, `pkg:cargo/kausa@<version>`, a `pkg:cargo/kube@` and a `pkg:deb/`
  component (`jq -e`), and outputs each file's SHA-256. It keeps a `contents: read` job token and
  the run's artifact store: `publish` uses nothing from it but the SBOM bytes, syft's word anyway.
- **`publish`**, in order: stop if the GitHub release exists; download `digest-amd64` and
  `digest-arm64` by name and trust neither: each must be one line `ghcr.io/fedeam/kausa@sha256:<64
  hex>` equal to the SHA-256 of `docker buildx imagetools inspect --raw` of `X.Y.Z-<arch>`, a tag
  only `image` can write until then; every later step uses only those digests; check the SBOMs
  against `sbom`'s outputs; the index, its signature and provenance; `actions/attest` of each SBOM
  to its digest (`push-to-registry: true`, `create-storage-record: false`); `helm package`, `helm
  push`, and a registry check that the tag holds helm's `Digest:` with the `.tgz` as its one chart
  layer; `cosign sign` and `attest-build-provenance` (same two inputs) of the chart's digest;
  `SHA256SUMS` over the `.tgz` and both SBOMs, `cosign sign-blob`, the notes; today's `.tgz`
  provenance and `.intoto.jsonl`; and, as a job output, one SHA-256 over every file it hands over.
- **`release`** refuses unless that SHA-256 and `sha256sum -c SHA256SUMS` match, then `gh release
  create` with today's four assets and both `kausa-X.Y.Z-linux-<arch>.cdx.json`.
- **`artifacthub`** runs Artifact Hub's `oras push` after the release, which it never blocks.

### The chart

`Chart.yaml` gains `maintainers: [{name: FedeAM, url: https://github.com/fedeam}]` (no email) and
`artifacthub.io/category: monitoring-logging`, `artifacthub.io/license: MIT OR Apache-2.0` (as
`Cargo.toml:11`), `artifacthub.io/images` (the image at `appVersion`, both platforms; required,
since a dry-run with default values fails without `rbac.scope`) and `artifacthub.io/links`
(documentation, changelog, security policy, issues, on `main`). A `ci`-target check and the tag
check refuse an images tag other than `appVersion`. A packaged `charts/kausa/README.md`, with no
version in it, says what kausa is, that `rbac.scope` is required, the OCI install with `--version`,
and the keyless identity (`release.yml@refs/tags/v<version>` of `fedeam/kausa`, GitHub's OIDC
issuer): the chart's "signing key information".

### Artifact Hub

`charts/artifacthub-repo.yml`, outside the package, holds a comment and `repositoryID`: the ID is
public on Artifact Hub, not a secret; no `owners`, which would publish an email. The maintainer
registers once (Helm charts, name `kausa`, URL `oci://ghcr.io/fedeam/charts/kausa`, no credentials)
and puts the ID in v0.6.0's release pull request. If the service refuses the URL before a push, they
register after v0.6.0, and Verified publisher comes with v0.6.1. Nothing about it reaches CI.

### The release notes

Install leads with `helm install kausa oci://ghcr.io/fedeam/charts/kausa --version X.Y.Z`, keeps the
URL, shows `helm upgrade … --reuse-values` from a URL install, and names every digest. Verify gives
login-free cosign lines first (the image's signature and provenance, each SBOM, the chart's
signature and provenance), then a pinned install (`cosign verify-blob` of `SHA256SUMS`, `helm pull`,
`sha256sum --ignore-missing -c`, `helm install` of the file), then `gh attestation verify`, marked
as needing `gh auth login`, with `--signer-workflow` and `--source-ref`. No install by digest: Helm
3.16.1 rejects `oci://…@sha256:`.

## Alternatives

- **The image's repository, a `chart-X.Y.Z` tag, a chart renamed `kausa-chart`:** the tag is the
  version; a rename changes installed releases. `fedeam/helm/kausa`: as good; `charts/` is common.
- **One `publish` job, as today:** syft, oras and helm would hold `contents: write` and the OIDC
  token. **Splitting off only `release`:** syft and oras keep the OIDC token and could mint
  attestations under the release identity. **A `chart` job without OIDC for helm:** helm writes the
  bytes that are signed anyway; one more job and hand-off for little. **Trusting `digest-*.txt`
  (`release.yml:110-114`, `:139`):** any job, `sbom` too, can upload one.
- **Refusing an existing chart version:** any failure after the push needs a hand deletion.
  **Reusing it on a rerun:** nothing tells whether this workflow pushed it.
- **`cosign attest` for the SBOM:** never reaches GitHub's attestation API. **`actions/attest-sbom`:**
  deprecated. **cargo-cyclonedx:** describes `Cargo.lock`, not the image or its Debian packages.
  **BuildKit `--sbom`:** SPDX, and Gitea has the classic builder. **One SBOM on the index:** two
  binaries. **CycloneDX 1.7, syft's default:** a bump would change the format silently.
- **`cargo install cargo-auditable`:** compiles it every build, a second kind of pin. **In the `ci`
  target:** nothing built there ships.
- **Metadata pushed by hand** (a personal token), **a `workflow_dispatch` job**, **`curl` against
  the registry API**, **oras in `publish`:** a credential, a second privileged entry point,
  hand-written token exchange, or oras with the OIDC token.
- **A Helm HTTP repository on Pages:** a branch per release, no cosign signature. **`.prov` with
  `signKey`:** a private key, a secret. **Backfilling older releases:** a later run would sign them.
- **`helm.sh/images`:** supported only since 2026-09-16. **`artifacthub.io/crds`:** the CRD is
  optional; one more hand-kept value. **`changes`, `prerelease`:** copies of `CHANGELOG.md`.

## Consequences

- **Users gain** `helm install oci://…` of a signed chart, byte-equal to the `.tgz`, with provenance
  on its OCI subject; an attested SBOM per architecture, also on the release; crates that `cargo
  audit bin`, syft, grype or Trivy read from the binary; login-free verification; a listing.
- **Costs:** three jobs; syft, oras and cargo-auditable bumped by hand, `actions/attest` by
  Dependabot; two Debian packages that float with the suite; about 3 kB per binary; two SBOMs of
  about 0.5 MB per release; a second GHCR package to keep public; one annotation line per release.
- **Reruns** before the release exists push the chart again under a new digest (repackaging is not
  byte-reproducible); the old one stays, perhaps signed, and the notes name the last. Once the
  release exists, `publish` refuses. A full rerun re-pushes the per-architecture tags, as today.
  Artifact Hub reprocesses only on a new version, so it may keep the first digest.
- **Docs, same change:** install-with-helm (OCI with `--version`, the URL as the alternative,
  URL-to-OCI upgrade, "Verify a release"), README (`:65-66`, `:167-170`), maintain.md (pins, "Cut a
  release", "List the chart on Artifact Hub", "If something fails"), helm-chart.md (`:9-11`),
  SECURITY.md ("Secrets": each job's permissions; verify by digest), ARCHITECTURE.md (`:86`),
  docs/index.md, dependabot.yml (`:17-20`), comments, CHANGELOG, Updates on 0003, 0010 and 0012.
  No Artifact Hub badge until `AGENTS.md:201-206` lists one.
- **Tests:** the `.dep-v0` guard (every runtime build); the images check; `helm lint --strict` with
  the new metadata; the SBOM, manifest, digest and hand-off guards. Before v0.6.0, the helm, docker,
  oras and syft commands are rehearsed on a throwaway registry. Keyless signing needs Actions: after
  v0.6.0 every command of the notes runs from a logged-out machine; an Update records it.
- **Hypotheses:** GHCR lets the job token create `charts/kausa`, links it, and may make it private;
  Helm 4.3.1 behaves as 3.16.1 did; `.dep-v0` survives `strip`, LTO and musl, and syft reads it and
  `dpkg/status.d` (each seen once in a scratch build); `-o cyclonedx-json@1.6=` and
  `SYFT_CHECK_FOR_APP_UPDATE` work as named (the guard checks the first); `cosign
  verify-attestation` finds what actions/attest pushed (shown for provenance); the live Artifact Hub
  accepts the URL before a push, shows Signed, renders the license expression and an email-less
  maintainer, and its scanner reads the crates; the `gh` flags behave as documented.
- **Follow-ups, each its own decision:** provenance on the per-architecture digests;
  `attest-build-provenance` to `actions/attest`; `helm.sh/images`; refusing re-pushed image tags.
