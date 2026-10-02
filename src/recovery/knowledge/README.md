# 修复经验与候选脚本

`recovery::knowledge` 提供纯逻辑的经验登记、可信验收校验、永久版本隔离及精确检索。`KnowledgeState` 不打开文件、不连接存储、不调用外部知识源；Host 负责读取完整可信历史、原子提交、索引与归档。领域关系见[恢复领域](../README.md)。

## 无脚本经验

`RepairExperience` 保存操作身份、目标、精确适用条件、关键词、可信结果和证据，以及模型提出的 `ExperienceReport`。经验不要求脚本；`Scriptability` 区分可脚本化、不适合和信息不足。Host 只能从受保护的恢复结果构造 `TrustedRepairExperience::attest`，再提交 `KnowledgeCommand::RecordExperience`；模型不能反序列化该权威断言。

`search_experiences` 精确匹配条件和关键词，按结果记录时间降序、ID 升序返回。失败和 Unknown 也作为明确标记的参考信息保留，不能当作成功修复方案；查询错误不同于空结果。候选脚本仅登记不可变版本，不进入 `search_reusable`，也不继承业务成功。旧案例与新经验共同占用 `max_records`；`snapshot.experiences` 完整导出经验，`projection.experiences` 返回数量。相同经验 ID 仅接受完全相同内容，重放使用同一校验。

## 源码导航

| 文件 | 职责 |
| --- | --- |
| [experience.rs](experience.rs) | 独立于脚本的经验、脚本化评估、可信结果断言与检索 |
| [contract.rs](contract.rs) | 候选、不可变脚本、案例、可信验收、输入命令与逻辑容量 |
| [state.rs](state.rs) | 纯状态迁移、提交提案、逐条历史恢复与隔离 |
| [validation.rs](validation.rs) | 有界输入、适用条件及可信验收关联校验 |
| [source.rs](source.rs) | 外部只读候选的来源、冲突、大小和适用性校验 |
| [query.rs](query.rs) | 不含脚本文本的查询摘要与稳定分页 |
| [mod.rs](mod.rs) | 公开领域入口 |

## 提案、提交与恢复

`KnowledgeState::new(KnowledgeConfig)` 建立空状态。`propose(commit_id, KnowledgeCommand)` 校验输入并返回 `Prepared<KnowledgeState>`，原状态保持不变。Host 保存命令及 `CommitRequest`，在同一原子操作中核对 `expected_revision`、去重提交 ID 并持久保存结果；只有可靠提交成功后，才能构造 `CommitReceipt::confirmed`，调用 `confirm` 并安装返回状态。提交请求同时绑定 `domain=knowledge`、先前完整历史的摘要、当前配置及命令内容。Host 必须在正确聚合范围比较版本；同提交 ID 携带不同领域或输入应拒绝，不能作为幂等成功返回，也不能重复释放同一提交的副作用。写入失败或写入结果不确定时，Host 必须保留原状态并核实，不得把队列接收或发送成功当作持久成功。提交接口见[共同操作](../../operation.rs)。

`KnowledgeCommand` 包含 `UpsertCandidate`、`RecordOutcome`、`Disable` 和 `ExpandCapacity`。候选 ID 和案例 ID 的完全相同重试不增加领域记录版本或案例数量；一次已确认提案仍递增聚合记录版本号，便于 Host 进行并发比较。容量、时序、版本冲突在提案阶段拒绝，不会部分修改输入状态。

`KnowledgeCommand` 支持序列化，可信验收断言只支持输出序列化；Host 可保存完整命令，不需从查询摘要推断事件。`snapshot` 导出可序列化的 `KnowledgeSnapshot`，包含聚合记录版本号、配置和全部记录、脚本、案例及禁用归属；该数据用于传输和查询，不能直接安装为权威状态。

`KnowledgeState::replay` 接受有序的 `KnowledgeReplayEntry { request, command, receipt }`，从空状态逐条执行相同提案与确认校验。记录版本跳跃、重复提交 ID、摘要或配置不一致、错误关联、脚本版本冲突和不合法案例均拒绝。摘要与结构共享约定见[领域维护](../../../docs/domain-maintenance.md#增量绑定与历史导出)。`KnowledgeState`、`KnowledgeCommand`、`CommitReceipt` 与可信验收断言没有直接反序列化为权威状态的入口；Host 需从受保护的已提交历史显式重建可信命令与确认。

历史完整性、存储事务、提交 ID 去重及正确聚合的版本比较由可信 Host 保证。Core 无法从被删减、重新编号且重新计算摘要的历史证明过去不存在隔离事实，也不校验存储真实性；Host 不得删除旧案例、失败、不确定、禁用或幂等事实后继续恢复。

## 候选、版本与案例

候选 ID 不可变：同 ID 只接受完全相同的候选。脚本 `(id, version)` 在知识状态内全局不可变，内容包括文本、语言、平台、前提与生成 Harness 及会话归属。`validate_script` 只校验有界内容和已知版本一致性，不保留新版本、不判断当前适用性、不授予执行许可。

脚本文本最多 32 KiB，语言标签接受 `powershell`、`sh`、`python`；标签不代表当前执行器支持。案例 ID 是全局幂等键，同 ID 携带不同结果或归属被拒绝；案例时间不能早于候选或该候选的最新案例时间。

记录状态为 `Candidate`、`Verified`、`Failed`、`Unknown`、`Disabled`。任一失败、Unknown 或禁用永久隔离同一脚本版本在全部候选中的自动检索；后来的成功可以留作历史，但不能解除隔离。`is_quarantined` 供执行前复核使用。新版本需重新验收。`Disable` 检查候选记录版本、操作员和原因，不撤销已经发出的许可。

`KnowledgeCandidate.reusable` 是可信流程计算的优先复用资格，缺失时为 false；它不代表审批或执行许可。独立故障轮次计算由[恢复编排](../workflow/README.md)负责。

## 可信业务验收

`RecordOutcome` 写入 `Verified` 必须提供 `TrustedBusinessVerification::attest` 构造的显式断言。断言绑定相同操作、目标、脚本版本、可信验证者、非空证据引用及验收时间，且验收时间不能晚于案例记录时间。失败或 Unknown 不得携带成功断言。

该类型没有 `Deserialize` 实现。Host 必须限制构造与命令提交入口，从独立业务检查获取断言，不能从模型回答、脚本退出码或文件读回推断成功。可反序列化的 `BusinessVerificationRecord` 和 `KnowledgeRecord` 只是查询数据，不能直接安装成 `KnowledgeState`。`attest` 不执行认证或验证证据真实性，Core 仅检查关联和领域规则。

## 精确检索与外部候选

`search` 返回已验证、未隔离且精确适用的经验；`search_reusable` 在限额前额外过滤 `reusable=true`。候选条件和脚本前提必须全部在查询中精确匹配，关键词使用大小写敏感的精确 AND 匹配。匹配结果按可信验收次数、最近可信验收时间、去重条件覆盖数降序排列，最后按候选 ID 升序稳定排序。条件及关键词各最多 32 项，一次返回最多 100 条。查询结果仍须经过当前适用性审核和一次许可。

Host 自行调用外部来源，再将响应交给 `validate_external_candidates(expected_source_id, query, proposals)`。校验要求固定来源身份、有界字段、查询条数上限和总计最多 4 MiB 的序列化输入；Host 另行限制外部响应读取分配。重复候选、身份或版本内容冲突、无效证据拒绝整个批次，不适用或已隔离版本被过滤。

外部结果在本地身份比较前强制 `reusable=false`，合并、去重并排序候选证据、来源证据及 `knowledge-source:<identity>` 标记。首次规范化登记后，相同 proposal、证据顺序变化及重复附加相同来源标记均可重复接入；真实候选内容、脚本或来源身份变化仍拒绝整个批次。输出按条件覆盖数和候选 ID 排序。结果只供诊断，不修改状态、不成为成功经验、不授予许可；外部返回字段无法构造可信验收。

## 逻辑容量与查询摘要

`KnowledgeConfig.max_records` 默认 1,024，有效范围为 1..=100,000；`max_cases_per_record` 默认 128，有效范围为 1..=1,024。这些是领域状态集合的内存预算，不是文件容量或自动清理政策。配置参与提交内容绑定，历史恢复必须从原配置开始，并按顺序重放扩容命令。达到上限拒绝新记录，完全相同的幂等重试仍可接受。Host 应在执行前安排经验交付与容量管理，不能删除安全事实绕过上限。

`ExpandCapacity { expected, target }` 仅接受与当前配置完全相等的 `expected`、有效范围内的 `target`，并要求两个容量均不下降且至少一个上升。扩容不改变候选记录版本、案例、脚本、幂等键、禁用或隔离事实；只在 Host 原子提交并确认后安装新配置，不产生执行效果。同一提交 ID 重复使用或在扩容后再次提交旧 `expected` 都返回冲突。提交结果未知时由 Host 查询原提交身份并重放可靠历史，不能用目标配置直接重放旧历史。容量达到合法最大值后仍需 Host 在执行前管理交付预算；本库不提供无限容量或删除事实的出口。

`projection` 返回记录、脚本、案例、隔离版本数量、状态计数及逻辑容量。`inspect` 按候选 ID 稳定分页，支持状态和隔离过滤，一页最多 100 条；摘要不包含脚本文本或完整验收证据。Core 不维护文件字节、文件锁、刷盘、日志格式、压缩、备份或外部知识源生命周期。

验证范围及测试归属见[集中测试](../../../tests/README.md)；纯逻辑测试不构成 Host 持久性或实际业务恢复验收。
