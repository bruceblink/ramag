# Ramag 开发入门指南

本文面向第一次参与 Ramag 开发的贡献者，目标是帮助开发者先建立可运行基线，再沿一条真实功能调用链理解项目，而不是从头阅读全部 crate。

架构细节请配合阅读[架构说明](architecture.md)，主线排期请参考[主线开发计划](development-roadmap.md)，构建和发布流程请参考[桌面端构建与发布](desktop-release.md)。

## 1. 开始之前

Ramag 是一个 Rust 2024 Cargo workspace，桌面界面使用 GPUI。仓库通过 [`rust-toolchain.toml`](../rust-toolchain.toml) 固定 Rust nightly 版本，进入仓库后 rustup 会自动选择对应工具链。

### 1.1 通用依赖

- Git
- rustup
- 系统 Git，供 VCS 功能调用
- OpenSSH，供 SSH、SFTP 和数据库 SSH 隧道使用

### 1.2 平台构建依赖

Windows 统一使用 Visual Studio 18 2026 Build Tools，还需要：

- Visual Studio 18 2026 Build Tools 的 C++ 工作负载
- Windows 10/11 SDK

Windows 日常开发请从 Visual Studio 的 `Developer PowerShell for VS 2026` 启动终端，确保 MSVC 和 Windows SDK 已加入当前环境；环境准备好后，运行、检查和测试仍使用下文的同一组 `cargo` 命令。

macOS 还需要 Xcode Command Line Tools：

```bash
xcode-select --install
```

可以先检查主要工具是否可用：

```powershell
git --version
rustup show active-toolchain
rustc --version
cargo --version
ssh -V
```

## 2. 建立可运行基线

在修改代码前，先从当前分支启动未修改的应用。Windows、Linux 和 macOS 都执行同一条命令：

```text
cd ramag
cargo dev
```

第一次构建需要下载并编译 GPUI 等依赖，耗时会明显长于后续增量构建。启动成功后，建议实际操作一次以下页面：

1. 首页和设置
2. 数据库连接与查询
3. Git 仓库和工作区
4. SSH 连接、终端与 SFTP
5. macOS/Windows 上的剪贴板工具

这一步的目标是建立产品认知，并确认后续异常不是由本地基础环境造成的。

## 3. 项目结构

```text
ramag
├── Cargo.toml                 Cargo workspace、公共依赖和编译配置
├── crates/
│   ├── ramag-bin              程序入口、依赖装配和平台生命周期
│   ├── ramag-ui               GPUI 主壳、首页、设置、主题和共享组件
│   ├── ramag-tool-*           数据库、Git、SSH、剪贴板等工具界面
│   ├── ramag-app              应用服务和业务用例编排
│   ├── ramag-domain           领域实体、错误和抽象 trait
│   ├── ramag-infra-*          数据库、Git、SSH、存储等具体实现
│   └── ramag-terminal         GPUI 内嵌终端组件
├── docs/                      架构、性能、设计和发布文档
├── scripts/                   打包和数据库测试脚本
├── .github/                   CI 与桌面发布工作流
└── Makefile                   常用开发、检查、测试和打包入口
```

依赖方向遵循务实的 Clean Architecture：

```text
ramag-bin                    最外层组合入口
├── ramag-tool-* / ramag-ui  展示层
├── ramag-infra-*            基础设施适配器
└── ramag-app                用例编排
    └── ramag-domain         核心实体和接口
```

各层职责如下：

| 层 | 职责 |
|---|---|
| `ramag-domain` | 定义实体、统一错误和 `Driver`、`Storage`、`Tool` 等抽象接口 |
| `ramag-app` | 通过 Domain trait 编排业务用例，提供 Service 和 `ToolRegistry` |
| `ramag-infra-*` | 实现数据库、Git、SSH、剪贴板、本地存储和更新等外部能力 |
| `ramag-tool-*` | 实现用户可以直接操作的具体工具界面和交互 |
| `ramag-ui` | 提供 `Shell`、`HomeView`、设置、主题和共享 UI |
| `ramag-bin` | 创建具体实现、注入依赖、注册工具并启动 GPUI |

## 4. 程序启动链路

二进制目标由 [`crates/ramag-bin/Cargo.toml`](../crates/ramag-bin/Cargo.toml) 声明，包名为 `ramag-bin`，生成的程序名为 `ramag`，入口文件是 [`crates/ramag-bin/src/main.rs`](../crates/ramag-bin/src/main.rs)。

正常启动链路如下：

```text
cargo dev
    ↓
main()
    ↓
SSH askpass 特殊模式判断
    ↓
初始化日志和单实例控制
    ↓
创建 Storage、Driver 和应用 Service
    ↓
创建 ToolRegistry
    ↓
创建并运行 GPUI Application
    ↓
注册主题、快捷键、退出回调和平台后台任务
    ↓
open_main_window()
    ↓
创建 Home、Settings 和各 Tool View
    ↓
注册到 Shell 并创建 Root
```

启动相关代码主要分布在：

| 文件 | 内容 |
|---|---|
| [`main.rs`](../crates/ramag-bin/src/main.rs) | 日志、单实例、GPUI 生命周期、快捷键和后台任务 |
| [`composition.rs`](../crates/ramag-bin/src/composition.rs) | Driver、Storage、Service 和 Tool 的依赖装配 |
| [`windows.rs`](../crates/ramag-bin/src/windows.rs) | 主窗口、工具视图注册、托盘和重复启动唤起 |
| [`shell.rs`](../crates/ramag-ui/src/shell.rs) | ActivityBar、首页、设置和工具页面切换 |

`crates/ramag-bin/build.rs` 是 Cargo 编译期构建脚本，不是程序运行入口。

## 5. 推荐的代码阅读顺序

不要先阅读所有基础设施实现。建议按以下顺序理解项目：

1. 阅读[架构说明](architecture.md)，理解分层和依赖方向。
2. 从 [`main()`](../crates/ramag-bin/src/main.rs) 了解应用启动阶段。
3. 阅读 [`composition.rs`](../crates/ramag-bin/src/composition.rs)，了解具体实现如何注入 Service。
4. 阅读 [`windows.rs`](../crates/ramag-bin/src/windows.rs)，了解视图如何创建并注册到 `Shell`。
5. 阅读 [`Tool` trait](../crates/ramag-domain/src/traits/tool.rs) 和 [`ToolRegistry`](../crates/ramag-app/src/tool_registry.rs)。
6. 阅读 [`Shell`](../crates/ramag-ui/src/shell.rs)，理解页面导航和工具视图容器。
7. 选择一个具体工具，沿一次用户操作追踪到基础设施实现。

例如，一次 SQL 查询的主要调用方向是：

```text
查询按钮
→ ramag-tool-dbclient
→ ConnectionService
→ Driver trait
→ MysqlDriver / PostgresDriver
→ 查询结果
→ UI 渲染
```

这种纵向阅读方式比逐个 crate 横向通读更容易建立完整认识。

## 6. 根据开发方向定位代码

| 开发方向 | 首先阅读 | 后续调用链 |
|---|---|---|
| 公共界面、主题、主导航 | `ramag-ui` | `ramag-bin/src/windows.rs` |
| SQL 数据库 | `ramag-tool-dbclient` | `ConnectionService` → `Driver` → `ramag-infra-sql-*` |
| Redis | `ramag-tool-redis` | `RedisService` → `KvDriver` → `ramag-infra-redis` |
| MongoDB | `ramag-tool-mongodb` | `MongoService` → `DocDriver` → `ramag-infra-mongodb` |
| Git | `ramag-tool-vcs` | `GitDriver` → `ramag-infra-git` |
| SSH/SFTP | `ramag-tool-ssh` | `SshService` → `ramag-infra-ssh` → `ramag-terminal` |
| 剪贴板 | `ramag-tool-clipboard` | `ClipboardService` → `ClipboardDriver` → `ramag-infra-clipboard` |
| 本地存储和加密 | `ramag-infra-storage` | `Storage` trait 及使用它的 Service |
| 新增独立工具 | `Tool` 和 `ToolRegistry` | `composition.rs` → `windows.rs` |

### 6.1 新增 Tool 的典型改动范围

新增完整工具通常需要：

1. 新建 `ramag-tool-*` crate 并实现 `Tool`。
2. 将 crate 加入 workspace。
3. 在 `build_tool_registry` 中注册 Tool。
4. 在 `open_main_window` 中创建视图。
5. 使用 `Shell::register_tool_view` 注册视图。

这会同时涉及多个 crate，不建议作为第一次改动。

## 7. 第一个开发任务如何选择

第一次改动应尽量限制在一个 crate 内，例如：

- 调整一个已有页面的交互或布局
- 修复一个范围明确的小问题
- 为纯函数补充边界处理和单元测试
- 给现有 Service 增加一个不改变跨层接口约定的小能力

不建议从以下任务开始：

- 新增一种数据库类型
- 新增完整工具
- 重构 `Storage`
- 统一 SQL、KV、文档数据库等语义不同的接口
- 升级 GPUI 或重新解析整个 `Cargo.lock`

这些任务通常会同时穿过 Domain、App、Infra、Tool 和 Bin，需要先充分理解现有接口约定。

## 8. 开发时必须守住的边界

1. `ramag-domain` 不得依赖 GPUI、sqlx、redb、Redis 或具体 Infra crate。
2. `ramag-app` 依赖 Domain trait，不直接创建 MySQL、Redis 等具体实现。
3. 具体实现集中在 `ramag-bin` 组合入口中装配。
4. UI 通过 Service 完成业务操作，不绕过应用层直接访问数据库或存储。
5. SQL、KV、文档、Git 和 SSH 应保留符合自身语义的接口，不制造万能 Driver。
6. 只有在存在真实重复和共同约束时才增加共享层。
7. GPUI 使用 smol，数据库和 SSH 等基础设施使用各自的 Tokio runtime，不要在 UI 任务中直接调用依赖 Tokio reactor 的驱动代码。
8. 剪贴板、托盘和单实例逻辑包含平台条件编译，修改后要检查相关平台 target。

## 9. 日常开发与验证

### 9.1 修改前

确认工作树状态，避免混入无关改动：

```powershell
git status --short --branch
git log -10 --oneline
```

### 9.2 开发过程中

优先执行当前 crate 的快速检查。以下命令在三个平台的写法相同：

```text
cargo fmt --all
cargo check -p ramag-ui --all-targets
cargo test -p ramag-ui
cargo clippy -p ramag-ui --all-targets -- -D warnings
```

上例以 `ramag-ui` 为例，开发其他模块时替换为实际 crate 名称。

修改跨 crate 接口约定时，应同时测试直接生产者和消费者。

### 9.3 提交前

完整提交前检查为：

```text
cargo fmt-check
cargo check-all
cargo clippy-all
cargo test-all
```

`Makefile` 只作为兼容入口；`make develop`、`make release`、`make fmt-check`、`make check`、`make clippy` 和 `make test` 会转发或补充 Cargo 命令。打包和 Docker 数据库测试仍使用各自的脚本入口。

数据库基础设施改动还应运行真实数据库集成测试：

```bash
make db-test
```

该命令会通过 Docker 启动并填充 MySQL、PostgreSQL、Redis 和 MongoDB 测试环境。普通 UI 或纯 Domain 改动不需要每次重建数据库测试数据。

### 9.4 提交

每个可独立评审的修复或功能使用单独提交。提交前查看近期历史并保持仓库现有风格，例如：

```text
新增：增加开发入门指南
修复：处理数据库连接取消状态
优化：减少 Git 工作区重复刷新
重构：拆分 SSH 传输状态管理
```

提交前检查暂存内容：

```powershell
git diff --cached --stat
git diff --cached --check
```

## 10. 常见问题

### 首次编译很慢

GPUI 和相关 Git 依赖编译量较大。首次构建耗时长属于预期现象，后续修改应尽量使用单 crate 的 `cargo check` 和 `cargo test` 保持反馈速度。

### 修改 UI 后没有自动进入目标页面

确认 Tool 已在 `build_tool_registry` 注册，并在 `open_main_window` 中通过 `Shell::register_tool_view` 注册对应视图。仅实现 `Tool` 不会自动创建页面。

### 异步代码提示没有 Tokio reactor

GPUI 的异步执行环境不是 Tokio。数据库、Redis、MongoDB 和 SSH 已有各自的 runtime 与桥接层，应通过对应 Service 或 Infra 封装执行，不要从 UI 直接调用底层异步驱动。

### 集成测试被跳过

部分 Infra 测试需要真实数据库或 OpenSSH 环境。测试进程成功但用例被跳过，不代表完成真实环境验收，应根据改动范围补充相应环境后重新执行。

### 是否可以直接更新依赖

不要为了普通功能开发刷新整个 `Cargo.lock`。GPUI 与 `gpui-component` 的 Git 依赖和类型版本需要保持一致，依赖升级应作为单独任务处理并执行完整检查。

## 11. 开发检查清单

开始开发前：

- [ ] 未修改版本可以正常启动
- [ ] 已阅读架构说明和相关功能入口
- [ ] 已确认改动所属层和涉及的 crate
- [ ] 工作树中没有被误认为本次任务的无关改动

完成开发后：

- [ ] 没有破坏 Domain、App、Infra、Tool 的依赖方向
- [ ] 已运行相关 crate 的格式、检查、测试和 Clippy
- [ ] 跨层接口约定改动已覆盖生产者和消费者
- [ ] 外部数据库、SSH 或平台功能已说明真实环境验证情况
- [ ] 提交只包含一个可独立评审的功能或修复
