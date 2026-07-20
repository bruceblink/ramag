# 数据库「按库导出/导入」设计

> 状态：已实现 · 更新于 2026-07-20
> 末尾「拍板结果与实现落点」记录最终决策；四引擎导出→删库→导入→校验的
> 端到端集成测试见 `crates/ramag-app/tests/transfer_live.rs`。

## 目标与范围

给四种数据库（MySQL / PostgreSQL / Redis / MongoDB）提供**按库导出、按库导入**能力：把一个库里的全部内容导出成文件，之后能导回同类型数据库。

### 先划清边界（与已有功能区分）

- **连接配置导出/导入**（已实现）：导出的是「怎么连」——host / 端口 / 密码，口令加密 JSON。
- **库数据导出/导入**（本设计）：导出的是「库里有什么」——表结构+数据 / key+value / 文档。

两者完全独立，入口、格式、用途都不同。

## 一、四个数据库的「库」语义不同

导出格式必须分库设计——四种「库」装的东西本质不同：

| 引擎 | 「库」是什么 | 装什么 | 结构 |
|---|---|---|---|
| MySQL / PostgreSQL | database / schema | 表、视图、索引、外键 | 强结构（DDL） |
| MongoDB | database | collection + 索引 | 无固定结构（文档自描述） |
| Redis | DB 编号（0-15） | key（6 类型）+ TTL | 无结构，类型在 key 上 |

## 二、核心架构决策：外部工具 vs 纯 driver

**方案 A — 调用外部 dump 工具**（mysqldump / pg_dump / redis-cli / mongodump）
- 优点：完整（触发器、存储过程、权限、精确类型全覆盖）、快、标准格式
- 缺点：要求用户装齐 4 个工具且版本匹配服务端；跨平台路径麻烦；与「纯 Rust 本地」定位冲突（SSH 隧道虽用系统 ssh，但那只依赖一个通用工具，dump 是 4 工具 × 版本，依赖面大得多）

**方案 B — 纯 driver 逐条读写 + 通用格式**（推荐）
- 优点：零外部依赖、跨平台一致、复用现有 driver 能力、产出的通用格式也能被官方客户端导入
- 缺点：覆盖不了触发器/存储过程/精确权限；大库比原生工具慢；类型序列化自己写（多数已有）

**推荐方案 B**。现有 driver 已能覆盖「表结构+数据 / 文档 / key-value」这个核心需求；触发器/存储过程等高级对象列为「暂不支持」并在导出时明确提示，不假装完整。

### 现有能力盘点（方案 B 的地基）

| 引擎 | 读（导出用） | 写（导入用） | 结构导出 |
|---|---|---|---|
| SQL | `execute` 分页 SELECT | `execute` 批量 INSERT | `ddl.rs` 已能生成建表语句 |
| Redis | `scan` + `get_value_limited` | `execute_command` | 类型在 key 上，无独立 DDL |
| Mongo | `find` + `list_indexes` | `insert_one`（大库需补 `insert_many`） | 索引定义可导 |

## 三、各库导出格式

| 引擎 | 格式 | 内容 | 官方工具可导入 |
|---|---|---|---|
| MySQL / PG | `.sql` 脚本（`-- ramag:begin` 段标记） | DDL + `INSERT` 批量；PG 另含枚举类型 / 序列预建 / FK 后置 ALTER / setval | 是（mysql / psql） |
| MongoDB | `.jsonl` | 集合行（原始索引 spec）+ 文档行 `{"doc": …}`（relaxed EJSON，Int64 保 `$numberLong`） | 需 `jq '.doc'` 提取后走 mongoimport |
| Redis | `.jsonl` | key 首记录 `{key,type,ttl_ms,value}`；大 key 拆多条续记录流式写读 | 否（需 App 导入） |

- SQL：MySQL 用 `SHOW CREATE`（FK 内联，导入时每块前缀 `SET FOREIGN_KEY_CHECKS=0` 免拓扑排序）；PG 结构化拼 DDL（FK 拆到数据后 ALTER，天然免排序、环形依赖也可导）。INSERT 单条一行、按主键 keyset 分页导出（无主键退化 OFFSET 并告警）、按字节+行数分批。
- Mongo：`_id` keyset 翻页用 `$expr + $literal`（聚合比较是跨类型全序，混合类型 `_id` 不漏）；索引用 listIndexes 原始 spec 往返。
- Redis：值分段读写（`read_value_page` / `write_value_items`），二进制成员 hex 编码保真；TTL 记导出时剩余毫秒，导入完成后 PEXPIRE 恢复。

## 四、导入语义（破坏性写操作）

1. **目标库**：SQL 以文件内记录的库名为准（改名导入受 DDL 内限定名限制，本轮不做）；Mongo 可导入任意目标库、Redis 可选目标 DB 编号。
2. **冲突处理**（同名表/collection/key 已存在）：跳过 / 合并 / 覆盖（先删）/ 报错停止；默认「跳过 + 汇总」。重复导入同一文件：「跳过」= 按对象断点续传，「合并」= 条目级去重补齐（SQL 改写为 INSERT IGNORE / ON CONFLICT DO NOTHING，Mongo 靠重复 `_id` 跳过；Redis 的 list/string 无法条目级去重，不提供合并），「覆盖」= 幂等重建。
3. **事务性**：不承诺整体回滚（MySQL DDL 隐式提交、逐块执行本就无法全局回滚）；按对象容错——某对象失败计入汇总并跳过其余语句，FK/类型/序列等重复对象错误按警告容忍。
4. **生产只读拦截**：`production=true` 的连接禁止导入，UI 入口、服务层、driver 层三层硬拦（导出为只读，允许）。
5. **二次确认**：导入前弹策略选择框（跳过 / 覆盖标红 / 停止），明示目标库与不可逆性。

## 五、driver 接口改动（实际落地）

- **Mongo**：`DocDriver::insert_many`（无序批量，重复 `_id` 计数不报错，供断点续传）。
- **Redis**：`KvDriver::read_value_page`（全量分段读，首页单往返内管道化 TYPE+PTTL 探测）与
  `write_value_items`（分段合并写，二进制安全）——原 `get_value_limited` 是截断预览，
  集合 1 万条 / String 4MiB 上限，导出用它必丢数据，必须新增。
- **SQL**：不改 trait。生成列 / 枚举 / 序列 / FK 全靠 app 层跑目录 SQL
  （information_schema / pg_catalog）；`ConnectionService` 增无历史记录的 `execute`
  （避免导库刷爆查询历史）。
- `build_ddl_query` 从 dbclient 视图层移入 `ramag-domain::entities::ddl` 供导出复用。

## 六、边界与安全（硬约束）

- **流式**：导出边读边写文件、导入逐行读，全程不把整库进内存（SQL 分页游标 / Redis scan 游标 / Mongo find batch）。
- **上限**：单文件大小、单值/单文档字节上限，复用现有 `MAX_*` 常量体系。
- **敏感数据**：导出文件是明文库数据，导出前提示妥善保管（数据量大，不做文件加密，靠放置在可信位置）。
- **取消**：大库耗时，导出/导入均可取消（复用现有 CancelHandle / AtomicBool）。

## 七、UI

- **入口**：左侧库树右键「导出此库」；「导入到此库」放库树右键或工具条。
- **导出**：选库 →（可选）勾选表/collection → 选路径 → 进度条 → 完成通知。
- **导入**：选文件 → 选目标库 + 冲突策略 → 二次确认 → 进度条 → 结果汇总（成功/跳过/失败明细进日志）。

## 八、分期实施

| 期 | 范围 | 复用度 | 风险 |
|---|---|---|---|
| P1 | MySQL/PG `.sql` 导出导入 | 高（DDL + execute + SQL 字面量现成） | 低 |
| P2 | MongoDB JSONL 导出导入 | 中（需加 insert_many） | 中 |
| P3 | Redis JSONL 导出导入 | 中（类型序列化 + RESTORE） | 中 |

先 P1：复用度最高、最快见效，用它验证整套 UI/进度/取消/确认框骨架，后两期沿用。

## 九、拍板结果与实现落点

1. **方案 B（纯 driver）**：零外部依赖；触发器 / 存储过程 / 事件 / 权限不导出，文件头注明。
2. **冲突默认「跳过 + 汇总」**；覆盖在策略选择框中红色标示。
3. **粒度：整库**（勾选部分表有 FK 依赖闭包问题，留待后续）。
4. **P1→P2→P3 一次交付**；三期共用同一套进度 / 取消 / 策略框骨架。
5. **SQL 对象范围**：表 + 视图（MySQL 剥 DEFINER）+ 索引 + FK + 注释 + PG 枚举类型 +
   序列（预建 + OWNED BY + 数据后 setval 续值；`GENERATED ALWAYS AS IDENTITY` 用
   OVERRIDING SYSTEM VALUE 导数）+ 生成列（DDL 保留、INSERT 排除）。
6. **一致性**：导出为非快照一致（无会话级事务抽象），文件头注明；导入取消保留已完成部分。

实现落点：编排在 `ramag-app/src/usecases/transfer/`（sql_export / sql_import /
sql_catalog / mongo / redis）；导出经有界通道 + 阻塞池 `write_atomic_with` 流式落盘，
成功才提交文件；共享 UI 件（进度行 / 取消 / 策略框）在 `ramag-ui/src/transfer_ui.rs`，
三工具树面板各自薄接入（`transfer_ops.rs`）。已知边界：非 UTF-8 的 key 沿用现有
SCAN 行为整批报错；二进制 hash field / stream field 无法用实体表达，跳过并计数进汇总。
