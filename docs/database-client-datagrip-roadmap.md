# 数据库查询工具 DataGrip-like 开发路线图

> 状态：M1 结果数据编辑器、M2-A SQL 查询上下文隔离与 M4-A 原始执行计划结果视图已落地，后续迭代中
> 更新日期：2026-08-23
> 适用范围：`ramag-tool-dbclient`、`ramag-tool-mongodb`、`ramag-domain`、`ramag-app` 及对应基础设施驱动

## 术语表与命名约定

| 规范名称 | English / Acronym | 在本路线图中的职责边界 | 不代表什么 |
|---|---|---|---|
| 数据库查询工具 | Database Client | Ramag 中连接、浏览、查询和编辑数据库的整体产品能力 | 不包含 Git、SSH、对象存储等其他工具 |
| 结果数据编辑器 | Result Data Editor | 展示查询结果、分页、滚动、排序、过滤和数据编辑的界面 | 不代表 SQL 编辑器或数据库表设计器 |
| 查询控制台 | Query Console | 绑定连接与当前 Schema 的 SQL/文档查询会话 | 不代表数据库连接池或后台执行线程 |
| Schema 浏览器 | Schema Explorer | 展示数据库、Schema、表、集合及元数据的对象树 | 不代表数据库元数据缓存本身 |
| 服务端分页 | Server-side Pagination | 通过 `LIMIT/OFFSET`、MongoDB `skip/limit` 等方式只读取当前数据页 | 不代表客户端只隐藏已加载数据 |
| 数据库驱动 | Database Driver | `ramag-domain` 定义的能力接口及 MySQL、PostgreSQL、MongoDB 等具体适配器 | 不强行把 SQL、文档和 Key-Value 能力合并成一个接口 |
| DataGrip-like | DataGrip-like Core Workflow | 参考 DataGrip 的核心数据库浏览、查询、编辑和分析工作流 | 不承诺复制 DataGrip 的全部方言、插件和 IDE 能力 |

## 1. 背景与目标

DataGrip 的参考价值在于完整的数据库工作流，而不是单个 SQL 输入框。官方文档将 Query Console、Database Explorer、数据编辑器和查询结果视图作为相互配合的核心界面：[Query Consoles](https://www.jetbrains.com/help/datagrip/query-consoles.html)、[数据编辑器](https://www.jetbrains.com/help/datagrip/data-editor-and-viewer.html) 和 [用户界面导览](https://www.jetbrains.com/help/datagrip/guided-tour-around-the-user-interface.html)。

Ramag 已经具备多数据库连接、Schema 浏览、查询编辑、结果编辑、导入导出和数据同步等基础能力。本路线图的目标是：

1. 先补齐高频且影响最大的数据库浏览体验，尤其是可见的双轴滚动和可控分页。
2. 建立可扩展的查询会话、事务和查询分析模型，避免继续把能力堆在单个结果面板中。
3. 以 MySQL 和 PostgreSQL 为第一目标，随后分别完善 MongoDB；Redis 保持符合 Key-Value 数据模型的专用交互。
4. 用可验收的阶段目标取代“完全对标 DataGrip”这一不可控的二元目标。

## 2. 当前基线

当前实现已经提供以下基础：

- SQL、MongoDB 和 Redis 的连接入口，以及 TLS、SSH 跳板、连接测试和查询取消能力。
- Schema 浏览器、表/集合元数据、列、索引、外键、DDL 生成和表设计器。
- SQL 补全、格式化、查询历史、草稿、查询标签页和结果面板。
- 结果搜索、列过滤、行过滤、复制、编辑、删除、导出、导入和数据同步。
- MySQL/PostgreSQL 的安全只读查询分页，以及 MongoDB 普通 `find` 的分页和哨兵行处理。
- 结果表已经使用虚拟列表，并在最近提交中补齐了横向滚动和可见滚动条的基础布局。

相关实现入口：

- [`crates/ramag-tool-dbclient/src/views/query_tab/paging.rs`](../crates/ramag-tool-dbclient/src/views/query_tab/paging.rs)
- [`crates/ramag-tool-mongodb/src/views/query_tab/paging.rs`](../crates/ramag-tool-mongodb/src/views/query_tab/paging.rs)
- [`crates/ramag-tool-dbclient/src/views/result_table`](../crates/ramag-tool-dbclient/src/views/result_table)
- [`crates/ramag-tool-mongodb/src/views/result_panel`](../crates/ramag-tool-mongodb/src/views/result_panel)
- [`crates/ramag-domain/src/traits/driver.rs`](../crates/ramag-domain/src/traits/driver.rs)

当前最需要继续处理的问题：

- M1 的真实 Windows 窗口截图仍需在带实际数据的连接上补充验收，确认列表高度、底部状态栏和垂直滚动条不会互相覆盖。
- M2-A 已为 SQL 查询代次和 COUNT 代次增加上下文隔离；多标签并行执行、连接断开和服务端取消仍需继续补充集成验证。
- 服务端分页目前只覆盖可安全识别的查询形状，深分页、服务端排序和服务端过滤仍需要明确边界。
- `Driver` 目前覆盖执行、取消和元数据读取，但没有统一的事务控制、结构化执行计划或会话能力接口。
- 当前已支持独立的原始 EXPLAIN 结果视图；结构化计划树、图形化执行计划、Schema Diagram、数据/结构对比和迁移工作流仍在后续迭代中。

## 3. 产品边界与原则

### 3.1 目标用户工作流

第一目标是开发者和运维人员的高频闭环：

```text
选择连接和 Schema
    -> 打开查询控制台
    -> 编写并执行查询
    -> 在结果数据编辑器中分页、滚动、筛选和编辑
    -> 查看影响范围或执行计划
    -> 提交、回滚或导出结果
```

### 3.2 设计原则

1. **先保证可见性，再追求密度**：结果列不能覆盖，双轴滚动条和当前页状态必须可发现。
2. **默认安全**：默认分页读取有限数据；写入操作明确显示影响行数，并提供提交/回滚边界。
3. **服务端优先**：分页、排序和过滤尽量下推数据库，客户端只处理当前窗口数据。
4. **按数据模型分层**：SQL、文档和 Key-Value 驱动分别演进，不为表面统一而引入大量 `NotImplemented`。
5. **能力逐步增强**：先交付稳定的文本和表格能力，再增加结构化计划、图形和对比功能。
6. **可验证交付**：每个里程碑都必须有单元/集成测试、`git diff --check` 和真实窗口截图或等价 UI 验证。

## 4. 分阶段开发计划

### M1：结果数据编辑器（优先级 P0）

目标：让数据库表和查询结果可以稳定地浏览大量行和大量列。

功能清单：

- 默认 `pageSize=100`。
- 提供 `200`、`500`、`1000` 预设值，以及自定义正整数输入。
- SQL 和 MongoDB 使用各自驱动语义执行服务端分页；用户切换 page size 时回到第 1 页并清理旧页状态。
- 同时提供可见的垂直和水平滚动条；底部分页状态栏不能遮挡最后一行。
- 保留虚拟列表，避免因为降低 page size 而退化为一次性渲染全量结果。
- 保留列宽估算和手动调整；宽列必须能滚动到完整内容。
- 为分页状态、哨兵行、页大小边界和结果表双轴滚动增加回归测试。

验收标准：

1. 查询超过 100 行时，首屏只请求并展示当前页，界面显示“第 1 页”，可以进入下一页。
2. 在 `100/200/500/1000` 和自定义 page size 间切换时，查询结果、页码和状态栏保持一致。
3. 行数超过视口高度时，可以拖动垂直滚动条；列总宽超过视口时，可以拖动水平滚动条。
4. 真实 Windows 窗口截图能确认滚动条、最后一行、分页控件和结果列没有重叠。
5. SQL、MongoDB 的既有分页安全门禁、显式 `LIMIT/OFFSET` 行为和写查询行为不回归。

实现边界：

- 页大小选择器和分页状态属于结果数据编辑器，不放入全局设置第一版。
- 首版只允许有限范围的自定义值，例如 `1..=10000`；超出范围时在输入处提示，不发送请求。
- 暂不为任意 SQL 自动改写带有显式分页、锁定、写入或多语句的查询。

### M2：查询控制台与结果会话（优先级 P1）

目标：让一次数据库连接下的多个查询会话可独立工作，接近 DataGrip 的 Query Console 体验。

功能清单：

- 一个连接下支持多个查询控制台，每个控制台绑定连接和当前 Schema。
- 查询编辑器、执行状态、取消操作、错误信息和结果页签互不覆盖。
- 查询历史和草稿按连接/控制台恢复，切换页签不丢失未提交文本。
- 支持结果标签页保留、关闭和重新打开；长查询执行期间允许切换到其他控制台。
- 记录当前连接、Schema、执行耗时、影响行数和分页状态。

架构要求：

- 在 `ramag-tool-dbclient` 中明确“查询控制台状态”和“结果数据编辑器状态”的所有权。
- 在 `ramag-domain`/`ramag-app` 中只抽象跨驱动都成立的会话语义；连接池、取消句柄等基础设施细节留在具体驱动。
- 查询执行任务必须保留代次或会话标识，旧请求完成后不能覆盖新控制台的结果。

验收标准：

- 两个控制台可以同时绑定同一连接并分别执行查询，结果不会串页或串状态。
- 关闭并重新打开工作区后，已保存草稿和最近查询可以恢复。
- 查询取消、连接断开和切换 Schema 时，不会把旧错误写入当前控制台。

### M3：事务与安全编辑（优先级 P1）

目标：为可编辑结果提供明确、可控、可恢复的写入流程。

功能清单：

- 自动提交/手动提交模式。
- 提交、回滚、当前事务状态和未提交变更提示。
- 编辑、删除、新增前显示关键列、影响范围和风险提示。
- 批量修改失败时显示成功/失败明细，不隐藏数据库返回的错误。
- 根据驱动能力显示事务控制；MongoDB、Redis 不强行复用 SQL 事务 UI。

架构要求：

- 为 SQL 驱动增加最小事务能力接口和隔离级别能力声明，避免在通用 `Driver` 中塞入所有后端专属语义。
- 将提交/回滚状态纳入查询控制台生命周期，而不是仅存在于结果表组件。
- 所有写操作继续复用现有安全门禁、确认和可取消机制。

验收标准：

- 手动提交模式下，编辑后关闭控制台会提示未提交变更。
- 回滚后重新查询，数据与数据库一致；提交后能显示影响行数。
- 连接断开或事务失效时，UI 明确显示状态，不允许误报提交成功。

### M4：查询分析与 Schema 协作（优先级 P2）

目标：补齐高级数据库开发辅助能力，但不阻塞核心查询流程。

#### M4-A：独立执行计划结果视图（已落地）

实现范围：

- 生成 EXPLAIN 时写入独立的执行计划结果面板，不替换数据结果面板中的内容。
- 在“数据结果”和“执行计划”视图之间切换；切回数据结果时保留原查询结果及分页状态。
- 计划失败只更新计划面板，并保留可用的数据结果错误或成功状态。
- 为数据查询和执行计划分别维护执行代次，旧的异步响应不能覆盖当前视图。

对应测试覆盖独立面板渲染、数据/计划切换、计划错误隔离和过期响应保护。

功能清单：

- EXPLAIN 结果先支持原始文本，再增加结构化树并评估图形化执行计划。DataGrip 将 Query Plan 作为独立结果页，并支持树、原始计划、图形和火焰图展示，可作为交互参考：[Query Execution Plan](https://www.jetbrains.com/help/datagrip/query-execution-plan.html)。
- 基于现有表、列、索引和外键元数据生成 Schema Diagram。
- 表结构和 DDL diff，展示变更预览后再执行。
- 数据网格之间的基础差异比较；DataGrip 的数据编辑器也提供网格对比能力，可作为交互参考：[Explore data in the data editor](https://www.jetbrains.com/help/datagrip/explore-data-in-data-editor.html)。
- 为 MySQL/PostgreSQL 增加更精确的方言检查、对象导航和补全；MongoDB 单独设计文档查询辅助。

验收标准：

- EXPLAIN 不影响原查询结果；计划执行失败时保留原始数据库错误。
- Schema Diagram 的节点和边全部来自已加载元数据，不凭 UI 推断关系。
- DDL diff 必须可预览、可复制，执行前必须经过现有写操作确认。

## 5. 架构落地顺序

1. **结果数据编辑器状态**：先抽出统一的 `PageSize`、页状态和边界校验，SQL/MongoDB 通过各自分页器接入。
2. **UI 滚动和分页控件**：在 SQL 和 MongoDB 结果表分别挂载纵向、横向滚动句柄，并复用统一的视觉规范。
3. **应用层查询会话**：为控制台建立独立的执行代次、结果集合和取消状态。
4. **驱动能力声明**：事务、结构化计划等能力按 SQL、文档、Key-Value 分组扩展，不修改无关驱动。
5. **元数据协作能力**：复用现有 Schema、Column、Index、ForeignKey 和 DDL 模型，先做只读预览，再增加执行。

禁止的捷径：

- 不把所有查询结果一次性加载到客户端来“解决”滚动问题。
- 不把默认 page size 仅改成较小常量而不提供用户选择和分页状态重置。
- 不在 `ramag-domain::Driver` 中加入每个数据库都无法实现的统一大接口。
- 不以“支持 DataGrip 全部功能”作为单个 PR 的验收标准。

## 6. 分支、提交与 PR 策略

本路线图对应多个独立交付，不创建一个长期承载全部功能的“大分支”。每个里程碑从最新 `origin/main` 建立独立分支，并在验证通过后立即提交和推送。

### M1 建议拆分

建议分支名：`codex/feat/result-editor-pagination`

建议提交顺序：

1. `fix: keep database result vertical scrolling usable`
2. `feat: add configurable database result page size`
3. `test: cover database result pagination controls`

如果滚动条修复已经在独立 PR 中完成，M1 新分支只包含分页大小、状态重置、SQL/MongoDB 接入和对应测试，避免重复修改已合并或待合并的滚动条代码。

建议 PR 主题：

> feat: improve database result editor pagination

PR 内容应说明：默认页大小、可选页大小、服务端分页边界、SQL/MongoDB 差异、回归测试和真实窗口截图证据。

## 7. 测试与发布门槛

每个功能完成后必须先完成与风险匹配的验证，测试通过后才能提交或推送：

### M1 最低验证集

```powershell
cargo test -p ramag-tool-dbclient
cargo test -p ramag-tool-mongodb
cargo check --workspace --all-targets
git diff --check
```

同时需要真实 Windows 窗口验证：

- 100、1000 和自定义 page size。
- 结果行数超过视口高度时的垂直滚动。
- 列总宽超过视口时的水平滚动。
- 切换页大小、切换查询、切换连接后的状态清理。

### M2/M3/M4 追加门槛

- 增加跨控制台、取消、连接断开和事务状态的异步测试。
- 增加至少一个 MySQL 和一个 PostgreSQL 的真实数据库集成验证。
- 对 MongoDB 的分页和文档编辑单独保留回归测试。
- 涉及图形、窗口布局或执行计划展示时，必须提供真实渲染截图；无法渲染时要在 PR 中明确限制。

## 8. 风险与决策门

| 风险 | 影响 | 应对 |
|---|---|---|
| 多数据库方言差异 | 自动分页、排序、事务和 EXPLAIN 语义不一致 | 先按驱动能力声明和方言适配器拆分 |
| 大结果集内存和 UI 性能 | 查询结果卡顿或占用过多内存 | 服务端分页、虚拟列表、页大小上限和取消机制并存 |
| 结果编辑的主键/并发问题 | 误更新或覆盖他人修改 | 明确身份列、生成条件更新、提交前确认并保留错误明细 |
| 图形化能力投入过大 | 延误核心查询体验 | M4 只读预览，先交付原始/结构化文本 |
| 长期分支漂移 | 难以合并和回滚 | 每个里程碑独立分支、单一功能提交 |

进入下一阶段前需要满足：

1. 上一阶段的验收标准和测试门槛全部通过。
2. 没有已知的分页、滚动、查询取消或数据写入回归。
3. 明确该阶段对应的驱动范围，不把未实现的驱动能力伪装成通用能力。
4. PR 描述包含功能边界、验证命令和截图/集成环境限制。

## 9. 结论

Ramag 应该采用 **“以 DataGrip 为交互参考、以核心工作流为产品边界、以里程碑逐步扩大能力”** 的策略。M1 结果数据编辑器已经完成默认 100 条分页、可选 page size 和可靠的双轴滚动；当前继续收紧 M2 查询控制台的异步上下文边界，再根据真实窗口和集成环境反馈投入查询会话、事务和高级分析能力。
