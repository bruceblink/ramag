# Ramag v0.0.1：用 Rust + GPUI 做了一个本地优先的开发者桌面工作台

大家好，我最近用 Rust 和 GPUI 开发了一个桌面应用：**Ramag**。

- GitHub：https://github.com/tools-rs/ramag
- Release：https://github.com/tools-rs/ramag/releases
- 架构说明：https://github.com/tools-rs/ramag/blob/main/docs/architecture.md
- 性能报告：https://github.com/tools-rs/ramag/blob/main/docs/performance.md

目前发布了早期版本 `v0.0.1`，支持 macOS Apple Silicon、macOS Intel 和 Windows x64。

Ramag 想解决的问题很直接：开发过程中，我们经常需要在数据库客户端、Git 工具和剪贴板历史工具之间来回切换。这些工具处理的数据彼此相关，却通常分散在多个窗口中。

因此，我尝试把它们整合到一个原生桌面工作台里：

```text
数据库工作台 + Git 工作台 + 剪贴板工作台
```

![Ramag 首页](https://raw.githubusercontent.com/tools-rs/ramag/main/docs/screenshots/home-dark-clipboard-enabled.png)

项目采用本地优先设计，不依赖浏览器壳，不要求登录账号，也不会把数据库连接、Git 仓库或剪贴板内容上传到 Ramag 服务。

## 一、目前包含哪些功能

### 1. 数据库工作台

当前支持四类数据库：

- MySQL
- PostgreSQL
- Redis
- MongoDB

连接管理、查询、结果浏览、数据编辑和导入导出都在同一个工作台中完成。

![四类数据库统一连接管理](https://raw.githubusercontent.com/tools-rs/ramag/main/docs/screenshots/database-connections-light.png)

#### MySQL 与 PostgreSQL

目前已经实现：

- Schema、表、视图、列、索引和 DDL 浏览
- SQL 高亮、补全和格式化
- 多语句执行和执行光标所在 SQL
- EXPLAIN 与查询取消
- 结果分页、排序和筛选
- 单元格编辑与查询历史
- 表级 JSONL 导入导出
- Schema 或数据库级 SQL 导入导出

类型展示方面，对大整数、高精度数值、JSON/JSONB、二进制、日期时间以及 PostgreSQL 原生类型做了保真处理，尽量避免在 UI 展示和导出过程中丢失精度。

大表读取使用分页和资源预算，不会一次性把整张表加载进内存。带主键的表在导出时优先使用 keyset 分页，避免深分页反复扫描前面的数据。

![MySQL 查询与十万行分页](https://raw.githubusercontent.com/tools-rs/ramag/main/docs/screenshots/database-mysql-query-dark.png)

#### Redis

Redis 部分目前支持：

- 使用 `:` 自动折叠 Key 命名空间
- SCAN 游标遍历和大型 Keyspace 虚拟列表
- String、Hash、List、Set、ZSet、Stream
- TTL 查看和修改
- 大 String 有界加载和大集合分批加载
- Key 新增、编辑和删除
- 内置 Redis 命令控制台
- 整库 JSONL 导入导出

导入导出会保留 Redis 数据类型、TTL、List 顺序、ZSet 分数、Stream ID 和二进制内容。命令控制台会对危险、阻塞和生产环境写命令进行识别，减少误操作风险。

#### MongoDB

MongoDB 部分目前支持：

- Database、Collection、索引和统计信息浏览
- `find`、`aggregate` 与通用命令
- 多查询标签、查询历史和 JSON 格式化
- 文档表格化展示和编辑
- Collection 级 JSONL 导入导出
- Database 级导入导出

嵌套对象会按 dotted path 展开。ObjectId、Decimal128、DateTime、Int64 等 BSON 类型使用 Extended JSON 往返，避免转换成普通 JSON 后丢失类型信息。

### 2. Git 工作台

Git 功能目前还是试验性功能，但已经覆盖了一套相对完整的日常工作流：

```text
打开仓库 → 查看工作区 → 检查 Diff → Stage → Commit → Push / Pull
```

当前支持：

- Changes、Project Files 和 Stash
- 提交历史、Commit 详情、Blame 和 Reflog
- Unified Diff 与 Split Diff
- Stage、Unstage 和 Amend
- Branch、Tag、Merge、Rebase 和 Cherry-pick
- 冲突处理
- 文件编辑与自动保存

Diff 和文件内容支持多种语言的语法高亮，大型 Diff 使用虚拟化展示。文件监听会尽量按变化路径增量刷新，普通文件保存不会直接触发整个仓库的完整扫描。

![Git 工作区、文件编辑与提交历史](https://raw.githubusercontent.com/tools-rs/ramag/main/docs/screenshots/git-workspace-dark.png)

Git 实现上没有完全重新实现一套凭据和网络认证体系。Ramag 使用 `gix` 发现仓库，同时让写操作和网络操作复用系统 Git、SSH Agent、Git 配置及已有凭据链。

这样做的主要考虑是兼容用户现有的 Git 环境，而不是在应用里再维护一套可能与命令行行为不一致的认证实现。

### 3. 剪贴板工作台

剪贴板模块支持：

- 纯文本、富文本、链接、颜色、图片和文件路径
- 来源应用记录
- 类型筛选与关键词搜索
- 纯文本复制
- 自动切回来源应用并粘贴
- 按数量和保存时间自动清理

全局快捷键：

```text
macOS：⌘⇧V
Windows：Ctrl+Shift+V
```

打开抽屉后，可以搜索历史记录，通过键盘选择并粘贴回原来的应用。剪贴板采集默认关闭，需要用户在设置中主动启用。

macOS 下会识别 Concealed 和 Transient 等剪贴板隐私标记。还可以配置来源应用黑名单，避免采集密码管理器等敏感应用的内容。

Windows 关闭主窗口后，Ramag 可以驻留系统托盘，剪贴板采集和全局快捷键仍可继续工作。

![剪贴板隐私与采集设置](https://raw.githubusercontent.com/tools-rs/ramag/main/docs/screenshots/clipboard-settings-light.png)

## 二、为什么选择 Rust + GPUI

这个项目最初就希望做成原生桌面应用，而不是 WebView 或 Electron 应用。

主要技术栈包括：

- Rust 2024
- GPUI 与 gpui-component
- sqlx
- redis-rs
- MongoDB 官方 Rust Driver
- gix
- redb
- aes-gcm
- tokio 与 smol

选择 Rust 的原因主要有三个。

第一，数据库结果、Git Diff、剪贴板图片等场景很容易碰到大数据量。Rust 可以让内存边界、并发模型和资源生命周期更加明确。

第二，项目需要同时接入数据库、Git、系统剪贴板、系统凭据库、文件监听和桌面窗口，Rust 在这类系统集成场景中比较合适。

第三，我希望把耗时操作和 UI 线程之间的边界设计清楚，而不是等界面卡顿后再到处补异步任务。

GPUI 的优势是原生渲染和 Rust 内部一致的状态管理模型。不过它目前仍在快速演进，依赖通常需要直接跟随 Git 版本，编译时间和 API 稳定性也是实际开发中必须面对的问题。

## 三、项目架构

Ramag 是一个由 18 个 crate 组成的 Cargo workspace，采用务实版本的 Clean Architecture。

整体依赖关系如下：

```text
ramag-bin
├── ramag-tool-*       功能界面
├── ramag-ui           GPUI 主壳与共享组件
├── ramag-infra-*      数据库、Git、剪贴板、隧道和存储适配器
├── ramag-app          用例编排
└── ramag-domain       实体与抽象接口
```

依赖方向只能向内。

### `ramag-domain`

领域层只定义实体和抽象接口，不依赖 GPUI、sqlx、Redis、MongoDB 或 redb。

不同数据模型使用不同接口：

- SQL 使用 `Driver`
- Redis 使用 `KvDriver`
- MongoDB 使用 `DocDriver`
- Git 使用 `GitDriver`
- 本地持久化使用 `Storage`

没有为了“统一”而把所有后端都塞进同一个通用 Driver。SQL、KV、文档数据库和 Git 的方法集合差别很大，强行统一通常会产生大量没有实际语义的 `NotImplemented`，最终反而让接口更难理解。

### `ramag-app`

应用层负责连接管理、数据库操作、导入导出、剪贴板采集和工具注册等业务用例编排。它只依赖领域接口，不知道具体数据库驱动和 GUI 实现。

### `ramag-infra-*`

基础设施层提供 MySQL、PostgreSQL、Redis、MongoDB、Git、剪贴板、SSH 隧道和 redb 本地存储的具体实现。

MySQL 和 PostgreSQL 之间还有一个 `ramag-infra-sql-shared`，集中处理 tokio runtime、连接池缓存、SQL 多语句切分、LIMIT 注入、错误映射和 Driver 模板实现，减少两个 SQL Driver 之间的重复代码。

### `ramag-tool-*` 与 `ramag-bin`

Tool 层承载数据库、Redis、MongoDB、Git 和剪贴板界面。最终由 `ramag-bin` 完成依赖注入、工具注册、快捷键绑定和平台生命周期管理。

## 四、异步运行时怎么处理

这个项目里有一个比较现实的问题：GPUI 内部使用 smol，而 sqlx、redis-rs 和 MongoDB Driver 依赖 tokio runtime。

如果直接在 GPUI 的执行环境中调用相关代码，可能因为找不到 tokio reactor 而出现运行时问题。目前分别维护了独立 runtime：

| Runtime | 用途 |
|---|---|
| smol | GPUI 事件循环 |
| tokio SQL runtime | MySQL 与 PostgreSQL |
| tokio Redis runtime | Redis |
| tokio MongoDB runtime | MongoDB |
| 有界线程池 | redb 与系统 Git 等同步操作 |

没有把所有数据库操作全部塞进同一个 tokio runtime。主要原因是 Redis 订阅、数据库长查询等任务的生命周期和负载差别较大，独立 runtime 可以减少某一类慢任务挤占其它数据库任务线程的情况。

同步 API 则通过固定上限的 worker pool 桥接到异步接口，避免高频操作不断创建新线程。

## 五、本地存储和数据安全

Ramag 使用 redb 保存数据库连接、查询历史、Git 仓库列表、用户偏好和剪贴板历史。

当前设计包括：

- 使用 AES-256-GCM 加密连接密码和敏感配置
- 主密钥存入 macOS Keychain 或 Windows Credential Manager
- 剪贴板正文、来源信息、原图和缩略图加密保存
- 导出文件通过临时文件完整写入后再原子替换
- 外部路径、连接标识和导入内容进入执行层前进行校验
- 日志文件设置大小上限并进行滚动

应用数据保存在操作系统标准用户数据目录中，核心数据库文件为 `ramag.redb`。卸载 Ramag 不会自动删除用户数据库、凭据、剪贴板媒体和日志，避免卸载或覆盖升级时误删用户数据。

## 六、性能方面做了什么

这个项目没有把“Rust 写的”直接等同于“自然就快”。

目前主要使用这些策略控制资源消耗：

- 增量刷新代替全量刷新
- 分页读取代替一次性载入
- keyset 分页代替深 OFFSET
- 虚拟列表代替一次构造所有行
- 大 String 和大集合分批加载
- 查询结果设置行数与字节预算
- 图片生成缩略图并限制并发加载数量
- CPU 和 IO 操作移出 UI 线程
- 剪贴板最近记录使用有界缓存
- Git 文件变化尽量按路径刷新

在 Apple M1 Max、Release 构建和本地 Docker 数据库环境中，部分测试结果如下：

| 场景 | 结果 |
|---|---:|
| 当前仓库完整 VCS 刷新 | 16.217 ms 中位数 |
| 当前仓库单路径状态 | 11.779 ms 中位数 |
| 100,000 条 VCS 状态补丁合并 | 91.917 μs |
| 100,000 次提交图布局 | 2.553 ms |
| MySQL 100,005 行导出 | 871 ms |
| PostgreSQL 100,004 行导出 | 884 ms |
| MongoDB 125,102 文档导出 / 导入 | 1.761 s / 2.371 s |
| Redis 45,014 Key 导出 / 导入复核 | 1.569 s / 13.794 s |
| 读取 100 万条剪贴历史中的最近 500 条 | 5.513 ms 中位数 |
| 100 万条剪贴历史完全无命中搜索 | 219.915 ms 中位数 |
| 4K 图片生成缩略图 | 58.603 ms，后台执行 |

这些数据不是跨设备的通用性能承诺，主要用于记录测试环境、发现退化，以及验证资源边界是否真的有效。完整测试方法、P95、数据库种子规模和优化对照可以查看项目中的性能报告。

## 七、如何运行

### 直接下载安装

可以从 GitHub Releases 下载：

https://github.com/tools-rs/ramag/releases

当前提供：

```text
Ramag-<version>-windows-x64-setup.exe
Ramag-<version>-macos-arm64.dmg
Ramag-<version>-macos-x86_64.dmg
SHA256SUMS.txt
```

支持范围：

- macOS 12+ Apple Silicon
- macOS 12+ Intel
- Windows 10/11 x64

当前暂不支持 Linux。Git 功能需要系统 Git，SSH 隧道需要系统 OpenSSH。

### 从源码运行

项目通过 `rust-toolchain.toml` 固定了 Rust nightly。macOS 需要先安装 Xcode Command Line Tools：

```bash
xcode-select --install
```

然后运行：

```bash
git clone https://github.com/tools-rs/ramag.git
cd ramag
make develop
```

Windows 需要 Visual Studio C++ Build Tools 和 Windows 10/11 SDK，然后可以运行：

```powershell
cargo run -p ramag-bin
```

常用质量检查命令：

```bash
make fmt-check
make check
make clippy
make test
```

如果本机有 Docker，还可以启动 MySQL、PostgreSQL、Redis 和 MongoDB 的完整集成测试环境：

```bash
make db-test
```

## 八、当前限制

这是一个 `0.0.x` 阶段的早期项目，还有不少需要继续验证和完善的地方。

目前已知的主要限制：

- Git 工作台仍属于试验性功能
- Windows 安装包尚未做 Authenticode 签名
- macOS 应用尚未做 Developer ID 签名和 Apple 公证
- Windows 可能显示未知发布者或 SmartScreen 提示
- macOS 下载后可能被 Gatekeeper 阻止
- Windows on ARM 尚未完成正式人工验收
- 当前不支持 Linux
- GPUI 仍在快速演进，升级可能带来接口兼容问题
- 首次源码编译需要下载并编译较多依赖，耗时较长

因此，现阶段更适合愿意尝鲜、反馈问题或参与开发的用户。涉及重要数据库或 Git 写操作时，也建议先确认目标环境并保留可恢复点。

## 九、希望获得哪些反馈

这是 Ramag 的第一个公开版本，希望听到 Rust 社区对以下问题的意见：

- Rust 原生桌面应用的交互和性能体验
- GPUI 在独立桌面工具中的实际使用体验
- Cargo workspace 和分层方式是否合理
- 多种异步 runtime 的隔离方式是否还有更合适的方案
- 数据库结果表格和大型列表的渲染体验
- Git 工作台还缺少哪些关键工作流
- Windows 和 macOS 上的兼容性问题
- 安装、首次启动和编译过程中遇到的问题

如果遇到问题，欢迎在 GitHub Issues 中附上操作系统及版本、Ramag 版本、可复现步骤和必要的错误日志。提交日志前请删除数据库连接、用户名、密码和业务数据。

## 十、项目链接

- GitHub：https://github.com/tools-rs/ramag
- Releases：https://github.com/tools-rs/ramag/releases
- Issues：https://github.com/tools-rs/ramag/issues
- 架构说明：https://github.com/tools-rs/ramag/blob/main/docs/architecture.md
- 性能报告：https://github.com/tools-rs/ramag/blob/main/docs/performance.md
- 构建与发布：https://github.com/tools-rs/ramag/blob/main/docs/desktop-release.md
- License：Apache-2.0

如果你也对 Rust 原生桌面应用、GPUI、数据库工具或 Git 可视化感兴趣，欢迎试用、提 Issue，或者一起参与完善 Ramag。
