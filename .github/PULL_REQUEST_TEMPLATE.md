## What and why

<!-- What this changes and why. Link the issue or ADR it implements. -->

## Checklist

- [ ] The title follows Conventional Commits with a scope: `type(scope): subject`, up to 64 characters (GitHub appends ` (#<number>)` and the result becomes the commit header on `main`)
- [ ] The checks in [AGENTS.md](https://github.com/fedeam/kausa/blob/main/AGENTS.md#verification-same-as-ci) pass (`docker build --target ci .` runs them all)
- [ ] A new or changed detection or redaction rule comes with fixtures that trigger it, the closest ones that must not, and the expected output
- [ ] Nothing new leaves the cluster, and no new permission is requested, without an ADR and an update to `SECURITY.md` and the reference pages
- [ ] Docs are updated, with an ADR if this makes a decision, and `CHANGELOG.md` under `Unreleased` if users will notice the change
- [ ] No secrets, internal hostnames, personal data or real cluster output (logs, event messages, names, addresses, even sanitized by hand) in code, fixtures or docs: fixtures use invented values
