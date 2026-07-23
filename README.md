<p align="center">
  <img src="scripts/icons/ramag.svg" width="112" alt="Ramag" />
</p>

<h1 align="center">Ramag</h1>

<p align="center">
  <strong>数据库、Git 与剪贴板，一个真正本地优先的桌面工作台。</strong>
</p>

<p align="center">
  MySQL · PostgreSQL · Redis · MongoDB · Git（试验性） · Clipboard
</p>

<p align="center">
  macOS & Windows · Rust + GPUI · Local-first
</p>

---

## 不是三个工具的简单拼接

Ramag 把开发中最频繁切换的三类上下文收进一个原生窗口：查数据库、处理 Git 工作区、找回刚才复制过的内容。它不依赖浏览器壳，不要求把连接配置或历史记录交给云端服务，也不会为了展示一张大表、一个大仓库或一段很长的剪贴历史就无边界地吃掉内存。

| 一个工作台 | 本地优先 | 面向真实数据量 | 原生交互 |
|---|---|---|---|
| 数据库、Git、剪贴板共享统一窗口与快捷键体系 | 密码与剪贴历史加密后落本地，主密钥进入系统凭据库 | 百万级历史、十万文件与十万级数据库种子均有压力验证 | Rust + GPUI，耗时任务与 UI 线程隔离 |

```text
连接数据库处理数据  ↔  在 Git 中检查并提交改动  ↔  随时找回和粘贴上下文
```

## 数据库工作台

从连接、结构浏览、查询，到结果编辑和完整迁移，四类数据库共用一套清晰的工作流。

### MySQL 与 PostgreSQL

- Schema、表、视图、列、索引与 DDL 浏览。
- SQL 补全、高亮、多语句执行、光标语句执行、格式化与 EXPLAIN。
- 查询取消、结果分页、排序、筛选和单元格编辑。
- 大整数、高精度数值、JSON/JSONB、二进制、时间以及 PostgreSQL 原生类型保真展示。
- 表级 JSONL 导入导出与 Schema / 数据库级 SQL 导入导出；主键表使用 keyset 分页，深页不会反复跳过前置数据。

### Redis

- 以 `:` 自动折叠 Key 命名空间，大型 Keyspace 使用游标 SCAN 和虚拟列表。
- String、Hash、List、Set、ZSet、Stream 六种类型统一查看与编辑。
- TTL 管理、大 String 有界加载、大集合自动分批继续加载。
- 内置命令控制台；危险、阻塞和生产写命令在执行前识别。
- 整库 JSONL 迁移保留类型、TTL、顺序、分数、Stream ID 与二进制内容。

### MongoDB

- Database、Collection、索引、统计信息和文档浏览。
- `find`、`aggregate` 与通用命令，支持格式化、历史记录和多查询标签。
- 嵌套文档按 dotted path 展开，ObjectId、Decimal128、DateTime、Int64 等使用 Extended JSON 保真往返。
- 文档编辑、集合级 JSONL 和数据库级导入导出；混合类型 `_id` 使用 keyset 连续读取。

### 连接与数据安全

- TLS、三档证书验证、自定义 CA 与系统 OpenSSH 隧道。
- 连接测试、颜色标签、连接配置加密导入导出。
- 连接可标记为生产环境：写查询、结果编辑和导入入口统一进入只读保护。
- SQL、Redis、MongoDB 使用独立执行 runtime，某个慢查询不会直接挤占其他数据库的任务线程。

## Git 工作台（试验性）

Ramag 的 Git 体验围绕“看清改动，然后安全完成操作”展开，而不是把命令行按钮化。

> 当前 VCS 能力处于试验阶段，适合体验和反馈；执行关键写操作前，建议确认工作区状态并保留可恢复点。

```text
打开仓库 → 检查工作区 → 对照 Diff → Stage → Commit → Push / Pull
```

- Changes、Project Files、Stash、历史日志、Commit 详情、Blame 与 Reflog。
- Unified / Split Diff、整文件上下文、35 种语法高亮和超大 Diff 虚拟化。
- Stage / Unstage、Amend、Branch、Tag、Stash、Merge、Rebase、Cherry-pick。
- 冲突三栏处理，可继续或中止 Merge / Rebase / Cherry-pick 流程。
- 提交图、分支与远端状态、文件编辑和自动保存。
- 文件监听按路径增量刷新；普通保存不重扫整个仓库。

写操作与网络认证直接复用系统 Git、SSH Agent 和用户已有配置，不在应用中再造一套不兼容的凭据体系。

## 剪贴板工作台

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

## 平台与文档

Ramag 支持 macOS Apple Silicon / Intel，以及 Windows 10/11 x64。Git 功能依赖系统 Git；SSH 隧道依赖系统 OpenSSH。

- [性能报告：VCS、数据库与剪贴板](docs/performance.md)
- [架构说明](docs/architecture.md)
- [桌面端构建与发布](docs/desktop-release.md)

## License

[Apache-2.0](https://www.apache.org/licenses/LICENSE-2.0)
