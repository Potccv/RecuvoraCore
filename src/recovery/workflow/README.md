# 恢复编排

[workflow.rs](../workflow.rs) 导出 `RecoveryState`、`RecoveryConfig`、`RecoveryCommand`、`RecoveryEvent` 和证据类型。Host 输入已提交事实，Core 返回待提交状态与后续操作意图，不启动运行循环。

实现位于私有 [service](service/README.md) 模块。完整过程见[恢复流程](../../../docs/recovery.md)，开发规则见[AGENTS](AGENTS.md)。


完整旧历史通过 `RecoveryImport::validate` 校验 `LegacyRecoveryRevision` 序列及原审批/知识证据，使用空聚合的 `prepare_import` 保存。导入保留原身份、revision、预算和历史隔离，无执行效果；不确定原审批必须先封锁，Publishing 保留原案例与时间。步骤与限制见[导入边界](../../../docs/host-boundary-migration.md#旧数据导入边界)。
