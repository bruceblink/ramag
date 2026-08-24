//! 表树中的同 Schema 结构对比入口。

use gpui::{AppContext as _, Context, ParentElement, Window, px};
use gpui_component::{WindowExt as _, notification::Notification};
use ramag_domain::entities::{ConnectionConfig, DriverKind, MAX_CONNECTION_IDENTIFIER_BYTES};
use ramag_ui::open_bounded_prompt;

use super::TableTreePanel;

impl TableTreePanel {
    /// 校验驱动后收集目标表名，并在输入弹窗关闭后打开只读结构对比视图。
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
            format!("输入 {schema} 下与 {source_table} 对比的目标表名"),
            "",
            "比较",
            MAX_CONNECTION_IDENTIFIER_BYTES,
            move |target_table, window, app| {
                let target_table = target_table.trim().to_string();
                if target_table.eq_ignore_ascii_case(&source_table) {
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
                                schema,
                                source_table,
                                target_table,
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
        schema: String,
        source_table: String,
        target_table: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let panel = cx.new(|cx| {
            crate::views::schema_diff_dialog::SchemaDiffDialog::new(
                self.service.clone(),
                connection,
                schema.clone(),
                source_table.clone(),
                target_table.clone(),
                cx,
            )
        });
        window.open_dialog(cx, move |dialog, _, _| {
            let panel_for_content = panel.clone();
            dialog
                .title(format!("比较表结构 · {} → {}", source_table, target_table))
                .width(px(1120.0))
                .margin_top(px(55.0))
                .content(move |content, _, _| content.child(panel_for_content.clone()))
        });
    }
}
