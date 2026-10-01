# 恢复领域

`recovery` 组织四个恢复业务域，各自拥有对应的事实和写入判定。恢复编排通过业务接口与权威记录引用组合它们，任务阶段不能替代其他域的决定。

| 域 | 职责与接口约定 |
| --- | --- |
| `incidents` | [故障事实](incidents/README.md)：故障轮次、确认及恢复状态 |
| `approval` | [持久审批](approval/README.md)：审核政策、人工或 Harness（AI 服务接入）决定、一次许可及执行事实 |
| `knowledge` | [知识库](knowledge/README.md)：修复经验、不可变脚本版本、可信验收、隔离及只读候选 |
| `workflow` | [恢复编排](workflow/README.md)：组合领域事实与可信业务端口 |

Core 与嵌入应用的分工见[架构](../../docs/architecture.md)，开发规则见[恢复领域规范](AGENTS.md)。
