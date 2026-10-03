# 源码导航

公开顶层模块只有 [operation](operation.rs) 与 [recovery](recovery/README.md)。

- [operation](operation/README.md)：中立动作、提交请求、确认和效果。
- [recovery](recovery/README.md)：恢复引擎、审批、任务、经验和纯业务计算。

`binding`、`collections`、`identity` 是内部摘要、结构共享和身份辅助。源码规则见[AGENTS](AGENTS.md)，整体职责见[架构](../docs/architecture.md)。
