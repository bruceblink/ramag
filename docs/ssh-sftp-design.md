# SSH + SFTP 工具设计方案

> 状态：首个完整版本已实现并通过本地质量门禁；真实外部 SSH 环境与 Windows 实机验收留给发布流程。
> 目标版本：当前开发版本。
> 最后更新：2026-07-27。

## 结论

Ramag 新增一个独立的 **SSH + SFTP** 工具：

- 表面入口是 SSH 连接管理器。
- 每个 SSH 配置打开一个工作区。
- 工作区左侧浏览远程文件，右侧运行一个或多个终端，底部显示传输任务。
- 文件协议是 **SFTP**，不是 FTP / FTPS；SFTP 通过 SSH 安全通道工作。
- 主界面继续完全使用 GPUI，不引入 WebView、Tauri、egui 等第二套 UI。
- 不复制或链接 Zed 的 GPL `terminal` / `terminal_view` 代码。
- 终端协议内核复用 Apache-2.0 的 `alacritty_terminal`，GPUI 显示与交互层独立实现。
- SSH 连接优先复用系统 OpenSSH，沿用用户已有的 `~/.ssh/config`、密钥、Agent 和
  `known_hosts`。
- macOS 与 Windows 共用同一产品能力；Windows 通过 OpenSSH Client + ConPTY 实现。
- SFTP 必须使用结构化协议接口，不解析 `sftp` 命令的文本输出。
- 现有数据库 SSH 隧道行为保持不变。
- 技术 PoC 只用于内部验证；连接管理、终端、SFTP 文件操作和传输队列作为首个完整版本
  一次性交付，不发布只能浏览、不能传输的半成品。

## 产品定位

这个工具解决的是一条完整工作流：

```text
管理服务器连接 → 打开远程终端 → 浏览远程文件 → 上传/下载/修改文件
```

第一版不追求替代专业终端或完整双栏文件管理器，只完成日常服务器维护所需的可靠闭环。

### 交付原则

开发过程可以拆成小步骤，但产品交付不能按残缺功能拆开：

- 阶段 0 是隔离的内部 PoC，不注册为正式工具，也不作为可用版本发布。
- 首个正式版本必须一次完成连接管理、多终端、SFTP 浏览与写操作、传输队列、状态恢复、
  错误处理和跨平台验证。
- 所有核心验收项通过后再启用 ActivityBar 入口；不能把“只读文件树”或“只有终端”当成
  SSH + SFTP 工具交付。
- 搜索、超链接、拖放、分屏、远程编辑、目录同步等不影响核心闭环的能力可以后续增加。

这里的“一次完成”指首个正式版本的交付边界，不代表把全部代码写在一个大改动中。实现
仍需按可测试的小步骤推进，每一步都保持可构建、可回退。

### 非目标

- 不实现 FTP、FTPS。
- 不自行实现 SSH 加密协议。
- 不复制、翻译或改写 Zed 的 GPL 终端源码。
- 不在第一版实现终端分屏、端口转发管理、远程编辑器、同步目录。
- 不改造现有数据库 SSH 隧道。

## 与数据库页面的对应关系

“数据库左边的数据对应 SSH 的什么”需要分两个层级理解：

| 数据库工具 | SSH + SFTP 工具 |
|---|---|
| 连接管理页中的数据库连接 | SSH 配置 |
| 顶部打开的数据库连接标签 | SSH 工作区标签 |
| 工作区左侧 Schema / Table 树 | 远程 SFTP 文件树 |
| 右侧 SQL 查询标签 | 右侧 Terminal 标签 |
| 查询执行状态 / 导入导出任务 | 上传、下载任务队列 |

因此，顶部标签代表“连接工作区”，内部 Terminal 标签代表这个服务器上的多个 Shell 会话。

## 页面结构

### 连接管理页

延续数据库工具的交互模型：

```text
┌──────────────────────────────────────────────────────┐
│ 搜索 SSH 连接                    OpenSSH 状态  刷新  + │
├──────────────────────────────────────────────────────┤
│ production       │ config / Agent │ host:22 │ 编辑 删除│
│ staging          │ OpenSSH 密钥   │ host:22 │ 编辑 删除│
└──────────────────────────────────────────────────────┘
```

主页面与数据库“数据源管理”使用同一套限宽工具栏、搜索框、紧凑连接行、响应式次要列和
顶部工作区标签。点击整行打开工作区；新建和编辑使用同款 720px 独立弹窗，表单不常驻占用
主工作区。弹窗主体可滚动、底部操作固定可见，小窗口下不能让输入框或按钮塌缩。

连接配置第一版包含：

- 名称与颜色标签。
- 主机或 `~/.ssh/config` 别名。
- 端口，默认 `22`。
- 用户名，可留空交给 SSH 配置解析。
- 认证方式：系统 SSH 配置 / Agent、密钥文件。
- 可选密钥路径。
- 可选初始远程目录。

密码认证可以在终端内由系统 SSH 交互输入；SFTP 的密码认证需要独立安全方案，第一版不把
明文密码塞进命令参数或环境变量。

### SSH 工作区

```text
┌──────────────────────────────────────────────────────────┐
│ server-a × │ server-b ×                         顶部标签 │
├──────────────────┬───────────────────────────────────────┤
│ 远程文件树       │ Terminal 1 │ Terminal 2 │ +          │
│                  ├───────────────────────────────────────┤
│ /                │                                       │
│ ├─ etc           │             终端区域                  │
│ ├─ home          │                                       │
│ └─ var           │                                       │
├──────────────────┴───────────────────────────────────────┤
│ 传输队列：等待 / 进行中 / 完成 / 失败 / 已取消          │
└──────────────────────────────────────────────────────────┘
```

一个工作区是逻辑上的服务器连接，不保证底层只有一条 TCP 连接：

- 每个 Terminal 标签可拥有一个独立的系统 `ssh` 进程。
- 文件树拥有一个独立的 SFTP 子系统进程。
- 后续可以研究 OpenSSH ControlMaster 复用，但不作为第一版前提。

关闭应用后只恢复工作区占位和最后路径，不自动重连、不恢复终端敏感输出；用户主动点击后
重新连接。

## 终端技术方案

### 分层

```text
GPUI TerminalView                 Ramag 独立实现
  ├─ 可见行绘制、光标、选区
  ├─ 键盘、鼠标、IME
  ├─ 复制、粘贴、滚动
  └─ 尺寸变化与主题
             │
             ▼
alacritty_terminal                Apache-2.0
  ├─ ANSI / VT 解析
  ├─ 屏幕缓冲区与备用屏幕
  ├─ 颜色、属性、光标状态
  └─ PTY / 终端事件能力
             │
             ▼
系统 ssh                          独立子进程
```

GPUI 是渲染框架，不自带可直接使用的终端控件。Ramag 只重新实现 GPUI 适配层，不重新实现
ANSI 状态机或 SSH 协议。

### TerminalView 第一版能力

- 启动、关闭和重连系统 `ssh`。
- 字符网格、ANSI 色彩、光标和备用屏幕。
- 滚动历史，默认有界，禁止无限增长。
- 键盘输入、中文输入法、Unicode 宽字符。
- 文本选择、复制、粘贴和 bracketed paste。
- 窗口尺寸变化后同步 PTY 行列数。
- 多终端标签。
- 子进程退出状态和 stderr 可定位展示。

搜索、超链接识别、鼠标协议增强、终端分屏属于后续能力。

### 进程安全

- 通过参数数组启动 `ssh`，禁止 `sh -c` 和字符串拼接执行。
- SSH 目标必须校验，且用 `--` 阻止目标被解释为额外选项。
- 不使用 `StrictHostKeyChecking=no`。
- 子进程 stdout / stderr 和事件队列全部有界或持续排空。
- 关闭标签和应用时终止并回收子进程，不能留下孤儿进程。

## 系统 OpenSSH 与 Windows

### 支持范围

- macOS 使用系统 OpenSSH。
- Windows 支持 Windows 10 1809 及以上、Windows 11 x64。
- Windows 交互终端通过 ConPTY 承载 `ssh.exe`，不额外弹出控制台窗口。
- Windows 后台 SFTP 进程使用管道和 `CREATE_NO_WINDOW`，不占用 ConPTY。

Windows 提供 OpenSSH Client，但它属于可选系统功能，Ramag 不能假设每台机器都已安装。
缺少 OpenSSH 时只禁用连接动作，配置管理页面仍可进入。

### 可执行文件发现

Windows 默认按以下顺序发现 OpenSSH：

1. 固定系统路径 `%WINDIR%\System32\OpenSSH\ssh.exe`。
2. `PATH` 中的 `ssh.exe`，解析后转成绝对路径。

如果用户明确配置了自定义绝对路径，它作为显式覆盖优先于自动发现。macOS 默认优先使用
`/usr/bin/ssh`，不存在时再查 `PATH`。所有平台都必须：

- 验证目标是普通文件。
- 通过带超时的 `ssh -V` 做能力探测。
- 缓存探测结果，但在执行失败后允许重新探测。
- 禁止从当前工作目录隐式加载同名 `ssh`，避免路径劫持。
- 找不到时显示具体安装路径和系统安装说明，不自动下载或提权安装。

Windows 安装提示应指向“可选功能”中的 OpenSSH Client，并可提供仅供用户主动复制执行的
管理员 PowerShell 命令：

```powershell
Add-WindowsCapability -Online -Name OpenSSH.Client~~~~0.0.1.0
```

`ssh.exe`、`ssh-keygen.exe`、`ssh-add.exe` 等配套程序应来自同一 OpenSSH 目录，避免混用
不同发行版。

### Windows 配置与认证

- OpenSSH 配置和主机记录使用 `%USERPROFILE%\.ssh\config`、`known_hosts`。
- Windows `ssh-agent` 服务可能默认禁用；Ramag 可以检测并提示，但不能自行提权启动服务。
- OpenSSH 格式密钥直接支持；PuTTY `.ppk` 和 Pageant 不属于首个版本的兼容范围。
- Terminal 在 ConPTY 中支持密码、密钥口令和首次主机指纹的交互提示。
- SFTP 是二进制管道，不能混入文本密码提示。首个版本的 SFTP 支持系统配置、已加载
  Agent、无需交互的密钥，以及已经由 Terminal 确认并写入 `known_hosts` 的主机。
- 未确认主机指纹时先完成 Terminal 信任流程，再启动 SFTP；禁止为绕过交互而关闭主机
  校验。

首个完整版本的认证矩阵：

| 认证方式 | Terminal | SFTP |
|---|---:|---:|
| SSH config + Agent | 支持 | 支持 |
| 无需交互的 OpenSSH 密钥 | 支持 | 支持 |
| 已由 Agent 解锁的口令密钥 | 支持 | 支持 |
| 直接输入密码或密钥口令 | 支持 | 暂不支持 |
| PuTTY `.ppk` / Pageant | 暂不支持 | 暂不支持 |

这个限制必须在连接表单和测试结果中明确展示，不能等到打开文件树后才给出模糊错误。

## SFTP 技术方案

### 推荐路径

推荐 PoC 验证以下组合：

```text
系统 ssh -s <target> sftp
       │ 二进制 stdin/stdout
       ▼
russh-sftp::SftpSession
       │ 结构化文件 API
       ▼
目录树与传输队列
```

理由：

- 系统 SSH 继续处理 SSH 配置、Agent、密钥和 `known_hosts`。
- `russh-sftp` 只处理 SFTP 二进制协议，可接入任意异步读写流。
- 不解析本地化、版本相关且不稳定的 `sftp` 文本输出。
- `russh-sftp` 当前声明为 Apache-2.0，但引入前仍需审计锁定版本及传递依赖。

### PoC 必须验证

- macOS 与 Windows 系统 OpenSSH 的 SFTP 子系统管道是否稳定。
- 子进程 stdin/stdout 与 Tokio 异步流的桥接和关闭顺序。
- Agent、密钥、SSH config alias、ProxyJump 的实际行为。
- Terminal 确认主机指纹后，SFTP 能否可靠复用同一份 `known_hosts` 结果。
- SFTP 遇到密码或密钥口令等交互认证时，能否在启动前识别并给出准确提示。
- 断网、远端主动关闭、超时和取消时能否完整回收资源。

如果阶段 0 证明既定认证矩阵无法形成可靠闭环，必须在进入正式开发前比较：

1. 增加受控的 SSH_ASKPASS 辅助流程。
2. 使用 Rust SSH 客户端配合 `russh-sftp`，自行承担配置、认证和主机指纹验证。

不能把认证缺口留到正式版本，也不采用“解析 `sftp` CLI 输出”的方案。

### 文件操作

第一版提供：

- 列目录、进入目录、返回上级、刷新。
- 查看名称、类型、大小、权限和修改时间。
- 新建目录、重命名。
- 上传、下载、覆盖策略和取消。
- 删除文件或目录，执行前明确提示远程删除通常不可恢复。
- 双击目录进入；双击文件默认下载，远程编辑后续再做。

传输规则：

- 大文件必须流式读写，禁止整文件读入内存。
- 下载先写本地临时文件，成功后原子替换目标。
- 上传优先写远端临时文件，成功后重命名；服务端不支持时明确降级。
- 并发传输数量、单任务缓冲和任务历史必须有上限。
- 软链接默认不递归跟随，避免循环目录和越界删除。
- 覆盖、删除、修改权限等高风险操作必须二次确认。

## Crate 与依赖方向

建议新增：

```text
ramag-bin
  ├─ ramag-tool-ssh              GPUI 页面与工作区
  │    ├─ ramag-terminal         通用 GPUI 终端视图
  │    ├─ ramag-app              SshService 与用例编排
  │    └─ ramag-domain           SSH 领域模型
  └─ ramag-infra-ssh             系统 OpenSSH、SFTP 与传输实现
       └─ ramag-domain           SshProfile / RemoteEntry / TransferTask / trait
```

职责：

- `ramag-domain`：配置、远程文件、传输状态和抽象接口，不依赖 GPUI。
- `ramag-app`：配置 CRUD、连接测试、文件操作和传输任务编排。
- `ramag-infra-ssh`：安全构造 OpenSSH 命令、管理子进程、实现结构化 SFTP。
- `ramag-terminal`：只负责终端状态与 GPUI 显示，不包含 SSH 业务页面。
- `ramag-tool-ssh`：连接管理页、工作区、文件树、Terminal 标签和传输队列。
- `ramag-bin`：依赖注入、注册工具、绑定快捷键。

现有 `ramag-infra-tunnel` 暂不改动。如果实现后确认存在稳定且完全相同的命令构造逻辑，
再在测试保护下提取公共帮助函数，不把重构作为 SSH 工具的前置条件。

## Runtime 与线程模型

- GPUI / smol：只执行 UI 状态更新与轻量事件分发。
- Terminal PTY：独立 I/O 循环，不在 GPUI 主线程阻塞读取。
- SFTP：新增独立 Tokio runtime，避免长时间传输占用数据库 runtime。
- 本地文件 I/O：通过受限后台任务执行。
- UI 更新合并后按批次提交，禁止每收到一个字节就重绘整个终端。

终端滚动历史、文件树节点、传输队列和并发任务都必须设置显式预算；具体默认值在 PoC
基准测试后固定。

## 数据与安全

- SSH 配置复用现有加密 Storage，敏感字段不明文落盘。
- 第一版优先使用系统 SSH 配置、Agent 和密钥，不保存密码。
- 私钥只记录路径，不读取或复制私钥内容到数据库。
- 未知主机指纹必须由用户明确确认，不能静默接受。
- 连接测试和错误提示不得回显密码、私钥内容或完整敏感命令。
- 远程路径和本地路径进入文件操作前分别校验。
- 终端输出默认不持久化，避免命令结果和密钥材料意外落盘。
- 日志使用英文并包含 profile/session/task 标识，但不记录敏感内容。

## 许可证边界

Ramag 继续保持 Apache-2.0：

| 组件 | 处理方式 |
|---|---|
| GPUI | 直接使用，Apache-2.0 |
| gpui-component | 继续作为 GPUI 组件库使用 |
| alacritty_terminal | 计划直接依赖，Apache-2.0 |
| russh-sftp | SFTP PoC 候选，当前声明 Apache-2.0 |
| 系统 OpenSSH | 作为独立系统进程调用 |
| Zed terminal / terminal_view | 不复制、不链接，GPL-3.0-or-later |

引入新版本前必须检查直接和传递依赖许可证，不能只看仓库首页。

实现审计确认，本次新增的 `alacritty_terminal 0.26.0`、`russh-sftp 2.3.0` 及其新增
传递依赖均提供 Apache-2.0、MIT 或其他兼容许可，且依赖图中没有 Zed 的 `terminal` /
`terminal_view`。同时，项目在本功能之前锁定的 GPUI 依赖图已包含标记为 GPL-3.0-or-later
的 `zlog`、`ztracing` 与 `ztracing_macro`；本功能没有引入或扩大这组依赖，但在完成仓库级
许可证整改前，不能把“整个锁定依赖图仅含 Apache 兼容许可”作为发布结论。

## 实施与交付

### 阶段 0：内部技术 PoC，不对外交付

- GPUI 显示本地 Shell，完成输入、输出、尺寸变化和退出回收。
- 在同一终端视图启动系统 SSH。
- 验证中英文、Emoji、大量输出和全屏程序。
- 打通 `ssh -s ... sftp` 与结构化 SFTP API。
- 验证 Windows OpenSSH 发现、ConPTY、无窗口 SFTP 和缺失能力提示。
- 在 macOS、Windows 分别验证 Agent、密钥、known_hosts 和认证矩阵。
- 完成许可证、MSRV 和传递依赖审计。

验收：终端和 SFTP 两条链路均可稳定关闭，无卡 UI、无孤儿进程、无 GPL 代码。PoC 失败
则先调整底层方案，不能带着未解决风险进入正式工具开发。

### 阶段 1：首个完整版本，一次性交付

这一阶段内部可按下列顺序开发，但只有全部完成后才算交付：

1. 领域模型、加密存储、配置 CRUD、校验和连接测试。
2. OpenSSH 能力发现、错误提示和 macOS / Windows 进程生命周期。
3. GPUI TerminalView、多 Terminal 标签、重连和断开状态。
4. SFTP 目录树、刷新、路径导航和文件元数据。
5. 上传、下载、取消、重试、进度与有界传输队列。
6. 新建目录、重命名、删除、覆盖确认和临时文件安全落盘。
7. 工作区占位与最后远程路径恢复，不恢复终端敏感输出。
8. 单元、集成、渲染、资源、安全和双平台测试。
9. 性能基准、长时间稳定性和进程泄漏验证。

完整版本验收：

- 用户可以保存、测试、编辑和删除 SSH 配置。
- 用户可以连接服务器、运行交互命令并管理多个 Terminal。
- 用户可以浏览目录并完成上传、下载、新建、重命名和删除。
- 大文件全程流式传输，断网和取消不会留下被误认为完整文件的结果。
- 不支持的认证方式在连接前明确说明。
- macOS 与 Windows 达到相同核心功能，不把单平台实现视为完成。
- 所有进程、任务和队列可正确关闭，无 UI 阻塞、孤儿进程和无界内存。
- 许可证审计确认 Ramag 继续保持 Apache-2.0。

以上任一项未完成，阶段 1 都不能标记完成，也不开放正式工具入口。

### 阶段 2：可选增强

- 终端搜索、超链接、鼠标协议增强。
- 拖放上传下载。
- 可选 ControlMaster 连接复用。
- 密码 / AskPass 安全交互。

是否加入分屏、远程编辑、目录同步，基于真实需求另行决策。

## 测试清单

- 单元测试：配置校验、SSH 参数构造、路径处理、状态机、取消和覆盖策略。
- 终端测试：ANSI、备用屏幕、resize、Unicode、IME、复制粘贴、大量输出。
- SFTP 集成测试：临时 OpenSSH 服务，覆盖目录、大小文件、权限、软链接和中断恢复。
- GPUI 渲染测试：连接页、文件树、终端容器、传输状态。
- 资源测试：滚动历史、十万目录项、大文件传输和并发任务的内存上限。
- 平台测试：macOS Apple Silicon / Intel、Windows 10/11 x64。
- 安全测试：参数注入、恶意路径、未知主机、日志脱敏、删除和覆盖确认。

## 首版已确定事项

阶段 0 与首版实现已经锁定以下结论：

1. 终端内核锁定 `alacritty_terminal 0.26.0`，直接使用其 Unix PTY / Windows ConPTY
   能力，不再补充第二个 PTY crate。
2. SFTP 锁定“系统 OpenSSH 子系统 + `russh-sftp 2.3.0`”，不引入完整 Rust SSH 栈。
3. 首版不增加密码 / AskPass；SFTP 保持 BatchMode，交互认证只在 Terminal 中完成。
4. 首版不启用 ControlMaster；Terminal 与 SFTP 各自维护独立进程。
5. 默认预算为：终端历史 10,000 行；目录 100,000 项且保留数据不超过 64 MiB；递归
   删除深度 64、项目 100,000 且路径数据不超过 64 MiB；并发传输 3、活动队列 64、
   完成历史 100；同时打开 16 个工作区，每个工作区最多 8 个 Terminal。

## 参考

- [GPUI](https://github.com/zed-industries/zed/tree/main/crates/gpui)
- [alacritty_terminal](https://docs.rs/crate/alacritty_terminal/latest)
- [russh-sftp](https://docs.rs/russh-sftp/latest/russh_sftp/)
- [OpenSSH manuals](https://www.openssh.com/manual.html)
- [Microsoft：OpenSSH for Windows](https://learn.microsoft.com/windows-server/administration/openssh/openssh-overview)
- [Microsoft：Windows OpenSSH 密钥管理](https://learn.microsoft.com/windows-server/administration/openssh/openssh_keymanagement)
- [Ramag 架构说明](architecture.md)
