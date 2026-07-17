//! 侧栏「远程仓库」：单行 remote（名 + fetch URL 内联 + 改URL/重命名/删除）+ 底部新建行。
//! 与「远程分支」区分：这里管 remote 配置（origin 等），非远端分支引用。
//! 行由 history 左栏的单个 uniform_list 统一渲染（28px 等高），段组装见 history_panel

use gpui::{
    AnyElement, ClickEvent, Context, InteractiveElement, IntoElement, ParentElement, SharedString,
    Styled, div, px,
};
use gpui_component::{
    ActiveTheme, Disableable as _, Icon, IconName, Sizable as _, button::ButtonVariants as _,
    h_flex, input::Input,
};
use ramag_domain::entities::Remote;

use super::helpers::side_op_button;
use super::sidebar::LEFT_ROW_H;
use super::vcs_view::VcsView;

impl VcsView {
    /// 底部「新建远程」输入行：name 一格 + url 一格 + 添加按钮（固定 28px 高，单行）
    pub(super) fn render_create_remote_row(&self, cx: &mut Context<Self>) -> AnyElement {
        let busy = self.busy;
        h_flex()
            .h(px(LEFT_ROW_H))
            .flex_none()
            .gap(px(4.0))
            .items_center()
            .child(
                div().flex_none().w(px(90.0)).child(
                    Input::new(&self.create_remote_name_input)
                        .xsmall()
                        .into_any_element(),
                ),
            )
            .child(
                div().flex_1().min_w_0().child(
                    Input::new(&self.create_remote_url_input)
                        .xsmall()
                        .into_any_element(),
                ),
            )
            .child(
                ramag_ui::clickable_button("vcs-remote-create")
                    .ghost()
                    .xsmall()
                    .icon(IconName::Plus)
                    .tooltip("添加远程（git remote add）")
                    .disabled(busy)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.handle_create_remote(cx);
                    })),
            )
            .into_any_element()
    }
}

/// 单条 remote 行：[globe] name + fetch URL 内联 + 行尾 [改URL][重命名][删除]（固定 28px 高）
pub(super) fn remote_row(
    idx: usize,
    r: &Remote,
    busy: bool,
    cx: &mut Context<VcsView>,
) -> impl IntoElement {
    let theme = cx.theme();
    let fg = theme.foreground;
    let muted_fg = theme.muted_foreground;
    let mono = theme.mono_font_family.clone();
    let hover_bg = theme.muted;
    // 蓝青色：远程仓库与分支（暖橙 tag / 常规分支）区分
    let remote_color = gpui::hsla(200.0 / 360.0, 0.6, 0.55, 1.0);

    let name = r.name.clone();
    let url = r.fetch_url.clone();
    let row_id = SharedString::from(format!("vcs-side-remote-{idx}-{name}"));

    h_flex()
        .id(row_id)
        .h(px(LEFT_ROW_H))
        .flex_none()
        .gap(px(6.0))
        .items_center()
        .px(px(4.0))
        .rounded(px(3.0))
        .hover(move |this| this.bg(hover_bg))
        .child(
            div()
                .flex_none()
                .w(px(14.0))
                .child(Icon::new(IconName::Globe).xsmall().text_color(remote_color)),
        )
        .child(
            h_flex()
                .flex_1()
                .min_w_0()
                .gap(px(6.0))
                .items_baseline()
                .child(
                    div()
                        .min_w_0()
                        .overflow_hidden()
                        .text_ellipsis()
                        .text_sm()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(fg)
                        .child(super::inline_text_preview(&name, 120)),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .text_ellipsis()
                        .text_xs()
                        .font_family(mono)
                        .text_color(muted_fg)
                        .child(super::inline_text_preview(&url, 240)),
                ),
        )
        .child(
            h_flex()
                .gap(px(6.0))
                .flex_none()
                .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| {
                    cx.stop_propagation();
                })
                .child({
                    let name = name.clone();
                    let url = url.clone();
                    side_op_button(
                        format!("vcs-side-remote-url-{idx}"),
                        "修改 fetch URL",
                        IconName::ExternalLink,
                        busy,
                        move |this, window, cx| {
                            this.prompt_remote_set_url(name.clone(), url.clone(), window, cx)
                        },
                        cx,
                    )
                })
                .child({
                    let name = name.clone();
                    side_op_button(
                        format!("vcs-side-remote-rename-{idx}"),
                        "重命名远程",
                        ramag_ui::icons::pencil(),
                        busy,
                        move |this, window, cx| this.prompt_remote_rename(name.clone(), window, cx),
                        cx,
                    )
                })
                .child(side_op_button(
                    format!("vcs-side-remote-delete-{idx}"),
                    "删除远程",
                    ramag_ui::icons::trash(),
                    busy,
                    move |this, window, cx| this.confirm_remote_delete(name.clone(), window, cx),
                    cx,
                )),
        )
}
