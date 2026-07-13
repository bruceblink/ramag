//! SQL 查询历史弹框与“填入/重跑”事件接线。

use gpui::{AppContext as _, Context, ParentElement, SharedString, Window, px};
use gpui_component::WindowExt as _;

use super::QueryPanel;
use crate::views::history_dialog::{HistoryEvent, HistoryList};

impl QueryPanel {
    /// 打开查询历史弹框：搜索 / 复制 / 填入 / 重跑 / 删除 / 清空。
    pub(super) fn open_history_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(conn) = self.connection.clone() else {
            return;
        };
        let list = cx.new(|cx| HistoryList::new(self.service.clone(), conn.id.clone(), window, cx));
        // 先关弹框再聚焦编辑器，避免焦点恢复覆盖编辑器焦点。
        self.history_sub = Some(cx.subscribe_in(
            &list,
            window,
            |this: &mut Self, _, event: &HistoryEvent, window, cx| match event {
                HistoryEvent::FillEditor(sql) => {
                    window.close_dialog(cx);
                    this.fill_active_sql(sql.clone(), window, cx);
                }
                HistoryEvent::RunSql(sql) => {
                    window.close_dialog(cx);
                    this.fill_active_sql(sql.clone(), window, cx);
                    if let Some(tab) = this.tabs.get(this.active) {
                        tab.update(cx, |tab, cx| tab.run(window, cx));
                    }
                }
            },
        ));

        let title = SharedString::from(format!("查询历史 · {}", conn.name));
        window.open_dialog(cx, move |dialog, _, _| {
            let list = list.clone();
            dialog
                .title(title.clone())
                .close_button(true)
                .width(px(760.0))
                .content(move |content, _, _| content.child(list.clone()))
        });
    }
}
