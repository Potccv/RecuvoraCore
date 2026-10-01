# 恢复编排

[workflow.rs](../workflow.rs) 导出 `RecoveryState`、`RecoveryConfig`、`RecoveryCommand`、`RecoveryEvent` 和证据类型。Host 输入已提交事实，Core 返回待提交状态与后续操作意图，不启动运行循环。

实现位于私有 [service](service/README.md) 模块。完整过程见[恢复流程](../../../docs/recovery.md)，开发规则见[AGENTS](AGENTS.md)。
