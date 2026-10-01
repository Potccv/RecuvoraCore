# 恢复领域开发规范

继承[源码规范](../AGENTS.md)，模块划分见[恢复领域](README.md)。

- 各领域保留其迁移与验证规则，跨域恢复编排放 `workflow`，不以任务阶段替代审批、验收或经验规则。
- 所有状态改变通过纯提案与 Host 提交确认表达；不得重新引入文件存储、运行时或外部调用。
- 多个领域的提交不能假定原子成功。保留操作关联、幂等身份和中间意图，按[维护约定](../../docs/domain-maintenance.md)定义失败与恢复。
- 沿修改路径阅读[审批](approval/AGENTS.md)、[故障](incidents/AGENTS.md)、[知识](knowledge/AGENTS.md)和[恢复流程](workflow/AGENTS.md)规则。
