//! 数据库搜索设置页。

use gpui::{
    ClickEvent, Context, IntoElement, ParentElement, Styled, Window, div,
    prelude::FluentBuilder as _, px,
};
use gpui_component::{
    ActiveTheme as _, Disableable as _, Sizable as _, button::ButtonVariants as _, h_flex,
    input::Input, notification::Notification, v_flex,
};
use ramag_domain::error::DomainError;

use super::{SettingsView, pages::settings_card};
use crate::{
    DATABASE_SEARCH_SETTINGS_PREF_KEY, DatabaseSearchSettings, set_database_search_settings,
    validate_id_converter_program,
};

impl SettingsView {
    pub(super) fn render_database_page(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let disabled = self.saving_database || self.picking_id_converter;

        v_flex()
            .w_full()
            .gap(px(16.0))
            .when(self.saving_database, |page| {
                page.child(div().text_xs().text_color(muted).child("保存中…"))
            })
            .child(
                settings_card("搜索设置", theme.border)
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
                                    .child(div().text_sm().child("启用雪花 ID 外部转换"))
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(muted)
                                            .child("启用后，SQL 结果行搜索可以选择 @ID 模式。"),
                                    ),
                            )
                            .child(
                                crate::clickable_switch("settings-db-id-conversion")
                                    .flex_none()
                                    .checked(self.database_enabled_draft)
                                    .disabled(disabled)
                                    .on_click(cx.listener(|this, _: &bool, _, cx| {
                                        this.database_enabled_draft =
                                            !this.database_enabled_draft;
                                        cx.notify();
                                    })),
                            ),
                    )
                    .child(
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
                                            .disabled(disabled),
                                    )
                                    .child(
                                        crate::clickable_button("settings-db-id-converter-pick")
                                            .outline()
                                            .small()
                                            .label(if self.picking_id_converter {
                                                "选择中…"
                                            } else {
                                                "选择…"
                                            })
                                            .disabled(disabled)
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
                                    .text_color(muted)
                                    .child("协议：Ramag 向 stdin 写入一行 UTF-8 搜索词；程序须在 2 秒内以 stdout 输出一个非负 i64 十进制整数并以状态码 0 退出。"),
                            ),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.warning)
                            .child("安全提示：程序会以当前用户权限运行。Ramag 不经 shell 执行，但仍应只选择你信任的程序。"),
                    )
                    .child(
                        h_flex().w_full().justify_end().child(
                            crate::clickable_button("settings-db-search-save")
                                .primary()
                                .small()
                                .label("保存搜索设置")
                                .disabled(disabled)
                                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                    this.save_database_search_settings(cx);
                                })),
                        ),
                    ),
            )
            .into_any_element()
    }

    fn pick_id_converter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.saving_database || self.picking_id_converter {
            return;
        }
        self.picking_id_converter = true;
        cx.notify();
        cx.spawn_in(window, async move |this, async_cx| {
            let picked = rfd::AsyncFileDialog::new().pick_file().await;
            let _ = this.update_in(async_cx, |this, window, cx| {
                this.picking_id_converter = false;
                if let Some(handle) = picked {
                    let Some(path) = handle.path().to_str().map(str::to_owned) else {
                        this.pending_notification =
                            Some(Notification::error("转换程序路径不是有效的 UTF-8"));
                        cx.notify();
                        return;
                    };
                    this.database_converter_program.update(cx, |state, cx| {
                        state.set_value(path, window, cx);
                    });
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn save_database_search_settings(&mut self, cx: &mut Context<Self>) {
        if self.saving_database {
            return;
        }
        let next = DatabaseSearchSettings {
            id_conversion_enabled: self.database_enabled_draft,
            id_converter_program: self.database_converter_program.read(cx).value().to_string(),
        };
        let json = match next.to_json() {
            Ok(json) => json,
            Err(error) => {
                self.pending_notification = Some(Notification::error(error));
                cx.notify();
                return;
            }
        };
        let Some(storage) = crate::theme::storage_from_cx(cx) else {
            self.pending_notification = Some(Notification::error("本地存储尚未初始化"));
            cx.notify();
            return;
        };

        self.saving_database = true;
        cx.notify();
        let validate_program = next.id_conversion_enabled;
        let program = next.id_converter_program.clone();
        cx.spawn(async move |this, cx| {
            let result: Result<(), String> = async {
                if validate_program {
                    ramag_app::run_blocking(move || {
                        validate_id_converter_program(&program).map_err(DomainError::InvalidConfig)
                    })
                    .await
                    .map_err(|error| error.to_string())?;
                }
                storage
                    .set_preference(DATABASE_SEARCH_SETTINGS_PREF_KEY, &json)
                    .await
                    .map_err(|error| error.to_string())
            }
            .await;

            let _ = this.update(cx, |this, cx| {
                this.saving_database = false;
                match result {
                    Ok(()) => {
                        this.database_enabled_draft = next.id_conversion_enabled;
                        set_database_search_settings(next, cx);
                        this.pending_notification =
                            Some(Notification::success("数据库搜索设置已保存").autohide(true));
                    }
                    Err(error) => {
                        this.pending_notification = Some(Notification::error(format!(
                            "数据库搜索设置保存失败：{error}"
                        )));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
}
