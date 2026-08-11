# Ramag 架构说明

## 设计目标

1. **可扩展**：从一开始就支持多种数据源与工具（当前 MySQL / PostgreSQL / Redis / MongoDB / Git / SSH / SFTP / COS / OSS）
2. **可演化**：未来加入新工具（不只是数据库）不需要重构 domain / app 层
3. **可测试**：核心业务逻辑能脱离 GUI 单独测试
4. **可维护**：模块边界清晰，依赖方向单一

## 架构思想

**Clean Architecture 务实版**——保留分层与依赖方向铁律，不强求 4 层、不每个 use case 都拆 Input/Output Port、不引入过度间接层。

### 分层与依赖方向

```
ramag-bin                          ← 入口：依赖注入 + 启动 GPUI
  ├── ramag-tool-dbclient                  ← DB Client 视图（SQL + Redis + MongoDB 共用入口）
  ├── ramag-tool-redis                     ← Redis 专属视图（key 树 / 详情）
  ├── ramag-tool-mongodb                   ← MongoDB 专属视图（collection 树 / 文档表格），由 dbclient 装载
  ├── ramag-tool-vcs                       ← VCS（Git）可视化视图
  ├── ramag-tool-ssh                       ← SSH 连接、内嵌终端、SFTP 与 JumpServer 导入
  ├── ramag-tool-clipboard                 ← 剪贴历史与详情视图
  ├── ramag-tool-object-storage             ← COS/OSS 账号、Bucket、对象与传输视图
  ├── ramag-ui                             ← Shell + ActivityBar + 主题
  ├── ramag-infra-mysql       impl SqlBackend
  ├── ramag-infra-postgres    impl SqlBackend
  ├── ramag-infra-sql-shared           ← SqlBackend trait + 模板 + tokio runtime
  ├── ramag-infra-redis       impl KvDriver
  ├── ramag-infra-mongodb     impl DocDriver
  ├── ramag-infra-git         impl GitDriver
  ├── ramag-infra-ssh         impl SshDriver + JumpServerDriver
  ├── ramag-terminal                       ← GPUI 终端解析、输入编码与绘制
  ├── ramag-infra-clipboard   impl ClipboardDriver
  ├── ramag-infra-object-storage             ← OpenDAL COS/OSS 数据面、专用 runtime
  ├── ramag-infra-tunnel                   ← 数据库系统 OpenSSH 隧道
  ├── ramag-infra-storage     impl Storage（redb + aes-gcm + 系统凭据库）
  └── ramag-app                            ← Use Cases + ToolRegistry
        └── ramag-domain                   ← 实体 + traits（无 GPUI / sqlx / redb / redis / mongodb 依赖）
```

### 铁律

1. **依赖方向单一**：永远向内/向下，禁止反向依赖
2. **Domain 纯净**：仅依赖 serde / thiserror / async-trait / chrono / uuid / futures
3. **接口先于实现**：跨层调用通过 Domain 定义的 trait（`Driver` / `KvDriver` / `DocDriver` / `GitDriver` / `SshDriver` / `JumpServerDriver` / `ClipboardDriver` / `Storage` / `Tool`）

## Crate 详解

### `ramag-domain`（核心）

**职责**：定义实体 + trait 抽象。

**关键内容**：
- `entities/`：数据库连接与结果、Redis / MongoDB 数据模型、Git 仓库模型、SSH 配置与传输、JumpServer 资源、剪贴条目、ID 转换配置等
- `traits/`：`Driver`（SQL）、`KvDriver`（Redis）、`DocDriver`（MongoDB）、`GitDriver`（Git）、`SshDriver`、`JumpServerDriver`、`ClipboardDriver`、`ObjectStorageDriver`、`Storage`、`Tool`
- `error.rs`：统一错误 `DomainError`

**为什么不让 `Driver` 涵盖一切**：SQL / KV / 文档 / Git / SSH 等后端的方法集差异大（`execute` vs `get_value` vs `find` vs `commit` vs `list_directory`），强合并会让一侧充斥 NotImplemented，破坏语义清晰度。

### `ramag-app`（应用层）

**职责**：编排 Domain trait 完成业务用例。

**关键内容**：
- `ConnectionService`：SQL 侧 facade，按 `config.driver` 自动分发到 MySQL / Postgres
- `RedisService`：Redis 侧 facade
- `MongoService`：MongoDB 侧 facade（连接 CRUD + 文档操作 + 查询历史，与 SQL 共用同一张 history 表）
- `SshService`：SSH 配置、连接测试、SFTP、传输队列与 JumpServer 导入编排
- `ObjectStorageService`：COS/OSS 账号生命周期、必填 Bucket 挂载验证、对象分页、只读门禁、传输队列与加密工作区
- `id_conversion`：结果搜索使用的双向 ID 转换，隔离内置算法和外部进程边界
- `ToolRegistry`：管理已注册的 Tool

### `ramag-infra-sql-shared`（SQL 共享层）

**职责**：MySQL / Postgres / 未来 SQLite 等所有关系型 driver 的唯一抽象层。

**关键内容**：
- `SqlBackend` trait：每个 driver 仅 impl 这一个，方言/取消 SQL/池构造/row 解码全在这里
- `impl_driver_for!` 宏：一行从 `SqlBackend` 生成 Domain `Driver` 实现
- `runtime.rs`：所有 SQL driver 共用的 tokio multi-thread runtime（2 worker）
- `pool.rs`：泛型 `PoolCache<Db>`
- `sql.rs`：多语句切分、LIMIT 注入

**收益**：MySQL / Postgres 各自 lib.rs ~170 行，不重复实现 sqlx 错误映射 / 多语句切分 / cancel handle 等模板。

### `ramag-infra-mysql` / `ramag-infra-postgres`

仅实现 `SqlBackend`。MySQL 用反引号 + `KILL QUERY`，Postgres 用双引号 + `pg_cancel_backend(pid)`、强制连接到具体 db。

### `ramag-infra-redis`

实现 `KvDriver`，封装 redis-rs 的 `aio::ConnectionManager`（自动重连）。

**连接缓存按 `(ConnectionId, db)` 维度**——Redis SELECT 是连接级状态，不能跨 db 复用。

**独立 tokio runtime**：与 SQL 共享会让 SQL 长查询挤占 Redis Pub/Sub 流。

### `ramag-infra-mongodb`

实现 `DocDriver`，封装 mongodb 官方驱动（`Client` 自带连接池 + 自动重连，clone 仅是 Arc 复制）。

**连接缓存按 `ConnectionId` 维度**——与 SQL 一致，**不像** Redis 要带 db：MongoDB 的 db 切换是命令级而非连接级。

**BSON ↔ Extended JSON 双向映射**：文档统一用 `serde_json::Value`，`types.rs` 负责 `ObjectId → {"$oid"}` / `Decimal128 → {"$numberDecimal"}` / `DateTime → {"$date"}` 等转换。

**独立 tokio runtime**：桥接形态同 Redis，避免某类数据库的长查询挤占其它类的查询。

### `ramag-infra-git`

实现 `GitDriver`：[`gix`](https://github.com/Byron/gitoxide) 负责仓库发现，status / diff / log / 分支与写操作复用系统 Git，保证与用户 Git 配置和凭据链兼容。

**同步 → async 桥接**：固定上限的 worker pool + `futures::oneshot` 派发，避免高频刷新反复创建线程，**不需要 tokio**。

**仓库路径与写锁按 `RepoId` 缓存**；只串行化写操作，status / 分支等只读查询可并发执行。文件监听优先走路径级 status，只有 Git refs 变化才刷新分支。

### `ramag-infra-ssh` / `ramag-terminal`

`ramag-infra-ssh` 实现 `SshDriver` 与 `JumpServerDriver`。终端命令和主机校验复用系统 OpenSSH；结构化文件操作通过 SFTP 会话完成，上传下载使用临时目标与提交阶段，避免失败时直接破坏原文件。SSH 使用独立 Tokio runtime，避免终端、目录读取和传输任务占用数据库 runtime。

`ramag-terminal` 是不依赖具体 SSH 后端的 GPUI 终端组件，负责 ANSI 状态、终端快照、键盘序列编码和绘制。工具层只负责终端生命周期与 SSH 进程 IO 桥接。

生产 SSH 配置当前只在领域层和基础设施层禁止 SFTP 写操作；终端命令仍由远端账号权限约束。更严格的低影响诊断模式仍属于独立设计目标，不能与现状混同。

### `ramag-infra-clipboard`

实现 `ClipboardDriver`，对接 macOS / Windows 系统剪贴板与来源应用。采集开关、全局热键和历史清理由应用层与全局设置统一编排；正文和媒体通过 `Storage` 加密持久化。

### `ramag-infra-storage`

实现 `Storage` trait：数据库连接、SSH 配置、查询历史、偏好 KV、Git 仓库列表、云对象存储账号与剪贴历史。

对象存储账号、每账号工作区偏好和全局会话偏好使用同一主密钥加密；账号删除通过单事务同时删除账号记录和对应的每账号工作区偏好，全局会话在 UI 保存或下次恢复时过滤已删除账号。

### `ramag-infra-object-storage`

对象存储基础设施只负责已配置 Bucket 的数据面操作，不请求账号级列桶 API。`OperatorCache` 使用 OpenDAL 访问 Bucket 内对象；Endpoint 根据服务商和用户填写的 Region 生成并经过官方 HTTPS 域名白名单校验。OSS 数据面通过受限 Reqwest transport 明确使用 V4 签名。HTTP 统一使用 `rustls-no-provider`，由 `ramag-bin` 组合根在创建任何 Reqwest Client 前安装进程级 `ring` Provider，不引入 OpenSSL 或 AWS-LC。

该 Crate 持有 2 worker 的独立 Tokio runtime、最多 32 个 Operator 的 LRU 缓存和有 TTL 的 Ramag 游标缓存。传输使用临时文件提交；Windows 覆盖下载通过 `MoveFileExW` 原子替换，避免先删除旧文件形成数据丢失窗口。关闭应用时由 `ObjectStorageService::shutdown` 有界停止 runtime。

### `ramag-tool-object-storage`

GPUI 视图提供账号搜索、加密会话恢复、必填 Bucket 挂载、地域分组、当前目录名称筛选、加密收藏路径、对象元数据详情、悬浮传输进度、多任务取消和覆盖确认。对象列表通过 `uniform_list` 行级虚拟化；上传、下载和删除按 OpenDAL 实际能力门控。OpenDAL 不可无损表示的远端 Key 仍可见，但所有数据面操作按钮均禁用。

**安全**：
- 主密钥由 `keyring` crate 以 `ramag` / `master-key` 存入 macOS Keychain / Windows Credential Manager，首次启动自动生成
- 数据库和 SSH 密码字段用 `aes-gcm` 加密后才落 redb
- 测试通过 `open_with_key(&path, &key)` 注入固定密钥，不污染真实系统凭据库

**所有服务共用同一个 Storage 实例**——数据库连接按 `DriverKind` 过滤，SSH 配置、Git 仓库和剪贴历史使用各自的存储表，彼此不混用。

### `ramag-tool-dbclient`

DB Client 主视图（SQL + Redis + MongoDB 共用入口）。新建连接表单内通过 driver 选择器决定路径，按 `DriverKind` 分发到 `SessionEntity::Sql`（MySQL + Postgres 共用 `ConnectionSession`）/ `SessionEntity::Redis` / `SessionEntity::Mongo`（装载 `ramag-tool-mongodb` 的视图）。

包含：连接列表、连接表单、表树、SQL 编辑器（含补全）、查询面板、结果集表格（行内编辑 / 排序 / 过滤 / 导出）、查询历史。

### `ramag-tool-redis`

Redis 专属视图：DB 切换、Key 树（Trie 命名空间分组 + uniform_list 行级虚拟化）、Key 详情（按 6 类型分发渲染：String / List / Hash / Set / ZSet / Stream）、新建 Key 对话框。

### `ramag-tool-mongodb`

MongoDB 专属视图，由 dbclient 在 `SessionEntity::Mongo` 装载（**非独立 Tool**）。左 collection 树（Database → Collection 双层）+ 右多 Tab 查询面板。runCommand 风格 JSON 编辑器（`{"find":..., "filter":...}` / `{"aggregate":..., "pipeline":[...]}`），文档结果表格化（嵌套 object 扁平化为 dotted path）。提供 `RunMongoQuery` / `NewMongoQueryTab` / `FormatMongoJson` / `ToggleMongoEditor` 四个 Action，靠 `KeyContext` 复用 SQL 键位而不冲突。

### `ramag-tool-vcs`

Git 客户端，IDEA 风格三栏布局：仓库管理页 / 工作区（Changes / Project Files / Stash）/ 历史日志 / Commit 详情 / Diff 视图（unified + split）/ Blame / Reflog / 冲突编辑器 / Interactive Rebase。

### `ramag-tool-ssh`

SSH 管理视图：连接列表与 SSH 命令解析、JumpServer 资源导入、内嵌多终端、SFTP 目录浏览、文件预览与编辑、上传下载及传输队列。一个连接对应一个独立工作区；终端和分栏状态不在不同连接间共享。

### `ramag-tool-clipboard`

剪贴历史列表与详情视图。工具是否注册为可见入口由全局设置控制；关闭时同时停止后台采集并释放全局热键。

### `ramag-ui`

主壳：`Shell`（左 ActivityBar + 中央 Tool 视图）、`HomeView`（首页）、主题（VSCode 风暗/亮色板）、`RamagAssets`（rust-embed 内嵌 svg + 上游 gpui-component-assets 兜底）。

### `ramag-bin`（主入口）

依赖注入中心：
1. `logging::init`：默认 `info`（可用 `RUST_LOG` 覆盖），stderr + 文件双路输出；日志超过 10 MiB 时保留一份滚动备份
2. `build_connection_service`：装配 `MysqlDriver` + `PostgresDriver` 进 `HashMap<DriverKind, Arc<dyn Driver>>` + `RedbStorage`
3. `build_redis_service` / `build_mongo_service`：分别装配 `RedisDriver` / `MongoDriver`，复用同一 Storage
4. `build_tool_registry`：注册 `DbClientTool` + `VcsTool` + `SshTool` + `ObjectStorageTool` + `ClipboardTool`；剪贴板默认关闭时保留实例但隐藏入口
5. `app.on_reopen`：macOS 无窗口时从 Dock 重开；Windows 走系统托盘常驻（关窗采集不停，托盘唤回/退出；托盘安装失败回退关窗即退）+ 单实例（双开唤起已有实例）
6. `cx.bind_keys`：使用 GPUI `secondary-*` 注册跨平台主修饰键（macOS Command / Windows Ctrl）

## 关键技术决策

### 1) 多 Runtime 桥接

GPUI 内部用 smol，sqlx / redis-rs / mongodb / SSH 基础设施依赖 tokio，**直接调用会 panic**（找不到 tokio reactor）。数据库类型和 SSH 各自持有独立 runtime，避免长查询、长连接或文件传输互相挤占任务线程。

| Runtime | 用途 | 来源 |
|---------|------|------|
| smol | UI 事件循环 | GPUI 内部 |
| tokio (SQL) | sqlx 查询 | `ramag-infra-sql-shared::runtime`（MySQL + Postgres + 未来 SQLite 共用，2 worker） |
| tokio (Redis) | redis-rs 操作 | `ramag-infra-redis::runtime`（独立 2 worker） |
| tokio (MongoDB) | mongodb 文档操作 | `ramag-infra-mongodb::runtime`（独立 2 worker） |
| tokio (SSH) | SFTP、JumpServer 与传输任务 | `ramag-infra-ssh::runtime`（独立 3 worker） |
| tokio (Object Storage) | COS/OSS 对象访问与传输 | `ramag-infra-object-storage::runtime`（独立 2 worker） |
| std::thread | redb / 系统 Git 同步 API | `Storage` 与 `GitDriver` 各自的 `run_blocking` |

**为什么分开**：Redis Pub/Sub、SSH 会话与传输是长生命周期任务，不应被 SQL 长查询挤占；MongoDB 同理独立一份。同种类型 driver（如多个 SQL）共享则合理。

### 2) GPUI / gpui-component 不钉 git rev

钉 rev 会让 ramag 与 gpui-component 各自引用一份 `zed`，类型不互通（`Hsla` 等会被 cargo 当成两个不同类型，编译百余个错）。版本固定靠 `Cargo.lock`。

升级流程：`cargo update -p gpui` + 同步检查 workspace 钉的 `lsp-types` / `ropey` 是否与 gpui-component 内部一致——不一致会因 `InputState` LSP 接口类型不兼容而编译失败。

### 3) `redis` crate features 缺一不可

```toml
features = ["aio", "tokio-comp", "tokio-rustls-comp", "tls-rustls-webpki-roots", "connection-manager"]
```

- 缺 `tokio-rustls-comp`：编译报 `connect_tcp_tls` 缺实现
- 缺 `connection-manager`：`PoolCache` 没有自动重连句柄

### 4) `mongodb` crate features 缺一不可

```toml
features = ["rustls-tls", "bson-2", "compat-3-3-0"]
```

- `bson-2`：与 workspace 单列的 `bson = "2"` 对齐，否则两份 bson 类型不互通（同 GPUI 双份 zed 的坑）
- `compat-3-3-0`：mongodb 3.3+ 强制要求显式接受的兼容性约束，缺了编译报错
- `rustls-tls`：走 rustls，与项目其它 TLS 选型一致，避免引入 openssl

### 5) Release Profile 极致优化

`lto = "fat"` + `codegen-units = 1` + `panic = "abort"` + `strip = true`——编译变慢但运行最快、二进制最小。

## 添加新功能的扩展指南

### 加新 Tool（如 JSON 格式化）

1. 新建 `crates/ramag-tool-jsonfmt/`，实现 `Tool` trait
2. 在 `Cargo.toml` 的 `members` 添加该 crate
3. 在 `ramag-bin/src/main.rs` 的 `build_tool_registry` 注册一行
4. 在 `open_main_window` 注册视图工厂到 `Shell::register_tool_view`

**不动 domain / app**——这就是 Clean Architecture 带来的扩展性。

### 加新 SQL 数据库（如 SQLite）

1. 新建 `crates/ramag-infra-sqlite/`，实现 `ramag-infra-sql-shared::SqlBackend` trait
2. crate 末尾写 `ramag_infra_sql_shared::impl_driver_for!(SqliteDriver);` 宏一行
3. 在 `ramag-domain` 的 `DriverKind` 枚举加 `Sqlite` 变体
4. 在 `ramag-bin/main.rs` 的 `build_connection_service` 把 driver 注册进 `HashMap<DriverKind, Arc<dyn Driver>>`

**dbclient 视图层无需改动**——SQL 类共用 `ConnectionSession`。

### 加新 KV 数据库（如 KeyDB / DragonflyDB）

实现 `KvDriver` trait（参考 `ramag-infra-redis`），**不要塞进 `Driver`**——方法集差异大会导致大量 NotImplemented。

### 加新文档数据库（如 CouchDB / DynamoDB）

实现 `DocDriver` trait（参考 `ramag-infra-mongodb`），**不要复用 `KvDriver`**——文档模型的 `find` / `aggregate` / `insert_one` 与 KV 的 `get_value` 语义完全不同。

1. 封装官方驱动 + 独立 tokio runtime + 按 `ConnectionId` 缓存 client
2. 在 `ramag-domain` 的 `DriverKind` 枚举加变体
3. 在 `ramag-bin/main.rs` 写 `build_xxx_service` 装配进对应 service（复用同一 Storage）

**也不要给它再开新 tokio runtime**——复用 `ramag-infra-mongodb` 那一份即可。

## 测试策略

| 层 | 测试类型 | 备注 |
|----|---------|------|
| Domain | 单元测试 | 纯 Rust 逻辑 |
| App | 单元测试 | 编排逻辑 |
| Infra（SQL / Redis / MongoDB） | 集成测试连真实 DB | 缺环境变量自动 skip |
| Infra（Object Storage） | XML/签名/缓存/传输单元测试 + 默认忽略的 COS/OSS 往返测试 | 真实测试要求专用 Bucket、Prefix 和显式环境变量 |
| Infra（Storage / Git） | 单元测试 + tempdir | 不依赖外部服务 |
| Infra（SSH） | 单元测试 + 可选 OpenSSH 集成测试 | 外部服务测试按环境跳过 |
| UI | GPUI 无头渲染与状态回归测试 + 手动验收 | 覆盖关键布局、焦点和交互状态 |

仓库已配置 Windows / macOS 桌面打包发布工作流；常规代码质量门禁仍需在提交前本地运行 `make fmt-check`、`make check`、`make clippy`、`make test`。数据库集成测试需配置 `RAMAG_TEST_*` 环境变量；SSH 集成测试需要可用的 OpenSSH 测试端点。

## 参考资料

- [Clean Architecture by Robert Martin](https://blog.cleancoder.com/uncle-bob/2012/08/13/the-clean-architecture.html)
- [zed-industries/zed](https://github.com/zed-industries/zed) — GPUI 框架来源
- [gpui-component](https://github.com/longbridge/gpui-component) — UI 组件库
- [gitoxide](https://github.com/Byron/gitoxide) — 纯 Rust Git 实现
- [mongo-rust-driver](https://github.com/mongodb/mongo-rust-driver) — MongoDB 官方 Rust 驱动
