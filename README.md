# Ramag

Rust + [GPUI](https://github.com/zed-industries/zed) 编写的 macOS / Windows 桌面工具平台：一个 App 聚合日常开发要用的多个小工具，全部本地运行、数据本地加密存储。

当前内置三个工具，经左侧 ActivityBar 切换：

| 工具 | 说明 |
|---|---|
| **数据库客户端** | MySQL / PostgreSQL / Redis / MongoDB 统一入口，driver 在连接表单内选择 |
| **版本管理** | Git 可视化客户端：仓库管理 / diff / 提交 / 分支 / 推拉合并 |
| **剪贴板** | 剪贴历史：采集 / 搜索筛选 / 全局热键悬浮抽屉快速粘贴，全本地加密 |

## 功能一览

### 数据库客户端

- **连接管理**：连接配置加密落盘（密码 AES-GCM 加密，主密钥存系统凭据库）、连接测试、颜色标签
- **SQL（MySQL / PostgreSQL）**：库表树（右键重命名 / 清空 / 删除，二次确认）、SQL 编辑器（语法高亮、补全、格式化、EXPLAIN）、多语句执行、运行中取消、结果集分页 / 单元格编辑 / 导出、DDL 查看、查询历史
- **Redis**：key 树按 `:` 折叠命名空间（5 万+ key 行级虚拟化）、String / Hash / List / Set / ZSet / Stream 全类型查看与编辑、TTL 管理、key 与前缀级删除
- **MongoDB**：database → collection 树、文档表格（嵌套字段扁平化、钻取、编辑、导出）、find / aggregate 等原始命令执行、常用命令示例

### 版本管理（Git）

- 工作区状态自动刷新（文件监听 + 窗口激活触发）、untracked 预览
- diff 分屏对照，按文件后缀全量语法高亮（tree-sitter，35 种语法）
- 提交（amend 保留原 message）、分支 / 标签 / stash、push / pull、merge / rebase / cherry-pick、reflog、blame、冲突三栏编辑、commit graph

### 剪贴板

- 采集循环独立于剪贴板视图和抽屉；应用运行期间持续记录，历史全本地 AES-GCM 加密存储（Windows 关窗后经系统托盘常驻，采集不中断）
- 搜索、按类型筛选、来源应用黑名单、条数 / 天数自动清理
- 全局热键：macOS `⌘⇧V`、Windows `Ctrl+Shift+V`（可在设置切换备用组合 `⌘⌥V` / `Ctrl+Alt+V`），唤起抽屉后可粘贴回原窗口

## 快速开始

支持 macOS（Apple Silicon / Intel）和 Windows 10 1703+ / Windows 11 x64。VCS 写操作需要系统已安装 Git；Windows 推荐安装 Git for Windows。Rust 工具链由 `rust-toolchain.toml` 固定。

```bash
git clone https://github.com/axemc/ramag.git
cd ramag

make develop        # debug 运行（编译快）
make release        # release 运行（首次 ~2-3 分钟）

make dmg            # 打包当前架构：svg → icns → build → Ramag.app → Ramag.dmg
make dmg-universal  # Intel + Apple Silicon 通用二进制（约 2 倍编译时间）

# 在 macOS 交叉编译 Windows x64 debug 版（编译验证）
make win-debug
```

Windows 原生开发需先安装 Rust、Visual Studio C++ Build Tools 与 Windows 10/11 SDK：

```powershell
cargo run -p ramag-bin
powershell -ExecutionPolicy Bypass -File scripts/build-windows.ps1 -Release
```

GPUI 的 Release 渲染着色器需要 Windows SDK `fxc.exe`，因此正式版须在 Windows 原生构建；macOS 交叉构建用于 debug 编译验证。Windows 目标静态链接 MSVC CRT，产出的便携 `.exe` 无需另装 VC++ Redistributable；安装包与代码签名属于发布流程。

所有常用任务封装在 `Makefile`，直接 `make` 查看完整列表。

## 常用快捷键

| 场景 | 快捷键 |
|---|---|
| SQL / Mongo 查询 | `Cmd/Ctrl+Enter` 运行 · `Cmd/Ctrl+Shift+Enter` 运行光标处语句 · `Cmd/Ctrl+T` 新建 · `Cmd/Ctrl+W` 关闭 |
| SQL 编辑 | `Cmd/Ctrl+Shift+F` 格式化 · `Cmd/Ctrl+Shift+E` EXPLAIN · `Cmd/Ctrl+E` 收起编辑器 |
| VCS | `Cmd/Ctrl+K` 聚焦提交信息 · `Cmd/Ctrl+Enter` 提交 · `Cmd/Ctrl+Shift+K` push · `Cmd/Ctrl+T` pull · `Cmd/Ctrl+R` 刷新 |
| 剪贴板 | `⌘⇧V`（macOS）/ `Ctrl+Shift+V`（Windows）唤起抽屉 · `Cmd/Ctrl+F` 搜索 · `Enter` 复制 · `↑↓` 选择 |

## 架构

Clean Architecture 务实版，Cargo Workspace 共 17 个 crate，依赖方向严格向内：

```
ramag-bin                 入口：依赖注入 + 启动 GPUI
  ├─ ramag-ui             Shell / ActivityBar / 主题 / 通用对话框
  ├─ ramag-tool-*         工具视图（dbclient / redis / mongodb / vcs / clipboard）
  ├─ ramag-app            Use Cases（ConnectionService / RedisService / MongoService / ClipboardService / ToolRegistry）
  ├─ ramag-infra-*        驱动实现（mysql / postgres / sql-shared / redis / mongodb / git / clipboard / storage）
  └─ ramag-domain         实体 + traits，零 UI / 框架 / 具体技术依赖
```

关键设计：

- **SQL 共享层**（`ramag-infra-sql-shared`）：关系型数据库只需 impl `SqlBackend`（方言 + 解码 + 元数据 SQL），`impl_driver_for!` 宏一行生成 `Driver` 实现；多语句切分、LIMIT 注入、连接池缓存、取消句柄均在共享层，新增 SQLite 等不必重写模板
- **双 runtime 桥接**：GPUI 用 smol，sqlx / redis-rs / mongodb 强依赖 tokio；driver 经 `run_in_tokio` 把 future 派发到独立 tokio runtime，结果用 oneshot 送回
- **凭证安全**：连接配置存 redb，密码字段单独 AES-GCM 加密，主密钥存 macOS Keychain / Windows Credential Manager，全程不落明文

完整分层说明与「新增数据库 / 新增工具」标准流程见 [docs/architecture.md](docs/architecture.md)。

## 开发

```bash
make check          # cargo check --all-targets（最快的类型检查）
make fmt            # cargo fmt --all
make clippy         # cargo clippy --all-targets -- -D warnings
make test           # cargo test --all
```

工程约束：Clippy 将 `unwrap / expect / panic` 标为警告，提交前使用 `-D warnings` 门禁；单文件保持在 600 行以内。

### 数据库集成测试

`crates/ramag-infra-{mysql,postgres,redis,mongodb}/tests/integration.rs` 连接真实数据库跑端到端流程；对应的一组环境变量缺任一字段即自动 skip，不影响 CI：

```bash
# 以 MySQL 为例（PG / Redis / Mongo 同款前缀：RAMAG_TEST_PG_* / RAMAG_TEST_REDIS_* / RAMAG_TEST_MONGO_*）
export RAMAG_TEST_MYSQL_HOST=127.0.0.1
export RAMAG_TEST_MYSQL_PORT=3306
export RAMAG_TEST_MYSQL_USER=root
export RAMAG_TEST_MYSQL_PASSWORD=...
export RAMAG_TEST_MYSQL_DB=test
cargo test -p ramag-infra-mysql --test integration -- --nocapture
```

### 升级 GPUI

`gpui` 与 `gpui-component` 故意不钉 rev（两者共享 zed 源码，钉版会让 cargo 编译两份 zed、类型不互通），版本锁定靠入库的 `Cargo.lock`。升级用 `cargo update -p gpui`，并同步核对 `lsp-types` / `ropey` 版本与 `gpui-component` 内部一致。

## 数据与日志位置

| 内容 | 路径 |
|---|---|
| macOS 数据 / 日志 | `~/Library/Application Support/com.ramag.ramag/` |
| Windows 数据 / 日志 | `%APPDATA%\ramag\ramag\data\` |
| 加密主密钥 | macOS Keychain / Windows Credential Manager |

## License

[Apache-2.0](https://www.apache.org/licenses/LICENSE-2.0)
