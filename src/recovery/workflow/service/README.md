# 持久恢复流程

`RecoveryService` 组合故障、审批与知识库，为一个配置目标维护恢复决策和持久任务。可信 Host 主动推进流程并实现实际检查、诊断、审核、执行和验收端口；本目录不装配这些服务。

| 文件 | 职责 |
| --- | --- |
| [contract.rs](contract.rs) | 领域配置、任务、方案、执行/验收证据及 `RepairBackend` |
| [engine.rs](engine.rs) | 决策状态机与跨域持久提交 |
| [incident_gate.rs](incident_gate.rs) | 当前权威故障的同步复查接口 |
| [ownership.rs](ownership.rs) | 规范目标身份、所有权端口与本地持久提供方 |
| [storage.rs](storage.rs) | 有界单写入者任务日志及重放校验 |
| [storage_paths.rs](storage_paths.rs) | 日志路径与打开文件身份检查 |
| [migration.rs](migration.rs) | 纯字节的完整历史校验与显式迁移 |
| [query.rs](query.rs) | 不含方案或脚本文本的有界查询与容量概览 |

## 调用方与配置

`open` 和 `open_with_clock` 使用默认审批与知识库容量；`open_with_store_configs` 和 `open_with_clock_and_store_configs` 接受显式 `ApprovalStoreConfig` 与 `KnowledgeStoreConfig`，时钟版本还接受可信 `RecoveryClock`。这些入口都校验 `RecoveryConfig`，状态目录须位于源码外。容量字段由[审批域](../../approval/README.md)和[知识库域](../../knowledge/README.md)维护，恢复流程配置迁移见[领域维护](../../../../docs/domain-maintenance.md)。

打开存储只启用恢复与查询。调用方还须绑定目标所有权和权威故障端口；共享规范目标的所有权不能因状态目录不同或异常退出而被绕过。端口绑定、独立故障轮次、日志格式与离线维护规则见[领域维护](../../../../docs/domain-maintenance.md)。

恢复流程阶段不授予权限：审核、一次许可、执行结果和业务验收分别由其权威记录决定。审批恢复不自动派发，未知执行结果（`Unknown`）不自动重放；修复经验交付失败仅重试交付。重新诊断只适用于流程允许的复用失败，不是所有明确失败的通用重试。具体阶段、审批关联恢复、显式恢复及失败处理见[恢复流程](../../../../docs/recovery.md)。

`bind_knowledge_source` 可绑定一个 Host 聚合后的知识库来源，并在绑定时固定其逻辑身份；同一运行期只允许绑定一次，重开恢复流程后重新绑定。来源返回错误或候选批次被拒绝时，流程保存有界原因并继续本地诊断；取消仍按调用取消处理。外部修复经验只参与诊断，来源与候选校验由[知识库域](../../knowledge/README.md)定义。

实际服务路由、脚本执行和业务验收属于可信嵌入方，验证范围见[实现状态](../../../../docs/implementation-status.md)。开发规则见[恢复流程规范](AGENTS.md)。

## 查询摘要

`inspect_tasks(&RecoveryQuery)` 按任务 ID 升序分页，`after_id` 返回严格晚于该 ID 的任务，一页为 1..=100 条。阶段过滤允许空集合或最多 12 个不重复阶段。`RecoveryTaskSummary` 返回任务/故障/目标身份、记录版本号（`revision`）、阶段、异常观察样本数与独立故障轮次数、诊断次数、审批/操作引用和时间，不包含方案、脚本文本或完整证据。

`overview` 核验任务日志仍对应已持有的文件身份与长度后，返回任务数量与限额、日志字节与限额、各阶段数量和 `Unknown` 审批数量。查询只返回领域记录的只读查询结果，不改变审批、许可、执行或验收事实；调用方的认证与展示转换由 Host 负责。

## 公开接口与持久记录

公开入口为 `recuvora_core::recovery::workflow`，导出 `RecoveryService`、`RecoveryConfig`、`RecoveryTask`、`RecoveryStage`、`RecoveryError` 及查询和日志维护接口。`RecoveryClock` 接受可信时钟实现，`SystemRecoveryClock` 提供系统时钟，`RecoveryFuture` 表达异步业务调用。

`RecoveryService::check_result` 核实未知执行结果，分别接受绑定原操作的 `ExecutionResultCheck` 和独立 `BusinessVerification`。执行证据包含操作、目标、执行器、执行者停止状态、证据引用与 `checked_at_ms` 时间；`CheckedExecution` 表达核实结论，不能根据目标当前健康状态推断执行结果或续发许可。

`RecoveryTask.result_check` 保存已接受的 `ResultCheckRecord`，包含独立执行证据和可信调用方的审计归属。`ExecutionResultCheck.checked_at_ms` 保存核实证据的 Unix 毫秒时间。字段和持久 JSON 均按当前数据结构校验，未知字段拒绝读取。

可信所有权实现必须提供 `TargetLease::recovery_directory`，返回绑定的规范存储目录，并保持独占、校验和显式释放语义。任务存储使用 `recovery.jsonl` 和 `recovery.lock`；目标所有权绑定存储目录及锁文件对象身份。发现不支持的存储文件时，拒绝打开任务存储或取得目标所有权，不创建新的任务日志或锁文件。日志格式、配置版本和恢复约束见[领域维护](../../../../docs/domain-maintenance.md)。
