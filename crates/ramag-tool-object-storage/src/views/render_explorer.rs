use gpui::{
    AnyElement, ClickEvent, Context, IntoElement, MouseButton, MouseDownEvent, ParentElement,
    SharedString, Styled, Window, div, prelude::*, px, uniform_list,
};
use gpui_component::{
    ActiveTheme, Disableable as _, Icon, IconName, Sizable as _,
    button::ButtonVariants as _,
    h_flex,
    menu::{ContextMenuExt as _, PopupMenu},
    resizable::{h_resizable, resizable_panel},
    v_flex,
};
use ramag_domain::entities::ObjectEntryKind;

use super::{
    layout::ExplorerLayout,
    model::ObjectStorageView,
    object_list_helpers::{
        OBJECT_ROW_HEIGHT, filtered_object_entry_indices, object_breadcrumbs, object_counts_at,
        object_modified_label, object_size_label, object_type_label,
    },
};

impl ObjectStorageView {
    pub(super) fn selected_read_only(&self) -> bool {
        self.selected_account_id
            .as_ref()
            .and_then(|id| self.accounts.iter().find(|account| &account.id == id))
            .is_none_or(|account| account.read_only)
    }

    fn render_object_breadcrumb(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let parts = object_breadcrumbs(&self.prefix);
        let last = parts.len().saturating_sub(1);
        let link = cx.theme().link;
        let link_hover = cx.theme().link_hover;
        let muted = cx.theme().muted_foreground;
        let mut path_parts = h_flex()
            .id("object-directory-path-scroll")
            .flex_1()
            .min_w_0()
            .gap(px(5.0))
            .overflow_x_scroll();
        for (index, (label, target)) in parts.into_iter().enumerate() {
            if index > 0 {
                path_parts = path_parts.child(
                    div()
                        .flex_none()
                        .text_color(muted)
                        .child(SharedString::from("›")),
                );
            }
            let target_for_click = target.clone();
            path_parts = path_parts.child(
                div()
                    .id(SharedString::from(format!("object-path-part-{index}")))
                    .debug_selector(move || format!("object-path-part-{index}"))
                    .flex_none()
                    .cursor_pointer()
                    .text_color(link)
                    .when(index == last, |part| {
                        part.font_weight(gpui::FontWeight::SEMIBOLD)
                    })
                    .hover(move |part| part.text_color(link_hover))
                    .child(label)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.open_prefix(target_for_click.clone(), window, cx);
                    })),
            );
        }
        h_flex()
            .id("object-directory-path")
            .debug_selector(|| "object-directory-path".into())
            .w_full()
            .h(px(40.0))
            .flex_none()
            .items_center()
            .gap(px(5.0))
            .px(px(10.0))
            .border_b_1()
            .border_color(cx.theme().border)
            .text_xs()
            .child(
                div()
                    .id("object-directory-path-label")
                    .debug_selector(|| "object-directory-path-label".into())
                    .flex_none()
                    .cursor_pointer()
                    .text_color(muted)
                    .hover(move |label| label.text_color(link_hover))
                    .child("路径")
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.prompt_object_path(window, cx);
                    })),
            )
            .child(path_parts)
    }

    fn render_objects(&self, compact: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let border = cx.theme().border;
        let muted = cx.theme().muted_foreground;
        let danger = cx.theme().danger;
        let upload_supported = !self.selected_read_only()
            && self
                .capabilities
                .as_ref()
                .is_some_and(|capabilities| capabilities.write && capabilities.atomic_create);
        let entries = self.entries.clone();
        let query = self.object_filter.read(cx).value().trim().to_lowercase();
        let filtered_indices = filtered_object_entry_indices(&entries, &query);
        let visible_len = filtered_indices
            .as_ref()
            .map_or(entries.len(), |indices| indices.len());
        let total_prefix_count = entries
            .iter()
            .filter(|entry| entry.kind == ObjectEntryKind::Prefix)
            .count();
        let total_object_count = entries.len().saturating_sub(total_prefix_count);
        let (prefix_count, object_count) = filtered_indices
            .as_ref()
            .map_or((total_prefix_count, total_object_count), |indices| {
                object_counts_at(&entries, indices)
            });
        let rows: AnyElement = if entries.is_empty() {
            div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .text_sm()
                .text_color(muted)
                .child(if self.loading {
                    "正在加载对象…"
                } else if self.selected_mount.is_none() {
                    "请选择 Bucket / 挂载点"
                } else {
                    "当前 Prefix 没有对象"
                })
                .into_any_element()
        } else if visible_len == 0 {
            div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .text_sm()
                .text_color(muted)
                .child("暂无匹配")
                .into_any_element()
        } else {
            let entity = cx.entity().clone();
            let filtered_indices_for_rows = filtered_indices.clone();
            uniform_list(
                "object-entry-list",
                visible_len,
                move |range, _window, cx| {
                    range
                        .filter_map(|visible_index| {
                            let index = filtered_indices_for_rows
                                .as_ref()
                                .and_then(|indices| indices.get(visible_index).copied())
                                .unwrap_or(visible_index);
                            let entry = entries.get(index)?.clone();
                            Some(entity.update(cx, |this, cx| {
                                this.render_object_row(entry, compact, border, muted, danger, cx)
                            }))
                        })
                        .collect::<Vec<_>>()
                },
            )
            .flex_1()
            .min_h_0()
            .into_any_element()
        };
        v_flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .border_r_1()
            .border_color(border)
            .child(self.render_object_breadcrumb(cx))
            .child(
                h_flex()
                    .id("object-directory-toolbar")
                    .debug_selector(|| "object-directory-toolbar".into())
                    .flex_none()
                    .h(px(40.0))
                    .items_center()
                    .px(px(6.0))
                    .gap(px(4.0))
                    .bg(cx.theme().secondary)
                    .border_b_1()
                    .border_color(border)
                    .when(compact, |toolbar| {
                        toolbar.child(
                            ramag_ui::clickable_button("object-toggle-mounts")
                                .ghost()
                                .xsmall()
                                .icon(IconName::Folder)
                                .tooltip("Bucket")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.show_mounts = !this.show_mounts;
                                    if this.show_mounts {
                                        this.explorer_resize = cx.new(|_| {
                                            gpui_component::resizable::ResizableState::default()
                                        });
                                        this.show_detail = false;
                                    }
                                    this.persist_workspace(cx);
                                    cx.notify();
                                })),
                        )
                    })
                    .child(
                        div().flex_1().min_w_0().child(
                            ramag_ui::cleanable_input(
                                &self.object_filter,
                                "object-filter-clear",
                                false,
                                cx,
                            )
                            .small()
                            .prefix(Icon::new(IconName::Search).small().text_color(muted)),
                        ),
                    )
                    .child(
                        ramag_ui::clickable_button("object-refresh")
                            .ghost()
                            .xsmall()
                            .icon(ramag_ui::icons::refresh_cw())
                            .tooltip("刷新")
                            .disabled(self.selected_mount.is_none() || self.loading)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.load_first_page(window, cx);
                            })),
                    )
                    .when(upload_supported, |toolbar| {
                        toolbar.child(
                            ramag_ui::clickable_button("object-upload")
                                .ghost()
                                .xsmall()
                                .icon(ramag_ui::icons::upload())
                                .tooltip("上传")
                                .disabled(
                                    self.selected_mount.is_none()
                                        || self.loading
                                        || self.upload_picker_open,
                                )
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.choose_upload(window, cx);
                                })),
                        )
                    }),
            )
            .child(
                h_flex()
                    .id("object-directory-columns")
                    .debug_selector(|| "object-directory-columns".into())
                    .w_full()
                    .h(px(28.0))
                    .flex_none()
                    .items_center()
                    .gap(px(8.0))
                    .px(px(10.0))
                    .border_b_1()
                    .border_color(border)
                    .bg(cx.theme().secondary)
                    .text_xs()
                    .text_color(muted)
                    .child(div().w(px(16.0)).flex_none())
                    .child(div().flex_1().min_w_0().child("名称"))
                    .when(!compact, |header| {
                        header
                            .child(div().w(px(72.0)).child("类型"))
                            .child(div().w(px(90.0)).text_right().child("大小"))
                            .child(div().w(px(160.0)).child("创建时间"))
                            .child(div().w(px(160.0)).child("修改时间"))
                    })
                    .when(compact, |header| {
                        header.child(div().w(px(90.0)).text_right().child("大小"))
                    }),
            )
            .child(rows)
            .child(
                h_flex()
                    .id("object-directory-summary")
                    .debug_selector(|| "object-directory-summary".into())
                    .w_full()
                    .h(px(32.0))
                    .flex_none()
                    .items_center()
                    .px(px(10.0))
                    .border_t_1()
                    .border_color(border)
                    .text_xs()
                    .text_color(muted)
                    .child(if filtered_indices.is_some() {
                        format!(
                            "目录 {prefix_count}/{total_prefix_count} · 对象 {object_count}/{total_object_count}"
                        )
                    } else {
                        format!("目录 {prefix_count} · 对象 {object_count}")
                    })
                    .when(self.next_cursor.is_some(), |summary| {
                        summary.child(
                            div().ml_auto().child(
                                ramag_ui::clickable_button("object-load-more")
                                    .ghost()
                                    .xsmall()
                                    .label("加载更多")
                                    .disabled(self.loading)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.load_next_page(window, cx);
                                    })),
                            ),
                        )
                    }),
            )
    }

    fn render_object_row(
        &self,
        entry: ramag_domain::entities::ObjectEntry,
        compact: bool,
        border: gpui::Hsla,
        muted: gpui::Hsla,
        danger: gpui::Hsla,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let key = entry.key.clone();
        let kind = entry.kind;
        let operable = entry.operable;
        // 详情抽屉打开时，焦点已经转移到抽屉，不继续高亮底层列表行。
        let selected = !self.show_detail && self.selected_key.as_ref() == Some(&key);
        let modified = object_modified_label(
            kind,
            entry
                .last_modified
                .map(|value| value.format("%Y-%m-%d %H:%M:%S").to_string()),
        );
        let size = object_size_label(kind, entry.size, entry.operable);
        let selector = format!("object-entry-{key}");
        let key_for_right_click = key.clone();
        let key_for_copy = key.clone();
        let row = h_flex()
            .id(SharedString::from(selector.clone()))
            .debug_selector(move || selector.clone())
            .w_full()
            .h(px(OBJECT_ROW_HEIGHT))
            .flex_none()
            .items_center()
            .gap(px(8.0))
            .px(px(10.0))
            .border_b_1()
            .border_color(border)
            .when(selected, |row| row.bg(cx.theme().muted))
            .cursor_pointer()
            .when(!self.show_detail, |row| {
                row.hover(|row| row.bg(cx.theme().muted))
            })
            .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                this.select_entry(key.clone(), cx);
                if ramag_ui::is_primary_modifier_double_click(event) {
                    ramag_ui::copy_text(key_for_copy.clone(), cx);
                    return;
                }
                if event.click_count() >= 2 {
                    this.open_entry(key.clone(), kind, operable, window, cx);
                }
            }))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, _, _, cx| {
                    this.select_entry(key_for_right_click.clone(), cx);
                }),
            )
            .child(
                Icon::new(if kind == ObjectEntryKind::Prefix {
                    IconName::Folder
                } else {
                    IconName::File
                })
                .small()
                .text_color(if entry.operable { muted } else { danger }),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .text_sm()
                    .child(entry.display_name),
            )
            .when(!compact, |row| {
                row.child(
                    div()
                        .w(px(72.0))
                        .text_xs()
                        .text_color(muted)
                        .child(object_type_label(kind)),
                )
                .child(
                    div()
                        .w(px(90.0))
                        .text_right()
                        .text_xs()
                        .text_color(if entry.operable { muted } else { danger })
                        .child(size.clone()),
                )
                .child(div().w(px(160.0)).text_xs().text_color(muted).child("—"))
                .child(
                    div()
                        .w(px(160.0))
                        .text_xs()
                        .text_color(muted)
                        .child(modified),
                )
            })
            .when(compact, |row| {
                row.child(
                    div()
                        .w(px(90.0))
                        .text_right()
                        .text_xs()
                        .text_color(if entry.operable { muted } else { danger })
                        .child(size),
                )
            });
        if kind != ObjectEntryKind::Object || !operable {
            return row.into_any_element();
        }
        let entity = cx.entity().clone();
        let menu_key = entry.key;
        let download_supported = self
            .capabilities
            .as_ref()
            .is_some_and(|capabilities| capabilities.read);
        let can_download = download_supported
            && !self.download_picker_open
            && !self.download_active_for_key(&menu_key)
            && !self.loading;
        let delete_supported = !self.selected_read_only()
            && self
                .capabilities
                .as_ref()
                .is_some_and(|capabilities| capabilities.delete);
        let can_delete = delete_supported && !self.loading;
        row.context_menu(move |menu: PopupMenu, _, _| {
            let view_entity = entity.clone();
            let view_key = menu_key.clone();
            let download_entity = entity.clone();
            let delete_entity = entity.clone();
            let download_key = menu_key.clone();
            let delete_key = menu_key.clone();
            let mut menu = menu.item(ramag_ui::menu_item_with_disabled("详情", false).on_click(
                move |_, window, app| {
                    view_entity.update(app, |this, cx| {
                        this.open_object_detail(view_key.clone(), true, window, cx);
                    });
                },
            ));
            if download_supported {
                menu = menu.item(
                    ramag_ui::menu_item_with_disabled("下载", !can_download).on_click(
                        move |_, window, app| {
                            download_entity.update(app, |this, cx| {
                                this.selected_key = Some(download_key.clone());
                                this.choose_download(window, cx);
                            });
                        },
                    ),
                );
            }
            if delete_supported {
                menu = menu.separator().item(
                    ramag_ui::menu_item_with_disabled("删除", !can_delete).on_click(
                        move |_, window, app| {
                            delete_entity.update(app, |this, cx| {
                                this.selected_key = Some(delete_key.clone());
                                this.request_delete_object(window, cx);
                            });
                        },
                    ),
                );
            }
            menu
        })
        .into_any_element()
    }

    pub(super) fn render_explorer(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let layout = ExplorerLayout::resolve(
            f32::from(window.viewport_size().width),
            self.show_mounts,
            self.show_detail,
        );
        let base = if layout.show_mounts {
            h_resizable("object-explorer-resize")
                .with_state(&self.explorer_resize)
                .child(
                    resizable_panel()
                        .size(px(280.0))
                        .size_range(px(200.0)..px(520.0))
                        .child(
                            div()
                                .size_full()
                                .border_r_1()
                                .border_color(cx.theme().border)
                                .child(self.render_mounts(cx)),
                        ),
                )
                .child(
                    resizable_panel().child(
                        div()
                            .size_full()
                            .min_w_0()
                            .child(self.render_objects(layout.compact, cx)),
                    ),
                )
                .into_any_element()
        } else {
            self.render_objects(layout.compact, cx).into_any_element()
        };
        div()
            .size_full()
            .relative()
            .child(base)
            .when(layout.show_detail, |root| {
                root.child(
                    div()
                        .id("object-detail-panel")
                        .debug_selector(|| "object-detail-panel".into())
                        .absolute()
                        .top_0()
                        .bottom_0()
                        .right_0()
                        .when(layout.detail_fullscreen, |panel| panel.left_0())
                        .when(!layout.detail_fullscreen, |panel| panel.w(px(420.0)))
                        .border_l_1()
                        .border_color(cx.theme().border)
                        .bg(cx.theme().background)
                        .shadow_lg()
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_mouse_down_out(cx.listener(|this, _: &MouseDownEvent, _window, cx| {
                            this.show_detail = false;
                            this.persist_workspace(cx);
                            cx.notify();
                        }))
                        .child(self.render_detail(cx)),
                )
            })
            .when(self.transfers_visible, |root| {
                root.child(self.render_transfer_panel(cx))
            })
    }
}

#[cfg(test)]
#[path = "render_explorer_tests.rs"]
mod tests;
