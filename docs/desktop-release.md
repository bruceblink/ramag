# 桌面端构建与发布（Windows + macOS）

> 状态：已实施 Windows x64 安装包、macOS ARM64/Intel 独立安装包和统一 GitHub Actions 发布工作流；首次真实 Runner 打包需在提交后通过手动工作流确认。
>
> 更新日期：2026-08-04。
>
> 原则：本地负责开发与复现，对外桌面 Release 统一由 GitHub Actions 汇总并发布。

## 结论

构建与发布入口分工如下：

| 入口 | 用途 | 是否发布 |
|---|---|---:|
| `make release` | 本地运行当前平台的优化构建 | 否 |
| `make win-debug` | macOS 交叉编译 Windows debug，尽早发现编译问题 | 否 |
| `scripts/package-windows.ps1` | Windows 本机复现完整 Release 打包 | 否 |
| `make dmg-*` | macOS 本机生成指定架构的开发 DMG | 否 |
| `make mac-package` | macOS 本机复现 ARM64 与 Intel Release 打包 | 否 |
| `Desktop Release` Action | 并行构建两平台；`v*` 标签自动发布，也可指定已有标签手动重试 | 是 |

`make release` 不创建安装包，也不代表对外发布。

最终 GitHub Release 固定包含：

```text
Ramag-<version>-windows-x64-setup.exe
Ramag-<version>-macos-arm64.dmg
Ramag-<version>-macos-x86_64.dmg
SHA256SUMS.txt
```

- Windows 安装包提供当前用户安装、快捷方式、覆盖升级和卸载。
- macOS ARM64 包用于 Apple Silicon，x86_64 包用于 Intel Mac。
- 校验文件用于发现下载损坏；不能替代代码签名与公证。

## 版本唯一来源

版本来自根 `Cargo.toml`：

```toml
[workspace.package]
version = "0.0.2"
```

发布标签必须完全一致：

```text
Cargo version 0.0.2  →  tag v0.0.2
```

两平台脚本都通过 `cargo metadata --locked --no-deps` 读取唯一的 `ramag-bin` 版本。标签、应用版本或产物版本不一致时，发布会在上传前失败。

macOS 的 `CFBundleShortVersionString` 和 `CFBundleVersion` 按 Apple 要求只使用 SemVer 数字核心，例如 `1.2.3-beta.1` 对应 `1.2.3`；完整 Cargo 版本同时写入 `RamagCargoVersion`，并用于标签与文件名校验。

## 文件职责

```text
scripts/build-windows.ps1
    Windows 原生构建；负责 Rust、FXC、PE、版本资源和 DLL 依赖检查

scripts/package-windows.ps1
    Windows Release 打包；负责版本、Inno Setup、冒烟测试和 SHA-256

scripts/windows/ramag.iss
    Windows 安装器行为

scripts/windows/test-installer.ps1
    Windows 静默安装和卸载冒烟测试

scripts/windows/package.Tests.ps1
    Windows 版本与标签回归测试

scripts/build-dmg.sh
    macOS 指定架构构建；负责 APP、Info.plist、adhoc 签名和 DMG

scripts/package-macos.sh
    macOS Release 打包；负责分别构建 ARM64/Intel、挂载复验和 SHA-256

scripts/macos/release-lib.sh
    macOS 发布脚本共用的 Cargo 版本与标签校验

scripts/macos/package-tests.sh
    macOS 版本与标签回归测试

.github/workflows/desktop-release.yml
    两平台 CI 编排、Artifact 上传和 GitHub Release 汇总发布
```

工作流只负责调用平台脚本和汇总产物，不在 YAML 中复制平台打包逻辑。

## GitHub Actions 流程

```text
workflow_dispatch 或 v* 标签
              ↓
手动指定标签时检出该标签；留空则检出当前分支
              ↓
       两个平台并行构建
       ↙               ↘
windows-2025          macos-15 ARM64
Windows x64 安装包     macOS ARM64 + Intel DMG
安装/卸载冒烟测试      两个 APP/架构/DMG 挂载复验
       ↘               ↙
         上传独立 Artifact
                 ↓
v* 推送或手动指定 release_tag：进入 desktop-release Environment
                 ↓
复验两平台 SHA-256 → 生成合并校验文件
                 ↓
创建草稿 Release → 上传 3 个安装产物 → 正式发布
```

发布任务不 checkout 源码、不执行 Cargo，只下载已验证的 Artifact，并通过 GitHub API 读取远端注释标签的说明；它单独获得 `contents: write`。任一平台失败都不会发布不完整版本。

已正式发布的同名 Release 不会被覆盖；失败后遗留的草稿可以由同一标签工作流重试。所有 GitHub 官方 Action 都固定到完整 commit SHA。

## 如何使用

### 手动验证

在 GitHub 仓库中进入：

```text
Actions → Desktop Release → Run workflow
```

`release_tag` 留空时，结果会生成两份保留 14 天的 Artifact，不创建 GitHub Release：

- Windows Artifact：安装器、平台 SHA-256。
- macOS Artifact：ARM64 DMG、Intel DMG、平台 SHA-256。

首次启用工作流、修改构建脚本或升级 GPUI/Xcode/Windows SDK 后，都应先手动运行。

### 正式发布

1. 修改根 `Cargo.toml` 的 workspace 版本，并通过项目检查同步 `Cargo.lock`。
2. 完成本地质量门禁、手动 Action 和真实桌面验收。
3. 创建与 Cargo 版本一致的带注释标签，例如 `v0.0.2`；标签注释会作为 GitHub Release 说明。
4. 推送标签，等待两个平台均通过后自动发布。

如果标签已经存在，但发布任务因工作流自身问题失败，不要强制移动标签。在包含修复后的默认分支上手动运行 `Desktop Release`，将 `release_tag` 填为原标签（例如 `v0.0.2`）。工作流会检出并重新构建该标签，核对产物版本，从 GitHub API 读取标签注释后继续发布。

正式使用前必须在 GitHub 设置中完成：

- 创建名为 `desktop-release` 的 Environment，建议配置 Required reviewers，并禁止管理员绕过。
- 为 `v*` 建立 Tag Ruleset，限制谁可以创建、更新和删除发布标签。

这些保护属于 GitHub 仓库配置，不能只靠工作流 YAML 完成。

## 本地 Windows 打包

普通打包：

```powershell
pwsh ./scripts/package-windows.ps1
```

同时执行静默安装和卸载：

```powershell
pwsh ./scripts/package-windows.ps1 -SmokeTest
```

本地冒烟测试会拒绝覆盖正在运行或已经安装的 Ramag。输出位于：

```text
target/windows-dist/
```

### Windows 安装器约束

- 永久 `AppId=com.axemc.ramag`，首个公开版本发布后不得修改。
- 当前用户安装到 `%LOCALAPPDATA%\Programs\Ramag`，不请求管理员权限。
- 最低 Windows 版本为 Windows 10，应用架构为 x64。
- 安装界面支持英文和简体中文。
- 创建开始菜单快捷方式；桌面快捷方式默认不选中。
- 复用 `Local\RamagSingleInstanceMutex`，升级或卸载前要求退出 Ramag。
- 卸载程序文件和快捷方式，不删除用户数据库、凭据、媒体或日志。
- 安装包包含项目 `LICENSE`。
- Git 与 OpenSSH 是部分功能的外部运行时前提，不随安装包捆绑。

构建脚本会拒绝动态 MSVC/UCRT，以及未随包提供的非系统 DLL 依赖。

## 本地 macOS 打包

正式结构的 ARM64 与 Intel 双包打包：

```bash
make mac-package
```

平台开发包：

```bash
make dmg             # 当前架构
make dmg-x86         # Intel
make dmg-arm64       # Apple Silicon
```

`make mac-package` 输出位于：

```text
target/macos-dist/
```

### macOS 应用约束

- Bundle ID 永久为 `com.axemc.ramag`。
- 最低系统版本为 macOS 12.0；编译时同时设置 Mach-O deployment target。
- 正式发布分别生成单架构 DMG：ARM64 包只能包含 `arm64`，Intel 包只能包含 `x86_64`。
- DMG 内应用名固定为 `Ramag.app`，并提供指向 `/Applications` 的快捷方式。
- DMG 使用 macOS 原生 ULMO（LZMA）压缩；该格式要求 macOS 10.15+，低于应用要求的 macOS 12.0。
- APP 内包含 ICNS 图标和项目 `LICENSE`。
- 所有内容完成后才签名；当前使用 adhoc 签名，并执行 `codesign --deep --strict` 校验。
- DMG 创建后会校验文件系统、只读挂载并再次验证 APP、版本、架构和签名。

## 自动验证

### Windows

- 使用 `--locked` 构建 `x86_64-pc-windows-msvc` Release。
- Pester 覆盖版本转换、Cargo 元数据读取和标签匹配。
- 校验 FXC、Visual Studio 工具链、Inno Setup、PE x64、GUI 子系统和版本资源。
- 拒绝动态 CRT 与未打包的非系统 DLL。
- 验证安装器静默安装、版本和卸载。

### macOS

- 使用锁定 Rust nightly 分别构建 `x86_64-apple-darwin` 与 `aarch64-apple-darwin`。
- shell 回归测试覆盖 Cargo 版本、Bundle 版本和标签匹配。
- 校验 Info.plist、Bundle ID、完整 Cargo 版本和 macOS 12.0 deployment target。
- 使用 `lipo` 确认两个应用各自只包含目标架构。
- 严格验证 APP adhoc 签名。
- 验证 DMG 文件系统，挂载后再次检查 APP 和 `/Applications` 快捷方式。

### 发布汇总

- 两个平台分别生成并校验 SHA-256。
- 发布任务再次校验平台清单，再生成包含全部三个产物的 `SHA256SUMS.txt`。
- 只有两个平台都成功，标签发布任务才会启动。

## 仍需人工验收

### Windows

- Windows 10 x64 与 Windows 11 x64 的安装、覆盖升级和卸载。
- 开始菜单、桌面快捷方式、单实例、托盘和全局快捷键。
- GPUI DirectX 渲染、Credential Manager 和剪贴板监听。
- 若声明支持，验证 Windows 11 ARM64 的 x64 模拟运行。

### macOS

- Intel Mac 与 Apple Silicon Mac 的安装和首次启动。
- Dock 图标、关窗保活、重新打开、全局快捷键和单实例行为。
- GPUI Metal 渲染、Keychain、剪贴板监听和系统权限提示。
- 升级后用户数据库、凭据和日志保持不变。
- 完成正式签名与公证后，在带 quarantine 属性的真实下载文件上验证 Gatekeeper。

## 签名与公证状态

当前对外产物尚未达到无警告分发状态：

- Windows EXE 和安装器未做 Authenticode 签名，可能显示未知发布者或 SmartScreen 警告。
- macOS APP 只有 adhoc 签名，DMG 未做 Developer ID 签名和 Apple 公证，下载后可能被 Gatekeeper 阻止。

macOS 正式签名与公证应按以下顺序接入：

1. 从受保护 Environment 临时导入 Developer ID Application 证书。
2. 使用 Hardened Runtime 签名 `Ramag.app` 并严格验证。
3. 创建并签名 DMG。
4. 使用 `xcrun notarytool submit --wait` 提交公证。
5. 使用 `xcrun stapler staple` 附加票据，并执行 Gatekeeper 验证。
6. 最后生成 SHA-256，再交给无签名凭据的发布任务。

Windows 应在受保护任务中签名应用、Inno Setup 安装器和卸载器，并添加 RFC 3161 时间戳。两平台的证书、口令、Apple ID、API Key 或 OIDC 参数都不得写入仓库。

## Inno Setup 许可边界

仓库未配置 Inno Setup 商业许可证。标准编译器仍可完成构建，但可能显示 `Non-commercial use only`。Inno Setup 官方请求符合其定义的商业用户购买许可证，同时说明这并非严格强制；Ramag 如果进入商业使用，应在正式发布前自行确认许可需求。

简体中文翻译固定自 Inno Setup 官方仓库提交 `683ee7e`，同目录保留上游许可，避免依赖 Runner 的可选语言文件。

## 构建环境边界

- Windows 使用明确的 `windows-2025` x64 Runner。
- macOS 使用明确的 `macos-15` ARM64 Runner，并在该机器上交叉构建 Intel 切片。
- 两个平台都使用 `rust-toolchain.toml` 锁定的 nightly，只缓存 Cargo registry/git，不缓存完整 `target/`。
- Runner 标签内部的 Visual Studio、Windows SDK、Xcode 和系统工具仍会滚动更新，因此不是字节级可复现构建。

工具链或 GPUI 升级后，必须重新运行手动 Action 和真实桌面验收。

## 参考资料

- [GitHub Actions Runner Images](https://github.com/actions/runner-images#available-images)
- [macOS 15 ARM64 Runner 清单](https://github.com/actions/runner-images/blob/main/images/macos/macos-15-arm64-Readme.md)
- [Windows Server 2025 Runner 清单](https://github.com/actions/runner-images/blob/main/images/windows/Windows2025-VS2026-Readme.md)
- [GitHub Actions 工作流语法](https://docs.github.com/en/actions/reference/workflows-and-actions/workflow-syntax)
- [GitHub Environments](https://docs.github.com/en/actions/how-tos/deploy/configure-and-manage-deployments/manage-environments)
- [GitHub Rulesets](https://docs.github.com/en/repositories/configuring-branches-and-merges-in-your-repository/managing-rulesets/about-rulesets)
- [Apple CFBundleVersion](https://developer.apple.com/documentation/bundleresources/information-property-list/cfbundleversion)
- [Apple CFBundleShortVersionString](https://developer.apple.com/documentation/bundleresources/information-property-list/cfbundleshortversionstring)
- [Apple Notarizing macOS software before distribution](https://developer.apple.com/documentation/security/notarizing-macos-software-before-distribution)
- [Inno Setup 商业许可证说明](https://jrsoftware.org/isorder.php)
