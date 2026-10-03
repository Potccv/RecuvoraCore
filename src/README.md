# 源码导航

`lib.rs` 仅导出 `operation` 与 `recovery`。本库为单一 Cargo 库包。

| 位置 | 责任 |
| --- | --- |
| [operation.rs](operation.rs) | `RepairArtifact` 与中立载荷边界 |
| [recovery](recovery/README.md) | 经验模型、匹配、修复请求与经验构造 |

修改遵循 [AGENTS](AGENTS.md)，业务说明见[架构](../docs/architecture.md)。
