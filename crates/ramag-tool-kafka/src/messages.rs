use super::*;

impl KafkaView {
    /// 启动一次有界消息读取；新任务会使旧任务结果失效，取消只更新 UI 代次并丢弃迟到结果。
    pub(super) fn read_messages(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.loading_runtime || self.loading_messages {
            return;
        }
        let Some(config) = self.form_config(cx).ok() else {
            self.notice = Some(("请先完成有效的集群配置".into(), true));
            cx.notify();
            return;
        };
        let topic = value(&self.topic_input, cx);
        let partitions = match parse_partition_list(&value(&self.partition_input, cx)) {
            Ok(value) => value,
            Err(error) => {
                self.notice = Some((error, true));
                cx.notify();
                return;
            }
        };
        let scan = match self.range_mode {
            KafkaRangeMode::Offset => {
                let start_offset =
                    match parse_i64_input(&self.start_offset_input, cx, "起始 Offset") {
                        Ok(value) => value,
                        Err(error) => {
                            self.notice = Some((error, true));
                            cx.notify();
                            return;
                        }
                    };
                let end_offset = match optional_i64_input(&self.end_offset_input, cx, "结束 Offset")
                {
                    Ok(value) => value,
                    Err(error) => {
                        self.notice = Some((error, true));
                        cx.notify();
                        return;
                    }
                };
                KafkaMessageQuery::by_offset(
                    topic.clone(),
                    partitions.clone(),
                    start_offset,
                    end_offset,
                )
            }
            KafkaRangeMode::Time => {
                let start_time = match parse_datetime_input(&self.start_time_input, cx, "起始时间")
                {
                    Ok(Some(value)) => value,
                    Ok(None) => {
                        self.notice = Some(("起始时间不能为空".into(), true));
                        cx.notify();
                        return;
                    }
                    Err(error) => {
                        self.notice = Some((error, true));
                        cx.notify();
                        return;
                    }
                };
                let end_time = match parse_datetime_input(&self.end_time_input, cx, "结束时间")
                {
                    Ok(value) => value,
                    Err(error) => {
                        self.notice = Some((error, true));
                        cx.notify();
                        return;
                    }
                };
                KafkaMessageQuery::by_time(topic, partitions, start_time, end_time)
            }
        }
        .with_limits(
            match parse_usize_input(&self.max_records_input, cx, "最多读取条数") {
                Ok(value) => value,
                Err(error) => {
                    self.notice = Some((error, true));
                    cx.notify();
                    return;
                }
            },
            DEFAULT_KAFKA_MAX_BYTES,
            DEFAULT_KAFKA_MAX_SCAN_SECONDS,
            DEFAULT_KAFKA_MAX_CONCURRENT_PARTITIONS,
        );
        if let Err(error) = scan.validate() {
            self.notice = Some((error, true));
            cx.notify();
            return;
        }
        let search_text = value(&self.message_search, cx);
        let search_fields = self.selected_search_fields();
        if !search_text.is_empty() && search_fields.is_empty() {
            self.notice = Some(("至少选择一个搜索字段".into(), true));
            cx.notify();
            return;
        }
        self.message_request_id = self.message_request_id.wrapping_add(1);
        let request_id = self.message_request_id;
        self.message_page = None;
        self.selected_message = None;
        self.reset_message_paging();
        self.loading_messages = true;
        self.notice = Some((
            if search_text.is_empty() {
                "正在按范围读取消息…".into()
            } else {
                "正在按范围扫描并搜索消息…".into()
            },
            false,
        ));
        let service = self.service.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = if search_text.is_empty() {
                service.read_messages(&config, &scan).await
            } else {
                let query =
                    KafkaMessageSearchQuery::new(search_text, scan).with_fields(search_fields);
                service.search_messages(&config, &query).await
            };
            let _ = this.update_in(cx, |this, _window, cx| {
                if this.message_request_id != request_id {
                    return;
                }
                this.loading_messages = false;
                match result {
                    Ok(page) => match page.validate() {
                        Ok(()) => {
                            let count = page.records.len();
                            let truncated = page.truncated;
                            this.message_page = Some(page);
                            this.notice = Some((
                                format!(
                                    "读取完成：返回 {count} 条，{}",
                                    if truncated {
                                        "已触达扫描预算"
                                    } else {
                                        "未触达扫描预算"
                                    }
                                ),
                                false,
                            ));
                        }
                        Err(error) => {
                            this.notice = Some((format!("消息结果校验失败：{error}"), true));
                        }
                    },
                    Err(error) => {
                        this.mark_runtime_failure("读取消息", &error);
                        this.notice =
                            Some((format!("读取消息失败：{}", error.user_message()), true));
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// 使当前消息任务失效；底层读取线程可以自然结束，但迟到结果不会回写页面。
    pub(super) fn cancel_message_read(&mut self, cx: &mut Context<Self>) {
        if !self.loading_messages {
            return;
        }
        self.invalidate_message_request();
        self.notice = Some(("消息读取已取消；迟到结果不会写入当前页面".into(), false));
        cx.notify();
    }

    pub(super) fn invalidate_message_request(&mut self) {
        self.message_request_id = self.message_request_id.wrapping_add(1);
        self.loading_messages = false;
    }

    /// 回到已加载消息的第一页，并把虚拟列表滚动位置归零，避免新查询沿用旧视口。
    pub(super) fn reset_message_paging(&mut self) {
        self.message_page_index = 0;
        self.selected_message = None;
        self.message_scroll
            .0
            .borrow()
            .base_handle
            .set_offset(gpui::point(gpui::px(0.0), gpui::px(0.0)));
        self.message_page_scroll
            .set_offset(gpui::point(gpui::px(0.0), gpui::px(0.0)));
    }

    /// 返回当前已加载结果的页数；分页只切分内存中的有界结果，不扩大 Kafka 扫描范围。
    pub(super) fn message_page_count(&self) -> usize {
        self.message_page.as_ref().map_or(0, |page| {
            page.records.len().div_ceil(self.message_page_size)
        })
    }

    /// 切换已加载结果页并清理详情选择，保证详情不会指向上一页的记录。
    pub(super) fn set_message_page(&mut self, page_index: usize, cx: &mut Context<Self>) {
        let page_count = self.message_page_count();
        if page_index >= page_count || page_index == self.message_page_index {
            return;
        }
        self.message_page_index = page_index;
        self.selected_message = None;
        self.message_scroll
            .0
            .borrow()
            .base_handle
            .set_offset(gpui::point(gpui::px(0.0), gpui::px(0.0)));
        cx.notify();
    }

    pub(super) fn selected_search_fields(&self) -> Vec<KafkaMessageSearchField> {
        KafkaMessageSearchField::all()
            .into_iter()
            .enumerate()
            .filter_map(|(index, field)| self.search_fields[index].then_some(field))
            .collect()
    }

    /// 将选中的消息写成可回放的 JSON 包络；字节字段使用 Base64，避免 UTF-8 失败导致丢数据。
    pub(super) fn export_message(
        &mut self,
        record: KafkaMessageRecord,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.exporting {
            return;
        }
        if record.retained_bytes() > MAX_KAFKA_EXPORT_BYTES {
            self.notice = Some((
                format!(
                    "消息过大，单条导出最多支持 {} MB",
                    MAX_KAFKA_EXPORT_BYTES / (1024 * 1024)
                ),
                true,
            ));
            cx.notify();
            return;
        }
        let file_name = suggested_message_file_name(&record);
        self.exporting = true;
        self.notice = Some(("正在准备消息导出…".into(), false));
        cx.spawn_in(window, async move |this, cx| {
            let outcome: std::result::Result<Option<String>, String> = async {
                let Some(handle) = rfd::AsyncFileDialog::new()
                    .set_file_name(&file_name)
                    .add_filter("Kafka JSON", &["json"])
                    .save_file()
                    .await
                else {
                    return Ok(None);
                };
                let path = handle.path().to_path_buf();
                let content = serde_json::to_string_pretty(&KafkaMessageExport::from(&record))
                    .map_err(|error| format!("生成消息 JSON 失败：{error}"))?;
                let write_path = path.clone();
                ramag_app::run_blocking(move || {
                    ramag_app::usecases::export::write_atomic(&write_path, &content)
                })
                .await
                .map_err(|error| format!("写入消息导出失败：{error}"))?;
                Ok(Some(path.display().to_string()))
            }
            .await;
            let _ = this.update(cx, |this, cx| {
                this.exporting = false;
                match outcome {
                    Ok(None) => {}
                    Ok(Some(path)) => {
                        this.notice = Some((format!("消息已导出到 {path}"), false));
                    }
                    Err(error) => {
                        this.notice = Some((error, true));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    // 选择 Topic 只更新当前筛选和详情状态；切换到消息页由详情区的明确按钮负责。
    pub(super) fn select_topic(
        &mut self,
        topic: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.invalidate_message_request();
        self.reset_message_paging();
        self.selected_topic = Some(topic.clone());
        set_value(&self.topic_input, topic, window, cx);
        if self.config_resource_type == KafkaConfigResourceType::Topic {
            self.invalidate_config_request();
            set_value(
                &self.config_resource_name,
                self.selected_topic.clone().unwrap_or_default(),
                window,
                cx,
            );
            set_value(&self.config_value, "", window, cx);
        }
        let target_partitions = self
            .selected_topic
            .as_ref()
            .and_then(|name| self.topics.iter().find(|candidate| &candidate.name == name))
            .map(|topic| topic.partitions.len().saturating_add(1).to_string())
            .unwrap_or_default();
        set_value(&self.topic_target_partitions, target_partitions, window, cx);
        self.message_page = None;
        self.selected_message = None;
        self.notice = None;
        cx.notify();
    }
}
