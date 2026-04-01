# Code of Conduct

## Our Standard

Tenkinoko is a project that values correctness, risk discipline, recoverability, and careful engineering judgment.

When participating in discussions, issues, reviews, or code contributions:

- keep technical discussion concrete, respectful, and verifiable
- criticize implementations, not people
- treat trading, risk, leakage, and recovery semantics seriously
- do not exaggerate unimplemented or unvalidated capabilities
- be patient with newer contributors without lowering the technical bar

## Unacceptable Behavior

The following behavior is not acceptable:

- personal attacks, harassment, or discriminatory language
- knowingly misrepresenting repository status, test results, or strategy capabilities
- pushing merges while aware of lookahead leakage, risk-control bypasses, or recovery defects
- repeatedly posting promotional content that is clearly unrelated to the project direction
- exposing secrets, sensitive account data, or private information

## Scope

This code of conduct applies to:

- GitHub issues
- pull requests
- code review
- documentation discussions
- other collaboration directly related to this project

## Enforcement

Maintainers may remove inappropriate content, close unsuitable discussions, reject contributions that conflict with project constraints, and limit participation when necessary.

For serious issues, contact the repository owner:

- `zhangzeyuan@sensetime.com`

## Project-Specific Note

This repository is related to a real-money trading system. Compared with an ordinary tooling project, tolerance is lower for:

- imprecise strategy claims
- unsupported performance claims
- unvalidated live-trading safety assumptions
- shortcuts that can damage risk controls or replay realism
