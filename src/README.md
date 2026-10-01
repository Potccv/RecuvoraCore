# 核心源码

[lib.rs](lib.rs) 只公开 `operation` 和 `recovery`，职责见[架构](../docs/architecture.md)。

| 模块 | 入口与职责 |
| --- | --- |
| `operation` | [提交提案](operation.rs)：`Prepared`、绑定完整输入的 `CommitRequest`、可信提交回执及确认后效果 |
| `recovery` | [恢复领域](recovery/README.md)：故障、审批、知识和恢复流程的纯逻辑 |

[identity.rs](identity.rs) 仅提供私有有界标识校验，不生成系统时间或进程相关身份，也不证明认证和目标归属。规则见[源码规范](AGENTS.md)。
