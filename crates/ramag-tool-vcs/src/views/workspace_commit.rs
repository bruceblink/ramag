use gpui::{
    AnyElement, ClickEvent, Context, IntoElement, ParentElement, Styled, div, prelude::*, px,
};
use gpui_component::{
    ActiveTheme, Disableable as _, Icon, IconName, Sizable as _, button::ButtonVariants as _,
    h_flex, input::Input, v_flex,
};
use ramag_domain::entities::MAX_COMMIT_MESSAGE_BYTES;
use ramag_ui::PointerDropdownMenu as _;

use super::vcs_view::VcsView;

impl VcsView {
    pub(super) fn render_commit_panel(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let muted_fg = theme.muted_foreground;
        let accent = theme.accent;
        let border = theme.border;

        let staged_count = self
            .status
            .as_ref()
            .map(|s| s.files.iter().filter(|f| f.staged.is_some()).count())
            .unwrap_or(0);
        // 修订可沿用原提交信息。
        let commit_value = self.commit_input.read(cx).value();
        let has_message = !commit_value.trim().is_empty();
        let message_too_large = commit_value.len() > MAX_COMMIT_MESSAGE_BYTES;
        let has_head = self
            .status
            .as_ref()
            .and_then(|status| status.head_commit.as_ref())
            .is_some();
        let can_commit = !self.busy
            && self
                .status
                .as_ref()
                .and_then(|status| status.operation)
                .is_none()
            && (!self.commit_amend || has_head)
            && (staged_count > 0 || self.commit_amend)
            && (has_message || self.commit_amend)
            && !message_too_large;

        let committing = self.busy_label == Some("提交中…");
        let commit_btn = ramag_ui::clickable_button("vcs-commit")
            .primary()
            .small()
            .icon(ramag_ui::icons::git_commit())
            .label(if committing {
                "提交中…".to_string()
            } else if self.commit_amend {
                "修订".to_string()
            } else if staged_count > 0 {
                format!("提交 ({staged_count})")
            } else {
                "提交".to_string()
            })
            .disabled(!can_commit)
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                this.confirm_commit(window, cx);
            }));
        let amend_on = self.commit_amend;
        let sign_on = self.commit_sign;
        let entity = cx.entity();
        let more_btn = ramag_ui::clickable_button("vcs-commit-more")
            .primary()
            .small()
            .icon(IconName::ChevronDown)
            .tooltip("提交选项")
            .pointer_dropdown_menu_with_anchor(gpui::Anchor::BottomRight, move |mut m, _, _| {
                let ent = entity.clone();
                let label = if amend_on { "✓ 修订" } else { "修订" };
                m = m.item(
                    ramag_ui::menu_item_with_disabled(label, !has_head).on_click(
                        move |_, _, app| {
                            ent.update(app, |this, cx| this.toggle_commit_amend(cx));
                        },
                    ),
                );
                let ent = entity.clone();
                let sign_label = if sign_on { "✓ 签名" } else { "签名" };
                m = m.item(ramag_ui::menu_item(sign_label).on_click(move |_, _, app| {
                    ent.update(app, |this, cx| {
                        this.commit_sign = !this.commit_sign;
                        cx.notify();
                    });
                }));
                m
            });

        v_flex()
            .flex_none()
            .gap(px(4.0))
            .px(px(10.0))
            .py(px(8.0))
            .border_t_1()
            .border_color(border)
            .child(
                h_flex()
                    .items_center()
                    .gap(px(6.0))
                    .child(
                        Icon::new(ramag_ui::icons::git_commit())
                            .small()
                            .text_color(accent),
                    )
                    .child(
                        div()
                            .text_xs()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(accent)
                            .child("提交"),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(muted_fg)
                            .child(if message_too_large {
                                format!(
                                    "· 提交信息超过 {} MiB 上限，请缩短后再提交",
                                    MAX_COMMIT_MESSAGE_BYTES / 1024 / 1024
                                )
                            } else if staged_count == 0 && !self.commit_amend {
                                "· 请先暂存文件".to_string()
                            } else if !has_message && !self.commit_amend {
                                format!("· 已暂存 {staged_count} 个文件，请填写提交信息")
                            } else {
                                format!("· 已暂存 {staged_count} 个文件")
                            }),
                    ),
            )
            .child(
                Input::new(&self.commit_input)
                    .h(px(72.0))
                    .into_any_element(),
            )
            .when_some(self.commit_draft_error.clone(), |panel, error| {
                panel.child(div().text_xs().text_color(theme.warning).child(error))
            })
            .child(
                h_flex()
                    .items_center()
                    .justify_end()
                    .gap(px(2.0))
                    .child(commit_btn)
                    .child(more_btn),
            )
            .into_any_element()
    }
}
