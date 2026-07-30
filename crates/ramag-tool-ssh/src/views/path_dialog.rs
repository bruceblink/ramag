//! SSH 远程路径直达与常用路径管理。

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
    MAX_SSH_FAVORITE_PATHS_PER_PROFILE, MAX_SSH_PATH_BYTES, SshProfileId,
};

use super::SshView;
use super::ops_files::validate_direct_remote_path;

pub(super) fn open_remote_path_dialog(
    owner: Entity<SshView>,
    workspace_id: SshProfileId,
    initial: String,
    favorites: Vec<String>,
    window: &mut Window,
    cx: &mut App,
) {
    let form = cx.new(|cx| {
        RemotePathDialog::new(
            owner.clone(),
            workspace_id.clone(),
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
                "ssh-path-dialog-close",
                "直达",
                |_, _| {},
            ))
            .close_button(false)
            .width(px(520.0))
            .margin_top(px(160.0))
            .on_ok({
                let form = form.clone();
                move |_, _, app| form.update(app, |this, cx| this.open_current(cx))
            })
            .content(move |content, _, _| content.child(form_for_content.clone()))
    });
}

struct RemotePathDialog {
    owner: Entity<SshView>,
    workspace_id: SshProfileId,
    input: Entity<InputState>,
    favorites: Vec<String>,
    error: Option<String>,
    _input_subscription: Subscription,
}

impl RemotePathDialog {
    fn new(
        owner: Entity<SshView>,
        workspace_id: SshProfileId,
        initial: String,
        favorites: Vec<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| {
            InputState::new(window, cx).validate(|value, _| value.len() <= MAX_SSH_PATH_BYTES)
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
            workspace_id,
            input,
            favorites,
            error: None,
            _input_subscription: input_subscription,
        }
    }

    fn current_path(&self, cx: &App) -> String {
        self.input.read(cx).value().trim().to_string()
    }

    fn open_current(&mut self, cx: &mut Context<Self>) -> bool {
        let path = self.current_path(cx);
        self.open_path(path, cx)
    }

    fn open_path(&mut self, path: String, cx: &mut Context<Self>) -> bool {
        if let Err(error) = validate_direct_remote_path(&path) {
            self.error = Some(error);
            cx.notify();
            return false;
        }
        let workspace_id = self.workspace_id.clone();
        self.owner.update(cx, |this, cx| {
            this.refresh_directory(workspace_id, Some(path), cx);
        });
        true
    }

    fn favorite_current(&mut self, cx: &mut Context<Self>) {
        let path = self.current_path(cx);
        let workspace_id = self.workspace_id.clone();
        match self.owner.update(cx, |this, cx| {
            this.add_path_favorite(workspace_id, path.clone(), cx)
        }) {
            Ok(true) => {
                self.favorites.push(path);
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

    fn remove_favorite(&mut self, path: &str, cx: &mut Context<Self>) {
        let workspace_id = self.workspace_id.clone();
        if self.owner.update(cx, |this, cx| {
            this.remove_path_favorite(&workspace_id, path, cx)
        }) {
            self.favorites.retain(|favorite| favorite != path);
            cx.notify();
        }
    }
}

impl Render for RemotePathDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let border = cx.theme().border;
        let mut content = v_flex()
            .w_full()
            .gap(px(10.0))
            .child(div().text_sm().text_color(muted).child("路径"))
            .child(Input::new(&self.input).small());

        if !self.favorites.is_empty() {
            let mut list = v_flex()
                .id("ssh-path-favorites")
                .debug_selector(|| "ssh-path-favorites".into())
                .w_full()
                .max_h(px(180.0))
                .overflow_y_scroll()
                .border_1()
                .border_color(border)
                .rounded(px(6.0));
            for (index, path) in self.favorites.clone().into_iter().enumerate() {
                let path_for_open = path.clone();
                let path_for_remove = path.clone();
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
                                .id(("ssh-path-favorite-open", index))
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
                                .hover(|style| style.bg(cx.theme().muted))
                                .child(path)
                                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                    if this.open_path(path_for_open.clone(), cx) {
                                        window.close_dialog(cx);
                                    }
                                })),
                        )
                        .child(
                            ramag_ui::clickable_button(("ssh-path-favorite-remove", index))
                                .ghost()
                                .xsmall()
                                .icon(IconName::Close)
                                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                    this.remove_favorite(&path_for_remove, cx);
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
                    .gap(px(8.0))
                    .pt(px(4.0))
                    .child(
                        ramag_ui::clickable_button("ssh-path-dialog-cancel")
                            .ghost()
                            .small()
                            .label("取消")
                            .on_click(|_: &ClickEvent, window, cx| window.close_dialog(cx)),
                    )
                    .child(
                        div()
                            .id("ssh-path-dialog-favorite-debug")
                            .debug_selector(|| "ssh-path-dialog-favorite".into())
                            .child(
                                ramag_ui::clickable_button("ssh-path-dialog-favorite")
                                    .outline()
                                    .small()
                                    .icon(IconName::Star)
                                    .label("收藏")
                                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                        this.favorite_current(cx);
                                    })),
                            ),
                    )
                    .child(
                        ramag_ui::clickable_button("ssh-path-dialog-open")
                            .primary()
                            .small()
                            .label("打开")
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                if this.open_current(cx) {
                                    window.close_dialog(cx);
                                }
                            })),
                    ),
            )
    }
}

impl SshView {
    fn add_path_favorite(
        &mut self,
        workspace_id: SshProfileId,
        path: String,
        cx: &mut Context<Self>,
    ) -> Result<bool, String> {
        let added =
            insert_path_favorite(self.path_favorites.entry(workspace_id).or_default(), path)?;
        if added {
            self.persist_workspaces(cx);
        }
        Ok(added)
    }

    fn remove_path_favorite(
        &mut self,
        workspace_id: &SshProfileId,
        path: &str,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(paths) = self.path_favorites.get_mut(workspace_id) else {
            return false;
        };
        let initial_len = paths.len();
        paths.retain(|favorite| favorite != path);
        let removed = paths.len() != initial_len;
        let empty = paths.is_empty();
        if empty {
            self.path_favorites.remove(workspace_id);
        }
        if removed {
            self.persist_workspaces(cx);
        }
        removed
    }
}

fn insert_path_favorite(paths: &mut Vec<String>, path: String) -> Result<bool, String> {
    validate_direct_remote_path(&path)?;
    if !path.starts_with('/') {
        return Err("收藏路径必须以 / 开头".into());
    }
    if paths.iter().any(|favorite| favorite == &path) {
        return Ok(false);
    }
    if paths.len() >= MAX_SSH_FAVORITE_PATHS_PER_PROFILE {
        return Err(format!(
            "常用路径最多 {MAX_SSH_FAVORITE_PATHS_PER_PROFILE} 个"
        ));
    }
    paths.push(path);
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn favorite_paths_are_absolute_unique_and_bounded() {
        let mut paths = Vec::new();
        assert!(insert_path_favorite(&mut paths, "/var/log".into()).unwrap());
        assert!(!insert_path_favorite(&mut paths, "/var/log".into()).unwrap());
        assert!(insert_path_favorite(&mut paths, ".".into()).is_err());

        for index in 1..MAX_SSH_FAVORITE_PATHS_PER_PROFILE {
            assert!(insert_path_favorite(&mut paths, format!("/tmp/{index}")).unwrap());
        }
        assert!(insert_path_favorite(&mut paths, "/overflow".into()).is_err());
    }
}
