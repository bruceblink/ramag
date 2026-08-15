use gpui::{Context, IntoElement, ParentElement, Render, Styled, Window, div, prelude::*};
use gpui_component::{ActiveTheme, WindowExt as _, notification::Notification, v_flex};

use super::model::ObjectStorageView;

impl Render for ObjectStorageView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some((message, error)) = self.notice.take() {
            let notification = if error {
                Notification::error(message)
            } else {
                Notification::success(message)
            };
            window.push_notification(notification, cx);
        }
        let body = if self.management_visible {
            self.render_accounts(window, cx).into_any_element()
        } else {
            self.render_explorer(window, cx).into_any_element()
        };
        v_flex()
            .key_context("ObjectStorageView")
            .track_focus(&self.focus_handle)
            .on_action(
                cx.listener(|this, _: &ramag_ui::OpenRecentItems, window, cx| {
                    if !this.management_visible {
                        let items = this
                            .accounts
                            .iter()
                            .map(|account| {
                                let bucket_count = account.manual_buckets.len();
                                let mut item = ramag_ui::recent_items_dialog::RecentItem::new(
                                    account.id.to_string(),
                                    account.name.clone(),
                                    format!(
                                        "{} · {bucket_count} 个 Bucket",
                                        account.provider.display_name()
                                    ),
                                    gpui_component::IconName::HardDrive,
                                )
                                .secondary(format!("账号 ID：{}", account.id))
                                .current(this.selected_account_id.as_ref() == Some(&account.id));
                                if account.read_only {
                                    item = item.badge("只读");
                                }
                                item
                            })
                            .collect();
                        let view = cx.entity().clone();
                        ramag_ui::recent_items_dialog::open_recent_item_picker(
                            window,
                            cx,
                            "最近打开的对象存储账号",
                            "搜索账号名称、云厂商或账号 ID",
                            "object_storage_recent_picker_favorites",
                            items,
                            std::sync::Arc::new(move |id, window, app| {
                                let account_id = view
                                    .read(app)
                                    .accounts
                                    .iter()
                                    .find(|account| account.id.to_string() == id)
                                    .map(|account| account.id.clone());
                                if let Some(id) = account_id {
                                    view.update(app, |this, cx| {
                                        this.select_account(id, window, cx)
                                    });
                                }
                            }),
                        );
                        cx.stop_propagation();
                    } else {
                        cx.propagate();
                    }
                }),
            )
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(self.render_sessions(cx))
            .child(div().flex_1().min_h_0().child(body))
    }
}
