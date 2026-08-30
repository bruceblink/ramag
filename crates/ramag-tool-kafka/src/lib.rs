//! Kafka 工作区 UI。
//!
//! 页面只通过 `KafkaService` 读取本地保存的配置和真实 Broker 数据，不在视图层生成
//! 集群、Topic 或消息样例。所有消息读取都要求用户给出 Topic、Partition、Offset 和
//! 有界预算，避免误把浏览操作变成无界消费。

use std::{ops::Range, sync::Arc};

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use chrono::{DateTime, Utc};
use gpui::{
    App, AppContext as _, ClickEvent, Context, Entity, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, ParentElement, Render, SharedString,
    StatefulInteractiveElement as _, Styled, Subscription, Window, div,
    prelude::FluentBuilder as _, px, uniform_list,
};
use gpui_component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _,
    button::ButtonVariants as _,
    h_flex,
    input::{Input, InputEvent, InputState},
    spinner::Spinner,
    v_flex,
};
use ramag_app::KafkaService;
use ramag_domain::{
    entities::{
        DEFAULT_KAFKA_MAX_BYTES, DEFAULT_KAFKA_MAX_CONCURRENT_PARTITIONS,
        DEFAULT_KAFKA_MAX_SCAN_SECONDS, KafkaClusterConfig, KafkaClusterId, KafkaClusterMetadata,
        KafkaMessagePage, KafkaMessageQuery, KafkaMessageRecord, KafkaMessageSearchField,
        KafkaMessageSearchQuery, KafkaReadOnlyState, KafkaSaslMechanism, KafkaSecurityProtocol,
        KafkaTlsConfig, KafkaTopic, MAX_KAFKA_QUERY_PARTITIONS, MAX_KAFKA_SCAN_RECORDS,
    },
    traits::{Tool, ToolMeta},
};
use serde::Serialize;

const MAX_VISIBLE_PARTITIONS: usize = 200;
const MESSAGE_PREVIEW_BYTES: usize = 512;
const MAX_KAFKA_EXPORT_BYTES: u64 = 64 * 1024 * 1024;

/// 创建 Kafka 工具的主视图，窗口生命周期由主壳持有。
pub fn create_kafka_view(
    service: Arc<KafkaService>,
    window: &mut Window,
    cx: &mut App,
) -> Entity<KafkaView> {
    cx.new(|cx| KafkaView::new(service, window, cx))
}

/// Kafka 工具在 Activity Bar 中显示的注册信息。
pub struct KafkaTool {
    meta: ToolMeta,
}

impl KafkaTool {
    pub const ID: &'static str = "kafka";

    /// 创建稳定的工具元数据；连接配置和运行时数据由 Kafka 工作区按需加载。
    pub fn new() -> Self {
        Self {
            meta: ToolMeta::new(Self::ID, "Kafka", "浏览集群、Topic、Partition 与消息")
                .with_icon("server"),
        }
    }
}

impl Default for KafkaTool {
    fn default() -> Self {
        Self::new()
    }
}

impl Tool for KafkaTool {
    fn meta(&self) -> &ToolMeta {
        &self.meta
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum KafkaSection {
    Overview,
    Topics,
    Messages,
    Config,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum KafkaRangeMode {
    Offset,
    Time,
}

impl KafkaRangeMode {
    const fn label(self) -> &'static str {
        match self {
            Self::Offset => "Offset",
            Self::Time => "时间",
        }
    }
}

impl KafkaSection {
    const ALL: [Self; 4] = [Self::Overview, Self::Topics, Self::Messages, Self::Config];

    const fn label(self) -> &'static str {
        match self {
            Self::Overview => "概览",
            Self::Topics => "Topics",
            Self::Messages => "消息",
            Self::Config => "配置",
        }
    }
}

/// Kafka 工作区的交互状态；运行时结果全部来自 `KafkaService`，空值表示尚未成功读取。
pub struct KafkaView {
    service: Arc<KafkaService>,
    clusters: Vec<KafkaClusterConfig>,
    selected_cluster_id: Option<KafkaClusterId>,
    selected_topic: Option<String>,
    metadata: Option<KafkaClusterMetadata>,
    topics: Vec<KafkaTopic>,
    message_page: Option<KafkaMessagePage>,
    selected_message: Option<usize>,
    section: KafkaSection,
    cluster_search: Entity<InputState>,
    topic_search: Entity<InputState>,
    message_search: Entity<InputState>,
    name: Entity<InputState>,
    bootstrap_servers: Entity<InputState>,
    client_id: Entity<InputState>,
    sasl_username: Entity<InputState>,
    sasl_password: Entity<InputState>,
    remark: Entity<InputState>,
    ca_cert_path: Entity<InputState>,
    client_cert_path: Entity<InputState>,
    client_key_path: Entity<InputState>,
    topic_input: Entity<InputState>,
    partition_input: Entity<InputState>,
    start_offset_input: Entity<InputState>,
    end_offset_input: Entity<InputState>,
    start_time_input: Entity<InputState>,
    end_time_input: Entity<InputState>,
    max_records_input: Entity<InputState>,
    search_fields: [bool; 3],
    range_mode: KafkaRangeMode,
    security_protocol: KafkaSecurityProtocol,
    sasl_mechanism: KafkaSaslMechanism,
    loading_clusters: bool,
    loading_runtime: bool,
    loading_messages: bool,
    testing: bool,
    saving: bool,
    deleting: bool,
    exporting: bool,
    runtime_request_id: u64,
    message_request_id: u64,
    notice: Option<(String, bool)>,
    focus_handle: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl KafkaView {
    /// 创建视图状态并异步加载本地配置；加载失败会保留在页面上，不伪装成空列表。
    pub fn new(service: Arc<KafkaService>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let cluster_search = input(window, cx, 4 * 1024, "搜索集群…", false, "");
        let topic_search = input(window, cx, 4 * 1024, "筛选 Topic…", false, "");
        let message_search = input(
            window,
            cx,
            4 * 1024,
            "搜索 Key / Value / Header（可选）",
            false,
            "",
        );
        let name = input(window, cx, 256, "集群名称", false, "");
        let bootstrap_servers = input(
            window,
            cx,
            16 * 1024,
            "broker-1:9092, broker-2:9092",
            false,
            "",
        );
        let client_id = input(window, cx, 256, "Client ID（可选）", false, "ramag-kafka");
        let sasl_username = input(window, cx, 4 * 1024, "SASL 用户名", false, "");
        let sasl_password = input(
            window,
            cx,
            64 * 1024,
            "SASL 密码（留空保持已保存密码）",
            true,
            "",
        );
        let remark = input(window, cx, 16 * 1024, "备注（可选）", false, "");
        let ca_cert_path = input(window, cx, 32 * 1024, "CA 证书路径（可选）", false, "");
        let client_cert_path = input(window, cx, 32 * 1024, "客户端证书路径（可选）", false, "");
        let client_key_path = input(window, cx, 32 * 1024, "客户端密钥路径（可选）", false, "");
        let topic_input = input(window, cx, 249, "Topic", false, "");
        let partition_input = input(window, cx, 4 * 1024, "Partition，例如 0,1,2", false, "0");
        let start_offset_input = input(window, cx, 32, "起始 Offset（可选）", false, "0");
        let end_offset_input = input(window, cx, 32, "结束 Offset（可选）", false, "");
        let start_time_input = input(window, cx, 64, "起始时间 RFC3339（可选）", false, "");
        let end_time_input = input(window, cx, 64, "结束时间 RFC3339（可选）", false, "");
        let max_records_input = input(window, cx, 32, "最多读取条数", false, "200");

        let mut subscriptions = Vec::new();
        for field in [
            &cluster_search,
            &topic_search,
            &message_search,
            &name,
            &bootstrap_servers,
            &client_id,
            &sasl_username,
            &sasl_password,
            &remark,
            &ca_cert_path,
            &client_cert_path,
            &client_key_path,
            &topic_input,
            &partition_input,
            &start_offset_input,
            &end_offset_input,
            &start_time_input,
            &end_time_input,
            &max_records_input,
        ] {
            subscriptions.push(cx.subscribe(field, |this, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    this.notice = None;
                    cx.notify();
                }
            }));
        }

        let mut this = Self {
            service,
            clusters: Vec::new(),
            selected_cluster_id: None,
            selected_topic: None,
            metadata: None,
            topics: Vec::new(),
            message_page: None,
            selected_message: None,
            section: KafkaSection::Overview,
            cluster_search,
            topic_search,
            message_search,
            name,
            bootstrap_servers,
            client_id,
            sasl_username,
            sasl_password,
            remark,
            ca_cert_path,
            client_cert_path,
            client_key_path,
            topic_input,
            partition_input,
            start_offset_input,
            end_offset_input,
            start_time_input,
            end_time_input,
            max_records_input,
            search_fields: [true, true, true],
            range_mode: KafkaRangeMode::Offset,
            security_protocol: KafkaSecurityProtocol::default(),
            sasl_mechanism: KafkaSaslMechanism::Plain,
            loading_clusters: true,
            loading_runtime: false,
            loading_messages: false,
            testing: false,
            saving: false,
            deleting: false,
            exporting: false,
            runtime_request_id: 0,
            message_request_id: 0,
            notice: None,
            focus_handle: cx.focus_handle(),
            _subscriptions: subscriptions,
        };
        this.load_clusters(window, cx);
        this
    }
}

mod helpers;
use helpers::*;
mod messages;
mod profile;
mod render_config;
mod render_main;
mod render_messages;
mod render_overview;
mod render_sidebar;
mod render_topics;
#[cfg(test)]
mod tests;

impl Focusable for KafkaView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for KafkaView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .id("kafka-root")
            .debug_selector(|| "kafka-root".into())
            .size_full()
            .bg(cx.theme().background)
            .child(self.render_sidebar(cx))
            .child(self.render_main(window, cx))
    }
}
