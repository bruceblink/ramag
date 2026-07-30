//! 数据库搜索设置页渲染。

use gpui::{
    ClickEvent, Context, IntoElement, ParentElement, Styled, div, prelude::FluentBuilder as _, px,
};
use gpui_component::{
    ActiveTheme as _, Disableable as _, Sizable as _, h_flex, input::Input, v_flex,
};
use ramag_domain::entities::IdConverterKind;

use super::{
    super::{DatabaseConverterTestDirection, DatabaseConverterTestState, SettingsView},
    algorithm::{quote_contents, render_algorithm_summary},
    id_converter_kind_label,
};
use crate::PointerDropdownMenu as _;

impl SettingsView {
    pub(in crate::settings_view) fn render_database_page(
        &self,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let current_kind = self.database_converter_kind;
        let picking = self.picking_id_converter;
        let view = cx.entity();

        let kind_selector = crate::clickable_button("settings-db-id-converter-kind")
            .outline()
            .small()
            .label(id_converter_kind_label(current_kind))
            .dropdown_caret(true)
            .disabled(picking)
            .pointer_dropdown_menu(move |mut menu, _, _| {
                for kind in IdConverterKind::ALL {
                    let view = view.clone();
                    menu = menu.item(
                        crate::menu_item(id_converter_kind_label(kind))
                            .checked(kind == current_kind)
                            .on_click(move |_: &ClickEvent, _, app| {
                                view.update(app, |this, cx| {
                                    this.select_database_converter_kind(kind, cx);
                                });
                            }),
                    );
                }
                menu
            });

        let configuration = super::super::pages::settings_card("ID 转换配置", theme.border)
            .child(
                div()
                    .text_xs()
                    .text_color(muted)
                    .child("两个 @ID 模式共用一套转换配置；内置与字符表模式按相同字符顺序双向换算。"),
            )
            .child(
                v_flex()
                    .w_full()
                    .gap(px(6.0))
                    .child(div().text_sm().child("转换方式"))
                    .child(kind_selector),
            )
            .when(current_kind.is_custom(), |card| {
                card.child(
                    v_flex()
                        .w_full()
                        .gap(px(6.0))
                        .child(div().text_sm().child("字符表"))
                        .child(Input::new(&self.database_custom_alphabet).w_full().small())
                        .child(
                            div()
                                .text_xs()
                                .text_color(muted)
                                .child("按数值从小到大排列；至少 2 个互不重复的可见 ASCII 字符，不允许空格。"),
                        ),
                )
            })
            .when(current_kind.is_external(), |card| {
                card.child(
                    v_flex()
                        .w_full()
                        .gap(px(6.0))
                        .child(div().text_sm().child("转换程序"))
                        .child(
                            h_flex()
                                .w_full()
                                .gap(px(8.0))
                                .child(
                                    Input::new(&self.database_converter_program)
                                        .flex_1()
                                        .small()
                                        .disabled(picking),
                                )
                                .child(
                                    crate::clickable_button(
                                        "settings-db-id-converter-pick",
                                    )
                                    .outline()
                                    .small()
                                    .label(if picking { "选择中…" } else { "选择…" })
                                    .disabled(picking)
                                    .on_click(cx.listener(
                                        |this, _: &ClickEvent, window, cx| {
                                            this.pick_id_converter(window, cx);
                                        },
                                    )),
                                ),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme.warning)
                                .child("程序以当前用户权限直接运行，请只选择你信任的可执行文件。"),
                        ),
                )
            })
            .child(render_algorithm_summary(
                current_kind,
                &self.database_custom_alphabet.read(cx).value(),
                cx,
            ))
            .when(current_kind.is_external(), |card| {
                card.child(
                    div()
                        .text_xs()
                        .text_color(muted)
                        .child("每次仅传入一个参数，不经 shell、不使用 stdin，超时为 2 秒。-s 须输出非负 i64；-i 须输出一行 UTF-8 字符串。"),
                )
            });

        v_flex()
            .w_full()
            .gap(px(16.0))
            .when(self.saving_database, |page| {
                page.child(
                    h_flex().w_full().justify_end().child(
                        div()
                            .text_xs()
                            .text_color(muted)
                            .child("正在自动保存…"),
                    ),
                )
            })
            .child(
                super::super::pages::settings_card("搜索模式", theme.border).child(
                    h_flex()
                        .w_full()
                        .items_center()
                        .justify_between()
                        .gap(px(16.0))
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .gap(px(2.0))
                                .child(div().text_sm().child("启用 ID 转换搜索"))
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(muted)
                                        .child("在数据库结果行搜索中启用 @ID -> I（字符串转整数）和 @ID -> S（整数转字符串）。"),
                                ),
                        )
                        .child(
                            crate::clickable_switch("settings-db-id-conversion")
                                .flex_none()
                                .checked(self.database_enabled_draft)
                                .disabled(picking)
                                .on_click(cx.listener(|this, _: &bool, _, cx| {
                                    this.database_enabled_draft =
                                        !this.database_enabled_draft;
                                    this.schedule_database_search_save(
                                        std::time::Duration::ZERO,
                                        cx,
                                    );
                                    cx.notify();
                                })),
                        ),
                ),
            )
            .child(configuration)
            .child(self.render_database_converter_test(cx))
            .into_any_element()
    }

    fn render_database_converter_test(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let theme = cx.theme();
        let testing_direction = match self.database_converter_test_state {
            DatabaseConverterTestState::Testing(direction) => Some(direction),
            _ => None,
        };
        let testing = testing_direction.is_some();
        let status = match &self.database_converter_test_state {
            DatabaseConverterTestState::Idle => div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child("尚未测试。"),
            DatabaseConverterTestState::Testing(direction) => div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(format!("正在执行 {}…", direction.action_label())),
            DatabaseConverterTestState::Success { direction, output } => div()
                .text_xs()
                .text_color(theme.success)
                .child(direction.success_message(output)),
            DatabaseConverterTestState::Error { direction, message } => div()
                .text_xs()
                .text_color(theme.danger)
                .child(format!("{}失败：{message}", direction.action_label())),
        };

        super::super::pages::settings_card("转换测试", theme.border)
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child("直接使用上方当前配置。输入一项值，再明确选择转换方向。"),
            )
            .child(
                Input::new(&self.database_converter_test_input)
                    .w_full()
                    .small(),
            )
            .child(
                h_flex()
                    .w_full()
                    .gap(px(8.0))
                    .child(
                        crate::clickable_button("settings-db-id-converter-test-integer")
                            .outline()
                            .small()
                            .flex_1()
                            .label(
                                if testing_direction
                                    == Some(DatabaseConverterTestDirection::ToInteger)
                                {
                                    "转换中…"
                                } else {
                                    "@ID -> I · 字符串 → 整数"
                                },
                            )
                            .disabled(testing)
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.run_database_converter_test(
                                    DatabaseConverterTestDirection::ToInteger,
                                    cx,
                                );
                            })),
                    )
                    .child(
                        crate::clickable_button("settings-db-id-converter-test-string")
                            .outline()
                            .small()
                            .flex_1()
                            .label(
                                if testing_direction
                                    == Some(DatabaseConverterTestDirection::ToString)
                                {
                                    "转换中…"
                                } else {
                                    "@ID -> S · 整数 → 字符串"
                                },
                            )
                            .disabled(testing)
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.run_database_converter_test(
                                    DatabaseConverterTestDirection::ToString,
                                    cx,
                                );
                            })),
                    ),
            )
            .child(status)
            .into_any_element()
    }
}

impl DatabaseConverterTestDirection {
    fn action_label(self) -> &'static str {
        match self {
            Self::ToInteger => "字符串 → 整数",
            Self::ToString => "整数 → 字符串",
        }
    }

    fn success_message(self, output: &str) -> String {
        match self {
            Self::ToInteger => format!("@ID -> I 结果：{output}"),
            Self::ToString => format!("@ID -> S 结果：\"{}\"", quote_contents(output)),
        }
    }
}
