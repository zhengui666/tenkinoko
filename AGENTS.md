# AGENTS.md

## 1. Purpose

This repository implements a **real-money, short-horizon automated trading system** for **Polymarket weather markets**.

Primary goal:
- Trade weather contracts with a holding period of **15 minutes to 1 day**.
- Use **multi-source weather forecasts + calibrated statistical models + constrained LLM assistance**.
- Maintain **strict risk limits** and a **single-machine, low-resource deployment profile**.

This is **not** a generic microservice playground, a research scrapbook, or a latency-arbitrage engine.

The system must remain:
- production-oriented
- resource-efficient
- recoverable after crashes
- easy for one developer to operate
- compatible with **Codex-driven development**

---

## 2. Non-negotiable project constraints

You must preserve all of the following constraints unless the user explicitly changes them.

### Trading scope
- Venue: **Polymarket**
- Domain: **weather only**
- Preferred markets for v1:
  - daily high temperature
  - daily low temperature
  - threshold-based weather outcomes
- Preferred holding window: **45 minutes to 12 hours**
- Allowed holding window: **15 minutes to 1 day**

### Strategy constraints
- Do **not** optimize for pure speed.
- Do **not** build a high-frequency market-making or queue-sniping strategy.
- Do **not** build long-horizon climate/event strategies for v1.
- Do **not** use raw LLM output as a direct trading signal.
- Do **not** allow lookahead leakage in backtests.

### Risk constraints
- Maximum exposure per market position: **<= 2% of total equity**.
- Total correlated exposure by cluster (same city / same date / same weather regime): conservative hard cap.
- Support risk states such as:
  - `Normal`
  - `Cautious`
  - `ReduceOnly`
  - `HaltOpen`
  - `EmergencyFlat`

### Infra constraints
- Target environment is a **low-spec server**.
- Database/storage: **RocksDB only**.
- Prefer **single main process**.
- Avoid introducing PostgreSQL, Redis, Kafka, NATS, Elastic, ClickHouse, or similar infrastructure.
- Every dependency must justify its memory and operational cost.

---

## 3. Architecture rules

### 3.1 Default deployment shape
Prefer this deployment model unless there is a strong reason not to:

- `tradingd`: the main process and the only writer for trading state
- optional `botd`: read-only Telegram/UI helper process

If a feature can live inside `tradingd` without harming clarity or reliability, keep it inside `tradingd`.

### 3.2 Single-writer rule
There must be exactly one component with authority to:
- submit orders
- cancel orders
- reconcile order state
- mutate live position state
- mutate live risk state

Do not introduce multiple concurrent trading writers.

### 3.3 Event-driven over service sprawl
Prefer:
- in-process modules
- typed internal events
- explicit state machines
- append-only event logs in RocksDB

Avoid:
- many tiny services
- RPC-heavy internal boundaries
- unnecessary message brokers
- duplicated caches

### 3.4 Module boundaries still matter
Even though deployment is compact, preserve clean code boundaries.

Expected major modules/crates:
- `domain-core`
- `config-core`
- `storage-rocksdb`
- `polymarket-adapter`
- `weather-adapter-openmeteo`
- `weather-adapter-noaa` or equivalent official-source adapter
- `llm-core`
- `posterior-models`
- `signal-engine`
- `risk-engine`
- `execution-engine`
- `telegram-bot`
- `backtest-engine`
- `scheduler-core`
- `telemetry-core`

Do not collapse everything into one giant file or one giant crate.

---

## 4. Core trading design expectations

### 4.1 What the strategy actually trades
The strategy should trade the difference between:
- the **model posterior distribution** for a weather outcome
- and the **market-implied executable probability** after fees, spread, and slippage

The strategy should **not** be framed as:
- “ask an LLM to predict the weather”
- “trade whatever moved recently”
- “maximize number of trades”

### 4.2 Model output requirements
Models must produce **probabilities or probability distributions**, not just point estimates.

Examples:
- `P(Tmax in bucket_i)`
- `P(precipitation > threshold)`
- calibrated confidence / uncertainty scores

### 4.3 LLM usage rules
LLM is allowed for:
- market rule parsing
- ambiguity resolution
- weather-discussion summarization
- source-divergence explanation
- daily report generation
- post-trade attribution

LLM is **not** allowed to be the sole decision-maker for:
- entering trades
- sizing positions
- bypassing risk checks

All LLM outputs that affect trading logic must be transformed into **strict structured data**.

Suggested pattern:
- JSON schema output
- validated before use
- persisted to RocksDB
- read by the core engine asynchronously

### 4.4 Execution style
Default execution style:
- **maker-first**
- taker only when justified by large net edge and timing constraints
- no blind crossing
- no constant quote-chasing loops

### 4.5 Exit logic
Exit logic must consider both market state and weather-state changes.
At minimum support exits caused by:
- posterior reversal
- edge collapse
- source divergence increase
- observation mismatch
- pre-resolution de-risking
- data-quality failure
- market-state anomaly

---

## 5. Backtesting rules

Backtesting is mandatory and must be realistic.

### 5.1 No leakage
Never use future weather observations as model inputs before they would have been known.
Never use hindsight-only labels as live features.
Never allow hidden date leakage into LLM prompts when the goal is to simulate historical decision-making.

### 5.2 Historical forecast requirement
Backtests must prefer **historical forecast data available at decision time**, not only realized weather.

### 5.3 Date obfuscation requirement
When LLM participates in historical replay tasks, dates should be obfuscated by remapping years into future years while preserving:
- month
- day
- time-of-day
- ordering
- leap-year compatibility where relevant

### 5.4 Replay design
Backtest modes should ideally be separated into:
- forecast-quality / model-quality evaluation
- execution replay
- full event-log replay

### 5.5 Backtest performance discipline
Backtests must stream data and write incrementally.
Do not load huge historical datasets fully into memory on a low-spec machine.

---

## 6. RocksDB rules

RocksDB is the only persistent data store unless the user explicitly changes the requirement.

### 6.1 Store design
Use RocksDB for:
- current state
- event log
- raw weather payload snapshots
- feature snapshots
- orders
- positions
- risk state
- telegram outbox
- command inbox
- job queue
- backtest artifacts
- date remapping tables

### 6.2 Column families
Prefer explicit column families for major domains. Typical examples:
- `market_meta`
- `market_runtime`
- `orderbook_delta`
- `weather_forecast_raw`
- `weather_obs_raw`
- `features`
- `signals`
- `orders`
- `positions`
- `risk_state`
- `telegram_outbox`
- `command_inbox`
- `job_queue`
- `backtest_raw`
- `date_map`
- `event_log`

### 6.3 Key design
Keys should be:
- stable
- prefix-sortable
- easy to range-scan
- explicit about entity and time

Examples:
- `market_meta#{market_id}`
- `forecast#{station}#{source}#{run_ts}`
- `signal#{market_id}#{decision_ts}`
- `position#{position_id}`
- `outbox#{status}#{created_ts}#{msg_id}`

### 6.4 Memory discipline
All RocksDB usage must respect a strict memory budget.
Do not assume abundant RAM.
Prefer:
- shared block cache
- explicit write-buffer budgeting
- compact value layouts
- bounded iterators
- minimal hot in-memory mirrors

---

## 7. Coding rules

### 7.1 Rust style
Write idiomatic, production-grade Rust.

Prefer:
- small focused modules
- explicit domain types
- enums for state machines
- typed errors
- clear ownership boundaries
- `Result`-based error handling
- testable pure functions for strategy logic

Avoid:
- massive god objects
- pervasive `unwrap()` in production paths
- stringly typed business logic
- hidden global mutable state
- overly clever macro-heavy designs unless justified

### 7.2 Error handling
All external IO boundaries must have robust handling for:
- timeout
- retry
- backoff
- partial failure
- stale data
- malformed payloads
- reconciliation after restart

### 7.3 Config discipline
All operationally important parameters must be configurable.
Examples:
- API endpoints
- polling intervals
- market filters
- risk limits
- memory budgets
- Telegram credentials
- LLM model selection
- replay speed

Do not hardcode secrets.
Do not hardcode environment-specific paths unless explicitly instructed.

### 7.4 Logging and telemetry
Every critical workflow must produce structured logs.
At minimum cover:
- market ingestion
- weather ingestion
- feature generation
- signal generation
- risk decisions
- order transitions
- reconciliation
- Telegram sends
- backtest milestones

### 7.5 Tests
At minimum, add or maintain tests for:
- rule parsing
- probability / bucket mapping
- no-lookahead guarantees
- risk-limit enforcement
- sizing logic
- order state-machine transitions
- RocksDB serialization and recovery

If code changes strategy behavior, tests or replay validations should be updated too.

---

## 8. Workflow expectations for Codex

### 8.1 Before coding
Before making meaningful changes, Codex should:
1. read this file
2. inspect repo structure
3. identify relevant crates/modules
4. avoid changing unrelated files
5. form a minimal implementation plan

### 8.2 When implementing features
Prefer small, reviewable changes over giant rewrites.
If a task is large, stage it:
- domain types first
- storage schema second
- adapter / ingest logic third
- signal/risk logic fourth
- execution integration last

### 8.3 When uncertain
If the repository contains an architecture note, design spec, or task-specific markdown file, follow those project documents.
Do not invent a conflicting architecture when a documented one already exists.

### 8.4 When touching strategy logic
Be conservative.
If a change could alter PnL, fill behavior, or risk behavior, update:
- tests
- replay fixtures
- docs/comments where necessary

### 8.5 When adding dependencies
Any new dependency must be justified by:
- problem solved
- memory cost
- binary size cost
- operational cost
- why the standard library or an existing dependency is insufficient

---

## 9. File and documentation conventions

### 9.1 Keep AGENTS.md high signal
This file should remain focused on evergreen instructions.
Do not bloat it with long implementation details that belong elsewhere.

### 9.2 Use task-specific docs when needed
Prefer creating or updating separate docs for:
- detailed architecture
- execution plans
- migration plans
- schema docs
- replay plans
- incident retrospectives

### 9.3 Document important invariants near code
If a module depends on a subtle invariant, document it in code comments and module docs.
Examples:
- order transitions that must be idempotent
- no-lookahead assumptions
- position exposure math
- date-obfuscation rules

---

## 10. Explicit anti-goals

Unless explicitly requested, do **not**:
- turn this into a generic multi-exchange trading platform
- add web dashboards before the core system is solid
- add Kubernetes, Docker Swarm, or distributed orchestration complexity
- add multiple databases
- add a separate feature store service
- add a separate orchestration service when an in-process scheduler is enough
- optimize prematurely for massive throughput
- weaken risk controls for backtest optics
- use hindsight features in live or replay logic
- replace calibrated models with prompt-only LLM forecasting

---

## 11. Preferred implementation order

When starting from scratch or expanding major functionality, prefer this order:

1. domain model and config
2. RocksDB schema and storage layer
3. market metadata + market data adapter
4. weather adapters
5. posterior model pipeline
6. signal engine
7. risk engine
8. execution engine
9. Telegram bot and notifications
10. backtest engine
11. replay and recovery hardening
12. optimization and tuning

---

## 12. Definition of done

A feature is not done just because code compiles.
A feature is done only when it is:
- implemented cleanly
- consistent with project constraints
- recoverable after restart when applicable
- covered by tests or replay validation when appropriate
- documented enough for future Codex runs and human maintenance

---

## 13. If you must choose

When trade-offs are unavoidable, prefer in this order:
1. correctness
2. risk control
3. recoverability
4. clarity
5. low resource usage
6. performance
7. convenience

Never sacrifice correctness and risk discipline just to make the system look faster or more sophisticated.
