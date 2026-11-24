# 2026-11-24 — The Incident resource

## Request

Write the ADR that records kausa's half of the contract with kurita, a controller that proposes
remediations for kausa's incidents: an `Incident` resource that only kausa writes, and the report
by webhook of what kurita proposed. Docs only; kurita's ADR 0008, its other half, was already on
kurita's `main` as `proposed`.

## What was done

- [0028. Publish each incident as an immutable Incident that only kausa writes, and report by
  webhook what kurita proposed for it, both off by default](../adr/0028-incident-resource-and-remediation-report.md),
  `proposed`. It records the design chosen by a review of two complete proposals, aligned field by
  field with kurita's ADR 0008: the Incident's schema, the data kausa keeps for it, retention,
  permissions, the `remediation` message of the webhook's `v2`, the trust statement, the costs
  computed and the bars that must be measured before both ADRs are accepted.
- `Update 2026-11-24` paragraphs at the end of 0001, 0009, 0015 and 0027, each saying what 0028
  would change there and that it takes effect only when 0028 is accepted.
- A security and a docs review of the proposal changed it the same day: `remediation` messages got
  an hourly bound of their own, so they never cost a `detected` or an `explained` message; the
  table of created Incidents never drops an entry younger than 6 hours, and kausa creates no
  Incident while 500 such entries exist; the trust statement gained whoever can create Events;
  the Incident's `spec` is required, so that its immutability rule always runs.
- Nothing is implemented, and nothing in it is measured.

## Decisions

- Proposed only: [0028](../adr/0028-incident-resource-and-remediation-report.md). It is accepted
  together with kurita's ADR 0008, once the bars of its "Conditions of acceptance" are measured on
  the Raspberry Pis and in kind.

## Next

- The implementation, behind `incidents.enabled` and `incidents.reportRemediations`, both off by
  default, with the reference pages that 0028 lists under Consequences.
- Measure bars 1 to 8, and settle what "Not verified" lists, first whether a metadata `get` of a
  ReplicaSet returns its annotations.
