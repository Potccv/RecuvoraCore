# 恢复流程

## 从故障到修复

`SessionCommand::Register` 接收 `ProblemContext` 与匹配来源的可信证据。默认 `origin: incident` 要求 `IncidentEvidence.active = true` 且 `received = false`；`origin: error_log` 要求 `active = false` 且 `received = true`，表示调用方已可靠接收不可变错误报告，不声称目标当前不健康。两类证据不能互相替代，身份和 revision 均须绑定本次问题。

错误报告原文完整放在 `summary`，最多 8192 UTF-8 字节，不截断；`report` 必须保留 `source_id`、`generation`、`record_id`、正整数 `sequence`、原始 `age_ms` 及最多 4096 字节、嵌套深度最多 24 的对象 `evidence`。这些均为描述性证据，不是提供方路由或执行权限。错误报告可以是历史日志，其年龄不构成拒收或业务健康结论。普通故障的 `report` 必须为空。

按故障身份去重；错误报告重复身份必须与完整已保存 `ProblemContext` 一致，不能借重送更换原文、来源或证据。首次受理错误报告允许收件证据 revision 大于或等于问题原始 revision，以接纳期间发生的人工确认；当前故障首次受理仍要求严格相等。同目标存在非终态或 Unknown 时拒绝新任务，调用方须保留尚未受理的报告。`RecoveryEngine::advance` 仍获取当前观察，调用统一规划函数匹配经验并准备有界 Harness 委托；收到报告本身不产生执行、验收或完成事实。

`Start` 在一次提案中保存操作、创建审批并关联任务，进入 `AwaitingApproval`。政策必须允许 `repair_with_harness`，具体动作仍受目标允许范围限制。经验仅供参考，已知和未知故障都需要审批。

目标配置 `required_facts` 最多 31 项，为 Core 注入的保留条件 `fault_fingerprint` 留出一项；问题条件与目标条件合并后同样不得超过 32 项，且不能覆盖该保留条件。

## 审批与执行

审核支持 `Human`、`Harness` 和 `HumanThenHarness`。人工等待不重复调用观察；模型审核先提交带期限的尝试身份，再调用独立审核能力。人工决定、模型建议和升级都经过硬政策及记录版本检查。原审批期限不会因恢复或重试延长。

批准后重新取得当前观察与目标保护；`Authorize` 在同一次提案中消费审批、按问题来源验证当前故障或不可变报告收件、核验观察、提交执行授权，确认后返回一次 `ExecutionPermit`。实际发送前 `validate_dispatch` 继续执行同一来源证据校验。任何业务校验失败都丢弃整个提案，审批不会单独被消费。

具体动作通过 `PrepareAction` 保存完整内容、来源、版本与前提，消耗原任务预算。实际发送前还需复核当前保护和产物隔离。`Executed` 统一记录审批结果和任务回执；回执不匹配原操作、实际动作轨迹或停止事实时保守归为 Unknown。

## 验收与未知结果

执行回执只表明执行事实。`Verifying` 阶段调用独立验收能力；证据绑定原操作、目标、验收规则及时间。已停止且业务健康才可成为 Completed；失败和未知保持相应状态与动作隔离。

Unknown 不自动重新执行。`CheckResult` 接收独立执行检查和业务验收，在一次提案中完成证据保存、审批核实和任务结果计算。目标健康不能证明原操作执行过；NotExecuted 也不会恢复旧许可。

## 总结与交付

结果提交时同时建立稳定身份的 `ExperienceJob`；失败及 Unknown 的实际动作隔离已经成立。引擎通过独立只读总结能力生成经验和脚本化评估，模型不能改变执行或验收事实。候选产物不进入实际动作轨迹，也不自动晋升为可执行脚本。

每次最多处理四个总结工作，自动最多三次，之后可以显式重试。总结开始和返回分别提交，重启清除悬空调用身份并保留次数。`Deliver` 原子保存可信经验及完成交付，失败不重新执行修复；业务结果和待交付状态分别查询。

## 恢复

受保护历史通过 `RecoverySession::restore` 校验后需提交 Recover。Executing 转 Unknown；AwaitingApproval 转 Paused，显式 Resume 使用原操作和审批期限；已经记录回执的 Verifying 可继续独立验收。重放和恢复本身不释放许可或派发外部动作。

调用方提供可靠未派发证据时，可按正常 CheckResult 处理；没有证据不能推断未执行。接口和提交义务见[调用契约](calling-contract.md)。
