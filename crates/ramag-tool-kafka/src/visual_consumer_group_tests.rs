use super::*;

struct KafkaConsumerGroupTestHost {
    view: gpui::Entity<KafkaView>,
}

impl Render for KafkaConsumerGroupTestHost {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let dialog_layer = gpui_component::Root::render_dialog_layer(window, cx);
        gpui::div()
            .relative()
            .size_full()
            .child(self.view.clone())
            .children(dialog_layer)
    }
}

#[gpui::test]
fn kafka_consumer_groups_fit_three_window_widths_with_long_fields(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let cluster = KafkaClusterConfig::new("消费者组布局 Kafka", vec!["127.0.0.1:19092".into()]);
    let service = Arc::new(KafkaService::new(
        Arc::new(FakeKafkaDriver),
        Arc::new(FakeStorage {
            cluster: cluster.clone(),
        }),
    ));
    let mut kafka_entity = None;
    let (_, visual_cx) = cx.add_window_view(|window, cx| {
        let kafka = cx.new(|cx| KafkaView::new(service, window, cx));
        kafka_entity = Some(kafka.clone());
        let host = cx.new(|_| KafkaConsumerGroupTestHost { view: kafka });
        gpui_component::Root::new(host, window, cx)
    });
    let Some(kafka_entity) = kafka_entity else {
        return;
    };

    let group_id = "ramag.integration.consumer".to_owned();
    kafka_entity.update(visual_cx, |view, cx| {
        view.clusters = vec![cluster.clone()];
        view.selected_cluster_id = Some(cluster.id.clone());
        view.metadata = Some(KafkaClusterMetadata {
            cluster_id: Some("consumer-group-layout-cluster".into()),
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
        view.section = KafkaSection::ConsumerGroups;
        view.loading_clusters = false;
        view.loading_runtime = false;
        view.loading_consumer_groups = false;
        view.consumer_groups = vec![KafkaConsumerGroup {
            group_id: group_id.clone(),
            state: Some("Stable".into()),
            protocol: Some("range".into()),
            members: vec![KafkaConsumerMember {
                member_id: "member-with-a-deliberately-long-identifier-for-narrow-window-layout"
                    .into(),
                client_id: "consumer-client-with-a-long-name".into(),
                client_host: Some(
                    "/192.168.100.25-with-an-extra-long-host-description-for-layout".into(),
                ),
                assigned_partitions: vec![KafkaConsumerPartitionAssignment {
                    topic: "ramag.integration.messages.with-a-long-topic-name".into(),
                    partition: 123,
                }],
            }],
            offsets: vec![KafkaConsumerGroupOffset {
                topic: "ramag.integration.messages.with-a-long-topic-name".into(),
                partition: 123,
                committed_offset: Some(1000),
                end_offset: Some(1250),
                lag: Some(250),
            }],
        }];
        view.selected_consumer_group = Some(group_id.clone());
        cx.notify();
    });
    visual_cx.run_until_parked();

    for (width, height) in [(360.0, 900.0), (1024.0, 900.0), (1440.0, 900.0)] {
        visual_cx.simulate_resize(size(px(width), px(height)));
        visual_cx.run_until_parked();

        for selector in [
            "kafka-consumer-groups",
            "kafka-consumer-group-list",
            "kafka-consumer-group-list-scroll",
            "kafka-consumer-group-row-ramag.integration.consumer",
            "kafka-consumer-group-detail",
            "kafka-consumer-group-detail-name",
            "kafka-consumer-group-members",
            "kafka-consumer-group-offset-rows",
        ] {
            assert_within_width(visual_cx, selector, width);
        }

        let Some(workspace) = visual_cx.debug_bounds("kafka-consumer-groups") else {
            return;
        };
        let Some(list) = visual_cx.debug_bounds("kafka-consumer-group-list") else {
            return;
        };
        let Some(detail) = visual_cx.debug_bounds("kafka-consumer-group-detail") else {
            return;
        };
        assert!(
            list.origin.x >= workspace.origin.x
                && list.origin.x + list.size.width <= workspace.origin.x + workspace.size.width
                && detail.origin.x >= workspace.origin.x
                && detail.origin.x + detail.size.width <= workspace.origin.x + workspace.size.width,
            "消费者组列表和详情不应横向超出工作区: list={list:?}, detail={detail:?}, workspace={workspace:?}"
        );
        if width < 1080.0 {
            assert!(
                list.origin.y + list.size.height <= detail.origin.y,
                "紧凑窗口中列表和详情不应重叠: list={list:?}, detail={detail:?}"
            );
        } else {
            assert!(
                list.origin.x + list.size.width <= detail.origin.x,
                "宽窗口中列表和详情不应重叠: list={list:?}, detail={detail:?}"
            );
        }
    }

    click(visual_cx, "kafka-consumer-group-detail-copy");
    let copied_group = visual_cx
        .read_from_clipboard()
        .and_then(|item| item.text())
        .unwrap_or_default();
    assert_eq!(copied_group, group_id, "消费者组复制按钮应保留完整 ID");
}
