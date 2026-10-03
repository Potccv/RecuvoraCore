# 恢复任务

`workflow.rs` 导出任务配置、事件、证据和状态。私有 service 实现任务迁移。恢复引擎在外层聚合中使用 `RecoveryState`，调用方无需手工关联审批和任务。

流程见[恢复参考](../../../docs/workflow.md)，提交前提见[调用契约](../../../docs/calling-contract.md)，开发规则见[AGENTS](AGENTS.md)。
