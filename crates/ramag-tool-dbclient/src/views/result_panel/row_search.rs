//! 结果行搜索模式与 `@ID` 外部转换状态机。

use std::time::Duration;

use gpui::{Context, Task};
use ramag_domain::entities::Value;

use super::{ResultPanel, ResultPanelEvent};

const ID_CONVERSION_DEBOUNCE: Duration = Duration::from_millis(250);

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum RowSearchMode {
    #[default]
    Normal,
    Id,
}

impl RowSearchMode {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Normal => "@TEXT",
            Self::Id => "@ID",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RowFilter {
    Text(String),
    Id(i64),
    /// 转换中或转换失败时不得回退展示全部结果。
    Unresolved(String),
}

impl RowFilter {
    pub(crate) fn is_active(&self) -> bool {
        match self {
            Self::Text(query) => !query.is_empty(),
            Self::Id(_) | Self::Unresolved(_) => true,
        }
    }

    pub(crate) fn matches(&self, value: &Value) -> bool {
        match self {
            Self::Text(query) => value.contains_query_lower(query),
            Self::Id(expected) => matches!(value, Value::Int(actual) if actual == expected),
            Self::Unresolved(_) => false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RowSearchBlocker {
    Converting,
    Error(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RowSearchConversionStatus {
    Converting,
    Ready(i64),
    Error(String),
}

enum IdConversionState {
    Idle,
    Converting { input: String },
    Ready { input: String, id: i64 },
    Error { input: String, message: String },
}

impl IdConversionState {
    fn visible_status(
        &self,
        mode: RowSearchMode,
        current_input: &str,
    ) -> Option<RowSearchConversionStatus> {
        if current_input.is_empty() || matches!(mode, RowSearchMode::Normal) {
            return None;
        }

        Some(match self {
            Self::Converting { input } if input == current_input => {
                RowSearchConversionStatus::Converting
            }
            Self::Ready { input, id } if input == current_input => {
                RowSearchConversionStatus::Ready(*id)
            }
            Self::Error { input, message } if input == current_input => {
                RowSearchConversionStatus::Error(message.clone())
            }
            // 输入变化后，旧结果在新转换完成前不可见。
            _ => RowSearchConversionStatus::Converting,
        })
    }
}

pub(super) struct RowSearchState {
    mode: RowSearchMode,
    last_input: String,
    conversion_seq: u64,
    conversion: IdConversionState,
    conversion_task: Option<Task<()>>,
}

impl Default for RowSearchState {
    fn default() -> Self {
        Self {
            mode: RowSearchMode::Normal,
            last_input: String::new(),
            conversion_seq: 0,
            conversion: IdConversionState::Idle,
            conversion_task: None,
        }
    }
}

impl ResultPanel {
    pub(crate) fn row_search_mode(&self) -> RowSearchMode {
        self.row_search.mode
    }

    pub(crate) fn set_row_search_mode(&mut self, mode: RowSearchMode, cx: &mut Context<Self>) {
        if self.row_search.mode == mode {
            return;
        }
        if matches!(mode, RowSearchMode::Id) && !ramag_ui::database_search_settings(cx).is_ready() {
            self.pending_notification = Some(
                gpui_component::notification::Notification::warning(
                    "请先在设置 → 数据库客户端 → 搜索设置中启用并配置转换程序",
                )
                .autohide(true),
            );
            cx.notify();
            return;
        }

        self.cancel_id_conversion();
        self.row_search.mode = mode;
        self.row_search.conversion = IdConversionState::Idle;
        self.invalidate_display_view();
        if matches!(mode, RowSearchMode::Id) {
            let input = self.row_filter_text(cx);
            self.row_search.last_input = input.clone();
            self.schedule_id_conversion(input, cx);
        }
        cx.emit(ResultPanelEvent::RowSearchChanged);
        cx.notify();
    }

    /// InputState 的 observe 同时覆盖键盘输入和清除按钮的程序化 set_value。
    pub(super) fn on_row_filter_input_updated(&mut self, cx: &mut Context<Self>) {
        let input = self.row_filter_text(cx);
        if self.row_search.last_input == input {
            cx.notify();
            return;
        }
        self.row_search.last_input = input.clone();
        self.invalidate_display_view();
        if matches!(self.row_search.mode, RowSearchMode::Id) {
            self.schedule_id_conversion(input, cx);
        }
        cx.emit(ResultPanelEvent::RowSearchChanged);
        cx.notify();
    }

    pub(super) fn on_database_search_settings_changed(&mut self, cx: &mut Context<Self>) {
        let settings = ramag_ui::database_search_settings(cx);
        if !settings.is_ready() {
            if matches!(self.row_search.mode, RowSearchMode::Id) {
                self.cancel_id_conversion();
                self.row_search.mode = RowSearchMode::Normal;
                self.row_search.conversion = IdConversionState::Idle;
                self.invalidate_display_view();
            }
            cx.emit(ResultPanelEvent::RowSearchChanged);
            cx.notify();
            return;
        }
        if matches!(self.row_search.mode, RowSearchMode::Id) {
            let input = self.row_filter_text(cx);
            self.schedule_id_conversion(input, cx);
        }
        cx.emit(ResultPanelEvent::RowSearchChanged);
        cx.notify();
    }

    pub(crate) fn effective_row_filter(&self, cx: &gpui::App) -> RowFilter {
        let input = self.row_filter_text(cx);
        if input.is_empty() || matches!(self.row_search.mode, RowSearchMode::Normal) {
            return RowFilter::Text(input.to_lowercase());
        }
        match &self.row_search.conversion {
            IdConversionState::Ready {
                input: converted_input,
                id,
            } if converted_input == &input => RowFilter::Id(*id),
            _ => RowFilter::Unresolved(input),
        }
    }

    pub(crate) fn row_search_blocker(&self, cx: &gpui::App) -> Option<RowSearchBlocker> {
        match self.row_search_conversion_status(cx) {
            Some(RowSearchConversionStatus::Converting) => Some(RowSearchBlocker::Converting),
            Some(RowSearchConversionStatus::Error(message)) => {
                Some(RowSearchBlocker::Error(message))
            }
            Some(RowSearchConversionStatus::Ready(_)) | None => None,
        }
    }

    pub(crate) fn converted_row_search_id(&self, cx: &gpui::App) -> Option<i64> {
        match self.row_search_conversion_status(cx) {
            Some(RowSearchConversionStatus::Ready(id)) => Some(id),
            _ => None,
        }
    }

    pub(crate) fn row_search_conversion_status(
        &self,
        cx: &gpui::App,
    ) -> Option<RowSearchConversionStatus> {
        let input = self.row_filter_text(cx);
        self.row_search
            .conversion
            .visible_status(self.row_search.mode, &input)
    }

    fn schedule_id_conversion(&mut self, input: String, cx: &mut Context<Self>) {
        self.cancel_id_conversion();
        if input.is_empty() {
            self.row_search.conversion = IdConversionState::Idle;
            self.invalidate_display_view();
            return;
        }
        let settings = ramag_ui::database_search_settings(cx);
        if !settings.is_ready() {
            self.row_search.conversion = IdConversionState::Error {
                input,
                message: "外部 ID 转换未启用或尚未配置转换程序".to_string(),
            };
            self.invalidate_display_view();
            return;
        }

        self.row_search.conversion_seq = self.row_search.conversion_seq.wrapping_add(1);
        let conversion_seq = self.row_search.conversion_seq;
        self.row_search.conversion = IdConversionState::Converting {
            input: input.clone(),
        };
        self.invalidate_display_view();
        let program = settings.id_converter_program;
        let task = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(ID_CONVERSION_DEBOUNCE).await;
            let result = crate::id_converter::convert_id(&program, &input).await;
            let _ = this.update(cx, |this, cx| {
                if !matches!(this.row_search.mode, RowSearchMode::Id)
                    || this.row_search.conversion_seq != conversion_seq
                    || this.row_filter_text(cx) != input
                {
                    return;
                }
                this.row_search.conversion = match result {
                    Ok(id) => IdConversionState::Ready {
                        input: input.clone(),
                        id,
                    },
                    Err(message) => IdConversionState::Error {
                        input: input.clone(),
                        message,
                    },
                };
                this.invalidate_display_view();
                cx.emit(ResultPanelEvent::RowSearchChanged);
                cx.notify();
            });
        });
        self.row_search.conversion_task = Some(task);
    }

    pub(super) fn cancel_id_conversion(&mut self) {
        self.row_search.conversion_seq = self.row_search.conversion_seq.wrapping_add(1);
        self.row_search.conversion_task.take();
    }
}

#[cfg(test)]
mod tests {
    use ramag_domain::entities::Value;

    use super::{IdConversionState, RowFilter, RowSearchConversionStatus, RowSearchMode};

    #[test]
    fn search_mode_labels_use_explicit_tags() {
        assert_eq!(RowSearchMode::Normal.label(), "@TEXT");
        assert_eq!(RowSearchMode::Id.label(), "@ID");
    }

    #[test]
    fn id_filter_matches_only_the_exact_integer_value() {
        let filter = RowFilter::Id(42);

        assert!(filter.matches(&Value::Int(42)));
        assert!(!filter.matches(&Value::Int(142)));
        assert!(!filter.matches(&Value::Text("42".into())));
        assert!(!filter.matches(&Value::Float(42.0)));
    }

    #[test]
    fn normal_filter_keeps_existing_contains_behavior() {
        let filter = RowFilter::Text("alpha".into());

        assert!(filter.matches(&Value::Text("Alpha Beta".into())));
        assert!(!filter.matches(&Value::Text("Beta".into())));
    }

    #[test]
    fn conversion_status_only_exposes_the_current_id_input() {
        let ready = IdConversionState::Ready {
            input: "external-id".into(),
            id: 42,
        };

        assert_eq!(
            ready.visible_status(RowSearchMode::Id, "external-id"),
            Some(RowSearchConversionStatus::Ready(42))
        );
        assert_eq!(
            ready.visible_status(RowSearchMode::Id, "new-input"),
            Some(RowSearchConversionStatus::Converting)
        );
        assert_eq!(ready.visible_status(RowSearchMode::Id, ""), None);
        assert_eq!(
            ready.visible_status(RowSearchMode::Normal, "external-id"),
            None
        );
    }
}
