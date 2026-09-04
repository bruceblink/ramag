use super::*;

struct KafkaTopicTestHost {
    view: gpui::Entity<KafkaView>,
}

impl Render for KafkaTopicTestHost {
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
fn kafka_topics_reflow_header_and_split_at_supported_widths(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let cluster = KafkaClusterConfig::new("主题布局 Kafka", vec!["127.0.0.1:19092".into()]);
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
        let host = cx.new(|_| KafkaTopicTestHost { view: kafka });
        gpui_component::Root::new(host, window, cx)
    });
    let Some(kafka_entity) = kafka_entity else {
        return;
    };

    kafka_entity.update(visual_cx, |view, cx| {
        view.clusters = vec![cluster.clone()];
        view.selected_cluster_id = Some(cluster.id.clone());
        view.metadata = Some(KafkaClusterMetadata {
            cluster_id: Some("topic-layout-cluster".into()),
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
        view.topics = vec![KafkaTopic {
            name: "ramag.integration.topic-with-a-long-name".into(),
            internal: false,
            partitions: vec![KafkaPartition {
                id: 0,
                leader: Some(0),
                replicas: vec![0],
                isr: vec![0],
                low_watermark: Some(0),
                high_watermark: Some(1),
            }],
        }];
        view.selected_topic = Some("ramag.integration.topic-with-a-long-name".into());
        view.section = KafkaSection::Topics;
        view.loading_clusters = false;
        view.loading_runtime = false;
        cx.notify();
    });
    visual_cx.run_until_parked();

    for (width, height) in [(360.0, 900.0), (900.0, 780.0), (1440.0, 900.0)] {
        visual_cx.simulate_resize(size(px(width), px(height)));
        visual_cx.run_until_parked();
        let topics = visual_cx.debug_bounds("kafka-topics");
        let header = visual_cx.debug_bounds("kafka-topic-header");
        let search = visual_cx.debug_bounds("kafka-topic-search");
        let split = visual_cx.debug_bounds("kafka-topic-split");
        let list = visual_cx.debug_bounds("kafka-topic-list-panel");
        let detail = visual_cx.debug_bounds("kafka-topic-detail");
        assert!(
            topics.is_some()
                && header.is_some()
                && search.is_some()
                && split.is_some()
                && list.is_some()
                && detail.is_some(),
            "主题页的标题、搜索、列表和详情都应参与布局"
        );
        let (Some(topics), Some(header), Some(search), Some(split), Some(list), Some(detail)) =
            (topics, header, search, split, list, detail)
        else {
            return;
        };

        for selector in [
            "kafka-topic-header",
            "kafka-topic-search",
            "kafka-topic-split",
            "kafka-topic-list-panel",
            "kafka-topic-detail",
            "kafka-topic-expand",
            "kafka-topic-delete",
            "kafka-open-topic-messages",
        ] {
            assert_within_width(visual_cx, selector, width);
        }
        assert!(
            header.origin.x >= topics.origin.x
                && header.right() <= topics.right()
                && search.origin.x >= header.origin.x
                && search.right() <= header.right(),
            "主题标题和搜索框不能越出页面: topics={topics:?}, header={header:?}, search={search:?}"
        );
        assert!(
            split.origin.x >= topics.origin.x
                && split.right() <= topics.right()
                && list.right() <= split.right()
                && detail.right() <= split.right(),
            "主题列表和详情不能横向越出分栏: split={split:?}, list={list:?}, detail={detail:?}"
        );

        if width < 700.0 {
            assert!(
                header.origin.y + header.size.height > search.origin.y,
                "极窄窗口应将主题搜索框移到标题下方: header={header:?}, search={search:?}"
            );
        }
        if width < 1200.0 {
            assert!(
                list.bottom() <= detail.origin.y && detail.bottom() <= split.bottom(),
                "紧凑窗口中列表和详情应上下排列且留在分栏内: split={split:?}, list={list:?}, detail={detail:?}"
            );
        } else {
            assert!(
                list.right() <= detail.origin.x && detail.bottom() <= split.bottom(),
                "宽窗口中列表和详情应左右排列且留在分栏内: split={split:?}, list={list:?}, detail={detail:?}"
            );
        }
    }
}
