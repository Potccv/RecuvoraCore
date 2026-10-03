# RecuvoraCore

RecuvoraCore 是恢复业务引擎，包名 `recuvora-core`，版本 0.2.0。它接收故障及错误报告，不判断日志是否活跃，直接匹配经验、推进审批与 Harness 修复、判定独立验收结果，并生成和交付修复经验。

## 功能

- 已知与未知故障统一生成有界修复请求，命中时附上经验参考。
- 校验硬政策、完整操作、审批期限和当前证据，可靠提交确认后才释放一次执行许可。
- 通过注入能力发起观察、审核、执行、验收和总结；具体存储、网络与运行监督由调用方实现。
- 将审批、任务和经验变化纳入同一恢复聚合提交，保留 Unknown、动作隔离和独立经验重试。

公开顶层模块只有 [operation](src/operation.rs) 与 [recovery](src/recovery/README.md)。库不包含故障采集台账、物理存储、网络客户端、系统时钟或运行时调度器。

## 使用

实现 `recovery::engine::RecoveryPlatform`，通过 `RecoveryEngine::advance` 推进恢复；调用方将 `RecoverySession::prepare` 返回的完整提案原子保存，确认后安装状态和交付效果。接入前提见[调用契约](docs/calling-contract.md)。底层匹配、请求准备和经验构造也提供独立纯函数。

## 阅读约定

- **故障**：待处理目标的问题及稳定适用条件。
- **经验**：结果、实际动作、证据和总结组成的参考记录，不代表执行权限。
- **Harness**：通过注入能力执行审核、修复或总结的外部能力；审核与执行使用独立会话。
- **脚本化评估**：可脚本化、不适合或无法判断；候选产物不是已验证的修复。
- `target`、`workload`、`provider observation`、`external action` 是中立业务对象，不绑定供应商。

## 导航

[架构](docs/architecture.md) · [恢复流程](docs/workflow.md) · [业务算法](docs/recovery.md) · [源码](src/README.md) · [文档](docs/README.md) · [开发](docs/development.md) · [测试](tests/README.md) · [实现状态](docs/implementation-status.md)

修改前阅读 [AGENTS](AGENTS.md)。构建与测试输出放源码外，检查入口见 [scripts](scripts/README.md)。
