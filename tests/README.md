# 核心测试

集中测试验证纯领域状态与 Host 提交边界。[Cargo.toml](../Cargo.toml) 显式登记目标，领域测试不创建文件、不调用外部执行器、不启动运行时。

| 目标 | 入口 | 范围 |
| --- | --- | --- |
| `commit` | [commit.rs](commit.rs) | 提交回执绑定完整领域输入、版本和域身份；确认前不返回效果 |
| `incidents` | [incidents.rs](incidents.rs) | 检查点/信号批次原子准备、故障轮次、Unknown、确认、幂等与历史校验 |
| `approval` | [approval.rs](approval.rs) | 当前硬政策、三种审核模式、期限、身份、人工拒绝、一次许可、目标阻塞、显式恢复、完整旧审批导入与身份保留 |
| `knowledge` | [knowledge.rs](knowledge.rs) | 候选/案例幂等、不可变版本、可信验收、永久隔离、排序、外部候选限制、完整传输与重放 |
| `recovery` | [recovery.rs](recovery.rs) | 原操作关联、提交后派发、诊断预算和回调版本、重启显式恢复、执行与验收区分、Unknown核实、复用失败、经验交付独立与隔离、完整旧流程导入与跨域关联拒绝 |

统一修复回归覆盖空经验和无脚本经验匹配、显式委托、总结失败/恢复与迟到回调、动作先提交及中断隔离、候选脚本不继承业务验收。

新增回归覆盖历史摘要和原配置冲突、无审核尝试拒绝、扩容中断与重放、永久隔离和外部候选规范化重试。各领域的 `scale_` 测试限制请求大小并验证持续积累后的重放；`Instant` 仅报告准备及重放耗时，不参与领域判断，也不设置易受环境影响的时限断言。测量方法及结果见[实现状态](../docs/implementation-status.md)。

测试以显式时间和可信 Host 提交断言驱动，不证明实际数据库事务、外部锁、网络执行、真实清理或业务恢复。

## Windows 检查脚本

[windows_check.rs](windows_check.rs) 通过 [check.rs](../scripts/windows/check.rs) 的 `rustc --test` 入口运行；[windows_check_cargo.rs](windows_check_cargo.rs) 是专属子进程替身。它们只验证开发检查脚本的参数、路径、顺序和失败行为；需要源码外 `RECUVORA_TEST_TEMP`，不属于产品存储实现。

运行入口见[脚本说明](../scripts/README.md)，验证政策见[测试与验证](../docs/testing.md)。
