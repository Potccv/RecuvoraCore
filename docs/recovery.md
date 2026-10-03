# 修复业务计算

## 稳定适用条件

`planning::stable_conditions` 合并 `ProblemContext.conditions` 与 `TargetBinding.required_facts`，同名同值合并，同名异值拒绝；最后加入保留键 `fault_fingerprint`。调用方不能自行提供该保留键，合并后的条件最多 32 项。动态 `TargetObservation.facts` 保留在请求中，但不参与经验适用条件生成。

## 经验匹配

`knowledge::matching_experiences` 要求经验的每项条件都与查询对应值一致，查询关键词采用区分大小写的 AND 匹配。结果按 `recorded_at_ms` 降序、`id` 升序排列，返回借用引用，不修改或复制全部经验载荷。失败和 Unknown 经验保留原结果标签，仍可作为参考。

查询 `limit` 的合法范围为 1..100；底层匹配返回全部命中以保留真实数量，由消费者按需要截取。单独调用匹配函数时，经验应来自唯一身份集合；完整恢复使用 KnowledgeState 校验幂等身份、验收归属和隔离。

## 统一请求

`planning::prepare_repair` 生成 `HarnessRepairRequest`。没有匹配经验时列表为空，有匹配经验时附上参考，两者使用同一处理路径。修复请求始终设置 `summarize_experience = true`、`assess_scriptability = true`。

完整 JSON 请求最多 32 KiB，含故障、观察、描述约束、经验及 JSON 转义开销。先检查必需上下文，再按排序逐条尝试纳入经验，最多四条；超大条目跳过并继续尝试后续条目，不截断文本或证据。`matched_experience_count` 保留全部命中数，即使没有条目能放入预算。必需上下文超限返回 `BusinessError`。

请求只表达要处理的业务内容，不授予执行权，也不核实提供的观察是否新鲜。

## 经验构造

`build_experience(ExperienceInput)` 从显式结果与总结构造 `RepairExperience`。稳定适用条件使用相同计算函数；身份、实际动作、证据、时间和结果按输入保留。

`ExperienceReport` 包含总结、经验教训、相关经验身份与 `Scriptability`。`Possible` 可以带候选产物，也可以仅解释可脚本化；`NotSuitable` 与 `Undetermined` 都需理由。候选始终留在报告中，不会被写入实际动作列表，也不会改变业务结果。

单独的经验构造函数不保存或重试；完整恢复由聚合从已确认任务提取结果，推进总结和可信交付。函数的数据结果不证明动作执行、目标健康或候选安全。

完整阶段与权威规则见[恢复流程](workflow.md)。
