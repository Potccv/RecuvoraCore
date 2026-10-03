# 修复业务

[knowledge](knowledge/README.md) 定义经验与匹配；[planning.rs](planning.rs) 合并稳定条件、生成统一修复请求；[experience.rs](experience.rs) 构造修复经验。

对外入口为 `knowledge::matching_experiences`、`planning::prepare_repair` 与 `build_experience`。`BusinessError` 表达无效业务数据和条件容量超限。本模块不保存任务状态。

算法与边界见[业务参考](../../docs/recovery.md)，约束见 [AGENTS](AGENTS.md)。
