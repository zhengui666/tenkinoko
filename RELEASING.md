# Releasing

Tenkinoko is still in active development, but the release process should stay disciplined from the start.

## Release Philosophy

A release is not done just because the workspace compiles. At minimum, confirm that:

- formatting, linting, and test compilation pass for the workspace
- [README.md](./README.md), [CHANGELOG.md](./CHANGELOG.md), and contributor-facing docs are up to date
- any behavior changes in risk, execution, recovery, or replay are explicitly documented
- no local-only configuration, data files, or build artifacts were included by mistake

## Pre-release Checklist

Run at least:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --no-run
```

If the release changes strategy behavior, also confirm that:

- backtest or replay results were reviewed
- no lookahead leakage was introduced
- risk limits and state transitions still behave as intended

## Changelog Discipline

Before formal versioning starts, keep the `Unreleased` section in [CHANGELOG.md](./CHANGELOG.md) current.

When tagging a version, prefer a structure like:

```md
## [0.1.0] - YYYY-MM-DD

### Added
- ...

### Changed
- ...

### Fixed
- ...
```

If a release affects any of the following, call it out explicitly:

- risk limits or risk-state behavior
- order execution state machines
- RocksDB column families or key schema
- no-lookahead guarantees in backtesting
- CLI compatibility or configuration expectations
