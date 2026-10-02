# 恢复领域

各域处理自己的状态与校验，公开模块由 [mod.rs](mod.rs) 导出，跨域流程位于 `workflow`。

| 模块 | 职责 |
| --- | --- |
| [故障](incidents/README.md) | `IncidentLedger`：检查点、信号归并、故障轮次、确认 |
| [审批](approval/README.md) | `ApprovalLedger`：政策、审核、一次许可、执行结果 |
| [知识](knowledge/README.md) | `KnowledgeState`：可信修复经验、实际动作与候选、不可变版本、永久隔离、精确检索 |
| [流程](workflow/README.md) | `RecoveryState`：统一 Harness 修复、动作绑定、审批关联、结果核实与独立经验总结 |

所有领域状态由 Host 持久化。规则见[恢复领域规范](AGENTS.md)。
