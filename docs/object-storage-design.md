# Ramag 云对象存储工具详细设计

## 文档状态

- 状态：已实现并通过全工作区质量门禁
- 核对日期：2026-08-11
- 用户可见名称：云存储
- 代码命名：`object_storage`
- 首次交付服务商：腾讯云 COS、阿里云 OSS
- 唯一认证方式：永久访问密钥（AK/SK）
- 核心依赖：Apache OpenDAL

本文记录云存储工具的产品边界、认证方式、数据模型、界面结构、分层架构、安全要求、测试方案和完整验收标准。本文不按阶段拆分；文中列出的“交付范围”必须作为一次完整功能实现并通过验收。

## 结论先行

云存储工具采用“显式 Bucket 挂载 + Bucket 内统一对象访问”的架构：

```text
永久 AK/SK + Bucket + Region
    │
    └── 官方 Endpoint + Apache OpenDAL Operator
          ├── 列举对象
          ├── 查看元数据
          ├── 上传、下载
          └── 单对象删除
```

确定的关键决策如下：

1. 使用 Apache OpenDAL 处理 Bucket 内的数据访问，不分别接入两套非官方 Rust SDK。
2. 不请求账号级列桶 API；用户必须显式配置至少一个 Bucket 和 Region。
3. 保存账号时通过已配置 Bucket 的对象列举接口验证凭据与访问权限。
4. 持久化模型从一开始就分离“云账号”和“Bucket”，不能把一个账号复制成多个 Bucket 连接。
5. 唯一支持永久 AK/SK；除非以后重新作出明确产品决策，否则不支持其他认证方式，也不为其他方式提前设计入口。
6. 生产模式默认关闭；用户开启后实施只读保护，删除和覆盖始终必须再次确认。
7. 界面复用数据库工具的会话骨架和 SSH 工具的文件操作习惯，但对象模型保持独立。
8. 只操作 OpenDAL 能无损表示的安全 Key；其他远端 Key 仍显示，但禁用下载、覆盖和删除。
9. Endpoint 只允许程序根据服务商和 Region 生成的官方 HTTPS 地址，不提供自定义 Endpoint 输入。
10. 同一 Bucket 可通过不同 Root Prefix 建立多个独立挂载点。
11. 上传和下载默认拒绝覆盖；目标已存在时先返回 Conflict，UI 只有在用户再次确认并看到竞态提示后才执行覆盖。
12. OpenDAL 数据面关闭默认配置加载；OSS 请求通过自定义 Reqwest transport 显式改签 V4，地域来自已校验的官方挂载点。

## 背景

Ramag 已有数据库、Redis、MongoDB、Git、剪贴板和 SSH/SFTP 工具。新增云存储工具的目标不是再做一个 SFTP 文件浏览器，而是提供统一的对象存储入口，让用户用同一套交互查看和管理 COS、OSS。

对象存储与文件系统存在本质差异：

- Bucket 是账号下的顶层容器，不是磁盘。
- Object Key 是字符串键，不是真实文件路径。
- “目录”通常只是共享前缀，没有独立目录实体。
- 列举是远程、分页且可能计费的 API 请求。
- 重命名通常等价于复制后删除，不是原子操作。
- 归档对象可能无法立即读取。
- 权限由账号策略、Bucket 策略和对象权限共同决定。

因此，界面可以统一，领域模型和安全语义不能复用 SFTP。

## 设计目标

### 功能目标

1. 管理多个 COS、OSS 云账号配置。
2. 新建或编辑账号时必须配置至少一个已知 Bucket 和 Region。
3. 支持已配置 Bucket 按地域分组、搜索和刷新。
4. 同一账号可配置多个 Bucket，也可对同一 Bucket 配置不同 Root Prefix。
5. 支持按前缀浏览对象和虚拟目录。
6. 支持查看对象元数据。
7. 支持流式上传、下载、取消和进度展示。
8. 支持经过确认的单对象删除。
9. 支持生产模式，并在 UI、应用层、基础设施层实施只读写入门禁。
10. COS、OSS 的 Bucket 内操作通过同一个领域接口完成。

### 质量目标

1. 密钥不得明文落盘、进入日志或错误诊断信息。
2. 大量 Bucket、对象和大文件不能阻塞 GPUI 线程或造成无界内存增长。
3. 所有外部输入都必须有长度、格式和范围校验。
4. 网络错误、鉴权失败、权限不足和厂商错误必须能区分并定位。
5. 幂等读取允许有界重试，写入和删除不能进行可能产生重复副作用的应用层盲目重试。
6. 核心逻辑、边界条件和持久化加密必须有自动化测试。

## 交付范围

### 包含

- COS、OSS 账号的新建、编辑、删除和连接测试。
- 永久 AK/SK 认证。
- 必填 Bucket、Region 与可选 Root Prefix 配置。
- 已配置 Bucket 的搜索、地域分组和刷新。
- Bucket 内前缀浏览和分页加载。
- 对象名称、大小、修改时间、ETag、内容类型、存储类型等可用元数据。
- 文件上传、下载、覆盖策略、进度和取消。
- 单对象删除及确认。
- 生产模式（只读保护）。
- 连接会话和工作区恢复。
- 亮色、暗色主题和响应式布局。

### 不包含

- STS 临时凭据、RAM/CAM 角色、OIDC、ECS/CVM 实例角色和本地 CLI 凭据链。
- 腾讯云账号、阿里云账号的网页登录或扫码登录。
- 账号密码采集和存储。
- 授权码、共享链接、预签名 URL 登录。
- 匿名、公有读 Bucket 登录。
- 创建或删除 Bucket。
- Bucket ACL、生命周期、版本控制、跨区域复制、CDN 和图片处理配置。
- 批量删除。
- 对象重命名、移动和跨 Bucket、跨云复制。
- 全 Bucket 模糊搜索或内容搜索。
- 归档对象恢复。
- 上传、下载断点续传承诺。
- 将云存储挂载为本地文件系统。

这些能力不属于产品范围。除非以后重新作出明确产品决策并同步修改本文，否则不支持其他认证方式，也不为它们提前增加领域枚举、表单入口、凭据加载器或基础设施适配器。

## 术语与产品语义

### 云账号

一组服务商、永久 AK/SK 和至少一个 Bucket 挂载的持久化配置。

### Bucket

COS 或 OSS 的存储空间。Bucket 名称和 Region 由用户显式配置。

### Object

Bucket 内由 Key 唯一标识的数据对象。UI 可以显示为文件，但领域层统一称为对象。

### Prefix

Object Key 的前缀。以 `/` 分隔后可模拟目录层级，但不是必须存在的真实目录。

### 工作区

一个已打开的云账号会话，包含选中的 Bucket、当前 Prefix、选择项、加载游标和传输队列显示状态。

### 为什么不自动列出 Bucket

账号级列桶权限与 Bucket 数据访问权限彼此独立。子账号或跨账号授权常常只能访问指定 Bucket/Prefix；强制请求 `ListBuckets` 会制造无意义的权限错误，也无法保证发现全部可访问资源。因此 Ramag 只使用用户明确配置的 Bucket，不申请或依赖账号级列桶权限。

## 认证方式

### 唯一支持：永久 AK/SK

统一 UI 名称为“访问密钥（AK/SK）”，根据服务商显示准确字段名：

| 服务商 | 标识字段 | 密钥字段 |
|---|---|---|
| 腾讯云 COS | `SecretId` | `SecretKey` |
| 阿里云 OSS | `AccessKey ID` | `AccessKey Secret` |

只允许通过 HTTPS 向官方端点发送签名请求。密钥只在内存、加密持久化记录和请求签名过程中使用。

此限制只针对登录和请求认证方式，不把云存储工具限制为只读查看器。账号是否允许上传和删除，仍由账号权限及 Ramag 的只读开关共同决定。

实现必须使用显式静态凭据：

- 创建 OpenDAL Operator 时始终传入当前账号保存的 AK/SK。
- 不读取环境变量、用户目录中的云 CLI Profile、ECS/CVM 元数据或其他默认凭据链。
- OpenDAL 服务支持关闭默认配置加载时必须显式关闭。
- AK/SK 为空或不完整时本地报错，不能静默回退到机器上的其他身份。

安全建议：

- 优先使用 CAM 子账号或 RAM 用户的 AK。
- 不建议使用主账号 AK。
- 权限按最小权限配置。
- 定期轮换密钥；保存新密钥后立即失效该账号的 Operator 和游标缓存。

### 明确不支持的方式

| 方式 | 凭据形态 | 产品状态 |
|---|---|---|
| STS 临时凭据 | AK、SK、Security Token | 不支持 |
| RAM/CAM 角色 | 自动换取临时凭据 | 不支持 |
| ECS/CVM 实例角色 | 元数据服务临时凭据 | 不支持 |
| OIDC 工作负载身份 | OIDC Token 换取临时凭据 | 不支持 |
| 环境变量或 CLI Profile | 本地凭据提供链 | 不支持 |
| 云账号网页登录 | 浏览器授权会话 | 不支持 |
| 授权码、共享链接 | 限定资源的临时授权 | 不支持 |
| 预签名 URL | 单次对象请求 URL | 不支持 |
| 匿名访问 | 无凭据 | 不支持 |

Ramag 不为以上方式显示禁用占位选项，也不实现自动探测或隐式降级。Ramag 永远不能要求用户输入并保存云账号密码。

## 权限要求

### Bucket 内操作

身份还需要与功能相匹配的对象列举、读取、写入或删除权限。Ramag 不应要求管理员直接授予服务商预置的全量管理权限，而应在文档和错误提示中列出缺失的具体 Action。

### 权限失败行为

连接测试必须区分以下情况：

1. AK/SK 格式无效：本地拒绝，不发送请求。
2. 签名或身份无效：显示“访问密钥无效或签名失败”。
3. 已配置 Bucket 无列举权限：阻止保存并显示该 Bucket 的访问错误。
4. 已配置 Bucket 可列举但结果为空：连接成功，显示空状态。
5. 网络、DNS、TLS、超时、限流和服务端错误：分别映射，不误报为密钥错误。

保存规则与验证结果保持分离：无效凭据或已配置 Bucket 的权限错误阻止保存；网络、DNS、TLS、超时、限流或服务端临时错误允许保存并标记为“未验证”，由用户稍后重试。

## Bucket 配置与验证

- 每个账号至少配置一个 Bucket 和 Region，Root Prefix 可选。
- Endpoint 不接受用户输入；COS 生成 `https://cos.{region}.myqcloud.com`，OSS 生成 `https://oss-{region}.aliyuncs.com`，并再次通过官方域名白名单校验。
- 保存时对每个挂载执行根 Prefix 的对象列举，以验证签名、Bucket、Region 和最小读取权限。
- 打开工作区只读取加密保存的挂载配置，不调用账号级 Bucket API。
- 挂载结果按“Bucket、Region、Root Prefix”稳定排序；同一 Bucket 可通过不同 Root Prefix 独立挂载。

## 数据模型

### 账号

```rust
pub struct ObjectStorageAccount {
    pub schema_version: u16,
    pub id: ObjectStorageAccountId,
    pub revision: u64,
    pub name: String,
    pub provider: CloudProvider,
    pub access_key_id: SecretString,
    pub access_key_secret: SecretString,
    pub read_only: bool,
    pub manual_buckets: Vec<ManualBucket>,
}

pub enum CloudProvider {
    TencentCos,
    AliyunOss,
}
```

不增加未使用的 STS、Role 等枚举变体。`schema_version` 只用于账号记录自身的兼容迁移，不代表预留其他认证方式。

### 配置 Bucket

```rust
pub struct ManualBucket {
    pub id: ObjectStorageMountId,
    pub name: String,
    pub region: String,
    pub root_prefix: Option<String>,
}
```

`manual_buckets` 是已落库字段的兼容名称，产品语义为账号必填的 Bucket 挂载。它作为账号配置的一部分整体加密，不接受 Endpoint；程序根据服务商和 Region 生成严格校验过的官方地址。

### 对象条目

```rust
pub struct ObjectEntry {
    pub key: String,
    pub display_name: String,
    pub kind: ObjectEntryKind,
    pub size: Option<u64>,
    pub last_modified: Option<DateTime<Utc>>,
    pub etag: Option<String>,
    pub content_type: Option<String>,
    pub storage_class: Option<String>,
}

pub enum ObjectEntryKind {
    Prefix,
    Object,
}
```

领域对象不得直接暴露 OpenDAL 的 `Entry`、`Metadata`、`Operator` 或错误类型。

### 工作区偏好

持久化内容包括：

- 已打开的账号 ID。
- 当前账号。
- 每个账号最后选中的 Bucket。
- 每个挂载点的收藏 Prefix。
- 窄窗口下 Bucket 导航的显示状态。

浏览 Prefix、分栏宽度和详情面板均为窗口内临时状态，不持久化；重新进入账号时从最后选中的 Bucket 根目录开始。

Bucket 名称和 Prefix 可能包含业务信息，必须使用 `Storage::seal` 加密后再写偏好 KV，不能明文保存。

## 输入校验与资源上限

首次实现采用以下上限，实际常量定义在 Domain：

| 项目 | 上限 |
|---|---:|
| 云账号数量 | 64 |
| 账号名称 | 128 bytes |
| AccessKey ID / SecretId | 256 bytes |
| AccessKey Secret / SecretKey | 512 bytes |
| Bucket 挂载数量/账号 | 128 |
| Bucket 名称 | 领域绝对上限 255 bytes；同时应用服务商规则，OSS 最多 63 bytes |
| Region | 128 bytes |
| Endpoint | 2 KiB |
| Object Key / Prefix | 4 KiB |
| 单页对象条目 | 500 |
| 工作区累计对象条目 | 20,000 |
| 并发传输 | 3 |
| 等待中传输 | 64 |
| 传输历史 | 100 |

校验规则：

- 所有输入按 UTF-8 字节长度检查。
- Bucket、Region 按服务商规则校验，错误说明具体字段。
- Endpoint 不作为用户输入；仅接受程序根据服务商和 Region 生成、且通过官方域名白名单校验的 HTTPS 地址。
- Object Key 是字符串而非文件路径，但 OpenDAL 会规范化路径。为避免操作错对象，只允许无首尾空白、无前导/尾随 `/`、无 `//`、无 `.`/`..` 独立分段、无控制字符和双向文本控制符的安全 Key。
- 不满足安全规则的远端 Key 仍可见并标记“仅查看”，不得传给 OpenDAL 执行读写删。
- Root Prefix 规范化为相对 Key，不能以协议或 Bucket 名开头。
- 服务端返回数据也必须执行长度和集合上限校验。

## 分层架构

### Crate 关系

```text
ramag-bin
  ├── ramag-tool-object-storage
  ├── ramag-infra-object-storage
  ├── ramag-infra-storage
  └── ramag-app
        └── ramag-domain
```

新增内容：

| Crate | 职责 |
|---|---|
| `ramag-domain` | 账号、Bucket、对象、传输实体及 Driver trait |
| `ramag-app` | `ObjectStorageService`，编排挂载验证、权限、传输和持久化 |
| `ramag-infra-object-storage` | OpenDAL COS/OSS 数据面、HTTP 传输、Runtime |
| `ramag-infra-storage` | 加密保存云账号和 Bucket 挂载 |
| `ramag-tool-object-storage` | GPUI 连接管理、Bucket 导航、对象列表、详情和传输 UI |
| `ramag-bin` | 依赖注入、工具注册、视图注册和快捷键 |

云存储不得依赖 `ramag-tool-ssh`、`ramag-infra-ssh` 或数据库工具。可复用的视觉原语只能下沉到 `ramag-ui`。

### Domain trait

领域层只暴露 Bucket 数据面的 `ObjectStorageDriver`：

```rust
#[async_trait]
pub trait ObjectStorageDriver: Send + Sync {
    async fn capabilities(
        &self,
        account: &ObjectStorageAccountSnapshot,
        mount: &ObjectStorageMount,
    ) -> ObjectStorageResult<ObjectCapabilities>;

    async fn list_page(
        &self,
        account: &ObjectStorageAccountSnapshot,
        mount: &ObjectStorageMount,
        query: &ObjectListQuery,
        cursor: Option<&ObjectListCursor>,
        request_generation: u64,
    ) -> ObjectStorageResult<ObjectPage>;

    async fn stat(
        &self,
        account: &ObjectStorageAccountSnapshot,
        mount: &ObjectStorageMount,
        key: &str,
    ) -> ObjectStorageResult<ObjectMetadata>;

    async fn upload(&self, request: ObjectUploadRequest) -> ObjectStorageResult<()>;
    async fn download(&self, request: ObjectDownloadRequest) -> ObjectStorageResult<()>;
    async fn delete(
        &self,
        account: &ObjectStorageAccountSnapshot,
        mount: &ObjectStorageMount,
        key: &str,
    ) -> ObjectStorageResult<()>;
    async fn invalidate_account(
        &self,
        account_id: &ObjectStorageAccountId,
        minimum_revision: u64,
    ) -> ObjectStorageResult<()>;
    async fn shutdown(&self) -> ObjectStorageResult<()>;
}
```

最终签名可按实现细化，但必须保持以下边界：

- Domain 不依赖 OpenDAL、HTTP 或 XML。
- UI 不直接调用 Driver。
- 写入门禁在 App 和 Infra 两层同时存在。
- 分页游标是 Ramag 自己的不可解析类型，不暴露厂商 Token。

### App Service

`ObjectStorageService` 负责：

- 账号 CRUD 和校验。
- 必填 Bucket 挂载转换和保存前访问验证。
- 工作区偏好加密和恢复。
- 全局加密保存已打开账号和当前账号；启动时过滤已删除账号后恢复会话。
- 当前 Prefix、分页和选择状态的业务规则。
- 只读门禁。
- 上传、下载任务队列、取消和历史。
- 保存、删除账号时失效 Operator、游标和传输。

### Infra

`ramag-infra-object-storage` 建议按职责拆分：

```text
src/
  lib.rs
  runtime.rs
  errors.rs
  operator_cache.rs
  cursor_store.rs
  objects/
    mod.rs
    list.rs
    preview.rs
    transfer.rs
```

单文件尽量控制在 300 行以内，超过 600 行必须拆分。

## OpenDAL 接入

### 定位

OpenDAL 是 Apache 开源的 Rust 数据访问层，不是腾讯云或阿里云官方 SDK。本项目把它作为 Bucket 内数据面的统一实现。

只启用 COS、OSS 和实际使用的 Layer Feature，不能启用全部服务。

### Operator 生命周期

- 每个“账号 ID + Bucket + Region + Endpoint + Root Prefix”创建一个 Operator。
- Operator 在内存中缓存并跨任务共享。
- 保存账号、轮换 AK/SK、修改生产模式或删除账号时立即清理相关 Operator。
- Operator 不持久化。
- 使用 `operator.info().capability()` 判断可用能力；不支持的按钮不显示或禁用。
- 缓存键包含账号 `revision`，失效时同时记录最小可接受 revision，防止并发中的旧 Operator 构建完成后重新写回缓存。

### Runtime

GPUI 使用 smol，而 OpenDAL 默认 HTTP 路径依赖 Tokio 生态。基础设施层持有一个对象存储专用 Tokio multi-thread Runtime：

- 初始 worker 数为 2。
- 所有账号和 Operator 共享，不能每个 Bucket 新建 Runtime。
- 网络 Future 通过 Tokio `JoinHandle` 与 GPUI/smol 桥接。
- 大文件传输不得在 UI 线程执行。
- Runtime 创建失败必须使云存储服务初始化失败，不能回退到同步阻塞 UI。
- Runtime 可显式停止；应用退出时停止接收新任务，并在独立停止线程中执行有界 `shutdown_timeout`。

### 列举分页

OpenDAL 的 `Lister` 由 Infra 持有并有界消费。领域层获得不透明游标：

```text
首次 list_page
  └── 创建 Lister，读取最多 500 条
        ├── 结束：next_cursor = None
        └── 未结束：保存 Lister，返回随机 cursor ID

下一页
  └── 用 cursor ID 继续消费同一个 Lister
```

游标要求：

- 绑定账号、Bucket、Prefix 和请求 generation。
- 十分钟未使用自动过期。
- 每个工作区最多保留一个活动游标。
- 刷新、切换 Prefix、关闭会话或修改账号时立即销毁。
- 无效或过期游标返回可恢复错误，UI 重新加载第一页。

### 依赖评估

需要新增的主要依赖及理由：

| 依赖 | 理由 |
|---|---|
| `opendal` | 统一 COS、OSS 的对象数据面 |
| `reqsign-aliyun-oss` | OSS 数据面受限 transport 需要显式 V4 改签 |
| `reqwest` | 为 OpenDAL 提供关闭默认凭据链的 rustls HTTP transport |

OSS 改签采用 `reqsign-aliyun-oss`。选择具体版本时必须确认：

- 与选定 OpenDAL 版本的依赖兼容。
- 支持当前 COS 和 OSS 签名版本。
- 能使用显式静态 AK/SK Provider，并关闭环境变量、CLI Profile、实例元数据等凭据链。
- 支持 rustls，不引入 OpenSSL。
- 错误不会输出密钥。

如果依赖树已有兼容实现，仍应声明直接依赖，不能依赖不可控的传递依赖。不得为了未使用的账号级资源发现保留签名或 XML 依赖。

## 对象浏览语义

### Prefix 导航

- 面包屑由当前 Prefix 按 `/` 分段生成。
- 点击虚拟目录只改变 Prefix。
- 不递归预加载完整树。
- 不向云端创建“空目录”标记。
- 空 Prefix 表示 Bucket 根。

### 搜索

对象存储原生只高效支持 Key 前缀列举。本次筛选定义为：

- Bucket 列表：对已加载 Bucket 做本地名称过滤。
- 对象列表：对当前已加载目录条目做本地、不区分大小写的名称包含筛选。
- 不提供全 Bucket 子串模糊搜索。
- 不扫描全部对象来模拟搜索。
- 搜索不会额外请求云端；加载更多后，新条目自动参与筛选。

对象浏览仍按当前 Prefix 分页列举；本地筛选只处理已经加载到工作区的条目，不能描述为全 Bucket 搜索。

### 元数据

列表只显示列举响应中已有字段。用户选中对象后再调用 `stat` 获取完整元数据，避免 N+1 请求。

原始 ETag、版本、存储类型和自定义元数据按服务端返回展示，不把 ETag 描述为可靠的内容哈希。

## 对象详情

- 双击对象后按需调用 `stat`，右侧抽屉只展示基本信息和自定义元数据。
- 单击只选择列表行，不发起远程请求，也不打开详情。
- 点击抽屉外关闭详情；抽屉自身必须使用不透明背景并阻止鼠标事件穿透。
- 不读取对象正文，不提供文本、图片或十六进制预览；需要内容时使用下载。

## 上传、下载与删除

### 上传

- 流式读取本地文件，不能整文件载入内存。
- 默认禁止覆盖。
- 目标 Key 已存在时展示 Bucket、完整 Key、现有大小和修改时间。
- 用户确认后才允许覆盖。
- 覆盖确认必须提示“检查目标”和“实际写入”之间仍存在远端竞态；产品不把确认描述为条件更新保证。
- 开启生产模式的账号在 UI、App、Infra 三处拒绝写操作。
- 应用层不对结果不明确的上传自动重试。

### 下载

- 流式写入同目录的 Ramag 临时文件。
- 完成并校验预期长度后再原子替换最终文件。
- Unix 使用同目录原子提交；Windows 使用 `MoveFileExW(REPLACE_EXISTING | WRITE_THROUGH)` 原子替换已有目标，不能先删除旧文件再重命名。
- 本地目标已存在时沿用明确的覆盖策略。
- 取消或失败后删除临时文件。
- 不承诺跨进程断点续传。

### 删除

- 只支持单对象删除。
- 对 Prefix 的“删除目录”入口不提供。
- 确认框展示账号、Bucket 和完整 Object Key。
- 开启生产模式的账号不得出现可执行删除入口。
- 不进行应用层自动重试。
- Bucket 开启版本控制时，删除可能产生 Delete Marker；UI 不能宣称数据已永久清除。

### 传输限制

- 全局最多 3 个并发云存储传输。
- 最多 64 个等待任务。
- 最多保留 100 条完成历史。
- 传输任务必须可取消。
- 关闭窗口不应留下无法控制的后台任务；应用退出时有界等待并终止 Runtime。

## 错误模型

至少区分：

| 类别 | 用户提示重点 |
|---|---|
| InvalidConfig | 具体字段和校验规则 |
| InvalidCredentials | AK/SK 无效或签名失败 |
| PermissionDenied | 缺少的厂商 Action 和目标资源 |
| ClockSkew | 本机时间与服务端偏差 |
| Network | DNS、连接、代理等网络问题 |
| Tls | 证书或 TLS 握手失败 |
| Timeout | 操作类型和超时时间 |
| RateLimited | 服务商限流，建议稍后刷新 |
| NotFound | Bucket 或 Object 不存在 |
| Conflict | 对象已变化或目标已存在 |
| Archived | 对象需要先恢复 |
| Cancelled | 用户取消 |
| Provider | 服务商错误码、RequestId |
| CorruptResponse | 响应缺字段、XML 非法或超限 |

错误信息要求：

- UI 可显示服务商 RequestId，便于排查。
- 日志使用英文短句。
- 日志只记录账号 ID、服务商、操作类型、状态码、错误码和 RequestId。
- 不记录 AK、SK、Authorization、签名、对象内容和完整响应。
- Object Key 和 Bucket 名称默认不进入 info 日志；必要诊断只能使用有界、脱敏形式。

## 超时与重试

初始策略：

| 操作 | 超时/策略 |
|---|---|
| DNS、TCP、TLS 建连 | 10 秒 |
| 保存前挂载验证 | 每个挂载执行一次有界对象列举 |
| OpenDAL 对象操作 | HTTP 单次读取 30 秒无数据则失败 |
| 传输无进度 | HTTP 单次读取 30 秒无数据则失败 |
| 大文件总时长 | 不设固定总时长，受取消和无进度超时约束 |

重试规则：

- 保存前的挂载验证不在应用层盲目重试；临时网络错误允许保存为“未验证”。
- 列对象使用有状态 Lister，流中失败时废弃游标并由 UI 重新加载；上传、下载流中失败不从中间盲目重放。
- 鉴权、权限、校验、404 和响应解析错误不重试。
- 上传、覆盖和删除不在 App 层自动重试。

## 持久化与密钥安全

### 存储方式

`ramag-infra-storage` 新增 `object_storage_accounts` 表。每条账号记录的完整 JSON 使用现有 `Cipher` 加密为密文后写入 redb，包括：

- 账号名称。
- 服务商。
- AK、SK。
- 生产模式状态（底层字段为 `read_only`，整体随账号密文保存）。
- Bucket 挂载。

不能只加密 SK 而明文保存其他字段，因为账号名、Bucket 和 Endpoint 也可能泄露基础设施信息。

### 主密钥边界

- 继续复用系统 Keychain/Credential Manager 中的 Ramag 主密钥。
- `database_has_encrypted_records` 必须纳入云账号表，防止存在云账号时错误重建主密钥。
- 测试通过 `open_with_key` 注入固定密钥。
- 删除账号后删除对应密文、加密偏好和内存缓存。

### 内存与日志

- 账号结构不得派生会输出 SK 的默认 `Debug`。
- 表单中的 SK 默认遮挡，编辑时不能回显到日志或通知。
- 错误转换前移除请求头和签名信息。
- 剪贴板粘贴 AK/SK 属于用户主动操作；Ramag 不额外复制凭据到剪贴板。

## UI 设计

### 整体原则

采用：

> 数据库工具的账号会话骨架 + SSH 工具的文件操作习惯 + 云存储的 Bucket 导航和对象详情。

统一的是视觉和交互骨架，不是数据模型。

### 顶部会话

```text
云存储 | COS·生产 [生产] × | OSS·测试 ×
```

- “连接管理”为固定页。
- 每个已打开云账号是一个可关闭会话。
- 会话显示服务商、账号名称、连接状态和生产模式标识。
- 同一账号只打开一个会话。

### 账号管理页

沿用 SSH/数据库连接管理风格：

- 搜索框。
- 新建账号按钮。
- COS/OSS 服务商标识。
- 账号名称、认证方式和生产模式状态。
- 编辑、连接、删除操作。
- 删除账号前显示会关闭的会话和取消的传输数量。

账号表单字段：

1. 服务商图标选择；腾讯云 COS 默认选中。
2. 账号名称。
3. 凭据字段随服务商显示：COS 使用 SecretId/SecretKey，OSS 使用 AccessKey ID/AccessKey Secret。
4. 生产模式（只读保护），新账号默认关闭。
5. 至少一个必填 Bucket 和 Region，可选 Root Prefix；Region 默认上海，COS 为 `ap-shanghai`，OSS 为 `cn-shanghai`。
6. 保存时逐个验证已配置 Bucket 的对象列举权限；任一挂载配置或权限错误都阻止保存。

### 工作区

```text
┌ 云存储 | COS·生产 [生产] × ───────────────────────────────┐
├──────────────┬──────────────────────────────────────────┤
│ Bucket 导航   │ 路径 / 可点击面包屑                       │
│              ├──────────────────────────────────────────┤
│ 华南         │ [当前目录名称筛选] [刷新] [传输] [上传]     │
│  bucket-a    │ 图标 名称 类型 大小 创建时间 修改时间       │
│ 华东         │ ...                                      │
│  bucket-b    │                              ┌──────────┐ │
│              │                              │对象详情  │ │
│              │                              └──────────┘ │
└──────────────┴──────────────────────────────────────────┘
```

- 顶部：与数据库、SSH 相同的“连接管理 + 已打开会话”标签栏。
- 连接管理：与 SSH 相同的居中列表、搜索、新建入口和弹层表单骨架。
- 左侧：Bucket 按地域分组，包含搜索和刷新；与主内容区之间可拖动调整宽度，宽度只作为当前窗口临时状态，不持久化。
- 主区：对象列表为主体，始终占最大宽度。
- 路径：采用与 SSH 相同的可点击面包屑；点击“路径”打开直达窗口，可输入绝对对象路径并管理当前挂载点的常用路径。
- 重新进入账号时恢复上次使用的 Bucket，但不恢复临时浏览 Prefix，始终从挂载根目录开始。
- 对象列表由应用明确保证目录优先、同类按名称升序；分页追加后重新维持整体顺序。
- 对象列表固定显示“名称、类型、大小、创建时间、修改时间”表头；虚拟目录类型显示“文件夹”。
- COS/OSS 的对象列举响应不提供创建时间；创建时间统一显示 `—`。虚拟目录也没有可靠的修改时间，显示 `—`，不得伪造日期。
- 对象修改时间显示到秒，格式为 `YYYY-MM-DD HH:mm:ss`。
- 对象列表使用固定行高虚拟滚动，只渲染可见区间；分页累计上限仍为 20,000 条。
- 详情：双击对象后按需从右侧覆盖打开；单击只选择。点击详情外自动关闭，不跨会话恢复。
- 详情只按“基本信息、自定义元数据”分区；下载和删除使用图标按钮，并提供明确的悬浮提示。
- 上传或下载开始后自动打开与 SSH 一致的悬浮传输面板；入口紧跟刷新按钮。面板显示方向、本地路径、对象键、百分比、已传输/总量和取消入口，完成历史可清理。
- 通知、账号表单及删除/覆盖确认复用其他工具的通知与居中遮罩弹层样式。

### 响应式

- 常规窗口（≥ 820 px）：Bucket + 对象列表两栏，详情按需从右侧覆盖打开。
- 小窗口（< 820 px）：对象列表优先，Bucket 导航按需切换，详情使用全屏覆盖面板。
- 对象列表始终优先获得宽度，不能照搬 SSH 的“窄文件区 + 宽终端区”。

### 状态

Bucket 区必须有独立状态：

- 正在加载配置。
- 加载成功。
- 旧账号没有 Bucket，需要编辑后补充。
- AK/SK 无效。
- 网络、限流或服务错误，可刷新。

对象区必须有独立状态：

- 尚未选择 Bucket。
- 正在加载 Prefix。
- 空 Prefix。
- 加载失败。
- 游标失效，重新加载。
- 归档或无读取权限。

## 并发与生命周期

- 对象刷新、详情和传输分别使用 generation 或任务 ID，防止旧任务覆盖新状态。
- 切换 Bucket 不立即销毁同账号其他 Bucket 的 Operator，但受缓存上限约束。
- Operator 缓存最多 32 个，使用最近最少使用策略淘汰。
- 删除或编辑账号时先禁止新任务，再取消传输（包括等待并发槽的任务）、最多等待 35 秒退出，清理 Operator，最后修改持久化；超时则不修改账号。
- 应用退出时调用 `ObjectStorageDriver::shutdown`。
- 账号读写闸门会跨数据面 `.await` 持有，以保证编辑/删除与正在执行的账号操作之间没有竞态；闸门本身是异步 RwLock，不持有同步 Mutex 跨 `.await`。
- 传输进度更新需节流，避免每个数据块触发 GPUI 重绘。

## 测试策略

### Domain 单元测试

- 账号、AK/SK、Bucket、Region、Endpoint、Key 和 Prefix 校验。
- Provider 字段名称映射。
- Bucket 必填、排序和重复挂载校验。
- 生产模式只读策略。
- Object Entry 和分页边界。
- 传输数量和对象列表资源上限。

### App 单元测试

使用 Fake Driver 和 Fake Storage：

- 未配置 Bucket 的账号拒绝保存。
- 保存时验证每个已配置 Bucket；无效凭据和权限错误拒绝保存。
- 读取挂载只访问本地加密配置，不调用远端账号级列桶接口。
- 编辑账号清理 Operator 和游标缓存。
- 生产模式账号的上传和删除在 Driver 调用前被拒绝。
- 写操作失败不自动重试。
- 工作区偏好加密保存和恢复。
- 传输排队、取消、进度和历史裁剪。

### Infra 单元测试

- 官方 Endpoint 和 Operator 配置。
- 错误码和 RequestId 映射。
- 数据面请求不泄露凭据到 Debug/Error。
- Operator Cache 和 Cursor Store 的失效、过期和上限。
- 下载临时文件提交及失败清理。
- 取消上传、下载和应用退出。

### Storage 测试

- 云账号 CRUD。
- 密文不包含账号名、AK、SK、Bucket 和 Endpoint。
- 主密钥错误时显式失败。
- 旧数据库启动时自动补表。
- 云账号表有记录时不得重建主密钥。
- 删除账号不影响其他账号和已有数据库、SSH 配置。

### 真实服务集成测试

真实测试默认 `#[ignore]`，只有设置环境变量时运行，不能在 CI 或源码中存放密钥：

```text
RAMAG_TEST_COS_SECRET_ID
RAMAG_TEST_COS_SECRET_KEY
RAMAG_TEST_COS_BUCKET
RAMAG_TEST_COS_REGION
RAMAG_TEST_COS_PREFIX

RAMAG_TEST_OSS_ACCESS_KEY_ID
RAMAG_TEST_OSS_ACCESS_KEY_SECRET
RAMAG_TEST_OSS_BUCKET
RAMAG_TEST_OSS_REGION
RAMAG_TEST_OSS_PREFIX
```

测试账号必须是专用、最小权限账号；写测试只能在显式的测试 Bucket 和 Prefix 下进行。测试覆盖：

- 显式配置的 Bucket 能通过对象列举验证。
- 根 Prefix 和子 Prefix 列举。
- stat。
- 上传、下载、覆盖拒绝和删除测试对象。
- 只读身份无法写入。
- 清理失败时报告残留 Key，不能静默忽略。

### UI 验证

- 亮色、暗色。
- 最小窗口、常用窗口、宽屏。
- 未配置 Bucket 的旧账号、多个地域、大量 Bucket。
- 大量对象虚拟滚动和加载更多。
- 长 Bucket 名、长 Key、中文和特殊字符。
- 详情抽屉和传输面板。
- 当前目录名称包含筛选、秒级时间显示和空目录标记过滤。
- 单击选择、双击打开、点击抽屉外关闭及鼠标事件不穿透。
- 只读按钮、删除确认和覆盖确认。
- 错误提示可读且不包含密钥。

## 完整验收标准

以下条件全部满足才算交付完成：

1. COS 和 OSS 都能使用永久 AK/SK 创建账号。
2. 新建或编辑账号必须配置至少一个 Bucket 和 Region。
3. 保存时只验证显式配置的 Bucket，不依赖 `ListBuckets`/`GetService` 权限。
4. Bucket 按地域稳定展示，刷新不会重复或丢失配置。
5. 两家服务都能浏览 Prefix、分页加载和查看元数据。
6. 双击对象可查看完整元数据，单击不会打开详情，点击详情外可关闭。
7. 上传、下载流式执行，支持进度、取消、去重和覆盖保护。
8. 单对象删除有完整确认，Prefix 不能被误当成目录递归删除。
9. 开启生产模式的账号在 UI、App、Infra 三层都无法写入。
10. AK/SK、账号名和 Bucket 信息加密落盘，日志不泄露凭据。
11. 大量对象、大文件传输和网络异常不会阻塞 UI。
12. 账号修改、删除、关闭会话和应用退出能正确清理任务与缓存。
13. Domain、App、Infra、Storage 核心测试通过。
14. `make fmt-check`、`make check`、`make clippy`、`make test` 全部通过。
15. `docs/architecture.md` 同步补充新增 Crate、Runtime 和依赖方向。

## 实现清单

本文不按阶段交付，以下清单全部属于同一次实现：

- [x] 新增 Domain 实体、校验常量和 `ObjectStorageDriver`。
- [x] 扩展 `Storage` trait 的云账号 CRUD，默认方法保持旧测试 Mock 兼容。
- [x] 新增 redb 加密表、Schema 初始化和密钥恢复保护。
- [x] 新建 `ramag-infra-object-storage`。
- [x] 接入 OpenDAL COS、OSS。
- [x] 实现必填 Bucket 挂载和官方 Endpoint 生成。
- [x] 保存时通过数据面列举验证全部配置挂载。
- [x] 实现 Operator Cache、Cursor Store 和专用 Runtime。
- [x] 实现浏览、元数据、上传、下载和单对象删除。
- [x] 实现 `ObjectStorageService`、只读门禁、缓存、传输和偏好。
- [x] 新建 `ramag-tool-object-storage` 和完整 UI。
- [x] 在 `ramag-bin` 完成依赖注入、工具及视图注册。
- [x] 增加 Domain、App、Infra、Storage 单元测试和 COS/OSS `#[ignore]` 集成测试。
- [x] 更新架构文档，并通过 `make fmt-check`、`make check`、`make clippy`、`make test`、`make win-debug`。

## 官方参考

- [Apache OpenDAL Rust 文档](https://opendal.apache.org/docs/rust/opendal/)
- [Apache OpenDAL COS 配置](https://opendal.apache.org/services/cos/)
- [Apache OpenDAL OSS 配置](https://opendal.apache.org/services/oss/)
- [腾讯云 COS：授权与身份认证流程](https://cloud.tencent.com/document/product/436/68279)
- [腾讯云 COSBrowser：桌面端登录方式](https://cloud.tencent.com/document/product/436/38103)
- [阿里云 OSS：用户签名验证](https://help.aliyun.com/zh/oss/verify-user-signatures)
- [阿里云 SDK：访问凭据配置方式](https://help.aliyun.com/zh/sdk/developer-reference/configure-credentials-2)
- [阿里云 ossbrowser：登录认证方式](https://help.aliyun.com/zh/oss/developer-reference/login-to-ossbrowser-2-0)
