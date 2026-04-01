# Changelog

本项目遵循“显式记录重要变化”的原则。变更日志采用类似 Keep a Changelog 的结构，但内容会更强调：

- 风控约束变化
- 状态机行为变化
- RocksDB schema 变化
- 回测 / replay 语义变化
- 外部接口或运行方式变化

## [Unreleased]

### Added

- 初始 Rust workspace 结构
- `tradingd` 主入口
- README、贡献指南、社区健康文件与基础 CI

### Changed

- 根目录 README 升级为完整项目首页风格

### Fixed

- 暂无

### Removed

- 删除旧 `docs` 目录，统一文档入口到根目录

## Versioning Notes

在正式版本化之前，建议每次合并仍维护 `Unreleased` 段落。后续发布版本时，请按如下方式整理：

```md
## [0.1.0] - YYYY-MM-DD

### Added
- ...

### Changed
- ...

### Fixed
- ...
```

如果某次发布影响以下内容，请单独写明：

- 风险限额或风险状态机
- 订单执行状态机
- RocksDB 列族 / key schema
- 回测无泄漏保证
- CLI 命令或配置兼容性
