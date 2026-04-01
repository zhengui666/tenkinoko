# Changelog

This project follows an explicit "record meaningful changes" policy. The structure is similar to Keep a Changelog, but the emphasis is stronger on:

- risk-control changes
- execution or state-machine behavior changes
- RocksDB schema changes
- backtest or replay semantics
- external interface and operational behavior changes

## [Unreleased]

### Added

- Initial Rust workspace structure
- `tradingd` main entry point
- README, contribution guide, community health files, and baseline CI
- Chinese README as an optional companion document

### Changed

- Promoted the root README to an English-first project homepage
- Replaced repository-local absolute links with portable relative links

### Fixed

- Removed hard-coded local absolute paths from documentation links

### Removed

- Consolidated the old `docs` entry points into root-level documentation

## Versioning Notes

Before formal releases begin, keep the `Unreleased` section updated on every meaningful merge.

When versioning starts, prefer a structure such as:

```md
## [0.1.0] - YYYY-MM-DD

### Added
- ...

### Changed
- ...

### Fixed
- ...
```

If a release impacts any of the following, document it explicitly:

- risk limits or risk-state machines
- order execution state machines
- RocksDB column families or key schema
- no-lookahead guarantees
- CLI commands or configuration compatibility
