# RecuvoraCore

RecuvoraCore 是无状态 Rust 业务计算库，包名 `recuvora-core`，版本 0.2.0。它从故障和经验生成有界修复请求，再从明确结果和总结构造修复经验。

## 功能

- 按稳定条件和关键词精确匹配经验，保留成功、失败和 Unknown 的结果标签。
- 已知与未知故障统一生成 `HarnessRepairRequest`，提供完整命中数与最多四条预算内经验；要求总结经验并评估脚本化。
- 根据显式结果、实际动作、证据和总结生成 `RepairExperience`，候选脚本与实际动作分开表达。

公开顶层模块只有 [operation](src/operation.rs) 与 [recovery](src/recovery/README.md)。库中没有审批、执行许可、故障台账、任务状态机、提交回执或重试队列；调用结果只是业务数据。

## 使用

通过 Rust 库依赖调用 `recovery::planning::prepare_repair`、`recovery::knowledge::matching_experiences` 和 `recovery::build_experience`。输入由调用方提供；本库不读取文件、访问网络、读取系统时间或执行动作。前提与示例见[调用契约](docs/calling-contract.md)。

## 阅读约定

- **故障**：待处理的目标问题及稳定适用条件。
- **经验**：包含结果、实际动作、证据和总结的参考记录，不代表执行权限。
- **Harness**：接收修复或总结请求的外部能力；本库只构造数据。
- **脚本化评估**：可脚本化、不适合或无法判断；候选产物不是已验证的修复。
- `target`、`workload`、`provider observation`、`external action` 表示中立业务对象，不绑定具体供应商。

## 导航

[架构](docs/architecture.md) · [业务参考](docs/recovery.md) · [源码](src/README.md) · [文档](docs/README.md) · [开发](docs/development.md) · [测试](tests/README.md) · [实现状态](docs/implementation-status.md)

修改前阅读 [AGENTS](AGENTS.md)。构建与测试输出放源码外，检查入口见 [scripts](scripts/README.md)。
