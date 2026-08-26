//! 表树中的 SQL 表结构对比入口。

use gpui::{AppContext as _, Context, ParentElement, Window, px};
use gpui_component::{WindowExt as _, notification::Notification};
use ramag_domain::entities::{ConnectionConfig, DriverKind, MAX_CONNECTION_IDENTIFIER_BYTES};
use ramag_ui::open_bounded_prompt;

use super::TableTreePanel;

#[derive(Debug, PartialEq, Eq)]
struct CompareTableRef {
    schema: String,
    table: String,
}

fn parse_compare_target(input: &str, source_schema: &str) -> Result<CompareTableRef, &'static str> {
    let input = input.trim();
    let (schema, table) = match input.split_once('.') {
        None => (source_schema, input),
        Some((schema, table)) if schema.contains('.') || table.contains('.') => {
            return Err("目标只能填写表名或 schema.table");
        }
        Some((schema, table)) => (schema.trim(), table.trim()),
    };
    if schema.is_empty() || table.is_empty() {
        return Err("目标 Schema 和表名不能为空");
    }
    if schema.len() > MAX_CONNECTION_IDENTIFIER_BYTES
        || table.len() > MAX_CONNECTION_IDENTIFIER_BYTES
    {
        return Err("目标 Schema 或表名超过长度限制");
    }
    if schema.chars().any(char::is_control) || table.chars().any(char::is_control) {
        return Err("目标 Schema 或表名不能包含控制字符");
    }
    Ok(CompareTableRef {
        schema: schema.to_string(),
        table: table.to_string(),
    })
}

impl TableTreePanel {
    /// 校验驱动后收集目标表引用，并在输入弹窗关闭后打开只读结构对比视图。
    pub(super) fn prompt_compare_table(
        &mut self,
        schema: String,
        source_table: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(connection) = self.connection.clone() else {
            return;
        };
        if !matches!(connection.driver, DriverKind::Mysql | DriverKind::Postgres) {
            self.pending_notification =
                Some(Notification::warning("当前数据库暂不支持表结构对比").autohide(true));
            cx.notify();
            return;
        }
        let tree = cx.entity().clone();
        let window_handle = window.window_handle();
        open_bounded_prompt(
            "比较表结构",
            format!("输入目标表名，或输入 schema.table；不带 Schema 时使用 {schema}"),
            "",
            "比较",
            MAX_CONNECTION_IDENTIFIER_BYTES,
            move |target_input, window, app| {
                let target = match parse_compare_target(&target_input, &schema) {
                    Ok(target) => target,
                    Err(message) => {
                        window
                            .push_notification(Notification::warning(message).autohide(true), app);
                        return;
                    }
                };
                if target.schema.eq_ignore_ascii_case(&schema)
                    && target.table.eq_ignore_ascii_case(&source_table)
                {
                    window.push_notification(
                        Notification::warning("源表和目标表不能相同").autohide(true),
                        app,
                    );
                    return;
                }
                // 输入弹窗的确认处理器返回后会关闭当前弹窗；延后打开对比弹窗，避免关闭动作误伤新弹窗。
                app.defer(move |app| {
                    let _ = window_handle.update(app, |_, window, app| {
                        tree.update(app, |this, cx| {
                            this.open_schema_diff_dialog(
                                connection,
                                CompareTableRef {
                                    schema,
                                    table: source_table,
                                },
                                target,
                                window,
                                cx,
                            )
                        });
                    });
                });
            },
            window,
            cx,
        );
    }

    /// 创建对比面板；面板随后异步读取两张表的元数据并负责渲染滚动内容。
    fn open_schema_diff_dialog(
        &mut self,
        connection: ConnectionConfig,
        source: CompareTableRef,
        target: CompareTableRef,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let panel = cx.new(|cx| {
            crate::views::schema_diff_dialog::SchemaDiffDialog::new(
                self.service.clone(),
                connection,
                source.schema.clone(),
                source.table.clone(),
                target.schema.clone(),
                target.table.clone(),
                cx,
            )
        });
        window.open_dialog(cx, move |dialog, _, _| {
            let panel_for_content = panel.clone();
            dialog
                .title(format!(
                    "比较表结构 · {}.{} → {}.{}",
                    source.schema, source.table, target.schema, target.table
                ))
                .width(px(1120.0))
                .margin_top(px(55.0))
                .content(move |content, _, _| content.child(panel_for_content.clone()))
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unqualified_target_uses_source_schema() {
        assert_eq!(
            parse_compare_target("  orders  ", "public"),
            Ok(CompareTableRef {
                schema: "public".into(),
                table: "orders".into(),
            })
        );
    }

    #[test]
    fn qualified_target_can_cross_schema() {
        assert_eq!(
            parse_compare_target(" archive . orders ", "public"),
            Ok(CompareTableRef {
                schema: "archive".into(),
                table: "orders".into(),
            })
        );
    }

    #[test]
    fn malformed_target_is_rejected() {
        for input in [
            "",
            ".orders",
            "archive.",
            "db.archive.orders",
            "archive.orders.more",
        ] {
            assert!(parse_compare_target(input, "public").is_err(), "{input:?}");
        }
    }

    #[test]
    fn control_characters_are_rejected() {
        assert!(parse_compare_target("archive.ord\ners", "public").is_err());
    }
}
