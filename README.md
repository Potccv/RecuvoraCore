# Recuvora Core

`recuvora-core` 是 Recuvora 的可信恢复决策库，维护故障、审批、执行结果、业务验收和修复经验的权威事实。它决定是否修复、采用什么方案、谁审核和是否获准，并在可信验收后发布或隔离知识案例。

## 阅读约定

- **恢复流程**：从故障诊断到审批、执行、验收和保存修复经验的完整过程，由 `RecoveryService` 提供接口，可信调用方负责推进。
- **Harness**：AI 服务接入。Host 选择并调用外部 AI 服务，Core 只保存参与诊断、审核和审计的逻辑身份。
- **知识库（knowledge）**：保存修复候选和经过可信验收的修复经验；外部候选只供诊断，不能产生执行权限。
- **接口约定**：规定可信调用方提供哪些能力、传入哪些参数以及返回什么证据；Core 校验证据后决定是否保存状态。
- **记录版本号（revision）**：记录每次修改后的版本。提交决定或恢复任务时须携带当前版本，防止使用过期记录操作。
- **未知执行结果（Unknown）**：无法确定外部动作是否完成，需要核实执行证据，不能直接重试。
- **查询摘要**：省略脚本文本或完整证据的只读查询结果；审批和授权仍须检查完整权威记录。

文档叙述使用上述名称，源码目录、Rust API、字段和命令使用实际名称。

## 使用方式

本项目是单个 Rust 库包，通过调用方的 Cargo 依赖使用，没有独立应用入口。公开顶层模块为 `operation` 与 `recovery`；恢复流程接口由 `recovery::workflow` 导出，使用 `RecoveryService`、`RecoveryConfig` 和 `RecoveryTask` 等类型。具体模块及服务接口见[源码导航](src/README.md)。包信息和依赖以 [Cargo.toml](Cargo.toml) 为准，当前包不发布到包仓库。

可信嵌入方负责认证、配置、调度与接口实现装配，并提供目标检查、AI 服务接入、执行和验收适配。Core 不运行脚本，不提供网络、HTTP、CLI 或 UI。接入职责见[架构](docs/architecture.md)，首次登记和推进任务的要求见[恢复流程](docs/recovery.md)。

## 能力与持久状态

Core 提供持久审批与一次许可、可恢复的修复流程、未知执行结果核实、跨实例目标所有权、只读知识候选、知识维护和运维查询。当前能力、策略限制及已验证范围由[实现状态](docs/implementation-status.md)统一记录。

Rust API、领域配置和持久日志各自有明确的校验要求。日志格式校验及离线维护规则见[领域维护](docs/domain-maintenance.md)；日志恢复不自动批准、派发或重放外部动作。

## 开发

先阅读[开发指南](docs/development.md)和[架构](docs/architecture.md)。贡献者和编码 Agent 遵循 [AGENTS.md](AGENTS.md) 及修改目录内的局部规范；检查入口是[开发检查脚本](scripts/README.md)。

完整文档导航见 [docs/README.md](docs/README.md)，测试范围和目标见[测试说明](tests/README.md)。
