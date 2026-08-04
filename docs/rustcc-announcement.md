# Ramag v0.0.2 发布：新增 SSH、SFTP 与 JumpServer 导入

大家好，Ramag v0.0.2 已经发布。

Ramag 是一个使用 Rust + GPUI 构建的本地优先开发者桌面工作台，将数据库、Git、SSH 和剪贴板放进同一个原生应用。

```text
数据库 ↔ Git ↔ SSH / SFTP ↔ 剪贴板
```

- GitHub：https://github.com/tools-rs/ramag
- 下载：https://github.com/tools-rs/ramag/releases/tag/v0.0.2
- 更新记录：https://github.com/tools-rs/ramag/blob/main/CHANGELOG.md

![Ramag v0.0.2 首页](https://cdn.jsdelivr.net/gh/tools-rs/ramag@main/docs/screenshots/v0.0.2/home-light.png)

## v0.0.2 主要更新

### SSH 连接管理

新增完整的 SSH 管理入口，支持：

- 密码、系统 SSH 配置和密钥认证
- 解析 `ssh user@host -p 2222 -i /path/to/key` 命令
- 连接测试、默认目录和生产连接标记
- 多连接标签和连接搜索
- 本机加密保存密码与敏感连接参数

终端连接复用系统 OpenSSH，因此可以继续使用现有的 `~/.ssh/config`、SSH Agent、密钥和 `known_hosts`。

![SSH 连接管理](https://cdn.jsdelivr.net/gh/tools-rs/ramag@main/docs/screenshots/v0.0.2/ssh-connections-empty-light.png)

### 内嵌终端与 SFTP 文件工作区

连接成功后，可以在同一个工作区中使用内嵌终端和远程文件浏览：

- ANSI 终端显示和常用键盘输入
- 一个连接下打开多个终端标签
- 始终保留至少一个终端，避免误关后留下空页面
- 远程目录浏览、路径导航和名称搜索
- 文本预览与编辑、日志跟随
- 文件和目录上传下载
- 覆盖确认、取消和传输进度

还可以从当前路径、目录或文件所在位置创建新终端。新终端会进入对应远程目录，不影响已有终端的运行状态。

生产连接会禁止 SFTP 上传、编辑、重命名和删除。终端命令仍由远端账号权限与服务器策略约束。

![SSH 内嵌终端与远程文件工作区](https://cdn.jsdelivr.net/gh/tools-rs/ramag@main/docs/screenshots/v0.0.2/ssh-workspace-light.png)

### JumpServer 导入

支持保存多个 JumpServer 登录，并直接读取：

- 组织与资产树
- 已授权资产
- 资产平台和地址
- 可用的授权账号

选择资产与授权账号后，可以导入为普通 SSH 连接，继续编辑、测试和打开。未开放 SSH 协议的资产会明确提示，不会错误导入。

### 数据库改进

- 结果搜索支持字符串 ID 与整数 ID 双向转换。
- 内置 Base10、Base16、Base36、Base58 Bitcoin、Base58 Flickr 和自定义字符表。
- 支持带路径、超时和输出限制的外部转换器。
- 修复 MySQL `SHOW WARNINGS` 被识别为普通分页查询的问题。
- 数据库连接导入导出迁移到全局设置，可统一处理 MySQL、PostgreSQL、Redis 和 MongoDB。
- 编辑连接时完整回填现有参数，生产连接继续受只读保护。

### Git 与剪贴板改进

Git 工作台新增明确的克隆入口，Markdown 文件默认渲染预览，并重新整理了分支、远端、Tag 和 Stash 操作。同时修复新建分支导致的界面崩溃、分栏宽度串联和部分标签无法通过 `⌘W` / `Ctrl+W` 关闭的问题。

剪贴板仍默认关闭。启用状态、采集行为、全局热键和“清空全部历史”统一迁移到全局设置；关闭后会隐藏入口并释放全局快捷键。

```text
macOS：⌘⇧V
Windows：Ctrl+Shift+V
```

## 本地优先与安全边界

Ramag 不要求登录账号，也不会把数据库连接、SSH 凭据、Git 仓库或剪贴板内容上传到 Ramag 服务。

- 敏感配置使用 AES-256-GCM 加密。
- 主密钥保存在 macOS Keychain 或 Windows Credential Manager。
- SSH 认证、主机校验和 Git 网络操作复用系统已有配置。
- 数据库和 SSH 生产连接会限制界面中的写操作。

Git 功能仍处于试验阶段。生产数据库操作、Git 写操作和远程终端命令仍需要使用者确认目标环境和影响范围。

## 下载

v0.0.2 Release：

https://github.com/tools-rs/ramag/releases/tag/v0.0.2

当前提供：

```text
Ramag-0.0.2-macos-arm64.dmg
Ramag-0.0.2-macos-x86_64.dmg
Ramag-0.0.2-windows-x64-setup.exe
SHA256SUMS.txt
```

支持 macOS 12+ Apple Silicon、macOS 12+ Intel 和 Windows 10/11 x64，暂不支持 Linux。

当前 Windows 安装包尚未做 Authenticode 签名，macOS 安装包尚未完成 Developer ID 签名与 Apple 公证。请只从项目 Releases 下载，并使用同一页面的 `SHA256SUMS.txt` 校验文件。

从源码运行：

```bash
git clone https://github.com/tools-rs/ramag.git
cd ramag
make develop
```

如果你正在使用 Rust、GPUI、数据库工具或 SSH/SFTP 工作流，欢迎下载体验。遇到问题时，可以在 GitHub Issues 中附上操作系统、Ramag 版本和复现步骤；提交前请删除服务器地址、用户名、密码和业务数据。

- Issues：https://github.com/tools-rs/ramag/issues
- 源码：https://github.com/tools-rs/ramag
