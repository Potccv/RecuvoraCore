# Core 文档导航

本目录说明纯领域 Core 的共同架构、恢复流程、Host 提交与验证范围。领域 API 由[源码目录](../src/README.md)下的所属 README 维护；存储实现、外部调用和应用接口由集成项目维护。术语见项目[阅读约定](../README.md#阅读约定)。

## 开始阅读

- 从源码开发：先读[开发指南](development.md)，再读[架构](architecture.md)和修改目录的局部 AGENTS。
- 接入恢复流程：先读[架构](architecture.md)与[领域维护](domain-maintenance.md)，再读[恢复流程](recovery.md)和[审批](approval.md)。
- 迁移旧调用方：阅读[0.1 到 0.2 的 Host 接入迁移](host-boundary-migration.md)。
- 核对现有能力和证据：查看[实现状态](implementation-status.md)和[测试与验证](testing.md)。
- 跟进领域缺口：查看[已知问题](implementation-status.md#已知问题)中的 CORE-001 至 CORE-004、处理状态和回归证据。

## 文档归属

| 文档 | 类型与范围 |
| --- | --- |
| [开发指南](development.md) | 流程指南：工具链、修改归属、源码外输出和检查入口 |
| [架构](architecture.md) | 参考：纯领域计算、Host 责任及提案确认边界 |
| [恢复流程](recovery.md) | 参考：阶段、显式恢复、审批关联、结果核实及经验交付 |
| [审批与许可](approval.md) | 参考：硬政策、审核身份、两阶段授权和一次执行许可 |
| [领域维护](domain-maintenance.md) | 参考：跨域提交顺序、目标所有权、故障轮次及历史维护 |
| [Host 接入迁移](host-boundary-migration.md) | 接入指南：破坏性的 0.2 API 变化、Host 适配及旧数据导入边界 |
| [实现状态](implementation-status.md) | 状态记录：当前能力、已知问题及处理状态、限制和已执行验证的范围 |
| [测试与验证](testing.md) | 参考：检查选择、资源范围与证据含义 |
| [源码导航](../src/README.md) | 模块职责与所属 API 入口；[模块地图](module-map.md)保留导航路径 |

文档编写遵循[本目录规范](AGENTS.md)。具体机制保留在所属文档，其他位置通过相对链接引用。
