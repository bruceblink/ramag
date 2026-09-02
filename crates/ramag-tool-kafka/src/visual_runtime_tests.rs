use super::*;

struct KafkaRuntimeTestHost {
    view: gpui::Entity<KafkaView>,
}

impl Render for KafkaRuntimeTestHost {
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
fn kafka_runtime_failure_can_recover_with_manual_retry(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let cluster = KafkaClusterConfig::new("Recovery Kafka", vec!["127.0.0.1:19092".into()]);
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
        let host = cx.new(|_| KafkaRuntimeTestHost { view: kafka });
        gpui_component::Root::new(host, window, cx)
    });
    let Some(kafka_entity) = kafka_entity else {
        return;
    };
    visual_cx.simulate_resize(size(px(1_000.0), px(700.0)));
    visual_cx.run_until_parked();

    kafka_entity.update(visual_cx, |view, cx| {
        view.clusters = vec![cluster.clone()];
        view.selected_cluster_id = Some(cluster.id.clone());
        view.metadata = None;
        view.topics.clear();
        view.loading_clusters = false;
        view.loading_runtime = false;
        view.runtime_error = Some("读取消息失败：Kafka Broker 暂时不可达".into());
        view.section = KafkaSection::Overview;
        cx.notify();
    });
    visual_cx.run_until_parked();
    assert!(visual_cx.debug_bounds("kafka-runtime-error").is_some());
    assert!(visual_cx.debug_bounds("kafka-runtime-retry").is_some());

    super::click(visual_cx, "kafka-runtime-retry");
    visual_cx.run_until_parked();
    assert!(kafka_entity.read_with(visual_cx, |view, _| {
        !view.loading_runtime && view.runtime_error.is_none() && view.metadata.is_some()
    }));
    assert!(visual_cx.debug_bounds("kafka-runtime-error").is_none());
}
