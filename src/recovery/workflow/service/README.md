# 任务领域实现

`contract.rs` 定义任务、配置与证据；`engine.rs` 校验迁移、操作绑定、时效、预算、验收和恢复；`experience.rs` 构造交付工作；`query.rs` 提供只读任务与经验状态。该模块计算领域状态，外层恢复引擎推进异步能力。

流程见[恢复参考](../../../../docs/workflow.md)，提交前提见[调用契约](../../../../docs/calling-contract.md)，开发规则见[AGENTS](AGENTS.md)。
