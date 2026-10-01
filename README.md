# Recuvora Core

`recuvora-core` 是恢复领域的纯逻辑 Rust 库，计算故障、审批、恢复流程和修复经验的合法状态迁移。可信 Host 提供当前状态、政策、时间与证据，负责存储、原子提交、调度、外部能力和运行生命周期。

## 阅读约定

- **恢复流程**：故障登记、方案选择、审批、执行结果核实、独立业务验收及经验交付的领域状态机，由 `RecoveryState` 计算。
- **Harness**：AI 服务接入；Host 调用具体服务，Core 校验其逻辑身份和返回证据。
- **记录版本号（revision）**：每次提交的版本。Host 必须原子比较预期版本并持久提交，过期输入不能覆盖新事实。
- **待提交变更**：`Prepared` 中的候选状态和提交请求；Host 确认提交后才安装状态并取得后续操作意图。
- **未知执行结果（Unknown）**：不能确定执行或业务恢复结论；独立核实证据之前不得重放动作。
- **修复经验**：经过规则校验的方案、脚本版本、案例和验收事实；经验交付状态与任务业务结果分别记录。

## 使用方式

本项目保持单个库包，公开顶层模块只有 `operation` 与 `recovery`，没有应用入口、文件存储、网络客户端、系统时钟或异步运行时。主要入口为 `ApprovalLedger`、`IncidentLedger`、`KnowledgeState` 和 `RecoveryState`，见[源码导航](src/README.md)。

典型接入顺序是：Host 加载已提交领域状态，调用 Core 准备变更，以提交请求中的预期版本执行原子持久提交，再用 `CommitReceipt` 确认并安装新状态。`CommitReceipt` 是可信 Host 的断言，不能由模型或界面构造；具体边界见[架构](docs/architecture.md)。

公开 API 及状态结构的接入要求见[Host 边界迁移](docs/host-boundary-migration.md)。本库不直接读取旧文件日志，也不自动适配已有 Host 调用。

## 能力与验证

Core 保留硬政策、审核身份和期限、一次执行许可、故障轮次、同目标流程互斥、Unknown 核实、精确知识检索及永久版本隔离。重放历史不会返回执行许可或外部操作；经验交付失败不会重跑修复。具体流程见[恢复流程](docs/recovery.md)，当前验证及限制见[实现状态](docs/implementation-status.md)。

开发遵循 [AGENTS](AGENTS.md) 和[开发指南](docs/development.md)。完整入口见[文档导航](docs/README.md)。
