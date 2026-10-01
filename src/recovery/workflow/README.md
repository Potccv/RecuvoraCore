# 恢复编排

[workflow.rs](../workflow.rs) 直接公开 `RecoveryService`、`RecoveryConfig`、任务、证据和可信接口，组合恢复领域事实与业务能力。

| 实现 | 职责与接口约定 |
| --- | --- |
| `service` | [持久恢复流程](service/README.md)：恢复决策、审批与执行关联、验收、修复经验交付及核实未知执行结果；实现模块保持私有，接口由 `workflow` 导出 |

调用方通过 `recuvora_core::recovery::workflow` 使用服务、配置、任务和证据类型；公开接口与持久记录见[恢复流程](service/README.md#公开接口与持久记录)。

可信嵌入应用主动调用恢复编排入口。该层不提供定时器、长期后台任务或应用装配；Host 负责发起和调度调用，Core 负责其领域决定与持久进度。异步调用使用 [operation](../../operation.rs) 的取消与监督支持，取消后等待正在处理的调用结束。

开发规则见[恢复编排规范](AGENTS.md)。
