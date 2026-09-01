use super::*;

/// 只接受代次和当前上下文都一致的结果，避免迟到任务覆盖新页面。
pub(super) fn request_matches<T: PartialEq>(
    current_request_id: u64,
    request_id: u64,
    current_context: Option<&T>,
    request_context: Option<&T>,
) -> bool {
    current_request_id == request_id && current_context == request_context
}

impl KafkaView {
    /// 使保存、连接测试和删除任务失效；底层请求仍可自然结束，但不能再更新视图。
    pub(super) fn invalidate_profile_operation(&mut self) {
        self.profile_operation_id = self.profile_operation_id.wrapping_add(1);
        self.saving = false;
        self.testing = false;
        self.deleting = false;
    }

    /// 使元数据刷新任务失效，防止切换到草稿或删除配置后恢复旧的集群快照。
    pub(super) fn invalidate_runtime_request(&mut self) {
        self.runtime_request_id = self.runtime_request_id.wrapping_add(1);
        self.loading_runtime = false;
        self.runtime_error = None;
    }

    /// 重新读取本地配置；每次读取都有独立代次，重复触发时只接受最后一次结果。
    pub(super) fn load_clusters(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.cluster_request_id = self.cluster_request_id.wrapping_add(1);
        let request_id = self.cluster_request_id;
        self.loading_clusters = true;
        self.cluster_load_error = None;
        let service = self.service.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = service.list_clusters().await;
            let _ = this.update_in(cx, |this, window, cx| {
                if this.cluster_request_id != request_id {
                    return;
                }
                this.loading_clusters = false;
                match result {
                    Ok(clusters) => {
                        this.cluster_load_error = None;
                        let selected = this
                            .selected_cluster_id
                            .clone()
                            .filter(|id| clusters.iter().any(|cluster| &cluster.id == id));
                        this.clusters = clusters;
                        if let Some(id) = selected {
                            if let Some(config) = this.cluster_by_id(&id).cloned() {
                                this.selected_cluster_id = Some(id);
                                this.set_form_from_config(&config, window, cx);
                            }
                        } else if this.selected_cluster_id.is_some() {
                            this.new_profile(window, cx);
                        }
                    }
                    Err(error) => {
                        this.cluster_load_error = Some(error.user_message());
                        this.notice =
                            Some((format!("加载集群配置失败：{}", error.user_message()), true));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn retry_cluster_load(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.loading_clusters {
            return;
        }
        self.load_clusters(window, cx);
        cx.notify();
    }

    /// 重新读取当前已保存集群的 Broker 元数据和 Topic；草稿或并发操作不会触发请求。
    pub(super) fn retry_runtime(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.loading_runtime
            || self.testing
            || self.saving
            || self.deleting
            || self.topic_operation
            || self.acl_operation
        {
            return;
        }
        let Some(config) = self.selected_config() else {
            return;
        };
        self.load_runtime(config, window, cx);
    }

    pub(super) fn cluster_by_id(&self, id: &KafkaClusterId) -> Option<&KafkaClusterConfig> {
        self.clusters.iter().find(|cluster| &cluster.id == id)
    }

    pub(super) fn selected_config(&self) -> Option<KafkaClusterConfig> {
        self.selected_cluster_id
            .as_ref()
            .and_then(|id| self.cluster_by_id(id))
            .cloned()
    }

    pub(super) fn set_form_from_config(
        &mut self,
        config: &KafkaClusterConfig,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.invalidate_config_request();
        self.reset_acl_state(window, cx);
        self.read_only = config.read_only;
        set_value(&self.name, config.name.clone(), window, cx);
        set_value(
            &self.bootstrap_servers,
            config.bootstrap_servers.join(", "),
            window,
            cx,
        );
        set_value(
            &self.client_id,
            config.client_id.clone().unwrap_or_default(),
            window,
            cx,
        );
        set_value(
            &self.sasl_username,
            config.sasl_username.clone().unwrap_or_default(),
            window,
            cx,
        );
        set_value(&self.sasl_password, "", window, cx);
        self.sasl_password.update(cx, |state, cx| {
            state.set_placeholder(
                if config.sasl_password.is_some() {
                    "已保存密码，留空保持；输入新值可替换"
                } else {
                    "SASL 密码"
                },
                window,
                cx,
            );
        });
        set_value(
            &self.remark,
            config.remark.clone().unwrap_or_default(),
            window,
            cx,
        );
        set_value(
            &self.ca_cert_path,
            config.tls.ca_cert_path.clone().unwrap_or_default(),
            window,
            cx,
        );
        set_value(
            &self.client_cert_path,
            config.tls.client_cert_path.clone().unwrap_or_default(),
            window,
            cx,
        );
        set_value(
            &self.client_key_path,
            config.tls.client_key_path.clone().unwrap_or_default(),
            window,
            cx,
        );
        self.security_protocol = config.security_protocol;
        self.sasl_mechanism = config.sasl_mechanism.unwrap_or(KafkaSaslMechanism::Plain);
        set_value(
            &self.topic_input,
            self.selected_topic.clone().unwrap_or_default(),
            window,
            cx,
        );
        set_value(&self.topic_create_name, "", window, cx);
        set_value(&self.topic_create_partitions, "1", window, cx);
        set_value(&self.topic_create_replication_factor, "1", window, cx);
        set_value(&self.topic_target_partitions, "", window, cx);
        self.config_resource_type = KafkaConfigResourceType::Topic;
        set_value(
            &self.config_resource_name,
            self.selected_topic.clone().unwrap_or_default(),
            window,
            cx,
        );
        set_value(&self.config_value, "", window, cx);
    }

    pub(super) fn new_profile(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.invalidate_profile_operation();
        self.invalidate_runtime_request();
        self.invalidate_message_request();
        self.invalidate_consumer_group_request();
        self.invalidate_topic_operation();
        self.invalidate_config_request();
        self.reset_acl_state(window, cx);
        self.selected_cluster_id = None;
        self.selected_topic = None;
        self.metadata = None;
        self.topics.clear();
        self.reset_topic_paging();
        self.consumer_groups.clear();
        self.selected_consumer_group = None;
        self.consumer_group_error = None;
        self.message_page = None;
        self.selected_message = None;
        self.section = KafkaSection::Config;
        self.security_protocol = KafkaSecurityProtocol::default();
        self.sasl_mechanism = KafkaSaslMechanism::Plain;
        self.read_only = KafkaReadOnlyState::default();
        for field in [
            &self.name,
            &self.bootstrap_servers,
            &self.client_id,
            &self.consumer_group_search,
            &self.sasl_username,
            &self.sasl_password,
            &self.remark,
            &self.ca_cert_path,
            &self.client_cert_path,
            &self.client_key_path,
            &self.config_resource_name,
            &self.config_value,
            &self.topic_input,
            &self.topic_create_name,
            &self.topic_target_partitions,
        ] {
            set_value(field, "", window, cx);
        }
        set_value(&self.topic_create_partitions, "1", window, cx);
        set_value(&self.topic_create_replication_factor, "1", window, cx);
        self.bootstrap_servers.update(cx, |state, cx| {
            state.set_placeholder("broker-1:9092, broker-2:9092", window, cx);
        });
        self.sasl_password.update(cx, |state, cx| {
            state.set_placeholder("SASL 密码", window, cx);
        });
        self.notice = None;
        cx.notify();
    }

    pub(super) fn select_cluster(
        &mut self,
        id: KafkaClusterId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(config) = self.cluster_by_id(&id).cloned() else {
            return;
        };
        self.invalidate_profile_operation();
        self.invalidate_runtime_request();
        self.invalidate_message_request();
        self.invalidate_consumer_group_request();
        self.clear_acl_snapshot();
        self.invalidate_acl_operation();
        self.selected_cluster_id = Some(id);
        self.selected_topic = None;
        self.invalidate_topic_operation();
        self.metadata = None;
        self.topics.clear();
        self.reset_topic_paging();
        self.consumer_groups.clear();
        self.selected_consumer_group = None;
        self.consumer_group_error = None;
        self.message_page = None;
        self.selected_message = None;
        self.section = KafkaSection::Overview;
        self.set_form_from_config(&config, window, cx);
        self.notice = None;
        self.load_runtime(config, window, cx);
        cx.notify();
    }

    /// 组装当前表单；保存或连接测试都复用这条路径，保证校验规则一致。
    pub(super) fn form_config(&self, cx: &App) -> Result<KafkaClusterConfig, String> {
        let name = value(&self.name, cx);
        let bootstrap_servers = parse_bootstrap_servers(&value(&self.bootstrap_servers, cx));
        let mut config = self
            .selected_config()
            .unwrap_or_else(|| KafkaClusterConfig::new(name.clone(), bootstrap_servers.clone()));
        config.name = name;
        config.bootstrap_servers = bootstrap_servers;
        config.security_protocol = self.security_protocol;
        config.client_id = optional_value(&self.client_id, cx);
        config.remark = optional_value(&self.remark, cx);
        config.tls = KafkaTlsConfig {
            verify: config.tls.verify,
            ca_cert_path: optional_value(&self.ca_cert_path, cx),
            client_cert_path: optional_value(&self.client_cert_path, cx),
            client_key_path: optional_value(&self.client_key_path, cx),
        };
        if self.security_protocol.uses_sasl() {
            config.sasl_mechanism = Some(self.sasl_mechanism);
            config.sasl_username = optional_value(&self.sasl_username, cx);
            if let Some(password) = optional_value(&self.sasl_password, cx) {
                config.sasl_password = Some(password);
            }
        } else {
            config.sasl_mechanism = None;
            config.sasl_username = None;
            config.sasl_password = None;
        }
        config.read_only = self.read_only;
        config
            .validate()
            .map(|()| config)
            .map_err(|error| error.to_string())
    }

    pub(super) fn save_profile(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.saving || self.testing || self.deleting {
            return;
        }
        let config = match self.form_config(cx) {
            Ok(config) => config,
            Err(error) => {
                self.notice = Some((error, true));
                cx.notify();
                return;
            }
        };
        let service = self.service.clone();
        let id = config.id.clone();
        let name = config.name.clone();
        let context_cluster_id = self.selected_cluster_id.clone();
        self.profile_operation_id = self.profile_operation_id.wrapping_add(1);
        let operation_id = self.profile_operation_id;
        self.saving = true;
        self.notice = Some(("正在保存本地加密配置…".into(), false));
        cx.spawn_in(window, async move |this, cx| {
            let result = service.save_cluster(&config).await;
            let _ = this.update_in(cx, |this, _window, cx| {
                if !request_matches(
                    this.profile_operation_id,
                    operation_id,
                    this.selected_cluster_id.as_ref(),
                    context_cluster_id.as_ref(),
                ) {
                    return;
                }
                this.saving = false;
                match result {
                    Ok(()) => {
                        if let Some(existing) =
                            this.clusters.iter_mut().find(|cluster| cluster.id == id)
                        {
                            *existing = config;
                        } else {
                            this.clusters.push(config);
                        }
                        this.selected_cluster_id = Some(id);
                        this.notice = Some((format!("已保存「{name}」，配置保存在本机",), false));
                    }
                    Err(error) => {
                        this.notice = Some((format!("保存失败：{}", error.user_message()), true));
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn test_connection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.testing || self.saving || self.deleting {
            return;
        }
        let config = match self.form_config(cx) {
            Ok(config) => config,
            Err(error) => {
                self.notice = Some((error, true));
                cx.notify();
                return;
            }
        };
        let service = self.service.clone();
        let context_cluster_id = self.selected_cluster_id.clone();
        self.profile_operation_id = self.profile_operation_id.wrapping_add(1);
        let operation_id = self.profile_operation_id;
        self.testing = true;
        self.notice = Some(("正在连接 Kafka Broker…".into(), false));
        cx.spawn_in(window, async move |this, cx| {
            let result = service.test_connection(&config).await;
            let _ = this.update_in(cx, |this, window, cx| {
                if !request_matches(
                    this.profile_operation_id,
                    operation_id,
                    this.selected_cluster_id.as_ref(),
                    context_cluster_id.as_ref(),
                ) {
                    return;
                }
                this.testing = false;
                match result {
                    Ok(()) => {
                        this.notice = Some(("连接成功；正在读取集群元数据和 Topic…".into(), false));
                        this.load_runtime(config, window, cx);
                    }
                    Err(error) => {
                        this.notice = Some((format!("连接失败：{}", error.user_message()), true));
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn load_runtime(
        &mut self,
        config: KafkaClusterConfig,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.invalidate_message_request();
        self.invalidate_consumer_group_request();
        self.clear_acl_snapshot();
        self.invalidate_acl_operation();
        self.consumer_groups.clear();
        self.selected_consumer_group = None;
        self.consumer_group_error = None;
        self.reset_topic_paging();
        self.runtime_request_id = self.runtime_request_id.wrapping_add(1);
        let request_id = self.runtime_request_id;
        let context_cluster_id = self.selected_cluster_id.clone();
        self.runtime_error = None;
        self.loading_runtime = true;
        let service = self.service.clone();
        cx.spawn_in(window, async move |this, cx| {
            let metadata = service.cluster_metadata(&config).await;
            let topics = service.list_topics(&config).await;
            let _ = this.update_in(cx, |this, window, cx| {
                if !request_matches(
                    this.runtime_request_id,
                    request_id,
                    this.selected_cluster_id.as_ref(),
                    context_cluster_id.as_ref(),
                ) {
                    return;
                }
                this.loading_runtime = false;
                match (metadata, topics) {
                    (Ok(metadata), Ok(topics)) => {
                        this.metadata = Some(metadata);
                        this.topics = topics;
                        this.runtime_error = None;
                        this.notice = Some(("集群元数据已更新".into(), false));
                    }
                    (Err(metadata_error), Ok(topics)) => {
                        this.metadata = None;
                        this.topics = topics;
                        this.runtime_error =
                            Some(format!("元数据读取失败：{}", metadata_error.user_message()));
                        this.notice = Some((
                            format!("元数据读取失败：{}", metadata_error.user_message()),
                            true,
                        ));
                    }
                    (Ok(metadata), Err(topic_error)) => {
                        this.metadata = Some(metadata);
                        this.topics.clear();
                        this.runtime_error = Some(format!(
                            "Topic 列表读取失败：{}",
                            topic_error.user_message()
                        ));
                        this.notice = Some((
                            format!("Topic 列表读取失败：{}", topic_error.user_message()),
                            true,
                        ));
                    }
                    (Err(error), Err(topic_error)) => {
                        this.metadata = None;
                        this.topics.clear();
                        this.runtime_error = Some(format!(
                            "元数据读取失败：{}；Topic 列表读取失败：{}",
                            error.user_message(),
                            topic_error.user_message()
                        ));
                        this.notice = Some((
                            format!(
                                "元数据读取失败：{}；Topic 列表读取失败：{}",
                                error.user_message(),
                                topic_error.user_message()
                            ),
                            true,
                        ));
                    }
                }
                if this.section == KafkaSection::ConsumerGroups
                    && this.selected_cluster_id.is_some()
                {
                    this.load_consumer_groups(config.clone(), window, cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn delete_profile(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.deleting || self.saving || self.testing {
            return;
        }
        let Some(id) = self.selected_cluster_id.clone() else {
            return;
        };
        let Some(config) = self.cluster_by_id(&id) else {
            return;
        };
        let view = cx.entity();
        ramag_ui::open_confirm(
            "删除 Kafka 配置？",
            format!(
                "将从本机删除「{}」及其加密认证信息。不会修改 Kafka 集群。",
                config.name
            ),
            "删除",
            true,
            move |window, app| {
                view.update(app, |this, cx| this.confirm_delete(id, window, cx));
            },
            window,
            cx,
        );
    }

    pub(super) fn confirm_delete(
        &mut self,
        id: KafkaClusterId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.deleting
            || self.saving
            || self.testing
            || self.selected_cluster_id.as_ref() != Some(&id)
        {
            return;
        }
        self.profile_operation_id = self.profile_operation_id.wrapping_add(1);
        let operation_id = self.profile_operation_id;
        let context_cluster_id = self.selected_cluster_id.clone();
        self.deleting = true;
        let service = self.service.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = service.delete_cluster(&id).await;
            let _ = this.update_in(cx, |this, _window, cx| {
                if !request_matches(
                    this.profile_operation_id,
                    operation_id,
                    this.selected_cluster_id.as_ref(),
                    context_cluster_id.as_ref(),
                ) {
                    return;
                }
                this.deleting = false;
                match result {
                    Ok(()) => {
                        this.invalidate_runtime_request();
                        this.invalidate_message_request();
                        this.invalidate_consumer_group_request();
                        this.clusters.retain(|cluster| cluster.id != id);
                        this.selected_cluster_id = None;
                        this.selected_topic = None;
                        this.metadata = None;
                        this.topics.clear();
                        this.reset_topic_paging();
                        this.consumer_groups.clear();
                        this.selected_consumer_group = None;
                        this.consumer_group_error = None;
                        this.message_page = None;
                        this.selected_message = None;
                        this.clear_acl_snapshot();
                        this.invalidate_acl_operation();
                        this.section = KafkaSection::Overview;
                        this.invalidate_topic_operation();
                        this.notice = Some(("本地 Kafka 配置已删除".into(), false));
                    }
                    Err(error) => {
                        this.notice = Some((format!("删除失败：{}", error.user_message()), true));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
}
