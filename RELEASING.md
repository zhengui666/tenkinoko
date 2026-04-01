# Releasing

Tenkinoko 当前仍处于活跃开发阶段，但发布流程应尽量从一开始保持一致。

## Release Philosophy

发布不是“代码能编译”就结束，至少要确认：

- workspace 能完成格式、lint 和测试编译
- README、CHANGELOG 与贡献文档同步
- 风控、执行、恢复、回放等关键语义没有未说明变化
- 没有把本地临时配置、数据文件或构建产物带入提交

## Pre-release Checklist

在准备版本前，至少执行：

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --no-run
```

如果发布包含策略行为变化，还应额外确认：

- 回测或 replay 结果已检查
- 没有引入 lookahead leakage
- 风险限额没有被无意修改
- 外部适配器失败路径仍然可恢复

## Update Metadata

发布前更新以下内容：

1. `CHANGELOG.md`
2. `README.md` 中任何会误导用户的状态描述
3. 需要时更新版本号、发布说明或兼容性说明

## Tagging

建议使用语义化标签：

```bash
git tag v0.1.0
git push origin v0.1.0
```

如果当前阶段尚未采用 crate 级版本发布，也至少保持 Git tag 与变更日志一致。

## Release Notes Template

建议每次发布说明至少包含：

- 概览：本次主要变化
- 风险相关变化：是否影响限额、状态机、执行逻辑
- 存储相关变化：是否影响 RocksDB schema 或恢复
- 回测相关变化：是否影响 replay 或历史结果比较
- 升级注意事项：是否需要迁移、重建、重新校准
