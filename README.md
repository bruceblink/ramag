<p align="center">
  <img src="scripts/icons/ramag.svg" width="112" alt="Ramag" />
</p>

<h1 align="center">Ramag</h1>

<p align="center">
  <strong>数据库、Git、SSH、云存储与剪贴板，一个真正本地优先的桌面工作台。</strong>
</p>

<p align="center">
  A local-first developer desktop workspace for databases, Git, SSH, object storage, and clipboard history.
</p>

<p align="center">
  MySQL · PostgreSQL · Redis · MongoDB · Git · SSH / SFTP · COS / OSS · Clipboard
</p>

<p align="center">
  Linux · macOS · Windows · Rust + GPUI · Local-first
</p>

<p align="center">
  <a href="https://github.com/tools-rs/ramag/releases">下载 / Releases</a> ·
  <a href="#快速开始">快速开始</a> ·
  <a href="#核心工作台">功能</a> ·
  <a href="docs/performance.md">性能</a> ·
  <a href="CONTRIBUTING.md">贡献</a> ·
  <a href="SECURITY.md">安全</a>
</p>

<p align="center">
  <img src="docs/screenshots/v0.0.5/home-light.png" alt="Ramag v0.0.5 首页：数据库、Git、SSH、云存储与剪贴板统一工作台">
</p>

---

## 项目状态 / Project status

Ramag 的四个主工作台——数据库、Git、SSH / SFTP 和云存储——已完成核心工作流并可用于日常开发工作；剪贴板为可选的本地效率工具。当前公开的 `0.0.x` 版本是功能预览 Release，项目正在整理稳定版发布所需的兼容性、签名和社区反馈。

The four primary workspaces—database, Git, SSH / SFTP, and object storage—are feature-complete for their core daily workflows. Current `0.0.x` releases are public feature-preview releases while Ramag prepares compatibility, signing, and community feedback for a stable release.

| 可验证交付 | 当前状态 |
|---|---|
| 支持平台 | Linux x86_64、macOS 12+（Apple Silicon / Intel）、Windows 10/11 x64 |
| 公开发布 | GitHub Releases 提供三平台安装包与 `SHA256SUMS.txt` |
| 质量门禁 | GitHub Actions 在 Linux、macOS、Windows 执行格式、编译、Clippy、测试与打包校验 |
| 数据边界 | 连接配置、凭据与剪贴历史保存于本机；Ramag 不提供托管服务，也不主动上传这些数据 |

Ramag is actively maintained. Contributions, reproducible feedback, and security reports are welcome; see [CONTRIBUTING.md](CONTRIBUTING.md) and [SECURITY.md](SECURITY.md).

## 为什么选择 Ramag

开发中最频繁的切换，不是语言或编辑器，而是数据、代码、远程主机和文件。Ramag 将这些本应关联的上下文放进一个原生窗口：查数据库时查看 Git 改动，通过 SSH 操作远程文件，管理已明确配置的对象存储，再按需找回剪贴上下文。

| 一个工作台 | 本地优先 | 面向真实数据量 | 原生交互 |
|---|---|---|---|
| 数据库、Git、SSH、云存储和剪贴板共享统一窗口与交互体系 | 数据库、SSH 和云存储凭据、剪贴历史加密后落本地，主密钥进入系统凭据库 | 百万级历史、十万文件与十万级数据库种子均有压力验证 | Rust + GPUI，耗时任务与 UI 线程隔离 |

```text
连接数据库处理数据  ↔  在 Git 中检查改动  ↔  通过 SSH 操作远程主机  ↔  管理云端对象  ↔  找回剪贴上下文
```

## 核心工作台

| 工作台 | 解决的问题 | 核心能力 |
|---|---|---|
| 数据库 | 在多种数据库间查询、编辑、迁移和同步 | MySQL、PostgreSQL、Redis、MongoDB；生产只读保护；JSONL/SQL 导入导出 |
| Git | 在不离开上下文的情况下检查和完成版本控制操作 | Diff、暂存、提交、分支、冲突处理、Rebase、Cherry-pick 与文件编辑 |
| SSH / SFTP | 连接远程主机并安全处理远程文件 | 系统 OpenSSH、内嵌终端、SFTP、传输队列、JumpServer 导入与生产写保护 |
| 云存储 | 面向指定 Bucket 管理对象，而非要求宽泛账号权限 | 腾讯云 COS、阿里云 OSS、上传下载、预览、收藏、传输进度与只读模式 |
| 剪贴板（可选） | 找回跨应用的短期上下文 | 本地加密历史、全局快捷键、隐私标记、来源应用黑名单与自动清理 |

## 快速开始

### 直接安装

前往 [GitHub Releases](https://github.com/tools-rs/ramag/releases)，按系统下载对应安装包：

| 系统 | 安装包 | 最低版本 |
|---|---|---|
| Apple Silicon Mac | `Ramag-*-macos-arm64.dmg` | macOS 12 |
| Intel Mac | `Ramag-*-macos-x86_64.dmg` | macOS 12 |
| Windows x64 | `Ramag-*-windows-x64-setup.exe` | Windows 10 |
| Linux x86_64 | `Ramag-*-linux-amd64.deb` / `Ramag-*-linux-x86_64.AppImage` | Ubuntu 24.04 或兼容发行版 |

> 项目仍处于 `0.0.x` 早期阶段。当前 Windows 安装包未做 Authenticode 签名，macOS 安装包未做 Developer ID 签名与 Apple 公证，系统可能显示未知发布者或安全警告。请只从本仓库 Releases 下载，并使用同一页面的 `SHA256SUMS.txt` 校验文件；完整状态见[桌面端构建与发布](docs/desktop-release.md#签名与公证状态)。

Git 功能需要系统已安装 `git`；SSH 管理、内嵌终端和数据库 SSH 隧道需要系统 OpenSSH。数据库、Git 仓库、SSH 凭据和剪贴板内容不会上传到 Ramag 服务。

### 从源码运行

准备 [Git](https://git-scm.com/)、[rustup](https://rustup.rs/) 和平台构建工具。仓库已通过 `rust-toolchain.toml` 固定 Rust nightly，进入目录后会由 rustup 自动选择，无需手动安装其它 Rust 版本。

macOS 还需要 Xcode Command Line Tools：

```bash
xcode-select --install
```

克隆并运行：

```bash
git clone https://github.com/tools-rs/ramag.git
cd ramag
make develop
```

Windows 源码构建统一使用 Visual Studio 18 2026 Build Tools，并需要其 C++ 工作负载与 Windows 10/11 SDK，然后在 PowerShell 中运行：

```powershell
cargo run -p ramag-bin
```

Linux 源码构建和打包所需系统依赖见[桌面端构建与发布](docs/desktop-release.md#本地-linux-打包)。首次构建需要下载 GPUI 等依赖，耗时会明显长于后续增量构建。

## 功能细节

### 数据库工作台

从连接、结构浏览、查询，到结果编辑和完整迁移，四类数据库共用一套清晰的工作流。

![MySQL、PostgreSQL、Redis 与 MongoDB 统一连接管理](docs/screenshots/v0.0.5/database-connections-light.png)

### MySQL 与 PostgreSQL

- Schema、表、视图、列、索引与 DDL 浏览。
- SQL 补全、高亮、多语句执行、光标语句执行、格式化与 EXPLAIN。
- 查询取消、结果分页、排序、筛选和单元格编辑。
- MySQL 与 PostgreSQL 图形化表设计器：创建或修改表名、字段结构，预览 DDL 后再执行。
- 大整数、高精度数值、JSON/JSONB、二进制、时间以及 PostgreSQL 原生类型保真展示。
- 表级 JSONL 导入导出与 Schema / 数据库级 SQL 导入导出；主键表使用 keyset 分页，深页不会反复跳过前置数据。

![Ramag MySQL 查询编辑器与十万行结果分页](docs/screenshots/v0.0.5/database-mysql-query-light.png)

### Redis

- 以 `:` 自动折叠 Key 命名空间，大型 Keyspace 使用游标 SCAN 和虚拟列表。
- String、Hash、List、Set、ZSet、Stream 六种类型统一查看与编辑。
- TTL 管理、大 String 有界加载、大集合自动分批继续加载。
- Key 树批量识别类型并展示紧凑标签；同一路径既是命名空间又是实际 Key 时，可在设置中开启“同名 Key 下沉展示”。
- 内置命令控制台；危险、阻塞和生产写命令在执行前识别。
- 整库 JSONL 迁移保留类型、TTL、顺序、分数、Stream ID 与二进制内容。

### MongoDB

- Database、Collection、索引、统计信息和文档浏览。
- `find`、`aggregate` 与通用命令，支持格式化、历史记录和多查询标签。
- 嵌套文档按 dotted path 展开，ObjectId、Decimal128、DateTime、Int64 等使用 Extended JSON 保真往返。
- 文档编辑、集合级 JSONL 和数据库级导入导出；混合类型 `_id` 使用 keyset 连续读取。

### 连接与数据安全

- TLS、三档证书验证、自定义 CA 与系统 OpenSSH 隧道。
- 连接测试与颜色标签。
- 数据库连接配置统一在全局设置中导入或导出，覆盖 MySQL、PostgreSQL、Redis 与 MongoDB；加密文件使用独立口令。
- 连接可标记为生产环境：写查询、结果编辑和导入入口统一进入只读保护。
- 结果搜索支持字符串 ID 与整数 ID 双向转换，内置 Base10、Base16、Base36、Base58 Bitcoin、Base58 Flickr 和自定义字符表，也可调用经过路径、超时与输出上限校验的外部转换器。
- SQL、Redis、MongoDB 使用独立执行 runtime，某个慢查询不会直接挤占其他数据库的任务线程。

![Ramag 数据库客户端设置与连接配置](docs/screenshots/v0.0.5/settings-database-light.png)

### 统一操作与快捷键

- 快捷键中心可查看、修改和重置应用快捷键；各工作台也提供统一的连接或仓库快速切换入口。
- 在支持的数据区域，macOS 使用 `⌘ + 双击`、Windows/Linux 使用 `Ctrl + 双击` 可复制当前已加载的完整值；普通双击仍用于打开、编辑或下钻。
- SQL、MongoDB、Redis、Git Diff/项目文件、SSH/SFTP、对象存储和剪贴板的复制行为与成功提示已统一。
- 所有可滚动区域保留鼠标滚轮和触控板滚动，但不显示可见滚动条，避免挤占内容空间。

### 四引擎数据同步

- 支持 MySQL、PostgreSQL、Redis 与 MongoDB 之间同类型连接的数据同步。
- 从真实元数据选择数据库、Schema、表或集合，执行前明确展示源、目标和覆盖范围。
- 同步前检查目标对象与依赖关系，并对写入已有库或 Schema 的操作再次确认。
- PostgreSQL 同步保留枚举与自定义类型依赖；Redis 保留类型与 TTL；MongoDB 保留 BSON 类型语义。
- 任务完成后汇总成功、跳过与失败对象，错误信息保留到具体对象和阶段。

### Git 工作台

Ramag 的 Git 体验围绕“看清改动，然后安全完成操作”展开，而不是把命令行按钮化。

> 执行关键 Git 写操作前，建议确认工作区状态并保留可恢复点。

```text
打开仓库 → 检查工作区 → 对照 Diff → Stage → Commit → Push / Pull
```

- Changes、Project Files、Stash、历史日志、Commit 详情、Blame 与 Reflog；文件树统一文件夹展开/收起和文件图标。
- Unified / Split Diff、整文件上下文、35 种语法高亮和超大 Diff 虚拟化。
- Stage / Unstage、Amend、Branch、Tag、Stash、Merge、Rebase、Cherry-pick。
- 冲突三栏处理，可继续或中止 Merge / Rebase / Cherry-pick 流程。
- 提交图、分支与远端状态、文件编辑和自动保存。
- Markdown 文件默认渲染为预览，可随时切换回原文；分支、远端、Tag 和 Stash 的操作集中在侧栏行菜单与右键菜单中。
- 支持从远程 URL 克隆到本地目录；各仓库分栏宽度在当前会话内相互隔离，重新启动后恢复默认布局。
- 文件监听按路径增量刷新；普通保存不重扫整个仓库。

写操作与网络认证直接复用系统 Git、SSH Agent 和用户已有配置，不在应用中再造一套不兼容的凭据体系。

![Ramag Git 仓库管理与仓库列表](docs/screenshots/v0.0.5/git-repositories-light.png)

![Ramag Git 工作区、项目文件与 SQL 编辑器](docs/screenshots/v0.0.5/git-workspace-light.png)

### SSH 管理

SSH 管理把连接配置、内嵌终端和远程文件浏览放在同一个工作区中，并继续复用系统 OpenSSH 的配置、主机校验和认证能力。

```text
选择连接 → 打开终端与 SFTP → 在远程目录间浏览、预览和传输文件
```

- 新建或编辑连接时可解析 `ssh user@host -p 22 -i /path/to/key`，支持密码、系统 SSH 配置和密钥认证；连接参数与密码使用本机主密钥加密保存。
- 原生内嵌终端支持多个标签、ANSI 样式和常用键盘输入；每个连接始终保留至少一个终端。
- SFTP 支持目录浏览、面包屑、名称搜索、文本预览与编辑、日志跟随、上传下载、目录传输和任务进度。
- 可将当前路径、目录或文件拖到终端区域，按对应目录创建一个新终端，不影响已有终端。
- JumpServer 导入支持保存多个加密登录、读取组织与资产树、选择授权账号，并生成可继续编辑和测试的 SSH 连接。
- 生产连接会禁止 SFTP 上传、编辑、重命名和删除；内嵌终端仍可使用，命令权限由远端账号和服务器策略负责。

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/screenshots/v0.0.2/ssh-workspace-dark.png">
  <img src="docs/screenshots/v0.0.2/ssh-workspace-light.png" alt="Ramag v0.0.2 SSH 内嵌终端与 SFTP 文件工作区">
</picture>

### 云存储工作台

云存储工作台面向用户明确配置的 Bucket，不要求账号具备列出全部 Bucket 的权限。

- 支持腾讯云 COS 与阿里云 OSS，凭据和工作区状态使用本机主密钥加密保存。
- 每个账号至少配置一个 Bucket、Region 和可选 Root Prefix；Endpoint 由服务商与 Region 自动生成。
- 支持目录面包屑与直接路径输入、名称模糊筛选、目录优先排序和路径收藏。
- 支持对象元数据、常见文本与 JSON 格式化查看，SFTP 与对象存储预览上限均为 50 MiB。
- 上传、下载、覆盖确认、取消和进度统一进入传输面板；重复任务只保留一条记录。
- 生产模式默认关闭；开启后隐藏上传、删除等写操作，只保留浏览、查看与下载。

### 剪贴板工作台

```text
⌘⇧V / Ctrl+Shift+V → 输入关键词或筛选类型 → Enter → 粘贴回原窗口
```

- 应用运行期间后台采集，无需保持剪贴板页面打开。
- 支持纯文本、富文本、链接、颜色、图片和文件路径。
- 记录来源应用，可加入黑名单；macOS 下遵循 Concealed / Transient 隐私标记。
- 复制、纯文本复制、自动切回来源应用并粘贴。
- 最近历史常驻有界缓存，完整历史与图片媒体在本地加密保存。
- 按数量和时间自动清理；图片使用缩略图、并发加载上限和内存预算。
- Windows 关闭主窗口后可驻留系统托盘，采集与全局快捷抽屉继续工作。
- 启用、采集、全局热键和清空全部历史统一放在全局设置中；关闭剪贴板后不显示工具入口，也不注册热键。

### Windows 系统设置

Windows 用户可打开“设置 → 系统设置”，启用“关闭时最小化到任务栏托盘”。启用后关闭主窗口只会隐藏窗口，Ramag 会继续在后台运行；点击托盘图标可重新打开窗口，使用托盘菜单中的“退出 Ramag”才会结束进程。关闭此开关后恢复默认的关闭行为。

![Windows 系统设置中的关闭到任务栏托盘开关](docs/screenshots/v0.0.5/settings-system-tray-windows.png)

| 剪贴历史与类型筛选 | 采集、自动粘贴与应用黑名单设置 |
|---|---|
| ![剪贴历史、搜索与类型筛选](docs/screenshots/v0.0.1/clipboard-history-light.png) | ![剪贴板隐私与采集设置](docs/screenshots/v0.0.1/clipboard-settings-light.png) |

剪贴板采集默认关闭，需要在设置中主动启用。常用快捷键：

| 操作 | macOS | Windows |
|---|---|---|
| 打开剪贴板抽屉 | `⌘⇧V` | `Ctrl+Shift+V` |
| 执行 SQL / MongoDB 查询 | `⌘Enter` | `Ctrl+Enter` |
| 执行光标所在 SQL | `⌘⇧Enter` | `Ctrl+Shift+Enter` |
| 新建查询标签 | `⌘T` | `Ctrl+T` |
| 格式化 SQL / MongoDB JSON | `⌘⇧F` | `Ctrl+Shift+F` |
| 在数据库、Git、SSH、云存储间切换 | `⌘1` / `⌘2` / `⌘3` / `⌘4` | `Ctrl+1` / `Ctrl+2` / `Ctrl+3` / `Ctrl+4` |

## 经得起数据量放大的性能设计

以下为 Apple M1 Max、Release 构建的代表性结果；数据库运行在本机 Docker 回环网络。

| 场景 | 实测结果 |
|---|---:|
| 当前仓库完整 VCS 刷新 | 16.217 ms 中位数 |
| 当前仓库单路径状态 | 11.779 ms 中位数 |
| 100,000 条 VCS 状态补丁合并 | 91.917 μs |
| 100,000 次提交图布局 | 2.553 ms |
| MySQL 100,005 行全库导出 | 871 ms，约 11.48 万行/s |
| PostgreSQL 100,004 行导出 | 884 ms，约 11.31 万行/s |
| MongoDB 125,102 文档导出 / 导入 | 1.761 s / 2.371 s |
| Redis 45,014 Key 完整保真导出 / 导入复核 | 1.569 s / 13.794 s |
| 1,000,000 条剪贴历史读取最近 500 条 | 5.513 ms 中位数 |
| 1,000,000 条剪贴历史完全无命中搜索 | 219.915 ms 中位数 |
| 剪贴板 500 × 4 KiB 即时过滤最坏样本 | 1.918 ms 中位数 |
| 4K 剪贴图片缩略图 | 58.603 ms，后台执行 |

这些数字背后是几条明确原则：能增量就不全量，能分页就不整库驻留，能虚拟化就不一次构造所有行，CPU 与 IO 重活不占用 UI 线程。

完整测试环境、P95、数据库吞吐、优化前后对照、索引磁盘取舍和复核方式，见 [性能报告](docs/performance.md)。百万条加密历史的常规深度搜索已从约 12.5 秒降到约 0.2 秒；极短查询与旧库首次后台建索引期间仍保留完整加密扫描作为正确性兜底。

## 本地优先，不等于只做“能跑”

- 数据库密码与敏感配置经 AES-256-GCM 加密后写入 redb。
- 主密钥保存在 macOS Keychain 或 Windows Credential Manager。
- 剪贴板正文、来源信息、原图和缩略图均以密文持久化。
- 导出文件采用临时文件、完整写入后原子替换，失败不会覆盖原文件。
- 查询结果、元数据、图片、剪贴历史、Redis 集合和导入行都有显式数量与字节预算。
- 外部命令、路径、连接标识和导入内容在进入执行层前校验，不静默吞掉错误。

应用数据写入操作系统标准用户数据目录，核心数据库文件名为 `ramag.redb`，日志位于同一数据目录的 `logs/ramag.log`。卸载应用不会自动删除用户数据库、凭据、剪贴板媒体或日志；需要清理时请先确认数据不再需要。

## 项目结构

Ramag 是一个 Rust 2024 Cargo workspace，采用务实的 Clean Architecture：业务规则在内层，数据库、Git、SSH、云存储、剪贴板与 GPUI 都是外层实现，依赖只能向内。

```text
ramag-bin              应用入口、依赖注入、快捷键与平台生命周期
├── ramag-tool-*       数据库、Redis、MongoDB、Git、SSH、云存储、剪贴板界面
├── ramag-ui           GPUI 主壳、主题和共享组件
├── ramag-infra-*      数据库、Git、SSH/SFTP、云存储、更新、剪贴板、隧道和本地存储适配器
├── ramag-terminal     GPUI 内嵌终端内核与视图
├── ramag-app          用例编排与工具注册
└── ramag-domain       实体和抽象接口，不依赖 GUI 或具体基础设施
```

这种分层让核心逻辑可以脱离 GUI 测试，也避免 SQL、KV、文档数据库和 Git 被塞进一个含义模糊的通用接口。详细依赖方向、各 crate 职责和扩展方式见[架构说明](docs/architecture.md)。

## 开发与验证

日常任务统一通过仓库 `Makefile` 执行，运行 `make` 可以查看完整列表：

| 命令 | 用途 |
|---|---|
| `make develop` | Debug 模式运行桌面应用 |
| `make release` | Release 模式在本机运行，不生成安装包 |
| `make check` | 检查所有 target 是否可编译 |
| `make fmt-check` | 检查 Rust 格式 |
| `make clippy` | 对所有 target 执行 Clippy，警告视为错误 |
| `make test` | 运行整个 workspace 测试 |
| `make db-test` | 用 Docker 启动并填充四类数据库，执行数据库测试与质量门禁 |

提交改动前建议依次运行：

```bash
make fmt-check
make check
make clippy
make test
```

外部数据库集成测试在缺少 `RAMAG_TEST_*` 环境变量时会自动跳过；需要完整验证时使用 `make db-test`，它会管理专用 Docker 容器、测试数据与本地测试凭据。`make db-test-clean` 会删除这些专用容器、数据卷和凭据，属于破坏性操作，请确认后再执行。

## 平台与文档

Ramag 支持 Linux x86_64、macOS 12+（Apple Silicon / Intel）和 Windows 10/11 x64。Linux 提供 Debian 安装包与 AppImage，并支持 X11 与 Wayland；Windows on ARM 仅计划通过系统 x64 模拟运行，尚未列为已完成人工验收的平台。

- [性能报告：VCS、数据库与剪贴板](docs/performance.md)
- [架构说明](docs/architecture.md)
- [桌面端构建与发布](docs/desktop-release.md)
- [版本变更记录](CHANGELOG.md)
- [贡献指南](CONTRIBUTING.md)
- [安全策略](SECURITY.md)
- [社区行为准则](CODE_OF_CONDUCT.md)

发现问题时，请在 [GitHub Issues](https://github.com/tools-rs/ramag/issues) 中附上操作系统、Ramag 版本、复现步骤和必要日志；提交前请移除连接地址、用户名、密码和业务数据。

## 交流群

欢迎加入 Ramag 官方交流群，交流使用体验、功能建议和问题反馈。群二维码有效期有限，过期后可扫码添加个人微信，再获取最新群二维码。

<table>
  <tr>
    <td align="center">官方交流群（二维码有效期有限）</td>
    <td align="center">个人中转二维码（群二维码过期后使用）</td>
  </tr>
  <tr>
    <td align="center"><img src="docs/community/group-qr.png" width="320" alt="Ramag 官方交流群二维码"></td>
    <td align="center"><img src="docs/community/personal-qr.png" width="320" alt="Ramag 个人中转二维码"></td>
  </tr>
</table>

## License

[Apache-2.0](https://www.apache.org/licenses/LICENSE-2.0)
