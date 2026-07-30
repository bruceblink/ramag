# SSH + SFTP Linux / Windows 远端支持与生产诊断实施计划

## 文档状态

- 状态：设计与实施计划，尚未实现
- 核对日期：2026-07-30
- 上位设计：[SSH + SFTP 生产低影响只读诊断模式设计](ssh-sftp-design.md)
- 适用范围：远端 Linux、远端 Windows Server、普通 SSH Terminal、SFTP、生产安全诊断终端、测试与分阶段交付
- 本地客户端范围：沿用 Ramag 当前 macOS、Windows 支持，不在本文讨论 Linux 桌面客户端

本文统一分析远端 Linux 与远端 Windows Server。目标不是简单做到“SSH 可以连接”，而是分别确认以下能力是否满足生产级要求：

1. 普通 SSH Terminal 可以稳定连接、交互和退出。
2. SFTP 可以正确处理远端路径、文件类型、传输和平台文件语义。
3. 生产模式可以排查问题，但不提供修改远端数据或改变系统状态的入口。

产品行为、安全承诺、允许与禁止操作、资源预算及服务器边界以上位设计为准。本文把目标架构设计为同时容纳 Linux 与 Windows，但各平台、各能力必须独立通过验收后才能启用；不能因为其中一个能力可用，就显示“该平台已完整支持”。

上位设计当前仍建议第一批安全诊断先交付 Linux。本文不取消这一安全门禁，而是在同一架构中补齐 Windows 后续里程碑：P1.2 可以先发布 Linux，P1.3 至 P1.5 分别通过后再开放 Windows 对应能力。如果产品决定 Linux、Windows 同版本发布，应同步修改上位设计的平台范围，不能只改实施计划。

## 结论先行

### 当前状态

| 能力 | 远端 Linux | 远端 Windows Server |
|---|---|---|
| SSH 认证与普通终端 | 当前主要目标，现有 `ssh -tt` 链路可用 | 协议和现有链路具备可行性，但缺少正式集成验证 |
| SFTP 握手 | 当前主要目标，已有真实集成测试 | Windows OpenSSH 支持 SFTP，现有客户端协议层理论可连接 |
| SFTP 浏览与传输 | 按 POSIX 路径实现 | 当前不能正式支持；盘符、根目录、路径与 ACL 语义尚未适配 |
| 生产模式 SFTP | 已在应用层和基础设施层禁止远端写操作 | 写门禁本身与平台无关，但 Windows 读取链路仍受路径兼容问题影响 |
| 生产模式终端 | **当前不安全**：仍会启动完整交互 Shell | **当前不安全**：同样会启动完整 `cmd.exe`、PowerShell 或其他默认 Shell |
| 结构化安全诊断 | 尚未实现 | 尚未实现，需要 Windows 专用诊断提供者 |

### 目标状态

1. 非生产连接在 Linux 和 Windows 上都可以使用普通 SSH Terminal。
2. SFTP 使用独立于本地操作系统的远端路径模型，同时支持 POSIX 路径、Windows 盘符路径和服务端虚拟根目录。
3. 生产连接在任何远端平台都禁止完整交互 Shell。
4. Linux 与 Windows 分别使用经过验证的诊断提供者，向 UI 返回同一种结构化结果。
5. 平台未知、能力未验证或操作未实现时失败关闭，不回退到完整 Shell。
6. Windows 支持基线先限定为 Microsoft OpenSSH Server，不把 Cygwin、WSL SSH 或第三方 SSH/SFTP Server 自动算作兼容。

### 推荐交付策略

架构从第一天同时考虑 Linux 和 Windows，但发布按能力逐项开放：

1. 先建立所有平台通用的生产终端硬门禁。
2. 再完成远端平台与 SFTP 命名空间抽象，避免继续累积 POSIX 假设。
3. 分别认证 Linux、Windows 的普通 SSH 和 SFTP。
4. 分别实现 Linux、Windows 的结构化生产诊断。
5. 高安全场景增加服务端安全网关和独立只读账号。

这样可以避免为了尽快显示“支持 Windows”，给用户造成错误的安全预期。

## 当前实现分析

### 普通 SSH Terminal

现有链路通过系统 OpenSSH 构造 `ssh -tt`，不在本地解析远端 Shell：

```text
TerminalView
    ↓
TerminalCore / 本地 PTY
    ↓
系统 OpenSSH：ssh -tt
    ↓
远端默认 Shell
```

对应实现位于：

- [`ramag-infra-ssh/src/command.rs`](../crates/ramag-infra-ssh/src/command.rs)
- [`ramag-terminal/src/core.rs`](../crates/ramag-terminal/src/core.rs)

这条链路对普通终端有利：远端是 Bash、Zsh、`cmd.exe` 或 PowerShell，都不需要在 Ramag 中实现一套 Shell。但它也意味着生产模式无法通过本地危险命令黑名单获得可靠保护，因为终端发送的是原始字节，远端最终行为不可判定。

当前应用服务和基础设施没有拒绝生产配置启动终端，现有测试还明确断言“生产配置允许 Terminal”。因此，在 P0 完成前，`production = true` 只能理解为“SFTP 远端写保护”，不能宣传为“终端只读”。

### SFTP

现有 SFTP 通过系统 OpenSSH 启动子系统：

```text
ssh -T ... -s -- <host> sftp
```

Ramag 在 stdin/stdout 上运行结构化 SFTP 协议，不依赖远端默认 Shell。这个选择对 Windows 有利：即使 Windows OpenSSH 默认 Shell 是 `cmd.exe`，SFTP 子系统仍可以独立工作。

现有实现已经具备：

- 有界目录读取。
- 有界文件预览与分段读取。
- 流式上传、下载和目录归档。
- 会话缓存、连接超时、取消和错误映射。
- 生产配置的应用层、基础设施层远端写门禁。

对应实现位于：

- [`ramag-infra-ssh/src/session.rs`](../crates/ramag-infra-ssh/src/session.rs)
- [`ramag-infra-ssh/src/transfer.rs`](../crates/ramag-infra-ssh/src/transfer.rs)
- [`ramag-app/src/usecases/ssh_service.rs`](../crates/ramag-app/src/usecases/ssh_service.rs)

### 当前 POSIX 假设

Windows SFTP 的主要阻碍不是协议，而是现有路径和文件语义：

- 初始路径只接受 `.` 或以 `/` 开头的绝对路径。
- 根目录被固定理解为 `/`。
- 路径拼接、父目录、面包屑和临时文件路径按 `/` 进行字符串处理。
- 收藏路径必须以 `/` 开头。
- 删除保护要求路径以 `/` 开头，并把 `/` 作为唯一根目录。
- 集成测试要求测试根目录是名称含 `ramag` 的 POSIX 绝对路径。
- 目录归档把 SFTP 的 `permissions`、`uid`、`gid` 写入 tar 头，隐含 POSIX 元数据语义。
- 编辑和覆盖会创建同目录临时文件再重命名，并尝试保留 POSIX 权限；该流程不能自动保留完整 NTFS ACL。

主要位置包括：

- [`ramag-domain/src/entities/ssh.rs`](../crates/ramag-domain/src/entities/ssh.rs)
- [`ramag-tool-ssh/src/views/ops_files.rs`](../crates/ramag-tool-ssh/src/views/ops_files.rs)
- [`ramag-tool-ssh/src/views/render_directory_helpers.rs`](../crates/ramag-tool-ssh/src/views/render_directory_helpers.rs)
- [`ramag-tool-ssh/src/views/path_dialog.rs`](../crates/ramag-tool-ssh/src/views/path_dialog.rs)
- [`ramag-infra-ssh/src/transfer/commit.rs`](../crates/ramag-infra-ssh/src/transfer/commit.rs)
- [`ramag-infra-ssh/tests/integration.rs`](../crates/ramag-infra-ssh/tests/integration.rs)

### 连接测试职责混合

当前 `test_connection` 实际会建立 SFTP 会话并执行 `canonicalize(initial_path)`。这会导致：

- SSH Terminal 可用但 SFTP 被服务器禁用时，整个“连接测试”失败。
- 无法告诉用户究竟是认证、Terminal、SFTP 子系统还是初始目录失败。
- Windows 远端的路径问题可能被误报为 SSH 不可用。

目标实现必须把认证/执行能力、Terminal 和 SFTP 分开探测并分别显示状态。

## 正式支持基线

### Linux 远端

第一批正式验证建议覆盖：

- Ubuntu Server 22.04 LTS、24.04 LTS。
- Debian 12。
- Rocky Linux 9 或同类 RHEL 兼容发行版。
- 系统 OpenSSH Server。
- systemd 与非 systemd 能力通过诊断提供者能力探测区分。

没有经过验证的发行版仍可尝试普通 SSH/SFTP，但安全诊断只开放已确认存在且行为一致的操作。

### Windows 远端

第一批正式验证限定为：

- Windows Server 2019。
- Windows Server 2022。
- Windows Server 2025。
- Microsoft 提供的 OpenSSH Server。
- Windows PowerShell 5.1 作为诊断提供者最低基线；PowerShell 7 只作为额外兼容项。

远端前置条件：

1. OpenSSH Server 已安装并启动。
2. SSH 端口已被防火墙和网络策略允许。
3. SFTP 子系统已启用。
4. 账号可以使用密码、公钥或现有 SSH 配置完成认证。
5. 生产诊断账号拥有所需读取权限，但不属于 Administrators，也没有等价高权限。

微软文档说明 Windows Server 2019/2022/2025 和 Windows 10/11 可以运行 OpenSSH Server；Windows Server 2025 默认安装 OpenSSH。产品第一阶段只认证 Server 版本，不把 Windows 桌面系统自动纳入正式支持范围。

### 首版不覆盖

- Cygwin、MSYS2、WSL 内部的 SSH Server。
- 第三方 Windows SSH/SFTP Server。
- UNC 路径和映射网络盘。
- Windows 设备路径，例如 `\\.\`、`\\?\`。
- NTFS Alternate Data Streams，例如 `file.txt:stream`。
- 生产模式中的任意脚本、任意 PowerShell、WMI/CIM 自定义查询或用户自定义白名单命令。

这些能力后续可以按真实需求单独评估，不能与 Microsoft OpenSSH 的验收结果混用。

## 统一实现原则

1. **远端系统与本地系统分离**：不能根据 Ramag 运行在 macOS 还是 Windows 推断远端系统。
2. **远端系统与 SFTP 命名空间分离**：Windows 服务器也可能通过 SFTP 暴露虚拟 `/`；Linux chroot 也可能暴露受限根目录。
3. **能力独立**：SSH 认证、Terminal、SFTP、诊断提供者分别记录状态。
4. **生产终端默认拒绝**：任何平台的生产连接都不能创建完整交互 Shell。
5. **结构化请求**：生产诊断不接受 `program`、`args`、脚本或原始命令字符串。
6. **失败关闭**：平台、路径风格、能力或策略不明确时拒绝对应操作。
7. **拒绝发生在远端执行前**：未知操作和非法参数不能启动 SSH 子进程或发送 SFTP 写请求。
8. **双层门禁**：UI 提供及时反馈，应用层负责业务规则，基础设施层防止绕过。
9. **资源有界**：诊断、目录读取、文件片段、输出、并发和超时都有硬上限。
10. **不隐式降级**：安全诊断失败后不回退完整 Shell，不扩大范围，不自动重试。

## 目标架构

```text
SshProfileId
    ↓
SshService 读取最新持久化配置
    ↓
RemoteCapabilityProbe
    ├─ SSH 认证 / 远端执行能力
    ├─ RemoteOs：Linux / Windows / Unknown
    ├─ SFTP 可用性与命名空间
    └─ DiagnosticProvider 能力
    ↓
    ├─ production = false
    │      ├─ 普通 Terminal：ssh -tt
    │      └─ SFTP：按平台无关 RemotePath 执行读写
    │
    └─ production = true
           ├─ 普通 Terminal：Forbidden
           ├─ SFTP：只允许结构化读操作
           └─ SafeDiagnosticTerminal
                  ↓
              结构化操作与参数
                  ↓
              策略 / 平台 / 资源预算
                  ↓
              LinuxDiagnosticProvider
                    或
              WindowsDiagnosticProvider
                  ↓
              ssh -T / 服务端安全网关
```

核心安全断言：生产配置调用完整终端时，必须在创建 `ssh -tt` 子进程前返回 `Forbidden`。

## 领域模型建议

### 平台偏好与探测结果

配置只保存用户偏好，真实探测结果属于当前远端会话：

```rust
pub enum RemotePlatformPreference {
    Auto,
    Linux,
    Windows,
}

pub enum RemoteOperatingSystem {
    Linux,
    Windows,
    Unknown,
}

pub enum RemoteShellKind {
    Posix,
    Cmd,
    WindowsPowerShell,
    PowerShellCore,
    Unknown,
}

pub enum SftpNamespaceKind {
    Posix,
    WindowsDrive,
    Virtual,
    Unknown,
}
```

`RemoteShellKind` 只用于普通终端兼容提示，不能成为生产安全判断依据。即使探测到 PowerShell，生产模式也不能开放 PowerShell 输入框。

建议把会话能力建模为：

```rust
pub struct SshRemoteCapabilities {
    pub operating_system: RemoteOperatingSystem,
    pub shell: RemoteShellKind,
    pub sftp_available: bool,
    pub sftp_namespace: SftpNamespaceKind,
    pub diagnostic_provider: Option<SshDiagnosticProviderKind>,
}
```

不要通过“尝试创建文件”探测 SFTP 写权限。写权限探测本身会修改远端数据；非生产写操作按实际请求处理，生产模式始终禁止。

### 远端路径

不能继续用无类型 `String` 配合 `starts_with('/')` 判断远端路径。建议新增值对象：

```rust
pub struct RemotePath {
    canonical: String,
    namespace: SftpNamespaceKind,
}
```

它至少提供以下受控操作：

- `parse_server_canonical`
- `join_child`
- `parent`
- `is_root`
- `is_same_location`
- `temporary_sibling`
- `breadcrumbs`

规则：

1. 远端路径不能使用本地 `std::path::Path` 解析，因为本地操作系统可能与远端不同。
2. `realpath(".")` 返回值是命名空间探测的重要输入，但不能单独证明远端操作系统。
3. 始终保留服务器返回的规范路径形式，不擅自把 `C:/` 改成 `/C:/`，反之亦然。
4. Windows 盘符根目录、SFTP 虚拟根目录和 POSIX `/` 都必须被识别为受保护根，禁止删除。
5. Windows 路径不能简单全量转小写。NTFS 通常大小写不敏感，但目录可以启用大小写敏感；显示和请求必须保留服务器返回的原始拼写。
6. 父目录和面包屑必须基于已识别的命名空间规则，不能只做 `rsplit('/')`。
7. 用户输入路径先做通用协议校验，再在连接后按探测到的命名空间做严格校验。
8. 工作区恢复路径和收藏路径必须记录或重新确认命名空间；平台变化时不能沿用未经验证的旧路径。

### 诊断请求

生产诊断请求使用封闭枚举，不暴露任意程序或命令：

```rust
pub enum SshDiagnosticOperation {
    SystemOverview,
    ResourceSnapshot,
    ProcessList,
    NetworkSnapshot,
    DiskOverview,
    FileMetadata { path: RemotePath },
    FileChunk {
        path: RemotePath,
        position: RemoteFileChunkPosition,
    },
    LogQuery {
        source: SshLogSource,
        max_items: u16,
        since: Option<DiagnosticTimeRange>,
    },
    ServiceStatus { name: SshServiceName },
}
```

建议新增 `crates/ramag-domain/src/entities/ssh_terminal.rs` 和 `ssh_remote_path.rs`，避免继续扩大 `ssh.rs`。

结果至少包含：

- 标准化后的结构化数据或有界文本。
- 远端平台与诊断提供者类型。
- 退出码或终止原因。
- 是否因为大小或行数限制截断。
- 实际耗时。
- 平台、权限或操作不支持时的稳定错误代码。

领域模型不得依赖 GPUI、PTY、PowerShell 对象或具体 OpenSSH 实现。

## 远端平台与能力探测

### 为什么不能看 SSH Banner

本地 `ssh -V` 只能说明客户端版本。服务器 Banner 可能被代理、堡垒机或管理员修改，也不能可靠区分 Linux、Windows 和自定义 OpenSSH 环境。因此不能用字符串包含 `OpenSSH_for_Windows` 作为唯一判断。

### 推荐流程

1. 探测本地 OpenSSH Client。
2. 分别执行 SSH 认证/远端执行测试与 SFTP 子系统测试。
3. SFTP 成功后调用 `realpath(".")`，记录服务器返回的规范路径和命名空间候选。
4. 只有进入安全诊断前才执行固定、无用户参数的平台识别操作。
5. `Auto` 模式根据 SFTP 路径只选择探测顺序，不把路径形式当作最终证据。
6. 用户显式选择的平台与探测结果冲突时拒绝诊断，并提示重新确认配置。
7. 结果只缓存到当前连接生命周期；配置、主机、用户、端口或 SSH 别名变化后重新探测。

平台识别本身也必须是结构化、只读、有超时和输出上限的操作。识别失败返回 `RemotePlatformUnknown`，不得试探任意 Shell，也不得回退完整终端。

### 连接测试拆分

UI 建议分别显示：

| 检查项 | 成功含义 |
|---|---|
| OpenSSH Client | 本地可安全启动受支持的 `ssh` 可执行文件 |
| SSH Authentication | 可以完成主机校验和身份认证 |
| Remote Exec | 可以执行固定的非交互能力探测 |
| Interactive Terminal | 可以分配 PTY 并启动远端默认 Shell |
| SFTP Subsystem | 可以完成 SFTP 握手和默认目录规范化 |
| Safe Diagnostic | 平台已识别且对应诊断提供者可用 |

Terminal 可用而 SFTP 不可用时，非生产连接可以只开放 Terminal；生产连接则只开放已经通过验证的读能力。

## 普通 SSH Terminal 实施

### Linux

继续使用现有 `ssh -tt` 链路，重点回归：

- Bash、Zsh 和服务器自定义默认 Shell。
- UTF-8、中文输入、粘贴和 ANSI 控制序列。
- 窗口尺寸变化和终端退出状态。
- 密码、公钥、Agent、SSH config 别名和 ProxyJump。

### Windows

微软 OpenSSH 的初始默认 Shell 是 `cmd.exe`，管理员可以通过注册表 `DefaultShell` 改成 Windows PowerShell、PowerShell 7 或其他 Shell。Ramag 普通终端不能假定远端一定是 PowerShell。

需要验证：

- `cmd.exe`、Windows PowerShell 5.1、PowerShell 7。
- 本地 macOS 和本地 Windows 连接同一 Windows Server。
- 中文、宽字符、组合字符、换行和退格。
- Ctrl+C、Ctrl+Break 可观察行为、退出码和断线。
- PTY 尺寸变化、全屏程序和 ANSI/VT 序列。
- 默认代码页不是 UTF-8 时的显示降级和提示。
- 域账号 `domain\user`、本地账号、公钥和密码认证。

普通终端不承诺只读；它只在 `production = false` 时存在。

### 生产模式硬门禁

`SshService::terminal_command` 应根据 `profile_id` 重新读取最新配置。生产配置直接返回 `DomainError::Forbidden`，非生产配置才返回 `SshLaunchCommand`。

基础设施的终端命令构造再检查一次 `profile.production`，防止未来新增调用路径绕过应用层。拒绝发生在 OpenSSH 探测、AskPass 令牌创建和 PTY 创建之前。

## SFTP 跨平台实施

### 共同协议层

Linux 和 Windows 继续复用当前结构化 SFTP 会话、流式传输和资源预算。以下能力不应复制两套实现：

- 握手、请求超时和包大小限制。
- 有界目录读取。
- 文件分段读取。
- 传输取消、进度和本地原子提交。
- 会话缓存和连接错误恢复。
- 生产模式写操作门禁。

平台差异集中在 `RemotePathPolicy`、元数据解释和写入提交策略中。

### Linux 路径与文件语义

- 使用 POSIX `/` 根目录和 `/` 分隔符。
- 保留大小写。
- 支持现有 POSIX 权限展示。
- 软链接不在通用文件预览中跟随。
- 删除、归档和临时文件路径继续遵守现有深度、条目数和内存上限。

### Windows 路径与文件语义

必须覆盖以下情况：

1. `C:/Users/...` 等盘符路径。
2. 服务器可能返回的虚拟 `/` 根目录或 chroot 根目录。
3. 当前目录位于非 C 盘。
4. 路径包含空格、中文和合法特殊字符。
5. 盘符大小写和服务器保留的原始路径拼写。
6. Windows 保留名称、末尾点/空格、设备路径和 ADS。
7. 符号链接、目录联接点和其他 reparse point。
8. 文件被其他进程占用时的打开、重命名和删除错误。

首版策略：

- 只接受服务器 `realpath` 已确认的路径形式。
- 用户新建名称按 Windows 文件名规则校验；远端已存在条目以服务器返回结果为准。
- 首版拒绝 UNC、设备路径和 ADS。
- 通用预览、下载和目录归档不跟随未知 reparse point。
- 当前 SFTP 属性模型无法完整表达 NTFS ACL；Windows UI 不把 `permissions` 字段宣传为真实 ACL。
- Windows 目录下载生成的 tar.gz 只保存文件内容和可表达的基础元数据，不承诺保存或还原 NTFS ACL。

### Windows 写操作的特殊风险

创建新文件、重命名和删除可以通过 SFTP 协议实现，但“编辑或覆盖现有文件”还有 ACL 与提交语义问题：

1. 当前实现创建同目录临时文件，再把原文件改名为备份，最后提交临时文件。
2. Windows 新临时文件通常继承父目录 ACL，不一定继承原文件 ACL。
3. SFTP 的 POSIX `permissions` 不能完整表示 NTFS ACL。
4. 文件被占用、杀毒软件扫描或共享模式限制时，重命名可能失败。
5. 即使文件内容替换成功，也不能自动声称原 ACL 完整保留。

因此 Windows SFTP 分级开放：

| 能力 | 首次开放条件 |
|---|---|
| 浏览、预览、单文件下载 | 路径模型和真实 Windows 集成测试通过 |
| 目录归档下载 | reparse point、文件占用和 tar 元数据策略通过测试 |
| 新建目录、上传新文件、重命名、删除 | 仅非生产模式，根路径保护和回滚测试通过 |
| 编辑或覆盖现有文件 | ACL 保留策略明确并通过测试；否则保持禁用 |

如果产品必须完整保留 ACL，推荐由受控 Windows 服务端助手调用 Win32 文件替换和 ACL API；不能用“复制几个 POSIX mode 位”假装已经保留 ACL。

### 生产模式 SFTP

两种远端平台保持相同产品行为：

| 操作 | 生产模式 |
|---|---|
| 列目录、查看元信息 | 允许，有界 |
| 预览和分段读取普通文件 | 允许，有界 |
| 下载文件、下载目录归档 | 允许；只写本地，仍需提示敏感数据风险 |
| 上传、新建、编辑、覆盖 | 禁止 |
| 重命名、移动、删除 | 禁止 |
| 修改权限、ACL、所有者、时间 | 禁止 |

门禁必须同时存在于应用层和基础设施层。生产 UI 不渲染写入口，但 UI 隐藏不能替代后端拒绝。

## 安全诊断终端实施

### 接口方向

`SshDriver` 增加平台探测和结构化诊断接口：

```rust
async fn probe_remote_capabilities(
    &self,
    profile: &SshProfile,
) -> Result<SshRemoteCapabilities>;

async fn execute_diagnostic(
    &self,
    profile: &SshProfile,
    capabilities: &SshRemoteCapabilities,
    operation: &SshDiagnosticOperation,
    cancellation: DiagnosticCancellation,
) -> Result<SshDiagnosticResult>;
```

最终是否流式返回根据 UI 实际需求决定。第一版单次输出上限为 2 MiB，可以先返回有界结果，避免为持续日志引入复杂流协议。

### 统一操作映射

下表中的命令或 API 是实现候选，不是允许用户输入的命令。最终模板必须固化在代码或服务端网关中。

| 诊断操作 | Linux 提供者 | Windows 提供者 | 共同限制 |
|---|---|---|---|
| 系统概况 | `uname`、`/etc/os-release`、运行时间 | `[Environment]::OSVersion`、固定 CIM 类 | 无用户参数；5 秒；256 KiB |
| 资源快照 | `/proc/meminfo`、`/proc/loadavg`、固定 CPU 快照 | 固定 `Win32_OperatingSystem`、`Win32_Processor` 字段 | 单次快照；禁止持续采样 |
| 进程列表 | 固定字段的 `ps` 或受控 `/proc` 读取 | `Get-Process` 后只选择 PID、名称、CPU、内存等字段 | 最多 5,000 项；默认不返回完整命令行 |
| 网络快照 | 固定参数的 `ss` | `Get-NetTCPConnection` | 被动读取；不执行 ping/curl/端口扫描 |
| 磁盘概况 | 固定参数的 `df` | `Get-Volume` 或固定 CIM 逻辑磁盘类 | 不递归统计目录；5 秒 |
| 文件元信息 | SFTP `lstat/fstat` | SFTP `lstat/fstat` | 每次一个路径；不跟随链接 |
| 文件片段 | SFTP 有界读取 | SFTP 有界读取 | 每次最多 2 MiB；不持续跟随 |
| 系统日志 | 固定 `journalctl` 查询 | `Get-WinEvent` 固定 `FilterHashtable` | 固定日志源、时间范围和最大条数 |
| 文本日志 | SFTP 文件片段 | SFTP 文件片段 | 普通文件；路径和大小受限 |
| 服务状态 | `systemctl show/status` 的固定只读字段 | `Get-Service` 的固定只读字段 | 精确服务名；禁止通配符和状态操作 |

补充规则：

- 数据库诊断继续使用 Ramag 数据库工具的生产只读保护，不通过 SSH 启动数据库 CLI。
- Docker、Kubernetes、云 CLI 和远程管理工具不进入首版安全诊断。
- 搜索、排序、筛选尽量在本地结果上完成。
- Windows Event Log 首版只允许明确列出的日志源，例如 `System`、`Application`；不接受任意 XPath、XML 或通配符。
- Security 日志涉及权限和敏感数据，默认不进入首版允许列表。
- 进程命令行、服务二进制路径、环境变量和事件正文可能包含密钥，默认只返回必要字段。

### Linux 提供者

无服务端网关时，只允许代码内固定的程序和参数组合：

- 平台固定程序路径或经过严格能力探测的受支持程序。
- 用户参数使用值对象限制字符集和长度。
- 支持 `--` 的程序必须终止选项解析。
- 不使用用户可控的管道、重定向、命令替换和环境变量。
- `systemctl`、`journalctl` 不启用分页器，不进入交互模式。
- 缺少工具或版本行为未验证时返回能力不支持，不尝试任意替代命令。

### Windows 提供者

Windows 诊断不能依赖远端默认 Shell。推荐显式启动系统自带的 `powershell.exe`，使用：

- `-NoLogo`
- `-NoProfile`
- `-NonInteractive`
- 固定的 `-EncodedCommand` 启动器

安全要求：

1. `EncodedCommand` 只承载 Ramag 内置、版本固定的启动器，不包含用户脚本。
2. 结构化参数通过受限 stdin JSON 传递，不拼进 PowerShell 命令文本。
3. stdin 请求有独立字节上限和严格反序列化 schema。
4. PowerShell 脚本只调用固定 cmdlet、固定 .NET API 和固定 CIM 类。
5. 禁止 `Invoke-Expression`、动态 ScriptBlock、`Start-Process`、`cmd /c` 和从输入加载模块或程序集。
6. 不使用 `-ExecutionPolicy Bypass` 绕过服务器管理员策略；策略不允许时明确失败。
7. 设置进程内输出编码并使用压缩 JSON，不能依赖表格化文本或当前代码页。
8. `$ErrorActionPreference = 'Stop'`，每类错误映射为稳定错误码。
9. 输出前只选择允许字段，不能直接序列化完整 PowerShell/CIM 对象。
10. 默认 Shell 是 `cmd.exe`、Windows PowerShell 或 PowerShell 7 时都要通过测试；自定义 Shell 无法启动固定提供者时返回不支持。

`-EncodedCommand` 只解决固定脚本的传输与转义问题，不是服务器安全边界。用户仍可使用其他 SSH 客户端登录时，必须依赖独立账号、ACL 和服务端网关。

### 执行资源边界

初始预算沿用上位设计：

| 资源 | 初始限制 |
|---|---|
| 单配置并发诊断 | 1 个 |
| 全局并发诊断 | 4 个 |
| 默认超时 | 10 秒 |
| 单操作硬超时 | 30 秒 |
| 单次输出 | 2 MiB |
| 单次文本行数或结构化项目 | 5,000 |
| 单次文件片段 | 2 MiB |
| 自动刷新最短间隔 | 5 秒 |
| 自动重试 | 0 次 |

基础设施必须并发读取 stdout/stderr，分别限制字节数。达到上限立即终止本地 SSH 子进程并返回截断或超限错误，不能继续在内存中收集。

关闭本地 SSH 不保证已经脱离会话的远端进程一定退出。因此固定诊断实现禁止创建后台任务；需要强保证时由服务端网关实施超时和进程树回收。Windows 网关可使用 Job Object 等系统机制，Linux 网关可使用服务管理器或资源限制执行受控子任务。

## 服务端强制保护

### 两种保护等级

| 等级 | 机制 | 能保证什么 | 不能保证什么 |
|---|---|---|---|
| 客户端保护 | Ramag UI、应用层和基础设施层门禁 | 防止通过 Ramag 误开完整 Shell 或执行写操作 | 用户仍可改用其他 SSH 客户端 |
| 服务端保护 | 独立账号、最小 ACL、`ForceCommand`、安全网关、资源限制 | 限制该凭据从任何客户端获得的能力 | 仍会产生必要读取开销和系统审计记录 |

如果产品承诺只是“防止 Ramag 中的误操作”，客户端保护可以先交付。如果要求“该凭据无论使用什么客户端都不能修改系统”，服务端保护是验收必选项。

### Linux 建议

- 使用独立只读账号和独立密钥。
- 不授予 `sudo`，不加入 Docker 等等价高权限组。
- 通过 Unix 权限、ACL 或只读挂载限制文件写入。
- 诊断密钥使用 `ForceCommand` 启动固定网关。
- 禁止 PTY、Agent、X11、TCP、Socket 和隧道转发。
- 网关程序和配置由 root 所有，普通账号不可写。

### Windows 建议

- 使用专用本地或域账号，不加入 Administrators。
- 只授予业务需要的读取权限；需要事件日志时评估最小的 Event Log Readers 等权限。
- 使用 NTFS ACL 拒绝业务目录、配置目录和系统位置的写入。
- 诊断账号通过 `ForceCommand` 进入签名、版本化的 `ramag-diagnostic-gateway.exe`。
- 禁止 PTY 和转发；实际使用的 `sshd_config` 指令必须在目标 Windows OpenSSH 版本上通过配置检查。
- 网关二进制和配置由 Administrators/SYSTEM 管理，诊断账号只有读取和执行权限。
- Windows 的 `ChrootDirectory` 只适用于 SFTP，不能用来隔离 `cmd.exe` 或 PowerShell 诊断会话。

### 诊断与 SFTP 凭据关系

同一账号使用 `ForceCommand` 启动诊断网关后，通常会拦截该账号的 SFTP 子系统请求。高安全部署推荐：

1. 诊断使用独立账号或独立密钥，只能进入安全网关。
2. SFTP 使用另一个由文件权限/ACL 限制为只读的账号。
3. Ramag 后续支持为诊断通道和 SFTP 通道配置不同凭据；在该能力实现前，可以用两个明确命名的 SSH 配置管理。

Windows SFTP-only 账号可以结合 `ForceCommand internal-sftp` 和 SFTP 专用 chroot；实际目录访问仍以 NTFS ACL 为最终权限边界。

## 分层改造范围

### `ramag-domain`

- 新增远端平台偏好、探测结果和能力模型。
- 新增 `RemotePath` 与命名空间策略。
- 定义诊断操作、参数值对象、结果、取消和资源预算。
- 把当前只接受 POSIX 初始路径的校验拆为通用预校验与连接后平台校验。
- 不依赖 GPUI、PTY、PowerShell 或具体 OpenSSH 实现。

### `ramag-app`

- 所有终端和诊断操作根据 `profile_id` 读取最新配置。
- 生产配置拒绝 `terminal_command`。
- 分别编排 SSH、Terminal、SFTP 和诊断能力测试。
- 根据远端能力校验 SFTP 路径和诊断操作。
- SFTP 生产写门禁继续作为业务规则中心。
- 建议拆分 `ssh_service/terminal.rs`、`diagnostic.rs` 和 `remote_path.rs`，避免主用例文件继续增长。

### `ramag-infra-ssh`

- 生产配置启动完整终端时再次拒绝。
- 增加固定平台探测和会话能力缓存。
- 把 POSIX 路径字符串处理替换为 `RemotePathPolicy`。
- 增加 Linux、Windows 两个诊断提供者，共享有界进程执行器。
- 安全诊断使用 `ssh -T`，不分配交互 PTY，不提供用户 stdin；内部 stdin 只发送有界结构化请求。
- stdout、stderr、超时、取消和子进程回收实施硬限制。
- Windows 写入提交策略与 POSIX 策略分离。

### `ramag-tool-ssh`

- 连接状态分别显示 Terminal、SFTP 和安全诊断能力。
- 面包屑、路径输入、收藏和工作区恢复使用 `RemotePath`。
- Windows 路径显示盘符或服务器虚拟根，不强制 `/`。
- 生产工作区渲染结构化安全诊断终端，不创建普通 `TerminalView`。
- 生产切换时冻结、确认并关闭全部已有终端。
- 不在 UI 中复制安全判断；UI 只消费应用层能力和拒绝原因。

### `ramag-terminal`

- 保持通用 PTY、ANSI 和原始终端职责。
- 不依赖 SSH 平台或生产诊断领域模型。
- 如复用结果渲染，只增加不可输入的展示模式；安全诊断不能通过 `TerminalCore::send` 发送命令。

### `ramag-infra-storage`

- `RemotePlatformPreference` 使用 `serde(default)`，旧配置迁移为 `Auto`。
- 会话探测结果不直接持久化，避免 SSH 别名换目标后复用旧平台。
- 旧工作区 POSIX 路径保留，但连接后必须按当前命名空间重新验证。
- 不可验证的收藏或恢复路径不自动转换、不自动访问，UI 提示用户确认。

## 参数、命令和输出安全

### 通用输入规则

- 拒绝 NUL、换行和控制字符。
- 数字参数使用类型和闭区间，不接受自由文本数字表达式。
- 服务名必须精确匹配受限字符集，不允许通配符。
- 日志源使用封闭枚举，不接受任意文件 glob、XPath、XML 或 PowerShell 表达式。
- 路径必须属于已识别的 SFTP 命名空间。
- 文件读取只允许普通文件并限制单次窗口。
- 不允许用户覆盖 PATH、PAGER、EDITOR、SHELL、PowerShell profile 等执行环境。
- 不继承可能改变固定程序行为的非必要远端环境变量。

### Shell 构造规则

理想路径是服务端网关接收结构化请求，不经过通用 Shell。客户端固定模板必须满足：

- 程序、选项、脚本和 API 由代码固定。
- 用户参数与命令模板完全分离。
- 能通过 stdin 传递结构化参数时不拼入命令。
- 无法避免命令参数时，必须使用平台专用编码并有注入测试。
- 不支持用户管道、重定向、命令替换、控制运算符和环境展开。
- 无法证明安全的操作不实现，不能回退任意 `sh -c`、`cmd /c` 或 PowerShell 文本。

### 输出规则

- Windows 优先输出 UTF-8 JSON，Linux 在可行时也归一化为结构化字段。
- JSON 深度、数组项目数、单字段长度和总字节数均有限制。
- 非法 UTF-8、非法 JSON、超深对象或重复超大字段返回协议错误。
- UI 不直接渲染远端 ANSI、OSC 链接或其他控制序列。
- 保存诊断结果只写本地文件，并沿用本地覆盖确认。

## 生产模式状态切换

### 开启生产模式

1. UI 冻结该配置下所有 `TerminalView` 输入。
2. 统计活动终端并显示将关闭的数量和影响。
3. 用户取消时恢复输入，不保存配置。
4. 用户确认时关闭全部 PTY 和 SSH 子进程。
5. 递增 terminal generation，使并发中的旧启动结果失效。
6. 终端关闭完成后保存生产配置。
7. 重新探测平台和安全诊断能力。
8. 工作区切换为安全诊断终端；能力不支持时显示只读 SFTP 或不可用原因。

保存和关闭之间必须避免旧终端重新加入工作区的竞态。不能只依赖按钮禁用，应用服务启动终端时仍要读取最新配置。

### 关闭生产模式

1. 显示主机、用户、远端平台和保护降级说明。
2. 要求输入连接名称确认。
3. 保存非生产配置。
4. 只允许之后新建完整终端，不恢复旧进程。
5. 记录降级事件，不记录凭据或完整远端内容。

## 错误处理与审计

建议使用稳定错误代码，UI 再映射为中文说明：

- `ProductionTerminalForbidden`
- `RemotePlatformUnknown`
- `RemotePlatformMismatch`
- `SftpUnavailable`
- `SftpNamespaceUnsupported`
- `RemotePathInvalid`
- `DiagnosticProviderUnavailable`
- `DiagnosticOperationUnsupported`
- `DiagnosticPolicyDenied`
- `DiagnosticTimeout`
- `DiagnosticOutputLimitExceeded`
- `DiagnosticProtocolInvalid`
- `GatewayVersionMismatch`

建议记录：

- 时间、`profile_id`、远端平台和操作类型。
- 允许、拒绝、成功、失败、超时或取消。
- 耗时、退出码、输出字节数和截断状态。
- 稳定错误码和诊断提供者版本。

默认不记录：

- 密码、密钥、Token 和 AskPass 内容。
- 文件、事件日志和命令输出正文。
- 完整敏感路径、进程命令行和环境变量。
- PowerShell stdin 请求原文。

应用日志使用简洁英文，UI 错误使用中文。

## 测试方案

### 远端路径纯逻辑测试

Linux 用例：

- `/`、`/var/log`、尾随 `/`、`.`。
- 空组件、`.`、`..`、NUL、换行、控制字符和超长 UTF-8。
- 根目录删除保护、临时兄弟路径和面包屑。

Windows 用例：

- `C:/`、`C:/Users/Admin`、`D:/Data`。
- 服务器返回虚拟 `/` 的 Windows SFTP。
- 盘符大小写、空格、中文和长路径。
- `C:relative`、UNC、设备路径、ADS、保留名称、末尾点和空格。
- 盘符根和虚拟根删除保护。
- reparse point、文件占用和路径在操作前发生变化。

### 领域与应用层测试

- 每种诊断操作分类准确，未知操作默认拒绝。
- 生产配置调用完整终端返回 `Forbidden`。
- 被拒绝操作不会调用 `SshDriver`。
- 允许操作使用最新持久化配置，不使用旧工作区副本。
- 平台偏好与探测结果冲突时失败关闭。
- 配置读取失败、配置删除和生产状态变化全部失败关闭。
- 非生产终端行为不回归。
- 两个平台的生产 SFTP 读操作允许、远端写操作全部拒绝。

### 基础设施测试

- 生产配置不能构造 `ssh -tt`。
- SSH、Terminal、SFTP 和诊断探测结果可以分别失败。
- SFTP 路径策略不会把 Windows 盘符当成 POSIX 相对路径。
- Windows 固定 PowerShell 启动器不包含用户输入。
- stdin JSON 不能注入额外 cmdlet、脚本或参数。
- Linux 固定模板不能被服务名等参数注入额外选项或 Shell 语法。
- 安全诊断不分配 PTY，不开放用户输入。
- stdout、stderr、超时、取消和子进程回收有界。
- 输出达到限制时明确终止并标记。
- 不支持的平台、命名空间和网关版本返回可定位错误。
- Windows SFTP 覆盖失败不会静默丢失原文件或备份；ACL 不可保留时拒绝操作。

### UI 测试

- 分别显示 Terminal、SFTP、诊断能力状态和失败原因。
- Windows 盘符路径和虚拟根面包屑正确。
- 生产工作区不创建普通 `TerminalView`。
- 生产标识、操作列表和资源边界持续可见。
- 参数非法时执行按钮禁用并说明原因。
- 粘贴只填写字段，不自动执行。
- 不存在单次绕过、临时允许或任意命令入口。
- 开启生产模式会冻结并关闭已有终端。
- 用户取消切换时恢复输入且配置不变。
- 关闭生产模式需要显式降级确认。

### 真实远端测试矩阵

| 维度 | Linux | Windows |
|---|---|---|
| 服务器 | Ubuntu 22.04/24.04、Debian 12、Rocky 9 | Server 2019/2022/2025 |
| OpenSSH | 发行版系统版本 | Microsoft inbox OpenSSH |
| Shell | Bash、Zsh | cmd、Windows PowerShell 5.1、PowerShell 7 |
| 认证 | 密码、公钥、Agent/配置 | 本地账号、域账号、密码、公钥 |
| SFTP 根 | 默认目录、POSIX/chroot `/` | C 盘、非 C 盘、虚拟根/SFTP chroot |
| 路径 | ASCII、空格、中文、长路径 | ASCII、空格、中文、盘符、合法特殊字符 |
| 诊断 | systemd 与能力缺失场景 | 普通账号、最小只读账号、权限不足场景 |

建议把真实测试环境变量拆分为平台前缀，例如：

```text
RAMAG_TEST_SSH_LINUX_HOST
RAMAG_TEST_SSH_LINUX_ROOT
RAMAG_TEST_SSH_WINDOWS_HOST
RAMAG_TEST_SSH_WINDOWS_ROOT
```

密钥和密码只通过环境变量或 CI 密钥存储注入，不能提交到仓库。非生产 SFTP 写测试必须使用名称含 `ramag` 的专用目录，并通过平台路径模型确认不是 `/`、盘符根或虚拟根后才允许清理。

### 生产安全回归

- 验证所有拒绝都发生在远端任务创建前。
- 验证生产 UI、应用服务和基础设施三层都无法启动完整 Shell。
- 验证超时、取消后没有本地 SSH 子进程残留。
- 验证固定诊断不会创建后台任务。
- 验证文件内容、目标服务状态和目标配置在诊断前后没有被主动修改。
- 不使用“系统完全零变化”作为断言，因为 SSH 登录、进程创建和读取本身会产生审计日志、缓存和少量资源消耗。
- 服务端网关模式验证同一凭据从其他 SSH 客户端也无法获得任意 Shell。

交付前使用仓库封装执行：

```text
make test
make clippy
make fmt-check
```

## 验收标准

### 通用安全验收

1. 生产配置无法通过任何 UI、应用接口或基础设施接口启动完整 SSH Shell。
2. 安全诊断只接受封闭结构化操作，不接受任意程序、参数数组或命令字符串。
3. 平台未知、路径命名空间未知、参数非法和操作未实现时失败关闭。
4. 允许操作具有并发、超时、输出和扫描范围上限。
5. 生产 SFTP 所有远端写操作在应用层和基础设施层均被拒绝。
6. 安全诊断失败不会回退到普通 Terminal。
7. UI 和文档不把客户端保护宣传为服务器绝对只读。

### Linux 正式支持验收

1. 普通 Terminal、SFTP 读写和生产只读 SFTP 通过 Linux 测试矩阵。
2. 每项 Linux 安全诊断在支持发行版上返回标准化结果。
3. 工具缺失或版本不支持时只禁用对应能力，不执行未经验证的替代命令。
4. systemd 与非 systemd 差异有明确能力状态。

### Windows 正式支持验收

1. Windows Server 2019/2022/2025 的普通 Terminal 通过 cmd 与 PowerShell 测试。
2. SFTP 正确处理当前盘符、非 C 盘和服务器虚拟根，不误判根目录。
3. 浏览、预览、下载和允许的非生产写操作逐项通过真实 Windows 集成测试。
4. ACL 无法正确保留的编辑或覆盖操作保持禁用，并向用户明确说明。
5. Windows 诊断提供者只输出允许字段，拒绝任意 PowerShell 和动态查询。
6. 诊断在普通账号、最小只读账号和权限不足场景中均有明确结果。
7. 默认 `cmd.exe` 和配置 `DefaultShell` 后都能正确识别能力，不能依赖交互 Shell 类型碰运气。

只有以上 Windows 验收全部满足，才能对外声明“支持远端 Windows Server 的 SSH + SFTP + 生产安全诊断”。在此之前应按能力标记“实验性”或“暂不支持”。

## 分阶段落地

### P0：建立所有平台通用的生产硬边界

- 修改生产模式语义和 UI 文案。
- 应用层、基础设施层拒绝生产完整终端。
- 开启生产模式时冻结并关闭已有终端。
- 更新现有“生产模式允许 Terminal”的回归测试。
- 安全诊断尚未实现时显示能力说明和暂不可用状态。

P0 完成后，Ramag 才能保证生产模式不再通过普通终端发送新命令。该阶段与远端是 Linux 还是 Windows 无关，应优先完成。

### P1：跨平台基础与双平台客户端能力

#### P1.1 平台和路径基础

- 增加平台偏好、能力探测和会话缓存。
- 拆分 SSH、Terminal 与 SFTP 连接测试。
- 引入 `RemotePath` / `RemotePathPolicy`。
- 迁移路径输入、收藏、面包屑、删除保护、临时文件和目录归档逻辑。

#### P1.2 Linux 回归与诊断

- 保证现有 Linux SFTP 和普通终端不回归。
- 实现 Linux 高频结构化诊断。
- 完成 Linux 远端测试矩阵。

#### P1.3 Windows SSH 与只读 SFTP

- 认证 Windows 普通 Terminal。
- 支持 Windows SFTP 浏览、预览和下载。
- 支持盘符和服务器虚拟根。
- 完成 Windows 只读路径、元数据和 reparse point 测试。

#### P1.4 Windows 非生产 SFTP 写能力

- 开放已经验证的新建、上传、重命名和删除。
- 完成文件占用、回滚和根目录保护测试。
- ACL 保留方案完成前不开放编辑/覆盖现有文件。

#### P1.5 Windows 生产诊断

- 实现固定 PowerShell 诊断提供者和 JSON 协议。
- 完成系统、资源、进程、网络、磁盘、日志和服务状态操作。
- 完成注入、编码、权限、超时和输出边界测试。

每个子阶段独立发布能力，不等待“Windows 全部完成”才暴露已经安全通过验收的只读功能。

### P2：服务器端强制保护

- 设计版本化 Linux/Windows 安全网关协议。
- 使用独立账号或凭据、`ForceCommand`、禁止 PTY 和转发。
- Windows 网关使用最小 ACL 和可回收进程树。
- 增加服务器资源限制和集中审计。
- 设计诊断与 SFTP 的双凭据配置方式。
- 验证同一受限凭据从 Ramag 以外客户端也无法获得任意 Shell。

### P3：按真实场景扩展

- 根据实际排障频率增加新的结构化操作。
- 每项操作单独评估数据、状态、主动探测、敏感信息和资源风险。
- 评估 Windows ACL 展示、受控文件替换和更多 SFTP Server 兼容。
- 评估 macOS、BSD 或其他远端平台。
- 只有出现明确合规需求时再建设专用审计存储。

## 风险与取舍

| 项目 | 好处 | 代价或风险 |
|---|---|---|
| 双平台统一能力模型 | 避免 UI 和业务层到处判断系统 | 初期领域模型和测试量增加 |
| 远端路径值对象 | 从根本上解决盘符、根目录和安全校验 | 需要迁移现有多处字符串路径逻辑 |
| Windows 固定 PowerShell 提供者 | 无需首版部署服务端程序 | 只属于客户端防误操作，依赖 PowerShell 可用性 |
| 服务端网关 | 安全边界更强、执行和审计稳定 | 部署、升级、凭据和运维成本更高 |
| Windows SFTP 分级开放 | 不会因 ACL 或文件占用造成静默数据问题 | 初期 Windows 写功能不完整 |
| 本地过滤与有界读取 | 降低远端负载，结果可测试 | 无法覆盖所有临时排障需求 |

最大的产品风险不是“不支持某个命令”，而是把“能连接”误写成“安全且完整支持”。能力级状态和独立验收必须贯穿 UI、文档和发布说明。

## 已采用的默认决策

除非后续明确调整，实施按以下默认值推进：

1. 远端 Windows 首批正式范围为 Windows Server 2019/2022/2025 + Microsoft OpenSSH。
2. 架构同时支持 Linux 和 Windows，能力按测试门禁分阶段开放。
3. 所有平台的生产连接都禁止完整交互 Shell。
4. 生产 SFTP 允许有界浏览、预览和下载，禁止全部远端写操作。
5. Windows 首版拒绝 UNC、设备路径、ADS 和未知 reparse point。
6. Windows ACL 无法可靠保留时，编辑和覆盖现有文件保持禁用。
7. Windows 客户端诊断基线使用 Windows PowerShell 5.1 的固定启动器，不依赖服务器默认 Shell。
8. 高安全承诺必须部署独立账号和服务端网关；客户端模式只承诺防止通过 Ramag 误操作。

## 实施前检查

开始 P0 前确认：

- 现有生产配置行为变更已经接受。
- UI 可以枚举、冻结和关闭一个配置下的全部终端。
- 完整终端门禁错误文案已经确定。

开始 P1.1 前确认：

- `RemotePath` 的序列化边界和工作区迁移方案已评审。
- Windows 正式支持基线和首版不支持范围已接受。
- SSH、Terminal、SFTP、诊断能力状态的 UI 表达已确定。

开始 Linux 或 Windows 诊断前确认：

- 第一版操作清单和资源预算已经确定。
- 文件、事件日志和进程信息的敏感字段策略已经确定。
- 是否要求服务端不可绕过保护已经决定；未部署网关时明确客户端边界。

开始 Windows 非生产写能力前确认：

- 专用 Windows SFTP 集成环境和清理保护可用。
- 文件占用、失败回滚、盘符根保护和 reparse point 场景已经覆盖。
- 编辑/覆盖现有文件的 NTFS ACL 语义已经明确；未明确时保持关闭。

## 官方依据

- [Microsoft Learn：Get started with OpenSSH Server for Windows](https://learn.microsoft.com/en-us/windows-server/administration/openssh/openssh_install_firstuse)
- [Microsoft Learn：OpenSSH Server configuration for Windows](https://learn.microsoft.com/en-us/windows-server/administration/openssh/openssh-server-configuration)
- [OpenBSD：sshd_config](https://man.openbsd.org/sshd_config)
- [Microsoft Learn：Get-Process](https://learn.microsoft.com/en-us/powershell/module/microsoft.powershell.management/get-process)
- [Microsoft Learn：Get-Service](https://learn.microsoft.com/en-us/powershell/module/microsoft.powershell.management/get-service)
- [Microsoft Learn：Get-NetTCPConnection](https://learn.microsoft.com/en-us/powershell/module/nettcpip/get-nettcpconnection)
- [Microsoft Learn：Get-Volume](https://learn.microsoft.com/en-us/powershell/module/storage/get-volume)
- [Microsoft Learn：Get-WinEvent](https://learn.microsoft.com/en-us/powershell/module/microsoft.powershell.diagnostics/get-winevent)

微软当前文档还明确说明：Windows OpenSSH 初始默认 Shell 是 `cmd.exe`，可以通过 `HKLM\SOFTWARE\OpenSSH\DefaultShell` 修改；`ChrootDirectory` 在 Windows 上只支持 SFTP 会话，`cmd.exe` 远程会话不会受其限制。这些差异是 Windows 诊断必须显式启动固定提供者、并在高安全场景使用独立账号和网关的直接原因。
