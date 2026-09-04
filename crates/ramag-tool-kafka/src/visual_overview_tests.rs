use super::*;

struct KafkaOverviewTestHost {
    view: gpui::Entity<KafkaView>,
}

impl Render for KafkaOverviewTestHost {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let dialog_layer = gpui_component::Root::render_dialog_layer(window, cx);
        gpui::div()
            .relative()
            .size_full()
            .child(self.view.clone())
            .children(dialog_layer)
    }
}

/// 检查概览页在宽、紧凑和窄窗口中保持顶部对齐，并让 Topic 预览紧跟 Broker 数据。
#[gpui::test]
fn kafka_overview_keeps_sections_aligned_without_vertical_gap(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let cluster = KafkaClusterConfig::new("Overview Kafka", vec!["127.0.0.1:19092".into()]);
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
        let host = cx.new(|_| KafkaOverviewTestHost { view: kafka });
        gpui_component::Root::new(host, window, cx)
    });
    let Some(kafka_entity) = kafka_entity else {
        return;
    };

    kafka_entity.update(visual_cx, |view, cx| {
        view.clusters = vec![cluster.clone()];
        view.selected_cluster_id = Some(cluster.id.clone());
        view.metadata = Some(KafkaClusterMetadata {
            cluster_id: Some("overview-cluster".into()),
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
        view.topics = (0..3)
            .map(|index| KafkaTopic {
                name: format!("overview.topic-{index}"),
                internal: false,
                partitions: vec![KafkaPartition {
                    id: 0,
                    leader: Some(0),
                    replicas: vec![0],
                    isr: vec![0],
                    low_watermark: Some(0),
                    high_watermark: Some(1),
                }],
            })
            .collect();
        view.loading_clusters = false;
        view.loading_runtime = false;
        view.section = KafkaSection::Overview;
        cx.notify();
    });

    for (width, height) in [(1440.0, 900.0), (1024.0, 900.0), (360.0, 900.0)] {
        visual_cx.simulate_resize(size(px(width), px(height)));
        visual_cx.run_until_parked();
        let bounds = [
            visual_cx.debug_bounds("kafka-root"),
            visual_cx.debug_bounds("kafka-sidebar"),
            visual_cx.debug_bounds("kafka-sidebar-header"),
            visual_cx.debug_bounds("kafka-main"),
            visual_cx.debug_bounds("kafka-overview"),
            visual_cx.debug_bounds("kafka-overview-scroll"),
            visual_cx.debug_bounds("kafka-overview-metrics"),
            visual_cx.debug_bounds("kafka-overview-sections"),
            visual_cx.debug_bounds("kafka-overview-primary"),
            visual_cx.debug_bounds("kafka-overview-broker"),
            visual_cx.debug_bounds("kafka-overview-topic"),
            visual_cx.debug_bounds("kafka-overview-cluster"),
        ];
        assert!(
            bounds.iter().all(Option::is_some),
            "概览页和 Shell 的关键区域都应参与布局: width={width}, bounds={bounds:?}"
        );
        let [
            Some(root),
            Some(sidebar),
            Some(sidebar_header),
            Some(main),
            Some(overview),
            Some(scroll),
            Some(metrics),
            Some(sections),
            Some(primary),
            Some(broker),
            Some(topic),
            Some(cluster),
        ] = bounds
        else {
            return;
        };

        assert!(sidebar.origin.x >= root.origin.x);
        assert!(sidebar.origin.y >= root.origin.y);
        assert!(sidebar.bottom() <= root.bottom());
        assert!(sidebar_header.origin.y <= sidebar.origin.y + px(1.0));
        assert!(sidebar_header.right() <= sidebar.right());
        assert!(main.right() <= root.right());
        assert!(overview.origin.x >= main.origin.x);
        assert!(overview.right() <= main.right());
        assert!(scroll.right() <= overview.right());
        assert!(metrics.right() <= scroll.right());
        assert!(sections.right() <= scroll.right());
        assert!(primary.right() <= sections.right());
        assert!(broker.right() <= primary.right());
        assert!(topic.right() <= primary.right());
        assert!(cluster.right() <= sections.right());
        assert!(topic.origin.y >= broker.bottom());
        assert!(
            topic.origin.y <= broker.bottom() + px(24.0),
            "Topic 预览不应等待右侧集群卡片结束: width={width}, broker={broker:?}, topic={topic:?}, cluster={cluster:?}"
        );
        if width >= 1100.0 {
            assert!(
                broker.origin.x < cluster.origin.x,
                "宽窗口应将 Broker 和集群信息并排: broker={broker:?}, cluster={cluster:?}"
            );
            assert!(
                topic.origin.y < cluster.bottom(),
                "宽窗口中的 Topic 预览应填充右侧集群卡片下方的空间: topic={topic:?}, cluster={cluster:?}"
            );
        } else {
            assert!(
                cluster.origin.y >= topic.bottom(),
                "紧凑窗口应将集群信息放在主内容之后: topic={topic:?}, cluster={cluster:?}"
            );
        }
    }
}
