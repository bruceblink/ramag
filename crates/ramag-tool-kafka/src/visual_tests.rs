use super::*;

struct KafkaDialogTestHost {
    view: gpui::Entity<KafkaView>,
}

impl Render for KafkaDialogTestHost {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let dialog_layer = gpui_component::Root::render_dialog_layer(window, cx);
        gpui::div()
            .relative()
            .size_full()
            .child(self.view.clone())
            .children(dialog_layer)
    }
}

const VISUAL_MESSAGE_COUNT: usize = 5_000;

#[gpui::test]
fn kafka_workspace_renders_real_data_and_cancel_control(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let cluster = KafkaClusterConfig::new("Docker Kafka", vec!["127.0.0.1:19092".into()]);
    let service = Arc::new(
        KafkaService::new(
            Arc::new(FakeKafkaDriver),
            Arc::new(FakeStorage {
                cluster: cluster.clone(),
            }),
        )
        .with_admin_driver(Arc::new(FakeKafkaAdminDriver)),
    );
    let mut kafka_entity = None;
    let (_, visual_cx) = cx.add_window_view(|window, cx| {
        let kafka = cx.new(|cx| KafkaView::new(service, window, cx));
        kafka_entity = Some(kafka.clone());
        let host = cx.new(|_| KafkaDialogTestHost { view: kafka });
        gpui_component::Root::new(host, window, cx)
    });
    assert!(kafka_entity.is_some(), "Kafka 视图实体应创建");
    let Some(kafka_entity) = kafka_entity else {
        return;
    };
    visual_cx.simulate_resize(size(px(1200.0), px(780.0)));
    visual_cx.run_until_parked();
    assert!(visual_cx.debug_bounds("kafka-root").is_some());
    assert!(visual_cx.debug_bounds("kafka-cluster-row").is_some());
    let status_bounds = visual_cx.debug_bounds("kafka-header-status");
    assert!(
        status_bounds.is_some(),
        "Kafka header status should participate in layout"
    );
    let Some(status_bounds) = status_bounds else {
        return;
    };
    assert!(
        status_bounds.size.width > px(140.0),
        "Kafka header status collapsed to {:?}",
        status_bounds.size
    );
    assert!(visual_cx.debug_bounds("kafka-welcome-add").is_some());
    assert!(visual_cx.debug_bounds("kafka-add-profile").is_some());
    click(visual_cx, "kafka-welcome-add");
    visual_cx.run_until_parked();
    let Some(config_bounds) = visual_cx.debug_bounds("kafka-config") else {
        return;
    };
    let Some(admin_panel_bounds) = visual_cx.debug_bounds("kafka-admin-mode-panel") else {
        return;
    };
    let Some(admin_copy_bounds) = visual_cx.debug_bounds("kafka-admin-mode-copy") else {
        return;
    };
    assert!(
        config_bounds.size.width >= px(700.0),
        "配置页内容宽度过小: {config_bounds:?}"
    );
    assert!(
        admin_panel_bounds.size.width >= px(600.0),
        "管理模式面板宽度过小: {admin_panel_bounds:?}"
    );
    assert!(
        admin_copy_bounds.size.width >= px(300.0),
        "管理模式说明被压缩: {admin_copy_bounds:?}"
    );

    visual_cx.simulate_resize(size(px(900.0), px(780.0)));
    visual_cx.run_until_parked();
    let compact_admin_panel_bounds = visual_cx.debug_bounds("kafka-admin-mode-panel");
    assert!(
        compact_admin_panel_bounds
            .as_ref()
            .is_some_and(|bounds| bounds.size.height < px(220.0)),
        "窄窗口管理模式面板不应占满表单高度: {compact_admin_panel_bounds:?}"
    );
    assert_within_width(visual_cx, "kafka-admin-mode-panel", 900.0);
    visual_cx.simulate_resize(size(px(1200.0), px(780.0)));
    visual_cx.run_until_parked();
    assert!(kafka_entity.read_with(visual_cx, |view, _| {
        view.section == KafkaSection::Config
    }));

    kafka_entity.update(visual_cx, |view, cx| {
        view.section = KafkaSection::Overview;
        cx.notify();
    });
    visual_cx.run_until_parked();
    click(visual_cx, "kafka-add-profile");
    visual_cx.run_until_parked();
    assert!(visual_cx.debug_bounds("kafka-config").is_some());

    kafka_entity.update(visual_cx, |view, cx| {
        view.selected_cluster_id = Some(cluster.id.clone());
        view.metadata = Some(KafkaClusterMetadata {
            cluster_id: Some("test-cluster".into()),
            controller_id: Some(0),
            brokers: vec![KafkaBroker {
                id: 0,
                host: "127.0.0.1".into(),
                port: 19092,
                rack: None,
                version: Some("4.0.0".into()),
                is_controller: true,
            }],
            kafka_version: Some("4.0.0".into()),
        });
        let mut topics = vec![KafkaTopic {
            name: "ramag.integration.messages".into(),
            internal: false,
            partitions: (0..50)
                .map(|id| KafkaPartition {
                    id,
                    leader: Some(0),
                    replicas: vec![0],
                    isr: vec![0],
                    low_watermark: Some(0),
                    high_watermark: Some(1),
                })
                .collect(),
        }];
        topics.extend((0..122).map(|index| KafkaTopic {
            name: format!("ramag.integration.topic-{index:03}"),
            internal: false,
            partitions: vec![KafkaPartition {
                id: 0,
                leader: Some(0),
                replicas: vec![0],
                isr: vec![0],
                low_watermark: Some(0),
                high_watermark: Some(1),
            }],
        }));
        view.topics = topics;
        view.section = KafkaSection::Overview;
        view.loading_runtime = false;
        cx.notify();
    });
    visual_cx.run_until_parked();
    click(visual_cx, "kafka-section-Topics");
    visual_cx.run_until_parked();
    let topic_admin_bounds = visual_cx.debug_bounds("kafka-topic-admin");
    let topic_row_bounds = visual_cx.debug_bounds("kafka-topic-row-ramag.integration.messages");
    assert!(
        topic_admin_bounds
            .as_ref()
            .is_some_and(|bounds| bounds.size.height < px(300.0)),
        "Topic 管理区不应占满剩余窗口高度: {topic_admin_bounds:?}"
    );
    assert!(
        topic_row_bounds
            .as_ref()
            .is_some_and(|bounds| { bounds.origin.y + bounds.size.height <= px(780.0) }),
        "Topic 行应保持在测试窗口内: {topic_row_bounds:?}"
    );
    assert!(
        visual_cx.debug_bounds("kafka-topic-v-scrollbar").is_some(),
        "Topic 列表应提供纵向滚动条入口"
    );
    assert!(
        visual_cx.debug_bounds("kafka-topic-pagination").is_some()
            && visual_cx.debug_bounds("kafka-topic-page-next").is_some(),
        "Topic 数量超过一页时应显示分页控件"
    );
    click(visual_cx, "kafka-topic-page-next");
    assert_eq!(
        kafka_entity.read_with(visual_cx, |view, _| view.topic_page_index),
        1,
        "Topic 下一页按钮应切换已加载快照页"
    );
    click(visual_cx, "kafka-topic-page-previous");
    assert_eq!(
        kafka_entity.read_with(visual_cx, |view, _| view.topic_page_index),
        0,
        "Topic 上一页按钮应返回第一页"
    );
    click(visual_cx, "kafka-topic-row-ramag.integration.messages");
    visual_cx.run_until_parked();
    assert!(kafka_entity.read_with(visual_cx, |view, _| {
        view.section == KafkaSection::Topics
    }));
    let selected_topic = kafka_entity.read_with(visual_cx, |view, _| view.selected_topic.clone());
    assert_eq!(
        selected_topic.as_deref(),
        Some("ramag.integration.messages"),
        "Topic 行点击后应选中详情: {selected_topic:?}"
    );
    assert!(
        visual_cx
            .debug_bounds("kafka-topic-copy-ramag.integration.messages")
            .is_some(),
        "Topic 行应提供明确的复制入口"
    );
    assert!(visual_cx.debug_bounds("kafka-partition-scroll").is_some());
    assert!(
        visual_cx
            .debug_bounds("kafka-partition-v-scrollbar")
            .is_some(),
        "Topic Partition 详情应提供始终可发现的纵向滚动条"
    );
    for selector in [
        "kafka-partition-scroll",
        "kafka-topic-expand",
        "kafka-topic-delete",
        "kafka-open-topic-messages",
    ] {
        let bounds = visual_cx.debug_bounds(selector);
        assert!(
            bounds
                .as_ref()
                .is_some_and(|bounds| bounds.origin.y + bounds.size.height <= px(780.0)),
            "Topic 详情控件不应被底部窗口边缘遮挡: {selector}={bounds:?}"
        );
    }
    assert_within_width(visual_cx, "kafka-topic-expand", 1200.0);
    assert_within_width(visual_cx, "kafka-topic-delete", 1200.0);
    let topic_detail_name_bounds = visual_cx.debug_bounds("kafka-topic-detail-name");
    let topic_detail_copy_bounds = visual_cx.debug_bounds("kafka-topic-detail-copy");
    assert!(
        topic_detail_name_bounds.is_some() && topic_detail_copy_bounds.is_some(),
        "选中 Topic 后应显示可选择名称和复制按钮: name={topic_detail_name_bounds:?}, copy={topic_detail_copy_bounds:?}"
    );
    click(visual_cx, "kafka-topic-detail-copy");
    let copied_topic = visual_cx
        .read_from_clipboard()
        .and_then(|item| item.text())
        .unwrap_or_default();
    assert_eq!(
        copied_topic, "ramag.integration.messages",
        "Topic 复制按钮应写入完整名称"
    );

    visual_cx.simulate_resize(size(px(900.0), px(780.0)));
    visual_cx.run_until_parked();
    for selector in [
        "kafka-topic-admin",
        "kafka-topic-row-ramag.integration.messages",
        "kafka-partition-scroll",
        "kafka-topic-expand",
        "kafka-topic-delete",
    ] {
        assert_within_width(visual_cx, selector, 900.0);
    }

    visual_cx.simulate_resize(size(px(1200.0), px(780.0)));
    visual_cx.run_until_parked();

    click(visual_cx, "kafka-section-ConsumerGroups");
    visual_cx.run_until_parked();
    assert!(visual_cx.debug_bounds("kafka-consumer-groups").is_some());
    assert!(
        visual_cx
            .debug_bounds("kafka-consumer-group-row-ramag.integration.consumer")
            .is_some(),
        "消费者组列表应显示真实驱动返回的组"
    );
    assert!(
        visual_cx
            .debug_bounds("kafka-consumer-group-v-scrollbar")
            .is_some(),
        "消费者组列表应提供纵向滚动条入口"
    );
    click(
        visual_cx,
        "kafka-consumer-group-row-ramag.integration.consumer",
    );
    visual_cx.run_until_parked();
    assert!(
        visual_cx
            .debug_bounds("kafka-consumer-group-detail-name")
            .is_some()
            && visual_cx
                .debug_bounds("kafka-consumer-group-detail-copy")
                .is_some(),
        "选中消费者组后应显示可选择名称和复制按钮"
    );
    click(visual_cx, "kafka-consumer-group-detail-copy");
    let copied_group = visual_cx
        .read_from_clipboard()
        .and_then(|item| item.text())
        .unwrap_or_default();
    assert_eq!(
        copied_group, "ramag.integration.consumer",
        "消费者组复制按钮应写入完整 ID"
    );
    for selector in [
        "kafka-consumer-group-list",
        "kafka-consumer-group-detail",
        "kafka-consumer-group-members",
    ] {
        assert_within_width(visual_cx, selector, 1200.0);
    }
    visual_cx.simulate_resize(size(px(900.0), px(780.0)));
    visual_cx.run_until_parked();
    for selector in [
        "kafka-consumer-group-list",
        "kafka-consumer-group-detail",
        "kafka-consumer-group-offset-rows",
    ] {
        assert_within_width(visual_cx, selector, 900.0);
    }
    visual_cx.simulate_resize(size(px(1200.0), px(780.0)));
    visual_cx.run_until_parked();

    visual_cx.update(|window, app| {
        kafka_entity.update(app, |view, cx| {
            view.set_form_from_config(&cluster, window, cx);
            view.read_only = KafkaReadOnlyState::ReadWrite;
            view.topic_create_name
                .update(cx, |input, cx| input.set_value("events-admin", window, cx));
            view.topic_create_partitions
                .update(cx, |input, cx| input.set_value("2", window, cx));
            view.topic_create_replication_factor
                .update(cx, |input, cx| input.set_value("1", window, cx));
            view.section = KafkaSection::Config;
            view.config_resource_type = KafkaConfigResourceType::Topic;
            view.config_resource_name.update(cx, |input, cx| {
                input.set_value("ramag.integration.messages", window, cx)
            });
            cx.notify();
        });
    });
    visual_cx.run_until_parked();
    let form_result = visual_cx.update(|_, app| kafka_entity.read(app).form_config(app));
    assert!(form_result.is_ok(), "管理表单应能组装配置: {form_result:?}");
    assert!(kafka_entity.read_with(visual_cx, |view, _| {
        view.read_only == KafkaReadOnlyState::ReadWrite
    }));
    assert!(visual_cx.debug_bounds("kafka-remote-config").is_some());
    assert!(visual_cx.debug_bounds("kafka-config-query").is_some());
    click(visual_cx, "kafka-config-read");
    visual_cx.run_until_parked();
    assert!(kafka_entity.read_with(visual_cx, |view, _| { view.config_entries.len() == 3 }));
    assert!(
        visual_cx
            .debug_bounds("kafka-config-entry-retention.ms")
            .is_some()
    );
    assert!(
        visual_cx
            .debug_bounds("kafka-config-set-retention.ms")
            .is_some()
    );
    assert!(
        visual_cx
            .debug_bounds("kafka-config-delete-retention.ms")
            .is_some()
    );
    assert_within_width(visual_cx, "kafka-remote-config", 1200.0);

    visual_cx.simulate_resize(size(px(900.0), px(780.0)));
    visual_cx.run_until_parked();
    assert_within_width(visual_cx, "kafka-config-query", 900.0);
    assert_within_width(visual_cx, "kafka-config-entry-retention.ms", 900.0);
    assert_within_width(visual_cx, "kafka-config-set-retention.ms", 900.0);
    visual_cx.simulate_resize(size(px(1200.0), px(780.0)));
    visual_cx.run_until_parked();
    kafka_entity.update(visual_cx, |view, cx| {
        view.section = KafkaSection::Topics;
        cx.notify();
    });
    visual_cx.run_until_parked();
    assert!(visual_cx.debug_bounds("kafka-topic-create").is_some());
    click(visual_cx, "kafka-topic-create");
    visual_cx.run_until_parked();
    assert!(
        visual_cx.debug_bounds("ramag-confirm-ok").is_some(),
        "创建 Topic 前必须显示确认对话框"
    );
    click(visual_cx, "ramag-confirm-cancel");
    visual_cx.run_until_parked();
    assert!(visual_cx.debug_bounds("ramag-confirm-ok").is_none());

    visual_cx.update(|window, app| {
        kafka_entity.update(app, |view, cx| {
            view.topic_target_partitions
                .update(cx, |input, cx| input.set_value("51", window, cx));
            cx.notify();
        });
    });
    visual_cx.run_until_parked();
    click(visual_cx, "kafka-topic-expand");
    visual_cx.run_until_parked();
    assert!(
        visual_cx.debug_bounds("ramag-confirm-ok").is_some(),
        "扩容 Topic 前必须显示确认对话框"
    );
    click(visual_cx, "ramag-confirm-cancel");
    visual_cx.run_until_parked();
    assert!(visual_cx.debug_bounds("ramag-confirm-ok").is_none());

    click(visual_cx, "kafka-topic-delete");
    visual_cx.run_until_parked();
    assert!(
        visual_cx.debug_bounds("ramag-confirm-ok").is_some(),
        "删除 Topic 前必须显示确认对话框"
    );
    click(visual_cx, "ramag-confirm-cancel");
    visual_cx.run_until_parked();
    assert!(visual_cx.debug_bounds("ramag-confirm-ok").is_none());

    assert!(
        visual_cx
            .debug_bounds("kafka-open-topic-messages")
            .is_some()
    );
    click(visual_cx, "kafka-open-topic-messages");
    visual_cx.run_until_parked();
    click(visual_cx, "kafka-section-Messages");
    visual_cx.run_until_parked();
    assert!(visual_cx.debug_bounds("kafka-messages").is_some());
    assert!(visual_cx.debug_bounds("kafka-read-messages").is_some());
    visual_cx.simulate_resize(size(px(1600.0), px(900.0)));
    visual_cx.run_until_parked();
    let message_search_bounds = visual_cx.debug_bounds("kafka-message-search");
    let message_search_fields_bounds = visual_cx.debug_bounds("kafka-message-search-fields");
    let message_search_note_bounds = visual_cx.debug_bounds("kafka-message-search-note");
    assert!(
        message_search_bounds.is_some()
            && message_search_fields_bounds.is_some()
            && message_search_note_bounds.is_some(),
        "消息搜索控件应全部参与布局"
    );
    let (
        Some(message_search_bounds),
        Some(message_search_fields_bounds),
        Some(message_search_note_bounds),
    ) = (
        message_search_bounds,
        message_search_fields_bounds,
        message_search_note_bounds,
    )
    else {
        return;
    };
    assert!(
        message_search_bounds.origin.x + message_search_bounds.size.width
            <= message_search_fields_bounds.origin.x,
        "搜索输入与字段按钮不应重叠: {message_search_bounds:?} / {message_search_fields_bounds:?}"
    );
    assert!(
        message_search_fields_bounds.origin.x + message_search_fields_bounds.size.width
            <= message_search_note_bounds.origin.x,
        "字段按钮与扫描说明不应重叠: {message_search_fields_bounds:?} / {message_search_note_bounds:?}"
    );
    let messages_bounds = visual_cx.debug_bounds("kafka-messages");
    assert!(messages_bounds.is_some(), "消息页面应参与布局");
    let Some(messages_bounds) = messages_bounds else {
        return;
    };
    assert!(
        message_search_note_bounds.origin.x + message_search_note_bounds.size.width
            <= messages_bounds.origin.x + messages_bounds.size.width,
        "扫描说明不应溢出消息页面: {message_search_note_bounds:?} / {messages_bounds:?}"
    );
    let range_mode_bounds = visual_cx.debug_bounds("kafka-range-mode");
    let range_input_bounds = visual_cx.debug_bounds("kafka-range-inputs");
    let action_bounds = visual_cx.debug_bounds("kafka-message-actions");
    assert!(
        range_mode_bounds.is_some() && range_input_bounds.is_some() && action_bounds.is_some(),
        "消息范围控件应全部参与布局"
    );
    let (Some(range_mode_bounds), Some(range_input_bounds), Some(action_bounds)) =
        (range_mode_bounds, range_input_bounds, action_bounds)
    else {
        return;
    };
    assert!(
        range_mode_bounds.origin.x + range_mode_bounds.size.width <= range_input_bounds.origin.x,
        "范围模式与范围输入不应重叠: {range_mode_bounds:?} / {range_input_bounds:?}"
    );
    assert!(
        range_input_bounds.origin.x + range_input_bounds.size.width <= action_bounds.origin.x,
        "范围输入与读取动作不应重叠: {range_input_bounds:?} / {action_bounds:?}"
    );
    assert!(
        action_bounds.origin.x + action_bounds.size.width
            <= messages_bounds.origin.x + messages_bounds.size.width,
        "读取动作不应溢出消息页面: {action_bounds:?} / {messages_bounds:?}"
    );

    kafka_entity.update(visual_cx, |view, cx| {
        let records = (0..VISUAL_MESSAGE_COUNT)
            .map(|offset| KafkaMessageRecord {
                topic: "ramag.integration.messages".into(),
                partition: i32::try_from(offset % 3).unwrap_or_default(),
                offset: i64::from(u32::try_from(offset).unwrap_or_default()),
                timestamp: None,
                key: Some(format!("key-{offset}").into_bytes()),
                value: Some(format!("value-{offset}").into_bytes()),
                headers: Vec::new(),
            })
            .collect::<Vec<_>>();
        view.message_page = Some(KafkaMessagePage {
            records,
            scanned_records: VISUAL_MESSAGE_COUNT,
            scanned_bytes: 1_024_000,
            truncated: false,
        });
        view.message_page_index = 0;
        view.loading_messages = false;
        cx.notify();
    });
    visual_cx.run_until_parked();
    assert!(
        visual_cx
            .debug_bounds("kafka-message-v-scrollbar")
            .is_some(),
        "消息结果区应显示纵向滚动条"
    );
    assert!(
        visual_cx.debug_bounds("kafka-message-pagination").is_some()
            && visual_cx.debug_bounds("kafka-message-page-next").is_some(),
        "消息结果超过一页时应显示分页控件"
    );
    click(visual_cx, "kafka-message-page-next");
    assert_eq!(
        kafka_entity.read_with(visual_cx, |view, _| view.message_page_index),
        1,
        "下一页按钮应切换已加载结果页"
    );

    visual_cx.simulate_resize(size(px(800.0), px(500.0)));
    visual_cx.run_until_parked();
    for selector in [
        "kafka-message-query-row",
        "kafka-message-search-row",
        "kafka-message-actions",
        "kafka-message-pagination",
    ] {
        assert_within_width(visual_cx, selector, 800.0);
    }

    visual_cx.simulate_resize(size(px(1200.0), px(780.0)));
    visual_cx.run_until_parked();
    let narrow_messages_bounds = visual_cx.debug_bounds("kafka-messages");
    let narrow_search_note_bounds = visual_cx.debug_bounds("kafka-message-search-note");
    let narrow_action_bounds = visual_cx.debug_bounds("kafka-message-actions");
    assert!(
        narrow_messages_bounds.is_some()
            && narrow_search_note_bounds.is_some()
            && narrow_action_bounds.is_some(),
        "窄窗口消息筛选控件应全部参与布局"
    );
    let (Some(narrow_messages_bounds), Some(narrow_search_note_bounds), Some(narrow_action_bounds)) = (
        narrow_messages_bounds,
        narrow_search_note_bounds,
        narrow_action_bounds,
    ) else {
        return;
    };
    assert!(
        narrow_search_note_bounds.origin.x + narrow_search_note_bounds.size.width
            <= narrow_messages_bounds.origin.x + narrow_messages_bounds.size.width,
        "窄窗口搜索说明不应溢出: {narrow_search_note_bounds:?} / {narrow_messages_bounds:?}"
    );
    assert!(
        narrow_action_bounds.origin.x + narrow_action_bounds.size.width
            <= narrow_messages_bounds.origin.x + narrow_messages_bounds.size.width,
        "窄窗口读取动作不应溢出: {narrow_action_bounds:?} / {narrow_messages_bounds:?}"
    );

    kafka_entity.update(visual_cx, |view, cx| {
        view.loading_messages = true;
        cx.notify();
    });
    visual_cx.run_until_parked();
    assert!(visual_cx.debug_bounds("kafka-message-loading").is_some());
    click(visual_cx, "kafka-cancel-messages");
    assert!(!kafka_entity.read_with(visual_cx, |view, _| view.loading_messages));
}
