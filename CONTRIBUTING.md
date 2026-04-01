# Contributing

Thank you for considering a contribution to Tenkinoko.

This repository is not a generic trading scaffold. It is a single-machine, risk-disciplined trading system for Polymarket weather markets. Before proposing changes, make sure your approach stays aligned with the architectural constraints documented in [AGENTS.md](./AGENTS.md) and [README.md](./README.md).

## Before You Start

- Read [AGENTS.md](./AGENTS.md) and [README.md](./README.md).
- Keep the scope limited to weather markets rather than expanding into a generic multi-exchange platform.
- Do not introduce extra databases, message brokers, or unnecessary service splits.
- Do not turn the LLM layer into a direct trading decision-maker.
- If a change affects strategy, execution, or risk behavior, update tests and replay validation accordingly.

## Development Setup

```bash
cargo build --release
cargo test --workspace
```

For a lighter validation pass, run at least:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets
```

## Change Expectations

- Prefer small, reviewable changes over large rewrites.
- Preserve the single-writer trading model.
- Preserve RocksDB as the only persistent store unless the project constraints are explicitly changed.
- Document important behavioral changes in [CHANGELOG.md](./CHANGELOG.md) when they affect users, operators, or reviewers.
- Keep docs and examples in sync with CLI behavior and repository structure.

## Pull Requests

When opening a pull request:

- explain the problem being solved
- describe the chosen approach and the tradeoffs
- call out any impact on risk, replay semantics, persistence, or operational behavior
- list the validation you ran

Follow the collaboration rules in [.github/CODE_OF_CONDUCT.md](./.github/CODE_OF_CONDUCT.md).
