# Windows 安装包发布方案

> 状态：设计已确认，尚未实施安装器和 GitHub Actions 工作流。
>
> 记录日期：2026-07-21。
>
> 目标：不依赖本地 Windows 电脑，由 GitHub Actions 生成可安装的 Windows x64 应用。

## 结论

采用以下组合：

```text
GitHub Actions Windows Runner：windows-2025
应用构建：scripts/build-windows.ps1 -Release
安装器：Inno Setup
发布：GitHub Actions Artifact / GitHub Release
```

推荐最终同时发布：

```text
Ramag-<version>-windows-x64-setup.exe
Ramag-<version>-windows-x64-portable.zip
SHA256SUMS.txt
```

- `setup.exe` 面向普通用户，提供安装、快捷方式、升级和卸载。
- `portable.zip` 面向希望解压即用的用户。
- `SHA256SUMS.txt` 用于校验下载文件完整性。

## 为什么可行

GitHub 当前提供 `windows-2025` x64 Runner。该镜像已安装 Visual Studio C++ x64 工具链、Windows 10/11 SDK、Rustup、Inno Setup 和 WiX Toolset。

项目现有的 `scripts/build-windows.ps1 -Release` 已负责：

1. 查找 Windows SDK 中的 `fxc.exe`，预编译 GPUI DirectX 着色器。
2. 使用锁文件构建 `x86_64-pc-windows-msvc` Release。
3. 校验产物是 x64 PE 和 Windows GUI 子系统。
4. 校验 Windows 版本资源中的 `ProductName`。
5. 校验没有动态依赖 MSVC/UCRT 运行库。

因此 GitHub Actions 只需复用现有构建脚本，再增加安装器和发布步骤，不需要在 CI 中重新实现 Rust 构建逻辑。

## 工具选型

| 工具 | 产物 | 优点 | 代价 | 结论 |
|---|---|---|---|---|
| Inno Setup | `Setup.exe` | 配置简单、用户体验成熟、Runner 已预装 | 不生成标准 MSI | 采用 |
| WiX | `.msi` | 适合企业批量部署和组策略 | 配置与升级规则更复杂 | 暂不采用 |
| MSIX | `.msix` | 现代化安装与商店分发 | 身份、签名和沙箱约束更多 | 暂不采用 |

当前目标是普通桌面工具发布，不需要为尚未出现的企业 MSI 或商店需求增加复杂度。

## 发布流程

```text
手动触发或推送 v* 标签
        ↓
GitHub 创建 windows-2025 Runner
        ↓
安装 rust-toolchain.toml 锁定的 Rust nightly
        ↓
运行 scripts/build-windows.ps1 -Release
        ↓
生成 target/x86_64-pc-windows-msvc/release/ramag.exe
        ↓
可选：签名 ramag.exe
        ↓
Inno Setup 编译安装器
        ↓
静默安装/卸载冒烟测试
        ↓
可选：签名最终安装器
        ↓
生成 portable.zip 与 SHA256SUMS.txt
        ↓
上传 Artifact；标签构建同时发布到 GitHub Release
```

## 计划新增文件

```text
.github/workflows/windows-release.yml
scripts/package-windows.ps1
scripts/windows/ramag.iss
```

### `windows-release.yml`

职责保持简单：

1. Checkout 源码。
2. 安装锁定的 Rust nightly 和 Windows x64 target。
3. 调用 `scripts/package-windows.ps1`。
4. 上传安装器、便携包和校验文件。
5. 仅在受保护的版本标签上执行签名和 GitHub Release 发布。

Runner 使用明确版本：

```yaml
runs-on: windows-2025
```

不使用 `windows-latest`，避免该标签未来迁移到新系统镜像时没有预警地改变构建环境。需要注意：即使固定 OS 标签，镜像内工具仍会定期更新，因此工作流必须保留前置检查并打印实际工具版本。

### `package-windows.ps1`

作为本地 Windows 和 CI 的统一打包入口：

1. 调用 `build-windows.ps1 -Release`，不绕过现有项目脚本。
2. 校验 Inno Setup 的 `ISCC.exe` 可用。
3. 从 Cargo workspace 读取应用版本。
4. 标签发布时校验 `v<version>` 与 Cargo 版本一致。
5. 调用 Inno Setup 生成安装器。
6. 生成便携 ZIP 和 SHA-256 校验文件。
7. 对缺失产物、版本不一致和命令失败显式报错。

### `ramag.iss`

安装器至少包含以下行为：

- 使用永久且固定的 `AppId`，后续版本不得修改，否则 Windows 会视为不同应用。
- 默认按当前用户安装到 `%LOCALAPPDATA%\Programs\Ramag`，避免请求管理员权限。
- 安装 `ramag.exe`，创建开始菜单快捷方式。
- 桌面快捷方式作为用户可选项，不默认强制创建。
- 在“已安装的应用”中显示版本、发布者和卸载入口。
- 升级或卸载前检测 Ramag 是否仍在运行。
- 卸载程序文件，但保留用户配置、数据库、凭据和日志。
- 使用 x64 安装模式，不生成 x86 或 ARM64 原生包。

当前应用资源和 Windows 图标均已嵌入 `ramag.exe`，MSVC CRT 也采用静态链接，因此安装器首版原则上只需包含一个应用 EXE。Git 和 OpenSSH 仍属于部分功能的外部运行时前提，不随安装包捆绑。

## 工作流触发策略

分为验证与发布两类：

| 触发方式 | 构建安装器 | 使用签名密钥 | 创建 GitHub Release |
|---|---:|---:|---:|
| `workflow_dispatch` | 是 | 默认否 | 否 |
| 普通分支/PR | 可选，仅验证 | 否 | 否 |
| 受保护的 `v*` 标签 | 是 | 是 | 是 |

推荐先通过 `workflow_dispatch` 生成未签名 Artifact，验证安装和卸载流程；稳定后再启用标签自动发布。

## 签名边界

未签名安装器在技术上可以安装，但 Windows 可能显示“未知发布者”或 SmartScreen 警告。正式公开发布建议完成 Authenticode 签名：

1. 签名 `ramag.exe`。
2. 让 Inno Setup 生成并签名卸载器。
3. 签名最终 `Setup.exe`。
4. 对签名进行验证后再上传。

证书及其口令不得写入仓库、安装脚本或工作流明文。签名只能在受保护的发布环境中使用 GitHub Secrets、硬件/云签名服务或 OIDC 短期凭据。来自 Fork 的 PR 不得进入签名任务。

## 验证标准

### CI 自动验证

- `build-windows.ps1 -Release` 全部校验通过。
- `ramag.exe` 和安装器均存在且大小非零。
- 安装器可静默安装到临时目录。
- 安装后存在 `ramag.exe` 和卸载器。
- 卸载命令成功，程序文件被移除。
- 便携 ZIP 可以解压且包含预期 EXE。
- SHA-256 文件与实际产物一致。
- 启用签名后，所有 PE 文件签名验证通过。

### Windows 人工验收

CI 的无交互 Runner 不能替代真实桌面验收。首个正式版本至少在 Windows 10/11 x64 验证：

- 安装、覆盖升级和卸载。
- 开始菜单和可选桌面快捷方式。
- 单实例、托盘、全局快捷键和窗口唤起。
- GPUI DirectX 渲染。
- Windows Credential Manager、剪贴板监听和用户数据保留。
- Git/OpenSSH 缺失时的错误提示。

## 安全与可复现性

- 所有 Cargo 构建必须使用 `--locked`。
- 第三方 GitHub Actions 应固定到完整 commit SHA；优先使用 GitHub 官方 Action 和 Runner 自带的 `gh` CLI。
- 默认工作流权限设为 `contents: read`；只有标签发布任务临时授予 `contents: write`。
- PR 构建不读取发布 Secrets，不使用 `pull_request_target` 构建不可信代码。
- 版本号必须来自 Cargo workspace，标签只负责声明版本，二者不一致时构建失败。
- 初版不缓存完整 `target/`，避免大型 Rust 构建缓存占满 Runner 磁盘；先只缓存 Cargo registry/git，观察耗时后再决定是否引入编译缓存。

## 已知风险

| 风险 | 影响 | 应对 |
|---|---|---|
| Windows Runner 镜像工具更新 | 偶发构建差异 | 固定 `windows-2025`，打印并检查工具版本 |
| 首次 Rust Release 构建较慢 | CI 时间增加 | 使用 Cargo 依赖缓存，暂不缓存整个 `target/` |
| 缺少代码签名证书 | SmartScreen/未知发布者提示 | 内测可暂时无签名，公开发布前补齐签名 |
| GitHub-hosted Runner 无真实桌面验收 | GUI、托盘问题可能漏检 | 正式发布前在真实 Windows 10/11 人工验收 |
| GPUI 或 Windows SDK 升级 | FXC 或构建逻辑变化 | 继续由 PowerShell 脚本动态查找 FXC，并在依赖升级后跑发布工作流 |

## 实施顺序

前置修正已完成：`make win-debug` 会显式传入 `--debug`。

1. 编写 `ramag.iss`，本地或 GitHub Runner 生成未签名安装器。
2. 编写 `package-windows.ps1`，封装构建、打包、校验和冒烟测试。
3. 增加 `windows-release.yml`，先只开放手动触发与 Artifact 下载。
4. 在真实 Windows 10/11 完成安装、升级、卸载和 GUI 验收。
5. 配置签名和受保护发布环境。
6. 启用 `v*` 标签自动创建 GitHub Release。

## 参考资料

- [GitHub Actions Runner Images](https://github.com/actions/runner-images#available-images)
- [Windows Server 2025 Runner 软件清单](https://github.com/actions/runner-images/blob/main/images/windows/Windows2025-Readme.md)
- [GitHub Actions 工作流语法](https://docs.github.com/en/actions/reference/workflows-and-actions/workflow-syntax)
- [GitHub Actions Artifact](https://docs.github.com/en/actions/tutorials/store-and-share-data)
- [GitHub Actions Secrets](https://docs.github.com/en/actions/how-tos/write-workflows/choose-what-workflows-do/use-secrets)
- [Microsoft：FXC 离线编译](https://learn.microsoft.com/en-us/windows/win32/direct3dtools/dx-graphics-tools-fxc-using)
- [Inno Setup 命令行编译器](https://jrsoftware.org/ishelp/topic_compilercmdline.htm)
- [Inno Setup SignTool](https://jrsoftware.org/ishelp/topic_setup_signtool.htm)
