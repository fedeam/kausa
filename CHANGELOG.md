# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

The first implementation weekend: detection and grouping. Nothing is released yet.

### Added

- `kausa run`: watches Pods and Warning events, detects crash loops, image pull failures,
  out-of-memory kills, container creation errors, scheduling failures, probe failures, volume
  failures and failed Jobs (`D001` to `D008`), groups the failures of one workload into one
  incident once they persist for the settle window (`--settle-seconds`, 60 by default), and logs
  each incident as a JSON line with a fingerprint. Nothing is sent to a model and no Event is
  written yet.
- `kausa --help` and `kausa --version`.
- Documentation: scope and positioning after a competitive survey, the planned architecture, the
  detection and redaction rules, the permissions and the data sent to the model provider for
  v0.1, how to build from source and run the checks, a security policy with a threat model, a
  contributing guide, and the decision records in `docs/adr/`.

[unreleased]: https://github.com/fedeam/kausa/commits/main
