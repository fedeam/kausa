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

## Later the same day: 0025 measured

The measurement of [0025](../adr/0025-withhold-destructive-commands.md), run by hand: the agents
that were to run it had hit their usage limit. Result and method in [Destructive commands
withheld, measured](../research/2026-11-05-destructive-commands-measured.md): every bar holds, so
the example alert `KausaAnswerWithheld` is added (`deploy/prometheus/kausa-alerts.yaml`, its
`promtool` test, its section in [Alerts](../reference/alerts.md), and the counts of alerts in
`src/metrics.rs` and `tests/chart/render.sh`).

- **Deviations:** the sink run's caps were raised to 36,000 calls, and the entries outside the
  sixteen measured were removed from the throwaway cluster, since the queue of 16 dropped most of
  the 31 incidents that arrived at once; `inject-log`'s Job was re-created for the same reason.
- **Checks:** `promtool check rules --lint=all --lint-fatal` and `promtool test rules` (3.15.1, the
  pinned version, in a container removed afterwards), `cargo test`, `tests/chart/render.sh`.
- **Next:** v0.6.1, with 0025 and the alert; then check that Artifact Hub shows the publisher as
  verified.

## Release v0.6.1

Version 0.6.1 in `Cargo.toml`, `Cargo.lock` and the chart, with the changelog's Unreleased entries
under `[0.6.1] - 2026-11-05`: 0025's withheld command shapes, the alert `KausaAnswerWithheld` and
the listing on Artifact Hub. The README, `SECURITY.md`, the front page, the command-line reference
and the install guide name v0.6.1. The release's verification, the check of Artifact Hub and the
bump of `tests/e2e/upgrade-from.txt` follow the tag.

- **Verification:** every command of the v0.6.1 notes passed on 2026-11-05: the `cosign` block
  (image signature and provenance, both SBOMs, chart signature and provenance) and the checksum
  block against an anonymous `helm pull` of the OCI chart, with no login, and the `gh` block,
  logged in. The `artifacthub.io` tag of the chart answers an anonymous pull.
- **Upgrade:** in kind (node `v1.36.4`), a release installed from the v0.6.0 OCI chart, namespaced
  to `default` with two replicas and `dryRun`, was upgraded to v0.6.1 with `--reuse-values`: the
  values stayed, both new Pods ran `ghcr.io/fedeam/kausa:0.6.1`, one of them took the Lease about
  30 s later, and no line was `warn` or `error`.
- **Artifact Hub:** within the hour after the tag, Artifact Hub listed 0.6.1, signed, with the
  publisher verified and no tracking errors (its public API, read without an account). This closes
  the next step above.
- **Next:** `tests/e2e/upgrade-from.txt` pins v0.6.1.
