//! 数据库搜索设置的交互与持久化。

mod algorithm;
mod render;
mod transfer;

use std::time::Duration;

use gpui::{Context, Window};
use gpui_component::{input::InputEvent, notification::Notification};
use ramag_domain::{
    entities::{IdConverterConfig, IdConverterKind},
    error::DomainError,
};

use super::{DatabaseConverterTestDirection, DatabaseConverterTestState, SettingsView};
use crate::{
    DATABASE_SEARCH_SETTINGS_PREF_KEY, DatabaseSearchSettings, set_database_search_settings,
    validate_id_converter_program,
};

const DATABASE_SEARCH_SAVE_DEBOUNCE: Duration = Duration::from_millis(500);

pub(super) fn id_converter_kind_label(kind: IdConverterKind) -> &'static str {
    match kind {
        IdConverterKind::Base10 => "Base10（十进制）",
        IdConverterKind::Base16 => "Base16（十六进制）",
        IdConverterKind::Base36 => "Base36",
        IdConverterKind::Base58Bitcoin => "Base58 Bitcoin",
        IdConverterKind::Base58Flickr => "Base58 Flickr",
        IdConverterKind::CustomAlphabet => "自定义字符表（Base-N）",
        IdConverterKind::ExternalProgram => "自定义算法（外部程序）",
    }
}

impl SettingsView {
    pub(super) fn on_database_converter_input_event(
        &mut self,
        event: &InputEvent,
        cx: &mut Context<Self>,
    ) {
        match event {
            InputEvent::Change => {
                self.invalidate_database_converter_test();
                self.schedule_database_search_save(DATABASE_SEARCH_SAVE_DEBOUNCE, cx);
            }
            InputEvent::PressEnter { .. } | InputEvent::Blur => {
                self.schedule_database_search_save(Duration::ZERO, cx);
            }
            InputEvent::Focus => {}
        }
    }

    pub(super) fn on_database_converter_test_input_event(
        &mut self,
        event: &InputEvent,
        cx: &mut Context<Self>,
    ) {
        match event {
            InputEvent::Change => {
                self.invalidate_database_converter_test();
                cx.notify();
            }
            // 双向测试共用输入框，回车无法可靠推断方向，由两个按钮显式选择。
            InputEvent::PressEnter { .. } => {}
            InputEvent::Focus | InputEvent::Blur => {}
        }
    }

    pub(super) fn clear_database_converter_test(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.invalidate_database_converter_test();
        if !self
            .database_converter_test_input
            .read(cx)
            .value()
            .is_empty()
        {
            self.database_converter_test_input
                .update(cx, |state, cx| state.set_value("", window, cx));
        }
        cx.notify();
    }

    fn select_database_converter_kind(
        &mut self,
        kind: IdConverterKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.database_converter_kind == kind {
            return;
        }
        self.database_converter_kind = kind;
        self.clear_database_converter_test(window, cx);
        self.schedule_database_search_save(Duration::ZERO, cx);
    }

    fn pick_id_converter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.picking_id_converter {
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
                            Some(Notification::error("ID 转换器路径不是有效的 UTF-8"));
                        cx.notify();
                        return;
                    };
                    this.database_converter_program.update(cx, |state, cx| {
                        state.set_value(path, window, cx);
                    });
                    this.invalidate_database_converter_test();
                    this.schedule_database_search_save(Duration::ZERO, cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn database_converter_draft(&self, cx: &gpui::App) -> IdConverterConfig {
        IdConverterConfig {
            kind: self.database_converter_kind,
            custom_alphabet: self.database_custom_alphabet.read(cx).value().to_string(),
            external_program: self.database_converter_program.read(cx).value().to_string(),
        }
    }

    fn run_database_converter_test(
        &mut self,
        direction: DatabaseConverterTestDirection,
        cx: &mut Context<Self>,
    ) {
        if matches!(
            self.database_converter_test_state,
            DatabaseConverterTestState::Testing(_)
        ) {
            return;
        }
        self.invalidate_database_converter_test();

        let input = self
            .database_converter_test_input
            .read(cx)
            .value()
            .to_string();
        let config = self.database_converter_draft(cx);
        if input.is_empty() {
            self.database_converter_test_state = DatabaseConverterTestState::Error {
                direction,
                message: "请先输入一个示例 ID".to_string(),
            };
            cx.notify();
            return;
        }
        if let Err(error) = config.validate_active() {
            self.database_converter_test_state = DatabaseConverterTestState::Error {
                direction,
                message: error,
            };
            cx.notify();
            return;
        }

        self.database_converter_test_seq = self.database_converter_test_seq.wrapping_add(1);
        let test_seq = self.database_converter_test_seq;
        self.database_converter_test_state = DatabaseConverterTestState::Testing(direction);
        cx.notify();

        let is_external = config.kind.is_external();
        let program = config.external_program.clone();
        let task = cx.spawn(async move |this, cx| {
            let result: Result<String, String> = async {
                if is_external {
                    ramag_app::run_blocking(move || {
                        validate_id_converter_program(&program).map_err(DomainError::InvalidConfig)
                    })
                    .await
                    .map_err(|error| error.to_string())?;
                }
                match direction {
                    DatabaseConverterTestDirection::ToInteger => {
                        ramag_app::convert_id_to_integer(&config, &input)
                            .await
                            .map(|value| value.to_string())
                    }
                    DatabaseConverterTestDirection::ToString => {
                        ramag_app::convert_id_to_string(&config, &input).await
                    }
                }
            }
            .await;

            let _ = this.update(cx, |this, cx| {
                if this.database_converter_test_seq != test_seq {
                    return;
                }
                this.database_converter_test_task = None;
                this.database_converter_test_state = match result {
                    Ok(output) => DatabaseConverterTestState::Success { direction, output },
                    Err(message) => DatabaseConverterTestState::Error { direction, message },
                };
                cx.notify();
            });
        });
        self.database_converter_test_task = Some(task);
    }

    fn invalidate_database_converter_test(&mut self) {
        self.database_converter_test_seq = self.database_converter_test_seq.wrapping_add(1);
        self.database_converter_test_task.take();
        self.database_converter_test_state = DatabaseConverterTestState::Idle;
    }

    fn schedule_database_search_save(&mut self, delay: Duration, cx: &mut Context<Self>) {
        self.database_save_debounce.take();
        if delay.is_zero() {
            self.request_database_search_save(cx);
            return;
        }

        self.database_save_debounce = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(delay).await;
            let _ = this.update(cx, |this, cx| {
                this.request_database_search_save(cx);
            });
        }));
    }

    fn database_search_draft(&self, cx: &gpui::App) -> DatabaseSearchSettings {
        DatabaseSearchSettings {
            id_conversion_enabled: self.database_enabled_draft,
            converter: self.database_converter_draft(cx),
        }
    }

    fn request_database_search_save(&mut self, cx: &mut Context<Self>) {
        if self.saving_database {
            self.database_save_pending = true;
            return;
        }
        self.database_save_pending = false;
        let next = self.database_search_draft(cx);
        if next == crate::database_search_settings(cx) {
            return;
        }
        let json = match next.to_json() {
            Ok(json) => json,
            Err(error) => {
                self.handle_database_search_save_error(error, cx);
                cx.notify();
                return;
            }
        };
        let Some(storage) = crate::theme::storage_from_cx(cx) else {
            self.handle_database_search_save_error("本地存储尚未初始化".to_string(), cx);
            cx.notify();
            return;
        };

        self.saving_database = true;
        cx.notify();
        let validate_program = next.id_conversion_enabled && next.converter.kind.is_external();
        let program = next.converter.external_program.clone();
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
                let draft_changed = this.database_search_draft(cx) != next;
                match result {
                    Ok(()) => {
                        // 每次写入均串行完成，成功后全局状态必须与当前落盘值一致。
                        set_database_search_settings(next.clone(), cx);
                    }
                    Err(error) => {
                        if !draft_changed {
                            this.handle_database_search_save_error(error, cx);
                        }
                    }
                }
                let save_latest = this.database_save_pending || draft_changed;
                this.database_save_pending = false;
                if save_latest {
                    this.schedule_database_search_save(Duration::ZERO, cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn handle_database_search_save_error(&mut self, error: String, cx: &gpui::App) {
        self.database_enabled_draft = crate::database_search_settings(cx).id_conversion_enabled;
        self.pending_notification = Some(Notification::error(format!("保存失败：{error}")));
    }
}
