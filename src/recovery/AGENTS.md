# 恢复领域开发规范

继承[源码规范](../AGENTS.md)，域划分见[恢复领域导航](README.md)。

- 状态迁移与存储校验保留在所属域；跨域流程放在 `workflow`，不得复制其他域的状态机或以任务阶段绕过其写入 API。
- 跨域提交不得假定多个日志原子同步。组合流程须保留权威记录的身份关联，并由所属恢复编排定义中断恢复与幂等重试。

修改各域时继续遵守[故障规范](incidents/AGENTS.md)、[审批规范](approval/AGENTS.md)、[知识库规范](knowledge/AGENTS.md)及[恢复编排规范](workflow/AGENTS.md)。
