# Host 接入

`recuvora-core` 提供纯领域计算、提案和验证。可信 Host 负责持久保存、外部能力调用、调度、取消等待和目标所有权。本库只支持当前接口和当前历史协议；恢复配置使用 schema 2，不提供旧协议适配或旧历史导入。包版本不替代领域配置版本。

## 接口与职责

| 入口 | Host 对接职责 |
| --- | --- |
| `IncidentLedger` | 提供观察及当前时间，原子保存完整观察事务和故障历史 |
| `ApprovalLedger` | 保存完整操作与政策、调用审核、提交消费并接收一次许可 |
| `RecoveryState` | 提供故障、观察、目标所有权和执行事实，确认后处理修复、验收与总结效果 |
| `KnowledgeState` | 保存可信修复经验与命令历史，提供精确查询和不可变版本事实 |
| `CommitRequest`、`CommitReceipt` | 在正确聚合上比较版本并可靠持久提交，再确认完全相同的请求 |

公开顶层只有 `operation` 与 `recovery`，本库没有应用入口、运行时或存储服务。完整模块及领域 API 见[源码导航](../src/README.md)。

## Host 接入步骤

1. 建立各领域聚合的稳定存储身份、原始可信配置和完整已提交历史。初始状态由构造器创建，当前协议的历史经所属恢复入口重建；数据快照不能直接安装为权威状态。
2. 保存完整事件或命令、提交请求和记录版本，原子比较旧版本，拒绝重复提交身份的内容冲突。可靠成功后才构造 `CommitReceipt`；超时、断连或队列接收不能代替确认。
3. 使用 `StartRepair` 创建统一 Harness 请求；审批显式委托 `repair_with_harness`，目标配置的 `allowed_action_kinds` 限定会话内具体动作范围。有经验和无经验采用相同接入流程。
4. 按[跨域提交顺序](domain-maintenance.md#跨域提交顺序)对接审批关联、许可消费、流程授权、动作保存、执行结果和业务验收。同一目标的跨聚合、跨实例所有权必须覆盖当前故障复核与实际派发。
5. 实现独立只读总结调用，保存 `ExperienceReport`，由受保护结果构造可信经验并提交知识域，最后确认流程经验交付。失败只重试未完成部分，不重跑修复。
6. 针对提交冲突、未知提交结果、各提交边界进程中断、迟到回调、存储不可用和幂等经验交付进行 Host 集成验收。Core 内存测试不替代这些验证。

## 当前增量提交接口

提交请求绑定先前历史摘要和完整新输入；故障与审批条目携带 `prior_digest`。Host 把请求作为完整内容绑定保存，用 `latest_entry()` 增量落库，用 `entries()` 导出完整历史。记录版本、领域、身份或输入不一致的回执不能确认提案。具体契约见[增量绑定与历史导出](domain-maintenance.md#增量绑定与历史导出)。

Harness 审核须先提交 `BeginReview`，确认后取得尝试，再提交 `AssessAttempt`；迟到或没有已提交尝试的回调被拒绝。恢复与审核规则见[审批](approval.md)。知识容量通过 `ExpandCapacity` 单调扩展，保存原配置和完整命令链，不直接替换初始配置，见[知识接口](../src/recovery/knowledge/README.md#逻辑容量)。

## 能力与证据适配

`RepairArtifact` 的 `kind`、JSON `payload`、前提、版本和生成来源作为完整不可变内容保存。Core 不解释具体语言、平台或提供方格式；Host 检查实际执行器支持、当前前提及能力限制，并提交 `RepairActionPrepared` 后才发送动作。生成来源绑定原操作，执行回执 `RepairReceipt.execution_trace` 精确回传已提交内容。

`RepairExecutionOutcome` 只表达执行结果。独立 `BusinessVerification` 表达业务验收；Unknown 核实还需 `ExecutionResultCheck`，不能用当前业务健康补造执行事实。超时、停止、在途调用等待和关闭属于 Host，取消或断连本身不是停止证据。

`ExperienceJob.record()` 将实际动作放入 `RepairExperience.actions`；总结候选来源绑定总结调用，不能被解释为已经验证的可执行权限。只有可信 Host 可从受保护结果构造 `TrustedRepairExperience`；模型输出、查询快照和外部响应不能直接恢复权威领域状态。

## 当前历史与维护

重启使用当前协议的原配置和完整历史，按各域相同校验重建；随后提交审批恢复和流程 `Recover`，保留原操作、原期限、已消费许可、总结尝试和永久隔离。历史恢复不返回可重新派发的执行效果。

物理日志格式、备份、原子安装、目录保护与配置切换均属于 Host。配置或协议不匹配应拒绝恢复，不能删除安全事实、改编号或编造提交来绕过校验。Core 没有通用历史裁剪、快照安装或协议转换入口。维护义务见[领域维护](domain-maintenance.md#数据维护)，当前验证范围见[实现状态](implementation-status.md)。
