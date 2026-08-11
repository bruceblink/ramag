use gpui::{Context, IntoElement, ParentElement, Render, Styled, Window, div, prelude::*};
use gpui_component::{ActiveTheme, WindowExt as _, notification::Notification, v_flex};

use super::model::ObjectStorageView;
use crate::RefreshObjectStorage;

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
            .on_action(cx.listener(|this, _: &RefreshObjectStorage, window, cx| {
                if this.selected_mount.is_some() {
                    this.load_first_page(window, cx);
                } else if let Some(id) = this.selected_account_id.clone() {
                    this.load_mounts(id, window, cx);
                } else {
                    this.load_accounts(window, cx);
                }
            }))
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(self.render_sessions(cx))
            .child(div().flex_1().min_h_0().child(body))
    }
}
