# 恢复流程

`RecoveryState` 计算流程，Host 加载和提交状态并执行操作意图。接口见[状态机实现](../src/recovery/workflow/service/README.md)，提交职责见[架构](architecture.md)。

## 正常流程

1. `Register` 根据当前活动故障登记任务。同一故障身份去重，同目标已有未结束任务拒绝新登记；独立故障轮次用于复用阈值。
2. Host 检查环境并提供 `SelectPlan`。有效的本地已验证候选进入审批；无候选时，在提交中记录诊断次数和调用身份，确认后返回 `Diagnose`。
3. `DiagnosisCompleted` 绑定效果中的任务版本和调用身份，校验脚本、环境和生成 Harness，提交完整 `ProposedOperation` 后输出 `RequestApproval`。
4. Host 经审批域提交原操作申请，再用 `ApprovalAttached` 保存关联。人工或 Harness 决定均经审批域硬政策和期限校验；Host 负责审核转交时限与实际调用。
5. Host 持有目标所有权和当前故障复核边界，经审批域消费一次许可，再以 `AuthorizeExecution` 准备流程派发。Host 提交确认后获得 `Execute`，实际执行一次，不能缓存或重放该效果。
6. Host 先提交审批执行结果，再提交匹配的 `ExecutionRecorded`。明确执行成功产生 `Verify`；独立业务验收成功才得到 `Completed`。
7. 成功、失败或 Unknown 同时生成稳定经验交付记录。明确成功/失败可以结束业务任务；经验保存通过 `pending_deliveries` 单独推进，`DeliveryConfirmed` 只确认交付。

每一步的 `Prepared` 都必须经过 Host 可靠提交后才能确认。经验交付暂时失败不会阻塞已经完成任务的新故障；Unknown 仍阻塞目标，失败版本隔离不依赖经验交付成功。

## 审批关联恢复

方案、观察、稳定操作和诊断预算先于审批申请保存。Host 重试原操作申请时不得更换身份或续期；审批域按操作与政策幂等。审批关联中断后，恢复流程先进入 Paused，`Resume` 返回原操作的申请意图，不重新诊断。已经消费许可的关联输入保留 Unknown 和版本隔离，不自动执行。

## 重启与取消

`restore` 只验证并重建历史，随后必须提交 `Recover`，才能进行其他状态变更。等待审批进入 Paused，需要当前任务版本显式 Resume；中断执行进入 Unknown 并保存隔离事实；中断诊断的次数保留，旧调用身份失效。审批域也需要独立的 `prepare_recovery`，中断审核转人工处理，不能接受旧审核回执。

Host 在恢复、取消或重试前负责协调仍在运行的外部调用，取消并等待其清理。没有进程监督或执行证据时，取消和网络断连不证明原执行者已停止。未派发任务取消时须先取消已关联审批；已经派发的任务必须核实结果。

## 重新诊断与结果核实

Harness 判定复用方案不适用时，可以在原预算内生成替代方案；人工拒绝始终终态。复用动作明确失败或业务验收失败，且执行者确认停止后，可以重新诊断并申请新审批。新诊断方案执行失败进入 Failed。

`ResultChecked` 分别接受绑定原操作的 `ExecutionResultCheck` 与 `BusinessVerification`，不能根据健康状态推断动作执行。审批仍为 Unknown 时，首先把独立证据准备并提交到流程中，任务保持 Unknown；Host 再按相同证据提交审批域 `Reconcile`，最后用当前任务版本再次提交 `ResultChecked` 完成流程。任一步中断均保留原事实、许可消费和隔离，不能重放动作。

已确认未执行进入 Canceled，原许可仍保持已消费。已确认执行但业务验收未知继续 Unknown，可提供新的只读验收证据；后续成功保留历史 Unknown 的版本隔离。

## 经验交付与维护

经验候选和案例身份固定，Host 按顺序交付同一候选的历史结果，保留先前失败、Unknown 与隔离。仅在知识域明确提交后确认交付；知识容量错误不会改变任务执行结果，也不得通过删除案例或更换幂等键绕过。配置、历史导出和维护规则见[领域维护](domain-maintenance.md)。
