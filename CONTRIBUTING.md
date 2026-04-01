# Contributing

感谢你考虑为 Tenkinoko 贡献代码。

这个项目不是通用交易脚手架，而是一个面向 Polymarket 天气市场的、强调风控和可恢复性的单机交易系统。提交改动前，请先确保你的方案与仓库中的架构约束一致。

## Before You Start

- 先阅读 `AGENTS.md` 与根目录 `README.md`
- 保持交易范围在天气市场，不扩展成多交易所通用平台
- 不要引入额外数据库、消息队列或微服务拆分
- 不要让 LLM 成为直接交易决策者
- 涉及策略、执行、风险的改动必须同时考虑测试与回放验证

## Development Setup

```bash
cargo build --release
cargo test --workspace
```

如果你只想快速验证改动，至少运行：

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --no-run
```

## Pull Request Guidelines

- 变更范围尽量小，避免把多个主题混在一个 PR
- 提交说明使用清晰英文，描述“做了什么”和“为什么”
- 如果改动可能影响 PnL、风控或执行行为，请明确写出影响面
- 如果新增依赖，请说明内存成本、运维成本和为什么现有依赖不够
- 如果修改 RocksDB schema、状态机或回放语义，请补充说明迁移或兼容策略

## Tests And Validation

以下改动通常需要测试或验证材料：

- 市场规则解析
- 概率分桶或后验映射
- 风险限额与仓位限制
- 执行状态机
- RocksDB 序列化与恢复
- 无未来数据泄漏保证

如果当前仓库还没有完整测试覆盖，请至少在 PR 描述中注明：

- 你运行了哪些命令
- 哪些部分尚未覆盖
- 还存在哪些已知限制

## Style Expectations

- 使用惯用、可维护的 Rust
- 避免 `unwrap()` 出现在生产路径
- 避免把业务逻辑写成字符串拼接或隐式全局状态
- 保持模块边界清晰，优先小而专注的改动

## What Usually Gets Rejected

- 与项目目标冲突的“平台化”改造
- 为了回测结果而削弱风控
- 使用 hindsight 数据的回测逻辑
- 未解释成本的重量级基础设施依赖
- 把 README 中尚未实现的能力包装成已完成特性
