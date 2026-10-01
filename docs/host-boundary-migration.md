# Host 接入迁移：0.1 到 0.2

`recuvora-core` 0.2 是破坏性的 Host API 变更。当前包只提供纯领域状态计算、提案和验证；持久保存、外部调用、调度、取消等待与目标所有权由可信 Host 实现。旧适配实现保留为项目外开发参考，不作为当前 Cargo 包编译内容，也不构成已经完成的 Host 集成。

## 接口替换

| 0.1 接入方式 | 0.2 接入方式 |
| --- | --- |
| `IncidentStore` 打开并写入本地日志 | `IncidentLedger` 计算观察或关注提案，Host 原子保存和确认 |
| `ApprovalStore` 同步审批日志后交付许可 | `ApprovalLedger` 返回审批提案，Host 确认后接收许可 |
| `KnowledgeStore` 保存候选、案例和检查点 | `KnowledgeState` 返回命令提案、查询和完整数据输出，Host 保存命令历史 |
| `RecoveryService` 编排存储和外部端口 | `RecoveryState` 计算事件迁移及确认后的 `RecoveryEffect` |
| `RepairBackend`、`KnowledgeSource` 异步端口 | Host 调用能力模块，把结果作为领域输入交回 Core |
| `IncidentGuard`、`TargetOwnership` 和文件所有权实现 | Host 保证当前故障版本与排他目标归属，提供 `IncidentEvidence`、`TargetAuthority` |
| `Cancellation`、`CallScope` 和服务关闭 | Host 管理计时、取消、在途调用、停止证据与关闭顺序 |
| Core 的文件日志、文件锁、物理压缩和日志迁移 | Host 存储适配及维护流程；Core 仅验证领域历史和不变量 |

公开顶层仍只有 `operation` 与 `recovery`，本库不新增应用入口、工作区子包或存储服务。完整源码与 API 导航见[模块入口](module-map.md)。

## Host 接入步骤

1. 建立各领域聚合的稳定存储身份、原始可信配置及完整已提交历史。初始状态由各域构造器创建，已有状态通过所属恢复入口重建。
2. 实现提案提交：保存完整事件或命令、提交请求及记录版本，原子比较旧版本，拒绝重复身份的内容冲突。只有可靠成功才构造 `CommitReceipt`；超时、断连或队列接收均不能代替确认。
3. 实现确认后副作用处理，把诊断、审核、实际执行和验收结果作为新输入返回。时间由 Host 显式提供，重试和取消必须保留原调用身份与停止证据。
4. 按[领域维护](domain-maintenance.md#跨域提交顺序)对接审批关联、两次执行授权提交、结果核实与经验交付。为同一目标实现跨聚合、跨实例的所有权，并保护当前故障版本复核边界。
5. 对提交冲突、未知提交结果、各提交边界的进程中断、迟到回调、存储不可用和经验重复交付进行 Host 集成验收。Core 的内存测试不替代这些验证。

查询快照可以传给存储或查询模块，但不是恢复所有权威状态的通用安装接口。尤其是知识快照不能直接变成成功验收或清除历史隔离；详细接口见[知识库说明](../src/recovery/knowledge/README.md)。

## 当前增量提交接口

当前提交请求采用先前历史摘要与完整新输入，故障和审批历史增加必需的 `prior_digest`。这与此前保存完整前状态的 0.2 开发请求不兼容，不能混合使用或用旧回执确认新提案。Host 应把请求作为不透明的内容绑定，保存 Core 生成的完整条目；增量落库用 `latest_entry()`，完整导出用现返回 `Vec<Entry>` 的 `entries()`。具体契约见[领域维护](domain-maintenance.md#增量绑定与历史导出)。

所有 Harness 回调切换为 `BeginReview` 提交确认后取得尝试，再提交 `AssessAttempt`。普通 `Assess` 在实时和普通历史迁移中均拒绝。完整旧审批历史只能通过 `ApprovalImport::validate` 与 `ApprovalLedger::prepare_import` 验证后导入；此入口按原政策、审核身份及期限验证历史 `Assess`，保留原 revision，不补造 `BeginReview` 或许可。

知识扩容使用 `ExpandCapacity` 命令，保存原配置以及扩容前后的完整命令链，不能直接替换初始配置。重复或未知提交的处理见[知识接口](../src/recovery/knowledge/README.md#逻辑容量与查询摘要)。

## 旧数据导入边界

Core 提供完整旧审批和恢复流程历史的纯导入接口，不读取旧 JSONL 或切换文件。Host 负责冻结来源、物理格式与完整性检查、原配置映射、知识命令重放及原子安装。Core 不接受单个任务终态快照替代完整历史。

导入必须保留故障轮次、完整原操作、审批和审核归属、一次消费事实、脚本版本、案例幂等键、失败隔离、Unknown 及独立结果核实证据。不能把已有执行意图转换成待执行的新动作，不能用健康状态填补缺失执行证据，也不能静默删除无法解释的记录。

Host 应在原存储冻结、在途状态已经核实的前提下转换数据，再通过 0.2 的领域迁移校验和专门导入测试验证。无法表达或证据不足的历史应阻塞切换并保留原数据，由可信维护流程处置。维护条件见[领域维护](domain-maintenance.md#数据维护)，当前实现与验证范围见[实现状态](implementation-status.md)。


### 领域导入顺序

1. Host 用 `ApprovalImport::validate` 校验完整 `LegacyApprovalEntry` 序列，经空审批聚合的 `prepare_import` 提案、可靠保存和确认，保留旧审核、原操作与消费事实。确认不产生审核或执行效果。
2. Host 将完整工作流记录映射为 `LegacyRecoveryRevision`，用原 `RecoveryConfig`、上一步审批聚合和已验证知识聚合调用 `RecoveryImport::validate`。格式 1 的缺失轮次从全部不同故障身份推导；格式 2 必须携带正确轮次。缺行、倒退、原操作或脚本变化、缺失审批、跨域结果和案例身份冲突均拒绝。
3. 若 `execution_uncertainties()` 返回证明，先用 `ApprovalLedger::prepare_legacy_uncertain` 保存封锁，再以更新后的审批重新验证工作流。旧 Executing/Unknown 与仍为 Approved 的审批可能处于消费结果不明窗口；封锁保留原操作与 revision，将审批置 Unknown，不虚构 Consume、不产生许可。未封锁时拒绝安装工作流。
4. 在空 `RecoveryState` 上 `prepare_import`，保存完整 `LegacyImported` 条目并确认。事件携带完整原历史及受保护的跨域校验证据；普通 `Event` 不接受它作为实时命令。`restore` 再次执行相同校验。导入无外部效果，激活新工作前仍须显式提交各域 Recover。
5. 未消费待审批保持原操作并进入 Paused，需显式 Resume；执行意图及未知结果保持 Unknown 和脚本隔离，不能自动重发。Publishing 保留原候选、案例 ID 和时间，并按已有证据归类为 Completed、Failed 或 Unknown；即使复用脚本失败后尚有诊断预算，也不在导入时自动续诊断。知识已经提交完全相同案例时不再交付，否则保留有序待交付。导入后的独立核实仍使用原案例身份，历史 Unknown 隔离永久保留。

导入要求全批验证与全批安装，不允许先激活一个领域再补齐其余领域。Host 文件导入、进程中断及所有权切换验证由 Host 项目维护；本库测试只证明上述纯领域规则。
