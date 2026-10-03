# 输入边界

本库没有应用配置或部署模板。故障、观察、约束和经验以 Rust 参数显式传入，见[调用契约](../docs/calling-contract.md)。

`SessionConfig` 包含恢复、审批和经验集合约束；`TargetBinding` 描述逻辑目标及允许动作。配置由调用方提供，本库不加载配置文件。规范见 [AGENTS](AGENTS.md)。
