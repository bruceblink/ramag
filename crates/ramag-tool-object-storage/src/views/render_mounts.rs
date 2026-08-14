//! 对象存储工作区的 Bucket 导航。

use gpui::{
    AnyElement, Context, IntoElement, ParentElement, SharedString, StatefulInteractiveElement as _,
    Styled, div, prelude::*, px,
};
use gpui_component::{
    ActiveTheme, Disableable as _, Icon, IconName, Selectable as _, Sizable as _, StyledExt as _,
    button::ButtonVariants as _, h_flex, v_flex,
};

use super::model::ObjectStorageView;

impl ObjectStorageView {
    pub(super) fn render_mounts(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let border = cx.theme().border;
        let selected_bg = cx.theme().muted;
        let muted = cx.theme().muted_foreground;
        let query = self
            .mount_search
            .read(cx)
            .value()
            .to_string()
            .to_lowercase();
        let mut rows = v_flex().w_full();
        let mut mounts: Vec<_> = self
            .mounts
            .iter()
            .filter(|mount| {
                query.is_empty()
                    || mount.bucket.to_lowercase().contains(&query)
                    || mount.region.to_lowercase().contains(&query)
                    || mount
                        .root_prefix
                        .as_deref()
                        .is_some_and(|prefix| prefix.to_lowercase().contains(&query))
            })
            .collect();
        mounts.sort_by(|left, right| {
            (&left.region, &left.bucket, &left.root_prefix).cmp(&(
                &right.region,
                &right.bucket,
                &right.root_prefix,
            ))
        });
        if mounts.is_empty() {
            rows = rows.child(
                div()
                    .w_full()
                    .py(px(48.0))
                    .px(px(16.0))
                    .text_center()
                    .text_xs()
                    .text_color(muted)
                    .child(if query.is_empty() {
                        "暂无 Bucket，请编辑账号并添加 Bucket"
                    } else {
                        "暂无匹配"
                    }),
            );
        }
        let mut last_region: Option<&str> = None;
        for mount in mounts {
            if last_region != Some(mount.region.as_str()) {
                last_region = Some(&mount.region);
                rows = rows.child(mount_section(mount.region.clone(), muted));
            }
            let selected = self
                .selected_mount
                .as_ref()
                .is_some_and(|value| value.id == mount.id);
            let target = mount.clone();
            rows = rows.child(
                h_flex()
                    .id(SharedString::from(format!("object-mount-{}", mount.id)))
                    .w_full()
                    .h(px(36.0))
                    .items_center()
                    .gap(px(8.0))
                    .px(px(10.0))
                    .cursor_pointer()
                    .when(selected, |row| row.bg(selected_bg))
                    .hover(|row| row.bg(selected_bg))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.select_mount(target.clone(), window, cx);
                    }))
                    .child(Icon::new(IconName::HardDrive).small().text_color(muted))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .text_sm()
                            .child(mount.bucket.clone()),
                    )
                    .when_some(mount.root_prefix.clone(), |row, prefix| {
                        row.child(
                            div()
                                .max_w(px(100.0))
                                .overflow_hidden()
                                .text_ellipsis()
                                .text_xs()
                                .text_color(muted)
                                .child(format!("/{prefix}")),
                        )
                    }),
            );
        }
        let summary = format!(
            "Bucket {} · 收藏 {}",
            self.mounts.len(),
            self.favorites.len()
        );
        v_flex()
            .size_full()
            .child(
                h_flex()
                    .id("object-mount-toolbar")
                    .debug_selector(|| "object-mount-toolbar".into())
                    .w_full()
                    .h(px(40.0))
                    .flex_none()
                    .items_center()
                    .justify_between()
                    .gap(px(4.0))
                    .px(px(6.0))
                    .bg(cx.theme().secondary)
                    .border_b_1()
                    .border_color(border)
                    .child(
                        div().flex_1().min_w_0().child(
                            ramag_ui::cleanable_input(
                                &self.mount_search,
                                "object-mount-search-clear",
                                false,
                                cx,
                            )
                            .small()
                            .prefix(Icon::new(IconName::Search).small().text_color(muted)),
                        ),
                    )
                    .child(
                        ramag_ui::clickable_button("refresh-object-mounts")
                            .ghost()
                            .xsmall()
                            .icon(ramag_ui::icons::refresh_cw())
                            .tooltip("刷新 Bucket")
                            .disabled(self.selected_account_id.is_none() || self.loading)
                            .on_click(cx.listener(|this, _, window, cx| {
                                if let Some(id) = this.selected_account_id.clone() {
                                    this.load_mounts(id, window, cx);
                                }
                            })),
                    )
                    .child(
                        div()
                            .id("object-transfers")
                            .debug_selector(|| "object-transfers".into())
                            .flex_none()
                            .child(
                                ramag_ui::clickable_button("object-transfers-button")
                                    .ghost()
                                    .xsmall()
                                    .icon(ramag_ui::icons::arrow_up_down())
                                    .tooltip("传输")
                                    .selected(self.transfers_visible)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.transfers_visible = !this.transfers_visible;
                                        if this.transfers_visible {
                                            this.show_detail = false;
                                            this.persist_workspace(cx);
                                        }
                                        cx.notify();
                                    })),
                            ),
                    ),
            )
            .child(
                div()
                    .id("object-mounts-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(rows),
            )
            .child(
                h_flex()
                    .id("object-mount-summary")
                    .debug_selector(|| "object-mount-summary".into())
                    .w_full()
                    .h(px(32.0))
                    .flex_none()
                    .items_center()
                    .px(px(10.0))
                    .border_t_1()
                    .border_color(border)
                    .text_xs()
                    .text_color(muted)
                    .child(summary),
            )
    }
}

fn mount_section(label: impl Into<SharedString>, color: gpui::Hsla) -> AnyElement {
    div()
        .w_full()
        .h(px(28.0))
        .flex()
        .items_center()
        .px(px(10.0))
        .text_xs()
        .font_semibold()
        .text_color(color)
        .child(label.into())
        .into_any_element()
}
