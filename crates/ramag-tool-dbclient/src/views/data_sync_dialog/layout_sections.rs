use gpui::{Anchor, ClickEvent, Context, IntoElement, ParentElement, Styled, div, prelude::*, px};
use gpui_component::{
    ActiveTheme, Disableable as _, Icon, IconName, Sizable as _, WindowExt as _,
    button::ButtonVariants as _, h_flex, spinner::Spinner, v_flex,
};
use ramag_domain::entities::DriverKind;
use ramag_ui::PointerDropdownMenu as _;

use super::catalog::connection_label;
use super::{DROPDOWN_MENU_MAX_HEIGHT, DataSyncDialog, PanelState};

impl DataSyncDialog {
    pub(super) fn render_safety_warning(&self, cx: &Context<Self>) -> impl IntoElement {
        let danger = cx.theme().danger;
        let mut background = danger;
        background.a = 0.08;
        h_flex()
            .id("data-sync-safety-warning")
            .w_full()
            .items_center()
            .gap(px(8.0))
            .px(px(10.0))
            .py(px(8.0))
            .rounded(px(6.0))
            .border_1()
            .border_color(danger)
            .bg(background)
            .child(
                Icon::new(IconName::TriangleAlert)
                    .small()
                    .text_color(danger),
            )
            .child(
                div()
                    .text_sm()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(danger)
                    .child("仅建议在非生产环境使用"),
            )
    }

    pub(super) fn render_connection_section(
        &self,
        busy: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let muted = cx.theme().muted;
        let muted_foreground = cx.theme().muted_foreground;
        let target = format!(
            "{}（{}）",
            connection_label(&self.target),
            driver_label(self.target.driver)
        );
        connection_columns(
            self.render_source_selector(busy, cx),
            target,
            muted,
            muted_foreground,
        )
    }

    pub(super) fn render_footer(
        &self,
        preflighting: bool,
        busy: bool,
        can_preflight: bool,
        ready: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let border = cx.theme().border;
        let muted = cx.theme().muted_foreground;
        let start_label = self
            .prepared
            .as_ref()
            .map(|prepared| {
                let report = prepared.report();
                if report.requires_second_confirmation && report.target_scope_exists {
                    match report.engine {
                        DriverKind::Mysql | DriverKind::Mongodb => "确认同步到已有库",
                        DriverKind::Postgres => "确认同步到已有 Schema",
                        DriverKind::Redis => "确认并开始",
                    }
                } else if report.requires_second_confirmation {
                    "确认同步到已有对象"
                } else {
                    "确认并开始"
                }
            })
            .unwrap_or("确认并开始");
        h_flex()
            .id("data-sync-dialog-footer")
            .w_full()
            .items_center()
            .justify_between()
            .gap(px(12.0))
            .pt(px(10.0))
            .border_t_1()
            .border_color(border)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_xs()
                    .text_color(muted)
                    .child("仅新增；不更新、不覆盖、不删除。"),
            )
            .child(
                h_flex()
                    .flex_none()
                    .gap(px(8.0))
                    .child(
                        ramag_ui::clickable_button("sync-dialog-cancel")
                            .ghost()
                            .small()
                            .label("关闭")
                            .disabled(preflighting)
                            .on_click(|_: &ClickEvent, window, cx| window.close_dialog(cx)),
                    )
                    .child(
                        ramag_ui::clickable_button("sync-dialog-preflight")
                            .outline()
                            .small()
                            .label(if preflighting {
                                "预检中…"
                            } else {
                                "预检"
                            })
                            .disabled(busy || !can_preflight)
                            .when(preflighting, |button| button.icon(Spinner::new().xsmall()))
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.preflight(cx);
                            })),
                    )
                    .child(
                        ramag_ui::clickable_button("sync-dialog-start")
                            .primary()
                            .small()
                            .label(start_label)
                            .disabled(!ready)
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.start(window, cx);
                            })),
                    ),
            )
    }

    fn render_source_selector(
        &self,
        busy: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let entity = cx.entity();
        let sources = self.sources.clone();
        let current = self
            .source()
            .map(connection_label)
            .unwrap_or_else(|_| "请选择源连接".into());
        let current_index = self.source_index;
        clipped_dropdown_button(
            "sync-source-selector",
            "sync-source-connection-text",
            current,
        )
        .disabled(busy || self.sources.is_empty() || self.state == PanelState::Preflighting)
        .pointer_dropdown_menu_with_anchor(Anchor::BottomLeft, move |mut menu, _, _| {
            menu = menu.scrollable(true).max_h(px(DROPDOWN_MENU_MAX_HEIGHT));
            for (index, source) in sources.iter().enumerate() {
                let entity = entity.clone();
                menu = menu.item(
                    ramag_ui::menu_item(connection_label(source))
                        .checked(Some(index) == current_index)
                        .on_click(move |_: &ClickEvent, _, app| {
                            entity.update(app, |this, cx| this.select_source(index, cx));
                        }),
                );
            }
            menu
        })
    }
}

fn connection_columns(
    source_selector: impl IntoElement,
    target_label: String,
    muted: gpui::Hsla,
    muted_foreground: gpui::Hsla,
) -> impl IntoElement {
    let source = connection_field("来源连接", source_selector)
        .id("sync-source-connection-field")
        .debug_selector(|| "sync-source-connection-field".into())
        .flex_1()
        .overflow_hidden();
    let target_value = h_flex()
        .id("sync-target-connection-value")
        .debug_selector(|| "sync-target-connection-value".into())
        .w_full()
        .min_w_0()
        .h(px(32.0))
        .items_center()
        .overflow_hidden()
        .px(px(10.0))
        .rounded(px(5.0))
        .bg(muted)
        .text_sm()
        .child(clipped_single_line(
            "sync-target-connection-text",
            target_label,
        ));
    let target = connection_field("目标连接（固定）", target_value)
        .id("sync-target-connection-field")
        .debug_selector(|| "sync-target-connection-field".into())
        .flex_1()
        .overflow_hidden();

    h_flex()
        .id("data-sync-connections")
        .w_full()
        .min_w_0()
        .items_end()
        .gap(px(10.0))
        .overflow_hidden()
        .child(source)
        .child(
            div()
                .id("sync-connection-arrow")
                .debug_selector(|| "sync-connection-arrow".into())
                .flex_none()
                .h(px(32.0))
                .flex()
                .items_center()
                .child(
                    Icon::new(IconName::ArrowRight)
                        .small()
                        .text_color(muted_foreground),
                ),
        )
        .child(target)
}

pub(super) fn clipped_dropdown_button(
    id: &'static str,
    text_id: &'static str,
    label: String,
) -> gpui_component::button::Button {
    ramag_ui::clickable_button(id)
        .outline()
        .small()
        .w_full()
        .min_w_0()
        .overflow_hidden()
        .tooltip(label.clone())
        .child(clipped_single_line(text_id, label))
        .dropdown_caret(true)
}

pub(super) fn clipped_single_line(id: &'static str, text: String) -> impl IntoElement {
    div()
        .id(id)
        .debug_selector(move || id.into())
        .flex_1()
        .min_w_0()
        .truncate()
        .child(text)
}

fn connection_field(label: &str, content: impl IntoElement) -> gpui::Div {
    v_flex()
        .w_full()
        .min_w_0()
        .gap(px(5.0))
        .child(
            div()
                .text_xs()
                .font_weight(gpui::FontWeight::MEDIUM)
                .child(label.to_string()),
        )
        .child(content)
}

fn driver_label(driver: DriverKind) -> &'static str {
    match driver {
        DriverKind::Mysql => "MySQL",
        DriverKind::Postgres => "PostgreSQL",
        DriverKind::Redis => "Redis",
        DriverKind::Mongodb => "MongoDB",
    }
}

#[cfg(test)]
mod tests {
    use gpui::{
        Context, ParentElement as _, Render, Styled as _, TestAppContext, Window, div, prelude::*,
        px, size,
    };
    use gpui_component::ActiveTheme as _;

    use super::{clipped_dropdown_button, connection_columns};

    struct LongConnectionLayout;

    impl Render for LongConnectionLayout {
        fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let source = clipped_dropdown_button(
                "test-source-selector",
                "sync-source-connection-text",
                "极长的来源连接名称".repeat(30),
            );
            div().size_full().p(px(16.0)).child(connection_columns(
                source,
                "极长的目标连接名称".repeat(30),
                cx.theme().muted,
                cx.theme().muted_foreground,
            ))
        }
    }

    #[gpui::test]
    fn long_connection_labels_stay_inside_their_columns(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let (view, cx) = cx.add_window_view(|_, _| LongConnectionLayout);
        cx.simulate_resize(size(px(560.0), px(180.0)));
        view.update(cx, |_, cx| cx.notify());
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();

        let source_field = cx
            .debug_bounds("sync-source-connection-field")
            .expect("来源栏应渲染");
        let source_text = cx
            .debug_bounds("sync-source-connection-text")
            .expect("来源文本应渲染");
        let arrow = cx
            .debug_bounds("sync-connection-arrow")
            .expect("方向箭头应渲染");
        let target_field = cx
            .debug_bounds("sync-target-connection-field")
            .expect("目标栏应渲染");
        let target_text = cx
            .debug_bounds("sync-target-connection-text")
            .expect("目标文本应渲染");

        assert!(right(source_text) <= right(source_field));
        assert!(right(source_field) <= arrow.origin.x);
        assert!(right(arrow) <= target_field.origin.x);
        assert!(right(target_text) <= right(target_field));
    }

    fn right(bounds: gpui::Bounds<gpui::Pixels>) -> gpui::Pixels {
        bounds.origin.x + bounds.size.width
    }
}
