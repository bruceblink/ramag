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
        let converter_test = self
            .database_enabled_draft
            .then(|| self.render_database_converter_test(cx));
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
                            .on_click(move |_: &ClickEvent, window, app| {
                                view.update(app, |this, cx| {
                                    this.select_database_converter_kind(kind, window, cx);
                                });
                            }),
                    );
                }
                menu
            });

        let configuration = v_flex()
            .w_full()
            .gap(px(12.0))
            .child(
                v_flex()
                    .w_full()
                    .gap(px(4.0))
                    .child(
                        div()
                            .text_sm()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child("转换配置"),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(muted)
                            .child("两个 @ID 模式共用此配置。"),
                    ),
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
                                .child("至少 2 个不重复的可见 ASCII 字符，不能含空格。"),
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
                                    crate::clickable_button("settings-db-id-converter-pick")
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
                    div().text_xs().text_color(muted).child(
                        "不经 shell 或 stdin；超时 2 秒。-s 输出非负 i64，-i 输出一行 UTF-8。",
                    ),
                )
            });

        v_flex()
            .w_full()
            .gap(px(16.0))
            .child(
                super::super::pages::settings_card("连接配置", theme.border)
                    .child(
                        h_flex()
                            .w_full()
                            .items_center()
                            .justify_between()
                            .gap(px(16.0))
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .gap(px(3.0))
                                    .child(
                                        div()
                                            .text_sm()
                                            .child("MySQL / PostgreSQL / Redis / MongoDB"),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(muted)
                                            .child("通过口令加密文件迁移全部连接与凭据。"),
                                    ),
                            )
                            .child(
                                h_flex()
                                    .flex_none()
                                    .gap(px(8.0))
                                    .child(
                                        crate::clickable_button("settings-db-connections-import")
                                            .outline()
                                            .small()
                                            .icon(crate::icons::download())
                                            .label("导入")
                                            .disabled(self.database_transferring)
                                            .on_click(cx.listener(
                                                |this, _: &ClickEvent, window, cx| {
                                                    this.import_connections(window, cx);
                                                },
                                            )),
                                    )
                                    .child(
                                        crate::clickable_button("settings-db-connections-export")
                                            .outline()
                                            .small()
                                            .icon(crate::icons::upload())
                                            .label("导出")
                                            .disabled(self.database_transferring)
                                            .on_click(cx.listener(
                                                |this, _: &ClickEvent, window, cx| {
                                                    this.prompt_connection_export(window, cx);
                                                },
                                            )),
                                    ),
                            ),
                    )
                    .when(self.database_transferring, |card| {
                        card.child(div().text_xs().text_color(muted).child("正在处理连接配置…"))
                    }),
            )
            .when(self.saving_database, |page| {
                page.child(
                    h_flex()
                        .w_full()
                        .justify_end()
                        .child(div().text_xs().text_color(muted).child("正在自动保存…")),
                )
            })
            .child(
                super::super::pages::settings_card("Redis Key 树", theme.border).child(
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
                                .child(div().text_sm().child("同名 Key 下沉展示"))
                                .child(
                                    div().text_xs().text_color(muted).child(
                                        "路径也是 Key 时，将该 Key 放到子树末尾。默认关闭。",
                                    ),
                                ),
                        )
                        .child(
                            crate::clickable_switch("settings-redis-key-sink")
                                .flex_none()
                                .checked(self.redis_sink_same_name_keys)
                                .on_click(cx.listener(|this, _: &bool, _, cx| {
                                    this.toggle_redis_key_sink(cx);
                                })),
                        ),
                ),
            )
            .child(
                super::super::pages::settings_card("搜索配置", theme.border)
                    .child(
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
                                    .child(div().text_sm().child("雪花 ID 转换"))
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(muted)
                                            .child("结果搜索支持 @ID -> I 和 @ID -> S。"),
                                    ),
                            )
                            .child(
                                crate::clickable_switch("settings-db-id-conversion")
                                    .flex_none()
                                    .checked(self.database_enabled_draft)
                                    .disabled(picking)
                                    .on_click(cx.listener(|this, _: &bool, window, cx| {
                                        this.database_enabled_draft = !this.database_enabled_draft;
                                        if !this.database_enabled_draft {
                                            this.clear_database_converter_test(window, cx);
                                        }
                                        this.schedule_database_search_save(
                                            std::time::Duration::ZERO,
                                            cx,
                                        );
                                        cx.notify();
                                    })),
                            ),
                    )
                    .when_some(converter_test, |card, converter_test| {
                        card.child(
                            v_flex()
                                .w_full()
                                .pt(px(12.0))
                                .gap(px(16.0))
                                .border_t_1()
                                .border_color(theme.border)
                                .child(configuration)
                                .child(converter_test),
                        )
                    }),
            )
            .child(
                super::super::pages::settings_card("结果显示", theme.border).child(
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
                                .child(div().text_sm().child("显示水平滚动条"))
                                .child(
                                    div().text_xs().text_color(muted).child(
                                        "控制所有数据库结果表底部水平滚动条的显示，默认开启。",
                                    ),
                                ),
                        )
                        .child(
                            crate::clickable_switch("settings-db-result-horizontal-scrollbar")
                                .flex_none()
                                .checked(self.show_database_result_horizontal_scrollbar)
                                .on_click(cx.listener(|this, _: &bool, _, cx| {
                                    this.toggle_database_result_horizontal_scrollbar(cx);
                                })),
                        ),
                ),
            )
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

        v_flex()
            .w_full()
            .pt(px(16.0))
            .gap(px(12.0))
            .border_t_1()
            .border_color(theme.border)
            .child(
                div()
                    .text_sm()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child("转换测试"),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child("使用当前配置；输入值后选择方向。"),
            )
            .child(
                crate::cleanable_input(
                    &self.database_converter_test_input,
                    "settings-db-id-converter-test-clear",
                    false,
                    cx,
                )
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
