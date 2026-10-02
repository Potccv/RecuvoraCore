# 调用契约

本页说明 `recuvora-core` 公共 API 的使用前提。所有入口都进行确定性领域计算；输入必须来自可信调用方，返回的提案不代表已经提交或执行。模块导航见[源码入口](../src/README.md)。

## 输入前提

| 输入 | 契约 |
| --- | --- |
| 配置与身份 | 原始可信配置、稳定聚合身份和逻辑目标；恢复配置使用 schema 2 |
| 时间与观察 | 显式时间、当前故障及目标事实；调用方保证来源可信及提交期间条件有效 |
| 历史 | 完整有序的已提交事件或命令、原配置、提交请求及版本；查询快照不能直接安装为权威状态 |
| 提交回执 | 已完成原子版本比较和可靠保存的确认，须绑定完全相同的请求 |
| 执行与验收 | 绑定原操作及目标的独立证据，执行结果与业务健康分别表达 |

## 准备与确认

`prepare` 或 `propose` 返回 `Prepared`，不修改原状态。调用方依据 `CommitRequest.expected_revision` 原子比较版本并保存完整变更；可靠成功后，以 `CommitReceipt::confirmed` 确认完全相同的请求，再安装返回状态并处理效果。提交结果未知时不能确认，也不能重复释放许可。

`latest_entry()` 提供本次新增历史的只读访问，`entries()` 导出完整历史。请求绑定先前历史摘要和完整新输入；故障及审批条目携带 `prior_digest`。具体绑定见[领域维护](domain-maintenance.md#增量绑定与历史导出)。

## 修复请求与经验

`StartRepair` 为有经验和无经验的故障生成同一种 `HarnessRepairRequest`。审批政策须允许 `repair_with_harness`，`TargetBinding.allowed_action_kinds` 限定具体动作。稳定条件由 `ProblemContext.conditions` 与 `TargetBinding.required_facts` 合并并加入故障指纹；影响适用性的版本等条件须显式提供，额外动态观察只作为证据保留。

`matched_experience_count` 表示预算筛选前的完整命中数，`experiences` 表示实际附带项，空列表不等于没有命中。请求限额与选择规则见[恢复流程](recovery.md#统一-harness-修复)。

`RepairActionPrepared` 保存完整动作，回执 `RepairReceipt.execution_trace` 必须精确绑定该内容。`RepairExecutionOutcome` 与 `BusinessVerification` 分别表达执行和业务验收；Unknown 核实另需 `ExecutionResultCheck`，不能根据健康状态补造执行事实。

`ExperienceJob.record()` 从已保存任务构造完整经验，实际动作与总结候选分别表达。`TrustedRepairExperience::attest` 校验经验格式和关联，不认证证据来源，也不把候选变成执行许可。跨域确认顺序见[领域维护](domain-maintenance.md#跨域提交顺序)。

## 历史恢复与限制

所属领域的恢复入口使用原配置和完整历史重建状态，不产生执行效果。审批恢复需 `prepare_recovery`，恢复流程需 `Recover`；原操作、审批期限、已消耗预算与隔离保留。完整约束见[领域维护](domain-maintenance.md)。

本库不提供物理存储、历史裁剪、快照直装、协议转换或运行生命周期接口。接口校验与内存测试不能证明外部提交、互斥或执行已完成；当前验证范围见[实现状态](implementation-status.md)。
