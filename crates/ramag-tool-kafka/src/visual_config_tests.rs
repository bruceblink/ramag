use super::*;

struct KafkaConfigTestHost {
    view: gpui::Entity<KafkaView>,
}

impl Render for KafkaConfigTestHost {
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
fn kafka_config_entries_fit_three_window_widths_with_long_values(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let cluster = KafkaClusterConfig::new("配置布局 Kafka", vec!["127.0.0.1:19092".into()]);
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
        let host = cx.new(|_| KafkaConfigTestHost { view: kafka });
        gpui_component::Root::new(host, window, cx)
    });
    let Some(kafka_entity) = kafka_entity else {
        return;
    };

    let config_key = "retention.ms".to_owned();
    kafka_entity.update(visual_cx, |view, cx| {
        view.clusters = vec![cluster.clone()];
        view.selected_cluster_id = Some(cluster.id.clone());
        view.metadata = Some(KafkaClusterMetadata {
            cluster_id: Some("config-layout-cluster".into()),
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
        view.section = KafkaSection::Config;
        view.loading_clusters = false;
        view.loading_runtime = false;
        view.loading_configs = false;
        view.read_only = KafkaReadOnlyState::ReadWrite;
        view.config_resource_type = KafkaConfigResourceType::Topic;
        view.config_entries = vec![KafkaConfigEntry {
            key: config_key.clone(),
            value: Some("very-long-kafka-config-value-".repeat(24)),
            source: KafkaConfigSource::DynamicTopic,
            is_read_only: false,
            is_default: false,
            is_sensitive: false,
        }];
        cx.notify();
    });
    visual_cx.run_until_parked();

    for (width, height) in [(360.0, 900.0), (1024.0, 900.0), (1440.0, 900.0)] {
        visual_cx.simulate_resize(size(px(width), px(height)));
        visual_cx.run_until_parked();
        for selector in [
            "kafka-config",
            "kafka-remote-config",
            "kafka-config-query",
            "kafka-config-list",
            "kafka-config-entry-retention.ms",
            "kafka-config-set-retention.ms",
            "kafka-config-delete-retention.ms",
        ] {
            assert_within_width(visual_cx, selector, width);
        }
        let Some(entry) = visual_cx.debug_bounds("kafka-config-entry-retention.ms") else {
            return;
        };
        let Some(set_button) = visual_cx.debug_bounds("kafka-config-set-retention.ms") else {
            return;
        };
        let Some(delete_button) = visual_cx.debug_bounds("kafka-config-delete-retention.ms") else {
            return;
        };
        assert!(
            set_button.origin.x >= entry.origin.x
                && delete_button.origin.x >= entry.origin.x
                && set_button.right() <= entry.right()
                && delete_button.right() <= entry.right()
                && set_button.bottom() <= entry.bottom()
                && delete_button.bottom() <= entry.bottom(),
            "配置项操作组不应越出配置行: width={width}, entry={entry:?}, set={set_button:?}, delete={delete_button:?}"
        );
    }

    assert!(
        kafka_entity.read_with(visual_cx, |view, _| view.config_entries[0].key
            == config_key),
        "配置项长值布局检查不应修改配置状态"
    );
}
