# 修复经验

`contract.rs` 定义经验、查询与配置；`query.rs` 精确匹配并稳定排序；`managed.rs` 提供可信经验断言与管理查询；`state.rs` 校验不可变动作版本、幂等记录和失败/Unknown 隔离。`validation.rs` 检查中立数据形状。模型报告只提供总结及脚本化判断，候选不继承业务验收。完整恢复通过 RecoverySession 原子交付，独立 KnowledgeState 仍支持明确容量扩展。

流程见[恢复参考](../../../docs/workflow.md)，提交前提见[调用契约](../../../docs/calling-contract.md)，开发规则见[AGENTS](AGENTS.md)。
