# 模块入口

[核心源码 README](../src/README.md)是模块职责和子域导航的统一入口，各域 README 维护自己的纯状态接口、提交提案、查询及历史校验约定。公开顶层模块保持 `operation` 与 `recovery`，本页不重复源码文件清单。

职责关系见[架构](architecture.md)，恢复步骤见[恢复流程](recovery.md)，Host 原子提交、跨域关联与目标归属见[领域维护](domain-maintenance.md)。从旧存储服务 API 接入的调用方先阅读[0.2 迁移说明](host-boundary-migration.md)。
