# 提交与动作

`RepairArtifact` 描述中立动作内容。`CommitRequest` 绑定身份、领域、完整输入和版本；`Prepared` 持有候选状态及未释放效果；`CommitReceipt` 由可信调用方确认后才可取得 `Committed`。

流程见[恢复参考](../../docs/workflow.md)，提交前提见[调用契约](../../docs/calling-contract.md)，开发规则见[AGENTS](AGENTS.md)。
