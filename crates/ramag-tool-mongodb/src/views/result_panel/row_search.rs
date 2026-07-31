//! MongoDB 结果行搜索模式与双向 ID 转换状态机。

use std::time::Duration;

use gpui::{Context, Task};
use ramag_domain::entities::{IdConverterConfig, parse_nonnegative_id_integer};

use super::ResultPanel;
use super::cell::Cell;

const ID_CONVERSION_DEBOUNCE: Duration = Duration::from_millis(250);

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum RowSearchMode {
    #[default]
    Normal,
    IdToInteger,
    IdToString,
}

impl RowSearchMode {
    pub(crate) const ALL: [Self; 3] = [Self::Normal, Self::IdToInteger, Self::IdToString];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Normal => "@TEXT",
            Self::IdToInteger => "@ID -> I",
            Self::IdToString => "@ID -> S",
        }
    }

    fn uses_id_conversion(self) -> bool {
        !matches!(self, Self::Normal)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ConvertedId {
    Integer(i64),
    String(String),
}

impl ConvertedId {
    pub(crate) fn display_preview(&self, max_characters: usize) -> String {
        match self {
            Self::Integer(value) => value.to_string(),
            Self::String(value) => {
                let mut characters = value.chars();
                let mut preview = characters.by_ref().take(max_characters).collect::<String>();
                if characters.next().is_some() {
                    preview.push('…');
                }
                preview
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RowFilter {
    Text(String),
    Integer(i64),
    ExactText(String),
    /// 转换中或转换失败时不得回退展示全部结果。
    Unresolved(String),
}

impl RowFilter {
    pub(crate) fn is_active(&self) -> bool {
        match self {
            Self::Text(query) => !query.is_empty(),
            Self::Integer(_) | Self::ExactText(_) | Self::Unresolved(_) => true,
        }
    }

    pub(crate) fn matches(&self, cell: &Cell) -> bool {
        match self {
            Self::Text(query) => {
                ramag_domain::entities::contains_case_insensitive(&cell.text, query)
            }
            Self::Integer(expected) => {
                matches!(cell.kind, "int" | "long")
                    && cell.text.parse::<i64>().ok() == Some(*expected)
            }
            Self::ExactText(expected) => cell.kind == "text" && cell.text == *expected,
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
    Ready(ConvertedId),
    Error(String),
}

enum IdConversionState {
    Idle,
    Converting {
        mode: RowSearchMode,
        input: String,
    },
    Ready {
        mode: RowSearchMode,
        input: String,
        output: ConvertedId,
    },
    Error {
        mode: RowSearchMode,
        input: String,
        message: String,
    },
}

impl IdConversionState {
    fn visible_status(
        &self,
        mode: RowSearchMode,
        current_input: &str,
    ) -> Option<RowSearchConversionStatus> {
        if current_input.is_empty() || !mode.uses_id_conversion() {
            return None;
        }
        Some(match self {
            Self::Converting {
                mode: converted_mode,
                input,
            } if *converted_mode == mode && input == current_input => {
                RowSearchConversionStatus::Converting
            }
            Self::Ready {
                mode: converted_mode,
                input,
                output,
            } if *converted_mode == mode && input == current_input => {
                RowSearchConversionStatus::Ready(output.clone())
            }
            Self::Error {
                mode: converted_mode,
                input,
                message,
            } if *converted_mode == mode && input == current_input => {
                RowSearchConversionStatus::Error(message.clone())
            }
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
        if mode.uses_id_conversion() && !ramag_ui::database_search_settings(cx).is_ready() {
            self.pending_notification = Some(
                gpui_component::notification::Notification::warning(
                    "请先在设置 → 数据库客户端 → 搜索配置中启用并配置雪花 ID 转换",
                )
                .autohide(true),
            );
            cx.notify();
            return;
        }

        self.cancel_id_conversion();
        self.row_search.mode = mode;
        self.row_search.conversion = IdConversionState::Idle;
        if mode.uses_id_conversion() {
            let input = self.row_filter_text(cx);
            self.row_search.last_input = input.clone();
            self.schedule_id_conversion(input, cx);
        } else {
            self.schedule_row_view(false, cx);
        }
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
        if self.row_search.mode.uses_id_conversion() {
            self.schedule_id_conversion(input, cx);
        } else {
            self.schedule_row_view(true, cx);
        }
    }

    pub(super) fn on_database_search_settings_changed(&mut self, cx: &mut Context<Self>) {
        let settings = ramag_ui::database_search_settings(cx);
        if !settings.is_ready() {
            if self.row_search.mode.uses_id_conversion() {
                self.cancel_id_conversion();
                self.row_search.mode = RowSearchMode::Normal;
                self.row_search.conversion = IdConversionState::Idle;
                self.schedule_row_view(false, cx);
            }
            cx.notify();
            return;
        }
        if self.row_search.mode.uses_id_conversion() {
            let input = self.row_filter_text(cx);
            self.schedule_id_conversion(input, cx);
        }
        cx.notify();
    }

    pub(crate) fn effective_row_filter(&self, cx: &gpui::App) -> RowFilter {
        let input = self.row_filter_text(cx);
        if input.is_empty() || !self.row_search.mode.uses_id_conversion() {
            return RowFilter::Text(input.to_lowercase());
        }
        match &self.row_search.conversion {
            IdConversionState::Ready {
                mode,
                input: converted_input,
                output,
            } if *mode == self.row_search.mode && converted_input == &input => match output {
                ConvertedId::Integer(value) => RowFilter::Integer(*value),
                ConvertedId::String(value) => RowFilter::ExactText(value.clone()),
            },
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

    pub(crate) fn converted_row_search(
        &self,
        cx: &gpui::App,
    ) -> Option<(RowSearchMode, ConvertedId)> {
        match self.row_search_conversion_status(cx) {
            Some(RowSearchConversionStatus::Ready(output)) => Some((self.row_search.mode, output)),
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

    pub(super) fn row_filter_text(&self, cx: &gpui::App) -> String {
        self.row_filter.read(cx).value().trim().to_string()
    }

    fn schedule_id_conversion(&mut self, input: String, cx: &mut Context<Self>) {
        self.cancel_id_conversion();
        if input.is_empty() {
            self.row_search.conversion = IdConversionState::Idle;
            self.schedule_row_view(false, cx);
            return;
        }
        let settings = ramag_ui::database_search_settings(cx);
        let mode = self.row_search.mode;
        if !settings.is_ready() {
            self.row_search.conversion = IdConversionState::Error {
                mode,
                input,
                message: "雪花 ID 转换未启用或配置无效".to_string(),
            };
            self.invalidate_row_view();
            cx.notify();
            return;
        }

        self.row_search.conversion_seq = self.row_search.conversion_seq.wrapping_add(1);
        let conversion_seq = self.row_search.conversion_seq;
        let config = settings.converter;
        if !config.kind.is_external() {
            match convert_local(mode, &config, &input) {
                Ok(output) => {
                    self.row_search.conversion = IdConversionState::Ready {
                        mode,
                        input,
                        output,
                    };
                    self.schedule_row_view(false, cx);
                }
                Err(message) => {
                    self.row_search.conversion = IdConversionState::Error {
                        mode,
                        input,
                        message,
                    };
                    self.invalidate_row_view();
                    cx.notify();
                }
            }
            return;
        }

        self.row_search.conversion = IdConversionState::Converting {
            mode,
            input: input.clone(),
        };
        self.invalidate_row_view();
        cx.notify();
        let task = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(ID_CONVERSION_DEBOUNCE).await;
            let result = match mode {
                RowSearchMode::IdToInteger => ramag_app::convert_id_to_integer(&config, &input)
                    .await
                    .map(ConvertedId::Integer),
                RowSearchMode::IdToString => ramag_app::convert_id_to_string(&config, &input)
                    .await
                    .map(ConvertedId::String),
                RowSearchMode::Normal => Err("当前搜索模式不需要 ID 转换".to_string()),
            };
            let _ = this.update(cx, |this, cx| {
                if this.row_search.mode != mode
                    || this.row_search.conversion_seq != conversion_seq
                    || this.row_filter_text(cx) != input
                {
                    return;
                }
                match result {
                    Ok(output) => {
                        this.row_search.conversion = IdConversionState::Ready {
                            mode,
                            input: input.clone(),
                            output,
                        };
                        this.schedule_row_view(false, cx);
                    }
                    Err(message) => {
                        this.row_search.conversion = IdConversionState::Error {
                            mode,
                            input: input.clone(),
                            message,
                        };
                        this.invalidate_row_view();
                        cx.notify();
                    }
                }
            });
        });
        self.row_search.conversion_task = Some(task);
    }

    fn cancel_id_conversion(&mut self) {
        self.row_search.conversion_seq = self.row_search.conversion_seq.wrapping_add(1);
        self.row_search.conversion_task.take();
    }
}

fn convert_local(
    mode: RowSearchMode,
    config: &IdConverterConfig,
    input: &str,
) -> Result<ConvertedId, String> {
    match mode {
        RowSearchMode::Normal => Err("当前搜索模式不需要 ID 转换".to_string()),
        RowSearchMode::IdToInteger => config.decode_local(input).map(ConvertedId::Integer),
        RowSearchMode::IdToString => {
            let value = parse_nonnegative_id_integer(input)?;
            config.encode_local(value).map(ConvertedId::String)
        }
    }
}

#[cfg(test)]
mod tests {
    use ramag_domain::entities::IdConverterConfig;

    use super::{ConvertedId, RowFilter, RowSearchMode, convert_local};
    use crate::views::result_panel::cell::Cell;

    #[test]
    fn search_mode_labels_match_sql_result_search() {
        assert_eq!(RowSearchMode::Normal.label(), "@TEXT");
        assert_eq!(RowSearchMode::IdToInteger.label(), "@ID -> I");
        assert_eq!(RowSearchMode::IdToString.label(), "@ID -> S");
    }

    #[test]
    fn converted_filters_match_only_the_target_bson_type_and_value() {
        let integer = RowFilter::Integer(42);
        assert!(integer.matches(&Cell {
            text: "42".into(),
            kind: "long",
        }));
        assert!(!integer.matches(&Cell {
            text: "42".into(),
            kind: "text",
        }));

        let text = RowFilter::ExactText("qwe".into());
        assert!(text.matches(&Cell {
            text: "qwe".into(),
            kind: "text",
        }));
        assert!(!text.matches(&Cell {
            text: "QWE".into(),
            kind: "text",
        }));
    }

    #[test]
    fn local_conversion_uses_the_shared_database_configuration() -> Result<(), String> {
        let config = IdConverterConfig::default();

        assert_eq!(
            convert_local(RowSearchMode::IdToInteger, &config, "qwe")?,
            ConvertedId::Integer(82_489)
        );
        assert_eq!(
            convert_local(RowSearchMode::IdToString, &config, "82489")?,
            ConvertedId::String("qwe".into())
        );
        Ok(())
    }
}
