# 调用契约

Core 接受普通业务数据并返回普通业务数据。调用方负责数据来源、当前有效性、授权、状态管理和结果保存。JSON 可反序列化为请求或经验，不会获得任何许可。

## 入口示例

```rust
use recuvora_core::recovery::knowledge::{KnowledgeQuery, matching_experiences};
use std::collections::BTreeMap;

let query = KnowledgeQuery {
    conditions: BTreeMap::from([("fault_fingerprint".into(), "unhealthy".into())]),
    keywords: vec![],
    limit: 4,
};
let matches = matching_experiences(&query, [])?;
assert!(matches.is_empty());
# Ok::<(), recuvora_core::recovery::BusinessError>(())
```

修复请求使用 `planning::RepairRequestInput` 和经验引用迭代器调用 `prepare_repair`。输入中的 `ProblemContext` 提供故障，`TargetObservation` 提供完整观察，`TargetBinding` 和委托字段描述调用方选定的约束。算法和完整预算见[业务参考](recovery.md)。

## 输入和结果

- 调用方管理经验唯一身份和可信来源，避免同一身份出现矛盾内容。
- 观察时间只是数据，Core 不读取当前时间或判定时效；条件匹配也不证明外部目标当前仍符合条件。
- `build_experience` 接收明确业务结果和实际动作。模型报告只提供总结与脚本化建议，不能替代真实结果。
- `RepairArtifact::validate`、`ExperienceReport::validate`、`ProblemContext::validate` 与 `KnowledgeQuery::validate` 仅检查中立数据形状和大小，不检查权限、执行环境或来源。
- `BusinessError::Invalid` 表示内容或预算不满足契约，`Capacity` 表示稳定事实数量超限；没有提交、重放或网络错误类型。

本库不提供状态安装、提交确认、历史重放、执行许可或交付队列接口。
