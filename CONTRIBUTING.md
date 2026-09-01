# 参与贡献

感谢你愿意改进 Ramag。Ramag 是一个本地优先的跨平台开发者桌面工作台，欢迎 Bug 报告、功能建议、文档改进和代码贡献。

## 开始之前

- 提交 Bug 前，请搜索已有 Issue，避免重复报告。
- 涉及密码、连接串、服务器地址、数据库内容、SSH 私钥或令牌的问题，**不要**公开提交 Issue；请按 [SECURITY.md](SECURITY.md) 的流程报告。
- 大型功能、架构调整或兼容性变化，请先创建 Feature Request Issue 说明目标和使用场景，确认方向后再投入实现。

## 本地开发

开发环境需要 Git、rustup 和当前平台的构建工具。仓库通过 `rust-toolchain.toml` 固定 Rust 工具链；完整的首次运行说明见 [开发入门指南](docs/development-guide.md)。

Windows、Linux 和 macOS 使用相同的 Cargo 命令：

```bash
cargo dev
cargo fmt-check
cargo check-all
cargo clippy-all
cargo test-all
```

仓库通过 `rust-toolchain.toml` 固定 Rust nightly，通过 `.cargo/config.toml` 提供这些别名。`Makefile` 仍保留给打包、数据库测试和旧入口使用；日常应用运行与 Rust 质量检查不需要安装 Make。

首次克隆或切换到新的工作区后，启用提交前源码尺寸检查：

Linux/macOS：

```bash
./scripts/install-githooks.sh
```

Windows PowerShell：

```powershell
.\scripts\install-githooks.ps1
```

安装了 Make 的环境可以执行 `make install-hooks`，该目标会根据当前系统调用对应脚本。安装后，hook 会在每次提交前检查 Rust 源文件是否超过 600 行，并运行 `cargo clippy --workspace --all-targets -- -D warnings`；任一检查失败，提交会被拒绝。

数据库基础设施改动需要真实环境验证时，运行 `make db-test`。该命令会启动专用 Docker 容器并写入测试数据；`make db-test-clean` 会删除这些测试容器、卷和凭据。

## 提交 Pull Request

1. 从最新的 `main` 创建聚焦的分支。
2. 保持一个 PR 只解决一个明确问题；不要混入无关重构或格式化。
3. 为新增或修改的核心逻辑补充测试，并运行与改动范围匹配的验证命令。
4. 在 PR 描述中说明问题、方案、验证结果，以及需要人工验证的平台。
5. 不要在提交、截图、日志或测试夹具中包含真实凭据、用户数据或私有服务地址。

CI 会在 Linux、macOS 和 Windows 上检查格式、编译、Clippy、测试和桌面打包逻辑。维护者可能要求补充测试、拆分范围或调整实现后再合并。

## English summary

Contributions are welcome. Please use public issues for reproducible bugs and feature discussions, keep pull requests focused, run the relevant checks, enable the repository pre-commit hook with `scripts/install-githooks.sh` or `scripts/install-githooks.ps1`, and never include credentials or private data. Report security-sensitive issues through [SECURITY.md](SECURITY.md).
