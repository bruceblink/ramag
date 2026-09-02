use super::*;

impl KafkaView {
    /// 使旧资源的配置快照失效，防止切换集群或 Topic 后继续显示上一份结果。
    pub(super) fn invalidate_config_request(&mut self) {
        self.config_request_id = self.config_request_id.wrapping_add(1);
        self.loading_configs = false;
        self.updating_config = false;
        self.config_entries.clear();
        self.editing_config_key = None;
    }

    pub(super) fn select_config_resource_type(
        &mut self,
        resource_type: KafkaConfigResourceType,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.config_resource_type == resource_type {
            return;
        }
        self.invalidate_config_request();
        self.config_resource_type = resource_type;
        let resource_name = match resource_type {
            KafkaConfigResourceType::Topic => self.selected_topic.clone().unwrap_or_default(),
            KafkaConfigResourceType::Broker => "-1".into(),
        };
        set_value(&self.config_resource_name, resource_name, window, cx);
        set_value(&self.config_value, "", window, cx);
        self.notice = None;
        cx.notify();
    }

    /// 读取指定 Topic 或 Broker 的当前有效配置；读取按钮是唯一触发入口。
    pub(super) fn load_configs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.loading_configs || self.updating_config {
            return;
        }
        let Some(cluster_id) = self.selected_cluster_id.clone() else {
            self.notice = Some(("请先选择已保存的 Kafka 集群".into(), true));
            cx.notify();
            return;
        };
        if self.saving || self.deleting {
            return;
        }
        let Some(config) = self.selected_config() else {
            self.notice = Some(("请先选择已保存的 Kafka 集群".into(), true));
            cx.notify();
            return;
        };
        let resource_type = self.config_resource_type;
        let resource_name = value(&self.config_resource_name, cx);
        if let Err(error) = resource_type.validate_resource_name(&resource_name) {
            self.notice = Some((error, true));
            cx.notify();
            return;
        }

        self.config_request_id = self.config_request_id.wrapping_add(1);
        let request_id = self.config_request_id;
        self.loading_configs = true;
        self.config_entries.clear();
        self.editing_config_key = None;
        set_value(&self.config_value, "", window, cx);
        self.notice = Some((
            format!(
                "正在读取 {}「{}」的 Kafka 配置…",
                resource_type.label(),
                resource_name
            ),
            false,
        ));
        let service = self.service.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = service
                .describe_configs(&config, resource_type, &resource_name)
                .await;
            let _ = this.update_in(cx, |this, _window, cx| {
                if this.config_request_id != request_id
                    || this.selected_cluster_id.as_ref() != Some(&cluster_id)
                {
                    return;
                }
                this.loading_configs = false;
                match result {
                    Ok(resource) => {
                        this.config_entries = resource.entries;
                        this.notice = Some((
                            format!(
                                "已读取 {}「{}」的 {} 个配置项；敏感值不会显示",
                                resource_type.label(),
                                resource_name,
                                this.config_entries.len()
                            ),
                            false,
                        ));
                    }
                    Err(error) => {
                        this.mark_runtime_failure("读取 Kafka 配置", &error);
                        this.notice = Some((
                            format!("读取 Kafka 配置失败：{}", error.user_message()),
                            true,
                        ));
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// 为设置操作选择配置项；再次点击同一行的“设置”才会提交确认请求。
    fn select_config_entry(&mut self, key: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(entry) = self.config_entries.iter().find(|entry| entry.key == key) else {
            return;
        };
        self.editing_config_key = Some(key);
        set_value(
            &self.config_value,
            entry.raw_value_for_update().unwrap_or_default(),
            window,
            cx,
        );
    }

    pub(super) fn begin_config_update(
        &mut self,
        operation: KafkaConfigUpdateOperation,
        key: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.loading_configs || self.updating_config || self.topic_operation {
            return;
        }
        let Some(cluster_id) = self.selected_cluster_id.clone() else {
            self.notice = Some(("请先选择已保存的 Kafka 集群".into(), true));
            cx.notify();
            return;
        };
        if !self.read_only.allows_admin() {
            self.notice = Some((
                format!(
                    "{}；请先在配置页开启管理模式",
                    ramag_domain::error::READ_ONLY_MESSAGE
                ),
                true,
            ));
            cx.notify();
            return;
        }
        if self.loading_runtime || self.saving || self.deleting || self.testing {
            return;
        }
        let Some(config) = self.selected_config() else {
            self.notice = Some(("请先保存并选择 Kafka 集群配置".into(), true));
            cx.notify();
            return;
        };
        let resource_type = self.config_resource_type;
        let resource_name = value(&self.config_resource_name, cx);
        let Some(entry) = self.config_entries.iter().find(|entry| entry.key == key) else {
            self.notice = Some(("配置项已失效，请重新读取 Kafka 配置".into(), true));
            cx.notify();
            return;
        };
        if !entry.can_modify(resource_type) {
            self.notice = Some((
                format!(
                    "配置项「{}」来源为「{}」，只能修改当前资源的动态非敏感配置",
                    key,
                    entry.source.label()
                ),
                true,
            ));
            cx.notify();
            return;
        }
        if operation == KafkaConfigUpdateOperation::Set
            && self.editing_config_key.as_deref() != Some(key.as_str())
        {
            self.select_config_entry(key.clone(), window, cx);
            self.notice = Some((
                format!("已选中「{}」，修改值后再次点击该行的“设置”", key),
                false,
            ));
            cx.notify();
            return;
        }

        let request = match operation {
            KafkaConfigUpdateOperation::Set => KafkaConfigUpdateRequest::set(
                resource_type,
                resource_name.clone(),
                key.clone(),
                value(&self.config_value, cx),
            ),
            KafkaConfigUpdateOperation::Delete => {
                KafkaConfigUpdateRequest::delete(resource_type, resource_name.clone(), key.clone())
            }
        };
        if let Err(error) = request.validate() {
            self.notice = Some((error, true));
            cx.notify();
            return;
        }
        let view = cx.entity();
        let description = match operation {
            KafkaConfigUpdateOperation::Set => format!(
                "将在集群「{}」的 {}「{}」上设置配置「{}」。确认后才会调用 Kafka Admin API；新值不会显示在确认对话框中。",
                config.name,
                resource_type.label(),
                resource_name,
                key
            ),
            KafkaConfigUpdateOperation::Delete => format!(
                "将在集群「{}」的 {}「{}」上删除配置「{}」的动态覆盖，使其恢复继承或默认值。确认后才会调用 Kafka Admin API。",
                config.name,
                resource_type.label(),
                resource_name,
                key
            ),
        };
        ramag_ui::open_confirm(
            match operation {
                KafkaConfigUpdateOperation::Set => "设置 Kafka 配置？",
                KafkaConfigUpdateOperation::Delete => "删除 Kafka 配置覆盖？",
            },
            description,
            match operation {
                KafkaConfigUpdateOperation::Set => "设置",
                KafkaConfigUpdateOperation::Delete => "删除覆盖",
            },
            operation == KafkaConfigUpdateOperation::Delete,
            move |window, app| {
                view.update(app, |this, cx| {
                    this.execute_config_update(cluster_id, request, window, cx);
                });
            },
            window,
            cx,
        );
    }

    fn execute_config_update(
        &mut self,
        cluster_id: KafkaClusterId,
        request: KafkaConfigUpdateRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.updating_config
            || self.selected_cluster_id.as_ref() != Some(&cluster_id)
            || !self.read_only.allows_admin()
        {
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
        self.updating_config = true;
        self.config_request_id = self.config_request_id.wrapping_add(1);
        let request_id = self.config_request_id;
        self.notice = Some((
            format!(
                "正在{} Kafka 配置「{}」…",
                request.operation.label(),
                request.key
            ),
            false,
        ));
        let service = self.service.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = service.update_config(&config, &request).await;
            let _ = this.update_in(cx, |this, window, cx| {
                if this.config_request_id != request_id
                    || this.selected_cluster_id.as_ref() != Some(&cluster_id)
                {
                    return;
                }
                this.updating_config = false;
                match result {
                    Ok(()) => {
                        this.editing_config_key = None;
                        set_value(&this.config_value, "", window, cx);
                        this.notice = Some((
                            format!("Kafka 配置「{}」已修改，正在重新读取…", request.key),
                            false,
                        ));
                        this.load_configs(window, cx);
                    }
                    Err(error) => {
                        this.mark_runtime_failure("修改 Kafka 配置", &error);
                        this.notice = Some((
                            format!("修改 Kafka 配置失败：{}", error.user_message()),
                            true,
                        ));
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
}
