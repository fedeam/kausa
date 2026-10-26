# 0005. Name the repository kausa, without a type prefix

- Status: accepted
- Date: 2026-10-23

## Context

The maintainer's personal standard gives every repository name a **type prefix**; a controller
would be `tool-kausa` or `infra-kausa`. Since 2026-10-23 the standard names public open-source
projects after the product, without a prefix, and each one records this in an ADR of its own.
kausa is meant to become such a project ([0001. Build a small, read-only explainer that writes
one redacted explanation per incident](0001-scope-and-positioning.md)).

The name: *k* for Kubernetes and *causa*, "cause" in Latin and Spanish, since the tool says why
a workload fails. Availability, checked on 2026-10-23 without credentials:

- crates.io: no crate named `kausa` (the sparse index, `https://index.crates.io/ka/us/kausa`,
  answers 404).
- npm (`https://registry.npmjs.org/kausa`) and PyPI (`https://pypi.org/pypi/kausa/json`): 404.
- GitHub: no repository named exactly `kausa` in the repository search. A search for `kausa`
  and `kubernetes` returns `kausality-io/kausality`, a different name.

The maintainer chose it among `kausa`, `kwhy`, `ksplain` and `kundig`, all available in the same
registries.

## Alternatives considered

- **Follow the standard as is** (`tool-kausa`): consistent with the maintainer's other
  repositories, but a project with a prefixed name is harder to find for most potential users.

## Decision

The repository is named `kausa`, without a type prefix, as the standard allows for public
open-source projects: the repository is named the way people search for and install the tool.
FedeAM took this decision on 2026-10-23.

## Consequences

- The repository, the crate, the binary and the container image share one name: `kausa`, on Gitea
  and on GitHub (`fedeam/kausa`).
- Reserving the crate name on crates.io is left for later, with a real release
  ([Publish the repository on GitHub](../how-to/publish-the-repository.md)).
