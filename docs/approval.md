# 审批与一次性执行许可

本页说明审批在恢复流程中的职责。`ApprovalStore` 的具体 API、审核转交、存储配置及恢复约束由[审批模块](../src/recovery/approval/README.md)维护；任务与审批的持久关联由[恢复流程](recovery.md)维护。

## 决定与身份

Core 按可信规则接受人工或指定 Harness 的审核，人工与模型决定均受不可绕过的规则约束。Harness 指 AI 服务接入。Host 认证调用者并限制 API 访问；Core 记录的操作者（`actor`）、Harness 与会话身份用于绑定与审计，不充当认证凭据。

模型提出方案或评估，不构造执行许可；知识库案例、故障确认和 UI 状态也不能代替审批。执行与审核上下文独立，可信宿主应用负责实际会话与权限隔离。

## 从批准到执行

批准本身不派发执行。调用方执行前复核当前目标与完整操作，`ApprovalStore::consume` 复核当前审批和规则、写入并同步执行意图后交付一次 `ExecutionPermit`。恢复流程通过 `AuthorizedScript` 将已绑定操作交给 `RepairBackend::execute`；Core 不包含外部执行器。

外部执行回执与独立业务验收是不同事实。结果无法确定时保留 Unknown，可信调用方核实执行结果后只记录已经核验的事实，不续期或产生新的执行许可。任务恢复、取消并等待当前调用结束和目标所有权的关系见[恢复流程](recovery.md)与[领域维护](domain-maintenance.md)。
