# 核心源码

[lib.rs](lib.rs) 公开 `operation` 与 `recovery` 两个模块。本页是公开模块的源码导航；项目职责见[架构](../docs/architecture.md)。

| 公开模块 | 职责与入口 |
| --- | --- |
| `operation` | [调用取消与监督](operation.rs)，供异步业务端口共用，支持取消并等待当前任务结束；不装配应用生命周期 |
| `recovery` | [恢复领域](recovery/README.md)，组织故障、审批、知识库与恢复编排 |

[identity.rs](identity.rs) 是私有实现，提供本库的有界标识校验和调用关联 ID。它不是认证服务、目标归属权威或跨进程 ID 规范；调用方通过公开领域类型表达这些关系。

开发规则见[源码规范](AGENTS.md)，各域规则沿目录继续继承。
