# 核心测试

本目录集中维护故障、审批、知识库、恢复流程和调用取消的领域不变量测试。[Cargo.toml](../Cargo.toml) 设置 `autotests = false`，集成目标在清单中显式登记；需要访问私有状态的库单元测试通过 `#[path]` 引入本目录文件。

## 集成目标

| Cargo 目标 | 入口与子模块 | 验证范围 |
| --- | --- | --- |
| `incidents` | [incidents.rs](incidents.rs) | 检查点与信号原子提交、故障轮次归并与异常解除后的新轮次、Unknown 观察不解除故障、确认、序号与时间、容量、路径、单写锁、损坏拒绝和关闭生命周期 |
| `approval` | [approval.rs](approval.rs) | 硬政策、三种审核模式、身份与审核尝试/记录版本号/截止时间绑定、迟到结果拒绝、一次许可、同目标互斥、执行 Unknown、核实未知执行结果及持久恢复 |
| `knowledge` | [knowledge.rs](knowledge.rs)、[knowledge_maintenance.rs](knowledge_maintenance.rs) | 候选与案例幂等、不可变脚本版本、可信验收绑定、精确条件和关键词、跨案例隔离与禁用、容量/锁/关闭/损坏；来源与响应边界、可信排序、无脚本文本的查询摘要、完整分块检查点、重复压缩与字节容量恢复、异常来源导出拒绝 |
| `recovery` | [recovery.rs](recovery.rs)、[recovery_approval.rs](recovery_approval.rs)、[recovery_guards.rs](recovery_guards.rs)、[recovery_shutdown.rs](recovery_shutdown.rs)、[recovery_naming.rs](recovery_naming.rs) | 假后端的诊断/审核/执行/验收/修复经验保存、脚本复用、明确失败与 Unknown、执行结果核实的提交边界、取消并等待当前调用结束、只重试保存；审批创建与关联故障恢复、原方案与幂等审批、诊断预算/到期/显式恢复/当前环境和政策冲突；权威故障与记录版本号、终态去重、异常样本与故障轮次、固定来源身份、外部候选只供诊断、来源错误的有界回退、跨目录 Unknown 所有权、有界查询摘要；日志变动时拒绝依据终态内存释放所有权；当前接口字段的严格解析、持久记录、Unknown 归属及暂停后的显式恢复 |
| `recovery_denial` | [recovery_denial.rs](recovery_denial.rs) | 人工拒绝的终态与持久性、迟到 Harness 不覆盖拒绝、复用脚本被 Harness 判定不适用后携带原评审上下文生成新方案并重新审批 |
| `target_ownership` | [target_ownership.rs](target_ownership.rs) | 不同目录/实例/进程使用同一权威所有权记录、真实子进程退出、持久所有权登记保留与显式释放、`recovery.lock` 文件对象身份、路径别名/硬链接/运行中替换拒绝与损坏拒绝 |

## 库单元模块

| 注册位置 | 测试文件 | 验证范围 |
| --- | --- | --- |
| [incidents.rs](../src/recovery/incidents.rs) 的 `storage_tests` | [incident_storage.rs](incident_storage.rs) | 写入失败保留内存与检查点并停用写入，重放拒绝过期确认和重复事务 |
| [approval.rs](../src/recovery/approval.rs) 的 `path_tests` | [approval_paths.rs](approval_paths.rs) | 原编译源码目录不存在时，已安装库的审批存储路径仍能正常使用 |
| [流程入口](../src/recovery/workflow/service/mod.rs) 的 `recovery_storage_tests` | [recovery_storage.rs](recovery_storage.rs) | 锁/路径/文件身份/损坏、可信配置及记录版本号、持久数据结构与迁移、成功终态必须有正向验收、实时保存和重放使用相同意图关联约束 |
| [流程入口](../src/recovery/workflow/service/mod.rs) 的 `recovery_migration_tests` | [recovery_migration.rs](recovery_migration.rs) | v1/v2 完整日志校验和实际 `TaskStore` 恢复，独立故障轮次推导，保留异常样本/记录版本号/终态/审批/操作/回执/执行结果核实记录；拒绝权威配置或动作/复用阈值修改、限额超出、伪造计数、预算不足、格式或来源损坏；空历史不生成事实 |

[workflow_support.rs](workflow_support.rs) 提供隔离临时目录及清理支持，由集成入口和 [recovery/mod.rs](../src/recovery/mod.rs) 的 `workflow_test_support` 引入；它不是独立测试目标。`operation` 的取消并等待当前调用结束行为通过 `recovery` 验证。

## Windows 检查脚本

[windows_check.rs](windows_check.rs) 由 [Windows 检查脚本](../scripts/windows/check.rs) 的测试模块引入，通过独立 `rustc --test` 运行，不登记为 Cargo 集成目标。[windows_check_cargo.rs](windows_check_cargo.rs) 是测试期间编译的 Cargo 替身，仅记录调用与模拟退出码。测试验证参数解析、检查顺序、首失败停止、退出码、Cargo 无法启动、子进程工作目录与环境、传入子进程的普通 Windows 路径、带空格和中文的路径，以及源码、输出目录重叠和 junction 拒绝边界；替身程序和运行记录均写入测试自身的临时子目录，结束后清理。运行命令见 [Windows 脚本说明](../scripts/windows/README.md)。

## 检查入口与证据范围

运行方式、环境变量和检查顺序由 [检查脚本说明](../scripts/README.md) 维护。测试临时目录来自源码外 `RECUVORA_TEST_TEMP`；测试只清理自身创建的路径。

流程测试使用假 `RepairBackend` 和中立夹具，不执行真实脚本或连接网络、Harness（AI 服务接入）、节点、供应商服务。所有权测试的真实子进程只提供本机文件锁与退出行为证据。这些测试不能证明真实业务执行、跨机接入或完整业务恢复；监控调度、应用配置、运行时、动作、模拟、网络和 CLI/HTTP 的应用测试由 Host 维护。
