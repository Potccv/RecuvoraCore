# Recuvora Core

`recuvora-core` 是恢复领域的纯逻辑 Rust 库，匹配修复经验、生成统一 Harness 修复请求，并校验动作、执行结果、独立业务验收及经验总结。所有计算以显式传入的事实、政策、时间与证据为依据，返回待提交变更；本库不执行外部操作。

## 阅读约定

- **恢复流程**：故障登记、经验匹配、受授权的 Harness 修复、独立业务验收及经验总结交付的领域状态机，由 `RecoveryState` 计算。
- **Harness**：参与修复或总结的 AI 能力逻辑身份；Core 生成请求并校验委托与返回证据，不包含服务接入实现。
- **记录版本号（revision）**：每次提交的版本。调用方原子比较预期版本并持久提交，过期输入不能覆盖新事实。
- **待提交变更**：`Prepared` 中的候选状态和提交请求；调用方确认提交后才安装状态并取得后续操作意图。
- **未知执行结果（Unknown）**：不能确定执行或业务恢复结论；独立核实证据之前不得重放动作。
- **修复经验**：带结果、证据、实际动作与总结的 `RepairExperience`；可选脚本化候选不继承业务成功作为执行或验收事实。
- **动作产物**：`RepairArtifact` 保存动作种类、JSON 载荷、前提、版本和生成来源。Core 校验领域绑定，具体能力解释由调用方负责。

## 使用方式

本项目保持单个库包，公开顶层模块只有 `operation` 与 `recovery`，没有应用入口、文件存储、网络客户端、系统时钟或异步运行时。主要入口为 `ApprovalLedger`、`IncidentLedger`、`KnowledgeState` 和 `RecoveryState`，见[源码导航](src/README.md)。

调用方加载完整已提交领域历史，调用 Core 准备变更，以提交请求中的预期版本执行原子持久提交，再用 `CommitReceipt` 确认并安装新状态。提交回执是可信调用方的断言，不能由模型或界面构造；具体边界见[架构](docs/architecture.md)。

恢复配置使用 `schema_version = 2`，只支持当前协议。`RecoveryCommand::StartRepair` 为有经验和无经验的故障生成同一种请求，经验总结和脚本化评估独立于业务终态。使用前提见[调用契约](docs/calling-contract.md)，详细行为见[恢复流程](docs/recovery.md)。

## 能力与验证

Core 保留硬政策、审核身份和期限、一次执行许可、同目标互斥、Unknown 核实、精确经验检索及永久动作版本隔离。历史重放不返回执行许可或外部操作；经验总结与交付失败不重跑修复。当前验证与边界见[实现状态](docs/implementation-status.md)。

开发遵循 [AGENTS](AGENTS.md) 和[开发指南](docs/development.md)。完整入口见[文档导航](docs/README.md)。
