# 实现状态与验证范围

Core 已实现持久故障、持久审批与一次性执行许可、知识库案例与不可变脚本隔离、可恢复流程、人工/Harness 审核、执行与验收分离、未知执行结果核实及修复经验保存。Harness 指 AI 服务接入；源码入口和各域接口约定见[核心源码](../src/README.md)。

| 能力 | 当前接口约定 |
| --- | --- |
| 只读外部候选、可信检索排序、知识库查询摘要与完整检查点 | [知识库](../src/recovery/knowledge/README.md) |
| 独立故障轮次、共享目标所有权、完整持久日志与限额迁移 | [领域维护](domain-maintenance.md) |
| 恢复流程、审批关联恢复及有限重新诊断 | [恢复流程](recovery.md) |
| 恢复服务、执行证据字段与持久记录 | [公开接口与持久记录](../src/recovery/workflow/service/README.md#公开接口与持久记录) |

审批申请意图和引用使用持久关联，行为见[审批关联恢复](recovery.md#审批关联恢复)。脚本失败后的自动重新诊断范围见[重新诊断与未知执行结果](recovery.md#重新诊断与未知执行结果)。

Core 不实现网络接入、身份认证、配置热加载、日志采集、真实脚本沙箱、进程树监督或具体业务验收。Host 接口接入与离线维护命令由可信宿主应用实现，节点/插件与实际部署另行验证；本库不声称真实脚本执行、跨机部署、完整业务恢复或长期稳定性已通过验收。

## 已有代码验证

已有代码检查使用 Rust 1.98.1 Windows LLVM 工具链，通过格式检查、全部测试目标 Clippy（`-D warnings`）、130 项单元/集成测试及文档测试命令。文档测试包含 0 个可执行示例；MSVC 因缺少链接器未完成验证。Host 的所有目标通过 Clippy，相关库、故障复核、调度与修复后端回归通过。

完整 Core 检查已通过 [Windows Rust 检查入口](../scripts/windows/README.md)执行。独立脚本及测试模块通过格式和 Clippy 检查，4 项脚本回归覆盖参数、检查顺序、首次失败停止、退出码、无法启动 Cargo、子进程环境、普通 Windows 路径和创建前的源码/重叠目录/重解析链接拒绝；测试入口见 [Windows 检查脚本测试](../tests/README.md#windows-检查脚本)。

集中回归使用模拟业务接口与领域存储，覆盖故障、审批、知识库、恢复流程和持久化必须保持的约束。审批提交边界由 [`recovery_approval.rs`](../tests/recovery_approval.rs) 覆盖，非法关联修改由 [`recovery_storage.rs`](../tests/recovery_storage.rs) 覆盖，当前接口字段的严格解析、持久记录、Unknown 归属及显式恢复由 [`recovery_naming.rs`](../tests/recovery_naming.rs) 覆盖；其他测试入口见[测试说明](../tests/README.md)。这些验证检查具有最终效力的状态与安全边界，不能替代外部执行或真实部署验收。开发检查步骤见[开发与验证](development.md)。
