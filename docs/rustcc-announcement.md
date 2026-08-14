# Ramag v0.0.4 发布：表设计器、快捷键中心与统一复制交互

大家好，Ramag v0.0.4 已经发布。

Ramag 是一个基于 Rust + GPUI 构建的本地优先开发者桌面工作台，将数据库、Git、SSH、云存储和剪贴板放进同一个原生应用。

```text
数据库 ↔ Git ↔ SSH / SFTP ↔ 云存储 ↔ 剪贴板
```

- GitHub：https://github.com/tools-rs/ramag
- 下载：https://github.com/tools-rs/ramag/releases/tag/v0.0.4
- 更新记录：https://github.com/tools-rs/ramag/blob/main/CHANGELOG.md

## v0.0.4 主要更新

### MySQL / PostgreSQL 图形化表设计器

MySQL 与 PostgreSQL 新增图形化表设计器，可在界面中：

- 创建新表或修改已有表；
- 修改表名和字段结构；
- 在实际执行前预览 DDL；
- 获得明确的执行耗时与错误反馈。

这让常见的表结构调整不必完全依赖手写 DDL，同时仍保留执行前可审阅的 SQL 边界。

### 快捷键中心与统一复制交互

新增快捷键中心，可查看、修改和重置应用快捷键；数据库、Git、SSH、云存储等工作台也提供统一的快速切换入口。

数据展示区域统一支持复制当前已加载的完整值：

```text
macOS：⌘ + 双击
Windows / Linux：Ctrl + 双击
```

该行为覆盖 SQL、MongoDB、Redis、Git Diff/项目文件、SSH/SFTP、对象存储和剪贴板等高频场景。普通双击仍保留原有的打开、编辑或下钻语义，复制成功会给出明确提示。

### Redis Key 树更清晰、更适合大 Keyspace

Redis Key 树本次重点改善层级展示和类型识别：

- 批量获取 Key 类型，避免逐 Key 请求；
- 使用紧凑类型标签显示 String、Hash、List、Set、ZSet、Stream；
- 改进树引导线、层级和文件夹/文件图标；
- 新增“同名 Key 下沉展示”设置，解决路径节点同时又是实际 Key 时的展示歧义。

### 稳定性与体验改进

- 修复 MySQL 事务中的 `SHOW WARNINGS` 读取，以及 MySQL/PostgreSQL 表结构变更的执行反馈问题。
- 修复全局唤醒快捷键、剪贴板悬浮框重复唤醒和 Git 文件树文件行对齐。
- Git 的 Changes 与 Project Files 树统一使用文件夹和文件图标。
- 所有可滚动区域保留滚轮与触控板滚动，但不显示可见滚动条。
- Linux 桌面端补全 X11 与 Wayland 后端支持。
- 改进更新检查、剪贴板容量显示、图标按钮提示，并在关于页加入官方交流群入口。

## 本地优先与安全边界

Ramag 不要求登录账号，也不会把数据库连接、SSH 凭据、Git 仓库、对象存储配置或剪贴板内容上传到 Ramag 服务。

- 敏感配置使用 AES-256-GCM 加密，主密钥由系统凭据库保存。
- SSH 认证、主机校验和 Git 网络操作复用系统已有配置。
- 数据库、SSH 和对象存储的生产模式继续限制界面中的写操作。
- “复制完整值”仅复制当前已成功加载到客户端的数据，不会绕过后端读取上限或暗中发起额外网络读取。

Git 功能仍处于试验阶段。生产数据库操作、Git 写操作和远程终端命令仍需在执行前确认目标环境与影响范围。

## 下载

v0.0.4 Release：

https://github.com/tools-rs/ramag/releases/tag/v0.0.4

提供：

```text
Ramag-0.0.4-macos-arm64.dmg
Ramag-0.0.4-macos-x86_64.dmg
Ramag-0.0.4-windows-x64-setup.exe
Ramag-0.0.4-linux-amd64.deb
Ramag-0.0.4-linux-x86_64.AppImage
SHA256SUMS.txt
```

支持 macOS 12+（Apple Silicon / Intel）、Windows 10/11 x64 和 Linux x86_64（X11 / Wayland）。

当前 Windows 安装包尚未做 Authenticode 签名；macOS 安装包尚未完成 Developer ID 签名与 Apple 公证；Linux 包未做独立代码签名。请只从本仓库 Releases 下载，并使用同一页面的 `SHA256SUMS.txt` 校验文件。

从源码运行：

```bash
git clone https://github.com/tools-rs/ramag.git
cd ramag
make develop
```

欢迎通过 Issues 或官方交流群反馈使用体验。提交问题前请移除服务器地址、用户名、密码和业务数据。

- Issues：https://github.com/tools-rs/ramag/issues
- 源码：https://github.com/tools-rs/ramag
