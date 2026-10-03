# 调用契约

`RecoveryPlatform` 是可信能力接口，`RecoveryEngine` 决定业务调用顺序。实现方提供配置、显式时间、取消状态、已确认状态读取，以及 inspect、review、execute、verify、summarize 和提交能力。

## 聚合提交

1. `commit` 使用当前 `RecoverySession::prepare(id, command, now_ms)` 生成提案。
2. 原子核对 `CommitRequest.expected_revision`，保存请求和 `pending.state().latest_entry()` 的完整条目。
3. 可靠持久提交后构造 `CommitReceipt::confirmed`，调用 `pending.confirm`。
4. 安装确认后的状态，向引擎返回效果。写入未知时不得安装或重试派发，应读取受保护的已提交历史恢复。

提交绑定完整命令、配置、时间、先前历史摘要与全部子域变化。效果在确认前不可取得；许可不可复制、不可反序列化。提交回执是可信调用方的声明，不是认证机制。

`RecoverySession::restore` 校验有序完整历史，不返回执行效果；恢复后先提交 `SessionCommand::Recover`。原操作和期限不变，中断执行进入 Unknown，等待审批进入 Paused。具体规则见[恢复流程](workflow.md)。历史摘要不防止可信存储整体被替换或删改后重算，来源保护属于调用方。

## 外部能力

- `acquire_execution` 必须获取当前故障及目标所有权保护，并保持到释放；逻辑 `TargetAuthority` 本身不是锁。
- `execute` 必须在实际外部发送前复核保护及 `validate_dispatch`，无自动执行重试；具体动作经 `PrepareAction` 提案确认后才派发。
- `release_execution` 应幂等。调用取消或 future 结束不证明远端执行者停止；实现方监督并排空原调用，无法证明完成则返回 Unknown。
- 审核身份来自可信能力绑定；模型只给出建议。验收独立于修复模型，证据绑定原操作、目标、验证规则和时间。
- 逻辑状态读取与提交必须使用同一聚合。多个驱动不得并发推进同一任务；物理锁和版本比较由实现方提供。
- 能力错误转换为 `EngineError::Port`；冲突、忙碌、停止与容量错误保留结构化分类。

## 独立计算示例

```rust
use recuvora_core::recovery::knowledge::{KnowledgeQuery, matching_experiences};
use std::collections::BTreeMap;
let query = KnowledgeQuery {
    conditions: BTreeMap::from([("fault_fingerprint".into(), "unhealthy".into())]),
    keywords: vec![],
    limit: 4,
};
assert!(matching_experiences(&query, [])?.is_empty());
# Ok::<(), recuvora_core::recovery::BusinessError>(())
```

`prepare_repair`、`matching_experiences`、`build_experience` 的单独调用只产生业务数据，不替代聚合的授权、验收和提交。
