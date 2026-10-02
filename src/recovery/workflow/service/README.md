# 恢复状态机实现

`RecoveryState` 为一个逻辑目标计算恢复任务，不持有文件、时钟、后端、所有权锁或异步任务。公开入口由 `recovery::workflow` 导出。

| 文件 | 职责 |
| --- | --- |
| [contract.rs](contract.rs) | 配置、任务、方案、执行与独立业务验收证据 |
| [engine.rs](engine.rs) | 命令、历史事件、提交提案、流程迁移与经验待交付记录 |
| [experience.rs](experience.rs) | 统一修复请求、独立经验任务及结果转换 |
| [query.rs](query.rs) | `RecoveryTaskSummary` 与 `RecoveryNextStep` 结构化进度 |

## 统一修复入口

`RecoveryCommand::StartRepair` 根据当前知识生成带可选经验的 `HarnessRepairRequest`，无论是否命中经验都采用同一受授权 Harness 会话。请求政策显式允许 `repair_with_harness`，执行前不要求 `RepairPlan`。`RepairActionPrepared` 在发送前保存会话内具体动作，最多一次；`repair_action(operation_id)` 查询已提交动作，不产生许可，结果的 `execution_trace` 保留其内容及版本。

`ExperienceJob` 在业务结果提交时创建；`BeginExperience` 确认后产生只读总结效果，`ExperienceSummarized` 保存总结及可选候选，`ExperienceFailed` 保留重试状态。`pending_experiences()` 返回未交付工作，`ExperienceDelivered` 只在 Host 保存经验后确认。完整流程与旧脚本兼容路径见[恢复流程](../../../../docs/recovery.md)。

## 使用与提交

`new(config)` 创建空状态；`prepare(commit_id, command, now_ms, knowledge)` 返回 `Prepared<RecoveryState, RecoveryEffect>`。准备不修改原状态，`state()` 只读访问拟提交状态；Host 持久保存新增 `RecoveryEntry` 及提交身份，原子比较聚合版本后调用 `confirm`，再安装结果并处理返回效果。请求绑定配置、先前完整领域历史摘要、完整事件与时间，内部集合与历史使用结构共享。

`RecoveryCommand::Event` 输入普通领域事件。实际执行使用 `RecoveryCommand::AuthorizeExecution`，要求持有审批域确认消费后返回的 `ExecutionPermit`；相应 `RecoveryEffect::Execute` 在流程提交确认后交还该一次许可。直接提交 `ExecutionAuthorized` 事件不能获取执行资格。Host 必须维持目标所有权及当前故障条件，不能把 `TargetAuthority` 数据结构当成分布式锁。

Host 执行诊断、审批接入、动作和验收；返回事件必须绑定提交后的任务版本、调用或操作身份。诊断效果携带超时和工具预算，Host 负责真正实施这些限制。观察、验收和结果核实证据接受最多 30 秒的新鲜度窗口；Unix 毫秒时间由可信调用方显式传入。

## 恢复与查询

`latest_entry()` 只读访问最新条目，`entries()` 复制导出完整 `Vec<RecoveryEntry>`；`restore(config, entries)` 重复实时验证并拒绝顺序、身份、内容或迁移不一致，不返回任何执行效果。恢复后 `recovery_required()` 为 true，必须先提交 `Recover`。审批等待转为 `Paused` 并要求显式 `Resume`；中断执行转 Unknown 并保存版本隔离；诊断次数不退还，旧回调失效。Host 在恢复或取消前处理旧调用的停止与资源归属。历史摘要及增量持久化约定见[领域维护](../../../../docs/domain-maintenance.md#增量绑定与历史导出)。

`task`、`tasks` 返回只读领域状态；`RecoveryTaskSummary::from` 给出阶段、下一步、版本及原因，不产生授权。任务登记区分独立故障轮次与异常样本数，未结束任务阻塞同目标新任务，重复故障身份返回原轮次。

## 经验交付

业务成功或明确失败后即确定任务结果，同时在同一提案中建立稳定 `KnowledgeDelivery`。失败和 Unknown 的脚本版本同步进入流程本地隔离集合，知识模块暂时不可用也不能再次执行该版本。

`pending_deliveries()` 返回尚未确认的经验交付数据，Host 可幂等重试；知识模块确认提交后才输入 `DeliveryConfirmed`。交付重试不重新诊断或执行。历史 Unknown 的隔离在后来验收成功后仍保留。交付顺序、重放与跨域恢复细节见[恢复流程](../../../../docs/recovery.md)和[领域维护](../../../../docs/domain-maintenance.md)。

规则见[AGENTS](AGENTS.md)。


[legacy.rs](legacy.rs) 校验完整旧任务迁移链和跨域关联，构造无副作用导入提案及不确定原审批封锁证明；不执行日志读取、文件安装或历史快照直装。实现使用共享记录映射以限制大记录插入的栈占用。
