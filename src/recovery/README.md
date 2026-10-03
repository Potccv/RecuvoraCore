# 恢复业务

- [engine](engine/README.md)：完整阶段推进、注入能力和原子聚合。
- [approval](approval/README.md)：政策、审核、一次许可与 Unknown。
- [workflow](workflow/README.md)：任务、执行证据、验收及经验工作。
- [knowledge](knowledge/README.md)：经验、匹配、产物版本和隔离。
- [planning.rs](planning.rs)：稳定适用条件与有界请求纯函数。
- [experience.rs](experience.rs)：依据明确结果构造经验。

业务计算错误为 `BusinessError`，引擎汇总为 `EngineError`。流程见[恢复参考](../../docs/workflow.md)，算法见[业务参考](../../docs/recovery.md)。
