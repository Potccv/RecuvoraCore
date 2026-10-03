# 恢复引擎

`runner.rs` 的 `RecoveryEngine` 控制阶段和能力调用；`RecoveryPlatform` 注入事实、提交、观察、审核、执行、验收与总结。`session.rs` 的 `RecoverySession` 将审批、任务和经验作为一个原子聚合。`approvals.rs` 只在外层提案内推进审批子状态，内部确认不释放外部权限。

问题来源区分普通故障与错误报告的数据形状。Register、Authorize 和实际发送复核绑定记录身份与 revision，不判断日志是否活跃；错误报告以完整上下文去重，原始日志和结构化证据进入聚合及修复请求。`inspect` 采集目标条件，审批与独立验收仍由后续流程执行。

流程见[恢复参考](../../../docs/workflow.md)，提交前提见[调用契约](../../../docs/calling-contract.md)，开发规则见[AGENTS](AGENTS.md)。
