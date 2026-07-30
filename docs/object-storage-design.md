# Ramag 云对象存储工具详细设计

## 文档状态

- 状态：已确认设计，尚未实现
- 核对日期：2026-07-30
- 用户可见名称：云存储
- 代码命名：`object_storage`
- 首次交付服务商：腾讯云 COS、阿里云 OSS
- 唯一认证方式：永久访问密钥（AK/SK）
- 核心依赖：Apache OpenDAL

本文记录云存储工具的产品边界、认证方式、数据模型、界面结构、分层架构、安全要求、测试方案和完整验收标准。本文不按阶段拆分；文中列出的“交付范围”必须作为一次完整功能实现并通过验收。

## 结论先行

云存储工具采用“账号级 Bucket 发现 + Bucket 内统一对象访问”的两层架构：

```text
永久 AK/SK
    │
    ├── Bucket 发现层
    │     ├── COS：GET Service（List Buckets）
    │     └── OSS：ListBuckets（GetService）
    │
    └── 选中 Bucket
          └── Apache OpenDAL Operator
                ├── 列举对象
                ├── 查看元数据和预览
                ├── 上传、下载
                └── 单对象删除
```

确定的关键决策如下：

1. 使用 Apache OpenDAL 处理 Bucket 内的数据访问，不分别接入两套非官方 Rust SDK。
2. OpenDAL 不负责账号级 Bucket 列举；由两个小型厂商适配器调用官方 REST API。
3. 一次交付即支持自动列出账号通过官方列桶 API 可见的全部 Bucket。
4. 持久化模型从一开始就分离“云账号”和“Bucket”，不能把一个账号复制成多个 Bucket 连接。
5. 唯一支持永久 AK/SK；除非以后重新作出明确产品决策，否则不支持其他认证方式，也不为其他方式提前设计入口。
6. 默认只读；写权限需要用户明确开启，删除和覆盖必须再次确认。
7. 界面复用数据库工具的会话骨架和 SSH 工具的文件操作习惯，但对象模型保持独立。

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
2. 保存账号后自动列出该身份通过列桶 API 可见的全部 Bucket。
3. 支持 Bucket 按地域分组、搜索和手动刷新。
4. 支持手动添加未被列桶 API 返回但用户已知且有权访问的 Bucket。
5. 支持按前缀浏览对象和虚拟目录。
6. 支持查看对象元数据、文本预览和图片预览。
7. 支持流式上传、下载、取消和进度展示。
8. 支持经过确认的单对象删除。
9. 支持只读账号，并在 UI、应用层、基础设施层实施写入门禁。
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
- 自动列出账号通过官方 API 可见的 Bucket。
- Bucket 搜索、地域分组、刷新和手动补充。
- Bucket 内前缀浏览和分页加载。
- 对象名称、大小、修改时间、ETag、内容类型、存储类型等可用元数据。
- 文本和图片安全预览。
- 文件上传、下载、覆盖策略、进度和取消。
- 单对象删除及确认。
- 只读模式。
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

一组服务商和永久 AK/SK 的持久化配置。一个云账号可以发现多个 Bucket。

### Bucket

COS 或 OSS 的存储空间。Bucket 信息来自官方列桶 API或用户手动补充。

### Object

Bucket 内由 Key 唯一标识的数据对象。UI 可以显示为文件，但领域层统一称为对象。

### Prefix

Object Key 的前缀。以 `/` 分隔后可模拟目录层级，但不是必须存在的真实目录。

### 工作区

一个已打开的云账号会话，包含选中的 Bucket、当前 Prefix、选择项、加载游标和传输队列显示状态。

### “列出全部 Bucket”的准确含义

产品承诺应表述为：

> 列出当前身份通过服务商官方列桶 API 可见的、账号拥有的全部 Bucket。

不能表述为“列出该密钥能够访问的所有 Bucket”，原因包括：

- 跨账号通过 Bucket Policy 授权的 Bucket 不一定出现在所有者列表。
- 子账号可能只有某个 Bucket 或 Prefix 权限，没有账号级列桶权限。
- 服务商控制台可能使用额外内部能力展示资源，公开 API 的结果不一定完全相同。

因此必须保留手动添加 Bucket 入口。手动添加不是自动发现的替代实现，而是处理受限权限和跨账号授权的必要兜底。

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

- 创建列桶签名器和 OpenDAL Operator 时始终传入当前账号保存的 AK/SK。
- 不读取环境变量、用户目录中的云 CLI Profile、ECS/CVM 元数据或其他默认凭据链。
- OpenDAL 服务支持关闭默认配置加载时必须显式关闭。
- AK/SK 为空或不完整时本地报错，不能静默回退到机器上的其他身份。

安全建议：

- 优先使用 CAM 子账号或 RAM 用户的 AK。
- 不建议使用主账号 AK。
- 权限按最小权限配置。
- 定期轮换密钥；保存新密钥后立即失效该账号的 Operator 和 Bucket 缓存。

### 明确不支持的方式

| 方式 | 凭据形态 | 能否自动列桶 | 产品状态 |
|---|---|---:|---|
| STS 临时凭据 | AK、SK、Security Token | 取决于临时策略 | 不支持 |
| RAM/CAM 角色 | 自动换取临时凭据 | 取决于角色权限 | 不支持 |
| ECS/CVM 实例角色 | 元数据服务临时凭据 | 取决于角色权限 | 不支持 |
| OIDC 工作负载身份 | OIDC Token 换取临时凭据 | 取决于角色权限 | 不支持 |
| 环境变量或 CLI Profile | 本地凭据提供链 | 取决于实际凭据 | 不支持 |
| 云账号网页登录 | 浏览器授权会话 | 通常可以 | 不支持 |
| 授权码、共享链接 | 限定资源的临时授权 | 通常不能 | 不支持 |
| 预签名 URL | 单次对象请求 URL | 不能 | 不支持 |
| 匿名访问 | 无凭据 | 不能 | 不支持 |

Ramag 不为以上方式显示禁用占位选项，也不实现自动探测或隐式降级。Ramag 永远不能要求用户输入并保存云账号密码。

## 权限要求

### 自动列桶

- COS：身份必须拥有 `cos:GetService`。
- OSS：身份必须拥有 `oss:ListBuckets`。

### Bucket 内操作

身份还需要与功能相匹配的对象列举、读取、写入或删除权限。Ramag 不应要求管理员直接授予服务商预置的全量管理权限，而应在文档和错误提示中列出缺失的具体 Action。

### 权限失败行为

连接测试必须区分以下情况：

1. AK/SK 格式无效：本地拒绝，不发送请求。
2. 签名或身份无效：显示“访问密钥无效或签名失败”。
3. 缺少列桶权限：显示缺少的 `cos:GetService` 或 `oss:ListBuckets`，允许保存账号并手动添加 Bucket。
4. 列桶成功但结果为空：连接成功，显示空状态。
5. Bucket 可见但无对象读取权限：Bucket 保留在列表，打开时显示权限错误。
6. 网络、DNS、TLS、超时、限流和服务端错误：分别映射，不误报为密钥错误。

## Bucket 自动发现

### COS

调用官方 `GET Service (List Buckets)`：

- 请求：`GET /`
- Host：`service.cos.myqcloud.com`
- 鉴权：COS 请求签名
- 返回：Owner，以及 Bucket 的 Name、Location、CreationDate
- 权限：`cos:GetService`

COS 返回的 `Location` 用于生成 Bucket 数据面 Endpoint，再创建 OpenDAL COS Operator。

### OSS

调用官方 `ListBuckets (GetService)`：

- 请求：`GET /`
- 默认 Host：`oss-cn-hangzhou.aliyuncs.com`
- 默认签名地域：`cn-hangzhou`
- 签名版本：OSS V4
- 鉴权：OSS 请求签名
- 参数：首个请求使用 `max-keys=1000`，后续请求携带 `marker`
- 返回：Name、Location、Region、ExtranetEndpoint、IntranetEndpoint、StorageClass 等
- 权限：`oss:ListBuckets`

官方说明列桶结果与发起请求所选的地域 Endpoint 无关，可以返回账号下所有地域的 Bucket，因此用户不需要先填写一个 Bucket 地域。实现不得携带资源组过滤头，必须按 `IsTruncated` 和 `NextMarker` 拉取全部分页，不能只显示第一页。桌面客户端访问 Bucket 时默认使用响应中的 `ExtranetEndpoint`。

### 统一结果

两个适配器都转换为领域对象：

```rust
pub struct BucketSummary {
    pub name: String,
    pub region: String,
    pub endpoint: String,
    pub created_at: Option<DateTime<Utc>>,
    pub storage_class: Option<String>,
    pub source: BucketSource,
}

pub enum BucketSource {
    Discovered,
    Manual,
}
```

结果按“地域、Bucket 名称”稳定排序。以服务商、账号 ID、Bucket 名称和地域去重，不能仅按显示名称去重。

### 刷新和缓存

- 打开账号工作区时自动加载。
- 五分钟内复用内存缓存。
- 用户点击刷新时绕过缓存。
- 同一账号的并发刷新必须合并为一个请求。
- 使用 generation 标识，旧请求晚返回时不能覆盖新结果。
- 不进行持续后台轮询。
- 自动发现结果默认不落盘；手动 Bucket 和收藏路径需要加密持久化。

## 数据模型

### 账号

```rust
pub struct ObjectStorageAccount {
    pub schema_version: u16,
    pub id: ObjectStorageAccountId,
    pub name: String,
    pub provider: CloudProvider,
    pub access_key_id: String,
    pub access_key_secret: String,
    pub read_only: bool,
    pub manual_buckets: Vec<ManualBucket>,
}

pub enum CloudProvider {
    TencentCos,
    AliyunOss,
}
```

不增加未使用的 STS、Role 等枚举变体。`schema_version` 只用于账号记录自身的兼容迁移，不代表预留其他认证方式。

### 手动 Bucket

```rust
pub struct ManualBucket {
    pub name: String,
    pub region: String,
    pub endpoint: Option<String>,
    pub root_prefix: Option<String>,
}
```

自动发现 Bucket 不复制进账号记录。手动 Bucket 作为账号配置的一部分整体加密。

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
- 每个 Bucket 最后访问的 Prefix。
- 收藏的 Bucket 和 Prefix。
- 分栏宽度和详情面板状态。

Bucket 名称和 Prefix 可能包含业务信息，必须使用 `Storage::seal` 加密后再写偏好 KV，不能明文保存。

## 输入校验与资源上限

首次实现采用以下上限，实际常量定义在 Domain：

| 项目 | 上限 |
|---|---:|
| 云账号数量 | 64 |
| 账号名称 | 128 bytes |
| AccessKey ID / SecretId | 256 bytes |
| AccessKey Secret / SecretKey | 512 bytes |
| 手动 Bucket 数量/账号 | 128 |
| Bucket 名称 | 255 bytes |
| Region | 128 bytes |
| Endpoint | 2 KiB |
| Object Key / Prefix | 4 KiB |
| 单页对象条目 | 500 |
| 工作区累计对象条目 | 20,000 |
| 文本预览 | 2 MiB |
| 图片预览源文件 | 16 MiB |
| 图片解码像素 | 40 MP |
| 并发传输 | 3 |
| 等待中传输 | 64 |
| 传输历史 | 100 |

校验规则：

- 所有输入按 UTF-8 字节长度检查。
- Bucket、Region 按服务商规则校验，错误说明具体字段。
- Endpoint 只接受有效 HTTPS URL；官方 Endpoint 由程序生成或使用 API 返回值。
- Object Key 禁止 NUL，但不能套用本地路径或 SFTP 绝对路径规则。
- `..` 在 Object Key 中只是字符，不能擅自按文件系统父目录解释。
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
| `ramag-app` | `ObjectStorageService`，编排缓存、权限、传输和持久化 |
| `ramag-infra-object-storage` | OpenDAL、COS/OSS 列桶适配器、HTTP 签名、Runtime |
| `ramag-infra-storage` | 加密保存云账号和手动 Bucket |
| `ramag-tool-object-storage` | GPUI 连接管理、Bucket 导航、对象列表、详情和传输 UI |
| `ramag-bin` | 依赖注入、工具注册、视图注册和快捷键 |

云存储不得依赖 `ramag-tool-ssh`、`ramag-infra-ssh` 或数据库工具。可复用的视觉原语只能下沉到 `ramag-ui`。

### Domain trait

账号控制面与 Bucket 数据面方法集不同，保持两个 trait：

```rust
#[async_trait]
pub trait BucketCatalog: Send + Sync {
    async fn list_buckets(
        &self,
        account: &ObjectStorageAccount,
        refresh: bool,
    ) -> Result<Vec<BucketSummary>>;
}

#[async_trait]
pub trait ObjectStorageDriver: Send + Sync {
    async fn list_page(
        &self,
        location: &ObjectLocation,
        prefix: &str,
        cursor: Option<&ObjectListCursor>,
    ) -> Result<ObjectPage>;

    async fn stat(
        &self,
        location: &ObjectLocation,
        key: &str,
    ) -> Result<ObjectMetadata>;

    async fn read_preview(
        &self,
        location: &ObjectLocation,
        key: &str,
    ) -> Result<ObjectPreview>;

    async fn upload(&self, request: ObjectUploadRequest) -> Result<()>;
    async fn download(&self, request: ObjectDownloadRequest) -> Result<()>;
    async fn delete(&self, location: &ObjectLocation, key: &str) -> Result<()>;
    async fn disconnect_account(&self, account_id: &ObjectStorageAccountId) -> Result<()>;
    async fn shutdown(&self) -> Result<()>;
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
- Bucket 自动发现与手动 Bucket 合并。
- Bucket 缓存和刷新合并。
- 工作区偏好加密和恢复。
- 当前 Prefix、分页和选择状态的业务规则。
- 只读门禁。
- 上传、下载任务队列、取消和历史。
- 保存、删除账号时失效 Operator、游标、Bucket 缓存和传输。

### Infra

`ramag-infra-object-storage` 建议按职责拆分：

```text
src/
  lib.rs
  runtime.rs
  errors.rs
  operator_cache.rs
  cursor_store.rs
  discovery/
    mod.rs
    cos.rs
    oss.rs
    xml.rs
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
- 保存账号、轮换 AK/SK、修改只读状态或删除账号时立即清理相关 Operator。
- Operator 不持久化。
- 使用 `full_capability` 判断可用能力；不支持的按钮不显示或禁用。

### Runtime

GPUI 使用 smol，而 OpenDAL 默认 HTTP 路径依赖 Tokio 生态。基础设施层持有一个对象存储专用 Tokio multi-thread Runtime：

- 初始 worker 数为 2。
- 所有账号和 Operator 共享，不能每个 Bucket 新建 Runtime。
- 网络 Future 通过 oneshot 与 GPUI/smol 桥接。
- 大文件传输不得在 UI 线程执行。
- Runtime 创建失败必须使云存储服务初始化失败，不能回退到同步阻塞 UI。

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
| `reqsign-tencent-cos`、`reqsign-aliyun-oss` | 官方列桶 API 需要签名，禁止自行实现安全敏感算法 |
| `reqwest` | OpenDAL 不暴露账号级列桶操作，需要独立调用官方 REST API |
| `quick-xml` | COS、OSS 列桶 API 返回 XML |

签名实现采用 `reqsign-tencent-cos` 和 `reqsign-aliyun-oss`。选择具体版本时必须确认：

- 与选定 OpenDAL 版本的依赖兼容。
- 支持当前 COS 和 OSS 签名版本。
- 能使用显式静态 AK/SK Provider，并关闭环境变量、CLI Profile、实例元数据等凭据链。
- 支持 rustls，不引入 OpenSSL。
- 错误不会输出密钥。

如果依赖树已有兼容实现，仍应声明直接依赖，不能依赖不可控的传递依赖。不得为了一个列桶接口引入两套完整的非官方厂商 SDK。

## 对象浏览语义

### Prefix 导航

- 面包屑由当前 Prefix 按 `/` 分段生成。
- 点击虚拟目录只改变 Prefix。
- 不递归预加载完整树。
- 不向云端创建“空目录”标记。
- 空 Prefix 表示 Bucket 根。

### 搜索

对象存储原生只高效支持 Key 前缀列举。本次搜索定义为：

- Bucket 列表：对已加载 Bucket 做本地名称过滤。
- 对象列表：按当前 Prefix 下的名称前缀查询。
- 不提供全 Bucket 子串模糊搜索。
- 不扫描全部对象来模拟搜索。
- UI 文案使用“前缀筛选”，避免误导用户。

### 元数据

列表只显示列举响应中已有字段。用户选中对象后再调用 `stat` 获取完整元数据，避免 N+1 请求。

原始 ETag、版本、存储类型和自定义元数据按服务端返回展示，不把 ETag 描述为可靠的内容哈希。

## 预览

### 文本

- 最多读取 2 MiB。
- 自动识别 UTF-8；非法 UTF-8 显示十六进制摘要或不支持提示。
- HTML、SVG、Markdown 只显示源码，不执行脚本或主动加载远程资源。
- 超过上限显示截断提示和下载入口。

### 图片

- 仅支持项目 `image` 依赖已启用的 PNG、JPEG、TIFF。
- 源文件最大 16 MiB。
- 解码前读取尺寸，超过 40 MP 拒绝预览。
- 解码和缩放在后台执行。
- 解码失败明确提示，不影响下载。

### 其他文件

显示元数据和下载入口，不尝试执行、解压或调用外部应用。

## 上传、下载与删除

### 上传

- 流式读取本地文件，不能整文件载入内存。
- 默认禁止覆盖。
- 目标 Key 已存在时展示 Bucket、完整 Key、现有大小和修改时间。
- 用户确认后才允许覆盖。
- 只读账号在 UI、App、Infra 三处拒绝。
- 应用层不对结果不明确的上传自动重试。

### 下载

- 流式写入同目录的 Ramag 临时文件。
- 完成并校验预期长度后再原子替换最终文件。
- 本地目标已存在时沿用明确的覆盖策略。
- 取消或失败后删除临时文件。
- 不承诺跨进程断点续传。

### 删除

- 只支持单对象删除。
- 对 Prefix 的“删除目录”入口不提供。
- 确认框展示账号、Bucket 和完整 Object Key。
- 只读账号不得出现可执行删除入口。
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
| 列 Bucket | 单页 30 秒，OSS 按页继续 |
| 列对象、stat | 30 秒 |
| 预览首字节 | 15 秒 |
| 传输无进度 | 30 秒触发失败 |
| 大文件总时长 | 不设固定总时长，受取消和无进度超时约束 |

重试规则：

- 列 Bucket、列对象、stat、预览和下载读取属于幂等读，可对临时网络错误、429、部分 5xx 最多重试两次。
- 使用指数退避和抖动。
- 鉴权、权限、校验、404 和响应解析错误不重试。
- 上传、覆盖和删除不在 App 层自动重试。

## 持久化与密钥安全

### 存储方式

`ramag-infra-storage` 新增 `object_storage_accounts` 表。每条账号记录的完整 JSON 使用现有 `Cipher` 加密为密文后写入 redb，包括：

- 账号名称。
- 服务商。
- AK、SK。
- 只读状态。
- 手动 Bucket。

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
连接管理 | COS·生产 [只读] × | OSS·测试 ×
```

- “连接管理”为固定页。
- 每个已打开云账号是一个可关闭会话。
- 会话显示服务商、账号名称、连接状态和只读标识。
- 同一账号只打开一个会话。

### 账号管理页

沿用 SSH/数据库连接管理风格：

- 搜索框。
- 新建账号按钮。
- COS/OSS 服务商标识。
- 账号名称、认证方式和只读状态。
- 编辑、连接、删除操作。
- 删除账号前显示会关闭的会话和取消的传输数量。

账号表单字段：

1. 账号名称。
2. 服务商。
3. SecretId/AccessKey ID。
4. SecretKey/AccessKey Secret。
5. 只读开关，默认开启。
6. 测试并列举 Bucket。

### 工作区

```text
┌ 连接管理 | COS·生产 [只读] × ─────────────────────────────┐
├──────────────┬─────────────────────────────┬─────────────┤
│ Bucket 导航   │ Bucket / Prefix             │ 详情 / 预览 │
│              │ [前缀筛选] [刷新] [上传]     │             │
│ 华南         │ 名称  大小  类型  修改时间   │ 元数据      │
│  bucket-a    │ ...                         │ 图片/文本   │
│ 华东         │                             │ 操作入口    │
│  bucket-b    │                             │             │
├──────────────┴─────────────────────────────┴─────────────┤
│ 对象数量 / 加载更多                           传输任务 3 │
└──────────────────────────────────────────────────────────┘
```

- 左侧：Bucket 按地域分组，包含刷新和手动添加。
- 中间：对象列表为主体，占最大宽度。
- 右侧：选中对象后的详情和预览，可收起。
- 底部：加载状态、对象数量和传输入口。
- 分栏可拖动并加密保存宽度。

### 响应式

- 宽屏：Bucket、对象列表、详情三栏。
- 中等宽度：Bucket + 对象列表，详情以抽屉打开。
- 小窗口：Bucket 导航可折叠，详情使用覆盖面板。
- 对象列表始终优先获得宽度，不能照搬 SSH 的“窄文件区 + 宽终端区”。

### 状态

Bucket 区必须有独立状态：

- 正在鉴权并加载。
- 加载成功。
- 账号没有 Bucket。
- 缺少列桶权限，可手动添加。
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

- Bucket 刷新、对象刷新、预览和传输分别使用 generation，防止旧任务覆盖新状态。
- 切换 Bucket 不立即销毁同账号其他 Bucket 的 Operator，但受缓存上限约束。
- Operator 缓存最多 32 个，使用最近最少使用策略淘汰。
- 删除或编辑账号时先禁止新任务，再取消传输、等待有界退出、清理 Operator，最后修改持久化。
- 应用退出时调用 `ObjectStorageDriver::shutdown`。
- 任何锁都不能跨 `.await` 持有。
- 传输进度更新需节流，避免每个数据块触发 GPUI 重绘。

## 测试策略

### Domain 单元测试

- 账号、AK/SK、Bucket、Region、Endpoint、Key 和 Prefix 校验。
- Provider 字段名称映射。
- 自动 Bucket 与手动 Bucket 合并、排序、去重。
- 只读策略。
- Object Entry 和分页边界。
- 预览大小、图片像素和传输数量上限。

### App 单元测试

使用 Fake Catalog、Fake Driver 和 Fake Storage：

- 空账号也视为列桶成功。
- 列桶权限不足允许保存并手动添加。
- 并发刷新合并。
- 旧 generation 结果不会覆盖新状态。
- 编辑账号清理 Bucket、Operator 和游标缓存。
- 只读账号的上传和删除在 Driver 调用前被拒绝。
- 写操作失败不自动重试。
- 工作区偏好加密保存和恢复。
- 传输排队、取消、进度和历史裁剪。

### Infra 单元测试

- 使用官方示例 XML 作为固定 Fixture。
- COS 列桶响应解析。
- OSS 单页、多页、空页和缺失 NextMarker。
- 超大 XML、重复 Bucket、非法字段和未知枚举。
- 错误码和 RequestId 映射。
- 签名请求不泄露凭据到 Debug/Error。
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
RAMAG_TEST_COS_PREFIX

RAMAG_TEST_OSS_ACCESS_KEY_ID
RAMAG_TEST_OSS_ACCESS_KEY_SECRET
RAMAG_TEST_OSS_BUCKET
RAMAG_TEST_OSS_PREFIX
```

测试账号必须是专用、最小权限账号；写测试只能在显式的测试 Bucket 和 Prefix 下进行。测试覆盖：

- 自动列桶能找到目标 Bucket。
- 根 Prefix 和子 Prefix 列举。
- stat、文本预览。
- 上传、下载、覆盖拒绝和删除测试对象。
- 只读身份无法写入。
- 清理失败时报告残留 Key，不能静默忽略。

### UI 验证

- 亮色、暗色。
- 最小窗口、常用窗口、宽屏。
- 空账号、多个地域、大量 Bucket。
- 大量对象虚拟滚动和加载更多。
- 长 Bucket 名、长 Key、中文和特殊字符。
- 详情抽屉、预览和传输面板。
- 只读按钮、删除确认和覆盖确认。
- 错误提示可读且不包含密钥。

## 完整验收标准

以下条件全部满足才算交付完成：

1. COS 和 OSS 都能使用永久 AK/SK 创建账号。
2. 连接后自动列出列桶 API 可见的全部 Bucket，而非只取第一页。
3. 缺少列桶权限时准确提示 Action，并能手动添加已知 Bucket。
4. Bucket 按地域稳定展示，刷新不会重复或丢失手动 Bucket。
5. 两家服务都能浏览 Prefix、分页加载和查看元数据。
6. 文本、图片预览满足大小和安全限制。
7. 上传、下载流式执行，支持进度、取消和覆盖保护。
8. 单对象删除有完整确认，Prefix 不能被误当成目录递归删除。
9. 只读账号在 UI、App、Infra 三层都无法写入。
10. AK/SK、账号名和 Bucket 信息加密落盘，日志不泄露凭据。
11. 大量对象、大文件传输和网络异常不会阻塞 UI。
12. 账号修改、删除、关闭会话和应用退出能正确清理任务与缓存。
13. Domain、App、Infra、Storage 核心测试通过。
14. `make fmt-check`、`make check`、`make clippy`、`make test` 全部通过。
15. `docs/architecture.md` 同步补充新增 Crate、Runtime 和依赖方向。

## 实现清单

本文不按阶段交付，以下清单全部属于同一次实现：

- [ ] 新增 Domain 实体、校验常量、`BucketCatalog` 和 `ObjectStorageDriver`。
- [ ] 扩展 `Storage` trait 的云账号 CRUD，默认方法保持旧测试 Mock 兼容。
- [ ] 新增 redb 加密表、Schema 初始化和密钥恢复保护。
- [ ] 新建 `ramag-infra-object-storage`。
- [ ] 接入 OpenDAL COS、OSS。
- [ ] 实现 COS `GET Service` 自动列桶。
- [ ] 实现 OSS `ListBuckets` 全量分页。
- [ ] 实现 Operator Cache、Cursor Store 和专用 Runtime。
- [ ] 实现浏览、元数据、预览、上传、下载和单对象删除。
- [ ] 实现 `ObjectStorageService`、只读门禁、缓存、传输和偏好。
- [ ] 新建 `ramag-tool-object-storage` 和完整 UI。
- [ ] 在 `ramag-bin` 完成依赖注入、工具及视图注册。
- [ ] 增加单元、存储、集成和 UI 验证。
- [ ] 更新架构文档并运行全部质量门禁。

## 官方参考

- [Apache OpenDAL Rust 文档](https://opendal.apache.org/docs/rust/opendal/)
- [Apache OpenDAL COS 配置](https://opendal.apache.org/services/cos/)
- [Apache OpenDAL OSS 配置](https://opendal.apache.org/services/oss/)
- [腾讯云 COS：GET Service 查询存储桶列表](https://cloud.tencent.com/document/product/436/113845)
- [腾讯云 COS：授权与身份认证流程](https://cloud.tencent.com/document/product/436/68279)
- [腾讯云 COSBrowser：桌面端登录方式](https://cloud.tencent.com/document/product/436/38103)
- [阿里云 OSS：ListBuckets](https://help.aliyun.com/zh/oss/developer-reference/listbuckets)
- [阿里云 OSS：列出账号所有地域的 Bucket](https://help.aliyun.com/en/oss/developer-reference/list-buckets)
- [阿里云 OSS：用户签名验证](https://help.aliyun.com/zh/oss/verify-user-signatures)
- [阿里云 SDK：访问凭据配置方式](https://help.aliyun.com/zh/sdk/developer-reference/configure-credentials-2)
- [阿里云 ossbrowser：登录认证方式](https://help.aliyun.com/zh/oss/developer-reference/login-to-ossbrowser-2-0)
