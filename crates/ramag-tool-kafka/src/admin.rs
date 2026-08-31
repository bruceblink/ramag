use super::*;

impl KafkaView {
    pub(super) fn toggle_admin_mode(&mut self, cx: &mut Context<Self>) {
        self.read_only = if self.read_only.allows_admin() {
            KafkaReadOnlyState::ReadOnly
        } else {
            KafkaReadOnlyState::ReadWrite
        };
        self.notice = Some((
            if self.read_only.allows_admin() {
                "Topic 管理模式已启用；保存配置后会保留此设置".into()
            } else {
                "已恢复只读保护；Topic 写操作已禁用".into()
            },
            false,
        ));
        cx.notify();
    }

    pub(super) fn invalidate_topic_operation(&mut self) {
        self.topic_operation_id = self.topic_operation_id.wrapping_add(1);
        self.topic_operation = false;
    }

    fn ensure_topic_admin_ready(&mut self, cx: &mut Context<Self>) -> bool {
        if self.selected_cluster_id.is_none() {
            self.notice = Some(("请先选择已保存的 Kafka 集群".into(), true));
            cx.notify();
            return false;
        }
        if !self.read_only.allows_admin() {
            self.notice = Some((
                format!(
                    "{}；请先在配置页开启 Topic 管理模式",
                    ramag_domain::error::READ_ONLY_MESSAGE
                ),
                true,
            ));
            cx.notify();
            return false;
        }
        if self.topic_operation || self.loading_runtime || self.saving || self.deleting {
            return false;
        }
        true
    }

    pub(super) fn begin_create_topic(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.ensure_topic_admin_ready(cx) {
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
        let request = KafkaTopicCreateRequest::new(
            value(&self.topic_create_name, cx),
            match parse_topic_count(&self.topic_create_partitions, cx, "初始 Partition 数量") {
                Ok(value) => value,
                Err(error) => {
                    self.notice = Some((error, true));
                    cx.notify();
                    return;
                }
            },
            match parse_topic_count(&self.topic_create_replication_factor, cx, "副本因子") {
                Ok(value) => value,
                Err(error) => {
                    self.notice = Some((error, true));
                    cx.notify();
                    return;
                }
            },
        );
        if let Err(error) = request.validate() {
            self.notice = Some((error, true));
            cx.notify();
            return;
        }
        let view = cx.entity();
        let description = format!(
            "将在集群「{}」创建 Topic「{}」，初始 Partition {} 个，副本因子 {}。确认后才会调用 Kafka Admin API。",
            config.name, request.name, request.partitions, request.replication_factor
        );
        ramag_ui::open_confirm(
            "创建 Topic？",
            description,
            "创建",
            false,
            move |window, app| {
                view.update(app, |this, cx| {
                    this.execute_create_topic(config, request, window, cx);
                });
            },
            window,
            cx,
        );
    }

    pub(super) fn begin_delete_topic(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.ensure_topic_admin_ready(cx) {
            return;
        }
        let Some(topic) = self
            .selected_topic
            .as_ref()
            .and_then(|name| self.topics.iter().find(|topic| &topic.name == name))
            .cloned()
        else {
            self.notice = Some(("请先选择要删除的 Topic".into(), true));
            cx.notify();
            return;
        };
        if topic.internal {
            self.notice = Some(("Kafka 内部 Topic 只能浏览，不能删除".into(), true));
            cx.notify();
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
        let view = cx.entity();
        let topic_name = topic.name.clone();
        let description = format!(
            "将在集群「{}」删除 Topic「{}」及其中的消息。该操作不可恢复，确认后才会调用 Kafka Admin API。",
            config.name, topic_name
        );
        ramag_ui::open_confirm(
            "删除 Topic？",
            description,
            "删除",
            true,
            move |window, app| {
                view.update(app, |this, cx| {
                    this.execute_delete_topic(config, topic_name, window, cx);
                });
            },
            window,
            cx,
        );
    }

    pub(super) fn begin_expand_topic(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.ensure_topic_admin_ready(cx) {
            return;
        }
        let Some(topic) = self
            .selected_topic
            .as_ref()
            .and_then(|name| self.topics.iter().find(|topic| &topic.name == name))
            .cloned()
        else {
            self.notice = Some(("请先选择要扩容的 Topic".into(), true));
            cx.notify();
            return;
        };
        if topic.internal {
            self.notice = Some(("Kafka 内部 Topic 只能浏览，不能扩容".into(), true));
            cx.notify();
            return;
        }
        let target =
            match parse_topic_count(&self.topic_target_partitions, cx, "目标 Partition 总数") {
                Ok(value) => value,
                Err(error) => {
                    self.notice = Some((error, true));
                    cx.notify();
                    return;
                }
            };
        if target <= topic.partitions.len() {
            self.notice = Some((
                format!(
                    "目标 Partition 总数必须大于当前数量 {}",
                    topic.partitions.len()
                ),
                true,
            ));
            cx.notify();
            return;
        }
        let request = KafkaTopicPartitionExpansion::new(topic.name.clone(), target);
        if let Err(error) = request.validate() {
            self.notice = Some((error, true));
            cx.notify();
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
        let view = cx.entity();
        let description = format!(
            "将在集群「{}」把 Topic「{}」的 Partition 总数从 {} 增加到 {}。Kafka 只支持增加，不能减少。",
            config.name,
            request.name,
            topic.partitions.len(),
            request.total_partitions
        );
        ramag_ui::open_confirm(
            "增加 Partition？",
            description,
            "增加",
            false,
            move |window, app| {
                view.update(app, |this, cx| {
                    this.execute_expand_topic(config, request, window, cx);
                });
            },
            window,
            cx,
        );
    }

    fn execute_create_topic(
        &mut self,
        config: KafkaClusterConfig,
        request: KafkaTopicCreateRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.topic_operation {
            return;
        }
        self.topic_operation = true;
        self.topic_operation_id = self.topic_operation_id.wrapping_add(1);
        let operation_id = self.topic_operation_id;
        let cluster_id = config.id.clone();
        let service = self.service.clone();
        self.notice = Some((format!("正在创建 Topic「{}」…", request.name), false));
        cx.spawn_in(window, async move |this, cx| {
            let result = service.create_topic(&config, &request).await;
            let _ = this.update_in(cx, |this, window, cx| {
                if this.topic_operation_id != operation_id
                    || this.selected_cluster_id.as_ref() != Some(&cluster_id)
                {
                    return;
                }
                this.topic_operation = false;
                match result {
                    Ok(()) => {
                        this.notice = Some((
                            format!("Topic「{}」已创建；正在刷新元数据", request.name),
                            false,
                        ));
                        set_value(&this.topic_create_name, "", window, cx);
                        this.load_runtime(config, window, cx);
                    }
                    Err(error) => {
                        this.notice =
                            Some((format!("创建 Topic 失败：{}", error.user_message()), true));
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn execute_delete_topic(
        &mut self,
        config: KafkaClusterConfig,
        topic: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.topic_operation {
            return;
        }
        self.topic_operation = true;
        self.topic_operation_id = self.topic_operation_id.wrapping_add(1);
        let operation_id = self.topic_operation_id;
        let cluster_id = config.id.clone();
        let service = self.service.clone();
        self.notice = Some((format!("正在删除 Topic「{}」…", topic), false));
        cx.spawn_in(window, async move |this, cx| {
            let result = service.delete_topic(&config, &topic).await;
            let _ = this.update_in(cx, |this, window, cx| {
                if this.topic_operation_id != operation_id
                    || this.selected_cluster_id.as_ref() != Some(&cluster_id)
                {
                    return;
                }
                this.topic_operation = false;
                match result {
                    Ok(()) => {
                        this.invalidate_message_request();
                        this.selected_topic = None;
                        set_value(&this.topic_input, "", window, cx);
                        set_value(&this.topic_target_partitions, "", window, cx);
                        this.notice =
                            Some((format!("Topic「{}」已删除；正在刷新元数据", topic), false));
                        this.load_runtime(config, window, cx);
                    }
                    Err(error) => {
                        this.notice =
                            Some((format!("删除 Topic 失败：{}", error.user_message()), true));
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn execute_expand_topic(
        &mut self,
        config: KafkaClusterConfig,
        request: KafkaTopicPartitionExpansion,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.topic_operation {
            return;
        }
        self.topic_operation = true;
        self.topic_operation_id = self.topic_operation_id.wrapping_add(1);
        let operation_id = self.topic_operation_id;
        let cluster_id = config.id.clone();
        let service = self.service.clone();
        self.notice = Some((
            format!("正在增加 Topic「{}」的 Partition…", request.name),
            false,
        ));
        cx.spawn_in(window, async move |this, cx| {
            let result = service.increase_topic_partitions(&config, &request).await;
            let _ = this.update_in(cx, |this, window, cx| {
                if this.topic_operation_id != operation_id
                    || this.selected_cluster_id.as_ref() != Some(&cluster_id)
                {
                    return;
                }
                this.topic_operation = false;
                match result {
                    Ok(()) => {
                        this.notice = Some((
                            format!("Topic「{}」已扩容；正在刷新元数据", request.name),
                            false,
                        ));
                        this.load_runtime(config, window, cx);
                    }
                    Err(error) => {
                        this.notice = Some((
                            format!("增加 Partition 失败：{}", error.user_message()),
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

fn parse_topic_count(field: &Entity<InputState>, cx: &App, label: &str) -> Result<usize, String> {
    let text = value(field, cx);
    let count = text
        .parse::<usize>()
        .map_err(|_| format!("{label}必须是正整数"))?;
    if count == 0 {
        return Err(format!("{label}必须大于 0"));
    }
    Ok(count)
}
