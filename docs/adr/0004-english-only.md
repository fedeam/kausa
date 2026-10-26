# 0004. Write everything in English

- Status: accepted
- Date: 2026-10-23

## Context

kausa is meant to become a public open-source project, read and contributed to by people the
maintainer does not know ([0001. Build a small, read-only explainer that writes one redacted
explanation per incident](0001-scope-and-positioning.md)).

The maintainer's personal standard, which every one of his repositories follows, is written for
Spanish:

- Repositories, documentation and commits are written in **Spanish**.
- Session logs live in **`docs/bitacoras/`**, and ADRs use Spanish field names and status words.
- README badges read `repositorio`, `internet` and `estado`.

His earlier open-source project, kaution, made the same exception on 2026-10-23.

## Alternatives considered

- **Follow the standard as is** (Spanish, `docs/bitacoras/`): consistent with the maintainer's
  other repositories, but a Spanish-only project is harder to find, read and contribute to for
  most potential users.

## Decision

Everything in the kausa repository is written in English, as an exception to the maintainer's
Spanish standard: code, comments, documentation, ADRs, journal entries, commit messages, issues
and pull requests. FedeAM took this decision on 2026-10-23. Conversations outside the
repository are not affected.

The documentation keeps the standard's structure: Diátaxis folders (`tutorials/`, `how-to/`,
`reference/`, `explanation/`), the optional `research/`, `adr/` and a front page in
`docs/index.md`. The English stand-ins are:

| Standard (Spanish) | kausa (English) |
|---|---|
| `docs/bitacoras/YYYY-MM-DD-slug.md` | `docs/journal/YYYY-MM-DD-slug.md`, H1 `# YYYY-MM-DD — Topic` |
| `- Estado:` / `- Fecha:` in ADRs | `- Status:` / `- Date:` |
| `propuesto`, `aceptado`, `rechazado`, `obsoleto`, `reemplazado por` | `proposed`, `accepted`, `rejected`, `obsolete`, `superseded by` |
| `Estado al YYYY-MM-DD` on the front page | `Status as of YYYY-MM-DD` |
| Badges `repositorio`, `internet`, `estado` | `repository`, `internet`, `status`, same values translated and same colors |

## Consequences

- The three badges of the standard stay on one line under the README's H1 with the standard's
  colors: `repository: private` and `internet: no` in `0969da`, `status: in development` in
  `9a6700`. When the repository goes public the badge becomes `repository: public` in `bc4c00`
  ([Publish the repository on GitHub](../how-to/publish-the-repository.md)). A second line carries
  the project's own badges, under the rule in [`AGENTS.md`](../../AGENTS.md#readme-badges).
- Agents working in this repository write English even where the maintainer's workspace asks for
  Spanish; the repository's own agent instructions say so.
- Explanations that kausa writes are in English in v0.1. Other languages would be a configuration
  of the prompt, decided in their own ADR if anyone asks.
