# GPUI 数据展示复制支持方案

> 状态：第一阶段代码已实现并提交；后续扩展与全量交互回归暂缓。
> 记录日期：2026-08-13。
> 适用范围：Ramag 全项目中“用户可读、可复用的数据内容”。

## 1. 背景与结论

GPUI 的 `div().child(text)` 和普通文本节点主要负责绘制，不会自动提供浏览器式的文本选区。因此，界面上“看得见”的文字不等于“可以拖拽选择和复制”的文字。

项目中还存在两类容易混淆的问题：

1. 只读详情使用 `Input.disabled(true)`。禁用输入框会同时关闭焦点、鼠标选择和快捷键处理，因此不能作为只读复制控件。
2. 表格为了性能只显示摘要，例如 MongoDB 的 `{N 字段}`、Redis 的 `List(N elems)`。如果复制显示文本，复制结果会丢失完整数据；嵌套对象、数组、二进制值也会进一步放大这个问题。

本次采用的统一结论是：

- 普通双击继续保留原有语义，例如编辑、下钻、打开文件。
- 使用“主修饰键 + 左键双击”复制完整逻辑值：macOS 为 `Command`，Windows/Linux 为 `Ctrl`。
- 长文本和只读详情使用可拖拽选择的文本组件，支持 `Command/Ctrl-C`。
- 结构化值复制数据源本身，不复制表格摘要、截断预览或格式化标签。
- 不设置全局鼠标拦截器。复制行为必须挂在具体的数据内容节点上，避免破坏按钮、编辑器、树节点和已有双击操作。

## 2. 统一交互协议

公共实现位于 `crates/ramag-ui/src/copy_support.rs`，通过 `ramag-ui` 导出。

### 2.1 主修饰键双击

```rust
ramag_ui::is_primary_modifier_double_click(event)
```

该判断同时满足：

- 标准左键点击；
- 点击次数至少为两次；
- GPUI 的平台主修饰键 `Modifiers::secondary()` 被按下。

GPUI 已经将 `secondary` 映射为 macOS 的 Command、其他平台的 Ctrl，因此业务代码不需要写平台分支。

接入规则：

```rust
if ramag_ui::is_primary_modifier_double_click(event) {
    ramag_ui::copy_text(value, cx);
    return;
}
// 原来的普通双击逻辑继续执行
```

`return` 很重要：它保证复制双击不会继续进入编辑或下钻分支；没有主修饰键的普通双击则完全保持原行为。

### 2.2 只读文本选择

```rust
ramag_ui::SelectableText::new("stable-id", text)
```

该组件复用 GPUI Component 的 `TextView` 选区、拖拽和 `Command/Ctrl-C` 支持。内部将内容放入动态长度的 fenced code block，避免原始内容中的 Markdown 标记、链接、星号、反引号等被富文本解析改写。

适用场景：

- 多行文本详情；
- 日志、剪贴板正文、文件列表；
- 只读 JSON / 文档详情。

不适用场景：

- 可编辑输入框；
- 按钮、标签、状态统计、导航路径；
- 本身已经具有编辑和选择能力的 Code Editor。

### 2.3 显式复制按钮

对于“复制完整对象”“复制当前文件”“复制当前视图”等明确动作，使用现有的 `gpui_component::clipboard::Clipboard`。大内容通过 `value_fn` 在点击时生成，避免每次 Render 都构造完整字符串。

## 3. 全项目接入范围

| 模块 | 当前复制入口 | 复制语义 | 普通双击是否保留 |
|---|---|---|---|
| SQL 结果表 | 主修饰键 + 双击；已有右键菜单 | 使用现有 `Value::to_clipboard_string()`，复制完整单元格值 | 是，继续打开编辑器 |
| MongoDB 结果表 | 主修饰键 + 双击；单元格详情可拖拽选择 | 对象/数组复制完整 pretty JSON；标量复制逻辑值；二进制保留完整 Extended JSON | 是，继续编辑或下钻 |
| MongoDB 下钻表 | 同上 | 使用完整点分路径读取值，不使用摘要；兼容字段名本身含点的情况 | 是 |
| Redis Key 详情 | Header 的“复制完整值”按钮 | 标量、List、Hash、Set、ZSet、Stream、Array 都支持；复合值递归序列化 | 不改变原有编辑/删除双击 |
| Redis 标量 | 主修饰键 + 双击；“复制当前视图”按钮 | 复制当前 Raw/JSON/Hex/Base64 展示文本 | 是，继续编辑文本 |
| Redis 容器行 | 主修饰键 + 双击；Array 行按钮 | 复制完整成员/字段值，不复制预览摘要 | 是，继续编辑 score/字段等 |
| 剪贴板详情 | 文本和文件列表可拖拽选择 | 复制当前展示内容；已有卡片复制仍使用完整服务逻辑 | 是，卡片双击仍复制条目 |
| VCS Diff | 行内容主修饰键 + 双击；文件头“复制完整 Diff” | 复制原始代码行或完整 unified diff，包含全部 hunk | 是，行号/blame/操作不受影响 |
| VCS Project Files | 文件头“复制当前文件内容”；Markdown 预览可选择 | 优先复制当前编辑器正文，否则复制已加载快照 | 是，编辑器行为不变 |
| SSH/SFTP 文件预览 | “复制文件内容/当前片段”按钮 | 普通文件复制当前已加载内容；窗口化预览复制当前片段 | 是，编辑/搜索/翻页不变 |
| 对象存储 | 对象预览复制按钮；详情值主修饰键 + 双击；对象列表 key 主修饰键 + 双击 | 复制当前已加载的对象内容、元数据值或对象 key | 是，目录打开和详情行为不变 |

项目中原来已有的复制入口继续保留，包括：SQL/Mongo 查询历史、VCS SHA/提交说明/错误、连接测试错误、剪贴板卡片、错误详情等。它们不需要被统一鼠标行为替换。

## 4. 嵌套数据类型策略

### 4.1 SQL

SQL 结果已经有领域值到剪贴板文本的转换逻辑。UI 层只负责调用，不根据显示文本重新推断类型，避免数字、NULL、二进制和 JSON 在 UI 层被误处理。

### 4.2 MongoDB

核心函数：

```rust
clipboard_text_for_value(&serde_json::Value)
value_at_path(&serde_json::Value, path)
```

读取路径时先尝试完整字段名，再按 `.` 分段向下遍历。这样同时兼容：

- `profile.name` 这种真正的嵌套路径；
- `a.b` 这种字段名本身包含点的历史数据。

复制规则：

- `Object` / `Array`：完整 pretty JSON；
- 普通标量：复制实际值；
- Extended JSON 标量：复制用户可读值；
- `$binary`：不能复制表格摘要，保留完整 base64 和 subtype JSON；
- 详情弹窗：使用可选择文本，不再使用禁用 Input。

钻取读取、单元格双击查看和主修饰键复制共用同一套路径解析，避免“能下钻但复制不到”或“复制的是摘要”的分叉行为。

### 4.3 Redis

核心函数：

```rust
RedisValue::to_clipboard_string()
```

根标量保留客户端常见语义：文本为原文、字节为十六进制、数字/布尔为字符串、Nil 为空字符串。复合值递归转换为 JSON：

- List / Set / Array → JSON 数组；
- Hash → JSON 对象；
- ZSet → `[{"member": ..., "score": ...}]`；
- Stream → `[{"id": ..., "fields": {...}}]`；
- 嵌套 Bytes → `{ "$bytes": "..." }`，避免与普通字符串混淆；
- 非法 JSON 浮点值保留为字符串，避免静默丢失。

这种策略的重点不是强行模拟 Redis 协议，而是保证不同类型嵌套时数据边界不丢失、复制结果可读且可继续处理。

## 5. 安全上限与“完整值”的边界

“完整值”指当前 UI 已经成功加载到内存中的完整逻辑值，不承诺绕过后端读取上限：

- Redis 超大字符串/集合只加载前缀或前若干元素时，复制的是当前加载内容；界面继续显示已有的部分加载警告。
- SSH/SFTP 窗口化文件只加载当前片段时，按钮名称明确为“复制当前片段”。
- 对象存储预览受文本预览上限约束，复制按钮复制当前预览内容；后续如需全量复制，应新增显式下载/全量读取动作。
- 剪贴板详情的展示可能有大小上限，但原有“复制和粘贴仍使用完整内容”路径保持不变。

不能把未从后端读取的字节伪装成已复制，否则会造成数据完整性误判。

## 6. 已完成代码与测试

本阶段新增公共复制能力，并接入 SQL、MongoDB、Redis、剪贴板、VCS、SSH 文件预览和对象存储高频数据展示。

已覆盖的核心测试：

- `ramag-ui`：拖拽选择 + `Command/Ctrl-C`，并验证 Markdown 特殊字符复制仍为原文；
- `ramag-domain`：Redis 根标量和多层复合值复制；
- `ramag-tool-mongodb`：嵌套对象、Extended JSON 二进制、点分路径和点字段；
- `ramag-tool-vcs`：完整 Diff 的单文件头、全部 hunk、路径安全和大小预算。

已执行过的代表性命令：

```text
cargo test -p ramag-ui copy_support_tests -- --nocapture
cargo test -p ramag-domain redis_value -- --nocapture
cargo test -p ramag-tool-mongodb cell -- --nocapture
cargo test -p ramag-tool-vcs vcs_view_ops_patch -- --nocapture
cargo check -p ramag-tool-mongodb -p ramag-tool-object-storage -p ramag-tool-ssh -p ramag-tool-vcs -p ramag-ui
```

当前工作树的改动尚未完成全工作区 `make check / make clippy / make test` 门禁；本次提交前会至少执行格式检查、差异检查和全目标编译检查，后续恢复任务时再补齐完整质量门禁。

## 7. 暂缓后的恢复顺序

恢复开发时按以下顺序推进：

1. 先执行 `make fmt-check`、`make check`、`make clippy`、`make test`，处理编译、测试和 Clippy 问题。
2. 在真实窗口中逐模块验证：普通双击、主修饰键双击、拖拽选择、`Command/Ctrl-C`、按钮复制五条路径。
3. 补充 SQL/Mongo/Redis 的视觉交互测试，重点验证复制分支不会触发编辑或下钻。
4. 统一复制成功提示和失败处理，避免不同模块提示风格分散。
5. 评估是否为 Redis、SSH、对象存储增加“加载完整内容后复制”的显式动作；这会涉及网络、内存和用户等待时间，需单独确认产品语义。
6. 继续盘点剩余数据型展示，只接入真正需要复用的内容，不给标签、导航、计数器和控制按钮强行增加文本选区。

## 8. 本阶段明确不做的事情

- 不改变普通双击的既有业务含义；
- 不引入新的依赖；
- 不把所有 UI 文本全局改成可选择；
- 不在复制动作中自动发起未授权的全量网络读取；
- 不重构与复制无关的编辑器、表格或数据加载架构。
