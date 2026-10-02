# Recuvora Core

`recuvora-core` 是恢复领域的纯逻辑 Rust 库，匹配故障经验、生成统一 Harness 修复请求，并校验修复结果、经验总结与可选脚本候选。可信 Host 提供当前状态、政策、时间与证据，负责存储、原子提交、调度、外部能力和运行生命周期。

## 阅读约定

- **恢复流程**：故障登记、经验匹配、受授权的 Harness 修复、独立业务验收及经验总结交付的领域状态机，由 `RecoveryState` 计算。
- **Harness**：AI 服务接入；Host 调用具体服务，Core 校验其逻辑身份和返回证据。
- **记录版本号（revision）**：每次提交的版本。Host 必须原子比较预期版本并持久提交，过期输入不能覆盖新事实。
- **待提交变更**：`Prepared` 中的候选状态和提交请求；Host 确认提交后才安装状态并取得后续操作意图。
- **未知执行结果（Unknown）**：不能确定执行或业务恢复结论；独立核实证据之前不得重放动作。
- **修复经验**：带结果与证据的修复发现和经验；脚本是可选候选，修复成功不证明候选脚本已验证；经验交付状态与任务业务结果分别记录。

## 使用方式

本项目保持单个库包，公开顶层模块只有 `operation` 与 `recovery`，没有应用入口、文件存储、网络客户端、系统时钟或异步运行时。主要入口为 `ApprovalLedger`、`IncidentLedger`、`KnowledgeState` 和 `RecoveryState`，见[源码导航](src/README.md)。

典型接入顺序是：Host 加载已提交领域状态，调用 Core 准备变更，以提交请求中的预期版本执行原子持久提交，再用 `CommitReceipt` 确认并安装新状态。`CommitReceipt` 是可信 Host 的断言，不能由模型或界面构造；具体边界见[架构](docs/architecture.md)。

公开 API 及状态结构的接入要求见[Host 边界迁移](docs/host-boundary-migration.md)。本库不直接读取旧文件日志，也不自动适配已有 Host 调用。

新流程通过 `RecoveryCommand::StartRepair` 为有经验和无经验的故障生成同一种请求。经验总结与脚本化评估独立于业务终态；旧脚本任务仍可按原审批与历史继续恢复。详细接口见[恢复流程](docs/recovery.md)。

## 能力与验证

Core 保留硬政策、审核身份和期限、一次执行许可、故障轮次、同目标流程互斥、Unknown 核实、精确知识检索及永久版本隔离。重放历史不会返回执行许可或外部操作；经验交付失败不会重跑修复。具体流程见[恢复流程](docs/recovery.md)，当前验证及限制见[实现状态](docs/implementation-status.md)。

提交使用增量历史绑定，Harness 审核必须关联已提交尝试，知识容量支持受校验扩容，外部候选支持确定性重复接入。CORE-001 至 CORE-004 的处理状态和规模验证见[已知问题](docs/implementation-status.md#已知问题)。

开发遵循 [AGENTS](AGENTS.md) 和[开发指南](docs/development.md)。完整入口见[文档导航](docs/README.md)。
