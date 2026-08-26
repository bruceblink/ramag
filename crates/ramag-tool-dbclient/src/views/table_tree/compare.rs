//! 表树中的 SQL 表结构对比入口。

use gpui::{AppContext as _, Context, ParentElement, Window, px};
use gpui_component::{WindowExt as _, notification::Notification};
use ramag_domain::entities::{ConnectionConfig, DriverKind, MAX_CONNECTION_IDENTIFIER_BYTES};
use ramag_ui::open_bounded_prompt;

use crate::views::connection_target::{
    SQL_CONNECTION_SEPARATOR, resolve_sql_connection, sql_connection_hint,
};

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

fn parse_compare_request(
    input: &str,
    source_schema: &str,
    source_connection: &ConnectionConfig,
    available_connections: &[ConnectionConfig],
) -> Result<(ConnectionConfig, CompareTableRef), &'static str> {
    let (connection_selector, table_input) = match input.split_once(SQL_CONNECTION_SEPARATOR) {
        Some((selector, table_input)) => (Some(selector.trim()), table_input),
        None => (None, input),
    };
    let target_connection = match connection_selector {
        None => source_connection.clone(),
        Some("") => return Err("目标连接不能为空"),
        Some(selector) => {
            resolve_sql_connection(selector, source_connection, available_connections)?
        }
    };
    let target = parse_compare_target(table_input, source_schema)?;
    Ok((target_connection, target))
}

fn compare_connection_hint(
    source: &ConnectionConfig,
    available_connections: &[ConnectionConfig],
) -> String {
    format!(
        "输入目标表名或 schema.table；跨连接时输入 连接名::schema.table。可用连接：{}",
        sql_connection_hint(source, available_connections),
    )
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
        let available_connections = self.connection_list.read(cx).connections().to_vec();
        let connection_hint = compare_connection_hint(&connection, &available_connections);
        let source_connection = connection.clone();
        let source_schema = schema.clone();
        let tree = cx.entity().clone();
        let window_handle = window.window_handle();
        open_bounded_prompt(
            "比较表结构",
            format!("{connection_hint} 不带 Schema 时使用 {schema}"),
            "",
            "比较",
            MAX_CONNECTION_IDENTIFIER_BYTES,
            move |target_input, window, app| {
                let (target_connection, target) = match parse_compare_request(
                    &target_input,
                    &source_schema,
                    &source_connection,
                    &available_connections,
                ) {
                    Ok(result) => result,
                    Err(message) => {
                        window
                            .push_notification(Notification::warning(message).autohide(true), app);
                        return;
                    }
                };
                if target_connection.id == source_connection.id
                    && target.schema.eq_ignore_ascii_case(&source_schema)
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
                                source_connection.clone(),
                                target_connection,
                                CompareTableRef {
                                    schema: source_schema,
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
        source_connection: ConnectionConfig,
        target_connection: ConnectionConfig,
        source: CompareTableRef,
        target: CompareTableRef,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let panel = cx.new(|cx| {
            crate::views::schema_diff_dialog::SchemaDiffDialog::new(
                self.service.clone(),
                source_connection.clone(),
                target_connection.clone(),
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
                    "比较表结构 · {} / {}.{} → {} / {}.{}",
                    source_connection.name,
                    source.schema,
                    source.table,
                    target_connection.name,
                    target.schema,
                    target.table
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

    fn postgres_connection(name: &str, host: &str) -> ConnectionConfig {
        let mut connection = ConnectionConfig::new_mysql(name, host, 5432, "postgres");
        connection.driver = DriverKind::Postgres;
        connection
    }

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

    #[test]
    fn qualified_request_selects_a_same_driver_connection() {
        let source = ConnectionConfig::new_mysql("source", "mysql-a", 3306, "root");
        let target = ConnectionConfig::new_mysql("archive", "mysql-b", 3306, "root");

        let (selected, table) = parse_compare_request(
            "archive::history.orders",
            "public",
            &source,
            &[source.clone(), target.clone()],
        )
        .expect("target connection should resolve");

        assert_eq!(selected.id, target.id);
        assert_eq!(table.schema, "history");
        assert_eq!(table.table, "orders");
    }

    #[test]
    fn connection_id_can_disambiguate_duplicate_names() {
        let source = postgres_connection("source", "pg-a");
        let first = postgres_connection("same", "pg-b");
        let second = postgres_connection("same", "pg-c");

        assert!(
            parse_compare_request(
                "same::public.orders",
                "public",
                &source,
                &[source.clone(), first.clone(), second.clone()],
            )
            .is_err()
        );

        let (selected, _) = parse_compare_request(
            &format!("{}::orders", second.id),
            "public",
            &source,
            &[source.clone(), first, second.clone()],
        )
        .expect("connection id should resolve");
        assert_eq!(selected.id, second.id);
    }

    #[test]
    fn cross_connection_request_rejects_another_driver() {
        let source = ConnectionConfig::new_mysql("source", "mysql-a", 3306, "root");
        let target = postgres_connection("pg", "pg-a");

        assert_eq!(
            parse_compare_request("pg::public.orders", "public", &source, &[target],),
            Err("找不到同类型的目标连接")
        );
    }
}
