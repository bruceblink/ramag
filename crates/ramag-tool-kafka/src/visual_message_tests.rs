use super::*;

struct KafkaMessageTestHost {
    view: gpui::Entity<KafkaView>,
}

impl Render for KafkaMessageTestHost {
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
fn kafka_message_table_and_detail_fit_three_window_widths(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let cluster = KafkaClusterConfig::new("消息布局 Kafka", vec!["127.0.0.1:19092".into()]);
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
        let host = cx.new(|_| KafkaMessageTestHost { view: kafka });
        gpui_component::Root::new(host, window, cx)
    });
    let Some(kafka_entity) = kafka_entity else {
        return;
    };
    visual_cx.simulate_resize(size(px(1024.0), px(900.0)));
    visual_cx.run_until_parked();

    kafka_entity.update(visual_cx, |view, cx| {
        view.clusters = vec![cluster.clone()];
        view.selected_cluster_id = Some(cluster.id.clone());
        view.metadata = Some(KafkaClusterMetadata {
            cluster_id: Some("message-layout-cluster".into()),
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
        view.section = KafkaSection::Messages;
        view.loading_clusters = false;
        view.loading_runtime = false;
        view.loading_messages = false;
        view.message_page = Some(KafkaMessagePage {
            records: vec![KafkaMessageRecord {
                topic: "ramag.integration.messages".into(),
                partition: 1,
                offset: 42,
                timestamp: None,
                key: Some(b"message-key".to_vec()),
                value: Some(vec![b'v'; 256]),
                headers: vec![ramag_domain::entities::KafkaMessageHeader {
                    key: "trace-id".into(),
                    value: Some(b"header-value".to_vec()),
                }],
            }],
            scanned_records: 1,
            scanned_bytes: 512,
            truncated: false,
        });
        view.message_page_index = 0;
        view.selected_message = None;
        cx.notify();
    });
    visual_cx.run_until_parked();
    let row_bounds = visual_cx.debug_bounds("kafka-message-row-0");
    assert!(row_bounds.is_some(), "消息行应参与布局");
    let Some(row_bounds) = row_bounds else {
        return;
    };
    visual_cx.simulate_click(row_bounds.center(), Modifiers::default());
    let selected_message = kafka_entity.read_with(visual_cx, |view, _| view.selected_message);
    assert_eq!(selected_message, Some(0));

    for (width, height) in [(360.0, 900.0), (1024.0, 900.0), (1440.0, 900.0)] {
        visual_cx.simulate_resize(size(px(width), px(height)));
        visual_cx.run_until_parked();
        let messages = visual_cx.debug_bounds("kafka-messages");
        let table = visual_cx.debug_bounds("kafka-message-table");
        let detail = visual_cx.debug_bounds("kafka-message-detail");
        let detail_scroll = visual_cx.debug_bounds("kafka-message-detail-scroll");
        let horizontal_scrollbar = visual_cx.debug_bounds("kafka-message-h-scrollbar");
        let pagination = visual_cx.debug_bounds("kafka-message-pagination");
        assert!(
            messages.is_some()
                && table.is_some()
                && detail.is_some()
                && detail_scroll.is_some()
                && horizontal_scrollbar.is_some()
                && pagination.is_some(),
            "消息表、详情、滚动条和分页控件都应参与布局"
        );
        let (
            Some(messages),
            Some(table),
            Some(detail),
            Some(detail_scroll),
            Some(horizontal_scrollbar),
            Some(pagination),
        ) = (
            messages,
            table,
            detail,
            detail_scroll,
            horizontal_scrollbar,
            pagination,
        )
        else {
            return;
        };

        assert!(
            messages.right() <= px(width),
            "消息页面不能横向溢出: {messages:?}"
        );
        assert!(
            table.right() <= messages.right(),
            "消息表不能越出消息页面: {table:?}"
        );
        assert!(
            detail.right() <= messages.right(),
            "详情不能越出消息页面: {detail:?}"
        );
        assert!(
            detail_scroll.right() <= detail.right(),
            "详情滚动区不能越出详情面板: {detail_scroll:?} / {detail:?}"
        );
        assert!(
            horizontal_scrollbar.right() <= messages.right(),
            "横向滚动条不能越出消息页面: {horizontal_scrollbar:?}"
        );
        assert!(
            pagination.right() <= messages.right(),
            "分页状态栏不能越出消息页面: {pagination:?}"
        );
        if width < 1280.0 {
            assert!(
                detail.origin.y >= table.origin.y + table.size.height,
                "窄窗口应将详情放到消息表下方: {table:?} / {detail:?}"
            );
        } else {
            assert!(
                detail.origin.x >= table.origin.x,
                "常规和宽窗口应保留表格与详情的横向工作区: {table:?} / {detail:?}"
            );
        }
    }
}
