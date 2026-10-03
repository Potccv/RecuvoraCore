# 测试导航

[business.rs](business.rs) 验证精确匹配、稳定排序、完整命中数、已知与未知统一请求、整条预算选择、UTF-8/JSON 临界预算、稳定条件冲突和容量、经验结果与脚本化报告分离及计算可重复性。

[windows_check.rs](windows_check.rs) 与 [windows_check_cargo.rs](windows_check_cargo.rs) 由开发脚本直接编译运行，验证路径与子进程环境保护。Cargo 目标以 [manifest](../Cargo.toml) 为准。

检查命令见[scripts](../scripts/README.md)。测试只证明本库计算和开发工具行为，不证明真实外部执行。约束见 [AGENTS](AGENTS.md)。
