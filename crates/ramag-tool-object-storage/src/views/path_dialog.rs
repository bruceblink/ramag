//! 对象存储路径直达与常用路径管理，交互与 SSH 路径窗口保持一致。

use gpui::{
    App, AppContext as _, ClickEvent, Context, Entity, IntoElement, ParentElement, Render, Styled,
    Subscription, Window, div, prelude::*, px,
};
use gpui_component::{
    ActiveTheme, IconName, Sizable as _, WindowExt as _,
    button::ButtonVariants as _,
    h_flex,
    input::{Input, InputEvent, InputState},
    v_flex,
};
use ramag_domain::entities::{
    MAX_OBJECT_STORAGE_KEY_BYTES, ObjectStorageFavorite, ObjectStorageMountId,
};

use super::{ObjectStorageView, object_helpers::normalize_object_path};

pub(super) fn open_object_path_dialog(
    owner: Entity<ObjectStorageView>,
    mount_id: ObjectStorageMountId,
    initial: String,
    favorites: Vec<ObjectStorageFavorite>,
    window: &mut Window,
    cx: &mut App,
) {
    let form = cx.new(|cx| {
        ObjectPathDialog::new(
            owner.clone(),
            mount_id.clone(),
            initial.clone(),
            favorites.clone(),
            window,
            cx,
        )
    });
    window.open_dialog(cx, move |dialog, _, _| {
        let form_for_content = form.clone();
        dialog
            .title(ramag_ui::closable_dialog_title(
                "object-path-dialog-close",
                "直达",
                |_, _| {},
            ))
            .close_button(false)
            .width(px(520.0))
            .margin_top(px(160.0))
            .on_ok({
                let form = form.clone();
                move |_, window, app| form.update(app, |this, cx| this.open_current(window, cx))
            })
            .content(move |content, _, _| content.child(form_for_content.clone()))
    });
}

struct ObjectPathDialog {
    owner: Entity<ObjectStorageView>,
    mount_id: ObjectStorageMountId,
    input: Entity<InputState>,
    favorites: Vec<ObjectStorageFavorite>,
    error: Option<String>,
    _input_subscription: Subscription,
}

impl ObjectPathDialog {
    fn new(
        owner: Entity<ObjectStorageView>,
        mount_id: ObjectStorageMountId,
        initial: String,
        favorites: Vec<ObjectStorageFavorite>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .validate(|value, _| value.len() <= MAX_OBJECT_STORAGE_KEY_BYTES + 1)
        });
        input.update(cx, |state, cx| {
            state.set_value(initial, window, cx);
            state.focus(window, cx);
        });
        let input_subscription =
            cx.subscribe_in(&input, window, |this, _, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change) {
                    this.error = None;
                    cx.notify();
                }
            });
        Self {
            owner,
            mount_id,
            input,
            favorites,
            error: None,
            _input_subscription: input_subscription,
        }
    }

    fn current_path(&self, cx: &App) -> String {
        self.input.read(cx).value().trim().to_string()
    }

    fn open_current(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let path = self.current_path(cx);
        self.open_path(path, window, cx)
    }

    fn open_path(&mut self, path: String, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let prefix = match normalize_object_path(&path) {
            Ok(prefix) => prefix,
            Err(error) => {
                self.error = Some(error);
                cx.notify();
                return false;
            }
        };
        let same_mount = self
            .owner
            .read(cx)
            .selected_mount
            .as_ref()
            .is_some_and(|mount| mount.id == self.mount_id);
        if !same_mount {
            self.error = Some("挂载点已切换，请重新打开路径窗口".into());
            cx.notify();
            return false;
        }
        self.owner
            .update(cx, |this, cx| this.open_prefix(prefix, window, cx));
        true
    }

    fn favorite_current(&mut self, cx: &mut Context<Self>) {
        let path = self.current_path(cx);
        let prefix = match normalize_object_path(&path) {
            Ok(prefix) => prefix,
            Err(error) => {
                self.error = Some(error);
                cx.notify();
                return;
            }
        };
        let mount_id = self.mount_id.clone();
        match self.owner.update(cx, |this, cx| {
            this.add_path_favorite(mount_id, prefix.clone(), cx)
        }) {
            Ok(true) => {
                let Some(account_id) = self.owner.read(cx).selected_account_id.clone() else {
                    self.error = Some("当前账号已关闭".into());
                    cx.notify();
                    return;
                };
                self.favorites.push(ObjectStorageFavorite {
                    account_id,
                    mount_id: self.mount_id.clone(),
                    prefix,
                });
                self.error = None;
                cx.notify();
            }
            Ok(false) => {}
            Err(error) => {
                self.error = Some(error);
                cx.notify();
            }
        }
    }

    fn remove_favorite(&mut self, prefix: &str, cx: &mut Context<Self>) {
        let removed = self.owner.update(cx, |this, cx| {
            this.remove_path_favorite(&self.mount_id, prefix, cx)
        });
        if removed {
            self.favorites
                .retain(|favorite| favorite.prefix.as_str() != prefix);
            cx.notify();
        }
    }
}

impl Render for ObjectPathDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let border = cx.theme().border;
        let mut content = v_flex()
            .id("object-path-dialog")
            .debug_selector(|| "object-path-dialog".into())
            .w_full()
            .gap(px(10.0))
            .child(div().text_sm().text_color(muted).child("路径"))
            .child(
                h_flex()
                    .w_full()
                    .items_center()
                    .gap(px(8.0))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(Input::new(&self.input).small()),
                    )
                    .child(
                        ramag_ui::clickable_button("object-path-dialog-open")
                            .primary()
                            .small()
                            .label("打开")
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                if this.open_current(window, cx) {
                                    window.close_dialog(cx);
                                }
                            })),
                    )
                    .child(
                        ramag_ui::clickable_button("object-path-dialog-favorite")
                            .outline()
                            .small()
                            .icon(IconName::Star)
                            .tooltip("收藏当前路径")
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.favorite_current(cx);
                            })),
                    ),
            );

        if !self.favorites.is_empty() {
            let mut list = v_flex()
                .id("object-path-favorites")
                .w_full()
                .max_h(px(180.0))
                .overflow_y_scroll()
                .border_1()
                .border_color(border)
                .rounded(px(6.0));
            for (index, favorite) in self.favorites.clone().into_iter().enumerate() {
                let path = format!("/{}", favorite.prefix);
                let path_for_open = path.clone();
                let prefix_for_remove = favorite.prefix.clone();
                list = list.child(
                    h_flex()
                        .w_full()
                        .h(px(34.0))
                        .items_center()
                        .gap(px(4.0))
                        .px(px(6.0))
                        .when(index > 0, |row| row.border_t_1().border_color(border))
                        .child(
                            div()
                                .id(("object-path-favorite-open", index))
                                .flex_1()
                                .min_w_0()
                                .h_full()
                                .flex()
                                .items_center()
                                .px(px(4.0))
                                .text_sm()
                                .overflow_hidden()
                                .text_ellipsis()
                                .cursor_pointer()
                                .hover(|row| row.bg(cx.theme().muted))
                                .child(path)
                                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                    if this.open_path(path_for_open.clone(), window, cx) {
                                        window.close_dialog(cx);
                                    }
                                })),
                        )
                        .child(
                            ramag_ui::clickable_button(("object-path-favorite-remove", index))
                                .ghost()
                                .xsmall()
                                .icon(IconName::Close)
                                .tooltip("移除收藏")
                                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                    this.remove_favorite(&prefix_for_remove, cx);
                                })),
                        ),
                );
            }
            content = content
                .child(div().text_sm().text_color(muted).child("常用"))
                .child(list);
        }

        content
            .when_some(self.error.clone(), |content, error| {
                content.child(div().text_xs().text_color(cx.theme().danger).child(error))
            })
            .child(
                h_flex()
                    .w_full()
                    .items_center()
                    .justify_end()
                    .pt(px(4.0))
                    .child(
                        ramag_ui::clickable_button("object-path-dialog-cancel")
                            .ghost()
                            .small()
                            .label("取消")
                            .on_click(|_: &ClickEvent, window, cx| window.close_dialog(cx)),
                    ),
            )
    }
}
