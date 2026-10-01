# Windows 开发检查

[check.rs](check.rs) 是本库的完整代码检查入口，使用 Rust 标准库编译与运行，检查格式、全部测试目标的 Clippy、单元/集成测试和文档示例。需要 Windows 和[项目工具链](../../rust-toolchain.toml)；脚本不安装工具链或更改全局配置，也不依赖 PowerShell 运行时。

## 参数

| 参数 | 行为 |
| --- | --- |
| `--build-dir` | 必填，源码外的构建输出目录 |
| `--temp-root` | 必填，源码外的测试临时根目录 |
| `--cargo-path` | 已有 Cargo 程序路径，默认使用当前环境中的 `cargo` |
| `--help` | 单独使用，显示调用方式并退出，不创建输出目录或运行检查 |

输出目录使用普通盘符或 UNC 路径。脚本将其解析为绝对路径，两目录必须位于本库源码之外，且互不包含；已有路径及所有祖先不得含重解析链接。全部路径校验完成后才创建缺失的输出目录。参数错误、路径校验失败或子进程启动失败均返回非零结果。

## 编译与运行

按[开发指南](../../docs/development.md)为 `CARGO_TARGET_DIR` 和 `RECUVORA_TEST_TEMP` 选择源码外的绝对目录。编译前创建构建目录，确认该目录及祖先没有重解析链接；脚本可执行文件同样放在构建目录内。

下面使用 PowerShell 作为命令终端。从项目根以绝对源码路径编译，让脚本记录本库源码位置：

```powershell
$checkSource = (Resolve-Path scripts/windows/check.rs).Path
rustc --edition=2024 -D warnings $checkSource -o "$env:CARGO_TARGET_DIR\core-check.exe"
if ($LASTEXITCODE -ne 0) { throw "检查脚本编译失败" }
```

编译成功后运行：

```powershell
& "$env:CARGO_TARGET_DIR\core-check.exe" --build-dir "$env:CARGO_TARGET_DIR" --temp-root "$env:RECUVORA_TEST_TEMP"
```

需要选择已有 Cargo 程序时增加 `--cargo-path` 参数；运行 `core-check.exe --help` 可查看参数。程序根据编译时记录的源码位置定位本库，不依赖执行时的工作目录。

## 执行与清理

脚本从本库项目根依次运行：

```text
cargo fmt --all -- --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --all-targets --locked
cargo test --doc --locked
```

每个 Cargo 子进程分别设置 `CARGO_TARGET_DIR`、`RECUVORA_TEST_TEMP` 和 `RUST_TEST_THREADS=4`，调用方环境保持不变。首次检查失败即停止并返回 Cargo 的非零退出码；全部检查成功才返回零。

各测试负责清理自己创建的临时子目录，脚本不递归清空共享输出目录。检查脚本的编译产物由调用方按本次记录范围保留或清理。本目录开发约束见 [AGENTS](AGENTS.md)，测试目标归属见[测试导航](../../tests/README.md)。

## 修改脚本后的检查

Cargo 的格式与 Clippy 检查不包含独立脚本。修改脚本后，从项目根检查脚本和测试替身的格式：

```powershell
rustfmt --edition=2024 --check scripts/windows/check.rs tests/windows_check_cargo.rs
```

使用已安装的 Clippy 驱动检查独立脚本，编译产物仍写入源码外目录：

```powershell
clippy-driver --edition=2024 -D warnings $checkSource -o "$env:CARGO_TARGET_DIR\core-check.exe"
if ($LASTEXITCODE -ne 0) { throw "检查脚本 Clippy 失败" }
```

回归测试位于 [windows_check.rs](../../tests/windows_check.rs)，通过 [windows_check_cargo.rs](../../tests/windows_check_cargo.rs) 记录子进程参数、环境和失败行为。沿用上述绝对源码路径及源码外输出目录，编译并运行脚本专属测试：

```powershell
rustc --test --edition=2024 -D warnings $checkSource -o "$env:CARGO_TARGET_DIR\core-check-tests.exe"
if ($LASTEXITCODE -ne 0) { throw "检查脚本测试编译失败" }
& "$env:CARGO_TARGET_DIR\core-check-tests.exe"
```

运行前，`RECUVORA_TEST_TEMP` 必须指向已存在的源码外临时目录；测试仅清理自身创建的子目录。
