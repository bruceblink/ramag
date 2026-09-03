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

#[gpui::test]
fn kafka_runtime_error_and_retry_fit_three_window_widths(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let cluster = KafkaClusterConfig::new("窄窗口恢复 Kafka", vec!["127.0.0.1:19092".into()]);
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
    kafka_entity.update(visual_cx, |view, cx| {
        view.clusters = vec![cluster.clone()];
        view.selected_cluster_id = Some(cluster.id.clone());
        view.metadata = None;
        view.topics.clear();
        view.loading_clusters = false;
        view.loading_runtime = false;
        view.runtime_error = Some(
            "读取 Kafka 元数据失败：Broker 连接超时，地址为 192.168.100.25:19092，当前请求暂时无法完成；请检查网络、认证配置和服务状态后重试"
                .into(),
        );
        view.section = KafkaSection::Overview;
        cx.notify();
    });
    visual_cx.run_until_parked();

    for (width, height) in [(360.0, 900.0), (1024.0, 900.0), (1440.0, 900.0)] {
        visual_cx.simulate_resize(size(px(width), px(height)));
        visual_cx.run_until_parked();
        let Some(error) = visual_cx.debug_bounds("kafka-runtime-error") else {
            return;
        };
        let Some(message) = visual_cx.debug_bounds("kafka-runtime-error-message") else {
            return;
        };
        let Some(retry) = visual_cx.debug_bounds("kafka-runtime-retry") else {
            return;
        };
        assert!(
            error.origin.x >= px(0.0)
                && error.right() <= px(width)
                && message.origin.x >= error.origin.x
                && message.right() <= error.right()
                && retry.origin.x >= error.origin.x
                && retry.right() <= error.right()
                && retry.origin.y >= error.origin.y
                && retry.bottom() <= error.bottom(),
            "连接失败提示或重试按钮越出容器: width={width}, error={error:?}, message={message:?}, retry={retry:?}"
        );
    }

    super::click(visual_cx, "kafka-runtime-retry");
    visual_cx.run_until_parked();
    assert!(kafka_entity.read_with(visual_cx, |view, _| {
        !view.loading_runtime && view.runtime_error.is_none() && view.metadata.is_some()
    }));
}
