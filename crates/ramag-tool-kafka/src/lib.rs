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
    InteractiveElement as _, IntoElement, ParentElement, Render, ScrollHandle, SharedString,
    StatefulInteractiveElement as _, Styled, Subscription, UniformListScrollHandle, Window, div,
    prelude::FluentBuilder as _, px, uniform_list,
};
use gpui_component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _,
    button::ButtonVariants as _,
    h_flex,
    input::{Input, InputEvent, InputState},
    scroll::{Scrollbar, ScrollbarShow},
    spinner::Spinner,
    v_flex,
};
use ramag_app::KafkaService;
use ramag_domain::{
    entities::{
        DEFAULT_KAFKA_MAX_BYTES, DEFAULT_KAFKA_MAX_CONCURRENT_PARTITIONS,
        DEFAULT_KAFKA_MAX_SCAN_SECONDS, KafkaAcl, KafkaAclFilter, KafkaAclOperation,
        KafkaAclPatternType, KafkaAclPermission, KafkaAclResourceType, KafkaClusterConfig,
        KafkaClusterId, KafkaClusterMetadata, KafkaConfigEntry, KafkaConfigResourceType,
        KafkaConfigUpdateOperation, KafkaConfigUpdateRequest, KafkaConsumerGroup, KafkaMessagePage,
        KafkaMessageQuery, KafkaMessageRecord, KafkaMessageSearchField, KafkaMessageSearchQuery,
        KafkaReadOnlyState, KafkaSaslMechanism, KafkaSecurityProtocol, KafkaTlsConfig, KafkaTopic,
        KafkaTopicCreateRequest, KafkaTopicPartitionExpansion, MAX_KAFKA_ACL_HOST_BYTES,
        MAX_KAFKA_ACL_RESOURCE_NAME_BYTES, MAX_KAFKA_CONFIG_RESOURCE_NAME_BYTES,
        MAX_KAFKA_CONFIG_VALUE_BYTES, MAX_KAFKA_PARTITIONS, MAX_KAFKA_QUERY_PARTITIONS,
        MAX_KAFKA_REPLICAS, MAX_KAFKA_SCAN_RECORDS,
    },
    traits::{Tool, ToolMeta},
};
use serde::Serialize;

const MAX_VISIBLE_PARTITIONS: usize = 200;
const MESSAGE_PREVIEW_BYTES: usize = 512;
const MAX_KAFKA_EXPORT_BYTES: u64 = 64 * 1024 * 1024;
const DEFAULT_MESSAGE_PAGE_SIZE: usize = 100;
const DEFAULT_TOPIC_PAGE_SIZE: usize = 50;

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
    ConsumerGroups,
    Acls,
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
    const ALL: [Self; 6] = [
        Self::Overview,
        Self::Topics,
        Self::Messages,
        Self::ConsumerGroups,
        Self::Acls,
        Self::Config,
    ];

    const fn label(self) -> &'static str {
        match self {
            Self::Overview => "概览",
            Self::Topics => "Topics",
            Self::Messages => "消息",
            Self::ConsumerGroups => "消费者组",
            Self::Acls => "ACL",
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
    topic_page_index: usize,
    topic_page_size: usize,
    topic_scroll: UniformListScrollHandle,
    consumer_groups: Vec<KafkaConsumerGroup>,
    selected_consumer_group: Option<String>,
    consumer_group_error: Option<String>,
    message_page: Option<KafkaMessagePage>,
    selected_message: Option<usize>,
    message_page_index: usize,
    message_page_size: usize,
    message_scroll: UniformListScrollHandle,
    topic_partition_scroll: ScrollHandle,
    consumer_group_scroll: UniformListScrollHandle,
    acls: Vec<KafkaAcl>,
    selected_acl: Option<KafkaAcl>,
    acls_loaded: bool,
    acl_error: Option<String>,
    acl_scroll: UniformListScrollHandle,
    section: KafkaSection,
    cluster_search: Entity<InputState>,
    topic_search: Entity<InputState>,
    consumer_group_search: Entity<InputState>,
    acl_principal_filter: Entity<InputState>,
    acl_host_filter: Entity<InputState>,
    acl_resource_name_filter: Entity<InputState>,
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
    config_resource_name: Entity<InputState>,
    config_value: Entity<InputState>,
    topic_input: Entity<InputState>,
    partition_input: Entity<InputState>,
    topic_create_name: Entity<InputState>,
    topic_create_partitions: Entity<InputState>,
    topic_create_replication_factor: Entity<InputState>,
    topic_target_partitions: Entity<InputState>,
    acl_principal: Entity<InputState>,
    acl_host: Entity<InputState>,
    acl_resource_name: Entity<InputState>,
    start_offset_input: Entity<InputState>,
    end_offset_input: Entity<InputState>,
    start_time_input: Entity<InputState>,
    end_time_input: Entity<InputState>,
    max_records_input: Entity<InputState>,
    search_fields: [bool; 3],
    range_mode: KafkaRangeMode,
    security_protocol: KafkaSecurityProtocol,
    sasl_mechanism: KafkaSaslMechanism,
    config_resource_type: KafkaConfigResourceType,
    acl_filter_resource_type: Option<KafkaAclResourceType>,
    acl_filter_pattern_type: Option<KafkaAclPatternType>,
    acl_filter_operation: Option<KafkaAclOperation>,
    acl_filter_permission: Option<KafkaAclPermission>,
    acl_resource_type: KafkaAclResourceType,
    acl_pattern_type: KafkaAclPatternType,
    acl_operation_kind: KafkaAclOperation,
    acl_permission: KafkaAclPermission,
    config_entries: Vec<KafkaConfigEntry>,
    editing_config_key: Option<String>,
    read_only: KafkaReadOnlyState,
    loading_clusters: bool,
    loading_runtime: bool,
    loading_messages: bool,
    loading_consumer_groups: bool,
    loading_acls: bool,
    loading_configs: bool,
    testing: bool,
    saving: bool,
    deleting: bool,
    updating_config: bool,
    acl_operation: bool,
    exporting: bool,
    runtime_request_id: u64,
    message_request_id: u64,
    consumer_group_request_id: u64,
    config_request_id: u64,
    acl_request_id: u64,
    topic_operation_id: u64,
    acl_operation_id: u64,
    topic_operation: bool,
    notice: Option<(String, bool)>,
    focus_handle: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl KafkaView {
    /// 创建视图状态并异步加载本地配置；加载失败会保留在页面上，不伪装成空列表。
    pub fn new(service: Arc<KafkaService>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let cluster_search = input(window, cx, 4 * 1024, "搜索集群…", false, "");
        let topic_search = input(window, cx, 4 * 1024, "筛选 Topic…", false, "");
        let consumer_group_search = input(window, cx, 4 * 1024, "筛选消费者组…", false, "");
        let acl_principal_filter = input(
            window,
            cx,
            MAX_KAFKA_ACL_HOST_BYTES,
            "Principal（可选）",
            false,
            "",
        );
        let acl_host_filter = input(
            window,
            cx,
            MAX_KAFKA_ACL_HOST_BYTES,
            "Host（可选）",
            false,
            "",
        );
        let acl_resource_name_filter = input(
            window,
            cx,
            MAX_KAFKA_ACL_RESOURCE_NAME_BYTES,
            "资源名称（可选）",
            false,
            "",
        );
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
        let config_resource_name = input(
            window,
            cx,
            MAX_KAFKA_CONFIG_RESOURCE_NAME_BYTES,
            "Topic 名称或 Broker ID",
            false,
            "",
        );
        let config_value = input(
            window,
            cx,
            MAX_KAFKA_CONFIG_VALUE_BYTES,
            "配置值",
            false,
            "",
        );
        let topic_input = input(window, cx, 249, "Topic", false, "");
        let partition_input = input(window, cx, 4 * 1024, "Partition，例如 0,1,2", false, "0");
        let topic_create_name = input(window, cx, 249, "新 Topic 名称", false, "");
        let topic_create_partitions = input(window, cx, 32, "初始 Partition 数量", false, "1");
        let topic_create_replication_factor = input(window, cx, 32, "副本因子", false, "1");
        let topic_target_partitions = input(window, cx, 32, "目标 Partition 总数", false, "");
        let acl_principal = input(
            window,
            cx,
            MAX_KAFKA_ACL_HOST_BYTES,
            "例如 User:app",
            false,
            "",
        );
        let acl_host = input(
            window,
            cx,
            MAX_KAFKA_ACL_HOST_BYTES,
            "例如 10.0.0.1 或 *",
            false,
            "*",
        );
        let acl_resource_name = input(
            window,
            cx,
            MAX_KAFKA_ACL_RESOURCE_NAME_BYTES,
            "Topic / Group 名称",
            false,
            "",
        );
        let start_offset_input = input(window, cx, 32, "起始 Offset（可选）", false, "0");
        let end_offset_input = input(window, cx, 32, "结束 Offset（可选）", false, "");
        let start_time_input = input(window, cx, 64, "起始时间 RFC3339（可选）", false, "");
        let end_time_input = input(window, cx, 64, "结束时间 RFC3339（可选）", false, "");
        let max_records_input = input(window, cx, 32, "最多读取条数", false, "200");

        let mut subscriptions = Vec::new();
        for field in [
            &cluster_search,
            &consumer_group_search,
            &acl_principal_filter,
            &acl_host_filter,
            &acl_resource_name_filter,
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
            &config_value,
            &topic_input,
            &partition_input,
            &topic_create_name,
            &topic_create_partitions,
            &topic_create_replication_factor,
            &topic_target_partitions,
            &acl_principal,
            &acl_host,
            &acl_resource_name,
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
        subscriptions.push(
            cx.subscribe(&topic_search, |this, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    this.topic_page_index = 0;
                    this.topic_scroll
                        .0
                        .borrow()
                        .base_handle
                        .set_offset(gpui::point(gpui::px(0.0), gpui::px(0.0)));
                    this.notice = None;
                    cx.notify();
                }
            }),
        );
        subscriptions.push(cx.subscribe(
            &config_resource_name,
            |this, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    this.invalidate_config_request();
                    this.notice = None;
                    cx.notify();
                }
            },
        ));
        for field in [
            &acl_principal_filter,
            &acl_host_filter,
            &acl_resource_name_filter,
        ] {
            subscriptions.push(cx.subscribe(field, |this, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    this.clear_acl_snapshot();
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
            topic_page_index: 0,
            topic_page_size: DEFAULT_TOPIC_PAGE_SIZE,
            topic_scroll: UniformListScrollHandle::new(),
            consumer_groups: Vec::new(),
            selected_consumer_group: None,
            consumer_group_error: None,
            message_page: None,
            selected_message: None,
            message_page_index: 0,
            message_page_size: DEFAULT_MESSAGE_PAGE_SIZE,
            message_scroll: UniformListScrollHandle::new(),
            topic_partition_scroll: ScrollHandle::new(),
            consumer_group_scroll: UniformListScrollHandle::new(),
            acls: Vec::new(),
            selected_acl: None,
            acls_loaded: false,
            acl_error: None,
            acl_scroll: UniformListScrollHandle::new(),
            section: KafkaSection::Overview,
            cluster_search,
            topic_search,
            consumer_group_search,
            acl_principal_filter,
            acl_host_filter,
            acl_resource_name_filter,
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
            config_resource_name,
            config_value,
            topic_input,
            partition_input,
            topic_create_name,
            topic_create_partitions,
            topic_create_replication_factor,
            topic_target_partitions,
            acl_principal,
            acl_host,
            acl_resource_name,
            start_offset_input,
            end_offset_input,
            start_time_input,
            end_time_input,
            max_records_input,
            search_fields: [true, true, true],
            range_mode: KafkaRangeMode::Offset,
            security_protocol: KafkaSecurityProtocol::default(),
            sasl_mechanism: KafkaSaslMechanism::Plain,
            config_resource_type: KafkaConfigResourceType::Topic,
            acl_filter_resource_type: None,
            acl_filter_pattern_type: None,
            acl_filter_operation: None,
            acl_filter_permission: None,
            acl_resource_type: KafkaAclResourceType::Topic,
            acl_pattern_type: KafkaAclPatternType::Literal,
            acl_operation_kind: KafkaAclOperation::Read,
            acl_permission: KafkaAclPermission::Allow,
            config_entries: Vec::new(),
            editing_config_key: None,
            read_only: KafkaReadOnlyState::default(),
            loading_clusters: true,
            loading_runtime: false,
            loading_messages: false,
            loading_consumer_groups: false,
            loading_acls: false,
            loading_configs: false,
            testing: false,
            saving: false,
            deleting: false,
            updating_config: false,
            acl_operation: false,
            exporting: false,
            runtime_request_id: 0,
            message_request_id: 0,
            consumer_group_request_id: 0,
            config_request_id: 0,
            acl_request_id: 0,
            topic_operation_id: 0,
            acl_operation_id: 0,
            topic_operation: false,
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
mod acls;
mod admin;
mod messages;
mod profile;
mod remote_config;
mod remote_config_render;
mod render_config;
mod render_consumer_group_helpers;
mod render_consumer_groups;
mod render_main;
mod render_message_detail;
mod render_messages;
mod render_overview;
mod render_sidebar;
mod render_topic_detail;
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
