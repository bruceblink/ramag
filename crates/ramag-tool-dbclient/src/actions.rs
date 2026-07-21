//! DB Client 快捷键 Action。绑定在 ramag-bin/main.rs 的 `cx.bind_keys`

use gpui::Action;
use schemars::JsonSchema;
use serde::Deserialize;

/// 当前 Query Tab 执行 SQL（默认主修饰键+Enter）
#[derive(Clone, Copy, PartialEq, Eq, Debug, Deserialize, JsonSchema, Action)]
#[action(namespace = ramag_dbclient)]
pub struct RunQuery;

/// 仅执行光标所在那条 SQL（按 `;` 切，默认主修饰键+Shift+Enter）
#[derive(Clone, Copy, PartialEq, Eq, Debug, Deserialize, JsonSchema, Action)]
#[action(namespace = ramag_dbclient)]
pub struct RunStatementAtCursor;

/// 新建 Query Tab（默认主修饰键+T）
#[derive(Clone, Copy, PartialEq, Eq, Debug, Deserialize, JsonSchema, Action)]
#[action(namespace = ramag_dbclient)]
pub struct NewQueryTab;

/// 聚焦结果集过滤栏（默认主修饰键+F）
#[derive(Clone, Copy, PartialEq, Eq, Debug, Deserialize, JsonSchema, Action)]
#[action(namespace = ramag_dbclient)]
pub struct FindInResults;

/// 格式化当前 SQL（默认主修饰键+Shift+F）
#[derive(Clone, Copy, PartialEq, Eq, Debug, Deserialize, JsonSchema, Action)]
#[action(namespace = ramag_dbclient)]
pub struct FormatSql;

/// EXPLAIN 当前 SQL（默认主修饰键+Shift+E）
#[derive(Clone, Copy, PartialEq, Eq, Debug, Deserialize, JsonSchema, Action)]
#[action(namespace = ramag_dbclient)]
pub struct ExplainQuery;

/// 右键菜单：复制选中单元格的完整值
#[derive(Clone, Copy, PartialEq, Eq, Debug, Deserialize, JsonSchema, Action)]
#[action(namespace = ramag_dbclient)]
pub struct CopyCellValue;

/// 右键菜单：复制选中单元格所在的列名
#[derive(Clone, Copy, PartialEq, Eq, Debug, Deserialize, JsonSchema, Action)]
#[action(namespace = ramag_dbclient)]
pub struct CopySelectedColumn;

/// 切换 SQL 编辑器显隐（默认主修饰键+E；仅控编辑器，工具条 / 结果保留）
#[derive(Clone, Copy, PartialEq, Eq, Debug, Deserialize, JsonSchema, Action)]
#[action(namespace = ramag_dbclient)]
pub struct ToggleSqlEditor;
