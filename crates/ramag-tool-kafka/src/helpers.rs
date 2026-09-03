use super::*;

pub(super) fn input(
    window: &mut Window,
    cx: &mut Context<KafkaView>,
    max_bytes: usize,
    placeholder: &'static str,
    masked: bool,
    default_value: &str,
) -> Entity<InputState> {
    cx.new(|cx| {
        InputState::new(window, cx)
            .validate(move |value, _| value.len() <= max_bytes)
            .placeholder(placeholder)
            .masked(masked)
            .default_value(default_value.to_string())
    })
}

pub(super) fn set_value(
    field: &Entity<InputState>,
    value: impl Into<String>,
    window: &mut Window,
    cx: &mut Context<KafkaView>,
) {
    let value = value.into();
    field.update(cx, |state, cx| state.set_value(value, window, cx));
}

pub(super) fn value(field: &Entity<InputState>, cx: &App) -> String {
    field.read(cx).value().trim().to_string()
}

pub(super) fn optional_value(field: &Entity<InputState>, cx: &App) -> Option<String> {
    let value = value(field, cx);
    (!value.is_empty()).then_some(value)
}

pub(super) fn parse_bootstrap_servers(value: &str) -> Vec<String> {
    value
        .split([',', '\n', '\r'])
        .map(str::trim)
        .filter(|server| !server.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

pub(super) fn parse_i64_input(
    field: &Entity<InputState>,
    cx: &App,
    label: &str,
) -> Result<i64, String> {
    let text = value(field, cx);
    text.parse::<i64>()
        .map_err(|_| format!("{label} 必须是非负整数"))
        .and_then(|number| {
            if number < 0 {
                Err(format!("{label} 必须是非负整数"))
            } else {
                Ok(number)
            }
        })
}

/// 解析逗号或空白分隔的 Partition，并在进入驱动前拒绝重复和超大范围。
pub(super) fn parse_partition_list(text: &str) -> Result<Vec<i32>, String> {
    let parts = text
        .split([',', ' ', '\n', '\r', '\t'])
        .filter(|part| !part.trim().is_empty())
        .map(str::trim)
        .collect::<Vec<_>>();
    if parts.is_empty() {
        return Err("Partition 至少需要一个非负整数".into());
    }
    if parts.len() > MAX_KAFKA_QUERY_PARTITIONS {
        return Err(format!(
            "Partition 数量不能超过 {MAX_KAFKA_QUERY_PARTITIONS} 个"
        ));
    }
    let mut partitions = Vec::with_capacity(parts.len());
    for part in parts {
        let partition = part
            .parse::<i32>()
            .map_err(|_| format!("Partition 必须是非负整数：{part}"))?;
        if partition < 0 {
            return Err(format!("Partition 必须是非负整数：{part}"));
        }
        if partitions.contains(&partition) {
            return Err(format!("Partition 不能重复：{partition}"));
        }
        partitions.push(partition);
    }
    Ok(partitions)
}

/// 解析 RFC3339 时间并统一为 UTC，保持 Kafka 时间定位的输入含义稳定。
pub(super) fn parse_datetime_input(
    field: &Entity<InputState>,
    cx: &App,
    label: &str,
) -> Result<Option<DateTime<Utc>>, String> {
    parse_datetime_text(&value(field, cx), label)
}

pub(super) fn parse_datetime_text(
    text: &str,
    label: &str,
) -> Result<Option<DateTime<Utc>>, String> {
    if text.is_empty() {
        return Ok(None);
    }
    DateTime::parse_from_rfc3339(text)
        .map(|value| Some(value.with_timezone(&Utc)))
        .map_err(|_| format!("{label} 必须是 RFC3339，例如 2026-08-30T10:00:00Z"))
}

pub(super) fn optional_i64_input(
    field: &Entity<InputState>,
    cx: &App,
    label: &str,
) -> Result<Option<i64>, String> {
    let text = value(field, cx);
    if text.is_empty() {
        Ok(None)
    } else {
        parse_i64_input(field, cx, label).map(Some)
    }
}

pub(super) fn parse_usize_input(
    field: &Entity<InputState>,
    cx: &App,
    label: &str,
) -> Result<usize, String> {
    let text = value(field, cx);
    text.parse::<usize>()
        .map_err(|_| format!("{label} 必须是 1 - {MAX_KAFKA_SCAN_RECORDS} 之间的整数"))
}

pub(super) fn field<E: IntoElement>(label: &'static str, input: E, width: f32) -> gpui::Div {
    let control = if width > 0.0 {
        div().w(px(width)).flex_none().child(input)
    } else {
        div().w_full().min_w_0().child(input)
    };
    v_flex()
        .flex_none()
        .gap(px(5.0))
        // A field can be used in both horizontal rows and vertical forms. Keep its
        // default flex behavior neutral; the horizontal caller opts into expansion.
        .when(width == 0.0, |this| this.w_full().min_w_0())
        .child(
            div()
                .w_full()
                .text_xs()
                .text_color(gpui::hsla(0.0, 0.0, 0.5, 1.0))
                .child(label),
        )
        .child(control)
}

pub(super) fn flexible_field<E: IntoElement>(label: &'static str, input: E) -> gpui::Div {
    field(label, input, 0.0).flex_1().min_w_0()
}

pub(super) fn section_heading(
    title: impl Into<SharedString>,
    subtitle: impl Into<SharedString>,
    theme: &gpui_component::Theme,
) -> impl IntoElement {
    v_flex()
        .w_full()
        .gap(px(2.0))
        .child(
            div()
                .text_sm()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .child(title.into()),
        )
        .child(
            div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(subtitle.into()),
        )
}

pub(super) fn metric_card(
    label: &'static str,
    value: usize,
    theme: &gpui_component::Theme,
) -> impl IntoElement {
    v_flex()
        .flex_1()
        .gap(px(4.0))
        .p(px(14.0))
        .border_1()
        .border_color(theme.border)
        .rounded(px(6.0))
        .bg(theme.secondary.opacity(0.45))
        .child(
            div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(label),
        )
        .child(
            div()
                .text_xl()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .child(value.to_string()),
        )
}

pub(super) fn broker_row(
    broker: &ramag_domain::entities::KafkaBroker,
    theme: &gpui_component::Theme,
) -> impl IntoElement {
    h_flex()
        .w_full()
        .items_center()
        .gap(px(10.0))
        .px(px(12.0))
        .py(px(9.0))
        .border_b_1()
        .border_color(theme.border)
        .child(
            div()
                .w(px(58.0))
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(format!("id {}", broker.id)),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_sm()
                .truncate()
                .child(format!("{}:{}", broker.host, broker.port)),
        )
        .child(
            div()
                .w(px(120.0))
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(broker.version.clone().unwrap_or_else(|| "版本未知".into())),
        )
        .child(
            div()
                .w(px(76.0))
                .text_xs()
                .text_color(if broker.is_controller {
                    theme.accent
                } else {
                    theme.muted_foreground
                })
                .child(if broker.is_controller {
                    "Controller"
                } else {
                    "Broker"
                }),
        )
}

pub(super) fn partition_row(
    partition: &ramag_domain::entities::KafkaPartition,
    theme: &gpui_component::Theme,
) -> impl IntoElement {
    v_flex()
        .w_full()
        .gap(px(4.0))
        .px(px(12.0))
        .py(px(9.0))
        .border_b_1()
        .border_color(theme.border)
        .child(
            h_flex()
                .justify_between()
                .child(div().text_sm().child(format!("Partition {}", partition.id)))
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(format!("Leader {}", display_option_i32(partition.leader))),
                ),
        )
        .child(
            div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(format!(
                    "ISR [{}] · replicas [{}]",
                    join_ids(&partition.isr),
                    join_ids(&partition.replicas)
                )),
        )
        .child(
            div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(format!(
                    "Offset {} → {}",
                    display_option_i64(partition.low_watermark),
                    display_option_i64(partition.high_watermark)
                )),
        )
}

pub(super) fn summary_row(
    label: &'static str,
    value: &str,
    theme: &gpui_component::Theme,
) -> impl IntoElement {
    v_flex()
        .gap(px(2.0))
        .child(
            div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(label),
        )
        .child(div().text_sm().truncate().child(value.to_owned()))
}

pub(super) fn value_block(
    label: &'static str,
    value: String,
    theme: &gpui_component::Theme,
) -> impl IntoElement {
    v_flex()
        .min_w_0()
        .gap(px(4.0))
        .child(
            div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(label),
        )
        .child(
            div()
                .w_full()
                .min_w_0()
                .p(px(8.0))
                .rounded(px(4.0))
                .bg(theme.muted.opacity(0.5))
                .text_xs()
                .whitespace_normal()
                .child(value),
        )
}

pub(super) fn message_table_header(theme: &gpui_component::Theme) -> impl IntoElement {
    h_flex()
        .w_full()
        .min_w(px(MESSAGE_TABLE_MIN_WIDTH))
        .items_center()
        .gap(px(10.0))
        .px(px(12.0))
        .py(px(8.0))
        .border_b_1()
        .border_color(theme.border)
        .bg(theme.secondary)
        .text_xs()
        .text_color(theme.muted_foreground)
        .child(div().w(px(56.0)).flex_none().child("分区"))
        .child(div().w(px(90.0)).flex_none().child("Offset"))
        .child(div().w(px(150.0)).flex_none().child("Timestamp"))
        .child(div().w(px(100.0)).flex_none().child("Key"))
        .child(div().flex_1().min_w(px(180.0)).child("Value 预览"))
        .child(div().w(px(70.0)).flex_none().child("Headers"))
}

pub(super) fn format_headers(record: &KafkaMessageRecord) -> String {
    if record.headers.is_empty() {
        return "<none>".into();
    }
    record
        .headers
        .iter()
        .map(|header| {
            let value = header
                .value
                .as_deref()
                .map(|value| ramag_domain::entities::preview_bytes(value, 160).text)
                .unwrap_or_else(|| "<null>".into());
            format!("{}: {}", header.key, value)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub(super) fn display_option_i32(value: Option<i32>) -> String {
    value.map_or_else(|| "未知".into(), |value| value.to_string())
}

pub(super) fn display_option_i64(value: Option<i64>) -> String {
    value.map_or_else(|| "未知".into(), |value| value.to_string())
}

pub(super) fn join_ids(values: &[i32]) -> String {
    values
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

#[derive(Debug, Serialize)]
pub(super) struct KafkaMessageExport {
    topic: String,
    partition: i32,
    offset: i64,
    timestamp: Option<String>,
    key_base64: Option<String>,
    value_base64: Option<String>,
    headers: Vec<KafkaHeaderExport>,
}

#[derive(Debug, Serialize)]
pub(super) struct KafkaHeaderExport {
    key: String,
    value_base64: Option<String>,
}

impl From<&KafkaMessageRecord> for KafkaMessageExport {
    fn from(record: &KafkaMessageRecord) -> Self {
        Self {
            topic: record.topic.clone(),
            partition: record.partition,
            offset: record.offset,
            timestamp: record.timestamp.map(|value| value.to_rfc3339()),
            key_base64: record.key.as_deref().map(bytes_to_base64),
            value_base64: record.value.as_deref().map(bytes_to_base64),
            headers: record
                .headers
                .iter()
                .map(|header| KafkaHeaderExport {
                    key: header.key.clone(),
                    value_base64: header.value.as_deref().map(bytes_to_base64),
                })
                .collect(),
        }
    }
}

pub(super) fn format_message_json(record: &KafkaMessageRecord) -> String {
    serde_json::to_string_pretty(&KafkaMessageExport::from(record))
        .unwrap_or_else(|_| "{\"error\":\"消息无法编码为 JSON\"}".into())
}

pub(super) fn encode_value_hex(record: &KafkaMessageRecord) -> String {
    record
        .value
        .as_deref()
        .map(bytes_to_hex)
        .unwrap_or_else(|| "<null>".into())
}

pub(super) fn encode_value_base64(record: &KafkaMessageRecord) -> String {
    record
        .value
        .as_deref()
        .map(bytes_to_base64)
        .unwrap_or_else(|| "<null>".into())
}

pub(super) fn bytes_to_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len().saturating_mul(2));
    for &byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

pub(super) fn bytes_to_base64(bytes: &[u8]) -> String {
    BASE64.encode(bytes)
}

pub(super) fn format_timestamp(timestamp: Option<DateTime<Utc>>) -> String {
    timestamp.map_or_else(|| "未知".into(), |value| value.to_rfc3339())
}

pub(super) fn search_field_label(field: KafkaMessageSearchField) -> &'static str {
    match field {
        KafkaMessageSearchField::Key => "Key",
        KafkaMessageSearchField::Value => "Value",
        KafkaMessageSearchField::Headers => "Headers",
    }
}

pub(super) fn suggested_message_file_name(record: &KafkaMessageRecord) -> String {
    let topic = record
        .topic
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-') {
                character
            } else {
                '_'
            }
        })
        .collect::<String>();
    let topic = if topic.is_empty() { "message" } else { &topic };
    format!("{topic}-p{}-o{}.json", record.partition, record.offset)
}
