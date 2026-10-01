# 审批内部实现

`recovery::approval` 保存审核决定、一次执行意图和执行结果。调用方是可信 Host 代码；模型回答只提供评审证据。身份认证、定时器、外部 Harness（AI 服务接入）调用和真实动作执行由 Host 负责。领域关系见 [恢复领域](../README.md)。

## 源码导航

父层 [approval.rs](../approval.rs) 导出接口约定，保留 `ApprovalStore`、`ExecutionPermit` 的私有字段、日志事件、当前政策检查与统一变更入口。内部文件属于同一个权威服务，不能单独装配成第二个审批提供者。

| 文件 | 职责 |
| --- | --- |
| [contract.rs](contract.rs) | 政策、完整操作、评审证据、记录、状态与错误接口约定及输入校验 |
| [requests.rs](requests.rs) | 请求幂等、事实查询、持久转交与审核尝试关联、人工决定、撤销、取消和人工升级 |
| [execution.rs](execution.rs) | 同步意图后的许可交付、许可身份核验、执行结果和 Unknown 执行结果核实 |
| [storage.rs](storage.rs) | 单写锁、日志恢复、同步追加及重启时执行记录转为 Unknown |
| [transitions.rs](transitions.rs) | 实时操作与日志重放共用的纯状态迁移校验 |
| [paths.rs](paths.rs) | 运行目录、普通文件、链接与硬链接检查 |

## 政策与请求

`ApprovalPolicy` 绑定政策身份、版本、审核模式、委派说明、目标与动作类型白名单和总有效期。目标和 `action.kind` 精确匹配；人工与 Harness 决定都必须经过同一硬政策检查，审核批准不能扩大白名单。

`request` 以 `(task_id, operation_id)` 幂等登记完整 `ProposedOperation` 和政策。只有操作及政策完全相同的重试返回已有请求，不能替换目标、动作、任务记录版本号或政策。后续审核及许可消费核对当前政策与持久政策一致，并检查到期和时间顺序；政策变化需要新的请求流程。

## 审核模式与时限

`ReviewerConfig` 支持 `Human`、`Harness { harness_id }` 和 `HumanThenHarness { harness_id, human_wait_secs, review_timeout_secs }`。人工优先的等待时限从请求创建时计算，Harness 单次审核时限从持久转交时计算。`ApprovalPolicy::ttl_secs` 限制整个请求，转交不会延长它；人工等待与审核预算之和必须小于总有效期，实际审核截止被总有效期截断。

`begin_harness_review` 核对当前政策、记录版本号、人工截止时间及阶段，同步转交记录后返回 `ReviewAttempt`，调用方再派发外部审核。审核尝试绑定请求、记录版本号、尝试序号、指定 Harness 和截止时间；模型仅返回 `ModelAssessment`，审核身份与关联字段由可信调用方提供。`assess_attempt` 接受绑定结果，`fail_review_attempt` 记录失败，`expire_review` 处理到期；迟到、过期身份或记录版本号不匹配的结果不能授权。

`ReviewStage` 与授权状态分离。人工优先从 `WaitingHuman` 转入 `ReviewingHarness`；失败、超时或升级进入 `NeedsHuman`，不自动重复 Harness 审核。人工明确拒绝为终态，不能触发回退。

`decide_human_at_revision` 可接管正在审核的请求，记录版本号防止过期页面或并发回调覆盖新决定。人工决定、撤销、取消或其他记录版本号变化使先前模型回执失效。`AssessmentSource::Human` 的操作人标记仅是审计归属，Host 必须先认证调用者。`assess` 只适用于 `Harness` 尚未启动跟踪审核尝试的路径，不能绕过人工优先策略或覆盖跟踪审核。

## 一次许可与执行结果

`consume` 核对获批状态、当前政策、有效期和完整操作，在同步执行意图后才返回 `ExecutionPermit`。同一审批存储内，目标已有 `Executing` 或 `Unknown` 时拒绝新的许可消费。跨存储的目标权威由 [恢复流程](../workflow/service/README.md) 管理。

`ExecutionPermit` 的字段私有，不可克隆、不可反序列化。`complete` 消费许可并核对原存储实例、请求记录版本号和操作身份，记录 `Executed`、`Failed` 或 `Unknown`。调用方仍须在实际执行前复核目标当前状态；执行完成不能代替独立业务验收。

执行期间取消或撤销转为 `Unknown`，保持目标阻塞并拒绝迟到完成。`reconcile_unknown` 只在可信调用方独立核对目标并停止原执行者后记录明确终态，不重新发放许可，不重放外部动作。

## 存储配置

`ApprovalStore::open(data_dir, config, now)` 接受源码外运行目录和可信配置。`ApprovalStoreConfig` 支持序列化与反序列化，缺失字段使用默认值、未知字段被拒绝；`validate` 与 `open` 使用相同容量校验。恢复流程的配置传入方式见 [恢复流程](../workflow/service/README.md)。

| 字段 | 默认值 | 有效范围 |
| --- | --- | --- |
| `max_requests` | 10,000 | 1..=100,000 个历史请求 |
| `max_journal_bytes` | 32 MiB | 1..=128 MiB |

单条 JSONL 最多 256 KiB。容量耗尽拒绝事务，执行意图不能在持久化失败后交付许可。

## 持久恢复与生命周期

实时操作和日志回放使用相同纯迁移检查；日志同步成功后才安装内存状态。写失败停用存储，完整损坏、末尾残片、序号或迁移错误拒绝恢复，不截断日志。写入与领域内部健康检查核对日志及锁文件身份、链接和长度，拒绝替换或变动后的存储。

存储持有排他写锁，释放实例后才能重新打开。路径保护依赖可信本机身份与目录权限，不能隔离恶意同账号或同进程代码。读取记录仅提供只读查询结果，不能跳过后续政策、记录版本号、许可和存储健康核验。

重启保留人工等待的原截止时间；中断的 Harness 审核转为人工介入，不重新派发或接受原审核尝试；`Executing` 恢复为 `Unknown`。审批决定的恢复本身不会触发外部执行。

审批日志使用格式 1，读取时严格校验转交事件和状态迁移，未知事件拒绝恢复。审批与恢复流程的关联和恢复职责见 [审批集成说明](../../../docs/approval.md)，测试映射见 [集中测试](../../../tests/README.md)。
