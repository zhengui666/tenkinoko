# Tenkinoko

这是 Tenkinoko 的中文说明。英文主文档见 [README.md](./README.md)。

Tenkinoko 是一个面向 **Polymarket 天气市场** 的单机自动交易守护进程。项目目标不是“更快地下单”，而是把 **历史可用天气预报、多源校准模型、严格风控** 组合成一套可恢复、可回放、可维护的短周期交易系统。

## 核心约束

- 只做 `Polymarket` 天气市场
- 单机部署，主进程为 `tradingd`
- `RocksDB` 是唯一持久化存储
- 交易依据是模型后验与市场可执行概率之间的偏差
- 信号不能绕过风控和执行状态机

## 文档入口

- 项目主页与架构说明：[README.md](./README.md)
- 贡献规范：[CONTRIBUTING.md](./CONTRIBUTING.md)
- 安全披露：[SECURITY.md](./SECURITY.md)
- 变更记录：[CHANGELOG.md](./CHANGELOG.md)
- 发布流程：[RELEASING.md](./RELEASING.md)

## 快速说明

- 优先持有周期：`45 minutes to 12 hours`
- 允许持有周期：`15 minutes to 1 day`
- 单市场单头寸暴露上限：`<= 2% total equity`
- LLM 只作为受约束辅助模块，不直接产生裸交易指令

如需完整项目背景、模块边界、命令示例与路线图，请阅读英文主文档 [README.md](./README.md)。
