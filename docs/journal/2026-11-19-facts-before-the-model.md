# 2026-11-19 — Facts before the model

## Request

Write down a new direction for kausa: report incidents as facts without a model, with the
model's explanation as an option. Then accept it.

## What was done

- [0026. Report every incident as facts without a model, keep the explanation optional, and
  widen coverage one bounded source at a time](../adr/0026-facts-before-the-model.md) was
  proposed in [#115](https://github.com/fedeam/kausa/pull/115) and accepted by FedeAM the same
  day. It is a direction: each of its four stages needs its own ADR, with its memory measured
  before that ADR is accepted.
- [0001. Build a small, read-only explainer that writes one redacted explanation per
  incident](../adr/0001-scope-and-positioning.md) got its `Update` paragraph.
- Nothing is implemented. The description of kausa (README, front page, chart, Artifact Hub)
  stays as it is until the release that ships stage 1, as the `Update` of 0026 says.

## Next

- The ADR of stage 1, the fact statement for the fourteen detections, and the prototype and
  research note that 0026's point 6 asks for before it can be accepted.
