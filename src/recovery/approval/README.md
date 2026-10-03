# 审批与许可

`ApprovalLedger` 计算审批迁移；contract 定义政策、操作、记录和审核身份；ledger 负责提案和重放；transitions 检查硬政策、当前版本、原期限、独立审核和一次消费。`ExecutionPermit` 不可复制、不可反序列化。完整恢复使用 RecoverySession 合并审批和任务提交，低层接口支持独立审批用途。

流程见[恢复参考](../../../docs/workflow.md)，提交前提见[调用契约](../../../docs/calling-contract.md)，开发规则见[AGENTS](AGENTS.md)。
