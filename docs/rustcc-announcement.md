# Ramag v0.0.5：把数据库、代码和远程环境放在同一个本地工作台

大家好，Ramag v0.0.5 是一次面向日常开发的稳定性与体验更新。

Ramag 基于 Rust + GPUI 构建，定位是本地优先的开发者桌面工作台：连接数据库、检查 Git 改动、管理 SSH / SFTP 远程文件、浏览云端对象，都在同一个原生窗口中完成。

```text
数据库 ↔ Git ↔ SSH / SFTP ↔ 云存储 ↔ 剪贴板
```

- GitHub：https://github.com/tools-rs/ramag
- 变更记录：https://github.com/tools-rs/ramag/blob/main/CHANGELOG.md
- v0.0.4…v0.0.5 对比：https://github.com/tools-rs/ramag/compare/v0.0.4...v0.0.5

## v0.0.5 界面截图

以下截图来自 v0.0.5 当前工作区，展示统一首页、数据库、Git 和设置页面。

| 首页 | 数据库连接管理 |
|---|---|
| ![Ramag v0.0.5 首页](https://raw.githubusercontent.com/tools-rs/ramag/main/docs/screenshots/v0.0.5/home-light.png) | ![Ramag v0.0.5 数据库连接管理](https://raw.githubusercontent.com/tools-rs/ramag/main/docs/screenshots/v0.0.5/database-connections-light.png) |

| MySQL 查询与结果 | Git 仓库管理 |
|---|---|
| ![Ramag v0.0.5 MySQL 查询与结果](https://raw.githubusercontent.com/tools-rs/ramag/main/docs/screenshots/v0.0.5/database-mysql-query-light.png) | ![Ramag v0.0.5 Git 仓库管理](https://raw.githubusercontent.com/tools-rs/ramag/main/docs/screenshots/v0.0.5/git-repositories-light.png) |

| Git 工作区 | 数据库客户端设置 |
|---|---|
| ![Ramag v0.0.5 Git 工作区](https://raw.githubusercontent.com/tools-rs/ramag/main/docs/screenshots/v0.0.5/git-workspace-light.png) | ![Ramag v0.0.5 数据库客户端设置](https://raw.githubusercontent.com/tools-rs/ramag/main/docs/screenshots/v0.0.5/settings-database-light.png) |

## 四大工作台：已有能力

这些能力在此前版本已经提供，v0.0.5 继续围绕稳定性和交互一致性打磨，并不把它们包装成本版首次新增功能。

### 数据库

- 支持 MySQL、PostgreSQL、Redis 和 MongoDB 的连接、查询、结构浏览、结果编辑与数据导入导出。
- 提供 SQL / 文档 / Key 数据查看，分页、筛选、搜索和格式化等高频操作。
- 支持四引擎之间的数据同步；生产连接对写操作提供保护。

### Git

- 在仓库内查看 Changes、Project Files、历史、Diff、Blame、Reflog 和提交详情。
- 支持暂存、提交、分支、Tag、Stash、Merge、Rebase 与 Cherry-pick 等工作流。
- Markdown 文件可在预览和原文之间切换，写操作继续复用系统 Git 与 SSH 配置。

### SSH / SFTP

- 复用系统 OpenSSH 的连接、认证和主机校验能力，提供内嵌终端与多标签会话。
- 支持 SFTP 目录浏览、路径导航、文件预览编辑、日志跟随和上传下载传输队列。
- 支持 JumpServer 导入；生产连接默认禁止 SFTP 上传、编辑、重命名和删除。

### 云存储

- 支持腾讯云 COS 与阿里云 OSS，按用户明确配置的 Bucket 浏览对象。
- 提供目录导航、搜索、收藏、对象详情、常见文本预览和上传下载进度。
- 生产模式保留只读浏览、查看与下载，凭据和工作区状态在本机加密保存。

## v0.0.5 本版更新

### Git 工作区更容易读懂

- 优化 Git 左栏布局和仓库级会话布局，切换仓库后仍保持清晰、独立的工作区状态。
- 统一 Changes 与 Project Files 的文件树图标体验，查看目录和文件改动时更容易建立层级感。

### Markdown 预览更稳定

- 修复预览资源与样式引用问题，覆盖跨平台路径与引用场景的回归测试。
- Markdown 预览在不同系统和工作区中使用一致的资源解析边界。

### 跨工作台交互更一致

- 统一复制成功反馈与相关界面文案，减少数据库、Git、SSH / SFTP、云存储和剪贴板之间的操作差异。
- 补充窗口打开闸门、VCS 布局更新和终端测试的可见性校验，并修复对应的 Clippy 问题。

### 更容易在本地验证

- 检查脚本兼容没有 `rg` 的环境，减少运行质量门禁时的额外工具要求。
- README、贡献指南和安全资料补充使用边界、开发流程与问题反馈方式。

## 本地优先与安全边界

Ramag 不要求登录账号，也不把数据库连接、SSH 凭据、Git 仓库、对象存储配置或剪贴板内容上传到 Ramag 服务。

- 敏感配置使用 AES-256-GCM 加密，主密钥由系统凭据库保存。
- 数据库写操作、Git 写操作和远程终端命令仍需用户在执行前确认目标与影响范围。
- 复制操作只处理当前已经加载到客户端的数据，不会绕过读取上限或暗中发起额外网络读取。
- Git 仍处于试验阶段；请在生产环境中先确认账号权限、仓库状态和远程命令影响。

## 下载与发布状态

v0.0.5 的版本信息、变更记录和发布流程见仓库中的 [CHANGELOG.md](../CHANGELOG.md) 与[桌面端构建与发布](desktop-release.md)。对外安装包是否生成、签名、公证或发布，以 GitHub Release 页面中可验证的实际产物为准；本公告不提前宣称尚未完成的构建结果。

从源码运行：

```bash
git clone https://github.com/tools-rs/ramag.git
cd ramag
make develop
```

欢迎通过 Issues 或官方交流群反馈使用体验。提交问题前请移除服务器地址、用户名、密码和业务数据。

- Issues：https://github.com/tools-rs/ramag/issues
- 源码：https://github.com/tools-rs/ramag
