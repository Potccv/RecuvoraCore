# Core 文档导航

本目录说明 Core 的共同架构、领域流程、接入维护及验证范围。模块内部 API 和存储约定由[源码目录](../src/README.md)下的所属 README 维护，应用接口与目标侧实现由集成项目维护。术语见项目[阅读约定](../README.md#阅读约定)。

## 开始阅读

- 从源码开发：先读[开发指南](development.md)，再读[架构](architecture.md)和修改目录的局部 AGENTS。
- 接入修复流程：先读[恢复流程](recovery.md)，再读[审批](approval.md)与[领域维护](domain-maintenance.md)。
- 核对现有能力和证据：查看[实现状态](implementation-status.md)和[测试与验证](testing.md)。

## 文档归属

| 文档 | 类型与范围 |
| --- | --- |
| [开发指南](development.md) | 流程指南：工具链、修改归属、外部输出目录和检查入口 |
| [架构](architecture.md) | 参考：职责、可信端口与权威状态的关系 |
| [恢复流程](recovery.md) | 参考：阶段、审批关联恢复、显式恢复与重诊断政策 |
| [审批与许可](approval.md) | 参考：审批政策、审核身份与一次执行许可 |
| [领域维护](domain-maintenance.md) | 参考：故障轮次、规范目标归属、日志迁移及可信 Host 维护要求 |
| [实现状态](implementation-status.md) | 状态记录：当前能力、限制和已执行验证的范围 |
| [测试与验证](testing.md) | 参考：按改动选择检查、资源范围和证据含义 |
| [源码导航](../src/README.md) | 模块职责及所属 API 文档入口；[模块地图](module-map.md)保留导航路径 |
| [Core 职责边界](host-boundary-migration.md) | 接入方职责导航 |

文档编写遵循[本目录规范](AGENTS.md)。详细事实保留在所属文档；父级只说明职责并提供链接。
