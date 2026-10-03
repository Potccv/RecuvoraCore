# 经验模型与匹配

`RepairExperience` 保存结果、适用条件、实际动作、证据与 `ExperienceReport`；`Scriptability` 表达可脚本化、不适合或无法判断。

`matching_experiences` 返回稳定排序的借用引用，不维护知识状态、可信断言、隔离表或存储。`KnowledgeQuery.limit` 的形状检查不截断底层匹配；完整算法见[业务参考](../../../docs/recovery.md#经验匹配)。

中立动作类型统一来自 [operation](../../operation.rs)，详细调用前提见[调用契约](../../../docs/calling-contract.md)。
