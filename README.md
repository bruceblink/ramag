<p align="center">
  <img src="scripts/icons/ramag.svg" width="112" alt="Ramag" />
</p>

<h1 align="center">Ramag</h1>

<p align="center">
  <strong>数据库、Git 与剪贴板，一个本地桌面工作台。</strong>
</p>

<p align="center">
  MySQL · PostgreSQL · Redis · MongoDB · Git · Clipboard
</p>

<p align="center">
  macOS & Windows · Local-first · Rust + GPUI
</p>

---

## 一个窗口，覆盖三类高频任务

| 数据库工作台 | Git 工作台 | 剪贴板工作台 |
|---|---|---|
| 连接并管理 MySQL、PostgreSQL、Redis、MongoDB | 从工作区检查一路完成到提交、同步与冲突处理 | 随时唤起历史记录，搜索并粘贴回原窗口 |
| SQL、命令、数据编辑、分页、导出与历史记录 | Diff、Stage、Commit、Branch、Stash、Rebase、Blame | 文本、图片、文件记录，支持筛选、黑名单与自动清理 |
| TLS、自定义 CA、SSH 隧道、连接配置加密导入导出 | 文件监听自动刷新，35 种语法高亮 | 后台持续采集，历史数据本地加密 |

## 数据库工作台

| 引擎 | 浏览与管理 | 查询与编辑 |
|---|---|---|
| **MySQL / PostgreSQL** | Schema、表、列、DDL、重命名、清空与删除 | SQL 补全与高亮、多语句执行、EXPLAIN、取消、结果分页、单元格编辑与导出 |
| **Redis** | 按 `:` 折叠 Key 命名空间，支持大型 Keyspace 扫描 | String、Hash、List、Set、ZSet、Stream 全类型查看与编辑，TTL 管理 |
| **MongoDB** | Database、Collection 与文档浏览 | `find`、`aggregate` 等命令，嵌套字段钻取、文档编辑与导出 |

连接配置支持颜色标签、连接测试、TLS、自定义 CA 和系统 SSH 隧道。密码在本地加密保存，配置文件导出使用自定义口令进行 AES-256-GCM 加密。

```text
创建连接 → 浏览结构 → 编写查询 → 检查或编辑结果 → 导出
```

## Git 工作台

```text
打开仓库 → 查看改动 → 对照 Diff → Stage → Commit → Push / Pull
```

- 工作区状态自动刷新，支持未跟踪文件预览与文件树浏览。
- Unified / Split Diff、整文件上下文和按文件类型语法高亮。
- Branch、Tag、Stash、Merge、Rebase、Cherry-pick、Reflog、Blame。
- 冲突三栏处理、提交图、Amend，以及可继续或中止的冲突流程。

写操作与网络认证复用系统 Git 和既有 SSH 配置，不在应用内复制一套凭据体系。

## 剪贴板工作台

```text
⌘⇧V / Ctrl+Shift+V → 搜索或筛选 → Enter → 粘贴回原窗口
```

- 应用运行期间持续采集，不需要保持剪贴板页面打开。
- 支持文本、图片和文件记录，以及来源应用黑名单。
- 支持数量与保留天数限制，历史内容本地加密存储。
- Windows 关闭主窗口后可驻留系统托盘，继续提供采集与快捷抽屉。

## 面向真实数据量验证

项目内置可重复执行的四数据库 Docker 测试环境，不只验证简单 CRUD：

| MySQL | PostgreSQL | Redis | MongoDB |
|---:|---:|---:|---:|
| 100,000+ 行 | 100,000+ 行与 8,000 条分析数据 | 46,000+ Keys | 125,102 个文档 |

测试覆盖大型字段、二进制数据、特殊字符、原生类型、分页扫描和完整 Keyspace / Collection 遍历。

## 快速运行

macOS 支持 Apple Silicon 与 Intel；Windows 支持 Windows 10/11 x64。VCS 功能需要系统已安装 Git，SSH 隧道需要系统 OpenSSH。

```bash
git clone https://github.com/tools-rs/ramag.git
cd ramag
make develop
```

Windows 原生开发：

```powershell
cargo run -p ramag-bin
```

Rust 版本已由 `rust-toolchain.toml` 固定。首次编译 GPUI 与数据库驱动需要一定时间，后续构建会复用缓存。

## 常用快捷键

| 场景 | 快捷键 |
|---|---|
| 执行 SQL / Mongo 查询 | `Cmd/Ctrl+Enter` |
| 执行光标所在语句 | `Cmd/Ctrl+Shift+Enter` |
| 格式化 SQL / EXPLAIN | `Cmd/Ctrl+Shift+F` / `Cmd/Ctrl+Shift+E` |
| Git 提交 / Push / Pull | `Cmd/Ctrl+Enter` / `Cmd/Ctrl+Shift+K` / `Cmd/Ctrl+T` |
| 唤起剪贴板抽屉 | macOS `⌘⇧V` / Windows `Ctrl+Shift+V` |

<details>
<summary><strong>开发、测试与打包</strong></summary>

```bash
make check             # 检查所有 target
make fmt-check         # 验证 Rust 格式
make clippy            # Clippy，警告视为错误
make test              # 完整工作区测试
make db-test           # 重建专用测试数据并运行四数据库门禁
make dmg               # 构建当前 macOS 架构的 DMG
make win-debug         # 在 macOS 验证 Windows x64 debug 构建
```

`make db-test` 只会重建 `ramag-db-test-*` 专用容器和数据卷。完整分层与扩展方式见 [架构文档](docs/architecture.md)，其余命令运行 `make` 查看。

</details>

## License

[Apache-2.0](https://www.apache.org/licenses/LICENSE-2.0)
