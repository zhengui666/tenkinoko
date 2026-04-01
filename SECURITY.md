# Security Policy

Tenkinoko is related to a real-money trading system. In this repository, "security" includes both traditional vulnerabilities and any issue that could lead to:

- bypassed risk constraints
- broken recovery paths
- inconsistent order state
- lookahead leakage in backtests or replay
- exposed credentials, account data, or sensitive configuration

## Supported Scope

The repository is still under active development. Reports are welcome for issues affecting:

- credential and key handling
- order execution correctness and state-machine consistency
- RocksDB recovery, replay, and persistence safety
- data-source integrity and failure handling
- no-lookahead guarantees in backtests
- dependency security issues

## Reporting A Vulnerability

Do not open a public issue containing sensitive details.

If you discover an issue such as:

- leaked API keys or secrets
- behavior that can cause incorrect order placement
- a risk-control bypass
- a recovery or replay flaw that can corrupt trading state

please report it privately to the repository owner and include:

- a concise description of the issue
- affected components
- impact assessment
- reproduction steps if they are safe to share

Until a dedicated private reporting channel is added, contact the maintainer directly.
